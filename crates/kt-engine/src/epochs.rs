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
//! Mutual removal (A removes B while B removes A) removes both: removal epochs are never
//! invalidated by a concurrent removal of their creator.

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
        if !rec.body.parents.iter().all(|p| self.records.contains_key(p)) {
            self.pending.insert(id, rec);
            return Ok(InsertOutcome::Pending);
        }
        self.check_structure(&rec)?;
        if self.group.is_none() {
            self.group = Some(rec.body.group);
        }
        self.records.insert(id, rec);
        self.structural.insert(id);
        // Try pending records whose parents may now be complete.
        loop {
            let ready: Vec<EpochId> = self
                .pending
                .iter()
                .filter(|(_, r)| r.body.parents.iter().all(|p| self.records.contains_key(p)))
                .map(|(k, _)| *k)
                .collect();
            if ready.is_empty() {
                break;
            }
            for pid in ready {
                let r = self.pending.remove(&pid).expect("present");
                if self.check_structure(&r).is_ok() {
                    self.records.insert(pid, r);
                    self.structural.insert(pid);
                }
            }
        }
        self.recompute_validity();
        Ok(InsertOutcome::Accepted)
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
                if inter.keys().next() != Some(&b.creator) {
                    return bad("merge not created by the deterministic merger (rule 2)");
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
    fn recompute_validity(&mut self) {
        // Phase 1: structural validity (already enforced on insert; descendants of
        // structurally invalid epochs never enter `records`).
        let base: BTreeSet<EpochId> = self.structural.clone();
        // Phase 2: an Add / Rotate / Merge is invalid if its creator was removed in an epoch
        // that is not its descendant (concurrent or earlier), and an Add is invalid if its
        // device was removed anywhere. Removal epochs are never invalidated this way
        // (mutual removal removes both).
        let mut invalid: BTreeSet<EpochId> = BTreeSet::new();
        let ancestors: HashMap<EpochId, BTreeSet<EpochId>> = base.iter().map(|e| (*e, self.ancestors_of(e))).collect();
        for e in &base {
            let r = &self.records[e];
            let creator = r.body.creator;
            let removes_creator_elsewhere = base.iter().any(|x| {
                x != e && !ancestors[x].contains(e) && self.records[x].body.removed.contains(&creator)
            });
            let invalid_here = match &r.body.kind {
                EpochKind::Add { device, .. } => {
                    // A device removed in any epoch that doesn't descend from this Add was
                    // removed before or concurrently: this Add is a (re-)add after removal.
                    let dev = DeviceId(device.sign);
                    removes_creator_elsewhere
                        || base.iter().any(|x| {
                            x != e && !ancestors[x].contains(e) && self.records[x].body.removed.contains(&dev)
                        })
                }
                EpochKind::Rotate | EpochKind::Merge => removes_creator_elsewhere,
                EpochKind::Remove { .. } | EpochKind::Genesis => false,
            };
            if invalid_here {
                invalid.insert(*e);
            }
        }
        // Descendants of invalid epochs are invalid.
        let valid: BTreeSet<EpochId> = base
            .iter()
            .filter(|e| !invalid.contains(e) && !ancestors[e].iter().any(|a| invalid.contains(a)))
            .copied()
            .collect();
        self.valid = valid;
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
        let fork_age = now_ms().saturating_sub(parents.iter().map(|p| p.body.created_ms).max().unwrap_or(0));
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
    mut parents: Vec<EpochId>,
    kind: EpochKind,
    recipients: BTreeMap<DeviceId, [u8; 32]>,
    removed: BTreeSet<DeviceId>,
) -> EpochRecord {
    parents.sort();
    let key = SymKey::random();
    let created_ms = now_ms();
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
