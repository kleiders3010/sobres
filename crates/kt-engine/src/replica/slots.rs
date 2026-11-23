//! Mailbox slots (DESIGN §7, §7a).
//!
//! Per group, each member device keeps one replaceable **slot** per epoch topic. Rewriting
//! a slot replaces the previous version, so a mailbox holds at most one slot per device and
//! group, whatever the number of edits.
//!
//! - **Main slot** (under the current head's topic): this device's own ops since the start
//!   of each document's current generation; the generation's proposal if this device made
//!   it (or its proposer left); and the key-epoch records this device must carry.
//! - **Forward slot** (under a parent epoch's topic, for [`FORWARD_WINDOW_MS`] after an
//!   epoch change): only the epoch records, so members that don't know the new epoch yet
//!   find it under the topic they still read.
//!
//! The whole slot is sealed with its epoch's key (header bound as associated data), so
//! mailboxes see only rotating topics and ciphertext. This also encrypts control messages
//! on every network path. Everything inside stays individually signed and goes through
//! [`Replica::receive`], so a slot is exactly as trusted as its contents.

use serde::{Deserialize, Serialize};

use super::{Message, Replica};
use crate::crypto::{self, SymKey};
use crate::envelope::{Envelope, Inner, MessageKind};
use crate::ids::{DeviceId, DocId, EpochId, GroupId};
use crate::{wire, Error, Result};
use loro::{ExportMode, IdSpan};

const SLOT_TAG: [u8; 2] = *b"SL";
const SLOT_VERSION: u16 = 1;
/// How long a device keeps writing forward slots under the previous epoch's topic.
pub const FORWARD_WINDOW_MS: u64 = 30 * 24 * 3600 * 1000;
/// How many recent epochs' topics a device reads (plus all current heads).
pub const READ_EPOCHS: usize = 4;

/// Network-visible slot address, derived from the epoch key: unknown to non-members and
/// changes with every key epoch.
pub fn topic_for(key: &SymKey, epoch: &EpochId) -> [u8; 32] {
    let mut m = Vec::with_capacity(64);
    m.extend_from_slice(&key.0);
    m.extend_from_slice(&epoch.0);
    crypto::hash("kt/topic/v1", &m)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotRole {
    Main,
    Forward,
    /// A joining device's request, under the invite epoch's topic.
    Join,
}

/// A slot this device should write.
#[derive(Clone, Debug)]
pub struct SlotTarget {
    pub group: GroupId,
    pub epoch: EpochId,
    pub topic: [u8; 32],
    pub role: SlotRole,
    /// Seed for the mailbox-level writer key (e.g. a Nostr key): per device, group and
    /// epoch, so a device's slots in different groups or epochs aren't linkable by key.
    pub writer_seed: [u8; 32],
}

/// A topic this device should read.
#[derive(Clone, Debug)]
pub struct ReadTopic {
    pub group: GroupId,
    pub epoch: EpochId,
    pub topic: [u8; 32],
}

/// "I'm on these document generations", signed by the member. Lets devices stop carrying
/// their previous generation's ops (prevgen) once every member has switched.
#[derive(Serialize, Deserialize)]
struct StatusBody {
    group: GroupId,
    device: DeviceId,
    created_ms: u64,
    /// Per document: the generation and what this device has of it (encoded version
    /// vector). Lets members compact only at versions everyone active acknowledged (D26).
    acks: Vec<(DocId, u32, Vec<u8>)>,
}

/// Status format 2: adds the device's feature level (the group's feature floor).
#[derive(Serialize, Deserialize)]
struct StatusBodyV2 {
    group: GroupId,
    device: DeviceId,
    created_ms: u64,
    acks: Vec<(DocId, u32, Vec<u8>)>,
    features: u32,
}

/// What a member last told us (its newest status).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MemberStatus {
    pub created_ms: u64,
    pub acks: std::collections::BTreeMap<DocId, (u32, Vec<u8>)>,
    /// The device's feature level (1 for status format 1).
    pub features: u32,
}

/// Our acknowledgments as last published (they change with every import; republished
/// with the slot's own changes, or at most every [`ACK_INTERVAL_MS`] on their own).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct PublishedAcks {
    pub at_ms: u64,
    pub acks: Vec<(DocId, u32, Vec<u8>)>,
}

pub const ACK_INTERVAL_MS: u64 = 24 * 3600 * 1000;

#[derive(Serialize, Deserialize)]
struct StatusRecord {
    body: Vec<u8>,
    sig: Vec<u8>,
}

const STATUS_TAG: [u8; 2] = *b"ST";
const WHERE_TAG: [u8; 2] = *b"WH";

#[derive(Serialize, Deserialize)]
struct WhereBody {
    group: GroupId,
    device: DeviceId,
    created_ms: u64,
    data: Vec<u8>,
}

/// A member device's latest whereabouts, as received.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberWhereabouts {
    pub device: DeviceId,
    pub created_ms: u64,
    pub data: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct SlotPayload {
    group: GroupId,
    epoch: EpochId,
    messages: Vec<Vec<u8>>,
}

/// What reading one slot did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlotReport {
    pub messages: usize,
    /// Messages that were rejected (bad signature, removed author, ...).
    pub failed: usize,
    /// Slots that didn't decrypt or had a wrong header.
    pub bad_slots: usize,
    /// Messages parked waiting for something not yet received (epoch, earlier generation).
    /// Parked messages live in memory only, so callers should read these slots again later.
    pub parked: usize,
}

pub(super) fn slot_aad(group: &GroupId, epoch: &EpochId, topic: &[u8; 32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(110);
    v.extend_from_slice(b"kt/slot/v1");
    v.extend_from_slice(&group.0);
    v.extend_from_slice(&epoch.0);
    v.extend_from_slice(topic);
    v
}

impl Replica {
    /// The slots this device should write now.
    pub fn slot_targets(&self) -> Vec<SlotTarget> {
        let now = super::now_ms();
        let me = self.identity.id();
        let mut out = Vec::new();
        for (g, state) in &self.groups {
            if !state.epochs.is_current_member(&me) {
                continue;
            }
            let mut push = |epoch: EpochId, role: SlotRole| {
                if let Some(key) = state.keys.get(&epoch) {
                    if out.iter().any(|t: &SlotTarget| t.group == *g && t.epoch == epoch) {
                        return;
                    }
                    let mut info = Vec::with_capacity(64);
                    info.extend_from_slice(&g.0);
                    info.extend_from_slice(&epoch.0);
                    let seed = crypto::derive(&self.identity.secret().to_bytes(), "kt/slot-writer/v1", &info);
                    out.push(SlotTarget { group: *g, epoch, topic: topic_for(key, &epoch), role, writer_seed: seed.0 });
                }
            };
            let heads = state.epochs.heads();
            for h in &heads {
                push(*h, SlotRole::Main);
            }
            // Forward under the parents of every recent epoch on the way back from the heads, not only the heads'
            // own parents: after two quick changes (a removal and its fresh key) a member still listening two
            // steps back must hear of both.
            let mut stack: Vec<EpochId> = heads.clone();
            let mut seen: std::collections::BTreeSet<EpochId> = std::collections::BTreeSet::new();
            while let Some(e) = stack.pop() {
                if !seen.insert(e) {
                    continue;
                }
                let Some(rec) = state.epochs.record(&e) else { continue };
                if now.saturating_sub(state.epochs.time_of(&e).unwrap_or(rec.body.created_ms)) < FORWARD_WINDOW_MS {
                    for p in &rec.body.parents {
                        push(*p, SlotRole::Forward);
                        stack.push(*p);
                    }
                }
            }
            // Epochs a device we just added may still be listening on (see invites).
            for (e, since) in self.answer_epochs(g) {
                if now.saturating_sub(since) < FORWARD_WINDOW_MS {
                    push(e, SlotRole::Forward);
                }
            }
            // ...and the doors they asked at (T10): the answer there is the group's epoch records.
            let answering = self.answer_epochs(g);
            for (e, key) in self.read_doors(g) {
                let asked = answering.iter().any(|(x, since)| *x == e && now.saturating_sub(*since) < FORWARD_WINDOW_MS);
                if asked && !out.iter().any(|t| t.group == *g && t.epoch == e) {
                    let mut info = Vec::with_capacity(64);
                    info.extend_from_slice(&g.0);
                    info.extend_from_slice(&e.0);
                    let seed = crypto::derive(&self.identity.secret().to_bytes(), "kt/slot-writer/v1", &info);
                    out.push(SlotTarget { group: *g, epoch: e, topic: topic_for(&key, &e), role: SlotRole::Forward, writer_seed: seed.0 });
                }
            }
        }
        for (p, topic, seed) in self.join_targets() {
            if !out.iter().any(|t| t.group == p.group) {
                out.push(SlotTarget { group: p.group, epoch: p.epoch, topic, role: SlotRole::Join, writer_seed: seed });
            }
        }
        out
    }

    /// The topics this device should read: every current head plus the most recent epochs
    /// it holds keys for (to catch forward slots and members that haven't switched yet).
    pub fn read_topics(&self) -> Vec<ReadTopic> {
        let mut out = Vec::new();
        for (g, state) in &self.groups {
            let mut epochs: Vec<(u64, EpochId)> = state
                .keys
                .keys()
                // T2-2: most recent by position in the graph, never by a written time; a refused record is not read at all.
                .filter(|e| state.epochs.record(e).is_none() || state.epochs.is_valid(e))
                .map(|e| (state.epochs.depth(e).unwrap_or(0) as u64, *e))
                .collect();
            epochs.sort_by(|a, b| b.cmp(a));
            let mut chosen: Vec<EpochId> = state.epochs.heads().into_iter().filter(|h| state.keys.contains_key(h)).collect();
            for (_, e) in epochs {
                if chosen.len() >= READ_EPOCHS.max(state.epochs.heads().len()) {
                    break;
                }
                if !chosen.contains(&e) {
                    chosen.push(e);
                }
            }
            for e in chosen {
                out.push(ReadTopic { group: *g, epoch: e, topic: topic_for(&state.keys[&e], &e) });
            }
            // Doors (T10): members hear join requests there, a joiner hears its answer.
            for (e, key) in self.read_doors(g) {
                if !out.iter().any(|t| t.group == *g && t.epoch == e) {
                    out.push(ReadTopic { group: *g, epoch: e, topic: topic_for(&key, &e) });
                }
            }
        }
        out
    }

    /// What a member writes under a door it answers at: every epoch record of the group (the joiner has nothing
    /// else to check its Add against), and nothing more.
    fn door_answer(&self, g: &GroupId) -> Result<Vec<Vec<u8>>> {
        let state = &self.groups[g];
        let mut recs: Vec<_> = state.epochs.all_records().collect();
        recs.sort_by_key(|r| (r.body.created_ms, r.id()));
        recs.into_iter().map(|r| r.encode()).collect()
    }

    /// Build the sealed contents of a slot. Deterministic in content (not in bytes: the
    /// nonce is random), so callers compare [`Replica::slot_fingerprint`] to skip rewrites.
    pub fn slot_blob(&mut self, t: &SlotTarget) -> Result<Vec<u8>> {
        if t.role == SlotRole::Main {
            // The slot is being written anyway: acknowledgments ride along.
            self.refresh_acks(&t.group, true)?;
        }
        let messages = self.slot_messages(t)?;
        let key = self.slot_key(&t.group, &t.epoch).ok_or_else(|| Error::NotFound("slot key".into()))?;
        let payload = wire::encode(SLOT_TAG, SLOT_VERSION, &SlotPayload { group: t.group, epoch: t.epoch, messages })?;
        Ok(crypto::seal(&key, &slot_aad(&t.group, &t.epoch, &t.topic), &payload))
    }

    /// A hash of what a slot would contain now (changes only when its content does).
    pub fn slot_fingerprint(&mut self, t: &SlotTarget) -> Result<[u8; 32]> {
        let mut h = Vec::new();
        for m in self.slot_messages_fingerprint_input(t)? {
            h.extend_from_slice(&crypto::hash("kt/slot-item/v1", &m));
        }
        let base = crypto::hash("kt/slot-fp/v1", &h);
        if t.role != SlotRole::Main {
            return Ok(base);
        }
        // Acknowledgments ride along for free when the slot changes anyway; on their own
        // they're republished at most every ACK_INTERVAL_MS (no extra writes per import).
        let changed = self.last_base_fp.get(&(t.group, t.epoch)) != Some(&base);
        self.last_base_fp.insert((t.group, t.epoch), base);
        self.refresh_acks(&t.group, changed)?;
        let acks = postcard::to_stdvec(&self.published_acks(&t.group).acks).unwrap_or_default();
        let mut m = base.to_vec();
        m.extend_from_slice(&crypto::hash("kt/slot-acks/v1", &acks));
        Ok(crypto::hash("kt/slot-fp/v2", &m))
    }

    fn acks_path(&self, g: &GroupId) -> std::path::PathBuf {
        self.layout.group_dir(g).join("acks_published")
    }

    pub(super) fn published_acks(&self, g: &GroupId) -> PublishedAcks {
        std::fs::read(self.acks_path(g)).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default()
    }

    fn current_acks(&self, g: &GroupId) -> Vec<(DocId, u32, Vec<u8>)> {
        self.groups.get(g).map(|s| s.docs.iter().map(|(d, ds)| (*d, ds.generation, ds.doc.oplog_vv().encode())).collect()).unwrap_or_default()
    }

    fn refresh_acks(&mut self, g: &GroupId, slot_changing: bool) -> Result<()> {
        let now = super::now_ms();
        let published = self.published_acks(g);
        let current = self.current_acks(g);
        if current == published.acks {
            return Ok(());
        }
        if slot_changing || published.at_ms == 0 || now.saturating_sub(published.at_ms) >= self.ack_interval_ms {
            let p = PublishedAcks { at_ms: now, acks: current };
            crate::storage::write_atomic(&self.acks_path(g), &postcard::to_stdvec(&p).map_err(|e| Error::Encoding(e.to_string()))?)?;
        }
        Ok(())
    }

    fn slot_messages_fingerprint_input(&mut self, t: &SlotTarget) -> Result<Vec<Vec<u8>>> {
        // Envelopes carry random nonces, so fingerprint the plaintext ingredients instead.
        let me_peer = self.identity.peer_id();
        if t.role == SlotRole::Join {
            return Ok(vec![b"join".to_vec(), t.epoch.0.to_vec()]);
        }
        if t.role == SlotRole::Forward && self.is_door(&t.group, &t.epoch) {
            return self.door_answer(&t.group);
        }
        let state = self.groups.get(&t.group).ok_or_else(|| Error::NotFound("group".into()))?;
        let mut out: Vec<Vec<u8>> = self.carried_records(&t.group)?.into_iter().collect();
        out.extend(self.invite_messages(&t.group)?);
        out.extend(self.mailbox_set_messages(&t.group));
        out.extend(self.newer_messages(&t.group));
        if t.role == SlotRole::Main {
            for (d, ds) in &state.docs {
                let cur = ds.doc.oplog_vv().get(&me_peer).copied().unwrap_or(0);
                let mut v = d.0.to_vec();
                v.extend_from_slice(&ds.generation.to_le_bytes());
                v.extend_from_slice(&cur.to_le_bytes());
                v.extend_from_slice(&ds.gen_base.to_le_bytes());
                v.push(self.prevgen_envelope(&t.group, d).is_some() as u8);
                out.push(v);
            }
            out.push(t.epoch.0.to_vec());
            if let Some(w) = &self.whereabouts {
                out.push(w.clone());
            }
            // Handovers must go out even when nothing else changed.
            out.extend(self.handover_messages(&t.group)?);
            // Health reports ride along with real changes, and go out at least daily.
            if self.health.is_some() {
                out.push(b"health".iter().copied().chain((super::now_ms() / super::health::HEALTH_INTERVAL_MS).to_le_bytes()).collect());
            }
        }
        Ok(out)
    }

    /// Own ops of the previous document generation, carried while some current member isn't
    /// known to have switched (and at most FORWARD_WINDOW_MS).
    fn prevgen_envelope(&self, g: &GroupId, d: &DocId) -> Option<Vec<u8>> {
        let b = std::fs::read(self.layout.doc_dir(g, d).join("prevgen")).ok()?;
        if b.len() < 8 {
            return None;
        }
        let at = u64::from_le_bytes(b[..8].try_into().ok()?);
        if super::now_ms().saturating_sub(at) >= FORWARD_WINDOW_MS {
            return None;
        }
        let state = self.groups.get(g)?;
        let mine = state.docs.get(d)?.generation;
        let me = self.identity.id();
        let known = self.member_generations(g);
        let head = state.epochs.working_head()?;
        let lagging = state
            .epochs
            .recipients(&head)?
            .keys()
            .filter(|m| **m != me)
            .any(|m| known.get(&(*m, *d)).copied().unwrap_or(0) < mine);
        lagging.then(|| b[8..].to_vec())
    }

    /// Set what this device publishes as its whereabouts (transport-defined bytes, e.g. an
    /// iroh endpoint address). Changing it changes the slot content (so it gets rewritten).
    pub fn set_whereabouts(&mut self, data: Option<Vec<u8>>) {
        self.whereabouts = data;
    }

    /// Latest whereabouts of the other current members of `g`.
    pub fn member_whereabouts(&self, g: &GroupId) -> Vec<MemberWhereabouts> {
        let Some(state) = self.groups.get(g) else { return Vec::new() };
        let me = self.identity.id();
        self.stored_whereabouts(g)
            .into_iter()
            .filter(|w| w.device != me && state.epochs.is_current_member(&w.device))
            .collect()
    }

    fn stored_whereabouts(&self, g: &GroupId) -> Vec<MemberWhereabouts> {
        std::fs::read(self.layout.group_dir(g).join("whereabouts"))
            .ok()
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default()
    }

    /// The device a whereabouts record claims to be from (unverified).
    pub(super) fn whereabouts_device(&self, bytes: &[u8]) -> Option<DeviceId> {
        let rec = postcard::from_bytes::<StatusRecord>(bytes).ok()?;
        Some(wire::decode::<WhereBody>(WHERE_TAG, 1, &rec.body).ok()?.1.device)
    }

    /// Whether a whereabouts record claims (and is signed by) `device`.
    pub(super) fn whereabouts_signed_by(&self, bytes: &[u8], device: &DeviceId) -> bool {
        let Ok(rec) = postcard::from_bytes::<StatusRecord>(bytes) else { return false };
        let Ok((_, body)) = wire::decode::<WhereBody>(WHERE_TAG, 1, &rec.body) else { return false };
        let Ok(sig) = <[u8; 64]>::try_from(rec.sig.as_slice()) else { return false };
        body.device == *device && crypto::verify(&device.0, &rec.body, &sig).is_ok()
    }

    pub(super) fn whereabouts_record(&self, g: &GroupId) -> Result<Option<Vec<u8>>> {
        let Some(data) = &self.whereabouts else { return Ok(None) };
        let body = WhereBody { group: *g, device: self.identity.id(), created_ms: super::now_ms(), data: data.clone() };
        let body = wire::encode(WHERE_TAG, 1, &body)?;
        let sig = self.identity.sign(&body).to_vec();
        Ok(Some(postcard::to_stdvec(&StatusRecord { body, sig }).map_err(|e| Error::Encoding(e.to_string()))?))
    }

    /// Our own signed whereabouts record for `g` (a peer-to-peer membership proof).
    pub fn own_whereabouts_record(&self, g: &GroupId) -> Option<Vec<u8>> {
        self.whereabouts_record(g).ok().flatten()
    }

    /// Verify and store a whereabouts record received directly from a peer. Returns the
    /// group, the (current member) device and the transport data it signed.
    pub fn accept_whereabouts_record(&mut self, bytes: &[u8]) -> Result<(GroupId, DeviceId, Vec<u8>)> {
        self.receive_whereabouts(bytes)?;
        let rec: StatusRecord = postcard::from_bytes(bytes).map_err(|e| Error::Encoding(e.to_string()))?;
        let body: WhereBody = wire::decode(WHERE_TAG, 1, &rec.body)?.1;
        Ok((body.group, body.device, body.data))
    }

    pub(super) fn receive_whereabouts(&mut self, bytes: &[u8]) -> Result<()> {
        let rec: StatusRecord = postcard::from_bytes(bytes).map_err(|e| Error::Encoding(e.to_string()))?;
        let body: WhereBody = wire::decode(WHERE_TAG, 1, &rec.body)?.1;
        let sig: [u8; 64] = rec.sig.as_slice().try_into().map_err(|_| Error::Crypto("bad signature length"))?;
        crypto::verify(&body.device.0, &rec.body, &sig)?;
        let state = self.groups.get(&body.group).ok_or_else(|| Error::NotFound("group".into()))?;
        if !state.epochs.is_current_member(&body.device) || body.data.len() > 4096 {
            return Err(Error::Rejected("whereabouts from a non-member or too large".into()));
        }
        let mut all = self.stored_whereabouts(&body.group);
        if all.iter().any(|w| w.device == body.device && w.created_ms >= body.created_ms) {
            return Ok(());
        }
        all.retain(|w| w.device != body.device);
        all.push(MemberWhereabouts { device: body.device, created_ms: body.created_ms, data: body.data });
        let b = postcard::to_stdvec(&all).map_err(|e| Error::Encoding(e.to_string()))?;
        crate::storage::write_atomic(&self.layout.group_dir(&body.group).join("whereabouts"), &b)
    }

    fn status_record(&self, g: &GroupId) -> Result<Vec<u8>> {
        // Current generations always (they change rarely and drive prevgen carry); version
        // vectors as last published (see refresh_acks).
        let published = self.published_acks(g).acks;
        let acks = self.groups[g]
            .docs
            .iter()
            .map(|(d, ds)| {
                let vv = published
                    .iter()
                    .find(|(pd, pg, _)| pd == d && *pg == ds.generation)
                    .map(|(_, _, v)| v.clone())
                    .unwrap_or_else(|| ds.doc.oplog_vv().encode());
                (*d, ds.generation, vv)
            })
            .collect();
        let body = StatusBodyV2 { group: *g, device: self.identity.id(), created_ms: super::now_ms(), acks, features: super::FEATURE_LEVEL };
        let body = wire::encode(STATUS_TAG, 2, &body)?;
        let sig = self.identity.sign(&body).to_vec();
        postcard::to_stdvec(&StatusRecord { body, sig }).map_err(|e| Error::Encoding(e.to_string()))
    }

    /// Newest status of every other member that told us one.
    pub fn member_status(&self, g: &GroupId) -> std::collections::BTreeMap<DeviceId, MemberStatus> {
        std::fs::read(self.layout.group_dir(g).join("member_status")).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default()
    }

    /// When another current member last wrote its slot here (its status record's time, never later than now):
    /// "Last update from the others" on a shared list (cold UX review U-T4). None if nobody else has yet.
    pub fn others_last_status_ms(&self, g: &GroupId) -> Option<u64> {
        let me = self.identity.id();
        let state = self.groups.get(g)?;
        let now = super::now_ms();
        self.member_status(g)
            .into_iter()
            .filter(|(m, _)| *m != me && state.epochs.is_current_member(m))
            .map(|(_, s)| s.created_ms.min(now))
            .max()
    }

    /// Known document generations of other members: (device, doc) -> generation.
    fn member_generations(&self, g: &GroupId) -> std::collections::BTreeMap<(DeviceId, DocId), u32> {
        self.member_status(g).into_iter().flat_map(|(dev, st)| st.acks.into_iter().map(move |(d, (gen_, _))| ((dev, d), gen_))).collect()
    }

    pub(super) fn receive_status(&mut self, bytes: &[u8]) -> Result<()> {
        let rec: StatusRecord = postcard::from_bytes(bytes).map_err(|e| Error::Encoding(e.to_string()))?;
        // Format 1 (no feature level) and 2 both read; newer ones are kept and passed on.
        let body = match rec.body.get(2..4).map(|v| u16::from_le_bytes([v[0], v[1]])) {
            Some(1) => {
                let b: StatusBody = wire::decode(STATUS_TAG, 1, &rec.body)?.1;
                StatusBodyV2 { group: b.group, device: b.device, created_ms: b.created_ms, acks: b.acks, features: 1 }
            }
            _ => wire::decode::<StatusBodyV2>(STATUS_TAG, 2, &rec.body)?.1,
        };
        let sig: [u8; 64] = rec.sig.as_slice().try_into().map_err(|_| Error::Crypto("bad signature length"))?;
        crypto::verify(&body.device.0, &rec.body, &sig)?;
        let state = self.groups.get(&body.group).ok_or_else(|| Error::NotFound("group".into()))?;
        if !state.epochs.is_current_member(&body.device) {
            return Err(Error::Rejected("status from a non-member".into()));
        }
        if body.acks.len() > 10_000 {
            return Err(Error::Rejected("status too large".into()));
        }
        let mut all = self.member_status(&body.group);
        if all.get(&body.device).is_some_and(|s| s.created_ms >= body.created_ms) {
            return Ok(());
        }
        all.insert(body.device, MemberStatus { created_ms: body.created_ms, acks: body.acks.into_iter().map(|(d, g, v)| (d, (g, v))).collect(), features: body.features });
        let b = postcard::to_stdvec(&all).map_err(|e| Error::Encoding(e.to_string()))?;
        crate::storage::write_atomic(&self.layout.group_dir(&body.group).join("member_status"), &b)
    }

    /// Epoch records this device carries: its own, plus those whose creator is no longer a
    /// member (nobody else would republish them).
    fn carried_records(&self, g: &GroupId) -> Result<Vec<Vec<u8>>> {
        let state = &self.groups[g];
        let me = self.identity.id();
        let mut recs: Vec<_> = state.epochs.all_records().filter(|r| r.body.creator == me || !state.epochs.is_current_member(&r.body.creator)).collect();
        // Deterministic order (the store is a hash map): stable slot fingerprints across runs.
        recs.sort_by_key(|r| (r.body.created_ms, r.id()));
        recs.into_iter().map(|r| r.encode()).collect()
    }

    fn slot_messages(&mut self, t: &SlotTarget) -> Result<Vec<Vec<u8>>> {
        if t.role == SlotRole::Join {
            let p = self.pending_joins().into_iter().find(|p| p.group == t.group).ok_or_else(|| Error::NotFound("pending join".into()))?;
            return Ok(vec![Message::JoinRequest(self.join_request(&p)?).encode()?]);
        }
        if t.role == SlotRole::Forward && self.is_door(&t.group, &t.epoch) {
            return self.door_answer(&t.group)?.into_iter().map(|r| Message::Epoch(r).encode()).collect();
        }
        let mut messages = Vec::new();
        for r in self.carried_records(&t.group)? {
            messages.push(Message::Epoch(r).encode()?);
        }
        // Newer apps' messages we can't read ourselves, passed on untouched.
        messages.extend(self.newer_messages(&t.group));
        if t.role == SlotRole::Forward {
            return Ok(messages);
        }
        let me = self.identity.id();
        let me_peer = self.identity.peer_id();
        let state = &self.groups[&t.group];
        // Envelopes go under the working head (during a fork, the deterministic safe branch).
        if state.epochs.working_head() != Some(t.epoch) || !state.epochs.is_current_member(&me) {
            return Ok(messages);
        }
        let key = state.keys.get(&t.epoch).cloned().ok_or_else(|| Error::NotFound("head key".into()))?;
        let docs: Vec<_> = state.docs.keys().copied().collect();
        messages.push(Message::Status(self.status_record(&t.group)?).encode()?);
        for ms in self.mailbox_set_messages(&t.group) {
            messages.push(Message::MailboxSet(ms).encode()?);
        }
        for inv in self.invite_messages(&t.group)? {
            messages.push(Message::Invite(inv).encode()?);
        }
        if let Some(w) = self.whereabouts_record(&t.group)? {
            messages.push(Message::Whereabouts(w).encode()?);
        }
        for h in self.handover_messages(&t.group)? {
            messages.push(Message::Handover(h).encode()?);
        }
        if let Some(h) = self.health_record(&t.group)? {
            messages.push(Message::Health(h).encode()?);
        }
        for d in docs {
            let ds = &self.groups[&t.group].docs[&d];
            // The generation's proposal (the document's base), if nobody else carries it.
            if ds.generation > 0 {
                if let Some(first) = crate::storage::read_records(&self.layout.doc_updates(&t.group, &d))?.into_iter().next() {
                    if let Ok(env) = Envelope::decode(&first) {
                        let carries = env.sender == me || !state.epochs.is_current_member(&env.sender);
                        if carries {
                            if let Some(k) = state.keys.get(&env.epoch) {
                                if env.open(&t.group, &state.epochs, k).map(|i| i.kind == MessageKind::DocEpochProposal).unwrap_or(false) {
                                    messages.push(Message::Envelope(first).encode()?);
                                }
                            }
                        }
                    }
                }
            }
            if let Some(env) = self.prevgen_envelope(&t.group, &d) {
                messages.push(Message::Envelope(env).encode()?);
            }
            let cur = ds.doc.oplog_vv().get(&me_peer).copied().unwrap_or(0);
            if cur > ds.gen_base {
                let span = IdSpan::new(me_peer, ds.gen_base, cur);
                let update = ds.doc.export(ExportMode::updates_in_range(vec![span])).map_err(|e| Error::Loro(e.to_string()))?;
                let inner = Inner {
                    group: t.group,
                    doc: d,
                    generation: ds.generation,
                    kind: MessageKind::DocUpdate,
                    peer: me_peer,
                    created_ms: super::now_ms(),
                    payload: update,
                };
                messages.push(Message::Envelope(Envelope::seal(&self.identity, t.epoch, &key, &inner)?.encode()?).encode()?);
            }
        }
        Ok(messages)
    }

    /// Open one slot read from a mailbox and apply everything in it.
    pub fn open_slot(&mut self, t: &ReadTopic, blob: &[u8]) -> Result<SlotReport> {
        self.open_slots(t, &[blob])
    }

    /// Open all slots fetched for one topic and apply them. Epoch records from **every**
    /// slot go first, so a removal published by one member is known before a removed
    /// device's late edits in another slot are considered (the remaining cross-fetch race is
    /// closed by document epochs at removal cutoffs, D24). Untrusted input: each message is
    /// verified on its own, and one bad slot or message doesn't block the rest.
    pub fn open_slots(&mut self, t: &ReadTopic, blobs: &[&[u8]]) -> Result<SlotReport> {
        // A door can close between listing topics and opening them (the join it served just finished): nothing to
        // open there any more.
        let Some(key) = self.slot_key(&t.group, &t.epoch) else {
            if self.groups.get(&t.group).is_some_and(|s| !s.keys.is_empty()) {
                return Ok(SlotReport::default());
            }
            return Err(Error::NotFound("slot key".into()));
        };
        let aad = slot_aad(&t.group, &t.epoch, &t.topic);
        let mut report = SlotReport::default();
        let mut epochs = Vec::new();
        let mut others = Vec::new();
        for blob in blobs {
            let payload: SlotPayload = match crypto::open(&key, &aad, blob)
                .map_err(Error::from)
                .and_then(|p| wire::decode(SLOT_TAG, SLOT_VERSION, &p).map(|x| x.1))
            {
                Ok(p) => p,
                Err(_) => {
                    report.bad_slots += 1;
                    continue;
                }
            };
            if payload.group != t.group || payload.epoch != t.epoch {
                report.bad_slots += 1;
                continue;
            }
            report.messages += payload.messages.len();
            for m in payload.messages {
                if matches!(Message::decode(&m), Ok(Message::Epoch(_))) {
                    epochs.push(m);
                } else {
                    others.push(m);
                }
            }
        }
        let parked_before = self.events.iter().filter(|e| matches!(e, super::Event::Parked)).count();
        for m in epochs.iter().chain(others.iter()) {
            match self.receive(m) {
                Ok(()) => {}
                // A newer app's message: kept and passed on, not a failure (principle 8).
                Err(Error::Newer(_)) => {
                    let _ = self.keep_newer(&t.group, m);
                }
                Err(_) => report.failed += 1,
            }
        }
        report.parked = self.events.iter().filter(|e| matches!(e, super::Event::Parked)).count().saturating_sub(parked_before);
        Ok(report)
    }
}
