//! Self-sustaining Nostr relay list (DESIGN §7): nothing here depends on the developer.
//!
//! - [`RelayDirectory`]: the shipped list is only a seed. Candidates come from Nostr itself:
//!   NIP-66 relay-monitor events (kind 30166; measured 2026-09-24: ~240 relays from 21–25
//!   monitors in 3 days) and NIP-65 relay lists (kind 10002, popularity). Candidates that
//!   need auth or payment, or aren't on the clear web, are skipped. A relay is **proven**
//!   only by read-back (it returned our own slots).
//! - [`maintain`]: every group gets a recorded relay set (engine `mailbox_set`, layer
//!   `"nostr"`); when a set degrades for this device, it proposes a repaired one (at most
//!   daily per group). The mailbox then writes and reads each group's set; the previous set
//!   is still read for a while. All automatic and invisible to users.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kt_engine::GroupId;
use nostr::prelude::*;
use serde::{Deserialize, Serialize};

use crate::nostr::NostrMailbox;
use crate::sync::Syncer;

pub const LAYER: &str = "nostr";

/// The seed list shipped with the app: only a starting point (the list is self-sustaining).
/// The 8 relays that passed the E4 acceptance probe.
pub const SEED_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://nostr.mom",
    "wss://offchain.pub",
    "wss://nostr.oxtr.dev",
    "wss://nostr-pub.wellorder.net",
    "wss://relay.snort.social",
];
/// Relays per group set: writes go to the best 3, reads cover all.
pub const SET_SIZE: usize = 4;
/// After a set change, keep writing to the previous relays this long.
pub const WRITE_PREVIOUS_MS: u64 = 3 * 86_400_000;
const MAX_CANDIDATES: usize = 300;
const DAY_MS: u64 = 86_400_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Candidate {
    pub seed: bool,
    /// Seen in a relay-monitor event (last time, ms).
    pub monitored_ms: u64,
    /// How many relay lists mention it.
    pub popularity: u32,
    pub rtt_ms: Option<u32>,
}

#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    candidates: BTreeMap<String, Candidate>,
    last_discovery_ms: u64,
    /// Our last set repair per group (rate limit: at most daily).
    last_proposal: BTreeMap<[u8; 32], u64>,
}

pub struct RelayDirectory {
    inner: Mutex<Persisted>,
    path: Option<PathBuf>,
    /// Tests only: accept ws://127.0.0.1 relays.
    allow_local: bool,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Canonical relay URL, or None if it isn't a usable clear-web websocket relay.
pub fn normalize(url: &str) -> Option<String> {
    let u = url.trim().trim_end_matches('/').to_ascii_lowercase();
    let host = u.strip_prefix("wss://")?;
    if host.is_empty() || host.contains(".onion") || host.contains(".i2p") || host.contains(' ') || host.starts_with("localhost") || host.starts_with("127.") {
        return None;
    }
    (u.len() <= 200).then_some(u)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoveryReport {
    pub monitor_events: usize,
    pub list_events: usize,
    pub new_candidates: usize,
}

impl RelayDirectory {
    pub fn new(seeds: &[&str], path: Option<PathBuf>) -> RelayDirectory {
        let mut p: Persisted = path.as_ref().and_then(|x| std::fs::read(x).ok()).and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default();
        for s in seeds {
            if let Some(u) = normalize(s) {
                p.candidates.entry(u).or_default().seed = true;
            }
        }
        RelayDirectory { inner: Mutex::new(p), path, allow_local: false }
    }

    /// Tests: accept local plain-websocket relays (never in the app).
    pub fn for_tests(seeds: &[&str]) -> RelayDirectory {
        let mut d = RelayDirectory { inner: Mutex::new(Persisted::default()), path: None, allow_local: true };
        for s in seeds {
            if let Some(u) = d.norm(s) {
                d.inner.get_mut().expect("lock").candidates.entry(u).or_default().seed = true;
            }
        }
        d
    }

    fn norm(&self, url: &str) -> Option<String> {
        if self.allow_local {
            let u = url.trim().trim_end_matches('/').to_ascii_lowercase();
            if u.starts_with("ws://127.0.0.1:") {
                return Some(u);
            }
        }
        normalize(url)
    }

    fn save(&self) {
        if let Some(p) = &self.path {
            if let Ok(b) = postcard::to_stdvec(&*self.inner.lock().expect("lock")) {
                let _ = kt_engine::storage::write_atomic(p, &b);
            }
        }
    }

    pub fn candidates(&self) -> BTreeMap<String, Candidate> {
        self.inner.lock().expect("lock").candidates.clone()
    }

    pub fn discovery_due(&self) -> bool {
        now_ms().saturating_sub(self.inner.lock().expect("lock").last_discovery_ms) > DAY_MS
    }

    /// Learn candidates from monitor events and relay lists, read from relays we use.
    pub async fn discover(&self, mb: &NostrMailbox) -> DiscoveryReport {
        let mut report = DiscoveryReport::default();
        let sources: Vec<String> = self.ordered(mb).into_iter().take(3).collect();
        let since = Timestamp::from_secs(Timestamp::now().as_secs().saturating_sub(3 * 86_400));
        let mut found: BTreeMap<String, Candidate> = BTreeMap::new();
        for src in sources {
            let conn = mb.conn(&src);
            if let Some(evs) = conn.query(vec![Filter::new().kind(Kind::Custom(30166)).since(since).limit(500)]).await {
                for e in evs.iter().filter(|e| e.verify().is_ok()) {
                    report.monitor_events += 1;
                    let Some(url) = e.tags.identifier().and_then(|d| self.norm(&d)) else { continue };
                    let mut network_ok = true;
                    let mut restricted = false;
                    let mut rtt = None;
                    for t in e.tags.iter() {
                        let (k, v) = (t.kind(), t.content().unwrap_or(""));
                        match k {
                            "n" if v != "clearnet" => network_ok = false,
                            "R" if ["auth", "payment", "writes", "pow"].contains(&v) => restricted = true,
                            "rtt-open" => rtt = v.parse::<u32>().ok(),
                            _ => {}
                        }
                    }
                    if network_ok && !restricted {
                        let c = found.entry(url).or_default();
                        c.monitored_ms = e.created_at.as_secs() * 1000;
                        c.rtt_ms = rtt.or(c.rtt_ms);
                    }
                }
            }
            if let Some(evs) = conn.query(vec![Filter::new().kind(Kind::Custom(10002)).since(since).limit(300)]).await {
                for e in evs.iter().filter(|e| e.verify().is_ok()) {
                    report.list_events += 1;
                    for t in e.tags.iter().filter(|t| t.kind() == "r") {
                        if let Some(url) = t.content().and_then(|c| self.norm(c)) {
                            found.entry(url).or_default().popularity += 1;
                        }
                    }
                }
            }
        }
        let mut inner = self.inner.lock().expect("lock");
        for (url, c) in found {
            // Only relays a monitor vouches for become candidates; lists only add weight.
            if c.monitored_ms == 0 && !inner.candidates.contains_key(&url) {
                continue;
            }
            let e = inner.candidates.entry(url).or_insert_with(|| {
                report.new_candidates += 1;
                Candidate::default()
            });
            e.monitored_ms = e.monitored_ms.max(c.monitored_ms);
            e.popularity = e.popularity.max(c.popularity);
            e.rtt_ms = c.rtt_ms.or(e.rtt_ms);
        }
        // Keep the list bounded: seeds, then the most popular and recently monitored.
        if inner.candidates.len() > MAX_CANDIDATES {
            let mut v: Vec<(String, Candidate)> = std::mem::take(&mut inner.candidates).into_iter().collect();
            v.sort_by_key(|(_, c)| (std::cmp::Reverse(c.seed), std::cmp::Reverse(c.popularity), std::cmp::Reverse(c.monitored_ms)));
            v.truncate(MAX_CANDIDATES);
            inner.candidates = v.into_iter().collect();
        }
        inner.last_discovery_ms = now_ms();
        drop(inner);
        self.save();
        report
    }

    /// All candidates in order of preference for this device: proven by read-back, then
    /// seeds, then untried candidates (popular, fast first). Failing relays go last.
    pub fn ordered(&self, mb: &NostrMailbox) -> Vec<String> {
        let health: BTreeMap<String, crate::nostr::RelayHealth> = mb.health_merged().into_iter().collect();
        let cands = self.candidates();
        let mut v: Vec<(u8, i64, String)> = cands
            .iter()
            .map(|(u, c)| {
                let h = health.get(u).cloned().unwrap_or_default();
                let failing = h.readback_misses > h.readback_hits + 2 || h.failures_in_a_row >= 3;
                let class = if failing {
                    3
                } else if h.readback_hits > 0 {
                    0
                } else if c.seed {
                    1
                } else {
                    2
                };
                let score = h.readback_hits as i64 * 100 + c.popularity as i64 * 10 - c.rtt_ms.unwrap_or(1000) as i64 / 100;
                (class, -score, u.clone())
            })
            .collect();
        v.sort();
        v.into_iter().map(|x| x.2).collect()
    }

    fn is_failing(mb: &NostrMailbox, url: &str) -> bool {
        mb.health_merged().into_iter().find(|(u, _)| u == url).is_some_and(|(_, h)| {
            h.readback_misses > h.readback_hits + 2 || h.failures_in_a_row >= 3
        })
    }

    /// A set for a group: best proven relays and seeds; untried candidates fill the rest,
    /// at most one if there are enough proven ones (a trial share).
    pub fn choose_set(&self, mb: &NostrMailbox, keep: &[String]) -> Vec<String> {
        let cands = self.candidates();
        let health: BTreeMap<String, crate::nostr::RelayHealth> = mb.health_merged().into_iter().collect();
        let mut out: Vec<String> = keep.to_vec();
        let trusted = self
            .ordered(mb)
            .iter()
            .filter(|u| !Self::is_failing(mb, u) && (health.get(*u).is_some_and(|h| h.readback_hits > 0) || cands.get(*u).is_some_and(|c| c.seed)))
            .count();
        let max_trials = SET_SIZE.saturating_sub(trusted + out.len()).max(1);
        let mut trials = 0;
        for u in self.ordered(mb) {
            if out.len() >= SET_SIZE {
                break;
            }
            if out.contains(&u) || Self::is_failing(mb, &u) {
                continue;
            }
            let proven = health.get(&u).is_some_and(|h| h.readback_hits > 0);
            let seed = cands.get(&u).is_some_and(|c| c.seed);
            if !proven && !seed {
                if trials >= max_trials {
                    continue;
                }
                trials += 1;
            }
            out.push(u);
        }
        out
    }

    fn may_propose(&self, g: &GroupId) -> bool {
        let inner = self.inner.lock().expect("lock");
        inner.last_proposal.get(&g.0).is_none_or(|t| now_ms().saturating_sub(*t) > DAY_MS)
    }

    fn proposed(&self, g: &GroupId) {
        self.inner.lock().expect("lock").last_proposal.insert(g.0, now_ms());
        self.save();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeeperReport {
    pub sets_created: usize,
    pub sets_repaired: usize,
}

/// Point the mailbox at each group's recorded relay set (call once at start).
pub fn install_resolver(syncer: &Arc<Syncer>, mb: &NostrMailbox) {
    let replica = syncer.replica();
    mb.set_resolver(Arc::new(move |topic: &[u8; 32]| {
        let r = replica.lock().ok()?;
        let g = r.read_topics().into_iter().find(|t| t.topic == *topic)?.group;
        let (cur, prev) = r.mailbox_set(&g, LAYER);
        let Some(cur) = cur else {
            // Joining: write and read where the invite record says members look (and our pool).
            let hints = r.endpoint_hints(&g, LAYER);
            return (!hints.is_empty()).then(|| crate::nostr::TopicRelays {
                current: hints,
                previous: vec![],
                previous_is_pool: true,
                read_previous: true,
                write_previous: true,
            });
        };
        let age = now_ms().saturating_sub(cur.created_ms);
        Some(crate::nostr::TopicRelays {
            previous_is_pool: prev.is_none(),
            previous: prev.map(|p| p.endpoints).unwrap_or_default(),
            current: cur.endpoints,
            read_previous: age < kt_engine::replica::PREVIOUS_SET_READ_MS,
            write_previous: age < WRITE_PREVIOUS_MS,
        })
    }));
}

/// Keep every group's relay set healthy, and this device's own pool ordered.
pub async fn maintain(syncer: &Arc<Syncer>, mb: &NostrMailbox, dir: &RelayDirectory) -> KeeperReport {
    let mut report = KeeperReport::default();
    if dir.discovery_due() {
        dir.discover(mb).await;
    }
    mb.set_pool(dir.ordered(mb).into_iter().take(8).collect());
    let replica = syncer.replica();
    let groups: Vec<(GroupId, Option<Vec<String>>)> = {
        let r = replica.lock().expect("replica lock");
        let me = r.device_id();
        r.groups()
            .into_iter()
            .filter(|g| r.epochs(g).is_some_and(|e| e.is_current_member(&me)))
            .map(|g| (g, r.mailbox_set(&g, LAYER).0.map(|s| s.endpoints)))
            .collect()
    };
    for (g, cur) in groups {
        match cur {
            None => {
                // Creating a first set doesn't count toward the daily repair limit.
                let set = dir.choose_set(mb, &[]);
                if !set.is_empty() && replica.lock().expect("replica lock").propose_mailbox_set(&g, LAYER, set).is_ok() {
                    report.sets_created += 1;
                }
            }
            Some(cur) => {
                let failing: Vec<&String> = cur.iter().filter(|u| RelayDirectory::is_failing(mb, u)).collect();
                if failing.len() * 2 >= cur.len() && dir.may_propose(&g) {
                    let keep: Vec<String> = cur.iter().filter(|u| !failing.contains(u)).cloned().collect();
                    let set = dir.choose_set(mb, &keep);
                    if set != cur && replica.lock().expect("replica lock").propose_mailbox_set(&g, LAYER, set).is_ok() {
                        dir.proposed(&g);
                        report.sets_repaired += 1;
                    }
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_normalized_and_filtered() {
        assert_eq!(normalize("wss://Relay.Example.com/").as_deref(), Some("wss://relay.example.com"));
        assert!(normalize("ws://plain.example").is_none());
        assert!(normalize("wss://abc.onion").is_none());
        assert!(normalize("wss://localhost:7447").is_none());
    }
}
