//! Invites and device linking inside the replica (DESIGN §5, D6, D7, D55).
//!
//! Inviter side:
//! - [`Replica::create_invite`] opens an invite (or a link code); the open invite (code
//!   included) is shared with the group's members inside sealed slots, so **any member
//!   device** can re-post the record and accept joiners (D55 layer 3), and any member can
//!   cancel it. An invite closes at its expiry, when cancelled, or when the group's key
//!   rotates for a removal or a periodic rotation (additions don't close it: multi-use).
//! - [`Replica::invite_publications`] gives the day's sealed records (always carrying the
//!   *current* epoch key) with their lookup keys, for every transport to publish.
//!
//! Joiner side:
//! - [`Replica::begin_join`] takes an opened record and posts a **signed join request** in a
//!   slot under the invite's **door** (a key only the code gives: safety audit T10). The door
//!   opens requests and answers only; the joiner reads the group once a member has let it in,
//!   so everyone who reads it is a visible member. Format 1 records (older apps, a backup's own
//!   rejoin) still carry the group's key and use the group's topic as before.
//!   Link records are refused here (D6); [`Replica::begin_link`] is the linking path.
//! - Any member that reads a valid request adds the device (a normal Add epoch, with
//!   history keys). The joiner learns it through the forward slot and becomes a member.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{Event, Replica};
use crate::crypto::{self, DevicePublic};
use crate::epochs::EpochKind;
use crate::ids::{DeviceId, EpochId, GroupId};
use crate::invite::{self, Code, CodeKind, InviteRecord};
use crate::{wire, Error, Result};

const INVITE_TAG: [u8; 2] = *b"IV";

/// The longest an invite or link code may live (the app offers 1-30 days).
pub const MAX_INVITE_MS: u64 = 30 * 86_400_000;
/// How far another device's clock may be ahead of ours.
const CLOCK_SKEW_MS: u64 = 10 * 60_000;

/// Other people's clocks decide nothing: an invite made "in the future", or living longer than any invite may, is
/// refused (it would outlive removals and cost every member one Argon2id run per day until its far expiry).
fn invite_times_ok(inv: &OpenInvite, now: u64) -> bool {
    inv.created_ms <= now + CLOCK_SKEW_MS
        && inv.expires_ms <= inv.created_ms.saturating_add(MAX_INVITE_MS + CLOCK_SKEW_MS)
        && inv.expires_ms <= now.saturating_add(MAX_INVITE_MS + CLOCK_SKEW_MS)
}
const JOIN_TAG: [u8; 2] = *b"JR";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenInvite {
    pub group: GroupId,
    pub code: Code,
    pub kind: CodeKind,
    pub created_ms: u64,
    pub expires_ms: u64,
    pub creator: DeviceId,
    pub cancelled: bool,
}

impl OpenInvite {
    pub fn id(&self) -> [u8; 32] {
        self.code.id()
    }
}

#[derive(Serialize, Deserialize)]
struct Signed {
    body: Vec<u8>,
    author: DeviceId,
    sig: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct JoinBody {
    group: GroupId,
    /// The epoch the joiner knows (its request's topic): the accepting member answers there.
    epoch: EpochId,
    invite_id: [u8; 32],
    device: DevicePublic,
    created_ms: u64,
}

/// A join this device has started (waiting to be added).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingJoin {
    pub group: GroupId,
    /// Where the group's members look (from the invite record), until we learn the sets.
    #[serde(default)]
    pub hints: Vec<(String, String)>,
    pub epoch: EpochId,
    pub invite_id: [u8; 32],
    pub kind: CodeKind,
    pub group_name: String,
    pub inviter_name: String,
}

impl Replica {
    fn invites_path(&self, g: &GroupId) -> std::path::PathBuf {
        self.layout.group_dir(g).join("invites")
    }

    pub fn invites(&self, g: &GroupId) -> Vec<OpenInvite> {
        std::fs::read(self.invites_path(g)).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default()
    }

    fn save_invites(&self, g: &GroupId, v: &[OpenInvite]) -> Result<()> {
        crate::storage::write_atomic(&self.invites_path(g), &postcard::to_stdvec(v).map_err(|e| Error::Encoding(e.to_string()))?)
    }

    /// Whether an invite accepts joiners right now.
    pub fn invite_is_open(&self, inv: &OpenInvite) -> bool {
        let now = super::now_ms();
        if inv.cancelled || now >= inv.expires_ms || !invite_times_ok(inv, now) {
            return false;
        }
        let Some(state) = self.groups.get(&inv.group) else { return false };
        // Its maker removed: closed (an invite planted before leaving must not keep handing out the key).
        if !state.epochs.is_current_member(&inv.creator) {
            return false;
        }
        // A removal after the invite was made closes it. Periodic rotations and additions
        // don't: invites expire within the rotation period anyway.
        !state.epochs.all_records().any(|r| {
            state.epochs.is_valid(&r.id()) && r.body.created_ms > inv.created_ms && matches!(r.body.kind, EpochKind::Remove { .. })
        })
    }

    /// Open an invite (or a link code) for `g`, valid for `valid_ms`.
    pub fn create_invite(&mut self, g: &GroupId, kind: CodeKind, valid_ms: u64) -> Result<(Code, u64)> {
        let state = self.groups.get(g).ok_or_else(|| Error::NotFound("group".into()))?;
        if !state.epochs.is_current_member(&self.identity.id()) {
            return Err(Error::Rejected("only members can invite".into()));
        }
        let now = super::now_ms();
        let valid_ms = valid_ms.min(MAX_INVITE_MS);
        let inv = OpenInvite { group: *g, code: Code::generate(), kind, created_ms: now, expires_ms: now + valid_ms, creator: self.identity.id(), cancelled: false };
        let mut all = self.invites(g);
        all.push(inv.clone());
        self.save_invites(g, &all)?;
        Ok((inv.code, inv.expires_ms))
    }

    /// Cancel an invite (any member may; cancelling wins over re-opening).
    pub fn cancel_invite(&mut self, g: &GroupId, invite_id: &[u8; 32]) -> Result<bool> {
        let mut all = self.invites(g);
        let mut found = false;
        for i in all.iter_mut().filter(|i| i.id() == *invite_id) {
            i.cancelled = true;
            found = true;
        }
        self.save_invites(g, &all)?;
        Ok(found)
    }

    /// Signed open-invite records to carry in our main slot for `g`.
    pub(super) fn invite_messages(&self, g: &GroupId) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        for inv in self.invites(g) {
            // Keep carrying cancellations until expiry, so they reach everyone.
            if super::now_ms() >= inv.expires_ms {
                continue;
            }
            let body = wire::encode(INVITE_TAG, 1, &inv)?;
            let sig = self.identity.sign(&body).to_vec();
            out.push(postcard::to_stdvec(&Signed { body, author: self.identity.id(), sig }).map_err(|e| Error::Encoding(e.to_string()))?);
        }
        Ok(out)
    }

    pub(super) fn receive_invite(&mut self, bytes: &[u8]) -> Result<()> {
        let s: Signed = postcard::from_bytes(bytes).map_err(|e| Error::Encoding(e.to_string()))?;
        let sig: [u8; 64] = s.sig.as_slice().try_into().map_err(|_| Error::Crypto("bad signature length"))?;
        crypto::verify(&s.author.0, &s.body, &sig)?;
        let inv: OpenInvite = wire::decode(INVITE_TAG, 1, &s.body)?.1;
        let state = self.groups.get(&inv.group).ok_or_else(|| Error::NotFound("group".into()))?;
        if !state.epochs.is_current_member(&s.author) {
            return Err(Error::Rejected("invite record from a non-member".into()));
        }
        let mut all = self.invites(&inv.group);
        match all.iter_mut().find(|i| i.id() == inv.id()) {
            Some(i) => {
                if inv.cancelled && !i.cancelled {
                    i.cancelled = true;
                } else {
                    return Ok(());
                }
            }
            None => {
                // A new invite is learnt only from its maker, with times this clock accepts: not made in the
                // future, not living longer than any invite may. (Others carry known invites and cancellations.)
                if s.author != inv.creator {
                    return Err(Error::Rejected("invite record not from its maker".into()));
                }
                if !invite_times_ok(&inv, super::now_ms()) {
                    return Err(Error::Rejected("invite times out of bounds".into()));
                }
                all.push(inv.clone())
            }
        }
        self.save_invites(&inv.group, &all)
    }

    /// The day's sealed invite records for every open invite of every group we're in:
    /// (lookup key, sealed record). `names(group)` gives (group name, our display name).
    /// Each invite costs one Argon2id run per day (cached).
    pub fn invite_publications(&mut self, day: u32, names: &dyn Fn(&GroupId) -> (String, String)) -> Result<Vec<([u8; 32], Vec<u8>)>> {
        let me = self.identity.id();
        let mut out = Vec::new();
        let gids: Vec<GroupId> = self.groups.keys().copied().collect();
        for g in gids {
            let (head, _key) = {
                let state = &self.groups[&g];
                let Some(head) = state.epochs.working_head() else { continue };
                if !state.epochs.is_current_member(&me) {
                    continue;
                }
                let Some(key) = state.keys.get(&head).cloned() else { continue };
                (head, key)
            };
            let open: Vec<OpenInvite> = self.invites(&g).into_iter().filter(|i| self.invite_is_open(i)).collect();
            for inv in open {
                let cache_key = (inv.id(), day, inv.kind);
                let (lookup, rkey) = match self.invite_cache.get(&cache_key) {
                    Some(v) => v.clone(),
                    None => {
                        let v = invite::derive(&inv.code, inv.kind, day)?;
                        self.invite_cache.insert(cache_key, v.clone());
                        v
                    }
                };
                let (group_name, inviter_name) = names(&g);
                let rec = InviteRecord {
                    kind: inv.kind,
                    group: g,
                    epoch: head,
                    epoch_key: None,
                    door: Some(invite::door_key(&inv.code, &g)),
                    invite_id: inv.id(),
                    expires_ms: inv.expires_ms,
                    group_name,
                    inviter_name,
                    whereabouts: self.whereabouts.clone(),
                    published_ms: super::now_ms(),
                    hints: self.invite_hints(&g),
                    point: self.group_point(&g, day),
                };
                out.push((lookup, invite::seal_record(&rec, &lookup, &rkey)?));
            }
        }
        if self.invite_cache.len() > 256 {
            self.invite_cache.clear();
        }
        Ok(out)
    }

    fn ahead_path(&self, g: &GroupId) -> std::path::PathBuf {
        self.layout.group_dir(g).join("invites_ahead")
    }

    fn ahead_done(&self, g: &GroupId) -> std::collections::BTreeSet<([u8; 32], u32)> {
        std::fs::read(self.ahead_path(g)).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default()
    }

    /// Copies of every open invite for each day from `today` until it expires, not yet
    /// published ahead: (group, invite id, day, lookup key, sealed record). Publishing
    /// them to storing layers (helpers, Nostr) lets a joiner find the invite on any day
    /// even while every member is offline. Security is unchanged: a guess still tests only
    /// one day's copies, and each day's copies are the invites valid that day.
    /// Costs one Argon2id run per new day (a one-time background job per invite).
    #[allow(clippy::type_complexity)]
    pub fn invite_publications_ahead(
        &mut self,
        today: u32,
        names: &dyn Fn(&GroupId) -> (String, String),
    ) -> Result<Vec<(GroupId, [u8; 32], u32, [u8; 32], Vec<u8>)>> {
        let me = self.identity.id();
        let mut out = Vec::new();
        let gids: Vec<GroupId> = self.groups.keys().copied().collect();
        for g in gids {
            let (head, _key) = {
                let state = &self.groups[&g];
                let Some(head) = state.epochs.working_head() else { continue };
                if !state.epochs.is_current_member(&me) {
                    continue;
                }
                let Some(key) = state.keys.get(&head).cloned() else { continue };
                (head, key)
            };
            let done = self.ahead_done(&g);
            let open: Vec<OpenInvite> = self.invites(&g).into_iter().filter(|i| self.invite_is_open(i)).collect();
            for inv in open {
                // Never more than an invite's longest life ahead (one Argon2id run per day).
                let last_day = invite::day_of(inv.expires_ms).min(today + (MAX_INVITE_MS / 86_400_000) as u32 + 1);
                for day in today..=last_day {
                    if done.contains(&(inv.id(), day)) {
                        continue;
                    }
                    let (lookup, rkey) = invite::derive(&inv.code, inv.kind, day)?;
                    let (group_name, inviter_name) = names(&g);
                    let rec = InviteRecord {
                        kind: inv.kind,
                        group: g,
                        epoch: head,
                        epoch_key: None,
                        door: Some(invite::door_key(&inv.code, &g)),
                        invite_id: inv.id(),
                        expires_ms: inv.expires_ms,
                        group_name,
                        inviter_name,
                        whereabouts: self.whereabouts.clone(),
                        published_ms: super::now_ms(),
                        hints: self.invite_hints(&g),
                        point: self.group_point(&g, day),
                    };
                    out.push((g, inv.id(), day, lookup, invite::seal_record(&rec, &lookup, &rkey)?));
                }
            }
        }
        Ok(out)
    }

    /// Remember that these (invite, day) copies were published ahead.
    pub fn mark_published_ahead(&mut self, done: &[(GroupId, [u8; 32], u32)]) -> Result<()> {
        let mut by_group: HashMap<GroupId, Vec<([u8; 32], u32)>> = HashMap::new();
        for (g, id, d) in done {
            by_group.entry(*g).or_default().push((*id, *d));
        }
        for (g, items) in by_group {
            let mut set = self.ahead_done(&g);
            set.extend(items);
            crate::storage::write_atomic(&self.ahead_path(&g), &postcard::to_stdvec(&set).map_err(|e| Error::Encoding(e.to_string()))?)?;
        }
        Ok(())
    }

    /// Start joining a group from an opened invite record. Refuses link codes (D6).
    pub fn begin_join(&mut self, rec: &InviteRecord) -> Result<()> {
        if rec.kind == CodeKind::Link {
            return Err(Error::Rejected(
                "this is a device link code, not an invite: use it under \"Link this device\" to join your own account".into(),
            ));
        }
        self.begin(rec)
    }

    /// Start linking this device to an account (the personal group) from a link record.
    pub fn begin_link(&mut self, rec: &InviteRecord) -> Result<()> {
        if rec.kind != CodeKind::Link {
            return Err(Error::Rejected("this is an invite to a group, not a device link code".into()));
        }
        self.begin(rec)
    }

    fn begin(&mut self, rec: &InviteRecord) -> Result<()> {
        if super::now_ms() >= rec.expires_ms {
            return Err(Error::Rejected("this code has expired".into()));
        }
        // Format 2 (T10): only the door; the request and the answer live under it, and the group is read once
        // a member has let us in. Format 1 (an older app's record, or a backup's own rejoin): the key itself.
        let epoch = match (rec.door, rec.epoch_key) {
            (Some(door), _) => {
                self.add_join_anchor(rec.group, rec.epoch);
                crate::storage::write_atomic(&self.layout.group_dir(&rec.group).join("join_door"), &door)?;
                if let Some(point) = rec.point {
                    crate::storage::write_atomic(&self.layout.group_dir(&rec.group).join("join_point"), &point)?;
                }
                invite::door_epoch(&door)
            }
            (None, Some(k)) => {
                self.add_invite_key(rec.group, rec.epoch, crypto::SymKey(k));
                rec.epoch
            }
            (None, None) => return Err(Error::Invalid("this code's record is incomplete".into())),
        };
        std::fs::create_dir_all(self.layout.group_dir(&rec.group))?;
        let p = PendingJoin {
            hints: rec.hints.clone(),
            group: rec.group,
            epoch,
            invite_id: rec.invite_id,
            kind: rec.kind,
            group_name: rec.group_name.clone(),
            inviter_name: rec.inviter_name.clone(),
        };
        let mut all = self.pending_joins();
        all.retain(|x| x.group != p.group);
        all.push(p);
        self.save_pending(&all)
    }

    fn pending_path(&self) -> std::path::PathBuf {
        self.layout.root().join("pending_joins")
    }

    pub fn pending_joins(&self) -> Vec<PendingJoin> {
        std::fs::read(self.pending_path()).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default()
    }

    fn save_pending(&self, v: &[PendingJoin]) -> Result<()> {
        crate::storage::write_atomic(&self.pending_path(), &postcard::to_stdvec(v).map_err(|e| Error::Encoding(e.to_string()))?)
    }

    /// Drop pending joins that completed (we're a member now). Returns the completed ones.
    pub fn settle_joins(&mut self) -> Vec<PendingJoin> {
        let me = self.identity.id();
        let (done, waiting): (Vec<_>, Vec<_>) =
            self.pending_joins().into_iter().partition(|p| self.groups.get(&p.group).is_some_and(|s| s.epochs.is_current_member(&me)));
        if !done.is_empty() {
            let _ = self.save_pending(&waiting);
            for p in &done {
                let _ = std::fs::remove_file(self.layout.group_dir(&p.group).join("join_door"));
                let _ = std::fs::remove_file(self.layout.group_dir(&p.group).join("join_point"));
            }
        }
        done
    }

    /// A few endpoints per layer from the group's sets, for invite records (kept small: the
    /// record must fit a DHT item).
    fn invite_hints(&self, g: &GroupId) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (layer, n) in [("nostr", 2usize), ("helpers", 1usize)] {
            if let Some(set) = self.mailbox_set(g, layer).0 {
                for e in set.endpoints.into_iter().take(n) {
                    out.push((layer.to_string(), e));
                }
            }
        }
        out
    }

    /// Endpoints to use for `g` on `layer`: the group's set, or while joining, the hints
    /// from the invite record.
    pub fn endpoint_hints(&self, g: &GroupId, layer: &str) -> Vec<String> {
        if let Some(set) = self.mailbox_set(g, layer).0 {
            return set.endpoints;
        }
        self.pending_joins().into_iter().filter(|p| p.group == *g).flat_map(|p| p.hints).filter(|(l, _)| l == layer).map(|(_, e)| e).collect()
    }

    /// Our signed join request for `g`, to hand to a member directly (P2P).
    /// Add endpoints where a pending join should also look (e.g. the helpers closest to
    /// the group's daily point, when the invite's hints are gone). Deduplicated, bounded.
    pub fn add_join_hints(&mut self, g: &GroupId, layer: &str, endpoints: Vec<String>) -> Result<usize> {
        let mut all = self.pending_joins();
        let Some(p) = all.iter_mut().find(|p| p.group == *g) else { return Ok(0) };
        let before = p.hints.len();
        for e in endpoints {
            if p.hints.len() >= 32 {
                break;
            }
            if !p.hints.iter().any(|(l, x)| l == layer && *x == e) {
                p.hints.push((layer.to_string(), e));
            }
        }
        let added = p.hints.len() - before;
        if added > 0 {
            self.save_pending(&all)?;
        }
        Ok(added)
    }

    pub fn pending_join_request(&self, g: &GroupId) -> Option<Vec<u8>> {
        let p = self.pending_joins().into_iter().find(|p| p.group == *g)?;
        self.join_request(&p).ok()
    }

    /// A join request handed to us directly by a joining device (P2P): same checks as in a
    /// slot. Returns the group if the device was added (or already was a member).
    pub fn accept_join_request(&mut self, bytes: &[u8]) -> Result<GroupId> {
        let s: Signed = postcard::from_bytes(bytes).map_err(|e| Error::Encoding(e.to_string()))?;
        let body: JoinBody = wire::decode(JOIN_TAG, 1, &s.body)?.1;
        self.receive_join_request(bytes)?;
        let ok = self.groups.get(&body.group).is_some_and(|st| st.epochs.is_current_member(&s.author));
        if ok { Ok(body.group) } else { Err(Error::Rejected("join request not accepted".into())) }
    }

    /// Our signed join request for a pending join (goes into a join slot).
    pub(super) fn join_request(&self, p: &PendingJoin) -> Result<Vec<u8>> {
        let body = JoinBody { group: p.group, epoch: p.epoch, invite_id: p.invite_id, device: self.identity.public(), created_ms: super::now_ms() };
        let body = wire::encode(JOIN_TAG, 1, &body)?;
        let sig = self.identity.sign(&body).to_vec();
        postcard::to_stdvec(&Signed { body, author: self.identity.id(), sig }).map_err(|e| Error::Encoding(e.to_string()))
    }

    /// A member reads a join request: if its invite is open, add the device.
    pub(super) fn receive_join_request(&mut self, bytes: &[u8]) -> Result<()> {
        let s: Signed = postcard::from_bytes(bytes).map_err(|e| Error::Encoding(e.to_string()))?;
        let sig: [u8; 64] = s.sig.as_slice().try_into().map_err(|_| Error::Crypto("bad signature length"))?;
        let body: JoinBody = wire::decode(JOIN_TAG, 1, &s.body)?.1;
        if s.author.0 != body.device.sign {
            return Err(Error::Rejected("join request not signed by the joining device".into()));
        }
        crypto::verify(&s.author.0, &s.body, &sig)?;
        let me = self.identity.id();
        let state = self.groups.get(&body.group).ok_or_else(|| Error::NotFound("group".into()))?;
        if !state.epochs.is_current_member(&me) || state.epochs.is_current_member(&s.author) {
            return Ok(()); // not ours to handle, or already in
        }
        if state.epochs.removed_devices().contains(&s.author) {
            return Err(Error::Rejected("a removed device can't rejoin".into()));
        }
        let Some(inv) = self.invites(&body.group).into_iter().find(|i| i.id() == body.invite_id) else {
            return Err(Error::Rejected("unknown invite".into()));
        };
        if !self.invite_is_open(&inv) {
            return Err(Error::Rejected("invite closed".into()));
        }
        self.add_member(&body.group, body.device)?;
        // Answer where the joiner listens: forward slots under its epoch for a while.
        self.add_answer_epoch(&body.group, body.epoch)?;
        self.events.push(Event::DeviceJoined { group: body.group, device: s.author, via_link: inv.kind == CodeKind::Link });
        Ok(())
    }

    fn answers_path(&self, g: &GroupId) -> std::path::PathBuf {
        self.layout.group_dir(g).join("answer_epochs")
    }

    /// Epochs a joiner may still be listening on (with the time we started answering).
    pub(super) fn answer_epochs(&self, g: &GroupId) -> Vec<(EpochId, u64)> {
        std::fs::read(self.answers_path(g)).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default()
    }

    fn add_answer_epoch(&self, g: &GroupId, e: EpochId) -> Result<()> {
        let now = super::now_ms();
        let mut v: Vec<(EpochId, u64)> = self.answer_epochs(g).into_iter().filter(|(x, t)| *x != e && now.saturating_sub(*t) < super::FORWARD_WINDOW_MS).collect();
        v.push((e, now));
        crate::storage::write_atomic(&self.answers_path(g), &postcard::to_stdvec(&v).map_err(|er| Error::Encoding(er.to_string()))?)
    }

    /// The door of our own pending join for `g`, if it came with one (records format 2).
    fn own_door(&self, g: &GroupId) -> Option<[u8; 32]> {
        if !self.pending_joins().iter().any(|p| p.group == *g) {
            return None;
        }
        std::fs::read(self.layout.group_dir(g).join("join_door")).ok()?.try_into().ok()
    }

    /// The doors this device reads for `g` as (door epoch, door key): its own pending join's, and as a member
    /// every open invite's plus every door it still answers under (T10: the joiner hears back there).
    pub(super) fn read_doors(&self, g: &GroupId) -> Vec<(EpochId, crypto::SymKey)> {
        let mut out = Vec::new();
        if let Some(d) = self.own_door(g) {
            out.push((invite::door_epoch(&d), crypto::SymKey(d)));
        }
        let me = self.identity.id();
        if self.groups.get(g).is_some_and(|s| s.epochs.is_current_member(&me)) {
            let now = super::now_ms();
            let answering: Vec<EpochId> =
                self.answer_epochs(g).into_iter().filter(|(_, t)| now.saturating_sub(*t) < super::FORWARD_WINDOW_MS).map(|(e, _)| e).collect();
            for inv in self.invites(g) {
                let d = invite::door_key(&inv.code, g);
                let e = invite::door_epoch(&d);
                if (answering.contains(&e) || self.invite_is_open(&inv)) && !out.iter().any(|(x, _)| *x == e) {
                    out.push((e, crypto::SymKey(d)));
                }
            }
        }
        out
    }

    /// The key a slot under `epoch` is sealed with: the epoch's own, or a door's.
    pub(super) fn slot_key(&self, g: &GroupId, epoch: &EpochId) -> Option<crypto::SymKey> {
        if let Some(k) = self.groups.get(g).and_then(|s| s.keys.get(epoch)) {
            return Some(k.clone());
        }
        self.read_doors(g).into_iter().find(|(e, _)| e == epoch).map(|(_, k)| k)
    }

    /// Whether `epoch` is one of `g`'s doors (not a real epoch).
    pub(super) fn is_door(&self, g: &GroupId, epoch: &EpochId) -> bool {
        !self.groups.get(g).is_some_and(|s| s.keys.contains_key(epoch)) && self.read_doors(g).iter().any(|(e, _)| e == epoch)
    }

    /// Join-slot targets: for each pending join, a slot under the invite epoch's topic (or its door's).
    pub(super) fn join_targets(&self) -> Vec<(PendingJoin, [u8; 32], [u8; 32])> {
        let mut out = Vec::new();
        for p in self.pending_joins() {
            if !self.groups.contains_key(&p.group) {
                continue;
            }
            let Some(key) = self.slot_key(&p.group, &p.epoch) else { continue };
            let topic = super::topic_for(&key, &p.epoch);
            let mut info = Vec::with_capacity(64);
            info.extend_from_slice(&p.group.0);
            info.extend_from_slice(&p.epoch.0);
            let seed = crypto::derive(&self.identity.secret().to_bytes(), "kt/join-writer/v1", &info).0;
            out.push((p, topic, seed));
        }
        out
    }
}

/// Derivation cache key: (invite id, day, kind).
pub(super) type InviteCache = HashMap<([u8; 32], u32, CodeKind), ([u8; 32], crypto::SymKey)>;
