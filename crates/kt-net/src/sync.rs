//! Drives a [`Replica`] against mailboxes: write own slots, read members' slots.
//!
//! - A slot is rewritten only when its content changed, when it's due for a refresh, or
//!   when reading showed the mailbox lost it (read-back).
//! - A slot version already applied isn't opened again.
//! - Replica work (crypto, sandboxed imports) runs on blocking threads.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use kt_engine::replica::{ReadTopic, Replica};

use crate::mailbox::{Mailbox, SlotRead, SlotWrite, WriteOutcome};
use crate::slotstore::SlotStore;

/// Pseudo mailbox index for the local slot store in `written`.
const STORE: usize = usize::MAX;

/// No layer holds a round up for longer than this (a server that takes the connection and never answers).
const LAYER_CAP: Duration = Duration::from_secs(25);
/// Once one layer has answered a read, the others get this much longer; a layer that misses it is read next
/// round (the slots it carries are the same ones the other layers carry, so nothing waits on it).
const LAYER_GRACE: Duration = Duration::from_secs(10);

/// Every mailbox's read, as each one answers; `None` for a layer that didn't answer in time.
async fn read_all(mailboxes: &[Arc<dyn Mailbox>], topics: &[[u8; 32]]) -> Vec<Option<Vec<SlotRead>>> {
    use futures_util::stream::{FuturesUnordered, StreamExt};
    let mut out: Vec<Option<Vec<SlotRead>>> = (0..mailboxes.len()).map(|_| None).collect();
    let mut pending: FuturesUnordered<_> = mailboxes.iter().enumerate().map(|(i, m)| async move { (i, m.read(topics).await) }).collect();
    let mut deadline = tokio::time::Instant::now() + LAYER_CAP;
    while let Ok(Some((i, slots))) = tokio::time::timeout_at(deadline, pending.next()).await {
        out[i] = Some(slots);
        deadline = deadline.min(tokio::time::Instant::now() + LAYER_GRACE);
    }
    out
}

/// Writer identity for own slots in the local store (and so on peer-to-peer links).
pub fn store_writer(writer_seed: &[u8; 32]) -> [u8; 32] {
    *blake3::Hasher::new_derive_key("kt/store-writer/v1").update(writer_seed).finalize().as_bytes()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Rewrite an unchanged slot after this long (mailboxes may prune old data).
pub const REFRESH: Duration = Duration::from_secs(3 * 24 * 3600);
/// Writes are where data can be lost without anyone knowing, so a layer counts as done
/// only when at least this many of its copies landed (all of them if fewer were tried).
pub const WRITE_QUORUM: usize = 2;
/// Below quorum: try that layer again after this long (not every round: relays ban
/// clients that republish constantly).
pub const PARTIAL_RETRY: Duration = Duration::from_secs(600);

/// When a write should count as made: now at quorum, or backdated so it comes due again
/// after [`PARTIAL_RETRY`] when it fell short.
fn written_at(now: u64, stored: usize, tried: usize) -> u64 {
    if stored >= tried.min(WRITE_QUORUM) {
        now
    } else {
        now.saturating_sub(REFRESH.as_secs()).saturating_add(PARTIAL_RETRY.as_secs())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteReport {
    pub written: usize,
    pub unchanged: usize,
    pub deferred: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadReport {
    pub slots_opened: usize,
    pub already_seen: usize,
    pub messages: usize,
    pub rejected: usize,
}

#[derive(Clone, Serialize, Deserialize)]
struct Written {
    fingerprint: [u8; 32],
    /// Unix seconds.
    at: u64,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

type Key = (usize, [u8; 32], [u8; 32]);

/// What survives restarts (Android runs each background sync in a fresh process): without
/// it every run would rewrite every slot and reopen every slot it had already applied.
#[derive(Default, Serialize, Deserialize)]
struct PersistedState {
    written: Vec<(Key, Written)>,
    seen: Vec<(Key, u64)>,
}

pub struct Syncer {
    replica: Arc<Mutex<Replica>>,
    mailboxes: Mutex<Vec<Arc<dyn Mailbox>>>,
    /// (mailbox, topic, writer seed) -> what was last written.
    written: Mutex<HashMap<(usize, [u8; 32], [u8; 32]), Written>>,
    /// (mailbox, topic, writer) -> newest version applied.
    seen: Mutex<HashMap<(usize, [u8; 32], [u8; 32]), u64>>,
    /// Slots the last read showed missing (force a rewrite).
    lost: Mutex<HashSet<(usize, [u8; 32], [u8; 32])>>,
    state_file: Option<PathBuf>,
    store: Arc<SlotStore>,
}

impl Syncer {
    pub fn new(replica: Arc<Mutex<Replica>>, mailboxes: Vec<Arc<dyn Mailbox>>) -> Arc<Syncer> {
        Self::with_state(replica, mailboxes, None)
    }

    /// Like [`Syncer::new`], remembering what was written and read in `state_file`.
    /// Mailbox indices must stay stable across runs for a given file.
    pub fn with_state(replica: Arc<Mutex<Replica>>, mailboxes: Vec<Arc<dyn Mailbox>>, state_file: Option<PathBuf>) -> Arc<Syncer> {
        Self::build(replica, mailboxes, state_file, None)
    }

    /// Full constructor: `store_file` keeps the local slot store (own groups) on disk.
    pub fn build(
        replica: Arc<Mutex<Replica>>,
        mailboxes: Vec<Arc<dyn Mailbox>>,
        state_file: Option<PathBuf>,
        store_file: Option<PathBuf>,
    ) -> Arc<Syncer> {
        let st: PersistedState = state_file
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        Arc::new(Syncer {
            replica,
            mailboxes: Mutex::new(mailboxes),
            written: Mutex::new(st.written.into_iter().collect()),
            seen: Mutex::new(st.seen.into_iter().collect()),
            lost: Mutex::new(HashSet::new()),
            state_file,
            store: Arc::new(SlotStore::open(store_file)),
        })
    }

    /// Add a mailbox after construction (e.g. a helper mailbox, which needs the P2P endpoint,
    /// which needs this syncer). Indices stay stable: mailboxes are only appended.
    pub fn add_mailbox(&self, mb: Arc<dyn Mailbox>) {
        self.mailboxes.lock().expect("lock").push(mb);
    }

    fn mbs(&self) -> Vec<Arc<dyn Mailbox>> {
        self.mailboxes.lock().expect("lock").clone()
    }

    pub fn store(&self) -> Arc<SlotStore> {
        self.store.clone()
    }

    fn save(&self) {
        let Some(p) = &self.state_file else { return };
        let st = PersistedState {
            written: self.written.lock().expect("lock").iter().map(|(k, v)| (*k, v.clone())).collect(),
            seen: self.seen.lock().expect("lock").iter().map(|(k, v)| (*k, *v)).collect(),
        };
        if let Ok(b) = postcard::to_stdvec(&st) {
            let _ = kt_engine::storage::write_atomic(p, &b);
        }
        self.store.save();
    }

    pub fn replica(&self) -> Arc<Mutex<Replica>> {
        self.replica.clone()
    }

    /// Write every slot that changed (or is due) to every mailbox.
    pub async fn write_round(&self) -> WriteReport {
        let replica = self.replica.clone();
        let plans = tokio::task::spawn_blocking(move || {
            let mut r = replica.lock().expect("replica lock");
            r.slot_targets()
                .into_iter()
                .filter_map(|t| r.slot_fingerprint(&t).ok().map(|fp| (t, fp)))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let mut report = WriteReport::default();
        let mailboxes = self.mbs();
        // layers that didn't answer within LAYER_CAP: not tried again this round (each slot would wait as long)
        let slow: Mutex<HashSet<usize>> = Mutex::new(HashSet::new());
        for (t, fp) in plans {
            let due: Vec<usize> = (0..mailboxes.len())
                .chain(std::iter::once(STORE))
                .filter(|mi| {
                    let key = (*mi, t.topic, t.writer_seed);
                    if self.lost.lock().expect("lock").contains(&key) {
                        return true;
                    }
                    match self.written.lock().expect("lock").get(&key) {
                        Some(w) => w.fingerprint != fp || now_secs().saturating_sub(w.at) > REFRESH.as_secs(),
                        None => true,
                    }
                })
                .collect();
            if due.is_empty() {
                report.unchanged += 1;
                continue;
            }
            let replica = self.replica.clone();
            let t2 = t.clone();
            let Ok(Ok(blob)) = tokio::task::spawn_blocking(move || replica.lock().expect("replica lock").slot_blob(&t2)).await else {
                report.failed += 1;
                continue;
            };
            let w = SlotWrite { topic: t.topic, writer_seed: t.writer_seed, blob };
            if due.contains(&STORE) {
                self.store.put(&SlotRead { topic: t.topic, writer: store_writer(&t.writer_seed), version: now_ms(), blob: w.blob.clone() });
                self.written.lock().expect("lock").insert((STORE, t.topic, t.writer_seed), Written { fingerprint: fp, at: now_secs() });
            }
            let due: Vec<usize> = due.into_iter().filter(|mi| *mi != STORE).collect();
            let outcomes = futures_util::future::join_all(due.iter().map(|mi| async {
                if slow.lock().expect("lock").contains(mi) {
                    return WriteOutcome::Unavailable;
                }
                tokio::time::timeout(LAYER_CAP, mailboxes[*mi].write(&w)).await.unwrap_or_else(|_| {
                    slow.lock().expect("lock").insert(*mi);
                    WriteOutcome::Unavailable
                })
            }))
            .await;
            for (mi, o) in due.iter().zip(outcomes) {
                let key = (*mi, t.topic, t.writer_seed);
                match o {
                    WriteOutcome::Stored { stored, tried } => {
                        report.written += 1;
                        self.written.lock().expect("lock").insert(key, Written { fingerprint: fp, at: written_at(now_secs(), stored, tried) });
                        self.lost.lock().expect("lock").remove(&key);
                    }
                    WriteOutcome::Deferred { .. } => report.deferred += 1,
                    WriteOutcome::Refused(_) | WriteOutcome::Unavailable => report.failed += 1,
                }
            }
        }
        self.save();
        report
    }

    /// Read members' slots from every mailbox and apply new versions. Repeats while reading
    /// reveals new topics (an epoch change learned from a forward slot).
    pub async fn read_round(&self) -> ReadReport {
        let mut report = ReadReport::default();
        let mut done_topics: HashSet<[u8; 32]> = HashSet::new();
        for _ in 0..3 {
            let replica = self.replica.clone();
            let (topics, my_targets) = tokio::task::spawn_blocking(move || {
                let r = replica.lock().expect("replica lock");
                (r.read_topics(), r.slot_targets())
            })
            .await
            .unwrap_or_default();
            let new_topics: Vec<ReadTopic> = topics.into_iter().filter(|t| !done_topics.contains(&t.topic)).collect();
            if new_topics.is_empty() {
                break;
            }
            let topic_ids: Vec<[u8; 32]> = new_topics.iter().map(|t| t.topic).collect();
            done_topics.extend(topic_ids.iter().copied());
            let mailboxes = self.mbs();
            let reads = read_all(&mailboxes, &topic_ids).await;
            // Group new slot versions by topic.
            let mut by_topic: HashMap<[u8; 32], Vec<Vec<u8>>> = HashMap::new();
            let mut marked: HashMap<[u8; 32], Vec<Key>> = HashMap::new();
            for (mi, slots) in reads.into_iter().enumerate() {
                // a layer that didn't answer in time: nothing from it this round (and no read-back verdict)
                let Some(slots) = slots else { continue };
                let own: HashMap<[u8; 32], [u8; 32]> =
                    my_targets.iter().map(|t| (mailboxes[mi].writer_id(&t.writer_seed), t.topic)).collect();
                let mut own_seen: HashSet<([u8; 32], [u8; 32])> = HashSet::new();
                for s in slots {
                    if own.get(&s.writer) == Some(&s.topic) {
                        own_seen.insert((s.topic, s.writer));
                        continue;
                    }
                    let key = (mi, s.topic, s.writer);
                    let mut seen = self.seen.lock().expect("lock");
                    if seen.get(&key).is_some_and(|v| *v >= s.version) {
                        report.already_seen += 1;
                        continue;
                    }
                    seen.insert(key, s.version);
                    self.store.put(&s);
                    marked.entry(s.topic).or_default().push(key);
                    by_topic.entry(s.topic).or_default().push(s.blob);
                }
                // Read-back: our own slots this mailbox should have returned.
                for t in &my_targets {
                    if !topic_ids.contains(&t.topic) {
                        continue;
                    }
                    let w = mailboxes[mi].writer_id(&t.writer_seed);
                    let key = (mi, t.topic, t.writer_seed);
                    if self.written.lock().expect("lock").contains_key(&key) && !own_seen.contains(&(t.topic, w)) {
                        self.lost.lock().expect("lock").insert(key);
                    }
                }
            }
            for t in new_topics {
                let Some(blobs) = by_topic.remove(&t.topic) else { continue };
                let topic = t.topic;
                let replica = self.replica.clone();
                let res = tokio::task::spawn_blocking(move || {
                    let refs: Vec<&[u8]> = blobs.iter().map(|b| b.as_slice()).collect();
                    (refs.len(), replica.lock().expect("replica lock").open_slots(&t, &refs))
                })
                .await;
                if let Ok((n, Ok(r))) = res {
                    report.slots_opened += n;
                    report.messages += r.messages;
                    report.rejected += r.failed + r.bad_slots;
                    if r.parked > 0 {
                        // Something in these slots waits for data we don't have yet: read
                        // them again next time instead of trusting in-memory parking.
                        let mut seen = self.seen.lock().expect("lock");
                        for k in marked.remove(&topic).unwrap_or_default() {
                            seen.remove(&k);
                        }
                    }
                }
            }
        }
        self.save();
        report
    }

    /// Apply slots received from a peer: keep the newer ones in the store and open them.
    pub async fn apply_incoming(&self, slots: Vec<SlotRead>) -> ReadReport {
        let mut report = ReadReport::default();
        let replica = self.replica.clone();
        let (topics, my_targets) = tokio::task::spawn_blocking(move || {
            let r = replica.lock().expect("replica lock");
            (r.read_topics(), r.slot_targets())
        })
        .await
        .unwrap_or_default();
        let own: HashSet<[u8; 32]> = my_targets.iter().map(|t| store_writer(&t.writer_seed)).collect();
        let mut by_topic: HashMap<[u8; 32], Vec<Vec<u8>>> = HashMap::new();
        for s in slots {
            if own.contains(&s.writer) || !topics.iter().any(|t| t.topic == s.topic) {
                continue;
            }
            if self.store.put(&s) {
                by_topic.entry(s.topic).or_default().push(s.blob);
            } else {
                report.already_seen += 1;
            }
        }
        for t in topics {
            let Some(blobs) = by_topic.remove(&t.topic) else { continue };
            let replica = self.replica.clone();
            let res = tokio::task::spawn_blocking(move || {
                let refs: Vec<&[u8]> = blobs.iter().map(|b| b.as_slice()).collect();
                (refs.len(), replica.lock().expect("replica lock").open_slots(&t, &refs))
            })
            .await;
            if let Ok((n, Ok(r))) = res {
                report.slots_opened += n;
                report.messages += r.messages;
                report.rejected += r.failed + r.bad_slots;
            }
        }
        self.store.save();
        report
    }

    /// One full round: send, receive, then send again if receiving changed our slots
    /// (e.g. merge duty, a new generation to rebase onto).
    pub async fn sync_once(&self) -> (WriteReport, ReadReport) {
        let mut w = self.write_round().await;
        let r = self.read_round().await;
        // Compact lists whose history outgrew them, where everyone active acknowledged (D26).
        let replica = self.replica.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let mut r = replica.lock().expect("replica lock");
            let _ = r.auto_rotate(); // periodic key rotation (D5 rule 7)
            r.auto_compact()
        })
        .await;
        let w2 = self.write_round().await;
        w.written += w2.written;
        w.deferred += w2.deferred;
        w.failed += w2.failed;
        (w, r)
    }

    /// Run until `stop` fires: write rounds every 30–60 s with jitter (D11), reads every
    /// `read_every`.
    pub async fn run(self: Arc<Self>, read_every: Duration, mut stop: tokio::sync::watch::Receiver<bool>) {
        use std::time::Instant;
        let mut next_write = Instant::now();
        let mut next_read = Instant::now();
        loop {
            if *stop.borrow() {
                return;
            }
            let now = Instant::now();
            if now >= next_write {
                self.write_round().await;
                next_write = Instant::now() + Duration::from_millis(30_000 + rand::random::<u64>() % 30_000);
            }
            if now >= next_read {
                self.read_round().await;
                next_read = Instant::now() + read_every;
            }
            let wake = next_write.min(next_read);
            tokio::select! {
                _ = tokio::time::sleep_until(wake.into()) => {}
                _ = stop.changed() => {}
            }
        }
    }
}

#[cfg(test)]
mod quorum_tests {
    use super::*;

    #[test]
    fn a_layer_is_done_only_at_quorum() {
        let now = 10 * REFRESH.as_secs();
        let due_after = |at: u64| at + REFRESH.as_secs() - now; // seconds until due again
        assert_eq!(written_at(now, 2, 3), now, "2 of 3 relays: done");
        assert_eq!(written_at(now, 1, 1), now, "the only one tried: done");
        assert_eq!(due_after(written_at(now, 1, 3)), PARTIAL_RETRY.as_secs(), "1 of 3: again in 10 minutes");
    }
}
