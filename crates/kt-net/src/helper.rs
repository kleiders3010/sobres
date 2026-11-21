//! Helper network, storage side (DESIGN §7a): devices keep other groups' slots in RAM and
//! on disk, like torrent seeding, so a group's changes wait somewhere even when all its
//! members are offline.
//!
//! **Disk copy** (decided 2026-09-24; RAM-only phones lost everything between jobs): one
//! encrypted file per slot, managed strictly because disk doesn't clear itself: the same
//! byte caps and quotas as RAM, slots unrefreshed for [`HelperLimits::ttl_ms`] deleted,
//! corrupt files deleted, [`wipe_disk`] when helping is turned off.
//!
//! **Helper (server):** [`HelperStore`], served on the device's iroh endpoint under
//! [`HELPER_ALPN`].
//! - RAM only for strangers' data; only the latest version per (topic, writer).
//! - **Signed puts:** the writer key is derived per device, group and epoch; a put is
//!   signed, so nobody can overwrite someone else's slot.
//! - **Junk-filling defenses:** proof of work for *new* slots, scaled with load (refreshes
//!   of existing slots are free); per-topic quota; young topics capped; when full, the
//!   least recently refreshed slots go first.
//! - Reports its session start, so clients can tell a restart (light) from data loss (heavy).
//!
//! **Client:** [`HelperMailbox`] implements [`Mailbox`]: it writes each slot to a few
//! helpers and reads from a few, preferring helpers with good [`Reliability`] records.
//! - **Replication:** the writer uploads to a few helpers and lists the rest of the
//!   group's helper set (`fanout`); those forward the signed put (the signature stays
//!   valid, so forwarding can't be abused). Forwarded puts don't fan out again.
//! - **Handoff:** before going offline a helper passes its slots to an online helper of
//!   each slot's set ([`crate::p2p::P2p::handoff`]).
//! - **Declared evictions:** a helper says which requested slots it evicted for space, so
//!   clients don't score that as withholding.
//!
//! Reliability records are local for now (sharing them within one's groups comes later).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::{RecvStream, SendStream};
use iroh::{EndpointAddr, EndpointId, PublicKey, SecretKey, Signature};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::mailbox::{BoxFut, Capabilities, Mailbox, SlotRead, SlotWrite, WriteOutcome};
use crate::p2p::P2p;

pub const HELPER_ALPN: &[u8] = b"kt/helper/1";
/// Blobs (up to [`HelperLimits::max_blob`]) travel in one frame.
const MAX_FRAME: usize = 10 * 1024 * 1024;

// ---------------------------------------------------------------- wire

/// A helper's address as carried in fan-out lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelperRef {
    pub endpoint: [u8; 32],
    pub ips: Vec<std::net::SocketAddr>,
    pub relays: Vec<String>,
    /// The relay this helper runs for others (`https://IP:port`, D51), if reachable. Its
    /// certificate carries `endpoint` as its key, which clients pin (`relay_tls`).
    #[serde(default)]
    pub relay: Option<String>,
}

impl HelperRef {
    pub fn from_addr(a: &EndpointAddr) -> HelperRef {
        let mut ips = Vec::new();
        let mut relays = Vec::new();
        for t in a.addrs.iter() {
            match t {
                iroh::TransportAddr::Ip(s) => ips.push(*s),
                iroh::TransportAddr::Relay(u) => relays.push(u.to_string()),
                _ => {}
            }
        }
        HelperRef { endpoint: *a.id.as_bytes(), ips, relays, relay: None }
    }

    pub fn to_addr(&self) -> Option<EndpointAddr> {
        crate::p2p::WhereData { endpoint: self.endpoint, relays: self.relays.clone(), ips: self.ips.clone(), dht: None }.to_addr()
    }
}

/// A signed put, as stored and forwarded.
#[derive(Clone, Serialize, Deserialize)]
pub struct SignedPut {
    pub topic: [u8; 32],
    pub writer: [u8; 32],
    pub version: u64,
    pub blob: Vec<u8>,
    pub sig: Vec<u8>,
    pub nonce: u64,
}

#[derive(Serialize, Deserialize)]
enum Request {
    /// `fanout`: other helpers of the set to forward to (empty on forwarded puts).
    Put { put: SignedPut, fanout: Vec<HelperRef> },
    Get { topics: Vec<[u8; 32]>, have: Vec<([u8; 32], [u8; 32], u64)> },
    Info,
    /// A helper introducing itself (helper peer exchange).
    Hello(HelperRef),
    /// Which other helpers do you know?
    Peers,
    /// Keep a sealed blob (proof of work unless already held: then it's a refresh).
    BlobPut { data: Vec<u8>, nonce: u64 },
    BlobGet { id: [u8; 32] },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PutReply {
    Stored,
    /// We already have this or a newer version.
    Stale,
    /// A new slot needs proof of work with this many leading zero bits.
    NeedPow(u8),
    TooLarge,
    /// Quota exhausted for this topic or the helper is full of established data.
    Full,
    BadSignature,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelperInfo {
    /// Changes when the helper restarts (RAM is lost then).
    pub session_start_ms: u64,
    pub used: u64,
    pub capacity: u64,
    pub pow_bits_new: u8,
}

#[derive(Serialize, Deserialize)]
enum Response {
    Put(PutReply),
    /// Slots newer than asked, plus the requested slots evicted for space this session.
    Slots(Vec<(([u8; 32], [u8; 32]), u64, Vec<u8>)>, Vec<([u8; 32], [u8; 32])>),
    Info(HelperInfo),
    Ok,
    Peers(Vec<HelperRef>),
    BlobPut(PutReply),
    Blob(Option<Vec<u8>>),
}

async fn send_frame<T: Serialize>(s: &mut SendStream, v: &T) -> Result<(), String> {
    let b = postcard::to_stdvec(v).map_err(|e| e.to_string())?;
    s.write_all(&(b.len() as u32).to_le_bytes()).await.map_err(|e| e.to_string())?;
    s.write_all(&b).await.map_err(|e| e.to_string())
}

async fn recv_frame<T: DeserializeOwned>(r: &mut RecvStream) -> Result<T, String> {
    // Bounded in time, memory grows only as bytes arrive (p2p::recv_frame_max).
    crate::p2p::recv_frame_max(r, MAX_FRAME).await
}

fn put_message(topic: &[u8; 32], version: u64, blob: &[u8]) -> Vec<u8> {
    let mut m = b"kt/helper-put/v1".to_vec();
    m.extend_from_slice(topic);
    m.extend_from_slice(&version.to_le_bytes());
    m.extend_from_slice(blake3::hash(blob).as_bytes());
    m
}

pub(crate) fn pow_leading_zeros(topic: &[u8; 32], writer: &[u8; 32], version: u64, nonce: u64) -> u32 {
    pow_bits(topic, writer, version, nonce)
}

fn pow_bits(topic: &[u8; 32], writer: &[u8; 32], version: u64, nonce: u64) -> u32 {
    let mut h = blake3::Hasher::new_derive_key("kt/helper-pow/v1");
    h.update(topic);
    h.update(writer);
    h.update(&version.to_le_bytes());
    h.update(&nonce.to_le_bytes());
    let out = h.finalize();
    let b = out.as_bytes();
    let mut z = 0;
    for byte in b {
        if *byte == 0 {
            z += 8;
        } else {
            z += byte.leading_zeros();
            break;
        }
    }
    z
}

/// Find a nonce with at least `bits` leading zero bits (2^bits hashes on average).
pub fn solve_pow(topic: &[u8; 32], writer: &[u8; 32], version: u64, bits: u8) -> u64 {
    let mut nonce = rand::random::<u64>();
    while pow_bits(topic, writer, version, nonce) < bits as u32 {
        nonce = nonce.wrapping_add(1);
    }
    nonce
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---------------------------------------------------------------- helper (server)

/// Where and how the disk copy lives.
#[derive(Clone, Debug)]
pub struct DiskConfig {
    pub dir: std::path::PathBuf,
    /// Device-held key (encryption at rest); `SymKey` never prints.
    pub key: kt_engine::crypto::SymKey,
}

#[derive(Clone, Debug)]
pub struct HelperLimits {
    /// RAM for strangers' slots (e.g. 16–32 MB on phones, 64–256 MB on desktops).
    pub capacity: u64,
    pub per_topic: u64,
    pub max_slot: u64,
    /// PoW bits for a new slot on an empty helper; grows with load.
    pub base_pow_bits: u8,
    /// A topic younger than this may use at most `young_topic_cap` bytes.
    pub young_age: Duration,
    pub young_topic_cap: u64,
    /// Slots not refreshed for this long are deleted (RAM and disk).
    pub ttl_ms: u64,
    /// Keep a disk copy (None = RAM only).
    pub disk: Option<DiskConfig>,
    /// Bytes of strangers' blobs (text attachments) kept, and the largest one taken.
    pub blob_capacity: u64,
    pub max_blob: u64,
}

impl Default for HelperLimits {
    fn default() -> Self {
        HelperLimits {
            capacity: 64 * 1024 * 1024,
            per_topic: 2 * 1024 * 1024,
            max_slot: 640 * 1024,
            base_pow_bits: 12,
            young_age: Duration::from_secs(24 * 3600),
            young_topic_cap: 256 * 1024,
            ttl_ms: 14 * 24 * 3600 * 1000,
            disk: None,
            blob_capacity: 32 * 1024 * 1024,
            max_blob: 8 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    put: SignedPut,
    /// Wall-clock ms (survives restarts with the disk copy).
    refreshed: u64,
    /// The slot's helper set as its writer told us (for handoff).
    set: Vec<HelperRef>,
    /// When its topic was first seen (young-topic cap), ms.
    topic_first_seen: u64,
}

struct TopicInfo {
    first_seen: u64,
    bytes: u64,
}

const DISK_AAD: &[u8] = b"kt/helper-disk/v1";

fn entry_file(dir: &std::path::Path, key: &([u8; 32], [u8; 32])) -> std::path::PathBuf {
    let mut m = key.0.to_vec();
    m.extend_from_slice(&key.1);
    let h = hex::encode(&blake3::hash(&m).as_bytes()[..16]);
    dir.join(&h[..2]).join(&h[2..])
}

fn verify_put(p: &SignedPut) -> bool {
    let (Ok(pk), Ok(sig)) = (PublicKey::from_bytes(&p.writer), <[u8; 64]>::try_from(p.sig.as_slice())) else { return false };
    pk.verify(&put_message(&p.topic, p.version, &p.blob), &Signature::from_bytes(&sig)).is_ok()
}

/// Delete a helper's disk copy (helping turned off), blobs included.
pub fn wipe_disk(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
    let mut name = dir.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push("-blobs");
    let _ = std::fs::remove_dir_all(dir.with_file_name(name));
}

pub struct HelperStore {
    limits: HelperLimits,
    session_start_ms: u64,
    inner: Mutex<(HashMap<([u8; 32], [u8; 32]), Entry>, HashMap<[u8; 32], TopicInfo>, u64)>,
    evicted: Mutex<std::collections::HashSet<([u8; 32], [u8; 32])>>,
    /// Other helpers this helper has heard of (introductions, replication fan-out lists).
    peers: Mutex<HashMap<[u8; 32], (HelperRef, u64)>>,
    /// Strangers' sealed blobs.
    pub blobs: crate::blobs::BlobHold,
}

const MAX_PEERS: usize = 512;

impl HelperStore {
    /// Remember another helper (bounded; oldest forgotten first).
    pub fn learn_peer(&self, h: HelperRef) {
        if h.ips.len() > 16 || h.relays.len() > 8 {
            return;
        }
        let mut p = self.peers.lock().expect("lock");
        p.insert(h.endpoint, (h, now_ms()));
        if p.len() > MAX_PEERS {
            if let Some(oldest) = p.iter().min_by_key(|(_, v)| v.1).map(|(k, _)| *k) {
                p.remove(&oldest);
            }
        }
    }

    /// Up to 32 known helpers, most recently heard first.
    pub fn known_peers(&self) -> Vec<HelperRef> {
        let mut v: Vec<(HelperRef, u64)> = self.peers.lock().expect("lock").values().cloned().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        v.into_iter().take(32).map(|x| x.0).collect()
    }

    pub fn new(limits: HelperLimits) -> Arc<HelperStore> {
        let store = HelperStore {
            session_start_ms: now_ms(),
            blobs: crate::blobs::BlobHold::new(&limits),
            limits,
            inner: Mutex::new((HashMap::new(), HashMap::new(), 0)),
            evicted: Mutex::new(std::collections::HashSet::new()),
            peers: Mutex::new(HashMap::new()),
        };
        Arc::new(store.load_disk())
    }

    /// Reload the disk copy: verify every file (signature, expiry, caps), delete what
    /// doesn't pass. The store's start time is kept with it, so a restart that kept the
    /// data isn't taken for data loss.
    fn load_disk(mut self) -> HelperStore {
        let Some(d) = self.limits.disk.clone() else { return self };
        let _ = std::fs::create_dir_all(&d.dir);
        let start_file = d.dir.join("store_start");
        match std::fs::read(&start_file).ok().and_then(|b| b.try_into().ok().map(u64::from_le_bytes)) {
            Some(t) => self.session_start_ms = t,
            None => {
                let _ = kt_engine::storage::write_atomic(&start_file, &self.session_start_ms.to_le_bytes());
            }
        }
        let now = now_ms();
        let mut loaded: Vec<Entry> = Vec::new();
        for shard in std::fs::read_dir(&d.dir).into_iter().flatten().flatten() {
            if !shard.path().is_dir() {
                continue;
            }
            for f in std::fs::read_dir(shard.path()).into_iter().flatten().flatten() {
                let ok = std::fs::read(f.path())
                    .ok()
                    .and_then(|b| kt_engine::crypto::open(&d.key, DISK_AAD, &b).ok())
                    .and_then(|b| postcard::from_bytes::<Entry>(&b).ok())
                    .filter(|e| verify_put(&e.put) && now.saturating_sub(e.refreshed) < self.limits.ttl_ms && e.put.blob.len() as u64 <= self.limits.max_slot);
                match ok {
                    Some(e) if entry_file(&d.dir, &(e.put.topic, e.put.writer)) == f.path() => loaded.push(e),
                    _ => {
                        let _ = std::fs::remove_file(f.path());
                    }
                }
            }
        }
        // Newest first, within the caps (a smaller cap after a settings change trims here).
        loaded.sort_by_key(|e| std::cmp::Reverse(e.refreshed));
        {
            let (entries, topics, used) = self.inner.get_mut().expect("lock");
            for e in loaded {
                let n = e.put.blob.len() as u64;
                let t = topics.entry(e.put.topic).or_insert(TopicInfo { first_seen: e.topic_first_seen, bytes: 0 });
                if *used + n > self.limits.capacity || t.bytes + n > self.limits.per_topic {
                    let _ = std::fs::remove_file(entry_file(&d.dir, &(e.put.topic, e.put.writer)));
                    continue;
                }
                t.first_seen = t.first_seen.min(e.topic_first_seen);
                t.bytes += n;
                *used += n;
                entries.insert((e.put.topic, e.put.writer), e);
            }
        }
        self
    }

    fn disk_write(&self, e: &Entry) {
        let Some(d) = &self.limits.disk else { return };
        let path = entry_file(&d.dir, &(e.put.topic, e.put.writer));
        if let (Some(parent), Ok(b)) = (path.parent(), postcard::to_stdvec(e)) {
            let _ = std::fs::create_dir_all(parent);
            let _ = kt_engine::storage::write_atomic(&path, &kt_engine::crypto::seal(&d.key, DISK_AAD, &b));
        }
    }

    fn disk_remove(&self, key: &([u8; 32], [u8; 32])) {
        if let Some(d) = &self.limits.disk {
            let _ = std::fs::remove_file(entry_file(&d.dir, key));
        }
    }

    /// Delete slots nobody refreshed within the TTL (RAM and disk). Returns how many.
    pub fn sweep(&self) -> usize {
        let now = now_ms();
        let mut g = self.inner.lock().expect("lock");
        let (entries, topics, used) = &mut *g;
        let stale: Vec<([u8; 32], [u8; 32])> = entries.iter().filter(|(_, e)| now.saturating_sub(e.refreshed) >= self.limits.ttl_ms).map(|(k, _)| *k).collect();
        for k in &stale {
            if let Some(e) = entries.remove(k) {
                let n = e.put.blob.len() as u64;
                *used -= n;
                if let Some(t) = topics.get_mut(&k.0) {
                    t.bytes -= n;
                }
            }
            self.disk_remove(k);
        }
        topics.retain(|_, t| t.bytes > 0);
        drop(g);
        stale.len() + self.blobs.sweep()
    }

    /// How many slots are held, and bytes used.
    pub fn held(&self) -> (usize, u64) {
        let g = self.inner.lock().expect("lock");
        (g.0.len(), g.2)
    }

    pub fn info(&self) -> HelperInfo {
        let used = self.inner.lock().expect("lock").2;
        HelperInfo { session_start_ms: self.session_start_ms, used, capacity: self.limits.capacity, pow_bits_new: self.pow_bits_now(used) }
    }

    fn pow_bits_now(&self, used: u64) -> u8 {
        // +1 bit per eighth of capacity in use: cheap when empty, costly for fillers.
        let load = (used * 8 / self.limits.capacity.max(1)).min(8) as u8;
        self.limits.base_pow_bits + load
    }

    pub fn put(&self, topic: [u8; 32], writer: [u8; 32], version: u64, blob: Vec<u8>, sig: &[u8], nonce: u64) -> PutReply {
        self.put_signed(SignedPut { topic, writer, version, blob, sig: sig.to_vec(), nonce }, Vec::new())
    }

    /// Store a signed put (with the writer's helper set, kept for handoff).
    pub fn put_signed(&self, p: SignedPut, set: Vec<HelperRef>) -> PutReply {
        let SignedPut { topic, writer, version, ref blob, ref sig, nonce } = p;
        if blob.len() as u64 > self.limits.max_slot {
            return PutReply::TooLarge;
        }
        let (Ok(pk), Ok(sig)) = (PublicKey::from_bytes(&writer), <[u8; 64]>::try_from(sig.as_slice())) else { return PutReply::BadSignature };
        if pk.verify(&put_message(&topic, version, blob), &Signature::from_bytes(&sig)).is_err() {
            return PutReply::BadSignature;
        }
        let mut g = self.inner.lock().expect("lock");
        let (entries, topics, used) = &mut *g;
        let key = (topic, writer);
        let old_len = match entries.get(&key) {
            Some(e) if e.put.version >= version => return PutReply::Stale,
            Some(e) => e.put.blob.len() as u64,
            None => {
                let need = self.pow_bits_now(*used);
                if pow_bits(&topic, &writer, version, nonce) < need as u32 {
                    return PutReply::NeedPow(need);
                }
                0
            }
        };
        let now = now_ms();
        let ti = topics.entry(topic).or_insert(TopicInfo { first_seen: now, bytes: 0 });
        let topic_first_seen = ti.first_seen;
        let new_topic_bytes = ti.bytes - old_len + blob.len() as u64;
        let young = now.saturating_sub(ti.first_seen) < self.limits.young_age.as_millis() as u64;
        let cap = if young { self.limits.young_topic_cap } else { self.limits.per_topic };
        if new_topic_bytes > cap {
            return PutReply::Full;
        }
        // Make room: evict the least recently refreshed slots of *other* topics.
        let mut need = (*used - old_len + blob.len() as u64).saturating_sub(self.limits.capacity);
        while need > 0 {
            let victim = entries.iter().filter(|(k, _)| k.0 != topic).min_by_key(|(_, e)| e.refreshed).map(|(k, _)| *k);
            let Some(v) = victim else { return PutReply::Full };
            let e = entries.remove(&v).expect("exists");
            self.disk_remove(&v);
            let n = e.put.blob.len() as u64;
            let mut ev = self.evicted.lock().expect("lock");
            if ev.len() < 100_000 {
                ev.insert(v);
            }
            *used -= n;
            if let Some(t) = topics.get_mut(&v.0) {
                t.bytes -= n;
            }
            need = need.saturating_sub(n);
        }
        *used = *used - old_len + blob.len() as u64;
        topics.get_mut(&topic).expect("inserted").bytes = new_topic_bytes;
        let set = if set.is_empty() { entries.get(&key).map(|e| e.set.clone()).unwrap_or_default() } else { set };
        let entry = Entry { put: p, refreshed: now, set, topic_first_seen };
        self.disk_write(&entry);
        entries.insert(key, entry);
        self.evicted.lock().expect("lock").remove(&key);
        PutReply::Stored
    }

    pub fn get(&self, topics: &[[u8; 32]], have: &[([u8; 32], [u8; 32], u64)]) -> Vec<(([u8; 32], [u8; 32]), u64, Vec<u8>)> {
        let have: HashMap<([u8; 32], [u8; 32]), u64> = have.iter().map(|(t, w, v)| ((*t, *w), *v)).collect();
        let g = self.inner.lock().expect("lock");
        g.0.iter()
            .filter(|(k, _)| topics.contains(&k.0))
            .filter(|(k, e)| have.get(*k).is_none_or(|v| *v < e.put.version))
            .map(|(k, e)| (*k, e.put.version, e.put.blob.clone()))
            .collect()
    }

    /// Requested slots we evicted for space this session (declared, not withheld).
    pub fn evicted_among(&self, topics: &[[u8; 32]]) -> Vec<([u8; 32], [u8; 32])> {
        self.evicted.lock().expect("lock").iter().filter(|k| topics.contains(&k.0)).copied().collect()
    }

    /// Everything held, with each slot's helper set (for handoff).
    pub fn all(&self) -> Vec<(SignedPut, Vec<HelperRef>)> {
        self.inner.lock().expect("lock").0.values().map(|e| (e.put.clone(), e.set.clone())).collect()
    }

    /// Serve one helper stream. Returns a put to forward to its fan-out list, if any.
    pub async fn serve(&self, mut s: SendStream, mut r: RecvStream) -> Result<Option<(SignedPut, Vec<HelperRef>)>, String> {
        let req: Request = recv_frame(&mut r).await?;
        let mut forward = None;
        let resp = match req {
            Request::Put { put, fanout } => {
                let fanout: Vec<HelperRef> = fanout.into_iter().take(32).collect();
                for h in &fanout {
                    self.learn_peer(h.clone());
                }
                let reply = self.put_signed(put.clone(), fanout.clone());
                if reply == PutReply::Stored && !fanout.is_empty() {
                    forward = Some((put, fanout));
                }
                Response::Put(reply)
            }
            Request::Get { topics, have } => {
                let ev = self.evicted_among(&topics);
                Response::Slots(self.get(&topics, &have), ev)
            }
            Request::Info => Response::Info(self.info()),
            Request::Hello(h) => {
                self.learn_peer(h);
                Response::Ok
            }
            Request::Peers => Response::Peers(self.known_peers()),
            Request::BlobPut { data, nonce } => Response::BlobPut(self.blobs.put(data, nonce)),
            Request::BlobGet { id } => Response::Blob(self.blobs.get(&id)),
        };
        send_frame(&mut s, &resp).await?;
        let _ = s.finish();
        Ok(forward)
    }
}

/// Forward a signed put to other helpers (replication / handoff). No further fan-out.
pub async fn forward(p2p: &P2p, put: &SignedPut, to: &[HelperRef]) -> usize {
    let mut ok = 0;
    for h in to {
        let Some(addr) = h.to_addr() else { continue };
        if addr.id == p2p.endpoint().id() {
            continue;
        }
        let res = async {
            let conn = p2p.connection_alpn(addr, HELPER_ALPN).await?;
            let (mut s, mut r) = conn.open_bi().await.map_err(|e| e.to_string())?;
            send_frame(&mut s, &Request::Put { put: put.clone(), fanout: vec![] }).await?;
            let _ = s.finish();
            recv_frame::<Response>(&mut r).await
        }
        .await;
        if let Ok(Response::Put(PutReply::Stored | PutReply::Stale)) = res {
            ok += 1;
        }
    }
    ok
}

// ---------------------------------------------------------------- client

/// What we know about one helper from our own dealings with it (§7a table).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reliability {
    pub reachable: u32,
    pub unreachable: u32,
    /// Failed calls since the last successful one.
    pub unreachable_in_a_row: u32,
    /// Returned an older version (or nothing) of a slot we stored on it this session.
    pub withheld: u32,
    /// Restarts noticed (session start changed): counted like downtime, lightly.
    pub restarts: u32,
    pub latency_ms: f64,
    pub last_session: u64,
    /// Returned data failing verification: excluded.
    pub excluded: bool,
}

impl Reliability {
    pub fn score(&self) -> f64 {
        if self.excluded {
            return f64::NEG_INFINITY;
        }
        let total = (self.reachable + self.unreachable).max(1) as f64;
        let up = self.reachable as f64 / total;
        up - 0.5 * self.withheld as f64 - 0.05 * self.restarts as f64 - self.latency_ms / 10_000.0
    }
}

/// A topic's helpers: the group's recorded helper set (`None`: no set yet, use the pool).
pub type HelperResolver = Arc<dyn Fn(&[u8; 32]) -> Option<Vec<EndpointAddr>> + Send + Sync>;

pub struct HelperMailbox {
    p2p: Arc<P2p>,
    /// This device's own helper candidates, best first.
    pool: Mutex<Vec<EndpointAddr>>,
    resolver: Mutex<Option<HelperResolver>>,
    down: Mutex<HashMap<EndpointId, (Instant, Duration)>>,
    records: Mutex<HashMap<EndpointId, Reliability>>,
    /// What we stored where this session: (helper, topic, writer) -> version.
    stored: Mutex<HashMap<(EndpointId, [u8; 32], [u8; 32]), u64>>,
    /// Missing slots the helper declared evicted (very light, §7a).
    pub declared_evictions: Mutex<u32>,
    writes_per_slot: usize,
    reads_per_topic: usize,
    /// When any helper last answered (proof this device can reach the network).
    last_success: Mutex<Option<Instant>>,
    /// Other members' observations (already weighted and decayed, `health`).
    shared: Mutex<HashMap<EndpointId, Reliability>>,
}

/// A device that reached a helper this recently counts as online (see [`HelperMailbox::we_are_online`]).
const ONLINE_EVIDENCE: Duration = Duration::from_secs(300);

fn writer_key(seed: &[u8; 32]) -> SecretKey {
    let k = kt_engine::crypto::derive(seed, "kt/helper-writer/v1", b"");
    SecretKey::from_bytes(&k.0)
}

impl HelperMailbox {
    pub fn new(p2p: Arc<P2p>, helpers: Vec<EndpointAddr>) -> HelperMailbox {
        HelperMailbox {
            p2p,
            pool: Mutex::new(helpers),
            resolver: Mutex::new(None),
            down: Mutex::new(HashMap::new()),
            records: Mutex::new(HashMap::new()),
            stored: Mutex::new(HashMap::new()),
            declared_evictions: Mutex::new(0),
            writes_per_slot: 3,
            reads_per_topic: 4,
            last_success: Mutex::new(None),
            shared: Mutex::new(HashMap::new()),
        }
    }

    /// Other members' observations, replacing the previous set (`health::apply`).
    pub fn set_shared(&self, shared: HashMap<EndpointId, Reliability>) {
        *self.shared.lock().expect("lock") = shared;
    }

    /// Only this device's own observations (what it shares with its groups).
    pub fn own_records(&self) -> HashMap<EndpointId, Reliability> {
        self.records.lock().expect("lock").clone()
    }

    /// Own observations plus members' (counts added; exclusion from either side).
    fn merged(&self, id: &EndpointId) -> Option<Reliability> {
        let own = self.records.lock().expect("lock").get(id).cloned();
        let sh = self.shared.lock().expect("lock").get(id).cloned();
        match (own, sh) {
            (None, None) => None,
            (Some(o), None) => Some(o),
            (None, Some(s)) => Some(Reliability { unreachable_in_a_row: 0, ..s }),
            (Some(o), Some(s)) => Some(Reliability {
                reachable: o.reachable + s.reachable,
                unreachable: o.unreachable + s.unreachable,
                withheld: o.withheld + s.withheld,
                restarts: o.restarts + s.restarts,
                excluded: o.excluded || s.excluded,
                ..o
            }),
        }
    }

    /// Test hook: record an observation as if made by this device.
    #[doc(hidden)]
    pub fn note_for_tests(&self, id: EndpointId, r: Reliability) {
        self.records.lock().expect("lock").insert(id, r);
    }

    /// Failures only count against helpers when this device is demonstrably online: it
    /// has a home relay, or some helper answered recently. A device that starts offline
    /// (or loses its network) mustn't drop helpers, and their relays, for its own state.
    fn we_are_online(&self) -> bool {
        self.p2p.is_online() || self.last_success.lock().expect("lock").is_some_and(|t| t.elapsed() < ONLINE_EVIDENCE)
    }

    /// How many helpers get each slot directly (the rest by replication). Default 3.
    pub fn with_direct_writes(mut self, n: usize) -> Self {
        self.writes_per_slot = n.max(1);
        self
    }

    /// What this device knows about helpers: its own observations plus its groups'.
    pub fn records(&self) -> HashMap<EndpointId, Reliability> {
        let mut ids: Vec<EndpointId> = self.records.lock().expect("lock").keys().copied().collect();
        ids.extend(self.shared.lock().expect("lock").keys().copied());
        ids.sort_by_key(|i| *i.as_bytes());
        ids.dedup();
        ids.into_iter().filter_map(|i| self.merged(&i).map(|r| (i, r))).collect()
    }

    pub fn set_pool(&self, helpers: Vec<EndpointAddr>) {
        *self.pool.lock().expect("lock") = helpers;
    }

    pub fn pool(&self) -> Vec<EndpointAddr> {
        self.pool.lock().expect("lock").clone()
    }

    pub fn set_resolver(&self, r: HelperResolver) {
        *self.resolver.lock().expect("lock") = Some(r);
    }

    pub fn p2p(&self) -> &Arc<P2p> {
        &self.p2p
    }

    /// Helpers for this topic that are working as far as this device knows (reached
    /// before, not failing). The cloud mailbox reads more often when this is low.
    pub fn healthy_helpers(&self, topic: &[u8; 32]) -> usize {
        self.ranked(topic).iter().filter(|h| !self.is_failing(&h.id) && self.merged(&h.id).is_some_and(|r| r.reachable > 0)).count()
    }

    /// Whether this device's records say a helper isn't working.
    pub fn is_failing(&self, id: &EndpointId) -> bool {
        let own_streak = self.records.lock().expect("lock").get(id).is_some_and(|r| r.unreachable_in_a_row >= 3);
        own_streak || self.merged(id).is_some_and(|r| r.excluded || r.score() < 0.0)
    }

    /// Skip a helper that just failed, for a growing interval (fail fast, like relays).
    fn is_down(&self, id: &EndpointId) -> bool {
        self.down.lock().expect("lock").get(id).is_some_and(|(until, _)| Instant::now() < *until)
    }

    /// Helpers for a topic: best reliability first, ties broken by a per-topic hash (so
    /// writers and readers tend to agree without talking).
    fn ranked(&self, topic: &[u8; 32]) -> Vec<EndpointAddr> {
        let resolver = self.resolver.lock().expect("lock").clone();
        let base = resolver.and_then(|r| r(topic)).filter(|v| !v.is_empty()).unwrap_or_else(|| self.pool());
        let mut v: Vec<(i64, [u8; 32], EndpointAddr)> = base
            .iter()
            .filter_map(|h| Some((h, self.merged(&h.id))))
            .filter(|(_, r)| !r.as_ref().is_some_and(|r| r.excluded))
            .map(|(h, r)| {
                let s = r.map(|r| r.score()).unwrap_or(0.5);
                let mut m = topic.to_vec();
                m.extend_from_slice(h.id.as_bytes());
                (-(s * 1000.0) as i64, *blake3::hash(&m).as_bytes(), h.clone())
            })
            .collect();
        v.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        v.into_iter().map(|x| x.2).collect()
    }

    async fn call(&self, h: &EndpointAddr, req: &Request) -> Option<Response> {
        if self.is_down(&h.id) {
            let mut rec = self.records.lock().expect("lock");
            let e = rec.entry(h.id).or_default();
            e.unreachable_in_a_row += 1;
            return None;
        }
        let t = Instant::now();
        let res = async {
            let conn = self.p2p.connection_alpn(h.clone(), HELPER_ALPN).await?;
            let (mut s, mut r) = conn.open_bi().await.map_err(|e| e.to_string())?;
            send_frame(&mut s, req).await?;
            let _ = s.finish();
            recv_frame::<Response>(&mut r).await
        }
        .await;
        let mut rec = self.records.lock().expect("lock");
        let e = rec.entry(h.id).or_default();
        match res {
            Ok(r) => {
                *self.last_success.lock().expect("lock") = Some(Instant::now());
                e.reachable += 1;
                e.unreachable_in_a_row = 0;
                self.down.lock().expect("lock").remove(&h.id);
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                e.latency_ms = if e.latency_ms == 0.0 { ms } else { e.latency_ms * 0.8 + ms * 0.2 };
                Some(r)
            }
            Err(_) if !self.we_are_online() => {
                // We're the ones offline: not the helper's fault. No penalty, no backoff.
                None
            }
            Err(_) => {
                e.unreachable += 1;
                e.unreachable_in_a_row += 1;
                let mut d = self.down.lock().expect("lock");
                let next = d.get(&h.id).map(|(_, iv)| (*iv * 2).min(Duration::from_secs(1800))).unwrap_or(Duration::from_secs(30));
                d.insert(h.id, (Instant::now() + next, next));
                None
            }
        }
    }

    /// Put a sealed blob on up to `copies` helpers (ranked for its ID, so readers look in
    /// the same place). Returns how many hold it now.
    pub async fn put_blob(&self, data: &[u8], copies: usize) -> usize {
        let id = kt_engine::blobs::blob_id(data);
        let len = data.len() as u64;
        let mut ok = 0;
        for h in self.ranked(&id) {
            if ok >= copies {
                break;
            }
            let mut nonce = 0u64;
            for _ in 0..2 {
                match self.call(&h, &Request::BlobPut { data: data.to_vec(), nonce }).await {
                    Some(Response::BlobPut(PutReply::NeedPow(bits))) if bits <= 26 => {
                        nonce = match tokio::task::spawn_blocking(move || crate::blobs::solve_blob_pow(&id, len, bits)).await {
                            Ok(n) => n,
                            Err(_) => break,
                        };
                    }
                    Some(Response::BlobPut(PutReply::Stored | PutReply::Stale)) => {
                        ok += 1;
                        break;
                    }
                    _ => break,
                }
            }
        }
        ok
    }

    /// Fetch a blob from the helpers ranked for its ID; only the exact bytes are accepted.
    pub async fn get_blob(&self, id: &[u8; 32]) -> Option<Vec<u8>> {
        for h in self.ranked(id).into_iter().take(crate::blobs::BLOB_COPIES + 2) {
            if let Some(Response::Blob(Some(b))) = self.call(&h, &Request::BlobGet { id: *id }).await {
                if kt_engine::blobs::blob_id(&b) == *id {
                    return Some(b);
                }
            }
        }
        None
    }

    /// Ask a helper which other helpers it knows (helper peer exchange).
    pub async fn ask_peers(&self, h: &EndpointAddr) -> Vec<HelperRef> {
        match self.call(h, &Request::Peers).await {
            Some(Response::Peers(v)) => v.into_iter().take(32).collect(),
            _ => Vec::new(),
        }
    }

    /// Introduce ourselves (as a helper) to another helper.
    pub async fn introduce(&self, h: &EndpointAddr, me: HelperRef) -> bool {
        matches!(self.call(h, &Request::Hello(me)).await, Some(Response::Ok))
    }

    async fn info(&self, h: &EndpointAddr) -> Option<HelperInfo> {
        match self.call(h, &Request::Info).await? {
            Response::Info(i) => {
                let mut rec = self.records.lock().expect("lock");
                let e = rec.entry(h.id).or_default();
                if e.last_session != 0 && e.last_session != i.session_start_ms {
                    e.restarts += 1;
                    // RAM was lost: what we stored there before is gone, not withheld.
                    self.stored.lock().expect("lock").retain(|k, _| k.0 != h.id);
                }
                e.last_session = i.session_start_ms;
                Some(i)
            }
            _ => None,
        }
    }

    async fn put_one(&self, h: &EndpointAddr, topic: [u8; 32], key: &SecretKey, version: u64, blob: &[u8], fanout: &[HelperRef]) -> Option<PutReply> {
        self.info(h).await?;
        let writer = *key.public().as_bytes();
        let sig = key.sign(&put_message(&topic, version, blob)).to_bytes().to_vec();
        let mut nonce = 0u64;
        for _ in 0..2 {
            let put = SignedPut { topic, writer, version, blob: blob.to_vec(), sig: sig.clone(), nonce };
            let req = Request::Put { put, fanout: fanout.to_vec() };
            match self.call(h, &req).await? {
                Response::Put(PutReply::NeedPow(bits)) if bits <= 24 => {
                    nonce = tokio::task::spawn_blocking(move || solve_pow(&topic, &writer, version, bits)).await.ok()?;
                }
                Response::Put(r) => {
                    if matches!(r, PutReply::Stored | PutReply::Stale) {
                        self.stored.lock().expect("lock").insert((h.id, topic, writer), version);
                    }
                    return Some(r);
                }
                _ => return None,
            }
        }
        None
    }
}

impl Mailbox for HelperMailbox {
    fn name(&self) -> String {
        format!("helpers({})", self.pool().len())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { replace: true, max_slot: 640 * 1024, ttl: Some(Duration::from_secs(14 * 24 * 3600)), read_auth: false, subscribe: false, write_budget: None }
    }

    fn writer_id(&self, writer_seed: &[u8; 32]) -> [u8; 32] {
        *writer_key(writer_seed).public().as_bytes()
    }

    fn write<'a>(&'a self, w: &'a SlotWrite) -> BoxFut<'a, WriteOutcome> {
        Box::pin(async move {
            let key = writer_key(&w.writer_seed);
            let version = now_ms();
            let ranked = self.ranked(&w.topic);
            let targets: Vec<EndpointAddr> = ranked.iter().take(self.writes_per_slot).cloned().collect();
            if targets.is_empty() {
                return WriteOutcome::Unavailable;
            }
            // The rest of the set gets it by replication from the first helpers.
            let rest: Vec<HelperRef> = ranked.iter().skip(self.writes_per_slot).map(HelperRef::from_addr).collect();
            let results = futures_util::future::join_all(targets.iter().map(|h| self.put_one(h, w.topic, &key, version, &w.blob, &rest))).await;
            let stored = results.iter().filter(|r| matches!(r, Some(PutReply::Stored) | Some(PutReply::Stale))).count();
            if stored > 0 {
                WriteOutcome::Stored { stored, tried: targets.len() }
            } else if results.iter().any(|r| matches!(r, Some(PutReply::TooLarge))) {
                WriteOutcome::Refused("slot too large for helpers".into())
            } else {
                WriteOutcome::Unavailable
            }
        })
    }

    fn read<'a>(&'a self, topics: &'a [[u8; 32]]) -> BoxFut<'a, Vec<SlotRead>> {
        Box::pin(async move {
            let mut per: HashMap<EndpointId, (EndpointAddr, Vec<[u8; 32]>)> = HashMap::new();
            for t in topics {
                for h in self.ranked(t).into_iter().take(self.reads_per_topic) {
                    per.entry(h.id).or_insert_with(|| (h.clone(), Vec::new())).1.push(*t);
                }
            }
            let calls = per.into_values().map(|(h, ts)| async move {
                let info = self.info(&h).await;
                let r = self.call(&h, &Request::Get { topics: ts.clone(), have: vec![] }).await;
                (h, ts, info, r)
            });
            let results = futures_util::future::join_all(calls).await;
            let mut best: HashMap<([u8; 32], [u8; 32]), SlotRead> = HashMap::new();
            for (h, ts, _info, r) in results {
                let Some(Response::Slots(slots, evicted)) = r else { continue };
                let got: HashMap<([u8; 32], [u8; 32]), u64> = slots.iter().map(|(k, v, _)| (*k, *v)).collect();
                // Withholding check: slots we stored on this helper this session.
                let stored: Vec<_> = self.stored.lock().expect("lock").iter().filter(|(k, _)| k.0 == h.id && ts.contains(&k.1)).map(|(k, v)| ((k.1, k.2), *v)).collect();
                for (k, v) in stored {
                    if got.get(&k).is_none_or(|g| *g < v) {
                        if evicted.contains(&k) {
                            *self.declared_evictions.lock().expect("lock") += 1;
                            self.stored.lock().expect("lock").remove(&(h.id, k.0, k.1));
                        } else {
                            self.records.lock().expect("lock").entry(h.id).or_default().withheld += 1;
                        }
                    }
                }
                for ((t, w), v, blob) in slots {
                    if !ts.contains(&t) {
                        continue;
                    }
                    if best.get(&(t, w)).is_none_or(|b| b.version < v) {
                        best.insert((t, w), SlotRead { topic: t, writer: w, version: v, blob });
                    }
                }
            }
            best.into_values().collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk_limits(dir: &std::path::Path, key: [u8; 32]) -> HelperLimits {
        HelperLimits {
            base_pow_bits: 0,
            young_age: Duration::from_secs(0),
            disk: Some(DiskConfig { dir: dir.to_path_buf(), key: kt_engine::crypto::SymKey(key) }),
            ..Default::default()
        }
    }

    fn put(h: &HelperStore, seed: u8, topic: u8, v: u64, len: usize) -> PutReply {
        let k = SecretKey::from_bytes(&[seed; 32]);
        let t = [topic; 32];
        let blob = vec![seed; len];
        let sig = k.sign(&put_message(&t, v, &blob)).to_bytes().to_vec();
        h.put(t, *k.public().as_bytes(), v, blob, &sig, 0)
    }

    #[test]
    fn disk_copy_survives_restarts_and_is_managed() {
        let dir = std::env::temp_dir().join(format!("kt-hdisk-{}", rand::random::<u32>()));
        let h = HelperStore::new(disk_limits(&dir, [7; 32]));
        assert_eq!(put(&h, 1, 1, 1, 500), PutReply::Stored);
        assert_eq!(put(&h, 2, 2, 1, 500), PutReply::Stored);
        let start = h.info().session_start_ms;
        drop(h);
        // Restart: same data, same store start (a restart isn't data loss).
        let h = HelperStore::new(disk_limits(&dir, [7; 32]));
        assert_eq!(h.held().0, 2);
        assert_eq!(h.info().session_start_ms, start);
        // Files are encrypted: no plaintext blob bytes on disk.
        let mut raw = Vec::new();
        for sh in std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.path().is_dir()) {
            for f in std::fs::read_dir(sh.path()).unwrap().flatten() {
                raw.extend(std::fs::read(f.path()).unwrap());
            }
        }
        assert!(!raw.windows(64).any(|w| w.iter().all(|b| *b == 1)), "blob bytes not visible on disk");
        // A corrupt file is deleted on load.
        let victim = std::fs::read_dir(&dir).unwrap().flatten().find(|e| e.path().is_dir()).unwrap();
        let f = std::fs::read_dir(victim.path()).unwrap().flatten().next().unwrap().path();
        std::fs::write(&f, b"garbage").unwrap();
        drop(h);
        let h = HelperStore::new(disk_limits(&dir, [7; 32]));
        assert_eq!(h.held().0, 1);
        assert!(!f.exists(), "corrupt file removed");
        drop(h);
        // Another key can't read it: everything is dropped, nothing is trusted.
        let h = HelperStore::new(disk_limits(&dir, [8; 32]));
        assert_eq!(h.held().0, 0);
        drop(h);
        // TTL: expired slots are swept from RAM and disk.
        let mut l = disk_limits(&dir, [8; 32]);
        l.ttl_ms = 0;
        let h = HelperStore::new(l);
        let _ = put(&h, 3, 3, 1, 100);
        assert_eq!(h.sweep(), 1);
        assert_eq!(h.held(), (0, 0));
        // A smaller cap after a settings change trims on load.
        let h = HelperStore::new(disk_limits(&dir, [8; 32]));
        for i in 0..5u8 {
            assert_eq!(put(&h, 10 + i, 20 + i, 1, 1000), PutReply::Stored);
        }
        drop(h);
        let mut small = disk_limits(&dir, [8; 32]);
        small.capacity = 2500;
        let h = HelperStore::new(small);
        assert_eq!(h.held().0, 2, "trimmed to the new cap");
        drop(h);
        // Helping turned off: the store is gone.
        wipe_disk(&dir);
        assert!(!dir.exists());
    }

    fn signed(key: &SecretKey, topic: [u8; 32], v: u64, blob: &[u8]) -> Vec<u8> {
        key.sign(&put_message(&topic, v, blob)).to_bytes().to_vec()
    }

    #[test]
    fn store_rules() {
        let h = HelperStore::new(HelperLimits { capacity: 10_000, per_topic: 6_000, max_slot: 5_000, base_pow_bits: 4, young_age: Duration::from_secs(0), young_topic_cap: 0, ..Default::default() });
        let k = SecretKey::from_bytes(&[1; 32]);
        let w = *k.public().as_bytes();
        let t = [9; 32];
        let blob = vec![1u8; 1000];
        // New slot needs PoW.
        assert!(matches!(h.put(t, w, 1, blob.clone(), &signed(&k, t, 1, &blob), 0), PutReply::NeedPow(_) | PutReply::Stored));
        let n = solve_pow(&t, &w, 1, 12);
        assert_eq!(h.put(t, w, 1, blob.clone(), &signed(&k, t, 1, &blob), n), PutReply::Stored);
        // Refreshes are free; stale versions refused; forged signatures refused.
        assert_eq!(h.put(t, w, 2, blob.clone(), &signed(&k, t, 2, &blob), 0), PutReply::Stored);
        assert_eq!(h.put(t, w, 2, blob.clone(), &signed(&k, t, 2, &blob), 0), PutReply::Stale);
        let other = SecretKey::from_bytes(&[2; 32]);
        assert_eq!(h.put(t, w, 3, blob.clone(), &signed(&other, t, 3, &blob), 0), PutReply::BadSignature);
        assert_eq!(h.put(t, w, 3, vec![0; 6000], &signed(&k, t, 3, &[0; 6000]), 0), PutReply::TooLarge);
        assert_eq!(h.get(&[t], &[]).len(), 1);
        assert!(h.get(&[t], &[(t, w, 2)]).is_empty());
    }
}

/// Fuzzing (DESIGN §9): requests from strangers, answers from helpers, and the disk copy.
#[cfg(test)]
mod fuzz_tests {
    use super::*;

    fn signed(topic: [u8; 32], seed: [u8; 32], version: u64, blob: &[u8]) -> SignedPut {
        let key = writer_key(&seed);
        let sig = key.sign(&put_message(&topic, version, blob)).to_bytes().to_vec();
        SignedPut { topic, writer: *key.public().as_bytes(), version, blob: blob.to_vec(), sig, nonce: 0 }
    }

    fn href() -> HelperRef {
        HelperRef { endpoint: [3; 32], ips: vec!["192.0.2.1:4444".parse().unwrap()], relays: vec!["https://relay.example/".into()], relay: Some("https://192.0.2.1:4443".into()) }
    }

    #[test]
    fn requests_and_responses() {
        let store = HelperStore::new(HelperLimits::default());
        let put = signed([1; 32], [2; 32], 1, b"sealed slot");
        let seeds = vec![
            postcard::to_stdvec(&Request::Put { put: put.clone(), fanout: vec![href()] }).unwrap(),
            postcard::to_stdvec(&Request::Get { topics: vec![[1; 32]], have: vec![([1; 32], put.writer, 0)] }).unwrap(),
            postcard::to_stdvec(&Request::Hello(href())).unwrap(),
            postcard::to_stdvec(&Request::Info).unwrap(),
            postcard::to_stdvec(&Request::BlobPut { data: b"sealed".to_vec(), nonce: 7 }).unwrap(),
            postcard::to_stdvec(&Request::BlobGet { id: [3; 32] }).unwrap(),
            postcard::to_stdvec(&Response::Slots(vec![(([1; 32], put.writer), 1, b"x".to_vec())], vec![([1; 32], [2; 32])])).unwrap(),
            postcard::to_stdvec(&Response::Peers(vec![href()])).unwrap(),
            postcard::to_stdvec(&Response::Info(store.info())).unwrap(),
        ];
        kt_fuzz::run("helper-frames", &seeds, 2.0, |b| {
            if let Ok(req) = postcard::from_bytes::<Request>(b) {
                match req {
                    Request::Put { put, fanout } => {
                        let _ = store.put_signed(put, fanout);
                    }
                    Request::Get { topics, have } => {
                        let _ = store.get(&topics, &have);
                    }
                    Request::Hello(h) => store.learn_peer(h),
                    Request::Info | Request::Peers => {}
                    Request::BlobPut { data, nonce } => {
                        let _ = store.blobs.put(data, nonce);
                    }
                    Request::BlobGet { id } => {
                        let _ = store.blobs.get(&id);
                    }
                }
            }
            if let Ok(Response::Peers(ps)) = postcard::from_bytes::<Response>(b) {
                for p in ps {
                    let _ = p.to_addr();
                }
            }
        });
    }

    #[test]
    fn disk_copy() {
        let dir = std::env::temp_dir().join(format!("kt-fuzz-disk-{}-{}", std::process::id(), rand::random::<u32>()));
        let key = kt_engine::crypto::SymKey([9; 32]);
        let limits = || HelperLimits { disk: Some(DiskConfig { dir: dir.clone(), key: key.clone() }), ..HelperLimits::default() };
        let put = signed([1; 32], [2; 32], 1, b"sealed slot");
        let entry = Entry { put: put.clone(), refreshed: now_ms(), set: vec![href()], topic_first_seen: now_ms() };
        let plain = postcard::to_stdvec(&entry).unwrap();
        let file = entry_file(&dir, &(put.topic, put.writer));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        kt_fuzz::run("helper-disk", &[plain], 0.2, |p| {
            let _ = std::fs::write(&file, kt_engine::crypto::seal(&key, DISK_AAD, p));
            let _ = HelperStore::new(limits());
        });
    }
}
