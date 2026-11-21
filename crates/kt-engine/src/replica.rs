//! One device's view of all its groups and documents: the engine's main entry point.
//!
//! Transport-agnostic (principle 4): the replica produces wire messages ([`Replica::outbox`])
//! and consumes them ([`Replica::receive`]) in any order, with duplicates. Transports (M2)
//! only move bytes.
//!
//! Receiving a document update:
//! 1. the envelope's epoch must be known (otherwise the message is parked);
//! 2. signature, membership, decryption and PeerID binding (see `envelope`);
//! 3. removal cutoffs (D5 rule 5);
//! 4. quarantine check, then the import sandbox validates against a copy of the live doc
//!    and proves the update touches only the sender's own PeerID (D2, D3);
//! 5. import into the live doc and persist the raw envelope (kept for relaying to members).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use loro::{ExportMode, IdSpan, LoroDoc, VersionVector};
use serde::{Deserialize, Serialize};

use crate::crypto::{DevicePublic, SymKey};
use crate::envelope::{Envelope, Inner, MessageKind};
use crate::epochs::{self, EpochRecord, GroupEpochs, InsertOutcome};
use crate::identity::{DeviceIdentity, IdentityLock};
use crate::ids::{peer_id_for, DeviceId, DocId, EpochId, GroupId};
use crate::sandbox::{self, Quarantine, SandboxConfig, Verdict};
use crate::storage::{self, Layout};
use crate::{crypto, wire, Error, Result};

mod discovery;
mod invites;
mod mailboxset;
pub use mailboxset::{MailboxSet, PREVIOUS_SET_READ_MS};
pub use invites::{OpenInvite, PendingJoin};
pub mod backup;
pub mod handover;
pub mod health;
mod rotation;
mod newer;
pub use rotation::{DEFAULT_ROTATE_AFTER_MS, ROTATE_CASCADE_MS};
mod fuzzing;
mod compaction;
mod slots;
pub use compaction::CompactionPolicy;
pub use slots::MemberStatus;
pub use discovery::{DhtLookup, DhtPublish, DHT_VALUE_MAX, LAN_WINDOW_SECS};
pub use slots::{topic_for, MemberWhereabouts, ReadTopic, SlotReport, SlotRole, SlotTarget, FORWARD_WINDOW_MS, READ_EPOCHS};

const MSG_TAG: [u8; 2] = *b"MS";
const MSG_VERSION: u16 = 1;
/// Save a fresh snapshot after this many imported updates.
const SNAPSHOT_EVERY: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    Epoch(Vec<u8>),
    Envelope(Vec<u8>),
    /// A member's signed per-document generation report (see `slots`).
    Status(Vec<u8>),
    /// A member device's signed, transport-defined "where am I" (network addresses), shared
    /// only inside sealed slots (DESIGN §6 discovery).
    Whereabouts(Vec<u8>),
    /// A member's signed open-invite record (so any member can re-post and accept).
    Invite(Vec<u8>),
    /// A joining device's signed request (in its join slot).
    JoinRequest(Vec<u8>),
    /// The group's signed mailbox set (which relays its devices use).
    MailboxSet(Vec<u8>),
    /// Another group handed to a linked device of the account (see `handover`).
    Handover(Vec<u8>),
    /// A member's signed helper/relay health observations (see `health`).
    Health(Vec<u8>),
}

/// How many message kinds this build knows. Kinds are only ever appended (principle 8),
/// so a higher kind number is a newer app's, not garbage.
const KNOWN_MESSAGE_KINDS: u8 = 9;

impl Message {
    pub fn encode(&self) -> Result<Vec<u8>> {
        wire::encode(MSG_TAG, MSG_VERSION, self)
    }
    pub fn decode(b: &[u8]) -> Result<Self> {
        match wire::decode(MSG_TAG, MSG_VERSION, b) {
            Ok((_, m)) => Ok(m),
            // postcard puts the kind first (a varint; any known kind fits in one byte).
            Err(Error::Encoding(_)) if b.len() > 4 && b[..2] == MSG_TAG && b[4] >= KNOWN_MESSAGE_KINDS => {
                Err(Error::Newer(format!("message kind {}", b[4])))
            }
            Err(e) => Err(e),
        }
    }
}

/// This build's feature level, advertised in status records: a group uses a new kind of
/// content only once every current member's device supports it (the feature floor, D28).
pub const FEATURE_LEVEL: u32 = 1;

/// App-specific knowledge the engine needs for document epochs (D24). The engine stays
/// generic (principle 7); the domain crate implements this.
pub trait DocSemantics: Send + Sync {
    /// Canonical representation of the live state; equal means "same list for the user".
    fn normalized(&self, doc: &LoroDoc) -> Vec<u8>;
    /// A fresh document with the same live state (no history, no tombstones), authored by `peer`.
    fn rebuild(&self, doc: &LoroDoc, peer: u64) -> Result<LoroDoc>;
    /// Re-apply the changes that turned `base` into `local` onto `target`, as new ops.
    fn rebase(&self, base: &LoroDoc, local: &LoroDoc, target: &LoroDoc) -> Result<()>;
}

#[derive(Serialize, Deserialize)]
struct EpochProposal {
    generation: u32,
    base_vv: Vec<u8>,
    snapshot: Vec<u8>,
}

/// Things the host (UI, logs, tests) may want to know about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Imported { group: GroupId, doc: DocId, author: DeviceId },
    Rejected { reason: String },
    Quarantined { author: DeviceId },
    Parked,
    EpochAccepted { group: GroupId, epoch: EpochId },
    DocEpochSwitched { group: GroupId, doc: DocId, generation: u32, rebased: bool },
    /// We added a device that presented an open invite or link code ("every join
    /// notifies the group"; for the personal group: "New device linked · Undo").
    DeviceJoined { group: GroupId, device: DeviceId, via_link: bool },
}

struct DocState {
    doc: LoroDoc,
    /// Own ops already sent (counter end, exclusive). Resent after restart (idempotent).
    sent_counter: i32,
    imported_since_snapshot: usize,
    generation: u32,
    /// Own counter at the start of this generation (non-zero for the proposer: its rebuild
    /// ops travel in the proposal). Slots carry own ops from here on.
    gen_base: i32,
    /// Set after switching generations: lets a better concurrent proposal replace ours.
    prev: Option<PrevGen>,
}

struct PrevGen {
    /// The document before the switch.
    doc: LoroDoc,
    /// Who proposed the generation we switched to.
    proposer: DeviceId,
    /// The new document right after the switch (before our later edits).
    start: LoroDoc,
}

fn new_doc_state(doc: LoroDoc, generation: u32) -> DocState {
    with_timestamps(&doc);
    DocState { doc, sent_counter: 0, imported_since_snapshot: 0, generation, gen_base: 0, prev: None }
}

/// Changes record when they were made (per minute): "undo changes by a person since a
/// time" (D27) needs it.
fn with_timestamps(doc: &LoroDoc) {
    doc.set_record_timestamp(true);
    doc.set_change_merge_interval(60);
}

/// Previous generations are kept this long after a switch, with their full history (D27:
/// at least 30 days of history, even across compactions).
pub const ARCHIVE_KEEP_MS: u64 = 30 * 86_400_000;

struct GroupState {
    epochs: GroupEpochs,
    keys: HashMap<EpochId, SymKey>,
    docs: BTreeMap<DocId, DocState>,
    new_records: Vec<EpochRecord>,
}

pub struct Replica {
    identity: DeviceIdentity,
    _lock: IdentityLock,
    layout: Layout,
    sandbox: SandboxConfig,
    quarantine: Quarantine,
    groups: BTreeMap<GroupId, GroupState>,
    /// Groups we're allowed to learn about (created, joined or invited to).
    accepted_groups: BTreeSet<GroupId>,
    parked: Vec<Vec<u8>>,
    /// Hashes of payloads already validated and imported (skip re-running the sandbox on
    /// resent slots). Bounded; forgetting only costs a re-validation.
    imported: std::collections::HashSet<[u8; 32]>,
    /// This device's current whereabouts (opaque, from the transport layer).
    whereabouts: Option<Vec<u8>>,
    /// This device's health observations for its groups (opaque, from the transport layer).
    health: Option<Vec<u8>>,
    /// Periodic key rotation period (D5 rule 7).
    rotate_after_ms: u64,
    invite_cache: invites::InviteCache,
    /// Bumped on every local commit (lets live transports push right away).
    local_changes: u64,
    /// Last slot fingerprint without acknowledgments, per (group, epoch).
    last_base_fp: HashMap<(GroupId, EpochId), [u8; 32]>,
    ack_interval_ms: u64,
    compaction: compaction::CompactionPolicy,
    events: Vec<Event>,
    semantics: Option<Box<dyn DocSemantics>>,
}

impl Replica {
    /// Open (or create) a replica. `identity_dir` must be excluded from backups (D1).
    pub fn open(identity_dir: &Path, data_dir: &Path, sandbox: SandboxConfig) -> Result<Self> {
        let identity = DeviceIdentity::load_or_create(identity_dir)?;
        let lock = IdentityLock::acquire(identity_dir)?;
        let layout = Layout::new(data_dir);
        let (quarantine, newly) = Quarantine::open(&layout.quarantine_journal(), &layout.quarantine_list())?;
        let mut r = Replica {
            identity,
            _lock: lock,
            layout,
            sandbox,
            quarantine,
            groups: BTreeMap::new(),
            accepted_groups: BTreeSet::new(),
            parked: Vec::new(),
            imported: std::collections::HashSet::new(),
            whereabouts: None,
            health: None,
            rotate_after_ms: rotation::DEFAULT_ROTATE_AFTER_MS,
            invite_cache: Default::default(),
            local_changes: 0,
            last_base_fp: HashMap::new(),
            ack_interval_ms: slots::ACK_INTERVAL_MS,
            compaction: Default::default(),
            events: newly.into_iter().map(|(_, a)| Event::Quarantined { author: DeviceId(a) }).collect(),
            semantics: None,
        };
        r.load()?;
        Ok(r)
    }

    fn load(&mut self) -> Result<()> {
        for g in self.layout.groups()? {
            self.accepted_groups.insert(g);
            let mut state = GroupState { epochs: GroupEpochs::new(g), keys: HashMap::new(), docs: BTreeMap::new(), new_records: Vec::new() };
            for rec in storage::read_records(&self.layout.control_log(&g))? {
                if let Ok(r) = EpochRecord::decode(&rec) {
                    let _ = state.epochs.insert(r);
                }
            }
            self.groups.insert(g, state);
            // Keys received through invites (before being added).
            let invite_keys: Vec<(EpochId, [u8; 32])> = std::fs::read(self.layout.group_dir(&g).join("invite_keys")).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default();
            for (e, k) in invite_keys {
                self.groups.get_mut(&g).expect("inserted").keys.entry(e).or_insert(SymKey(k));
            }
            self.refresh_keys(&g);
            for d in self.layout.docs(&g)? {
                let doc = match std::fs::read(self.layout.doc_snapshot(&g, &d)) {
                    Ok(b) => LoroDoc::from_snapshot(&b).map_err(|e| Error::Loro(e.to_string()))?,
                    Err(_) => LoroDoc::new(),
                };
                doc.set_peer_id(self.identity.peer_id()).map_err(|e| Error::Loro(e.to_string()))?;
                let generation = std::fs::read(self.layout.doc_dir(&g, &d).join("generation"))
                    .ok()
                    .and_then(|b| b.try_into().ok().map(u32::from_le_bytes))
                    .unwrap_or(0);
                let sent = std::fs::read(self.layout.doc_dir(&g, &d).join("sent"))
                    .ok()
                    .and_then(|b| b.try_into().ok().map(i32::from_le_bytes))
                    .unwrap_or(0);
                let gen_base = std::fs::read(self.layout.doc_dir(&g, &d).join("genbase"))
                    .ok()
                    .and_then(|b| b.try_into().ok().map(i32::from_le_bytes))
                    .unwrap_or(0);
                let mut st = new_doc_state(doc, generation);
                st.sent_counter = sent;
                st.gen_base = gen_base;
                self.groups.get_mut(&g).expect("inserted").docs.insert(d, st);
                // Re-apply logged envelopes (idempotent): restores pending changes too.
                for env in storage::read_records(&self.layout.doc_updates(&g, &d))? {
                    let _ = self.apply_envelope_bytes(&env, false);
                }
            }
        }
        Ok(())
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    pub fn device_id(&self) -> DeviceId {
        self.identity.id()
    }

    pub fn device_public(&self) -> DevicePublic {
        self.identity.public()
    }

    /// Install the app's document semantics (enables document epochs).
    pub fn set_semantics(&mut self, s: Box<dyn DocSemantics>) {
        self.semantics = Some(s);
    }

    pub fn doc_generation(&self, g: &GroupId, d: &DocId) -> Option<u32> {
        self.groups.get(g)?.docs.get(d).map(|s| s.generation)
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    pub fn groups(&self) -> Vec<GroupId> {
        self.groups.keys().copied().collect()
    }

    pub fn epochs(&self, g: &GroupId) -> Option<&GroupEpochs> {
        self.groups.get(g).map(|s| &s.epochs)
    }

    pub fn docs(&self, g: &GroupId) -> Vec<DocId> {
        self.groups.get(g).map(|s| s.docs.keys().copied().collect()).unwrap_or_default()
    }

    /// Drop a group from this device entirely (its data was copied elsewhere, or the user
    /// left). Other members are unaffected.
    pub fn forget_group(&mut self, g: &GroupId) -> Result<()> {
        self.groups.remove(g);
        self.accepted_groups.remove(g);
        let dir = self.layout.group_dir(g);
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    /// Allow epoch records of this group to be accepted (after an invite, for example).
    pub fn accept_group(&mut self, g: GroupId) {
        self.accepted_groups.insert(g);
    }

    /// Make a key known out of band (the invite path, D55): lets a joiner read before being added.
    pub fn add_invite_key(&mut self, g: GroupId, epoch: EpochId, key: SymKey) {
        self.accept_group(g);
        let state = self.groups.entry(g).or_insert_with(|| GroupState {
            epochs: GroupEpochs::new(g),
            keys: HashMap::new(),
            docs: BTreeMap::new(),
            new_records: Vec::new(),
        });
        state.keys.insert(epoch, key.clone());
        // Kept with the group: the next process (app restart, background job) still reads.
        let _ = std::fs::create_dir_all(self.layout.group_dir(&g));
        let path = self.layout.group_dir(&g).join("invite_keys");
        let mut all: Vec<(EpochId, [u8; 32])> = std::fs::read(&path).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default();
        if !all.iter().any(|(e, _)| *e == epoch) {
            all.push((epoch, key.0));
            if let Ok(b) = postcard::to_stdvec(&all) {
                let _ = storage::write_atomic(&path, &b);
            }
        }
    }

    pub fn create_group(&mut self) -> Result<GroupId> {
        let g = GroupId::random();
        let rec = epochs::genesis(&self.identity, g);
        self.accept_group(g);
        self.groups.insert(g, GroupState { epochs: GroupEpochs::new(g), keys: HashMap::new(), docs: BTreeMap::new(), new_records: Vec::new() });
        self.insert_epoch(g, rec, true)?;
        Ok(g)
    }

    pub fn create_doc(&mut self, g: &GroupId) -> Result<DocId> {
        let d = DocId::random();
        self.ensure_doc(g, &d)?;
        Ok(d)
    }

    fn ensure_doc(&mut self, g: &GroupId, d: &DocId) -> Result<()> {
        let peer = self.identity.peer_id();
        let state = self.groups.get_mut(g).ok_or_else(|| Error::NotFound("group".into()))?;
        if !state.docs.contains_key(d) {
            let doc = LoroDoc::new();
            doc.set_peer_id(peer).map_err(|e| Error::Loro(e.to_string()))?;
            state.docs.insert(*d, new_doc_state(doc, 0));
            std::fs::create_dir_all(self.layout.doc_dir(g, d))?;
        }
        Ok(())
    }

    /// Read access to a document (for the domain layer).
    pub fn doc(&self, g: &GroupId, d: &DocId) -> Option<&LoroDoc> {
        self.groups.get(g)?.docs.get(d).map(|s| &s.doc)
    }

    /// Counter of local commits in this session (compare to notice new local changes).
    pub fn local_changes(&self) -> u64 {
        self.local_changes
    }

    /// Commit local edits made on [`Replica::doc`] and persist a snapshot.
    pub fn commit(&mut self, g: &GroupId, d: &DocId) -> Result<()> {
        self.local_changes += 1;
        let state = self.groups.get_mut(g).ok_or_else(|| Error::NotFound("group".into()))?;
        let ds = state.docs.get_mut(d).ok_or_else(|| Error::NotFound("doc".into()))?;
        ds.doc.commit();
        let snap = ds.doc.export(ExportMode::Snapshot).map_err(|e| Error::Loro(e.to_string()))?;
        storage::write_atomic(&self.layout.doc_snapshot(g, d), &snap)?;
        ds.imported_since_snapshot = 0;
        Ok(())
    }

    pub fn add_member(&mut self, g: &GroupId, device: DevicePublic) -> Result<()> {
        let state = self.groups.get(g).ok_or_else(|| Error::NotFound("group".into()))?;
        let rec = state.epochs.create_add(&self.identity, device, &state.keys)?;
        self.insert_epoch(*g, rec, true)
    }

    pub fn remove_member(&mut self, g: &GroupId, device: DeviceId) -> Result<()> {
        let state = self.groups.get(g).ok_or_else(|| Error::NotFound("group".into()))?;
        let peer = peer_id_for(&device);
        let cutoffs = state.docs.iter().map(|(d, s)| (*d, s.doc.oplog_vv().get(&peer).copied().unwrap_or(0))).collect();
        let rec = state.epochs.create_remove(&self.identity, device, cutoffs)?;
        self.insert_epoch(*g, rec, true)?;
        // Build on the removal at once (a fresh key on top of it): from then on a removal the removed device signs
        // on an older branch is refused (epochs.rs), instead of working whenever it arrives.
        if self.groups[g].epochs.head().is_some() {
            self.rotate(g)?;
        }
        Ok(())
    }

    pub fn rotate(&mut self, g: &GroupId) -> Result<()> {
        let rec = self.groups.get(g).ok_or_else(|| Error::NotFound("group".into()))?.epochs.create_rotate(&self.identity)?;
        self.insert_epoch(*g, rec, true)
    }

    fn insert_epoch(&mut self, g: GroupId, rec: EpochRecord, own: bool) -> Result<()> {
        let bytes = rec.encode()?;
        let state = self.groups.get_mut(&g).ok_or_else(|| Error::NotFound("group".into()))?;
        let outcome = state.epochs.insert(rec.clone())?;
        if outcome == InsertOutcome::AlreadyKnown {
            return Ok(());
        }
        storage::append_record(&self.layout.control_log(&g), &bytes)?;
        if own {
            state.new_records.push(rec.clone());
        }
        if outcome == InsertOutcome::Accepted {
            self.events.push(Event::EpochAccepted { group: g, epoch: rec.id() });
        }
        self.refresh_keys(&g);
        // Deterministic merger duty (rule 2).
        let merge = self.groups[&g].epochs.merge_if_mine(&self.identity);
        if let Some(m) = merge {
            self.insert_epoch(g, m, true)?;
        }
        self.retry_parked();
        Ok(())
    }

    fn refresh_keys(&mut self, g: &GroupId) {
        let me = &self.identity;
        if let Some(state) = self.groups.get_mut(g) {
            let ids: Vec<EpochId> = state.epochs.all_records().map(|r| r.id()).collect();
            for id in ids {
                if !state.keys.contains_key(&id) {
                    if let Ok(k) = state.epochs.key_for(&id, me) {
                        state.keys.insert(id, k);
                    }
                }
            }
            for (id, k) in state.epochs.history_keys_for(me) {
                state.keys.entry(id).or_insert(k);
            }
        }
    }

    fn retry_parked(&mut self) {
        let parked = std::mem::take(&mut self.parked);
        for p in parked {
            let _ = self.apply_envelope_bytes(&p, true);
        }
    }

    /// Everything this device should send now: its new epoch records and its own new ops.
    pub fn outbox(&mut self) -> Result<Vec<Vec<u8>>> {
        self.flush_own_ops()?;
        let path = self.outbox_path();
        let msgs = storage::read_records(&path)?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(msgs)
    }

    fn outbox_path(&self) -> std::path::PathBuf {
        self.layout.root().join("outbox.log")
    }

    /// Queue an outgoing message durably (survives the app being killed before sending).
    fn queue_out(&self, msg: Vec<u8>) -> Result<()> {
        storage::append_record(&self.outbox_path(), &msg)
    }

    fn save_sent_counter(&self, g: &GroupId, d: &DocId, counter: i32) -> Result<()> {
        storage::write_atomic(&self.layout.doc_dir(g, d).join("sent"), &counter.to_le_bytes())
    }

    /// Seal this device's unsent own ops (and new epoch records) into messages: appended to
    /// the local log (for relaying to new members) and queued for the next `outbox`.
    fn flush_own_ops(&mut self) -> Result<()> {
        let me_peer = self.identity.peer_id();
        let gids: Vec<GroupId> = self.groups.keys().copied().collect();
        for g in gids {
            let state = self.groups.get_mut(&g).expect("exists");
            let records: Vec<EpochRecord> = state.new_records.drain(..).collect();
            let mut envs = Vec::new();
            let head_and_key = match state.epochs.working_head() {
                Some(h) if state.epochs.is_current_member(&self.identity.id()) => state.keys.get(&h).cloned().map(|k| (h, k)),
                _ => None,
            };
            let Some((head, key)) = head_and_key else {
                for rec in records {
                    self.queue_out(Message::Epoch(rec.encode()?).encode()?)?;
                }
                continue;
            };
            for (d, ds) in state.docs.iter_mut() {
                let current = ds.doc.oplog_vv().get(&me_peer).copied().unwrap_or(0);
                if current <= ds.sent_counter {
                    continue;
                }
                let span = IdSpan::new(me_peer, ds.sent_counter, current);
                let update = ds.doc.export(ExportMode::updates_in_range(vec![span])).map_err(|e| Error::Loro(e.to_string()))?;
                let inner = Inner { group: g, doc: *d, generation: ds.generation, kind: MessageKind::DocUpdate, peer: me_peer, created_ms: now_ms(), payload: update };
                let env = Envelope::seal(&self.identity, head, &key, &inner)?;
                ds.sent_counter = current;
                envs.push((*d, env.encode()?, current));
            }
            for rec in records {
                self.queue_out(Message::Epoch(rec.encode()?).encode()?)?;
            }
            for (d, env_bytes, counter) in envs {
                storage::append_record(&self.layout.doc_updates(&g, &d), &env_bytes)?;
                self.queue_out(Message::Envelope(env_bytes).encode()?)?;
                self.save_sent_counter(&g, &d, counter)?;
            }
        }
        Ok(())
    }

    /// Everything a new member of `g` needs to catch up: all epoch records and all stored
    /// envelopes (own and relayed, each still signed by its author).
    pub fn export_group(&mut self, g: &GroupId) -> Result<Vec<Vec<u8>>> {
        self.flush_own_ops()?;
        let mut out = Vec::new();
        for rec in storage::read_records(&self.layout.control_log(g))? {
            out.push(Message::Epoch(rec).encode()?);
        }
        for d in self.layout.docs(g)? {
            for env in storage::read_records(&self.layout.doc_updates(g, &d))? {
                out.push(Message::Envelope(env).encode()?);
            }
        }
        Ok(out)
    }

    /// Receive one wire message from any transport.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<()> {
        match Message::decode(bytes)? {
            Message::Epoch(b) => {
                let rec = EpochRecord::decode(&b)?;
                let g = rec.body.group;
                if !self.accepted_groups.contains(&g) {
                    return Err(Error::Rejected("epoch for a group we didn't join".into()));
                }
                self.groups.entry(g).or_insert_with(|| GroupState {
                    epochs: GroupEpochs::new(g),
                    keys: HashMap::new(),
                    docs: BTreeMap::new(),
                    new_records: Vec::new(),
                });
                std::fs::create_dir_all(self.layout.group_dir(&g))?;
                self.insert_epoch(g, rec, false)
            }
            Message::Envelope(b) => self.apply_envelope_bytes(&b, true),
            Message::Status(b) => self.receive_status(&b),
            Message::Whereabouts(b) => self.receive_whereabouts(&b),
            Message::Invite(b) => self.receive_invite(&b),
            Message::JoinRequest(b) => self.receive_join_request(&b),
            Message::MailboxSet(b) => self.receive_mailbox_set(&b),
            Message::Handover(b) => self.receive_handover(&b),
            Message::Health(b) => self.receive_health(&b),
        }
    }

    fn remember_imported(&mut self, h: [u8; 32]) {
        if self.imported.len() >= 100_000 {
            self.imported.clear();
        }
        self.imported.insert(h);
    }

    fn find_group_of_epoch(&self, epoch: &EpochId) -> Option<GroupId> {
        self.groups.iter().find(|(_, s)| s.epochs.record(epoch).is_some() || s.keys.contains_key(epoch)).map(|(g, _)| *g)
    }

    fn apply_envelope_bytes(&mut self, bytes: &[u8], persist: bool) -> Result<()> {
        let env = Envelope::decode(bytes)?;
        let Some(g) = self.find_group_of_epoch(&env.epoch) else {
            if persist && self.parked.len() < 10_000 {
                self.parked.push(bytes.to_vec());
                self.events.push(Event::Parked);
            }
            return Ok(());
        };
        if env.sender == self.identity.id() && persist {
            return Ok(()); // our own message echoed back
        }
        let state = self.groups.get(&g).expect("found");
        if state.epochs.record(&env.epoch).is_none() {
            // Only an invite key so far: park until the epoch record arrives.
            if persist && self.parked.len() < 10_000 {
                self.parked.push(bytes.to_vec());
                self.events.push(Event::Parked);
            }
            return Ok(());
        }
        let key = state.keys.get(&env.epoch).cloned().ok_or_else(|| Error::Rejected("no key for that epoch".into()))?;
        let inner = match env.open(&g, &state.epochs, &key) {
            Ok(i) => i,
            Err(e) => {
                self.events.push(Event::Rejected { reason: e.to_string() });
                return Err(e);
            }
        };
        if inner.kind == MessageKind::DocEpochProposal {
            return self.handle_proposal(g, env.sender, inner, bytes, persist);
        }
        self.ensure_doc(&g, &inner.doc)?;
        let cur_gen = self.groups[&g].docs[&inner.doc].generation;
        if inner.generation < cur_gen {
            // Written for an older document generation: its author will rebase.
            return Ok(());
        }
        if inner.generation > cur_gen {
            if persist && self.parked.len() < 10_000 {
                self.parked.push(bytes.to_vec());
                self.events.push(Event::Parked);
            }
            return Ok(());
        }
        let payload_hash = crypto::hash("kt/payload/v1", &inner.payload);
        if self.imported.contains(&payload_hash) {
            return Ok(()); // exact duplicate (mailboxes resend slots)
        }
        if self.quarantine.is_quarantined(&payload_hash) {
            return Err(Error::Rejected("quarantined update".into()));
        }
        let snapshot = {
            let ds = &self.groups[&g].docs[&inner.doc];
            ds.doc.export(ExportMode::Snapshot).map_err(|e| Error::Loro(e.to_string()))?
        };
        self.quarantine.begin(payload_hash, env.sender.0)?;
        let verdict = sandbox::validate(&self.sandbox, &snapshot, &inner.payload);
        self.quarantine.end()?;
        match verdict? {
            Verdict::Ok { ranges } => {
                if ranges.keys().any(|p| *p != inner.peer) {
                    self.events.push(Event::Rejected { reason: "update contains another device's ops (impersonation)".into() });
                    return Err(Error::Rejected("impersonation".into()));
                }
                // D5 rule 5: a removed device's ops beyond its cutoff are rejected.
                if let Some(cutoffs) = self.groups[&g].epochs.cutoffs_for(&env.sender) {
                    let end = ranges.get(&inner.peer).map(|r| r.1).unwrap_or(0);
                    if end > cutoffs.get(&inner.doc).copied().unwrap_or(0) {
                        self.events.push(Event::Rejected { reason: "ops from a removed device beyond its cutoff".into() });
                        return Err(Error::Rejected("removed device's late ops".into()));
                    }
                }
                if ranges.is_empty() {
                    self.remember_imported(payload_hash);
                    return Ok(()); // nothing new
                }
            }
            Verdict::Rejected(reason) => {
                self.events.push(Event::Rejected { reason: reason.clone() });
                return Err(Error::Rejected(reason));
            }
            Verdict::Crashed(reason) => {
                self.quarantine.add(payload_hash, env.sender.0)?;
                self.events.push(Event::Quarantined { author: env.sender });
                return Err(Error::Rejected(format!("poison update quarantined: {reason}")));
            }
        }
        // Safe to import into the live document.
        self.quarantine.begin(payload_hash, env.sender.0)?;
        let ds = self.groups.get_mut(&g).expect("found").docs.get_mut(&inner.doc).expect("ensured");
        let res = ds.doc.import(&inner.payload);
        self.quarantine.end()?;
        res.map_err(|e| Error::Loro(e.to_string()))?;
        ds.imported_since_snapshot += 1;
        let snapshot_due = ds.imported_since_snapshot >= SNAPSHOT_EVERY;
        self.remember_imported(payload_hash);
        if persist {
            storage::append_record(&self.layout.doc_updates(&g, &inner.doc), bytes)?;
        }
        if snapshot_due {
            self.commit(&g, &inner.doc)?;
        }
        self.events.push(Event::Imported { group: g, doc: inner.doc, author: env.sender });
        if !self.parked.is_empty() {
            // Newly imported ops may unblock parked messages (e.g. a waiting epoch proposal).
            self.retry_parked();
        }
        Ok(())
    }
}

impl Replica {
    /// Propose a document epoch (D24): replace the document with a fresh rebuild of its live
    /// state. If devices were removed, the rebuild is taken at their cutoffs (D5 rule 5), so
    /// every member converges even if some imported late ops before learning of a removal.
    pub fn propose_doc_epoch(&mut self, g: &GroupId, d: &DocId) -> Result<()> {
        let sem = self.semantics.as_ref().ok_or_else(|| Error::Invalid("no document semantics installed".into()))?;
        let me = self.identity.peer_id();
        let state = self.groups.get(g).ok_or_else(|| Error::NotFound("group".into()))?;
        let head = state.epochs.head().ok_or_else(|| Error::Rejected("epochs not converged".into()))?;
        let key = state.keys.get(&head).cloned().ok_or_else(|| Error::Rejected("no key".into()))?;
        let ds = state.docs.get(d).ok_or_else(|| Error::NotFound("doc".into()))?;
        // Base version: current, with removed devices capped at their cutoffs.
        let mut base = ds.doc.oplog_vv();
        for removed in state.epochs.removed_devices() {
            let peer = peer_id_for(&removed);
            if let Some(cut) = state.epochs.cutoffs_for(&removed).and_then(|c| c.get(d).copied()) {
                if base.get(&peer).copied().unwrap_or(0) > cut {
                    base.insert(peer, cut);
                }
            }
        }
        let source = if base == ds.doc.oplog_vv() {
            ds.doc.fork()
        } else {
            ds.doc.fork_at(&ds.doc.vv_to_frontiers(&base)).map_err(|e| Error::Loro(e.to_string()))?
        };
        let fresh = sem.rebuild(&source, me)?;
        let generation = ds.generation + 1;
        let proposal = EpochProposal {
            generation,
            base_vv: base.encode(),
            snapshot: fresh.export(ExportMode::Snapshot).map_err(|e| Error::Loro(e.to_string()))?,
        };
        let inner = Inner {
            group: *g,
            doc: *d,
            generation,
            kind: MessageKind::DocEpochProposal,
            peer: me,
            created_ms: now_ms(),
            payload: postcard::to_stdvec(&proposal).map_err(|e| Error::Encoding(e.to_string()))?,
        };
        let env_bytes = Envelope::seal(&self.identity, head, &key, &inner)?.encode()?;
        self.switch_generation(g, d, fresh, generation, self.identity.id(), None, &env_bytes)?;
        let ds = self.groups.get_mut(g).expect("exists").docs.get_mut(d).expect("exists");
        ds.sent_counter = ds.doc.oplog_vv().get(&me).copied().unwrap_or(0);
        ds.gen_base = ds.sent_counter;
        let sent = ds.sent_counter;
        self.save_sent_counter(g, d, sent)?;
        storage::write_atomic(&self.layout.doc_dir(g, d).join("genbase"), &sent.to_le_bytes())?;
        self.queue_out(Message::Envelope(env_bytes).encode()?)?;
        Ok(())
    }

    fn handle_proposal(&mut self, g: GroupId, sender: DeviceId, inner: Inner, bytes: &[u8], persist: bool) -> Result<()> {
        if self.semantics.is_none() {
            return Ok(());
        }
        let proposal: EpochProposal = postcard::from_bytes(&inner.payload).map_err(|e| Error::Encoding(e.to_string()))?;
        if proposal.generation != inner.generation {
            return Err(Error::Rejected("proposal generation mismatch".into()));
        }
        self.ensure_doc(&g, &inner.doc)?;
        let (cur_gen, better_than_current) = {
            let ds = &self.groups[&g].docs[&inner.doc];
            let better = ds.prev.as_ref().map(|p| sender < p.proposer).unwrap_or(false);
            (ds.generation, better)
        };
        // Accept the next generation, or a concurrent proposal for the current one from a
        // lower device ID (deterministic tie-break).
        let replacing = proposal.generation == cur_gen && better_than_current;
        if proposal.generation != cur_gen + 1 && !replacing {
            if proposal.generation > cur_gen + 1 && persist && self.parked.len() < 10_000 {
                self.parked.push(bytes.to_vec());
                self.events.push(Event::Parked);
            }
            return Ok(());
        }
        // The snapshot is untrusted: validate it in the sandbox (a fresh doc authored by the sender).
        let verdict = sandbox::validate(&self.sandbox, &[], &proposal.snapshot)?;
        match verdict {
            Verdict::Ok { ranges } if ranges.keys().all(|p| *p == inner.peer) => {}
            Verdict::Crashed(r) => {
                let h = crypto::hash("kt/payload/v1", &proposal.snapshot);
                self.quarantine.add(h, sender.0)?;
                self.events.push(Event::Quarantined { author: sender });
                return Err(Error::Rejected(format!("poison proposal: {r}")));
            }
            _ => return Err(Error::Rejected("invalid document epoch proposal".into())),
        }
        let base = VersionVector::decode(&proposal.base_vv).map_err(|e| Error::Loro(e.to_string()))?;
        let me = self.identity.peer_id();
        let (base_doc, rebase) = {
            let ds = &self.groups[&g].docs[&inner.doc];
            // When replacing a concurrent proposal, compare against the pre-switch document.
            let reference = if replacing { &ds.prev.as_ref().expect("replacing implies prev").doc } else { &ds.doc };
            let have = reference.oplog_vv();
            if base.iter().any(|(p, c)| have.get(p).copied().unwrap_or(0) < *c) {
                // We're missing ops the proposer had: wait until they arrive.
                if persist && self.parked.len() < 10_000 {
                    self.parked.push(bytes.to_vec());
                    self.events.push(Event::Parked);
                }
                return Ok(());
            }
            let base_doc = if base == have {
                reference.fork()
            } else {
                reference.fork_at(&reference.vv_to_frontiers(&base)).map_err(|e| Error::Loro(e.to_string()))?
            };
            let rebase = if replacing {
                // Changes we made after switching to the losing generation.
                let p = ds.prev.as_ref().expect("replacing implies prev");
                Some((p.start.fork(), ds.doc.fork()))
            } else {
                let mine = have.get(&me).copied().unwrap_or(0);
                if mine > base.get(&me).copied().unwrap_or(0) {
                    // Our view restricted to the base plus our own ops: only our changes get re-authored.
                    let mut vv_mine = base.clone();
                    vv_mine.insert(me, mine);
                    let local = reference.fork_at(&reference.vv_to_frontiers(&vv_mine)).unwrap_or_else(|_| reference.fork());
                    Some((base_doc.fork(), local))
                } else {
                    None
                }
            };
            (base_doc, rebase)
        };
        let fresh = LoroDoc::from_snapshot(&proposal.snapshot).map_err(|e| Error::Loro(e.to_string()))?;
        let sem = self.semantics.as_ref().expect("checked");
        if sem.normalized(&fresh) != sem.normalized(&base_doc) {
            self.events.push(Event::Rejected { reason: "document epoch proposal doesn't match our state".into() });
            return Err(Error::Rejected("proposal state mismatch".into()));
        }
        self.switch_generation(&g, &inner.doc, fresh, proposal.generation, sender, rebase, bytes)
    }

    /// Earlier generations of a document still kept (generation, when it was left, the
    /// document with its full history), oldest first.
    pub fn doc_archive(&self, g: &GroupId, d: &DocId) -> Vec<(u32, u64, LoroDoc)> {
        let dir = self.layout.doc_dir(g, d).join("archive");
        let mut out: Vec<(u32, u64, LoroDoc)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let generation: u32 = e.file_name().to_str()?.strip_prefix("gen-")?.parse().ok()?;
                let b = std::fs::read(e.path()).ok()?;
                let at = u64::from_le_bytes(b.get(..8)?.try_into().ok()?);
                let doc = LoroDoc::from_snapshot(&b[8..]).ok()?; // our own file
                Some((generation, at, doc))
            })
            .collect();
        out.sort_by_key(|x| x.0);
        out
    }

    /// Replace a document with a new generation, rebasing local changes if given.
    #[allow(clippy::too_many_arguments)]
    fn switch_generation(
        &mut self,
        g: &GroupId,
        d: &DocId,
        fresh: LoroDoc,
        generation: u32,
        proposer: DeviceId,
        rebase: Option<(LoroDoc, LoroDoc)>,
        proposal_env: &[u8],
    ) -> Result<()> {
        let me = self.identity.peer_id();
        fresh.set_peer_id(me).map_err(|e| Error::Loro(e.to_string()))?;
        with_timestamps(&fresh);
        let start = fresh.fork();
        let mut rebased = false;
        if let (Some((base, local)), Some(sem)) = (&rebase, self.semantics.as_ref()) {
            if sem.normalized(base) != sem.normalized(local) {
                sem.rebase(base, local, &fresh)?;
                rebased = true;
            }
        }
        // Our own ops of the generation we're leaving: members still on it may lack some of
        // them (we may have edited right before switching), and the proposal can only be
        // verified on top of them. Slots keep carrying them for FORWARD_WINDOW_MS.
        let leaving = {
            let state = self.groups.get(g).ok_or_else(|| Error::NotFound("group".into()))?;
            let ds = state.docs.get(d).ok_or_else(|| Error::NotFound("doc".into()))?;
            if ds.generation < generation {
                let cur = ds.doc.oplog_vv().get(&me).copied().unwrap_or(0);
                let head_key = state.epochs.working_head().and_then(|h| state.keys.get(&h).cloned().map(|k| (h, k)));
                match head_key {
                    Some((h, k)) if cur > ds.gen_base => {
                        let span = IdSpan::new(me, ds.gen_base, cur);
                        let update = ds.doc.export(ExportMode::updates_in_range(vec![span])).map_err(|e| Error::Loro(e.to_string()))?;
                        let inner = Inner { group: *g, doc: *d, generation: ds.generation, kind: MessageKind::DocUpdate, peer: me, created_ms: now_ms(), payload: update };
                        Some(Some(Envelope::seal(&self.identity, h, &k, &inner)?.encode()?))
                    }
                    _ => Some(None),
                }
            } else {
                None // replacing a concurrent proposal of the same generation: keep what we have
            }
        };
        match leaving {
            Some(Some(env)) => {
                let mut b = now_ms().to_le_bytes().to_vec();
                b.extend_from_slice(&env);
                storage::write_atomic(&self.layout.doc_dir(g, d).join("prevgen"), &b)?;
            }
            Some(None) => {
                let _ = std::fs::remove_file(self.layout.doc_dir(g, d).join("prevgen"));
            }
            None => {}
        }
        let ds = self
            .groups
            .get_mut(g)
            .ok_or_else(|| Error::NotFound("group".into()))?
            .docs
            .get_mut(d)
            .ok_or_else(|| Error::NotFound("doc".into()))?;
        let switching_up = ds.generation < generation;
        let old = std::mem::replace(&mut ds.doc, fresh);
        if switching_up {
            // Keep the generation we're leaving, with its full history (D27).
            let dir = self.layout.doc_dir(g, d).join("archive");
            std::fs::create_dir_all(&dir)?;
            let mut b = now_ms().to_le_bytes().to_vec();
            b.extend_from_slice(&old.export(ExportMode::Snapshot).map_err(|e| Error::Loro(e.to_string()))?);
            storage::write_atomic(&dir.join(format!("gen-{}", ds.generation)), &b)?;
            for e in std::fs::read_dir(&dir)?.flatten() {
                let keep = std::fs::read(e.path()).ok().filter(|b| b.len() >= 8).map(|b| u64::from_le_bytes(b[..8].try_into().expect("8")));
                if keep.is_none_or(|t| now_ms().saturating_sub(t) > ARCHIVE_KEEP_MS) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let ds = self.groups.get_mut(g).expect("checked").docs.get_mut(d).expect("checked");
        let prev_doc = match ds.prev.take() {
            // Replacing a concurrent proposal: the pre-switch document stays the reference.
            Some(p) if ds.generation == generation => p.doc,
            _ => old,
        };
        ds.prev = Some(PrevGen { doc: prev_doc, proposer, start });
        ds.generation = generation;
        ds.sent_counter = 0;
        ds.gen_base = 0;
        ds.imported_since_snapshot = 0;
        storage::write_atomic(&self.layout.doc_dir(g, d).join("sent"), &0i32.to_le_bytes())?;
        storage::write_atomic(&self.layout.doc_dir(g, d).join("genbase"), &0i32.to_le_bytes())?;
        let snap = ds.doc.export(ExportMode::Snapshot).map_err(|e| Error::Loro(e.to_string()))?;
        storage::write_atomic(&self.layout.doc_snapshot(g, d), &snap)?;
        storage::write_atomic(&self.layout.doc_dir(g, d).join("generation"), &generation.to_le_bytes())?;
        // The update log restarts with the proposal (kept for relaying to new members).
        storage::write_atomic(&self.layout.doc_updates(g, d), &[])?;
        storage::append_record(&self.layout.doc_updates(g, d), proposal_env)?;
        self.events.push(Event::DocEpochSwitched { group: *g, doc: *d, generation, rebased });
        self.retry_parked();
        Ok(())
    }
}

/// Wall-clock milliseconds (the time every record carries).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
