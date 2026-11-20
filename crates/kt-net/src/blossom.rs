//! Blossom (BUD-01/02) as a best-effort blob layer (DESIGN §4, D50): public HTTP servers
//! that keep blobs by SHA-256, the same ID ours have.
//!
//! - Only servers that accept `application/octet-stream` from any key are used; media-only
//!   servers are never fed ciphertext under a false type (probe 2026-09-25:
//!   `tests/blossom_probe.rs`).
//! - Every upload is authorized by a fresh random key, so uploads can't be linked to each
//!   other or to this device's other keys.
//! - Before re-offering, a HEAD tells whether the server still has it.
//! - A server that fails is skipped for a while; nothing here is ever required.
//! - **Self-sustaining list** ([`BlossomDirectory`]), like the relay list: the manifest's
//!   servers are a seed; devices also read the server lists Nostr users publish (BUD-03,
//!   kind 10063) and try the most listed ones exactly as they'd use them (a sealed blob
//!   as octet-stream, read back, deleted). Only servers that pass are used; a few
//!   candidates a week, rejected ones left alone for 30 days.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use kt_engine::blobs::{blob_id, BlobId};
use nostr::prelude::*;

/// Seed servers (the probe's result). Best effort: the app works without any of them.
pub const SEED_SERVERS: [&str; 2] = ["https://nostr.download", "https://cdn.hzrd149.com"];
/// Largest blob offered (what the probe confirmed works).
pub const MAX_BLOB: usize = 3 * 1024 * 1024;
/// Servers each blob goes to.
pub const COPIES: usize = 2;
const SKIP_AFTER_FAILURE: Duration = Duration::from_secs(3600);

pub struct Blossom {
    servers: Mutex<Vec<String>>,
    client: reqwest::Client,
    down: Mutex<HashMap<String, Instant>>,
}

impl Blossom {
    pub fn new(servers: Vec<String>) -> Option<Blossom> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = crate::http::client(Duration::from_secs(60))?;
        Some(Blossom { servers: Mutex::new(servers), client, down: Mutex::new(HashMap::new()) })
    }

    pub fn seeded() -> Option<Blossom> {
        Blossom::new(SEED_SERVERS.iter().map(|s| s.to_string()).collect())
    }

    /// Replace the server list (best first): the manifest's and discovered ones.
    pub fn set_servers(&self, servers: Vec<String>) {
        *self.servers.lock().expect("lock") = servers;
    }

    pub fn servers(&self) -> Vec<String> {
        self.servers.lock().expect("lock").clone()
    }

    fn up(&self) -> Vec<String> {
        let d = self.down.lock().expect("lock");
        self.servers.lock().expect("lock").iter().filter(|s| d.get(*s).is_none_or(|t| t.elapsed() > SKIP_AFTER_FAILURE)).cloned().collect()
    }

    fn failed(&self, server: &str) {
        self.down.lock().expect("lock").insert(server.to_string(), Instant::now());
    }

    fn auth(verb: &str, sha: &str) -> Option<String> {
        Self::auth_with(&Keys::generate(), verb, sha) // unlinkable: a new key per request
    }

    fn auth_with(keys: &Keys, verb: &str, sha: &str) -> Option<String> {
        let ev = EventBuilder::new(Kind::Custom(24242), format!("{verb} blob"))
            .tags([Tag::parse(["t", verb]).ok()?, Tag::parse(["x", sha]).ok()?, Tag::expiration(Timestamp::now() + 300)])
            .finalize(keys)
            .ok()?;
        Some(format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(ev.as_json())))
    }

    /// Make sure up to [`COPIES`] servers hold the blob. Returns how many do.
    pub async fn offer(&self, data: &[u8]) -> usize {
        if data.len() > MAX_BLOB {
            return 0;
        }
        let sha = hex::encode(blob_id(data));
        let mut ok = 0;
        for s in self.up() {
            if ok >= COPIES {
                break;
            }
            // Already there: nothing to send.
            if matches!(self.client.head(format!("{s}/{sha}")).send().await, Ok(r) if r.status().is_success()) {
                ok += 1;
                continue;
            }
            let Some(auth) = Self::auth("upload", &sha) else { continue };
            match self.client.put(format!("{s}/upload")).header("Authorization", auth).header("Content-Type", "application/octet-stream").body(data.to_vec()).send().await {
                Ok(r) if r.status().is_success() => ok += 1,
                _ => self.failed(&s),
            }
        }
        ok
    }

    /// Fetch a blob; only the exact bytes are accepted.
    pub async fn get(&self, id: &BlobId) -> Option<Vec<u8>> {
        let sha = hex::encode(id);
        for s in self.up() {
            match self.client.get(format!("{s}/{sha}")).send().await {
                Ok(r) if r.status().is_success() => {
                    if let Ok(b) = r.bytes().await {
                        if b.len() <= MAX_BLOB + 1024 && blob_id(&b) == *id {
                            return Some(b.to_vec());
                        }
                    }
                    self.failed(&s); // served something else
                }
                Ok(_) => {} // not there: not a failure
                Err(_) => self.failed(&s),
            }
        }
        None
    }
}

/// Blossom's user server list (BUD-03).
pub const SERVER_LIST_KIND: u16 = 10063;
/// How often devices look for new servers.
pub const DISCOVER_EVERY_MS: u64 = 7 * 86_400_000;
/// New candidates tried per discovery.
pub const TRY_PER_ROUND: usize = 3;
/// A server that failed the trial is left alone this long.
pub const REJECT_FOR_MS: u64 = 30 * 86_400_000;
/// Discovered servers kept.
pub const MAX_DISCOVERED: usize = 8;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct DirState {
    /// Servers that passed the trial (URL, when).
    verified: Vec<(String, u64)>,
    /// Servers that failed it (URL -> when).
    rejected: HashMap<String, u64>,
    last_discovery_ms: u64,
}

/// Normalize a server URL: https only, no path, no trailing slash.
pub fn normalize(url: &str) -> Option<String> {
    let u = url.trim().trim_end_matches('/');
    let rest = u.strip_prefix("https://")?;
    if rest.is_empty() || rest.contains('/') || rest.contains(char::is_whitespace) || rest.len() > 253 {
        return None;
    }
    Some(format!("https://{}", rest.to_ascii_lowercase()))
}

/// Server URLs listed in kind-10063 events, most listed first.
pub fn rank_candidates(events: &[Event]) -> Vec<String> {
    let mut count: HashMap<String, (usize, std::collections::HashSet<PublicKey>)> = HashMap::new();
    for ev in events {
        if ev.kind != Kind::Custom(SERVER_LIST_KIND) {
            continue;
        }
        for t in ev.tags.iter() {
            let v = t.as_slice();
            if v.len() >= 2 && v[0] == "server" {
                if let Some(u) = normalize(&v[1]) {
                    let e = count.entry(u).or_default();
                    if e.1.insert(ev.pubkey) {
                        e.0 += 1; // one vote per publisher
                    }
                }
            }
        }
    }
    let mut v: Vec<(String, usize)> = count.into_iter().map(|(u, (n, _))| (u, n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.into_iter().map(|x| x.0).collect()
}

/// The discovered part of the Blossom server list, persisted.
pub struct BlossomDirectory {
    path: Option<std::path::PathBuf>,
    state: Mutex<DirState>,
}

impl BlossomDirectory {
    pub fn new(path: Option<std::path::PathBuf>) -> BlossomDirectory {
        let state = path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default();
        BlossomDirectory { path, state: Mutex::new(state) }
    }

    fn save(&self) {
        if let (Some(p), Ok(b)) = (&self.path, postcard::to_stdvec(&*self.state.lock().expect("lock"))) {
            let _ = kt_engine::storage::write_atomic(p, &b);
        }
    }

    /// Servers that passed the trial, most recently verified first.
    pub fn verified(&self) -> Vec<String> {
        let mut v = self.state.lock().expect("lock").verified.clone();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v.into_iter().map(|x| x.0).collect()
    }

    pub fn discovery_due(&self) -> bool {
        now_ms().saturating_sub(self.state.lock().expect("lock").last_discovery_ms) >= DISCOVER_EVERY_MS
    }

    /// Candidates worth a trial: not known, not recently rejected.
    pub fn fresh(&self, ranked: Vec<String>, known: &[String]) -> Vec<String> {
        let st = self.state.lock().expect("lock");
        let now = now_ms();
        ranked
            .into_iter()
            .filter(|u| !known.contains(u) && !st.verified.iter().any(|v| &v.0 == u) && st.rejected.get(u).is_none_or(|t| now.saturating_sub(*t) >= REJECT_FOR_MS))
            .take(TRY_PER_ROUND)
            .collect()
    }

    pub fn record(&self, url: &str, passed: bool) {
        let mut st = self.state.lock().expect("lock");
        let now = now_ms();
        if passed {
            st.verified.retain(|v| v.0 != url);
            st.verified.push((url.to_string(), now));
            st.verified.sort_by(|a, b| b.1.cmp(&a.1));
            st.verified.truncate(MAX_DISCOVERED);
            st.rejected.remove(url);
        } else {
            st.rejected.insert(url.to_string(), now);
            st.verified.retain(|v| v.0 != url);
        }
        drop(st);
        self.save();
    }

    /// Weekly: read published server lists from these relays, try a few new servers.
    /// Returns the servers that passed this time.
    pub async fn discover(&self, blossom: &Blossom, relays: &[std::sync::Arc<crate::nostr::RelayConn>]) -> Vec<String> {
        if !self.discovery_due() {
            return Vec::new();
        }
        self.state.lock().expect("lock").last_discovery_ms = now_ms();
        self.save();
        let filter = Filter::new().kind(Kind::Custom(SERVER_LIST_KIND)).limit(300);
        let mut events = Vec::new();
        for r in relays {
            if let Some(evs) = r.query(vec![filter.clone()]).await {
                events.extend(evs.into_iter().filter(|e| e.verify().is_ok()));
            }
        }
        let mut passed = Vec::new();
        for url in self.fresh(rank_candidates(&events), &blossom.servers()) {
            let ok = blossom.trial(&url).await;
            self.record(&url, ok);
            if ok {
                passed.push(url);
            }
        }
        passed
    }
}

impl Blossom {
    /// Use a server exactly as devices would: a small sealed blob as octet-stream, read
    /// back byte for byte, then deleted (with the key that uploaded it).
    pub async fn trial(&self, server: &str) -> bool {
        let noise: Vec<u8> = (0..2048).map(|_| rand::random::<u8>()).collect();
        let Ok((_, data)) = kt_engine::blobs::seal(&noise) else { return false };
        let sha = hex::encode(blob_id(&data));
        let keys = Keys::generate();
        let Some(auth) = Self::auth_with(&keys, "upload", &sha) else { return false };
        let put = self.client.put(format!("{server}/upload")).header("Authorization", auth).header("Content-Type", "application/octet-stream").body(data.clone()).send().await;
        if !put.is_ok_and(|r| r.status().is_success()) {
            return false;
        }
        let back = match self.client.get(format!("{server}/{sha}")).send().await {
            Ok(r) if r.status().is_success() => r.bytes().await.is_ok_and(|b| b.as_ref() == data.as_slice()),
            _ => false,
        };
        if let Some(auth) = Self::auth_with(&keys, "delete", &sha) {
            let _ = self.client.delete(format!("{server}/{sha}")).header("Authorization", auth).send().await;
        }
        back
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(keys: &Keys, servers: &[&str]) -> Event {
        let tags: Vec<Tag> = servers.iter().map(|s| Tag::parse(["server", s]).unwrap()).collect();
        EventBuilder::new(Kind::Custom(SERVER_LIST_KIND), "").tags(tags).finalize(keys).unwrap()
    }

    #[test]
    fn candidates_by_publishers_and_the_directory_rules() {
        let (a, b, c) = (Keys::generate(), Keys::generate(), Keys::generate());
        let evs = vec![
            list(&a, &["https://one.example/", "https://two.example", "http://plain.example", "https://x.example/path"]),
            list(&b, &["https://two.example", "https://ONE.example"]),
            list(&c, &["https://two.example", "https://two.example"]),
        ];
        assert_eq!(rank_candidates(&evs), vec!["https://two.example", "https://one.example"], "https only, no paths, one vote per publisher");

        let d = BlossomDirectory::new(None);
        assert!(d.discovery_due());
        let fresh = d.fresh(vec!["https://a.example".into(), "https://b.example".into(), "https://c.example".into(), "https://d.example".into()], &["https://a.example".into()]);
        assert_eq!(fresh, vec!["https://b.example", "https://c.example", "https://d.example"], "known ones skipped, at most 3");
        d.record("https://b.example", true);
        d.record("https://c.example", false);
        assert_eq!(d.verified(), vec!["https://b.example"]);
        assert_eq!(d.fresh(vec!["https://b.example".into(), "https://c.example".into(), "https://e.example".into()], &[]), vec!["https://e.example"], "verified and rejected aren't retried");
    }
}
