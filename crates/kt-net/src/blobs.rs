//! Blob transfer (DESIGN §4 "Large files"): text attachments move over every layer at once.
//!
//! - **P2P** (ALPN [`crate::p2p::BLOB_ALPN`]): a device serves the blobs it holds to
//!   anyone who asks by ID. The ID is the hash of ciphertext and only appears inside
//!   encrypted documents, so asking proves nothing and reveals nothing.
//! - **Helpers** ([`BlobHold`]): strangers' devices keep sealed blobs for a while, like
//!   slots, so a blob waits somewhere while its group is offline. Every put costs proof of
//!   work (scaled with load and size); a re-put of a held blob only refreshes it.
//! - Each device re-offers the blobs its lists use to the group's helpers weekly, so helper
//!   copies outlive their TTL for as long as the blob is in use.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use kt_engine::blobs::{blob_id, BlobId, BlobStore};
use serde::{Deserialize, Serialize};

use crate::helper::{HelperLimits, HelperMailbox, PutReply};
use crate::p2p::P2p;

/// Helpers drop blobs not put or refreshed for this long.
pub const HELPER_BLOB_TTL_MS: u64 = 30 * 86_400_000;
/// How often a device re-offers a blob in use to its helpers.
pub const REOFFER_MS: u64 = 7 * 86_400_000;
/// Helpers each blob goes to.
pub const BLOB_COPIES: usize = 3;
/// Member devices asked per missing blob in one round.
const PEERS_PER_BLOB: usize = 4;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Proof of work for a blob put: the same hash race as slot puts, bound to the blob.
pub fn blob_pow_ok(id: &BlobId, len: u64, nonce: u64, bits: u8) -> bool {
    crate::helper::pow_leading_zeros(id, &[0; 32], len, nonce) >= bits as u32
}

pub fn solve_blob_pow(id: &BlobId, len: u64, bits: u8) -> u64 {
    crate::helper::solve_pow(id, &[0; 32], len, bits)
}

// ------------------------------------------------------------------ helper side

/// A helper's blobs: sealed bytes from strangers, capped, expiring, on disk when the
/// helper keeps a disk copy (they are ciphertext already).
pub struct BlobHold {
    capacity: u64,
    max_blob: u64,
    base_pow_bits: u8,
    dir: Option<PathBuf>,
    inner: Mutex<(HashMap<BlobId, (Vec<u8>, u64)>, u64)>,
}

impl BlobHold {
    pub fn new(limits: &HelperLimits) -> BlobHold {
        let dir = limits.disk.as_ref().map(|d| {
            let mut name = d.dir.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            name.push("-blobs");
            d.dir.with_file_name(name)
        });
        let hold = BlobHold { capacity: limits.blob_capacity, max_blob: limits.max_blob, base_pow_bits: limits.base_pow_bits, dir, inner: Mutex::new((HashMap::new(), 0)) };
        hold.load();
        hold
    }

    fn load(&self) {
        let Some(dir) = &self.dir else { return };
        let now = now_ms();
        let mut g = self.inner.lock().expect("lock");
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let ok = (|| {
                let id: BlobId = hex::decode(e.file_name().to_string_lossy().as_ref()).ok()?.try_into().ok()?;
                let modified = e.metadata().ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as u64;
                let data = std::fs::read(e.path()).ok()?;
                (blob_id(&data) == id && now.saturating_sub(modified) < HELPER_BLOB_TTL_MS && g.1 + data.len() as u64 <= self.capacity).then_some((id, data, modified))
            })();
            match ok {
                Some((id, data, t)) => {
                    g.1 += data.len() as u64;
                    g.0.insert(id, (data, t));
                }
                None => {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    /// Proof-of-work bits a new blob of `len` bytes needs now: more as the hold fills, and
    /// one more per MiB.
    pub fn pow_bits(&self, len: u64) -> u8 {
        let used = self.inner.lock().expect("lock").1;
        let load = (used * 8 / self.capacity.max(1)).min(8) as u8;
        self.base_pow_bits + load + (len / (1024 * 1024)).min(8) as u8
    }

    pub fn put(&self, data: Vec<u8>, nonce: u64) -> PutReply {
        let len = data.len() as u64;
        if len > self.max_blob || len > self.capacity {
            return PutReply::TooLarge;
        }
        let id = blob_id(&data);
        let now = now_ms();
        let mut g = self.inner.lock().expect("lock");
        if let Some(e) = g.0.get_mut(&id) {
            e.1 = now; // a refresh: free
            drop(g);
            self.touch(&id);
            return PutReply::Stale;
        }
        drop(g);
        let bits = self.pow_bits(len);
        if !blob_pow_ok(&id, len, nonce, bits) {
            return PutReply::NeedPow(bits);
        }
        let mut g = self.inner.lock().expect("lock");
        // Full: the least recently refreshed go first.
        while g.1 + len > self.capacity {
            let Some(oldest) = g.0.iter().min_by_key(|(_, v)| v.1).map(|(k, _)| *k) else { break };
            if let Some((d, _)) = g.0.remove(&oldest) {
                g.1 -= d.len() as u64;
            }
            if let Some(dir) = &self.dir {
                let _ = std::fs::remove_file(dir.join(hex::encode(oldest)));
            }
        }
        if let Some(dir) = &self.dir {
            let _ = std::fs::create_dir_all(dir);
            let _ = kt_engine::storage::write_atomic(&dir.join(hex::encode(id)), &data);
        }
        g.1 += len;
        g.0.insert(id, (data, now));
        PutReply::Stored
    }

    fn touch(&self, id: &BlobId) {
        if let Some(dir) = &self.dir {
            let p = dir.join(hex::encode(id));
            if let Ok(f) = std::fs::File::options().write(true).open(&p) {
                let _ = f.set_modified(std::time::SystemTime::now());
            }
        }
    }

    pub fn get(&self, id: &BlobId) -> Option<Vec<u8>> {
        self.inner.lock().expect("lock").0.get(id).map(|e| e.0.clone())
    }

    /// Drop expired blobs.
    pub fn sweep(&self) -> usize {
        let now = now_ms();
        let mut g = self.inner.lock().expect("lock");
        let stale: Vec<BlobId> = g.0.iter().filter(|(_, v)| now.saturating_sub(v.1) >= HELPER_BLOB_TTL_MS).map(|(k, _)| *k).collect();
        for id in &stale {
            if let Some((d, _)) = g.0.remove(id) {
                g.1 -= d.len() as u64;
            }
            if let Some(dir) = &self.dir {
                let _ = std::fs::remove_file(dir.join(hex::encode(id)));
            }
        }
        stale.len()
    }

    /// (blobs held, bytes used)
    pub fn held(&self) -> (usize, u64) {
        let g = self.inner.lock().expect("lock");
        (g.0.len(), g.1)
    }

    /// Delete the disk copy (helping turned off).
    pub fn wipe(&self) {
        let mut g = self.inner.lock().expect("lock");
        g.0.clear();
        g.1 = 0;
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

// ------------------------------------------------------------------ client side

/// When each blob was last offered to helpers (persisted beside the blob store).
#[derive(Default, Serialize, Deserialize)]
struct Offered(HashMap<BlobId, u64>);

fn offered_path(store: &BlobStore) -> PathBuf {
    let mut name = store.dir().file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".offered");
    store.dir().with_file_name(name)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlobReport {
    /// Missing blobs that arrived this round.
    pub fetched: usize,
    /// Blobs offered to helpers this round.
    pub offered: usize,
    /// Still missing after this round.
    pub missing: usize,
}

/// One blob round: fetch what's missing (members first, then helpers, then Blossom), and
/// offer the blobs in use to helpers and Blossom when they're due.
pub async fn round(p2p: Option<&P2p>, helpers: Option<&HelperMailbox>, blossom: Option<&crate::blossom::Blossom>, store: &BlobStore, wanted: &[BlobId], in_use: &[BlobId]) -> BlobReport {
    let mut rep = BlobReport::default();
    let members = p2p.map(|p| p.member_addrs()).unwrap_or_default();
    for id in wanted {
        if store.has(id) {
            continue;
        }
        let mut got = None;
        if let Some(p) = p2p {
            for a in members.iter().take(PEERS_PER_BLOB) {
                if let Some(b) = p.fetch_blob(a.clone(), *id).await {
                    got = Some(b);
                    break;
                }
            }
        }
        if got.is_none() {
            if let Some(h) = helpers {
                got = h.get_blob(id).await;
            }
        }
        if got.is_none() {
            if let Some(b) = blossom {
                got = b.get(id).await;
            }
        }
        match got.map(|b| store.put_expected(id, &b)) {
            Some(Ok(())) => rep.fetched += 1,
            _ => rep.missing += 1,
        }
    }
    if helpers.is_some() || blossom.is_some() {
        let path = offered_path(store);
        let mut offered: Offered = std::fs::read(&path).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default();
        let now = now_ms();
        offered.0.retain(|id, _| in_use.contains(id));
        for id in in_use {
            if offered.0.get(id).is_some_and(|t| now.saturating_sub(*t) < REOFFER_MS) {
                continue;
            }
            let Some(b) = store.get(id) else { continue };
            let mut held = 0;
            if let Some(h) = helpers {
                held += h.put_blob(&b, BLOB_COPIES).await;
            }
            if let Some(bl) = blossom {
                held += bl.offer(&b).await;
            }
            if held > 0 {
                offered.0.insert(*id, now);
                rep.offered += 1;
            }
        }
        if let Ok(b) = postcard::to_stdvec(&offered) {
            let _ = kt_engine::storage::write_atomic(&path, &b);
        }
    }
    rep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_rules() {
        let limits = HelperLimits { blob_capacity: 3000, max_blob: 2000, base_pow_bits: 4, ..Default::default() };
        let h = BlobHold::new(&limits);
        let blob = |n: u8, len: usize| vec![n; len];
        let a = blob(1, 1000);
        let id_a = blob_id(&a);
        let bits = match h.put(a.clone(), 0) {
            PutReply::NeedPow(b) => b,
            r => panic!("{r:?}"),
        };
        assert_eq!(h.put(a.clone(), solve_blob_pow(&id_a, 1000, bits)), PutReply::Stored);
        assert_eq!(h.get(&id_a).unwrap(), a);
        assert_eq!(h.put(a.clone(), 0), PutReply::Stale, "a refresh needs no work");
        assert_eq!(h.put(blob(2, 2500), 0), PutReply::TooLarge);
        // Full: the least recently refreshed goes (times are in ms: keep them apart).
        for n in 3..6u8 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            let d = blob(n, 1000);
            let id = blob_id(&d);
            let bits = h.pow_bits(1000);
            assert_eq!(h.put(d, solve_blob_pow(&id, 1000, bits)), PutReply::Stored);
        }
        assert!(h.held().1 <= 3000);
        assert!(h.get(&id_a).is_none(), "oldest evicted");
    }
}
