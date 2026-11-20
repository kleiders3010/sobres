//! Peer-to-peer sync over iroh (DESIGN §6, D49).
//!
//! - Each device runs one iroh endpoint. Its address (endpoint ID, relay URLs, IPs) is
//!   published to group members only, as signed whereabouts inside sealed slots.
//! - **Relays:** a custom relay map (helper relays, select relays; D49: IPv4 URLs), empty
//!   by default. n0's relays only if explicitly enabled (best-effort layer, D50).
//! - **Protocol** (ALPN `kt/slots/1`), one bidirectional stream per exchange:
//!   1. dialer → `Hello { topics, have }` (its read topics for one group + store summary)
//!   2. responder → `Reply { slots newer than have, its own have }`
//!   3. dialer → `Push { slots the responder lacks }`
//!   4. responder → `Ack { applied }` once it has applied them: delivery confirmation
//!      (the "On Alex's phone" state of D18), not just "sent".
//!
//!   Both sides then open what's new through the same path as mailbox slots (sealed,
//!   individually verified, sandboxed imports). Because the store holds every member's
//!   latest slot, a member device relays the others' changes ("relay through a member").
//! - Only member devices are served, and only for topics the responder itself reads: the
//!   endpoint must be listed in a member's whereabouts, or the dialer's `Hello` carries a
//!   device-signed whereabouts record naming this very endpoint (iroh authenticates
//!   endpoint IDs, so the proof can't be replayed from another endpoint).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{presets, Connection, PortmapperConfig, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey, TransportAddr};
use kt_engine::GroupId;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::mailbox::SlotRead;
use crate::slotstore::Summary;
use crate::sync::{ReadReport, Syncer};

pub const ALPN: &[u8] = b"kt/slots/1";
/// Invite records by lookup key (D55 layers 1 and 3: the inviter's and members' devices).
pub const INVITE_ALPN: &[u8] = b"kt/invite/1";
/// Blobs by ID ([`crate::blobs`]).
pub const BLOB_ALPN: &[u8] = b"kt/blob/1";
const MAX_FRAME: usize = 32 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Helper relays in use are probed again this often.
pub const RELAY_RECHECK: Duration = Duration::from_secs(600);
/// How long an unreachable helper relay is skipped before being tried again.
pub const RELAY_DEAD_FOR: Duration = Duration::from_secs(6 * 3600);

#[derive(Clone, Debug, Default)]
pub struct P2pConfig {
    /// Custom relays (helper / select relays), e.g. "https://203.0.113.5".
    pub relays: Vec<String>,
    /// Also use n0's public relays (best-effort, D50), next to our own.
    pub n0_relays: bool,
    /// Also publish and look up addresses through n0's DNS (best-effort, DESIGN §6: the
    /// third discovery provider next to Nostr slots and the Mainline DHT).
    pub n0_dns: bool,
    /// Bind address; `None` = all interfaces on a random port. Tests use 127.0.0.1:0.
    pub bind: Option<SocketAddr>,
    /// Ask the router for a port mapping (UPnP / NAT-PMP / PCP).
    pub portmap: bool,
    /// Token presented to the custom relays (group-token access on helper relays).
    pub relay_token: Option<String>,
    /// No direct IP paths at all, only relays (tests, USB tunnels).
    pub relay_only: bool,
    /// Help other groups by storing their slots in RAM (§7a); `None` = not helping.
    pub helper: Option<crate::helper::HelperLimits>,
}

/// What a device publishes to its groups as whereabouts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhereData {
    pub endpoint: [u8; 32],
    pub relays: Vec<String>,
    pub ips: Vec<SocketAddr>,
    /// This device's Mainline DHT node, when publicly reachable: members bootstrap into
    /// the DHT through each other if the public bootstrap nodes ever disappear.
    #[serde(default)]
    pub dht: Option<SocketAddr>,
}

impl WhereData {
    pub fn to_addr(&self) -> Option<EndpointAddr> {
        let id = EndpointId::from_bytes(&self.endpoint).ok()?;
        let mut addrs: Vec<TransportAddr> = self.ips.iter().map(|a| TransportAddr::Ip(*a)).collect();
        for r in &self.relays {
            if let Ok(u) = r.parse::<RelayUrl>() {
                addrs.push(TransportAddr::Relay(u));
            }
        }
        Some(EndpointAddr::from_parts(id, addrs))
    }
}

#[derive(Serialize, Deserialize)]
struct WireSlot {
    topic: [u8; 32],
    writer: [u8; 32],
    version: u64,
    blob: Vec<u8>,
}

impl From<SlotRead> for WireSlot {
    fn from(s: SlotRead) -> Self {
        WireSlot { topic: s.topic, writer: s.writer, version: s.version, blob: s.blob }
    }
}

impl From<WireSlot> for SlotRead {
    fn from(s: WireSlot) -> Self {
        SlotRead { topic: s.topic, writer: s.writer, version: s.version, blob: s.blob }
    }
}

#[derive(Serialize, Deserialize)]
struct Hello {
    topics: Vec<[u8; 32]>,
    have: Summary,
    /// The dialer's device-signed whereabouts for the group: proves that this endpoint
    /// belongs to a current member device, even if the responder hasn't heard of it yet.
    proof: Option<Vec<u8>>,
    /// A joining device's signed join request (it holds an invite): the responder adds it
    /// if the invite is open, then serves it as a member.
    join: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
struct Reply {
    slots: Vec<WireSlot>,
    have: Summary,
}

#[derive(Serialize, Deserialize)]
struct Push {
    slots: Vec<WireSlot>,
}

#[derive(Serialize, Deserialize)]
struct Ack {
    applied: u32,
}

async fn send_frame<T: Serialize>(s: &mut SendStream, v: &T) -> Result<(), String> {
    let b = postcard::to_stdvec(v).map_err(|e| e.to_string())?;
    if b.len() > MAX_FRAME {
        return Err("frame too large".into());
    }
    s.write_all(&(b.len() as u32).to_le_bytes()).await.map_err(|e| e.to_string())?;
    s.write_all(&b).await.map_err(|e| e.to_string())
}

async fn recv_frame<T: DeserializeOwned>(r: &mut RecvStream) -> Result<T, String> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await.map_err(|e| e.to_string())?;
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err("frame too large".into());
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b).await.map_err(|e| e.to_string())?;
    postcard::from_bytes(&b).map_err(|e| e.to_string())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct P2pReport {
    pub peers_tried: usize,
    pub peers_reached: usize,
    pub sent: usize,
    /// Slots the peers confirmed they applied.
    pub confirmed: usize,
    pub received: ReadReport,
}

/// An endpoint builder with this device's network settings: relay map (n0's as one layer,
/// then the custom relays), TLS verifier with pinned helper keys, port mapping, n0 DNS and
/// bind address. Shared by the real endpoint and the measurement-only extra endpoints
/// ([`crate::measure`]), so both keep exactly the same connections.
pub(crate) fn endpoint_builder(cfg: &P2pConfig, secret: SecretKey, pins: crate::relay_tls::Pins, alpns: Vec<Vec<u8>>) -> Result<iroh::endpoint::Builder, String> {
    let mut cfgs = Vec::new();
    if cfg.n0_relays {
        // n0's public relays as one best-effort layer among ours (D50), never alone:
        // helper relays still come and go at runtime.
        use iroh::defaults::prod::*;
        for c in [default_na_east_relay(), default_na_west_relay(), default_eu_relay(), default_ap_relay()] {
            cfgs.push(Arc::new(c));
        }
    }
    for r in &cfg.relays {
        let u: RelayUrl = r.parse().map_err(|e| format!("bad relay url {r}: {e}"))?;
        let mut c = iroh::RelayConfig::new(u, None);
        if let Some(t) = &cfg.relay_token {
            c = c.with_auth_token(t);
        }
        cfgs.push(Arc::new(c));
    }
    // A custom map (not Disabled, possibly empty) so relays can be added at runtime (E10).
    let relay_mode = RelayMode::Custom(RelayMap::from_iter(cfgs));
    let mut b = Endpoint::builder(presets::Minimal)
        .ca_tls_config(crate::relay_tls::ca_tls_config(pins))
        .secret_key(secret)
        .alpns(alpns)
        .relay_mode(relay_mode);
    if !cfg.portmap {
        b = b.portmapper_config(PortmapperConfig::Disabled);
    }
    if cfg.n0_dns {
        b = b.address_lookup(iroh::address_lookup::PkarrPublisher::n0_dns()).address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns());
    }
    if cfg.relay_only {
        b = b.clear_ip_transports();
    } else if let Some(a) = cfg.bind {
        b = b.clear_ip_transports().bind_addr(a).map_err(|e| e.to_string())?;
    }
    Ok(b)
}

pub struct P2p {
    ep: Endpoint,
    syncer: Arc<Syncer>,
    conns: Mutex<HashMap<(EndpointId, Vec<u8>), Connection>>,
    helper: Option<Arc<crate::helper::HelperStore>>,
    /// Helper relays' pinned keys (by IP), checked by the endpoint's TLS verifier.
    pins: crate::relay_tls::Pins,
    /// The relay this device runs for others, when reachable (announced with it).
    own_relay: Mutex<Option<String>>,
    /// Our DHT node, when publicly reachable (shared with members in whereabouts).
    dht_entry: Mutex<Option<SocketAddr>>,
    /// Helper relays this endpoint currently uses (URL -> key, when last probed).
    used_relays: Mutex<HashMap<String, ([u8; 32], std::time::Instant)>>,
    /// Helper relays that didn't answer a probe (URL -> until when skipped).
    dead_relays: Mutex<HashMap<String, std::time::Instant>>,
    /// Helping right now (a phone helps only on Wi-Fi, and by default while charging or
    /// open: the store stays, serving pauses).
    helping: std::sync::atomic::AtomicBool,
    /// Sealed invite records we serve, by lookup key.
    invites: Mutex<HashMap<[u8; 32], Vec<u8>>>,
    /// This device's blobs, served by ID.
    blob_store: Mutex<Option<kt_engine::blobs::BlobStore>>,
    /// The settings it was started with (measurement-only extra endpoints copy them).
    cfg: P2pConfig,
}

/// How long a device keeps one network ID.
pub const ID_PERIOD_DAYS: u64 = 30;

/// The endpoint key for the period containing `now_ms`. Each device's periods start on its
/// own day (from its secret), so devices don't all change ID at once.
pub fn endpoint_seed(device_secret: &[u8], now_ms: u64) -> [u8; 32] {
    let offset = u64::from(kt_engine::crypto::hash("kt/iroh-endpoint-offset/v1", device_secret)[0]) % ID_PERIOD_DAYS;
    let period = (now_ms / 86_400_000 + offset) / ID_PERIOD_DAYS;
    kt_engine::crypto::derive(device_secret, "kt/iroh-endpoint/v2", &period.to_le_bytes()).0
}

impl P2p {
    /// Start the endpoint. Its key is derived from the device secret (one-way, so the
    /// network ID reveals nothing about the device key) and the current [`ID_PERIOD_DAYS`]
    /// period, so an old network ID stops pointing at the device (D23: rotation instead of
    /// per-group identities). Members learn the new one from sealed whereabouts.
    pub async fn start(syncer: Arc<Syncer>, cfg: P2pConfig) -> Result<Arc<P2p>, String> {
        let seed = {
            let r = syncer.replica();
            let r = r.lock().expect("replica lock");
            endpoint_seed(&r.identity().secret().to_bytes(), kt_engine::replica::now_ms())
        };
        let pins: crate::relay_tls::Pins = Default::default();
        let alpns = vec![ALPN.to_vec(), crate::helper::HELPER_ALPN.to_vec(), INVITE_ALPN.to_vec(), BLOB_ALPN.to_vec()];
        let b = endpoint_builder(&cfg, SecretKey::from_bytes(&seed), pins.clone(), alpns)?;
        let ep = b.bind().await.map_err(|e| e.to_string())?;
        let helper = cfg.helper.clone().map(crate::helper::HelperStore::new);
        let cfg = P2pConfig { helper: None, ..cfg };
        let p = Arc::new(P2p {
            ep,
            syncer,
            conns: Mutex::new(HashMap::new()),
            helper,
            pins,
            own_relay: Mutex::new(None),
            dht_entry: Mutex::new(None),
            used_relays: Mutex::new(HashMap::new()),
            dead_relays: Mutex::new(HashMap::new()),
            helping: std::sync::atomic::AtomicBool::new(true),
            invites: Mutex::new(HashMap::new()),
            blob_store: Mutex::new(None),
            cfg,
        });
        let acc = p.clone();
        tokio::spawn(async move { acc.accept_loop().await });
        Ok(p)
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.ep
    }

    pub fn where_data(&self) -> WhereData {
        let a = self.ep.addr();
        let mut relays = Vec::new();
        let mut ips = Vec::new();
        for t in a.addrs.iter() {
            match t {
                TransportAddr::Relay(u) => relays.push(u.to_string()),
                TransportAddr::Ip(s) => ips.push(*s),
                _ => {}
            }
        }
        WhereData { endpoint: *self.ep.id().as_bytes(), relays, ips, dht: *self.dht_entry.lock().expect("lock") }
    }

    /// Put our current address into our slots (members learn it on their next read).
    pub fn publish_whereabouts(&self) {
        let data = postcard::to_stdvec(&self.where_data()).ok();
        self.syncer.replica().lock().expect("replica lock").set_whereabouts(data);
    }

    /// Current members' endpoint addresses per group.
    fn members(&self) -> Vec<(GroupId, EndpointAddr)> {
        let r = self.syncer.replica();
        let r = r.lock().expect("replica lock");
        let mut out = Vec::new();
        for g in r.groups() {
            for w in r.member_whereabouts(&g) {
                if let Some(a) = postcard::from_bytes::<WhereData>(&w.data).ok().and_then(|d| d.to_addr()) {
                    out.push((g, a));
                }
            }
        }
        out
    }

    fn topics_of(&self, g: &GroupId) -> Vec<[u8; 32]> {
        let r = self.syncer.replica();
        let r = r.lock().expect("replica lock");
        r.read_topics().into_iter().filter(|t| t.group == *g).map(|t| t.topic).collect()
    }

    async fn connection(&self, addr: EndpointAddr) -> Result<Connection, String> {
        self.connection_alpn(addr, ALPN).await
    }

    /// A (cached) connection to `addr` for protocol `alpn`.
    pub(crate) async fn connection_alpn(&self, addr: EndpointAddr, alpn: &[u8]) -> Result<Connection, String> {
        let key = (addr.id, alpn.to_vec());
        if let Some(c) = self.conns.lock().expect("lock").get(&key) {
            if c.close_reason().is_none() {
                return Ok(c.clone());
            }
        }
        let c = tokio::time::timeout(CONNECT_TIMEOUT, self.ep.connect(addr, alpn))
            .await
            .map_err(|_| "connect timeout".to_string())?
            .map_err(|e| e.to_string())?;
        self.conns.lock().expect("lock").insert(key, c.clone());
        Ok(c)
    }

    /// Serve this device's blobs to anyone who asks by ID.
    pub fn set_blob_store(&self, store: kt_engine::blobs::BlobStore) {
        *self.blob_store.lock().expect("lock") = Some(store);
    }

    /// Member devices' endpoints, one per device.
    pub fn member_addrs(&self) -> Vec<EndpointAddr> {
        let me = self.ep.id();
        let mut seen = std::collections::HashSet::new();
        self.members().into_iter().map(|(_, a)| a).filter(|a| a.id != me && seen.insert(a.id)).collect()
    }

    /// Ask a device for a blob; only the exact bytes are accepted.
    pub async fn fetch_blob(&self, addr: EndpointAddr, id: [u8; 32]) -> Option<Vec<u8>> {
        let conn = self.connection_alpn(addr, BLOB_ALPN).await.ok()?;
        let (mut s, mut r) = conn.open_bi().await.ok()?;
        send_frame(&mut s, &id).await.ok()?;
        let _ = s.finish();
        let b = recv_frame::<Option<Vec<u8>>>(&mut r).await.ok().flatten()?;
        (kt_engine::blobs::blob_id(&b) == id).then_some(b)
    }

    /// Serve these sealed invite records (lookup key -> record) to anyone who asks by key.
    pub fn set_invite_board(&self, records: Vec<([u8; 32], Vec<u8>)>) {
        *self.invites.lock().expect("lock") = records.into_iter().collect();
    }

    /// Ask a device for the invite record under `lookup`.
    pub async fn fetch_invite(&self, addr: EndpointAddr, lookup: [u8; 32]) -> Option<Vec<u8>> {
        let conn = self.connection_alpn(addr, INVITE_ALPN).await.ok()?;
        let (mut s, mut r) = conn.open_bi().await.ok()?;
        send_frame(&mut s, &lookup).await.ok()?;
        let _ = s.finish();
        recv_frame::<Option<Vec<u8>>>(&mut r).await.ok().flatten()
    }

    /// Joining: hand our join request for `g` to a member device directly, then sync with
    /// it as a member. Returns true once we're in.
    pub async fn join_via(&self, g: GroupId, addr: EndpointAddr) -> bool {
        let (join, topics) = {
            let r = self.syncer.replica();
            let r = r.lock().expect("replica lock");
            (r.pending_join_request(&g), r.read_topics().into_iter().filter(|t| t.group == g).map(|t| t.topic).collect::<Vec<_>>())
        };
        let Some(join) = join else { return false };
        let res = async {
            let conn = self.connection(addr.clone()).await?;
            let (mut s, mut r) = conn.open_bi().await.map_err(|e| e.to_string())?;
            let store = self.syncer.store();
            send_frame(&mut s, &Hello { topics: topics.clone(), have: store.summary(&topics), proof: None, join: Some(join) }).await?;
            let reply: Reply = recv_frame(&mut r).await?;
            send_frame(&mut s, &Push { slots: vec![] }).await?;
            let _ = s.finish();
            let incoming: Vec<SlotRead> = reply.slots.into_iter().filter(|x| topics.contains(&x.topic)).map(SlotRead::from).collect();
            self.syncer.apply_incoming(incoming).await;
            let _ack: Ack = recv_frame(&mut r).await?;
            Ok::<(), String>(())
        }
        .await;
        if res.is_err() {
            return false;
        }
        // Now a member (if accepted): a normal exchange with the same device brings the rest.
        self.publish_whereabouts();
        let topics = self.topics_of(&g);
        let _ = self.exchange(g, addr, topics).await;
        let r = self.syncer.replica();
        let r = r.lock().expect("replica lock");
        r.epochs(&g).is_some_and(|e| e.is_current_member(&r.device_id()))
    }

    /// Our helper store, if helping right now.
    pub fn helper(&self) -> Option<Arc<crate::helper::HelperStore>> {
        self.helper.clone().filter(|_| self.helping.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Use a helper's relay (`https://IP:port` with the helper's endpoint key from its
    /// signed announcement, or plain `http://` on a LAN). It joins this endpoint's relay
    /// map; iroh picks among its relays by latency.
    pub async fn use_helper_relay(&self, url: &str, key: [u8; 32]) -> Result<(), String> {
        let u: RelayUrl = url.parse().map_err(|e| format!("bad relay url {url}: {e}"))?;
        let host = u.host_str().map(str::to_string).ok_or("relay url without a host")?;
        let ip: std::net::IpAddr = host.trim_start_matches('[').trim_end_matches(']').parse().map_err(|_| "helper relays are addressed by IP")?;
        self.pins.write().expect("pins lock").insert(ip, key);
        self.ep.insert_relay(u.clone(), Arc::new(iroh::RelayConfig::new(u, None))).await;
        Ok(())
    }

    /// Connected to a home relay: this device can reach others, so failures to reach a
    /// peer are the peer's.
    pub fn is_online(&self) -> bool {
        self.ep.addr().addrs.iter().any(|a| matches!(a, TransportAddr::Relay(_)))
    }

    /// Is a helper relay reachable right now? An HTTPS `GET /ping` (the relay protocol's
    /// probe), verified against the helper's pinned key: an outside-in check.
    pub async fn probe_helper_relay(&self, url: &str, key: [u8; 32]) -> bool {
        let Ok(u) = url.parse::<RelayUrl>() else { return false };
        let Some(ip) = u.host_str().and_then(|h| h.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>().ok()) else { return false };
        self.pins.write().expect("pins lock").insert(ip, key);
        let Ok(client) = reqwest::Client::builder()
            .use_preconfigured_tls(crate::relay_tls::client_config(self.pins.clone()))
            .timeout(Duration::from_secs(5))
            .build()
        else {
            return false;
        };
        let ping = format!("{}/ping", url.trim_end_matches('/'));
        client.get(ping).send().await.is_ok_and(|r| r.status().is_success())
    }

    /// Use up to `max` of these helper relays (URL, key, best first), each probed first
    /// (and re-probed every [`RELAY_RECHECK`]); one that doesn't answer is skipped for
    /// [`RELAY_DEAD_FOR`] and the next candidate takes its place. Returns how many are in use.
    pub async fn sync_helper_relays(&self, candidates: Vec<(String, [u8; 32])>, max: usize) -> usize {
        let now = std::time::Instant::now();
        let current = self.used_relays.lock().expect("lock").clone();
        self.dead_relays.lock().expect("lock").retain(|_, until| *until > now);
        let mut next: HashMap<String, ([u8; 32], std::time::Instant)> = HashMap::new();
        for (url, key) in candidates {
            if next.len() >= max {
                break;
            }
            if self.dead_relays.lock().expect("lock").contains_key(&url) {
                continue;
            }
            let fresh = current.get(&url).is_some_and(|(k, checked)| *k == key && now.duration_since(*checked) < RELAY_RECHECK);
            if fresh {
                let v = current[&url];
                next.insert(url, v);
                continue;
            }
            if self.probe_helper_relay(&url, key).await {
                if !current.get(&url).is_some_and(|(k, _)| *k == key) && self.use_helper_relay(&url, key).await.is_err() {
                    continue;
                }
                next.insert(url, (key, now));
            } else {
                self.dead_relays.lock().expect("lock").insert(url, now + RELAY_DEAD_FOR);
            }
        }
        for url in current.keys().filter(|u| !next.contains_key(*u)) {
            self.drop_helper_relay(url).await;
        }
        let n = next.len();
        *self.used_relays.lock().expect("lock") = next;
        n
    }

    /// Helper relays currently in use.
    pub fn helper_relays(&self) -> Vec<String> {
        self.used_relays.lock().expect("lock").keys().cloned().collect()
    }

    /// Helper relays in use with their pinned keys.
    pub fn helper_relay_keys(&self) -> Vec<(String, [u8; 32])> {
        self.used_relays.lock().expect("lock").iter().map(|(u, (k, _))| (u.clone(), *k)).collect()
    }

    /// The settings this endpoint was started with (without the helper store).
    pub fn config(&self) -> &P2pConfig {
        &self.cfg
    }

    /// Our DHT node's reachable address (`DhtDiscovery::entry_point`), shared with members.
    pub fn set_dht_entry(&self, addr: Option<SocketAddr>) {
        *self.dht_entry.lock().expect("lock") = addr;
    }

    /// The relay this device runs for others (set by the relay runner; `None` = not relaying).
    pub fn set_own_relay(&self, url: Option<String>) {
        *self.own_relay.lock().expect("lock") = url;
    }

    /// How this device appears as a helper: its addresses, and its relay if it runs one.
    pub fn helper_ref(&self) -> Option<crate::helper::HelperRef> {
        let mut h = crate::helper::HelperRef::from_addr(&self.where_data().to_addr()?);
        h.relay = self.own_relay.lock().expect("lock").clone();
        Some(h)
    }

    /// Stop using a helper's relay.
    pub async fn drop_helper_relay(&self, url: &str) {
        if let Ok(u) = url.parse::<RelayUrl>() {
            self.ep.remove_relay(&u).await;
        }
    }

    /// Our endpoint secret (a helper relay's certificate is made from it).
    pub fn endpoint_secret(&self) -> [u8; 32] {
        self.ep.secret_key().to_bytes()
    }

    /// Pause or resume helping others (held slots stay; strangers are turned away and
    /// nothing is announced while paused).
    pub fn set_helping(&self, on: bool) {
        self.helping.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Exchange slots with every reachable member device, all at once (an offline member
    /// costs one connect timeout in parallel, not one each).
    pub async fn sync_round(&self) -> P2pReport {
        // Make sure our own current slots are in the store.
        self.syncer.write_round().await;
        let jobs: Vec<(GroupId, EndpointAddr, Vec<[u8; 32]>)> =
            self.members().into_iter().map(|(g, a)| (g, a, self.topics_of(&g))).filter(|(_, _, t)| !t.is_empty()).collect();
        let mut report = P2pReport { peers_tried: jobs.len(), ..Default::default() };
        let results = futures_util::future::join_all(jobs.into_iter().map(|(g, a, t)| self.exchange(g, a, t))).await;
        for (sent, confirmed, rr) in results.into_iter().flatten() {
            report.peers_reached += 1;
            report.sent += sent;
            report.confirmed += confirmed;
            report.received.slots_opened += rr.slots_opened;
            report.received.messages += rr.messages;
            report.received.rejected += rr.rejected;
            report.received.already_seen += rr.already_seen;
        }
        report
    }

    async fn exchange(&self, g: GroupId, addr: EndpointAddr, topics: Vec<[u8; 32]>) -> Result<(usize, usize, ReadReport), String> {
        let conn = self.connection(addr).await?;
        let (mut s, mut r) = conn.open_bi().await.map_err(|e| e.to_string())?;
        let store = self.syncer.store();
        let proof = self.syncer.replica().lock().expect("replica lock").own_whereabouts_record(&g);
        send_frame(&mut s, &Hello { topics: topics.clone(), have: store.summary(&topics), proof, join: None }).await?;
        let reply: Reply = recv_frame(&mut r).await?;
        let push: Vec<WireSlot> = store.newer_than(&topics, &reply.have).into_iter().map(WireSlot::from).collect();
        let sent = push.len();
        send_frame(&mut s, &Push { slots: push }).await?;
        let _ = s.finish();
        let incoming: Vec<SlotRead> = reply.slots.into_iter().filter(|x| topics.contains(&x.topic)).map(SlotRead::from).collect();
        let rr = self.syncer.apply_incoming(incoming).await;
        let ack: Ack = recv_frame(&mut r).await?;
        Ok((sent, ack.applied as usize, rr))
    }

    async fn accept_loop(self: Arc<Self>) {
        while let Some(inc) = self.ep.accept().await {
            let me = self.clone();
            tokio::spawn(async move {
                let Ok(conn) = inc.await else { return };
                let remote = conn.remote_id();
                if conn.alpn() == BLOB_ALPN {
                    while let Ok((mut s, mut r)) = conn.accept_bi().await {
                        let store = me.blob_store.lock().expect("lock").clone();
                        tokio::spawn(async move {
                            if let Ok(id) = recv_frame::<[u8; 32]>(&mut r).await {
                                let _ = send_frame(&mut s, &store.and_then(|st| st.get(&id))).await;
                                let _ = s.finish();
                            }
                        });
                    }
                    return;
                }
                if conn.alpn() == INVITE_ALPN {
                    while let Ok((mut s, mut r)) = conn.accept_bi().await {
                        let board = me.invites.lock().expect("lock").clone();
                        tokio::spawn(async move {
                            if let Ok(lookup) = recv_frame::<[u8; 32]>(&mut r).await {
                                let _ = send_frame(&mut s, &board.get(&lookup).cloned()).await;
                                let _ = s.finish();
                            }
                        });
                    }
                    return;
                }
                if conn.alpn() == crate::helper::HELPER_ALPN {
                    // Helping strangers: no membership needed, the store enforces its rules.
                    let Some(h) = me.helper() else {
                        conn.close(2u32.into(), b"not helping");
                        return;
                    };
                    while let Ok((s, r)) = conn.accept_bi().await {
                        let h = h.clone();
                        let me3 = me.clone();
                        tokio::spawn(async move {
                            if let Ok(Some((put, fanout))) = h.serve(s, r).await {
                                crate::helper::forward(&me3, &put, &fanout).await;
                            }
                        });
                    }
                    return;
                }
                while let Ok((s, r)) = conn.accept_bi().await {
                    let me2 = me.clone();
                    tokio::spawn(async move {
                        let _ = me2.serve(remote, s, r).await;
                    });
                }
            });
        }
    }

    /// Groups whose members may use this connection: known member endpoints, or a valid
    /// membership proof for this very endpoint.
    fn allowed_topics(&self, remote: EndpointId, proof: Option<&[u8]>) -> Vec<[u8; 32]> {
        let mut groups: Vec<GroupId> = self.members().into_iter().filter(|(_, a)| a.id == remote).map(|(g, _)| g).collect();
        if let Some(p) = proof {
            let res = self.syncer.replica().lock().expect("replica lock").accept_whereabouts_record(p);
            if let Ok((g, _device, data)) = res {
                let bound = postcard::from_bytes::<WhereData>(&data).map(|w| w.endpoint == *remote.as_bytes()).unwrap_or(false);
                if bound && !groups.contains(&g) {
                    groups.push(g);
                }
            }
        }
        groups.iter().flat_map(|g| self.topics_of(g)).collect()
    }

    async fn serve(&self, remote: EndpointId, mut s: SendStream, mut r: RecvStream) -> Result<(), String> {
        let hello: Hello = recv_frame(&mut r).await?;
        let mut joined = None;
        if let Some(j) = &hello.join {
            // Adds the device if its invite is open (checks as for a join slot); then our
            // fresh slots (with the Add epoch) go into the store for this very reply.
            let res = self.syncer.replica().lock().expect("replica lock").accept_join_request(j);
            if let Ok(g) = res {
                self.syncer.write_round().await;
                joined = Some(g);
            }
        }
        let mut allowed = self.allowed_topics(remote, hello.proof.as_deref());
        if let Some(g) = joined {
            allowed.extend(self.topics_of(&g));
        }
        if allowed.is_empty() {
            return Err("not a member".into());
        }
        let topics: Vec<[u8; 32]> = hello.topics.into_iter().filter(|t| allowed.contains(t)).collect();
        let store = self.syncer.store();
        let slots = store.newer_than(&topics, &hello.have).into_iter().map(WireSlot::from).collect();
        send_frame(&mut s, &Reply { slots, have: store.summary(&topics) }).await?;
        let push: Push = recv_frame(&mut r).await?;
        let incoming: Vec<SlotRead> = push.slots.into_iter().filter(|x| topics.contains(&x.topic)).map(SlotRead::from).collect();
        let n = incoming.len() as u32;
        let rr = self.syncer.apply_incoming(incoming).await;
        // Applied (or already had): new plus duplicates, minus nothing rejected per slot.
        let applied = n.min((rr.slots_opened + rr.already_seen) as u32);
        send_frame(&mut s, &Ack { applied }).await?;
        let _ = s.finish();
        Ok(())
    }

    /// Before going offline as a helper: pass every held slot to one other online helper of
    /// its set (§7a handoff). Returns how many slots found a new home.
    pub async fn handoff(&self) -> usize {
        let Some(h) = &self.helper else { return 0 };
        let mut moved = 0;
        for (put, set) in h.all() {
            for target in set.iter().filter(|r| r.endpoint != *self.ep.id().as_bytes()) {
                if crate::helper::forward(self, &put, std::slice::from_ref(target)).await > 0 {
                    moved += 1;
                    break;
                }
            }
        }
        moved
    }

    /// Close gracefully, but never hang on a peer that doesn't answer the close.
    pub async fn close(&self) {
        for c in self.conns.lock().expect("lock").drain().map(|(_, c)| c) {
            c.close(0u32.into(), b"bye");
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), self.ep.close()).await;
    }
}

/// Fuzzing (DESIGN §9): frames from any peer that connects.
#[cfg(test)]
mod fuzz_tests {
    use super::*;

    #[test]
    fn the_network_id_rotates_by_period() {
        let secret = [7u8; 64];
        let day = 86_400_000u64;
        let t0 = 1_800_000_000_000u64;
        let ids: Vec<[u8; 32]> = (0..70).map(|d| endpoint_seed(&secret, t0 + d * day)).collect();
        let changes = ids.windows(2).filter(|w| w[0] != w[1]).count();
        assert!((2..=3).contains(&changes), "about one change per {ID_PERIOD_DAYS} days: {changes} in 70 days");
        assert_eq!(endpoint_seed(&secret, t0), endpoint_seed(&secret, t0 + 1000), "stable within a period");
        assert_ne!(endpoint_seed(&secret, t0), endpoint_seed(&[8u8; 64], t0), "per device");
    }

    #[test]
    fn frames_and_addresses() {
        let slot = || WireSlot { topic: [1; 32], writer: [2; 32], version: 3, blob: b"sealed".to_vec() };
        let w = WhereData { endpoint: *iroh::SecretKey::from_bytes(&[5; 32]).public().as_bytes(), relays: vec!["https://relay.example/".into()], ips: vec!["192.0.2.9:7".parse().unwrap()], dht: Some("192.0.2.9:6881".parse().unwrap()) };
        let seeds = vec![
            postcard::to_stdvec(&Hello { topics: vec![[1; 32]], have: vec![([1; 32], [2; 32], 3)], proof: Some(b"proof".to_vec()), join: None }).unwrap(),
            postcard::to_stdvec(&Reply { slots: vec![slot()], have: vec![] }).unwrap(),
            postcard::to_stdvec(&Push { slots: vec![slot(), slot()] }).unwrap(),
            postcard::to_stdvec(&Ack { applied: 2 }).unwrap(),
            postcard::to_stdvec(&w).unwrap(),
        ];
        kt_fuzz::run("p2p-frames", &seeds, 2.0, |b| {
            let _ = postcard::from_bytes::<Hello>(b);
            let _ = postcard::from_bytes::<Reply>(b).map(|r| r.slots.into_iter().map(SlotRead::from).count());
            let _ = postcard::from_bytes::<Push>(b);
            let _ = postcard::from_bytes::<Ack>(b);
            let _ = postcard::from_bytes::<WhereData>(b).ok().and_then(|w| w.to_addr());
        });
    }
}
