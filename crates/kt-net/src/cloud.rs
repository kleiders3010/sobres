//! Cloud mailbox client (DESIGN §7b, D56): a developer-run slot store listed in the
//! operator manifest (a Cloudflare Worker, `cloud/mailbox-worker`). Optional capacity, never
//! required: when it's gone or over its daily budget, Nostr, helpers and member devices
//! carry on.
//!
//! - **Writes always go there** ([`CloudMode::Always`]): helpers can vanish after a write,
//!   so deciding at write time would miss exactly the case it's for. The manifest can
//!   switch it to [`CloudMode::WhenHelpersWeak`] once the helper network is large.
//! - **Reads are cheap:** "what's newer than revision N" per topic, usually a few bytes.
//!   Every round while this device's helpers are weak, every [`READ_EVERY`] rounds otherwise.
//! - Its own writer key derivation, so the cloud can't link a device's slots to its helper
//!   writes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::SecretKey;
use sha2::{Digest, Sha256};

use crate::helper::HelperMailbox;
use crate::mailbox::{BoxFut, Capabilities, Mailbox, SlotRead, SlotWrite, WriteOutcome};

/// The developer-run mailbox the app ships with (a baseline entry, like the operator
/// manifest's; the app works without it).
pub const DEFAULT_URL: &str = "https://mailbox.jurisgames.com";
pub const MAX_SLOT: usize = 640 * 1024;
/// Reads when helpers are healthy: one round in this many.
pub const READ_EVERY: u64 = 4;
/// "Helpers are weak": fewer healthy helpers than this for a topic.
pub const WEAK_BELOW: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CloudMode {
    Always,
    WhenHelpersWeak,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn writer_key(seed: &[u8; 32]) -> SecretKey {
    SecretKey::from_bytes(&kt_engine::crypto::derive(seed, "kt/cloud-writer/v1", b"").0)
}

pub fn put_message(topic: &[u8; 32], version: u64, blob: &[u8]) -> Vec<u8> {
    let mut m = b"kt/cloud-put/v1".to_vec();
    m.extend_from_slice(topic);
    m.extend_from_slice(&version.to_le_bytes());
    m.extend_from_slice(&Sha256::digest(blob));
    m
}

fn pow_zeros(topic: &[u8; 32], writer: &[u8; 32], version: u64, nonce: u64) -> u32 {
    let mut h = Sha256::new();
    h.update(b"kt/cloud-pow/v1");
    h.update(topic);
    h.update(writer);
    h.update(version.to_le_bytes());
    h.update(nonce.to_le_bytes());
    let out = h.finalize();
    let mut z = 0;
    for b in out {
        if b == 0 {
            z += 8;
        } else {
            return z + b.leading_zeros();
        }
    }
    z
}

pub fn solve_pow(topic: &[u8; 32], writer: &[u8; 32], version: u64, bits: u32) -> u64 {
    let mut nonce = rand::random::<u64>();
    while pow_zeros(topic, writer, version, nonce) < bits {
        nonce = nonce.wrapping_add(1);
    }
    nonce
}

/// Parse a topic read: current revision, then records.
fn parse_read(topic: [u8; 32], b: &[u8]) -> Option<(u64, Vec<SlotRead>)> {
    let rev = u64::from_le_bytes(b.get(..8)?.try_into().ok()?);
    let mut out = Vec::new();
    let mut i = 8;
    while i < b.len() {
        let writer: [u8; 32] = b.get(i..i + 32)?.try_into().ok()?;
        let version = u64::from_le_bytes(b.get(i + 32..i + 40)?.try_into().ok()?);
        let len = u32::from_le_bytes(b.get(i + 48..i + 52)?.try_into().ok()?) as usize;
        if len > MAX_SLOT {
            return None;
        }
        let blob = b.get(i + 52..i + 52 + len)?.to_vec();
        out.push(SlotRead { topic, writer, version, blob });
        i += 52 + len;
    }
    Some((rev, out))
}

/// A read that takes longer than this counts as the cloud being down.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// The same for a write (a slot is at most 640 KB); below the syncer's limit for a layer, so a cloud that doesn't
/// answer is noticed here and left alone, instead of being cut off by the syncer each time.
const WRITE_TIMEOUT: Duration = Duration::from_secs(20);
/// After the cloud didn't answer: leave it alone this long, doubling up to `DOWN_MAX` while it stays down.
const DOWN_START: Duration = Duration::from_secs(60);
const DOWN_MAX: Duration = Duration::from_secs(15 * 60);

pub struct CloudMailbox {
    /// Until when to leave the cloud alone, and the next wait.
    down: Mutex<(Option<Instant>, Duration)>,
    base: String,
    client: reqwest::Client,
    mode: Mutex<CloudMode>,
    helpers: Mutex<Option<Arc<HelperMailbox>>>,
    rounds: AtomicU64,
    /// Per topic: last revision seen and each writer's latest slot.
    cache: Mutex<HashMap<[u8; 32], (u64, HashMap<[u8; 32], SlotRead>)>>,
}

impl CloudMailbox {
    pub fn new(base: &str, mode: CloudMode) -> Option<CloudMailbox> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = crate::http::client(Duration::from_secs(30))?;
        Some(CloudMailbox { down: Mutex::new((None, DOWN_START)), base: base.trim_end_matches('/').to_string(), client, mode: Mutex::new(mode), helpers: Mutex::new(None), rounds: AtomicU64::new(0), cache: Mutex::new(HashMap::new()) })
    }

    /// Helper health decides how often to read (and whether to write, in fallback mode).
    pub fn set_helpers(&self, h: Arc<HelperMailbox>) {
        *self.helpers.lock().expect("lock") = Some(h);
    }

    pub fn set_mode(&self, m: CloudMode) {
        *self.mode.lock().expect("lock") = m;
    }

    fn helpers_weak(&self, topic: &[u8; 32]) -> bool {
        self.helpers.lock().expect("lock").as_ref().is_none_or(|h| h.healthy_helpers(topic) < WEAK_BELOW)
    }

    fn is_down(&self) -> bool {
        self.down.lock().expect("lock").0.is_some_and(|t| Instant::now() < t)
    }

    /// No answer (a connection error or a timeout, not a refusal): leave it alone for a while.
    fn went_down(&self) {
        let mut d = self.down.lock().expect("lock");
        d.0 = Some(Instant::now() + d.1);
        d.1 = (d.1 * 2).min(DOWN_MAX);
    }

    fn answered(&self) {
        *self.down.lock().expect("lock") = (None, DOWN_START);
    }

    fn url(&self, topic: &[u8; 32]) -> String {
        format!("{}/v1/t/{}", self.base, hex::encode(topic))
    }

    async fn put(&self, topic: &[u8; 32], key: &SecretKey, version: u64, blob: &[u8]) -> WriteOutcome {
        let writer = *key.public().as_bytes();
        let sig = key.sign(&put_message(topic, version, blob)).to_bytes();
        let mut nonce = 0u64;
        for _ in 0..2 {
            let mut body = Vec::with_capacity(112 + blob.len());
            body.extend_from_slice(&writer);
            body.extend_from_slice(&version.to_le_bytes());
            body.extend_from_slice(&nonce.to_le_bytes());
            body.extend_from_slice(&sig);
            body.extend_from_slice(blob);
            let r = match self.client.put(self.url(topic)).body(body).timeout(WRITE_TIMEOUT).send().await {
                Ok(r) => r,
                Err(_) => {
                    self.went_down();
                    return WriteOutcome::Unavailable;
                }
            };
            self.answered();
            match r.status().as_u16() {
                200 => return WriteOutcome::Stored { stored: 1, tried: 1 },
                428 => {
                    let bits: u32 = r.headers().get("x-pow-bits").and_then(|v| v.to_str().ok()?.parse().ok()).unwrap_or(99);
                    if bits > 24 {
                        return WriteOutcome::Deferred { retry_after: Duration::from_secs(3600) };
                    }
                    let t = *topic;
                    nonce = match tokio::task::spawn_blocking(move || solve_pow(&t, &writer, version, bits)).await {
                        Ok(n) => n,
                        Err(_) => return WriteOutcome::Unavailable,
                    };
                }
                413 => return WriteOutcome::Refused("slot too large for the cloud mailbox".into()),
                // Topic full, daily budget used up, overloaded: later.
                507 | 429 | 503 => return WriteOutcome::Deferred { retry_after: Duration::from_secs(3600) },
                _ => return WriteOutcome::Unavailable,
            }
        }
        WriteOutcome::Unavailable
    }
}

impl Mailbox for CloudMailbox {
    fn name(&self) -> String {
        format!("cloud:{}", self.base)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { replace: true, max_slot: MAX_SLOT, ttl: Some(Duration::from_secs(14 * 86_400)), read_auth: false, subscribe: false, write_budget: None }
    }

    fn writer_id(&self, writer_seed: &[u8; 32]) -> [u8; 32] {
        *writer_key(writer_seed).public().as_bytes()
    }

    fn write<'a>(&'a self, w: &'a SlotWrite) -> BoxFut<'a, WriteOutcome> {
        Box::pin(async move {
            if *self.mode.lock().expect("lock") == CloudMode::WhenHelpersWeak && !self.helpers_weak(&w.topic) {
                return WriteOutcome::Refused("helpers are enough (fallback mode)".into());
            }
            if w.blob.len() > MAX_SLOT {
                return WriteOutcome::Refused("slot too large for the cloud mailbox".into());
            }
            if self.is_down() {
                return WriteOutcome::Unavailable;
            }
            self.put(&w.topic, &writer_key(&w.writer_seed), now_ms(), &w.blob).await
        })
    }

    fn read<'a>(&'a self, topics: &'a [[u8; 32]]) -> BoxFut<'a, Vec<SlotRead>> {
        Box::pin(async move {
            let round = self.rounds.fetch_add(1, Ordering::Relaxed);
            let due: Vec<[u8; 32]> = if self.is_down() {
                vec![] // what it gave before is still returned below
            } else {
                topics
                    .iter()
                    .filter(|t| self.helpers_weak(t) || round % READ_EVERY == 0 || !self.cache.lock().expect("lock").contains_key(*t))
                    .copied()
                    .collect() // (helpers carry the rest; the cloud is checked every few rounds)
            };
            // all topics at once, each with a short time limit: a cloud that doesn't answer costs one wait, not one per list
            let got = futures_util::future::join_all(due.iter().map(|t| async move {
                let after = self.cache.lock().expect("lock").get(t).map(|c| c.0).unwrap_or(0);
                let r = self.client.get(format!("{}?after={after}", self.url(t))).timeout(READ_TIMEOUT).send().await.ok()?;
                let ok = r.status().is_success();
                // A topic holds at most 2 MB of slots plus their headers.
                let b = crate::http::bytes_capped(r, 3 * 1024 * 1024).await?;
                Some((*t, ok.then_some(b)))
            }))
            .await;
            if !due.is_empty() {
                if got.iter().all(|g| g.is_none()) {
                    self.went_down();
                } else {
                    self.answered();
                }
            }
            for (t, b) in got.into_iter().flatten() {
                let Some(b) = b else { continue };
                let t = &t;
                let Some((rev, slots)) = parse_read(*t, &b) else { continue };
                let mut c = self.cache.lock().expect("lock");
                let e = c.entry(*t).or_insert((0, HashMap::new()));
                if rev < e.0 {
                    e.1.clear(); // the topic was swept and started over
                }
                e.0 = rev;
                for s in slots {
                    e.1.insert(s.writer, s);
                }
            }
            let c = self.cache.lock().expect("lock");
            topics.iter().filter_map(|t| c.get(t)).flat_map(|e| e.1.values().cloned()).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_format() {
        let mut b = 7u64.to_le_bytes().to_vec();
        b.extend_from_slice(&[1; 32]);
        b.extend_from_slice(&5u64.to_le_bytes());
        b.extend_from_slice(&7u64.to_le_bytes());
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(b"abc");
        let (rev, s) = parse_read([9; 32], &b).unwrap();
        assert_eq!((rev, s.len(), s[0].version, s[0].blob.as_slice()), (7, 1, 5, &b"abc"[..]));
        assert!(parse_read([9; 32], &b[..b.len() - 1]).is_none(), "truncated");
        let mut huge = 0u64.to_le_bytes().to_vec();
        huge.extend_from_slice(&[0; 48]);
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_read([9; 32], &huge).is_none());
    }

    #[test]
    fn pow() {
        let n = solve_pow(&[1; 32], &[2; 32], 3, 10);
        assert!(pow_zeros(&[1; 32], &[2; 32], 3, n) >= 10);
    }
}
