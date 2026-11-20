//! Self-sustaining helper list (DESIGN §6/§7a): the same pattern as relays.
//!
//! - **Announce:** a device that helps publishes a signed announcement (its address) on
//!   Nostr under a public, daily-rotating rendezvous tag. Its announcement key is derived
//!   from its endpoint key, so each helper has one announcement that it keeps replacing.
//! - **Discover:** devices read today's and yesterday's announcements into a candidate list.
//!   Anyone can announce; only helpers that prove themselves in use (reliability records)
//!   get preferred, and untried candidates are picked by closeness to a secret, daily
//!   point per group, so outsiders can't pre-position fake helpers for a group.
//! - **Per-group helper set** (engine `mailbox_set`, layer `"helpers"`), created and
//!   repaired automatically, invisibly to users.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use kt_engine::GroupId;
use nostr::prelude::*;
use serde::{Deserialize, Serialize};

use crate::helper::{HelperMailbox, HelperRef};
use crate::nostr::NostrMailbox;
use crate::sync::Syncer;

pub const LAYER: &str = "helpers";
pub const ANNOUNCE_KIND: u16 = 35714;
/// Helpers per group set (the first few get writes directly, the rest by replication).
pub const HELPER_SET: usize = 8;
const MAX_CANDIDATES: usize = 300;
const DAY_MS: u64 = 86_400_000;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn today() -> u32 {
    (now_ms() / DAY_MS) as u32
}

/// The public rendezvous topic helpers announce under on a given day.
pub fn rendezvous(day: u32) -> [u8; 32] {
    *blake3::Hasher::new_derive_key("kt/helper-rendezvous/v1").update(&day.to_le_bytes()).finalize().as_bytes()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announce {
    pub helper: HelperRef,
    pub capacity: u64,
    pub session_start_ms: u64,
}

/// Publish our helper announcement (only when helping).
pub async fn announce(hmb: &HelperMailbox, mb: &NostrMailbox) -> usize {
    let p2p = hmb.p2p();
    let Some(store) = p2p.helper() else { return 0 };
    let info = store.info();
    let a = Announce {
        helper: p2p.helper_ref().expect("own address"),
        capacity: info.capacity,
        session_start_ms: info.session_start_ms,
    };
    let seed = kt_engine::crypto::derive(&p2p.endpoint().secret_key().to_bytes(), "kt/helper-announce/v1", b"").0;
    let Ok(sk) = SecretKey::from_slice(&seed) else { return 0 };
    let keys = Keys::new(sk);
    let topic = rendezvous(today());
    let content = base64::engine::general_purpose::STANDARD.encode(postcard::to_stdvec(&a).unwrap_or_default());
    let Ok(ev) = EventBuilder::new(Kind::Custom(ANNOUNCE_KIND), content)
        .tags([Tag::identifier("kt-helper"), Tag::hashtag(crate::nostr::topic_tag(&topic))])
        .finalize(&keys)
    else {
        return 0;
    };
    mb.publish_on(&topic, ev).await
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelperCandidate {
    pub helper: HelperRef,
    pub seen_ms: u64,
    pub capacity: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    candidates: BTreeMap<[u8; 32], HelperCandidate>,
    last_discovery_ms: u64,
    last_repair: BTreeMap<[u8; 32], u64>,
}

pub struct HelperDirectory {
    inner: Mutex<Persisted>,
    path: Option<PathBuf>,
}

impl HelperDirectory {
    pub fn new(path: Option<PathBuf>) -> HelperDirectory {
        let p = path.as_ref().and_then(|x| std::fs::read(x).ok()).and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default();
        HelperDirectory { inner: Mutex::new(p), path }
    }

    pub fn save(&self) {
        if let Some(p) = &self.path {
            if let Ok(b) = postcard::to_stdvec(&*self.inner.lock().expect("lock")) {
                let _ = kt_engine::storage::write_atomic(p, &b);
            }
        }
    }

    pub fn candidates(&self) -> Vec<HelperCandidate> {
        self.inner.lock().expect("lock").candidates.values().cloned().collect()
    }

    /// Add a helper learned elsewhere (a group's set, a manifest's select node, ...).
    pub fn add(&self, h: HelperRef) {
        let mut i = self.inner.lock().expect("lock");
        i.candidates.entry(h.endpoint).or_insert(HelperCandidate { helper: h, seen_ms: now_ms(), capacity: 0 });
    }

    pub fn discovery_due(&self) -> bool {
        now_ms().saturating_sub(self.inner.lock().expect("lock").last_discovery_ms) > DAY_MS / 4
    }

    /// Read today's and yesterday's announcements.
    pub async fn discover(&self, mb: &NostrMailbox) -> usize {
        let mut added = 0;
        for day in [today(), today().saturating_sub(1)] {
            let topic = rendezvous(day);
            let filter = Filter::new().kind(Kind::Custom(ANNOUNCE_KIND)).hashtag(crate::nostr::topic_tag(&topic)).limit(500);
            for ev in mb.query_on(&topic, filter).await {
                let Ok(b) = base64::engine::general_purpose::STANDARD.decode(&ev.content) else { continue };
                let Ok(a) = postcard::from_bytes::<Announce>(&b) else { continue };
                if a.helper.to_addr().is_none() || a.helper.ips.len() > 16 || a.helper.relays.len() > 8 {
                    continue;
                }
                let mut i = self.inner.lock().expect("lock");
                let e = i.candidates.entry(a.helper.endpoint).or_insert_with(|| {
                    added += 1;
                    HelperCandidate { helper: a.helper.clone(), seen_ms: 0, capacity: 0 }
                });
                e.helper = a.helper;
                e.seen_ms = ev.created_at.as_secs() * 1000;
                e.capacity = a.capacity;
            }
        }
        let mut i = self.inner.lock().expect("lock");
        if i.candidates.len() > MAX_CANDIDATES {
            let mut v: Vec<([u8; 32], HelperCandidate)> = std::mem::take(&mut i.candidates).into_iter().collect();
            v.sort_by_key(|(_, c)| std::cmp::Reverse(c.seen_ms));
            v.truncate(MAX_CANDIDATES);
            i.candidates = v.into_iter().collect();
        }
        i.last_discovery_ms = now_ms();
        drop(i);
        self.save();
        added
    }

    /// Candidates best first for this device: proven by our records, then untried ones
    /// ordered by closeness to `point` (a group's secret daily point, if given).
    pub fn ordered(&self, hmb: &HelperMailbox, point: Option<[u8; 32]>) -> Vec<HelperRef> {
        let rec = hmb.records();
        let own = *hmb.p2p().endpoint().id().as_bytes();
        let mut v: Vec<(u8, i64, [u8; 32], HelperRef)> = self
            .candidates()
            .into_iter()
            .filter(|c| c.helper.endpoint != own)
            .map(|c| {
                let id = iroh::EndpointId::from_bytes(&c.helper.endpoint).ok();
                let r = id.and_then(|i| rec.get(&i).cloned());
                let failing = id.is_some_and(|i| hmb.is_failing(&i));
                let class = if failing {
                    3
                } else if r.as_ref().is_some_and(|r| r.reachable > 0) {
                    0
                } else {
                    1
                };
                let score = r.map(|r| (r.score() * 1000.0) as i64).unwrap_or(0);
                let mut dist = [0u8; 32];
                if let Some(p) = point {
                    for k in 0..32 {
                        dist[k] = p[k] ^ c.helper.endpoint[k];
                    }
                }
                (class, -score, dist, c.helper)
            })
            .collect();
        v.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
        v.into_iter().map(|x| x.3).collect()
    }

    fn may_repair(&self, g: &GroupId) -> bool {
        self.inner.lock().expect("lock").last_repair.get(&g.0).is_none_or(|t| now_ms().saturating_sub(*t) > DAY_MS)
    }

    fn repaired(&self, g: &GroupId) {
        self.inner.lock().expect("lock").last_repair.insert(g.0, now_ms());
        self.save();
    }
}

fn encode_ref(h: &HelperRef) -> String {
    hex::encode(postcard::to_stdvec(h).unwrap_or_default())
}

pub fn decode_ref(s: &str) -> Option<HelperRef> {
    postcard::from_bytes(&hex::decode(s).ok()?).ok()
}

/// Point the helper mailbox at each group's helper set.
/// How many helpers' relays a device keeps in its relay map.
pub const HELPER_RELAYS: usize = 3;

pub fn install_resolver(syncer: &Arc<Syncer>, hmb: &HelperMailbox) {
    let replica = syncer.replica();
    hmb.set_resolver(Arc::new(move |topic: &[u8; 32]| {
        let r = replica.lock().ok()?;
        let g = r.read_topics().into_iter().find(|t| t.topic == *topic)?.group;
        let (cur, prev) = r.mailbox_set(&g, LAYER);
        let endpoints = match cur {
            Some(c) => c.endpoints,
            None => r.endpoint_hints(&g, LAYER), // joining: the invite record's helpers
        };
        if endpoints.is_empty() {
            return None;
        }
        let mut v: Vec<iroh::EndpointAddr> = endpoints.iter().filter_map(|e| decode_ref(e)?.to_addr()).collect();
        for p in prev.map(|p| p.endpoints).unwrap_or_default() {
            if let Some(a) = decode_ref(&p).and_then(|h| h.to_addr()) {
                if !v.iter().any(|x| x.id == a.id) {
                    v.push(a);
                }
            }
        }
        Some(v)
    }));
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HelperKeeperReport {
    pub announced: usize,
    pub discovered: usize,
    pub sets_created: usize,
    pub sets_repaired: usize,
    /// Helper relays this device uses after this round.
    pub relays_in_use: usize,
}

/// Learn helpers without Nostr: every helper in any of our groups' sets (groups overlap,
/// so helpers spread through the social graph), plus peer exchange: ask a few known
/// helpers which helpers they know. If we help, introduce ourselves to a few of them.
pub async fn exchange_peers(syncer: &Arc<Syncer>, hmb: &HelperMailbox, dir: &HelperDirectory) -> usize {
    let before = dir.candidates().len();
    {
        let r = syncer.replica();
        let r = r.lock().expect("replica lock");
        for g in r.groups() {
            let (cur, prev) = r.mailbox_set(&g, LAYER);
            for e in cur.into_iter().chain(prev).flat_map(|s| s.endpoints) {
                if let Some(h) = decode_ref(&e) {
                    dir.add(h);
                }
            }
        }
    }
    let asked: Vec<HelperRef> = dir.ordered(hmb, None).into_iter().take(4).collect();
    let me = hmb.p2p().helper().map(|_| HelperRef::from_addr(&hmb.p2p().where_data().to_addr().expect("own address")));
    for h in asked {
        let Some(addr) = h.to_addr() else { continue };
        for p in hmb.ask_peers(&addr).await {
            if p.to_addr().is_some() {
                dir.add(p);
            }
        }
        if let (Some(me), Some(store)) = (&me, hmb.p2p().helper()) {
            if hmb.introduce(&addr, me.clone()).await {
                store.learn_peer(h.clone());
            }
        }
    }
    // A helper also offers the helpers it has heard of.
    if let Some(store) = hmb.p2p().helper() {
        for p in store.known_peers() {
            dir.add(p);
        }
    }
    dir.save();
    dir.candidates().len().saturating_sub(before)
}

/// Keep announcements, candidates and every group's helper set up to date. Nostr is one
/// source among several: group data, peer exchange and the DHT keep it going without it.
pub async fn maintain(syncer: &Arc<Syncer>, hmb: &HelperMailbox, mb: &NostrMailbox, dir: &HelperDirectory) -> HelperKeeperReport {
    maintain_with_dht(syncer, hmb, mb, dir, None).await
}

/// [`maintain`] plus the DHT helper slots (announce when helping; read when this device
/// knows few working helpers).
pub async fn maintain_with_dht(
    syncer: &Arc<Syncer>,
    hmb: &HelperMailbox,
    mb: &NostrMailbox,
    dir: &HelperDirectory,
    dht: Option<&crate::dht::DhtDiscovery>,
) -> HelperKeeperReport {
    let mut report = HelperKeeperReport { announced: announce(hmb, mb).await, ..Default::default() };
    if let Some(store) = hmb.p2p().helper() {
        store.sweep(); // disk doesn't clear itself: drop slots nobody refreshed
    }
    let due = dir.discovery_due();
    if due {
        report.discovered = dir.discover(mb).await;
    }
    if let Some(dht) = dht {
        let p2p = hmb.p2p();
        if p2p.helper().is_some() && due {
            let me = p2p.helper_ref().expect("own address");
            if dht.announce_helper(&me, p2p.endpoint().secret_key(), today()).await {
                report.announced += 1;
            }
        }
        let working = dir.ordered(hmb, None).iter().filter(|h| iroh::EndpointId::from_bytes(&h.endpoint).is_ok_and(|i| !hmb.is_failing(&i))).count();
        if due || working < 4 {
            for h in dht.find_helpers(today()).await {
                if h.endpoint != *p2p.endpoint().id().as_bytes() {
                    dir.add(h);
                    report.discovered += 1;
                }
            }
        }
    }
    report.discovered += exchange_peers(syncer, hmb, dir).await;
    hmb.set_pool(dir.ordered(hmb, None).into_iter().filter_map(|h| h.to_addr()).take(HELPER_SET).collect());
    // Relays run by known helpers (D51): the endpoint can pick one as its home relay, or
    // reach devices that use them. Several, so one leaving doesn't matter. Not filtered by
    // storage health: a device that isn't online yet can't reach any helper's storage, and
    // may need exactly these relays to get online (iroh tracks relay health itself).
    let me = *hmb.p2p().endpoint().id().as_bytes();
    let relays: Vec<(String, [u8; 32])> = dir
        .ordered(hmb, None)
        .into_iter()
        .filter(|h| h.endpoint != me)
        .filter_map(|h| h.relay.clone().map(|u| (u, h.endpoint)))
        .take(HELPER_RELAYS * 3) // spares, for relays that turn out unreachable
        .collect();
    report.relays_in_use = hmb.p2p().sync_helper_relays(relays, HELPER_RELAYS).await;
    let replica = syncer.replica();
    let day = today();
    // Groups we're joining: besides the invite's hints, the helpers closest to the group's
    // secret daily point (the same rule members use for their set, §7a), so a join still
    // finds the group's helpers when the hinted ones are gone.
    let joining: Vec<(GroupId, Option<[u8; 32]>)> = {
        let r = replica.lock().expect("replica lock");
        r.pending_joins().into_iter().map(|p| (p.group, r.group_point(&p.group, day))).collect()
    };
    for (g, point) in joining {
        if let Some(point) = point {
            let near: Vec<String> = dir.ordered(hmb, Some(point)).iter().take(HELPER_SET).map(encode_ref).collect();
            let _ = replica.lock().expect("replica lock").add_join_hints(&g, LAYER, near);
        }
    }
    let groups: Vec<(GroupId, Option<Vec<String>>, Option<[u8; 32]>)> = {
        let r = replica.lock().expect("replica lock");
        let me = r.device_id();
        r.groups()
            .into_iter()
            .filter(|g| r.epochs(g).is_some_and(|e| e.is_current_member(&me)))
            .map(|g| (g, r.mailbox_set(&g, LAYER).0.map(|s| s.endpoints), r.group_point(&g, day)))
            .collect()
    };
    for (g, cur, point) in groups {
        let ordered = dir.ordered(hmb, point);
        match cur {
            None => {
                let set: Vec<String> = ordered.iter().take(HELPER_SET).map(encode_ref).collect();
                if !set.is_empty() && replica.lock().expect("replica lock").propose_mailbox_set(&g, LAYER, set).is_ok() {
                    report.sets_created += 1;
                }
            }
            Some(cur) => {
                let refs: Vec<HelperRef> = cur.iter().filter_map(|e| decode_ref(e)).collect();
                let failing: Vec<[u8; 32]> = refs
                    .iter()
                    .filter(|h| iroh::EndpointId::from_bytes(&h.endpoint).is_ok_and(|i| hmb.is_failing(&i)))
                    .map(|h| h.endpoint)
                    .collect();
                let rec = hmb.records();
                let proven = |h: &HelperRef| {
                    iroh::EndpointId::from_bytes(&h.endpoint).is_ok_and(|i| rec.get(&i).is_some_and(|r| r.reachable > 0) && !hmb.is_failing(&i))
                };
                let degraded = failing.len() * 2 >= refs.len().max(1);
                // Topping up a short set only adds helpers we've seen work ourselves: never
                // re-add helpers another member just dropped on its own evidence.
                let short = refs.len() < HELPER_SET && ordered.iter().any(|h| proven(h) && !refs.iter().any(|r| r.endpoint == h.endpoint));
                if (degraded || short) && dir.may_repair(&g) {
                    let mut keep: Vec<HelperRef> = refs.into_iter().filter(|h| !failing.contains(&h.endpoint)).collect();
                    for h in ordered {
                        if keep.len() >= HELPER_SET {
                            break;
                        }
                        let usable = if degraded { !failing.contains(&h.endpoint) } else { proven(&h) };
                        if usable && !keep.iter().any(|k| k.endpoint == h.endpoint) {
                            keep.push(h);
                        }
                    }
                    let set: Vec<String> = keep.iter().map(encode_ref).collect();
                    if set != cur && replica.lock().expect("replica lock").propose_mailbox_set(&g, LAYER, set).is_ok() {
                        dir.repaired(&g);
                        report.sets_repaired += 1;
                    }
                }
            }
        }
    }
    report
}
