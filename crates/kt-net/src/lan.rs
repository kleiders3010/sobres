//! Same-network discovery with rotating identifiers (DESIGN §6, D57).
//!
//! Every few seconds while the app is open, a device sends a small UDP beacon (multicast
//! on the LAN): per group, a tag that rotates every [`LAN_WINDOW_SECS`] and the sealed,
//! device-signed whereabouts. Nothing in it is stable or readable by non-members, and the
//! entry count is padded so it doesn't reveal how many groups a device is in. Receivers
//! match tags against their own groups (their own clock ±1 window, so old beacons can't be
//! replayed into the future), open the entry and learn the member's LAN address; P2P then
//! dials it directly.
//!
//! Android needs a `MulticastLock` while the app listens (M4, §8).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::{Replica, LAN_WINDOW_SECS};
use tokio::net::UdpSocket;

pub const LAN_PORT: u16 = 47707;
pub const LAN_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 77, 77);
const MAGIC: &[u8; 4] = b"KTL1";
const MAX_DATAGRAM: usize = 1400;

#[derive(Clone, Debug)]
pub struct LanConfig {
    pub bind: SocketAddr,
    /// Where beacons go: the multicast group normally; explicit addresses in tests.
    pub targets: Vec<SocketAddr>,
    pub join_multicast: bool,
    pub interval: Duration,
}

impl Default for LanConfig {
    fn default() -> Self {
        LanConfig {
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, LAN_PORT)),
            targets: vec![SocketAddr::from((LAN_GROUP, LAN_PORT))],
            join_multicast: true,
            interval: Duration::from_secs(5),
        }
    }
}

fn window_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) / LAN_WINDOW_SECS
}

/// Encode entries, padded to a power of two with random entries of similar size.
fn encode(mut entries: Vec<([u8; 16], Vec<u8>)>) -> Vec<Vec<u8>> {
    let typical = entries.first().map(|e| e.1.len()).unwrap_or(300);
    let mut target = 2usize;
    while target < entries.len() {
        target *= 2;
    }
    while entries.len() < target {
        let tag: [u8; 16] = rand::random();
        let body: Vec<u8> = (0..typical).map(|_| rand::random::<u8>()).collect();
        entries.push((tag, body));
    }
    // Shuffle so padding isn't always last.
    for i in (1..entries.len()).rev() {
        let j = rand::random::<usize>() % (i + 1);
        entries.swap(i, j);
    }
    // Split into datagrams under the size limit.
    let mut out = Vec::new();
    let mut cur: Vec<u8> = MAGIC.to_vec();
    for (tag, body) in entries {
        let need = 16 + 2 + body.len();
        if cur.len() + need > MAX_DATAGRAM && cur.len() > MAGIC.len() {
            out.push(std::mem::replace(&mut cur, MAGIC.to_vec()));
        }
        cur.extend_from_slice(&tag);
        cur.extend_from_slice(&(body.len() as u16).to_le_bytes());
        cur.extend_from_slice(&body);
    }
    if cur.len() > MAGIC.len() {
        out.push(cur);
    }
    out
}

fn decode(d: &[u8]) -> Vec<([u8; 16], &[u8])> {
    let mut out = Vec::new();
    if d.len() < 4 || &d[..4] != MAGIC {
        return out;
    }
    let mut i = 4;
    while i + 18 <= d.len() && out.len() < 64 {
        let tag: [u8; 16] = d[i..i + 16].try_into().expect("16");
        let n = u16::from_le_bytes([d[i + 16], d[i + 17]]) as usize;
        i += 18;
        if i + n > d.len() {
            break;
        }
        out.push((tag, &d[i..i + n]));
        i += n;
    }
    out
}

pub struct Lan {
    sock: UdpSocket,
    replica: Arc<Mutex<Replica>>,
    cfg: LanConfig,
    /// Extra entries to beacon (sealed invite records under their lookup tag).
    extra: Mutex<Vec<([u8; 16], Vec<u8>)>>,
    /// Tags we're listening for (a joiner's invite lookup) and what we heard under them.
    watched: Mutex<std::collections::HashMap<[u8; 16], Vec<Vec<u8>>>>,
    /// Fires when a beacon taught us a (new) member address.
    pub learned: tokio::sync::Notify,
}

impl Lan {
    pub async fn start(replica: Arc<Mutex<Replica>>, cfg: LanConfig) -> std::io::Result<Arc<Lan>> {
        let sock = UdpSocket::bind(cfg.bind).await?;
        if cfg.join_multicast {
            let _ = sock.join_multicast_v4(LAN_GROUP, Ipv4Addr::UNSPECIFIED);
            let _ = sock.set_multicast_loop_v4(false);
        }
        let lan = Arc::new(Lan {
            sock,
            replica,
            cfg,
            extra: Mutex::new(Vec::new()),
            watched: Mutex::new(std::collections::HashMap::new()),
            learned: tokio::sync::Notify::new(),
        });
        let rx = lan.clone();
        tokio::spawn(async move { rx.recv_loop().await });
        Ok(lan)
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    /// Also beacon these entries (e.g. sealed invite records under their lookup tag).
    pub fn set_extra(&self, entries: Vec<([u8; 16], Vec<u8>)>) {
        *self.extra.lock().expect("lock") = entries;
    }

    /// Listen for entries under `tag` (collected until taken).
    pub fn watch(&self, tag: [u8; 16]) {
        self.watched.lock().expect("lock").entry(tag).or_default();
    }

    pub fn take_heard(&self, tag: &[u8; 16]) -> Vec<Vec<u8>> {
        self.watched.lock().expect("lock").get_mut(tag).map(std::mem::take).unwrap_or_default()
    }

    /// Send one beacon now.
    pub async fn beacon(&self) -> usize {
        let mut entries = self.replica.lock().expect("replica lock").lan_beacon_entries(window_now());
        entries.extend(self.extra.lock().expect("lock").iter().cloned());
        if entries.is_empty() {
            return 0;
        }
        let mut sent = 0;
        for d in encode(entries) {
            for t in &self.cfg.targets {
                if self.sock.send_to(&d, t).await.is_ok() {
                    sent += 1;
                }
            }
        }
        sent
    }

    /// Beacon every `interval` until `stop`.
    pub async fn run(self: Arc<Self>, mut stop: tokio::sync::watch::Receiver<bool>) {
        loop {
            self.beacon().await;
            tokio::select! {
                _ = tokio::time::sleep(self.cfg.interval) => {}
                _ = stop.changed() => return,
            }
        }
    }

    async fn recv_loop(self: Arc<Self>) {
        let mut buf = vec![0u8; 2048];
        while let Ok((n, _from)) = self.sock.recv_from(&mut buf).await {
            let w = window_now();
            let mut applied = false;
            {
                let entries = decode(&buf[..n]);
                let mut watched = self.watched.lock().expect("lock");
                for (tag, sealed) in &entries {
                    if let Some(v) = watched.get_mut(tag) {
                        if v.len() < 16 {
                            v.push(sealed.to_vec());
                        }
                    }
                }
                drop(watched);
                let mut r = self.replica.lock().expect("replica lock");
                for (tag, sealed) in entries {
                    applied |= r.open_lan_entry(w, &tag, sealed);
                }
            }
            if applied {
                self.learned.notify_waiters();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beacons_pad_and_roundtrip() {
        let e: Vec<([u8; 16], Vec<u8>)> = (0..3u8).map(|i| ([i; 16], vec![i; 200])).collect();
        let ds = encode(e);
        let all: Vec<_> = ds.iter().flat_map(|d| decode(d)).collect();
        assert_eq!(all.len(), 4, "3 entries padded to 4");
        for i in 0..3u8 {
            assert!(all.iter().any(|(t, b)| *t == [i; 16] && b.len() == 200));
        }
        assert!(decode(b"junk").is_empty());
        assert!(decode(b"KTL1\x00\x01").is_empty());
    }
}
