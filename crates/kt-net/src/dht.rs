//! Discovery over the Mainline DHT (DESIGN §6): members publish sealed whereabouts as
//! BEP44 mutable items, in client mode, directly (not through n0 or any pkarr relay).
//!
//! Keys and values come from the engine (`Replica::dht_publish` / `dht_lookups`): only
//! group members can compute where a member's record lives, the location rotates with
//! every key epoch, and the content is sealed and device-signed.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use kt_engine::replica::Replica;
use mainline::async_dht::AsyncDht;
use mainline::{Dht, MutableItem, SigningKey};

pub struct DhtDiscovery {
    /// Loopback test node: its local address counts as reachable.
    loopback: bool,
    dht: AsyncDht,
    replica: Arc<Mutex<Replica>>,
    cache: Option<std::path::PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DhtReport {
    pub published: usize,
    pub publish_failed: usize,
    pub found: usize,
    pub applied: usize,
}

impl DhtDiscovery {
    /// `bootstrap`: `None` = the public Mainline DHT. `bind`: `None` = all interfaces.
    pub fn new(replica: Arc<Mutex<Replica>>, bootstrap: Option<Vec<String>>, bind: Option<Ipv4Addr>) -> std::io::Result<DhtDiscovery> {
        let mut b = Dht::builder();
        if let Some(bs) = &bootstrap {
            b.bootstrap(bs);
        }
        if let Some(ip) = bind {
            b.bind_address(ip);
            if ip.is_loopback() {
                b.server_mode(); // a loopback test network: answer queries like a public node
            }
        }
        let loopback = bind.is_some_and(|ip| ip.is_loopback());
        Ok(DhtDiscovery { loopback, dht: b.build()?.as_async(), replica, cache: None })
    }

    /// Like [`DhtDiscovery::new`] on the public DHT, remembering good nodes in `cache`
    /// between sessions (used alongside the library's community bootstrap nodes, so the
    /// DHT is reachable even if those ever disappear).
    pub fn with_cache(replica: Arc<Mutex<Replica>>, cache: std::path::PathBuf, bind: Option<Ipv4Addr>) -> std::io::Result<DhtDiscovery> {
        let saved: Vec<String> = std::fs::read_to_string(&cache).map(|s| s.lines().map(str::to_string).filter(|l| !l.is_empty()).take(64).collect()).unwrap_or_default();
        let mut b = Dht::builder();
        if !saved.is_empty() {
            b.extra_bootstrap(&saved);
        }
        if let Some(ip) = bind {
            b.bind_address(ip);
        }
        Ok(DhtDiscovery { loopback: false, dht: b.build()?.as_async(), replica, cache: Some(cache) })
    }

    /// Our node's address if others can reach it (Mainline confirmed it's not firewalled),
    /// to share with our groups' devices as a bootstrap entry point.
    pub async fn entry_point(&self) -> Option<std::net::SocketAddr> {
        let info = self.dht.info().await;
        if self.loopback {
            return Some(std::net::SocketAddr::V4(info.local_addr()));
        }
        info.public_address().filter(|_| !info.firewalled()).map(std::net::SocketAddr::V4)
    }

    /// Our groups' devices that are DHT entry points (from their whereabouts).
    pub fn member_entry_points(&self) -> Vec<String> {
        let r = self.replica.lock().expect("replica lock");
        let mut out: Vec<String> = r
            .groups()
            .iter()
            .flat_map(|g| r.member_whereabouts(g))
            .filter_map(|w| postcard::from_bytes::<crate::p2p::WhereData>(&w.data).ok()?.dht)
            .map(|a| a.to_string())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Remember good nodes for the next start (call periodically and before shutdown):
    /// our groups' devices first, then what the DHT currently knows. With these, the DHT
    /// stays reachable even if the community bootstrap nodes disappear.
    pub async fn save_cache(&self) -> usize {
        let Some(p) = &self.cache else { return 0 };
        let mut nodes = self.member_entry_points();
        for n in self.dht.to_bootstrap().await {
            if !nodes.contains(&n) {
                nodes.push(n);
            }
        }
        nodes.truncate(64);
        if nodes.is_empty() {
            return 0;
        }
        let _ = kt_engine::storage::write_atomic(p, nodes.join("\n").as_bytes());
        nodes.len()
    }

    /// Store a value (≤ 1000 bytes) as a mutable item under a key derived from `seed`.
    pub async fn put_value(&self, seed: [u8; 32], value: &[u8]) -> bool {
        if value.len() > 1000 {
            return false;
        }
        let item = MutableItem::new(SigningKey::from_bytes(&seed), value, now_secs(), None);
        self.dht.put_mutable(item, None).await.is_ok()
    }

    /// Read the newest value stored under a key derived from `seed`.
    pub async fn get_value(&self, seed: [u8; 32]) -> Option<Vec<u8>> {
        let pk = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        self.dht.get_mutable_most_recent(&pk, None).await.map(|i| i.value().to_vec())
    }

    pub async fn bootstrapped(&self) -> bool {
        self.dht.bootstrapped().await
    }

    /// Publish our records (call when whereabouts change, and hourly: DHT items expire
    /// after about two hours).
    pub async fn publish(&self) -> DhtReport {
        let items = self.replica.lock().expect("replica lock").dht_publish();
        let seq = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let mut report = DhtReport::default();
        for it in items {
            let item = MutableItem::new(SigningKey::from_bytes(&it.key_seed), &it.value, seq, None);
            match self.dht.put_mutable(item, None).await {
                Ok(_) => report.published += 1,
                Err(_) => report.publish_failed += 1,
            }
        }
        report
    }

    /// Look up the other members' records and apply what's found.
    pub async fn lookup(&self) -> DhtReport {
        let lookups = self.replica.lock().expect("replica lock").dht_lookups();
        let mut report = DhtReport::default();
        for l in lookups {
            let pk = SigningKey::from_bytes(&l.key_seed).verifying_key().to_bytes();
            if let Some(item) = self.dht.get_mutable_most_recent(&pk, None).await {
                report.found += 1;
                if self.replica.lock().expect("replica lock").open_dht_value(&l, item.value()).is_ok() {
                    report.applied += 1;
                }
            }
        }
        report
    }
}

// ---------------------------------------------------------------- helper slots (fallback)

/// Public daily slots on the DHT where helpers post their (self-signed) address: a way to
/// find helpers that depends on neither Nostr nor any helper we already know. Anyone can
/// write a slot (its key is public), so it's a fallback, not a guarantee; entries can't be
/// forged (each is signed by the helper's own endpoint key) and are verified on use.
pub const HELPER_SLOTS: u32 = 32;
const REFS_PER_SLOT: usize = 4;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SignedRef {
    helper: crate::helper::HelperRef,
    day: u32,
    sig: Vec<u8>,
}

fn slot_key(day: u32, i: u32) -> SigningKey {
    let mut info = day.to_le_bytes().to_vec();
    info.extend_from_slice(&i.to_le_bytes());
    SigningKey::from_bytes(&kt_engine::crypto::derive(b"public", "kt/helper-dht-slot/v1", &info).0)
}

fn ref_message(h: &crate::helper::HelperRef, day: u32) -> Vec<u8> {
    let mut m = b"kt/helper-ref/v1".to_vec();
    m.extend_from_slice(&postcard::to_stdvec(h).unwrap_or_default());
    m.extend_from_slice(&day.to_le_bytes());
    m
}

fn verified(r: &SignedRef) -> bool {
    let (Ok(pk), Ok(sig)) = (iroh::PublicKey::from_bytes(&r.helper.endpoint), <[u8; 64]>::try_from(r.sig.as_slice())) else { return false };
    pk.verify(&ref_message(&r.helper, r.day), &iroh::Signature::from_bytes(&sig)).is_ok()
}

impl DhtDiscovery {
    /// Post our helper address in today's slot (merged with what's there, newest kept), then
    /// read it back and retry if a concurrent writer overwrote us (DHT lost updates).
    pub async fn announce_helper(&self, me: &crate::helper::HelperRef, key: &iroh::SecretKey, day: u32) -> bool {
        for attempt in 0..3u64 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(200 * attempt + u64::from(me.endpoint[2]) % 200)).await;
            }
            if self.announce_helper_once(me, key, day).await && self.slot_has(me, day).await {
                return true;
            }
        }
        false
    }

    async fn slot_has(&self, me: &crate::helper::HelperRef, day: u32) -> bool {
        let i = (u32::from(me.endpoint[0]) << 8 | u32::from(me.endpoint[1])) % HELPER_SLOTS;
        let pk = slot_key(day, i).verifying_key().to_bytes();
        self.dht
            .get_mutable_most_recent(&pk, None)
            .await
            .and_then(|it| postcard::from_bytes::<Vec<SignedRef>>(it.value()).ok())
            .is_some_and(|v| v.iter().any(|r| r.helper.endpoint == me.endpoint && r.day == day))
    }

    async fn announce_helper_once(&self, me: &crate::helper::HelperRef, key: &iroh::SecretKey, day: u32) -> bool {
        let i = (u32::from(me.endpoint[0]) << 8 | u32::from(me.endpoint[1])) % HELPER_SLOTS;
        let sk = slot_key(day, i);
        let pk = sk.verifying_key().to_bytes();
        let current = self.dht.get_mutable_most_recent(&pk, None).await;
        let mut refs: Vec<SignedRef> = current
            .as_ref()
            .and_then(|it| postcard::from_bytes::<Vec<SignedRef>>(it.value()).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.day == day && r.helper.endpoint != me.endpoint && verified(r))
            .collect();
        let sig = key.sign(&ref_message(me, day)).to_bytes().to_vec();
        refs.insert(0, SignedRef { helper: me.clone(), day, sig });
        refs.truncate(REFS_PER_SLOT);
        let mut value = postcard::to_stdvec(&refs).unwrap_or_default();
        while value.len() > 1000 && refs.len() > 1 {
            refs.pop();
            value = postcard::to_stdvec(&refs).unwrap_or_default();
        }
        let seq = current.as_ref().map(|c| c.seq() + 1).unwrap_or(1).max(now_secs());
        let item = MutableItem::new(sk, &value, seq, None);
        self.dht.put_mutable(item, current.map(|c| c.seq())).await.is_ok()
    }

    /// Read today's helper slots (and yesterday's if today's yield few), a few lookups at
    /// a time: DHT lookups are slow and we shouldn't flood the network.
    pub async fn find_helpers(&self, day: u32) -> Vec<crate::helper::HelperRef> {
        use futures_util::StreamExt;
        let mut out: Vec<crate::helper::HelperRef> = Vec::new();
        for d in [day, day.saturating_sub(1)] {
            let items: Vec<_> = futures_util::stream::iter(0..HELPER_SLOTS)
                .map(|i| {
                    let pk = slot_key(d, i).verifying_key().to_bytes();
                    async move { self.dht.get_mutable_most_recent(&pk, None).await }
                })
                .buffer_unordered(8)
                .collect()
                .await;
            for it in items.into_iter().flatten() {
                for r in postcard::from_bytes::<Vec<SignedRef>>(it.value()).unwrap_or_default() {
                    if verified(&r) && !out.iter().any(|x| x.endpoint == r.helper.endpoint) {
                        out.push(r.helper);
                    }
                }
            }
            if out.len() >= 8 {
                break;
            }
        }
        out
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Fuzzing (DESIGN §9): DHT values are written by anyone.
#[cfg(test)]
mod fuzz_tests {
    use super::*;

    #[test]
    fn helper_refs() {
        let key = iroh::SecretKey::from_bytes(&[6; 32]);
        let me = crate::helper::HelperRef { endpoint: *key.public().as_bytes(), ips: vec!["192.0.2.3:1".parse().unwrap()], relays: vec![], relay: None };
        let sig = key.sign(&ref_message(&me, 20_000)).to_bytes().to_vec();
        let seeds = vec![postcard::to_stdvec(&vec![SignedRef { helper: me, day: 20_000, sig }]).unwrap()];
        kt_fuzz::run("dht-refs", &seeds, 2.0, |b| {
            for r in postcard::from_bytes::<Vec<SignedRef>>(b).unwrap_or_default() {
                let _ = verified(&r);
            }
        });
    }
}
