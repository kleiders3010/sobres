//! Group key epochs (DESIGN §5, D5), validated by every receiver.
//!
//! A group's working key lives in an **epoch**. Epochs form a DAG: each new epoch names
//! its parent(s). Every epoch record is signed by its creator and carries the recipient
//! set (device → key-exchange key), the set of removed devices, and the new epoch key
//! wrapped for each recipient.
//!
//! Rules enforced here (simulated in research E8):
//! 1. **Strong remove:** a removed device can never be re-added, and a device removed in a
//!    concurrent or earlier branch can't add anyone.
//! 2. **Concurrent epochs merge:** a deterministic merger (lowest device ID among the
//!    merged recipients) creates one successor whose recipients are the *intersection*.
//! 3. **No ghosts:** every epoch must be created by a recipient of its parent(s), i.e. a
//!    device that actually holds the current keys, signed with that device's key.
//! 4. **Every new epoch excludes every removal its creator knows about** (removed sets are
//!    inherited from all parents).
//! 5. A removal carries **per-document cutoffs** for the removed device: the counter up to
//!    which the remover had seen that device's ops. Receivers reject the removed device's
//!    ops beyond the cutoff, and the post-removal document epoch is rebuilt at the cutoff,
//!    so all members converge even if some imported late ops before learning of the removal.
//!
//! A removal signed by a device that a concurrent counting removal removes is refused, and a refused
//! removal changes nothing at all. Mutual removal (A removes B while B removes A) has one winner: the
//! one a third member built on, else the device that has been in the group longer. A group has one
//! genesis: a second parentless record is refused.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::crypto::{self, DevicePublic, SymKey, WrappedKey};
use crate::identity::DeviceIdentity;
use crate::ids::{DeviceId, DocId, EpochId, GroupId};
use crate::{wire, Error, Result};

const TAG: [u8; 2] = *b"EP";
const VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EpochKind {
    Genesis,
    /// `history`: keys of earlier epochs, wrapped for the added device only, so a new member
    /// can read the group's existing content (sharing a list shares its contents).
    Add { device: DevicePublic, history: BTreeMap<EpochId, WrappedKey> },
    Remove { device: DeviceId, cutoffs: BTreeMap<DocId, i32> },
    Rotate,
    Merge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochBody {
    pub group: GroupId,
    pub parents: Vec<EpochId>,
    pub creator: DeviceId,
    pub kind: EpochKind,
    pub recipients: BTreeMap<DeviceId, [u8; 32]>,
    pub removed: BTreeSet<DeviceId>,
    pub created_ms: u64,
    pub wrapped: BTreeMap<DeviceId, WrappedKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochRecord {
    pub body: EpochBody,
    pub sig: Vec<u8>,
}

impl EpochRecord {
    fn body_bytes(body: &EpochBody) -> Vec<u8> {
        postcard::to_stdvec(body).expect("epoch body serializes")
    }

    pub fn id(&self) -> EpochId {
        EpochId(crypto::hash("kt/epoch-id/v1", &Self::body_bytes(&self.body)))
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wire::encode(TAG, VERSION, self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(wire::decode(TAG, VERSION, bytes)?.1)
    }

    fn verify_signature(&self) -> Result<()> {
        let sig: [u8; 64] = self.sig.as_slice().try_into().map_err(|_| Error::Crypto("bad signature length"))?;
        crypto::verify(&self.body.creator.0, &Self::body_bytes(&self.body), &sig)
    }
}

/// Associated data binding a wrapped epoch key to its epoch context and recipient.
fn wrap_aad(group: &GroupId, parents: &[EpochId], creator: &DeviceId, created_ms: u64, recipient: &DeviceId) -> Vec<u8> {
    let mut v = Vec::with_capacity(32 * (3 + parents.len()) + 8);
    v.extend_from_slice(&group.0);
    for p in parents {
        v.extend_from_slice(&p.0);
    }
    v.extend_from_slice(&creator.0);
    v.extend_from_slice(&created_ms.to_le_bytes());
    v.extend_from_slice(&recipient.0);
    v
}

fn history_aad(group: &GroupId, epoch: &EpochId, recipient: &DeviceId) -> Vec<u8> {
    let mut v = Vec::with_capacity(100);
    v.extend_from_slice(b"kt/history/v1");
    v.extend_from_slice(&group.0);
    v.extend_from_slice(&epoch.0);
    v.extend_from_slice(&recipient.0);
    v
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, PartialEq, Eq)]
pub enum InsertOutcome {
    Accepted,
    AlreadyKnown,
    /// Parents not known yet; kept and retried when they arrive.
    Pending,
}

/// The epoch graph of one group, as known by one device.
/// See [`GroupEpochs::merge_if_mine`].
pub const MERGE_FALLBACK_MS: u64 = 6 * 3600 * 1000;

#[derive(Default, Clone)]
pub struct GroupEpochs {
    group: Option<GroupId>,
    records: HashMap<EpochId, EpochRecord>,
    pending: HashMap<EpochId, EpochRecord>,
    /// Structurally valid epochs (signature, parents, recipient/removed arithmetic).
    structural: BTreeSet<EpochId>,
    /// Fully valid epochs after the strong-remove / ghost rules. Recomputed on every change.
    valid: BTreeSet<EpochId>,
    /// Epochs an invite named (round 5, S8). While set and no genesis is known, a genesis is only accepted
    /// once one of these arrives and it is that epoch's founding record: a forged genesis sent first can't win.
    anchors: BTreeSet<EpochId>,
    /// When each record arrived here, by this device's clock (second-pass audit T2-2): another device's written time
    /// is never later than this, so a record dated in the future can't stay "newest" or hold off a key change.
    arrived: HashMap<EpochId, u64>,
}

impl GroupEpochs {
    pub fn new(group: GroupId) -> Self {
        GroupEpochs { group: Some(group), ..Default::default() }
    }

    pub fn group(&self) -> Option<GroupId> {
        self.group
    }

    pub fn record(&self, id: &EpochId) -> Option<&EpochRecord> {
        self.records.get(id)
    }

    pub fn all_records(&self) -> impl Iterator<Item = &EpochRecord> {
        self.records.values()
    }

    /// A record's time for decisions on this device: its written time, cut back to when it arrived here (T2-2, rule 51).
    pub fn time_of(&self, id: &EpochId) -> Option<u64> {
        let r = self.records.get(id)?;
        Some(self.arrived.get(id).map_or(r.body.created_ms, |a| r.body.created_ms.min(*a)))
    }

    /// Position in the graph: how many records come before this one (T2-2: "most recent" by position, not by a
    /// written time).
    pub fn depth(&self, id: &EpochId) -> Option<usize> {
        self.records.contains_key(id).then(|| self.ancestors_of(id).len())
    }

    /// When each record arrived here, for keeping across restarts (second-pass Sobres audit S3-8).
    pub fn arrivals(&self) -> Vec<(EpochId, u64)> {
        self.arrived.iter().map(|(k, v)| (*k, *v)).collect()
    }

    /// Put back arrival times kept from before a restart: the earlier time wins (records are inserted again on
    /// load, which would otherwise date them all to the start).
    pub fn restore_arrivals(&mut self, saved: &[(EpochId, u64)]) {
        for (id, t) in saved {
            if let Some(a) = self.arrived.get_mut(id) {
                *a = (*a).min(*t);
            }
        }
    }

    pub fn is_valid(&self, id: &EpochId) -> bool {
        self.valid.contains(id)
    }

    /// Insert a record received from anywhere. Invalid records are rejected with an error.
    pub fn insert(&mut self, rec: EpochRecord) -> Result<InsertOutcome> {
        let id = rec.id();
        if self.records.contains_key(&id) {
            return Ok(InsertOutcome::AlreadyKnown);
        }
        if let Some(g) = self.group {
            if rec.body.group != g {
                return Err(Error::Rejected("epoch for another group".into()));
            }
        }
        rec.verify_signature()?;
        let waiting_for_anchor = rec.body.parents.is_empty() && self.awaiting_anchor();
        if waiting_for_anchor || !rec.body.parents.iter().all(|p| self.records.contains_key(p)) {
            self.pending.insert(id, rec);
            return Ok(self.resolve_anchor().unwrap_or(InsertOutcome::Pending));
        }
        self.check_structure(&rec)?;
        if self.group.is_none() {
            self.group = Some(rec.body.group);
        }
        self.records.insert(id, rec);
        self.arrived.insert(id, now_ms());
        self.structural.insert(id);
        self.drain_pending();
        Ok(InsertOutcome::Accepted)
    }

    /// Name the epoch an invite gave this device (S8). Only matters before a genesis is known.
    pub fn set_anchor(&mut self, epoch: EpochId) {
        if !self.records.values().any(|r| r.body.parents.is_empty()) {
            self.anchors.insert(epoch);
            let _ = self.resolve_anchor();
        }
    }

    fn awaiting_anchor(&self) -> bool {
        !self.anchors.is_empty() && !self.records.values().any(|r| r.body.parents.is_empty())
    }

    /// Once an anchored epoch is here (pending or known), accept the genesis it descends from.
    fn resolve_anchor(&mut self) -> Option<InsertOutcome> {
        if !self.awaiting_anchor() {
            return None;
        }
        for a in self.anchors.clone() {
            let mut cur = a;
            let mut steps = 0;
            let genesis = loop {
                let Some(r) = self.pending.get(&cur).or_else(|| self.records.get(&cur)) else { break None };
                if r.body.parents.is_empty() {
                    break Some(cur);
                }
                cur = r.body.parents[0];
                steps += 1;
                if steps > 100_000 {
                    break None;
                }
            };
            let Some(gid) = genesis else { continue };
            let Some(g) = self.pending.remove(&gid) else { continue };
            if self.check_structure(&g).is_err() {
                continue;
            }
            self.records.insert(gid, g);
            self.arrived.insert(gid, now_ms());
            self.structural.insert(gid);
            self.anchors.clear();
            self.drain_pending();
            return Some(InsertOutcome::Accepted);
        }
        None
    }

    fn drain_pending(&mut self) {
        // Try pending records whose parents may now be complete (never another genesis: one per group).
        loop {
            let ready: Vec<EpochId> = self
                .pending
                .iter()
                .filter(|(_, r)| !r.body.parents.is_empty() && r.body.parents.iter().all(|p| self.records.contains_key(p)))
                .map(|(k, _)| *k)
                .collect();
            if ready.is_empty() {
                break;
            }
            for pid in ready {
                let r = self.pending.remove(&pid).expect("present");
                if self.check_structure(&r).is_ok() {
                    self.records.insert(pid, r);
                    self.arrived.insert(pid, now_ms());
                    self.structural.insert(pid);
                }
            }
        }
        self.recompute_validity();
    }

    fn check_structure(&self, rec: &EpochRecord) -> Result<()> {
        let b = &rec.body;
        let bad = |m: &str| Err(Error::Rejected(format!("epoch: {m}")));
        let mut sorted = b.parents.clone();
        sorted.sort();
        sorted.dedup();
        if sorted != b.parents {
            return bad("parents must be sorted and unique");
        }
        if b.wrapped.keys().collect::<Vec<_>>() != b.recipients.keys().collect::<Vec<_>>() {
            return bad("wrapped keys must match recipients exactly");
        }
        if b.recipients.keys().any(|d| b.removed.contains(d)) {
            return bad("recipients include a removed device (rule 4)");
        }
        if b.parents.is_empty() {
            if b.kind != EpochKind::Genesis || !b.removed.is_empty() || !b.recipients.contains_key(&b.creator) {
                return bad("invalid genesis");
            }
            // One founding record per group: the first one this device accepted is pinned, any other is refused on
            // every path. A second genesis would let anyone who ever held a key of the group start a branch of
            // their own (dead list, or a branch with more members that everyone then encrypts to).
            if self.records.values().any(|r| r.body.parents.is_empty()) {
                return bad("second genesis for a group that already has one");
            }
            return Ok(());
        }
        let parents: Vec<&EpochRecord> = b.parents.iter().map(|p| &self.records[p]).collect();
        if parents.iter().any(|p| !self.structural.contains(&p.id())) {
            return bad("parent is invalid");
        }
        // Rule 3: the creator holds the keys of every parent.
        if parents.iter().any(|p| !p.body.recipients.contains_key(&b.creator)) {
            return bad("creator is not a recipient of the parent epoch (rule 3)");
        }
        // Rule 4: removed sets are inherited.
        let inherited: BTreeSet<DeviceId> = parents.iter().flat_map(|p| p.body.removed.iter().copied()).collect();
        match &b.kind {
            EpochKind::Genesis => return bad("genesis with parents"),
            EpochKind::Add { device, .. } => {
                if parents.len() != 1 {
                    return bad("add must have one parent");
                }
                let dev = DeviceId(device.sign);
                if inherited.contains(&dev) {
                    return bad("re-adding a removed device (rule 1)");
                }
                let mut expected = parents[0].body.recipients.clone();
                expected.insert(dev, device.kex);
                if b.recipients != expected || b.removed != inherited {
                    return bad("add recipient arithmetic");
                }
            }
            EpochKind::Remove { device, .. } => {
                if parents.len() != 1 {
                    return bad("remove must have one parent");
                }
                // Only a current member can be removed (S1: no banning a device before it is ever added).
                if !parents[0].body.recipients.contains_key(device) {
                    return bad("removing a device that is not a member");
                }
                let mut removed = inherited.clone();
                removed.insert(*device);
                let expected: BTreeMap<_, _> =
                    parents[0].body.recipients.iter().filter(|(d, _)| !removed.contains(d)).map(|(d, k)| (*d, *k)).collect();
                if b.recipients != expected || b.removed != removed {
                    return bad("remove recipient arithmetic");
                }
            }
            EpochKind::Rotate => {
                if parents.len() != 1 {
                    return bad("rotate must have one parent");
                }
                if b.recipients != parents[0].body.recipients || b.removed != inherited || !b.recipients.contains_key(&b.creator) {
                    return bad("rotate recipient arithmetic");
                }
            }
            EpochKind::Merge => {
                if parents.len() < 2 {
                    return bad("merge needs two or more parents");
                }
                let mut inter: BTreeMap<DeviceId, [u8; 32]> = parents[0].body.recipients.clone();
                for p in &parents[1..] {
                    inter.retain(|d, _| p.body.recipients.contains_key(d));
                }
                inter.retain(|d, _| !inherited.contains(d));
                if b.recipients != inter || b.removed != inherited {
                    return bad("merge recipient arithmetic (rule 2)");
                }
                // The lowest-ID member merges at once and the k-th lowest after a wait (`merge_if_mine`), so any
                // remaining member may sign it: the recipients are fixed by the arithmetic above (S12).
                if !inter.contains_key(&b.creator) {
                    return bad("merge not created by a remaining member (rule 2)");
                }
            }
        }
        Ok(())
    }

    fn ancestors_of(&self, id: &EpochId) -> BTreeSet<EpochId> {
        let mut out = BTreeSet::new();
        let mut stack = vec![*id];
        while let Some(x) = stack.pop() {
            if let Some(r) = self.records.get(&x) {
                for p in &r.body.parents {
                    if out.insert(*p) {
                        stack.push(*p);
                    }
                }
            }
        }
        out
    }

    /// Strong-remove / ghost rules across the whole graph. Deterministic given the set of
    /// known records, so devices with the same records agree.
    ///
    /// Safety audit round 5 (S1): which removals count is decided FIRST, and only removals that count are used
    /// anywhere after that. A refused removal changes nothing: not who is in, not which key, not who may act.
    fn recompute_validity(&mut self) {
        // Phase 1: structural validity (already enforced on insert; descendants of
        // structurally invalid epochs never enter `records`).
        let base: BTreeSet<EpochId> = self.structural.clone();
        let ancestors: HashMap<EpochId, BTreeSet<EpochId>> = base.iter().map(|e| (*e, self.ancestors_of(e))).collect();
        let concurrent = |a: &EpochId, b: &EpochId| a != b && !ancestors[a].contains(b) && !ancestors[b].contains(a);
        let removals: Vec<EpochId> =
            base.iter().filter(|e| matches!(self.records[e].body.kind, EpochKind::Remove { .. })).copied().collect();
        // Seniority: how early a device became a recipient (graph depth of the first record that has it), then id.
        let mut joined: HashMap<DeviceId, usize> = HashMap::new();
        for e in &base {
            let depth = ancestors[e].len();
            for d in self.records[e].body.recipients.keys() {
                joined.entry(*d).and_modify(|v| *v = (*v).min(depth)).or_insert(depth);
            }
        }
        let seniority = |d: &DeviceId| (joined.get(d).copied().unwrap_or(usize::MAX), *d);
        // `x` removes the creator of `r` while the two are concurrent: `x` would refuse `r`.
        // Second-pass audit T2-1: only if `x`'s signer was a member before the two branches split (a recipient of a
        // record both come from). A device added on one branch after the split (a removed device's own helper)
        // carries that branch's removals in its records, but refuses nothing on the other.
        let kills = |x: &EpochId, r: &EpochId| {
            let signer = self.records[x].body.creator;
            concurrent(x, r)
                && self.records[x].body.removed.contains(&self.records[r].body.creator)
                && ancestors[x].intersection(&ancestors[r]).any(|c| self.records[c].body.recipients.contains_key(&signer))
        };
        // Mutual removal (each removes the other's creator): one wins, never both. First the one a third member
        // built on (an epoch after it, not after the other, by someone neither removal removes and whom no removal
        // outside the other's creator targets); then the device that has been in the group longer.
        //
        // Second-pass audit T2-1: only a device that was a recipient of the epochs both removals start from may
        // confirm, or cancel a confirmation. A device added after the split (by either side) is nobody's witness:
        // otherwise a removed device adds a helper device of its own and has it "confirm" the removed device's
        // counter-removal and cancel the real member's confirmation.
        let original = |x: &EpochId, r: &EpochId, d: &DeviceId| {
            ancestors[x].intersection(&ancestors[r]).any(|c| self.records[c].body.recipients.contains_key(d))
        };
        let confirmed = |x: &EpochId, r: &EpochId| {
            let (cx, cr) = (self.records[x].body.creator, self.records[r].body.creator);
            base.iter().any(|y| {
                let by = self.records[y].body.creator;
                ancestors[y].contains(x)
                    && !ancestors[y].contains(r)
                    && by != cx
                    && by != cr
                    && original(x, r, &by)
                    && !self.records[r].body.removed.contains(&by)
                    && !base.iter().any(|z| {
                        !ancestors[z].contains(y)
                            && z != y
                            && self.records[z].body.creator != cr
                            && original(x, r, &self.records[z].body.creator)
                            && matches!(&self.records[z].body.kind, EpochKind::Remove { device, .. } if *device == by)
                    })
            })
        };
        let wins = |a: &EpochId, b: &EpochId| {
            let (ca, cb) = (confirmed(a, b), confirmed(b, a));
            if ca != cb {
                return ca;
            }
            let (sa, sb) = (seniority(&self.records[a].body.creator), seniority(&self.records[b].body.creator));
            sa < sb || (sa == sb && a < b)
        };
        // `x` refuses `r` unless `r` wins their mutual removal.
        let refuses = |x: &EpochId, r: &EpochId| kills(x, r) && !(kills(r, x) && wins(r, x));

        let mut excluded: BTreeSet<EpochId> = BTreeSet::new(); // removals out because an ancestor is invalid
        loop {
            // Decide the removals: one is refused by a counting removal that refuses it, counts when every removal
            // that would refuse it is refused itself; a cycle is broken by seniority.
            let mut decided: BTreeMap<EpochId, bool> = excluded.iter().map(|e| (*e, false)).collect();
            loop {
                let mut progress = false;
                for r in &removals {
                    if decided.contains_key(r) {
                        continue;
                    }
                    let refusers: Vec<&EpochId> = removals.iter().filter(|x| refuses(x, r)).collect();
                    if refusers.iter().any(|x| decided.get(*x) == Some(&true)) {
                        decided.insert(*r, false);
                        progress = true;
                    } else if refusers.iter().all(|x| decided.get(*x) == Some(&false)) {
                        decided.insert(*r, true);
                        progress = true;
                    }
                }
                if progress {
                    continue;
                }
                let open: Vec<&EpochId> = removals.iter().filter(|r| !decided.contains_key(r)).collect();
                let Some(first) = open.iter().min_by(|a, b| {
                    let (sa, sb) = (seniority(&self.records[a].body.creator), seniority(&self.records[b].body.creator));
                    sa.cmp(&sb).then(a.cmp(b))
                }) else {
                    break;
                };
                decided.insert(**first, true);
            }
            let counting: Vec<EpochId> = removals.iter().filter(|r| decided.get(r) == Some(&true)).copied().collect();
            // Phase 2: an Add / Rotate / Merge is invalid if a counting removal that is not its descendant
            // (concurrent or earlier) removed its creator; an Add also if one removed its device.
            let removed_outside = |e: &EpochId, d: &DeviceId| {
                counting.iter().any(|x| x != e && !ancestors[x].contains(e) && self.records[x].body.removed.contains(d))
            };
            let mut invalid: BTreeSet<EpochId> = BTreeSet::new();
            for e in &base {
                let r = &self.records[e];
                let bad = match &r.body.kind {
                    EpochKind::Add { device, .. } => {
                        removed_outside(e, &r.body.creator) || removed_outside(e, &DeviceId(device.sign))
                    }
                    EpochKind::Rotate | EpochKind::Merge => removed_outside(e, &r.body.creator),
                    EpochKind::Remove { .. } => decided.get(e) != Some(&true),
                    EpochKind::Genesis => false,
                };
                if bad {
                    invalid.insert(*e);
                }
            }
            // Descendants of invalid epochs are invalid.
            let valid: BTreeSet<EpochId> = base
                .iter()
                .filter(|e| !invalid.contains(e) && !ancestors[e].iter().any(|a| invalid.contains(a)))
                .copied()
                .collect();
            // A counting removal under an invalid epoch does not count either: decide again without it.
            let newly: Vec<EpochId> = counting.iter().filter(|r| !valid.contains(r)).copied().collect();
            if newly.is_empty() {
                self.valid = valid;
                return;
            }
            excluded.extend(newly);
        }
    }

    /// Valid epochs with no valid child.
    pub fn heads(&self) -> Vec<EpochId> {
        let mut has_child: BTreeSet<EpochId> = BTreeSet::new();
        for e in &self.valid {
            for p in &self.records[e].body.parents {
                has_child.insert(*p);
            }
        }
        self.valid.iter().filter(|e| !has_child.contains(e)).copied().collect()
    }

    /// The single current head, if the graph has converged.
    pub fn head(&self) -> Option<EpochId> {
        let h = self.heads();
        if h.len() == 1 { Some(h[0]) } else { None }
    }

    /// The head to encrypt new data with. Converged: the head. During a fork (concurrent
    /// membership changes, waiting for the merge): the branch with the most recipients,
    /// ties broken by lowest ID, so every member picks the same one without talking and
    /// sync never waits for the merging device. Never a branch that still includes a
    /// device removed on any branch (strong remove); if no branch qualifies, `None` until
    /// the merge.
    pub fn working_head(&self) -> Option<EpochId> {
        let heads = self.heads();
        if heads.len() == 1 {
            return Some(heads[0]);
        }
        let removed = self.removed_devices();
        heads
            .into_iter()
            .filter(|h| self.records[h].body.recipients.keys().all(|d| !removed.contains(d)))
            .max_by(|a, b| {
                let (ra, rb) = (self.records[a].body.recipients.len(), self.records[b].body.recipients.len());
                ra.cmp(&rb).then(b.cmp(a))
            })
    }

    pub fn recipients(&self, id: &EpochId) -> Option<&BTreeMap<DeviceId, [u8; 32]>> {
        self.records.get(id).map(|r| &r.body.recipients)
    }

    /// Per-document cutoffs for a removed device, merged across all valid removal epochs
    /// (the smallest cutoff wins: the most conservative view of what was legitimately seen).
    pub fn cutoffs_for(&self, device: &DeviceId) -> Option<BTreeMap<DocId, i32>> {
        let mut out: Option<BTreeMap<DocId, i32>> = None;
        for e in &self.valid {
            if let EpochKind::Remove { device: d, cutoffs } = &self.records[e].body.kind {
                if d == device {
                    let acc = out.get_or_insert_with(BTreeMap::new);
                    for (doc, c) in cutoffs {
                        acc.entry(*doc).and_modify(|v| *v = (*v).min(*c)).or_insert(*c);
                    }
                }
            }
        }
        out
    }

    /// Devices removed as known across all valid epochs.
    pub fn removed_devices(&self) -> BTreeSet<DeviceId> {
        self.valid.iter().flat_map(|e| self.records[e].body.removed.iter().copied()).collect()
    }

    /// Whether `me` should confirm a removal now: on the way to the single head there is a removal made by someone
    /// else, `me` is the first of its remaining members other than its creator, and no epoch after it was made by
    /// anyone but that creator.
    pub fn removal_to_confirm(&self, me: &DeviceId) -> bool {
        let Some(head) = self.head() else { return false };
        let mut line = self.ancestors_of(&head);
        line.insert(head);
        line.iter().any(|r| {
            let rec = &self.records[r];
            matches!(rec.body.kind, EpochKind::Remove { .. })
                && rec.body.creator != *me
                && rec.body.recipients.keys().find(|d| **d != rec.body.creator) == Some(me)
                && !line.iter().any(|y| {
                    y != r && self.ancestors_of(y).contains(r) && self.records[y].body.creator != rec.body.creator
                })
        })
    }

    /// Is `device` a recipient of the current head(s)?
    pub fn is_current_member(&self, device: &DeviceId) -> bool {
        let heads = self.heads();
        !heads.is_empty() && heads.iter().all(|h| self.records[h].body.recipients.contains_key(device))
    }

    /// Unwrap the key of epoch `id` for this device.
    pub fn key_for(&self, id: &EpochId, me: &DeviceIdentity) -> Result<SymKey> {
        let r = self.records.get(id).ok_or_else(|| Error::NotFound("epoch".into()))?;
        let w = r.body.wrapped.get(&me.id()).ok_or_else(|| Error::Rejected("not a recipient of this epoch".into()))?;
        let aad = wrap_aad(&r.body.group, &r.body.parents, &r.body.creator, r.body.created_ms, &me.id());
        crypto::unwrap_key(me.secret(), w, &aad)
    }

    /// If several heads exist and this device is the deterministic merger, the merge it should publish.
    pub fn merge_if_mine(&self, me: &DeviceIdentity) -> Option<EpochRecord> {
        let heads = self.heads();
        if heads.len() < 2 {
            return None;
        }
        let parents: Vec<&EpochRecord> = heads.iter().map(|h| &self.records[h]).collect();
        let removed: BTreeSet<DeviceId> = parents.iter().flat_map(|p| p.body.removed.iter().copied()).collect();
        let mut inter = parents[0].body.recipients.clone();
        for p in &parents[1..] {
            inter.retain(|d, _| p.body.recipients.contains_key(d));
        }
        inter.retain(|d, _| !removed.contains(d));
        // Merge duty: the lowest-ID member at once; if the fork persists (that device is
        // away), the k-th lowest after k × MERGE_FALLBACK_MS. A concurrent double merge is
        // just another fork, merged the same way.
        let rank = inter.keys().position(|d| *d == me.id())?;
        let fork_age = now_ms().saturating_sub(heads.iter().filter_map(|h| self.time_of(h)).max().unwrap_or(0));
        if fork_age < rank as u64 * MERGE_FALLBACK_MS {
            return None;
        }
        Some(build(me, self.group?, heads, EpochKind::Merge, inter, removed))
    }

    fn single_head(&self) -> Result<&EpochRecord> {
        let h = self.head().ok_or_else(|| Error::Rejected("epochs have not converged (merge pending)".into()))?;
        Ok(&self.records[&h])
    }

    /// Add a device. `history_keys` are the earlier epoch keys this device holds; they are
    /// wrapped for the new device so it can read existing content.
    pub fn create_add(
        &self,
        me: &DeviceIdentity,
        device: DevicePublic,
        history_keys: &std::collections::HashMap<EpochId, SymKey>,
    ) -> Result<EpochRecord> {
        let head = self.single_head()?;
        let new_dev = DeviceId(device.sign);
        if self.removed_devices().contains(&new_dev) {
            return Err(Error::Rejected("device was removed and can't be re-added".into()));
        }
        let history = history_keys
            .iter()
            .map(|(e, k)| (*e, crypto::wrap_key(&device.kex, k, &history_aad(&head.body.group, e, &new_dev))))
            .collect();
        let mut recipients = head.body.recipients.clone();
        recipients.insert(new_dev, device.kex);
        Ok(build(me, head.body.group, vec![head.id()], EpochKind::Add { device, history }, recipients, head.body.removed.clone()))
    }

    /// Keys of earlier epochs delivered to this device by the Add epoch(s) that added it.
    pub fn history_keys_for(&self, me: &DeviceIdentity) -> Vec<(EpochId, SymKey)> {
        let mut out = Vec::new();
        for e in &self.valid {
            if let EpochKind::Add { device, history } = &self.records[e].body.kind {
                if DeviceId(device.sign) == me.id() {
                    for (eid, w) in history {
                        if let Ok(k) = crypto::unwrap_key(me.secret(), w, &history_aad(&self.records[e].body.group, eid, &me.id())) {
                            out.push((*eid, k));
                        }
                    }
                }
            }
        }
        out
    }

    pub fn create_remove(&self, me: &DeviceIdentity, device: DeviceId, cutoffs: BTreeMap<DocId, i32>) -> Result<EpochRecord> {
        let head = self.single_head()?;
        if !head.body.recipients.contains_key(&device) {
            return Err(Error::Rejected("not a member of this group".into()));
        }
        let mut removed = head.body.removed.clone();
        removed.insert(device);
        let recipients = head.body.recipients.iter().filter(|(d, _)| !removed.contains(d)).map(|(d, k)| (*d, *k)).collect();
        Ok(build(me, head.body.group, vec![head.id()], EpochKind::Remove { device, cutoffs }, recipients, removed))
    }

    pub fn create_rotate(&self, me: &DeviceIdentity) -> Result<EpochRecord> {
        let head = self.single_head()?;
        Ok(build(me, head.body.group, vec![head.id()], EpochKind::Rotate, head.body.recipients.clone(), head.body.removed.clone()))
    }
}

/// Create a new genesis epoch for a new group with `me` as the only recipient.
pub fn genesis(me: &DeviceIdentity, group: GroupId) -> EpochRecord {
    let mut recipients = BTreeMap::new();
    recipients.insert(me.id(), me.public().kex);
    build(me, group, vec![], EpochKind::Genesis, recipients, BTreeSet::new())
}

fn build(
    me: &DeviceIdentity,
    group: GroupId,
    parents: Vec<EpochId>,
    kind: EpochKind,
    recipients: BTreeMap<DeviceId, [u8; 32]>,
    removed: BTreeSet<DeviceId>,
) -> EpochRecord {
    build_at(me, group, parents, kind, recipients, removed, now_ms())
}

/// [`build`] with a written time of the caller's choice (tests: a record dated in the future).
fn build_at(
    me: &DeviceIdentity,
    group: GroupId,
    mut parents: Vec<EpochId>,
    kind: EpochKind,
    recipients: BTreeMap<DeviceId, [u8; 32]>,
    removed: BTreeSet<DeviceId>,
    created_ms: u64,
) -> EpochRecord {
    parents.sort();
    let key = SymKey::random();
    let wrapped = recipients
        .iter()
        .map(|(d, kex)| (*d, crypto::wrap_key(kex, &key, &wrap_aad(&group, &parents, &me.id(), created_ms, d))))
        .collect();
    let body = EpochBody { group, parents, creator: me.id(), kind, recipients, removed, created_ms, wrapped };
    let sig = me.sign(&EpochRecord::body_bytes(&body)).to_vec();
    EpochRecord { body, sig }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{rngs::StdRng, Rng, SeedableRng};

    fn devices(n: usize) -> Vec<DeviceIdentity> {
        (0..n).map(|_| DeviceIdentity::generate()).collect()
    }

    fn setup(n: usize) -> (Vec<DeviceIdentity>, GroupEpochs) {
        let ds = devices(n);
        let g = GroupId::random();
        let mut ge = GroupEpochs::new(g);
        ge.insert(genesis(&ds[0], g)).unwrap();
        for d in &ds[1..] {
            let add = ge.create_add(&ds[0], d.public(), &Default::default()).unwrap();
            ge.insert(add).unwrap();
        }
        (ds, ge)
    }

    #[test]
    fn add_remove_and_keys() {
        let (ds, mut ge) = setup(3);
        let h = ge.head().unwrap();
        for d in &ds {
            assert!(ge.key_for(&h, d).is_ok());
        }
        let rm = ge.create_remove(&ds[0], ds[2].id(), BTreeMap::new()).unwrap();
        ge.insert(rm).unwrap();
        let h2 = ge.head().unwrap();
        assert!(ge.key_for(&h2, &ds[1]).is_ok());
        assert!(ge.key_for(&h2, &ds[2]).is_err(), "removed device can't read the new epoch");
        assert!(ge.create_add(&ds[0], ds[2].public(), &Default::default()).is_err(), "strong remove");
    }

    #[test]
    fn outsider_cannot_create_epochs() {
        let (_ds, mut ge) = setup(2);
        let outsider = DeviceIdentity::generate();
        // Build a well-formed Add signed by a non-member (ghost attempt).
        let head = ge.head().unwrap();
        let rec = ge.records[&head].clone();
        let mut recipients = rec.body.recipients.clone();
        recipients.insert(outsider.id(), outsider.public().kex);
        let ghost = build(&outsider, rec.body.group, vec![head], EpochKind::Add { device: outsider.public(), history: BTreeMap::new() }, recipients, rec.body.removed.clone());
        assert!(ge.insert(ghost).is_err());
    }

    #[test]
    fn forged_signature_rejected() {
        let (ds, mut ge) = setup(2);
        let mut rot = ge.create_rotate(&ds[1]).unwrap();
        rot.body.created_ms += 1; // tamper after signing
        assert!(ge.insert(rot).is_err());
    }

    #[test]
    fn concurrent_removals_merge_to_intersection() {
        let (ds, ge) = setup(4);
        let mut a = ge.clone();
        let mut b = ge.clone();
        let ra = a.create_remove(&ds[0], ds[2].id(), BTreeMap::new()).unwrap();
        let rb = b.create_remove(&ds[1], ds[3].id(), BTreeMap::new()).unwrap();
        a.insert(ra.clone()).unwrap();
        b.insert(rb.clone()).unwrap();
        a.insert(rb).unwrap();
        b.insert(ra).unwrap();
        assert_eq!(a.heads().len(), 2);
        // Exactly one of the two remaining devices is the merger.
        let m0 = a.merge_if_mine(&ds[0]);
        let m1 = a.merge_if_mine(&ds[1]);
        assert!(m0.is_some() ^ m1.is_some());
        let merge = m0.or(m1).unwrap();
        a.insert(merge.clone()).unwrap();
        b.insert(merge).unwrap();
        let h = a.head().unwrap();
        assert_eq!(b.head(), Some(h));
        let rec = a.recipients(&h).unwrap();
        assert!(rec.contains_key(&ds[0].id()) && rec.contains_key(&ds[1].id()));
        assert!(!rec.contains_key(&ds[2].id()) && !rec.contains_key(&ds[3].id()));
    }

    #[test]
    fn concurrent_add_by_removed_device_is_invalid() {
        let (ds, ge) = setup(3);
        let newcomer = DeviceIdentity::generate();
        let mut a = ge.clone();
        let rm = a.create_remove(&ds[0], ds[2].id(), BTreeMap::new()).unwrap();
        // ds[2] concurrently adds a newcomer before learning it was removed.
        let add = ge.create_add(&ds[2], newcomer.public(), &Default::default()).unwrap();
        a.insert(rm).unwrap();
        a.insert(add).unwrap();
        assert!(!a.is_current_member(&newcomer.id()), "strong remove: removed device can't add");
        assert_eq!(a.heads().len(), 1);
    }

    /// Safety audit round 4, T1: anyone who held a group key could send a second genesis and kill the list or start
    /// a branch everyone then encrypts to. pinned: one founding record per group.
    #[test]
    fn second_genesis_refused() {
        let (ds, mut ge) = setup(3);
        let g = ge.group().unwrap();
        let before = ge.head();
        let attacker = DeviceIdentity::generate();
        assert!(ge.insert(genesis(&attacker, g)).is_err(), "an outsider's genesis");
        assert!(ge.insert(genesis(&ds[2], g)).is_err(), "a member's second genesis");
        assert_eq!(ge.head(), before);
        assert!(ds.iter().all(|d| ge.is_current_member(&d.id())));
    }

    /// T2: a removed device signing a removal on the branch where it was still a member. pinned: removal is final
    /// against the removed party once another member built on it.
    #[test]
    fn late_removal_by_removed_device_refused() {
        // 0 removes 2 and builds on it (Replica::remove_member), 1 confirms (Replica::insert_epoch); 2 then removes
        // 0, and 1, from the old head
        let (ds, ge) = setup(3);
        let old = ge.clone();
        let mut a = ge.clone();
        let rm = a.create_remove(&ds[0], ds[2].id(), BTreeMap::new()).unwrap();
        a.insert(rm).unwrap();
        let own = a.create_rotate(&ds[0]).unwrap();
        a.insert(own).unwrap();
        let ack = a.create_rotate(&ds[1]).unwrap();
        a.insert(ack).unwrap();
        let late = old.create_remove(&ds[2], ds[0].id(), BTreeMap::new()).unwrap();
        let mut after = old.clone();
        after.insert(late.clone()).unwrap();
        let chained = after.create_remove(&ds[2], ds[1].id(), BTreeMap::new()).unwrap();
        let sibling = old.create_remove(&ds[2], ds[1].id(), BTreeMap::new()).unwrap();
        for r in [late, chained, sibling] {
            let _ = a.insert(r);
        }
        assert!(a.is_current_member(&ds[0].id()) && a.is_current_member(&ds[1].id()), "the remover and the confirmer stay");
        assert!(!a.is_current_member(&ds[2].id()));
        assert_eq!(a.heads().len(), 1, "the late removals are invalid, so nothing forks");
    }

    /// Round 5, S1: mutual removal nobody built on has one winner, the device in the group longer (never both:
    /// that let a removed device take the remover out with it). pinned: mutual removal has a fixed winner.
    #[test]
    fn concurrent_mutual_removal_senior_wins() {
        let (ds, ge) = setup(3);
        let mut a = ge.clone();
        let ra = a.create_remove(&ds[0], ds[1].id(), BTreeMap::new()).unwrap();
        let rb = ge.create_remove(&ds[1], ds[0].id(), BTreeMap::new()).unwrap();
        a.insert(ra).unwrap();
        a.insert(rb).unwrap();
        assert_eq!(a.heads().len(), 1);
        assert!(a.is_current_member(&ds[0].id()) && !a.is_current_member(&ds[1].id()));
        assert!(a.is_current_member(&ds[2].id()));
    }

    /// The other side wins a mutual removal when a third member built on it, whoever is senior.
    #[test]
    fn mutual_removal_built_on_by_third_member_wins() {
        let (ds, ge) = setup(3);
        let mut a = ge.clone();
        // 1 removes 0 (the senior one); 2 confirms. 0 then removes 1 from the old head.
        let r1 = a.create_remove(&ds[1], ds[0].id(), BTreeMap::new()).unwrap();
        a.insert(r1).unwrap();
        let ack = a.create_rotate(&ds[2]).unwrap();
        a.insert(ack).unwrap();
        let late = ge.create_remove(&ds[0], ds[1].id(), BTreeMap::new()).unwrap();
        a.insert(late).unwrap();
        assert!(!a.is_current_member(&ds[0].id()));
        assert!(a.is_current_member(&ds[1].id()) && a.is_current_member(&ds[2].id()));
        assert_eq!(a.heads().len(), 1);
    }

    /// S1 (a)+(b): a refused late removal changes nothing: people added since stay, the key does not roll back,
    /// and the group can still add and rotate afterwards. pinned: a refused record has no effect.
    #[test]
    fn refused_removal_has_no_effects() {
        let (ds, ge) = setup(3);
        let old = ge.clone();
        let mut a = ge.clone();
        let rm = a.create_remove(&ds[0], ds[2].id(), BTreeMap::new()).unwrap();
        a.insert(rm).unwrap();
        let newcomer = DeviceIdentity::generate();
        let add = a.create_add(&ds[0], newcomer.public(), &Default::default()).unwrap();
        a.insert(add).unwrap();
        let rot = a.create_rotate(&ds[1]).unwrap();
        let rot_id = rot.id();
        a.insert(rot).unwrap();
        for target in [ds[0].id(), ds[1].id()] {
            let late = old.create_remove(&ds[2], target, BTreeMap::new()).unwrap();
            let _ = a.insert(late);
        }
        assert!(a.is_current_member(&newcomer.id()), "added after the removal stays");
        assert!(a.is_valid(&rot_id) && a.head() == Some(rot_id), "the key does not roll back");
        assert!(!a.removed_devices().contains(&ds[0].id()) && !a.removed_devices().contains(&ds[1].id()));
        let another = DeviceIdentity::generate();
        let add2 = a.create_add(&ds[1], another.public(), &Default::default()).unwrap();
        a.insert(add2).unwrap();
        assert!(a.is_current_member(&another.id()), "the group is not frozen");
        let rot2 = a.create_rotate(&ds[0]).unwrap();
        let rot2_id = rot2.id();
        a.insert(rot2).unwrap();
        assert!(a.is_valid(&rot2_id));
    }

    /// S1 (c): two removed devices; one's late removal must not bring the other back with a working key.
    #[test]
    fn second_removed_device_stays_out() {
        let (ds, ge) = setup(4); // 0, 1, c1 = 2, c2 = 3
        let old = ge.clone();
        let mut a = ge.clone();
        let r1 = a.create_remove(&ds[0], ds[2].id(), BTreeMap::new()).unwrap();
        a.insert(r1).unwrap();
        let r2 = a.create_remove(&ds[0], ds[3].id(), BTreeMap::new()).unwrap();
        a.insert(r2).unwrap();
        let late = old.create_remove(&ds[2], ds[0].id(), BTreeMap::new()).unwrap();
        a.insert(late).unwrap();
        assert!(a.is_current_member(&ds[0].id()) && a.is_current_member(&ds[1].id()));
        assert!(!a.is_current_member(&ds[3].id()) && a.removed_devices().contains(&ds[3].id()));
        let h = a.working_head().unwrap();
        assert!(a.key_for(&h, &ds[3]).is_err(), "c2 can't open the head key");
        assert_eq!(a.heads().len(), 1);
    }

    /// S1 (d): a two-device group (or one's own account with one device left): nobody can confirm, so the fixed
    /// winner decides and the remover stays.
    #[test]
    fn two_device_group_remover_stays() {
        let (ds, ge) = setup(2);
        let mut a = ge.clone();
        let rm = a.create_remove(&ds[0], ds[1].id(), BTreeMap::new()).unwrap();
        a.insert(rm).unwrap();
        let late = ge.create_remove(&ds[1], ds[0].id(), BTreeMap::new()).unwrap();
        a.insert(late).unwrap();
        assert_eq!(a.heads().len(), 1);
        assert!(a.is_current_member(&ds[0].id()) && !a.is_current_member(&ds[1].id()));
        assert!(a.working_head().is_some());
    }

    /// S1 (e): two removed devices each removing a different member, from the old head.
    #[test]
    fn two_removed_devices_cannot_split_the_group() {
        let (ds, ge) = setup(4);
        let old = ge.clone();
        let mut a = ge.clone();
        let r1 = a.create_remove(&ds[0], ds[2].id(), BTreeMap::new()).unwrap();
        a.insert(r1).unwrap();
        let r2 = a.create_remove(&ds[1], ds[3].id(), BTreeMap::new()).unwrap();
        a.insert(r2).unwrap();
        let l1 = old.create_remove(&ds[2], ds[0].id(), BTreeMap::new()).unwrap();
        let l2 = old.create_remove(&ds[3], ds[1].id(), BTreeMap::new()).unwrap();
        a.insert(l1).unwrap();
        a.insert(l2).unwrap();
        assert!(a.is_current_member(&ds[0].id()) && a.is_current_member(&ds[1].id()));
        assert!(!a.is_current_member(&ds[2].id()) && !a.is_current_member(&ds[3].id()));
        assert_eq!(a.heads().len(), 1);
    }

    /// Second-pass T2-1: the removed device's records on the old head: it removes `victims[0]`, adds a helper device
    /// of its own, and the helper rotates and removes `victims[1..]`.
    fn helper_attack(old: &GroupEpochs, attacker: &DeviceIdentity, victims: &[DeviceId]) -> (DeviceIdentity, Vec<EpochRecord>) {
        let helper = DeviceIdentity::generate();
        let mut b = old.clone();
        let mut out = vec![];
        let rm = b.create_remove(attacker, victims[0], BTreeMap::new()).unwrap();
        b.insert(rm.clone()).unwrap();
        let rm_id = rm.id();
        out.push(rm);
        let add = b.create_add(attacker, helper.public(), &Default::default()).unwrap();
        b.insert(add.clone()).unwrap();
        let add_id = add.id();
        out.push(add);
        let rot = b.create_rotate(&helper).unwrap_or_else(|e| panic!("{e}: heads {}, valid {:?}", b.heads().len(), [rm_id, add_id].map(|x| b.is_valid(&x))));
        b.insert(rot.clone()).unwrap();
        out.push(rot);
        for v in &victims[1..] {
            let rm = b.create_remove(&helper, *v, BTreeMap::new()).unwrap();
            b.insert(rm.clone()).unwrap();
            out.push(rm);
        }
        (helper, out)
    }

    /// T2-1, three members: Ana (0) removes Xavi (2), Berta (1) confirms; Xavi removes Ana from the old head, adds
    /// X9 and X9 "confirms" and removes Berta. pinned: a device added after the split confirms and cancels nothing.
    #[test]
    fn removed_device_helper_cannot_confirm_or_cancel() {
        for attacker_first in [false, true] {
            let (ds, ge) = setup(3);
            let mut a = ge.clone();
            let rm = a.create_remove(&ds[0], ds[2].id(), BTreeMap::new()).unwrap();
            a.insert(rm.clone()).unwrap();
            let ack = a.create_rotate(&ds[1]).unwrap();
            a.insert(ack.clone()).unwrap();
            let (helper, recs) = helper_attack(&ge, &ds[2], &[ds[0].id(), ds[1].id()]);
            let mut g = ge.clone();
            let mut all: Vec<EpochRecord> = if attacker_first { recs.clone() } else { vec![rm.clone(), ack.clone()] };
            all.extend(if attacker_first { vec![rm, ack] } else { recs });
            for r in all {
                let _ = g.insert(r);
            }
            assert!(g.is_current_member(&ds[0].id()) && g.is_current_member(&ds[1].id()), "Ana and Berta stay ({attacker_first})");
            assert!(!g.is_current_member(&ds[2].id()) && !g.is_current_member(&helper.id()), "Xavi and X9 are out ({attacker_first})");
            let h = g.working_head().unwrap();
            assert!(g.key_for(&h, &ds[2]).is_err() && g.key_for(&h, &helper).is_err());
            assert!(g.key_for(&h, &ds[0]).is_ok() && g.key_for(&h, &ds[1]).is_ok());
        }
    }

    /// T2-1, two members (or one's own account with two devices): Ana (0, senior) removes Xavi; Xavi removes Ana and
    /// adds a helper that builds on Xavi's removal. pinned: the helper is no third member.
    #[test]
    fn removed_device_helper_two_members() {
        let (ds, ge) = setup(2);
        let mut a = ge.clone();
        let rm = a.create_remove(&ds[0], ds[1].id(), BTreeMap::new()).unwrap();
        a.insert(rm).unwrap();
        let (helper, recs) = helper_attack(&ge, &ds[1], &[ds[0].id()]);
        for r in recs {
            let _ = a.insert(r);
        }
        assert!(a.is_current_member(&ds[0].id()));
        assert!(!a.is_current_member(&ds[1].id()) && !a.is_current_member(&helper.id()));
    }

    /// T2-1, "the confirmer is kept in": X9 removes nobody, only builds on Xavi's removal of Ana. Berta (1), who
    /// confirmed Ana's removal of Xavi, stays a member, and Ana wins.
    #[test]
    fn removed_device_helper_cannot_keep_the_confirmer_next_to_it() {
        let (ds, ge) = setup(3);
        let mut a = ge.clone();
        let rm = a.create_remove(&ds[0], ds[2].id(), BTreeMap::new()).unwrap();
        a.insert(rm).unwrap();
        let ack = a.create_rotate(&ds[1]).unwrap();
        a.insert(ack).unwrap();
        let (helper, recs) = helper_attack(&ge, &ds[2], &[ds[0].id()]);
        for r in recs {
            let _ = a.insert(r);
        }
        assert!(a.is_current_member(&ds[0].id()) && a.is_current_member(&ds[1].id()));
        assert!(!a.is_current_member(&ds[2].id()) && !a.is_current_member(&helper.id()));
        assert_eq!(a.heads().len(), 1);
    }
    /// Second-pass T2-2: a key change dated 100 years ahead is "now" on this device (its arrival), not the newest
    /// record for good, and the most recent epochs are picked by their place in the graph. pinned: arrival clamp.
    #[test]
    fn a_record_dated_in_the_future_is_cut_back_to_its_arrival() {
        let (ds, mut ge) = setup(2);
        let head = ge.head().unwrap();
        let rec = ge.records[&head].clone();
        let far = now_ms() + 100 * 365 * 24 * 3600 * 1000;
        let rot = build_at(&ds[1], rec.body.group, vec![head], EpochKind::Rotate, rec.body.recipients.clone(), rec.body.removed.clone(), far);
        let id = rot.id();
        ge.insert(rot).unwrap();
        assert!(ge.time_of(&id).unwrap() <= now_ms(), "cut back to its arrival");
        assert_eq!(ge.depth(&id), Some(ge.depth(&head).unwrap() + 1));
    }
    /// S1: a device that is not a member can't be "removed" ahead of time.
    #[test]
    fn removing_a_non_member_refused() {
        let (ds, mut ge) = setup(2);
        let outsider = DeviceIdentity::generate();
        let head = ge.head().unwrap();
        let rec = ge.records[&head].clone();
        let mut removed = rec.body.removed.clone();
        removed.insert(outsider.id());
        let pre = build(&ds[0], rec.body.group, vec![head], EpochKind::Remove { device: outsider.id(), cutoffs: BTreeMap::new() }, rec.body.recipients.clone(), removed);
        assert!(ge.insert(pre).is_err());
    }

    /// S8: a joiner that knows the invite's epoch takes that epoch's genesis, even when a forged one comes first.
    /// pinned: the invite decides the genesis.
    #[test]
    fn forged_genesis_sent_first_loses_to_the_invites_epoch() {
        let (ds, ge) = setup(2);
        let g = ge.group().unwrap();
        let invite_epoch = ge.head().unwrap();
        let mut joiner = GroupEpochs::new(g);
        joiner.set_anchor(invite_epoch);
        let attacker = DeviceIdentity::generate();
        assert_eq!(joiner.insert(genesis(&attacker, g)).unwrap(), InsertOutcome::Pending);
        let mut real: Vec<EpochRecord> = ge.all_records().cloned().collect();
        real.sort_by_key(|r| r.body.parents.len()); // genesis first, like the sync order
        for r in real {
            let _ = joiner.insert(r);
        }
        assert_eq!(joiner.head(), Some(invite_epoch));
        assert!(joiner.is_current_member(&ds[0].id()) && joiner.is_current_member(&ds[1].id()));
        assert!(!joiner.is_current_member(&attacker.id()));
    }

    /// S12: when the lowest-ID member is away, the next one's merge is accepted.
    #[test]
    fn merge_by_another_remaining_member_accepted() {
        let (ds, ge) = setup(4);
        let mut a = ge.clone();
        let mut b = ge.clone();
        let ra = a.create_remove(&ds[0], ds[2].id(), BTreeMap::new()).unwrap();
        let rb = b.create_remove(&ds[1], ds[3].id(), BTreeMap::new()).unwrap();
        a.insert(ra.clone()).unwrap();
        a.insert(rb).unwrap();
        let heads = a.heads();
        assert_eq!(heads.len(), 2);
        let parents: Vec<&EpochRecord> = heads.iter().map(|h| &a.records[h]).collect();
        let removed: BTreeSet<DeviceId> = parents.iter().flat_map(|p| p.body.removed.iter().copied()).collect();
        let mut inter = parents[0].body.recipients.clone();
        inter.retain(|d, _| parents[1].body.recipients.contains_key(d) && !removed.contains(d));
        let second = *inter.keys().nth(1).unwrap();
        let who = ds.iter().find(|d| d.id() == second).unwrap();
        let m = build(who, a.group().unwrap(), heads, EpochKind::Merge, inter, removed);
        a.insert(m).unwrap();
        assert_eq!(a.heads().len(), 1);
    }

    /// Randomized property test mirroring research E8: concurrent adds/removes with random
    /// delivery order must never give a removed device a later key, and must converge.
    #[test]
    fn randomized_concurrency_property() {
        for seed in 0..300u64 {
            let mut rng = StdRng::seed_from_u64(seed);
            let n = rng.gen_range(3..6);
            let (ds, ge) = setup(n);
            let mut views: Vec<GroupEpochs> = (0..n).map(|_| ge.clone()).collect();
            let mut outbox: Vec<EpochRecord> = Vec::new();
            for _ in 0..rng.gen_range(1..4) {
                let actor = rng.gen_range(0..n);
                if !views[actor].is_current_member(&ds[actor].id()) || views[actor].head().is_none() {
                    continue;
                }
                let target = rng.gen_range(0..n);
                let rec = if rng.gen_bool(0.7) && target != actor {
                    views[actor].create_remove(&ds[actor], ds[target].id(), BTreeMap::new()).ok()
                } else {
                    views[actor].create_rotate(&ds[actor]).ok()
                };
                if let Some(r) = rec {
                    views[actor].insert(r.clone()).unwrap();
                    outbox.push(r);
                }
            }
            // Deliver everything to everyone in random order, then run merges until quiet.
            for _round in 0..6 {
                for v in views.iter_mut() {
                    let mut msgs = outbox.clone();
                    for i in (1..msgs.len()).rev() {
                        let j = rng.gen_range(0..=i);
                        msgs.swap(i, j);
                    }
                    for m in msgs {
                        let _ = v.insert(m);
                    }
                }
                let mut new = Vec::new();
                for (i, v) in views.iter().enumerate() {
                    if let Some(m) = v.merge_if_mine(&ds[i]) {
                        new.push(m);
                    }
                }
                if new.is_empty() {
                    break;
                }
                outbox.extend(new);
            }
            // Invariant 1 (I1): no epoch created by a device that knew of a removal lists the removed device.
            for v in &views {
                for r in v.all_records() {
                    for removed in &r.body.removed {
                        assert!(!r.body.recipients.contains_key(removed), "seed {seed}: removed device is a recipient");
                    }
                }
            }
            // Invariant 2 (I2): remaining members converge on one head.
            let removed = views[0].removed_devices();
            let heads: BTreeSet<Vec<EpochId>> = views
                .iter()
                .enumerate()
                .filter(|(i, _)| !removed.contains(&ds[*i].id()))
                .map(|(_, v)| v.heads())
                .collect();
            assert!(heads.len() <= 1, "seed {seed}: members disagree on heads: {heads:?}");
            if let Some(h) = heads.iter().next() {
                assert!(h.len() <= 1 || removed.len() >= n - 1, "seed {seed}: no convergence");
            }
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let (_ds, ge) = setup(2);
        let h = ge.head().unwrap();
        let r = ge.record(&h).unwrap().clone();
        let bytes = r.encode().unwrap();
        assert_eq!(EpochRecord::decode(&bytes).unwrap(), r);
    }
}
