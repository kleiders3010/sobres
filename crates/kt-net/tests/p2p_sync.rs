//! Peer-to-peer sync over iroh (loopback only: no firewall prompts, no internet).
//! Whereabouts travel in sealed slots via a local Nostr relay; after that, devices sync
//! directly, relay each other's slots, and refuse non-members.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};
use kt_net::mailbox::{Mailbox, RateBudget};
use kt_net::nostr::{NostrMailbox, NostrPolicy};
use kt_net::p2p::{P2p, P2pConfig, ALPN};
use kt_net::sync::Syncer;
use kt_net::testrelay::{RelayKnobs, TestRelay};

fn sandbox() -> SandboxConfig {
    SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) }
}

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-p2p-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    Replica::open(&root.join("identity"), &root.join("data"), sandbox()).unwrap()
}

fn loopback() -> P2pConfig {
    P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), ..Default::default() }
}

struct Dev {
    s: Arc<Syncer>,
    p: Arc<P2p>,
}

async fn dev(r: Replica, relay: &str) -> Dev {
    let policy = NostrPolicy { budget: RateBudget { per_minute: 600.0, burst: 50 }, ..NostrPolicy::default() };
    let mb = Arc::new(NostrMailbox::new(&[relay], policy));
    let s = Syncer::new(Arc::new(Mutex::new(r)), vec![mb as Arc<dyn Mailbox>]);
    let p = P2p::start(s.clone(), loopback()).await.unwrap();
    p.publish_whereabouts();
    Dev { s, p }
}

fn set(d: &Dev, g: &GroupId, doc: &DocId, k: &str, v: &str) {
    let r = d.s.replica();
    let mut r = r.lock().unwrap();
    r.doc(g, doc).unwrap().get_map("tasks").insert(k, v).unwrap();
    r.commit(g, doc).unwrap();
}

fn value(d: &Dev, g: &GroupId, doc: &DocId) -> String {
    let r = d.s.replica();
    let r = r.lock().unwrap();
    format!("{:?}", r.doc(g, doc).map(|x| x.get_deep_value()))
}

fn group(owner: &mut Replica, others: &mut [&mut Replica]) -> (GroupId, DocId) {
    let g = owner.create_group().unwrap();
    let d = owner.create_doc(&g).unwrap();
    owner.doc(&g, &d).unwrap().get_map("tasks").insert("t0", "start").unwrap();
    owner.commit(&g, &d).unwrap();
    for o in others.iter_mut() {
        owner.add_member(&g, o.device_public()).unwrap();
        o.accept_group(g);
        for m in owner.export_group(&g).unwrap() {
            let _ = o.receive(&m);
        }
    }
    let _ = owner.outbox();
    (g, d)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn devices_sync_directly_after_learning_addresses_from_slots() {
    let relay = TestRelay::start(None).await.unwrap();
    let (mut ra, mut rb) = (replica("a"), replica("b"));
    let (g, d) = group(&mut ra, &mut [&mut rb]);
    let a = dev(ra, &relay.url()).await;
    let b = dev(rb, &relay.url()).await;
    // Whereabouts travel over Nostr once.
    for _ in 0..2 {
        a.s.sync_once().await;
        b.s.sync_once().await;
    }
    // From now on the relay is gone: only P2P.
    relay.set_knobs(RelayKnobs { offline: true, ..Default::default() });
    set(&a, &g, &d, "t1", "direct from A");
    let rep = a.p.sync_round().await;
    assert_eq!(rep.peers_reached, 1, "{rep:?}");
    assert!(rep.confirmed >= 1, "B confirmed it applied A's slot: {rep:?}");
    assert!(value(&b, &g, &d).contains("direct from A"), "pushed to B");
    set(&b, &g, &d, "t2", "direct from B");
    b.p.sync_round().await;
    assert!(value(&a, &g, &d).contains("direct from B"));
    // Nothing new: nothing sent.
    let rep = a.p.sync_round().await;
    assert_eq!(rep.sent, 0);
    assert_eq!(rep.received.slots_opened, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_relays_changes_between_devices_that_never_meet() {
    let relay = TestRelay::start(None).await.unwrap();
    let (mut ra, mut rb, mut rc) = (replica("ma"), replica("mb"), replica("mc"));
    let (g, d) = group(&mut ra, &mut [&mut rb, &mut rc]);
    let a = dev(ra, &relay.url()).await;
    let b = dev(rb, &relay.url()).await;
    let c = dev(rc, &relay.url()).await;
    for _ in 0..2 {
        for x in [&a, &b, &c] {
            x.s.sync_once().await;
        }
    }
    relay.set_knobs(RelayKnobs { offline: true, ..Default::default() });
    // A edits and syncs with B only, then goes away.
    set(&a, &g, &d, "t1", "from A via B");
    a.p.sync_round().await;
    a.p.close().await;
    assert!(value(&b, &g, &d).contains("from A via B"));
    // C comes online later and syncs with B (A is gone).
    let rep = c.p.sync_round().await;
    assert!(rep.peers_reached >= 1, "{rep:?}");
    assert!(value(&c, &g, &d).contains("from A via B"), "B relayed A's slot to C");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn non_members_are_refused() {
    let relay = TestRelay::start(None).await.unwrap();
    let (mut ra, mut rb) = (replica("na"), replica("nb"));
    let (_g, _d) = group(&mut ra, &mut [&mut rb]);
    let a = dev(ra, &relay.url()).await;
    let b = dev(rb, &relay.url()).await;
    for _ in 0..2 {
        a.s.sync_once().await;
        b.s.sync_once().await;
    }
    // An outsider who somehow learned A's address.
    let x = dev(replica("nx"), &relay.url()).await;
    let a_addr = a.p.where_data().to_addr().unwrap();
    let conn = x.p.endpoint().connect(a_addr, ALPN).await.unwrap();
    let res = async {
        let (mut s, mut r) = conn.open_bi().await.map_err(|e| e.to_string())?;
        s.write_all(&[0, 0, 0, 0]).await.map_err(|e| e.to_string())?;
        let mut buf = [0u8; 4];
        r.read_exact(&mut buf).await.map_err(|e| e.to_string())
    };
    let out = tokio::time::timeout(Duration::from_secs(5), res).await;
    assert!(!matches!(out, Ok(Ok(()))), "outsider must get nothing");
}

async fn dev_with(r: Replica, relay: &str, cfg: P2pConfig) -> Dev {
    let policy = NostrPolicy { budget: RateBudget { per_minute: 600.0, burst: 50 }, ..NostrPolicy::default() };
    let mb = Arc::new(NostrMailbox::new(&[relay], policy));
    let s = Syncer::new(Arc::new(Mutex::new(r)), vec![mb as Arc<dyn Mailbox>]);
    let p = P2p::start(s.clone(), cfg).await.unwrap();
    Dev { s, p }
}

/// Wait until the endpoint is connected to its home relay, then publish its address.
async fn online(d: &Dev) {
    let _ = tokio::time::timeout(Duration::from_secs(10), d.p.endpoint().online()).await;
    d.p.publish_whereabouts();
}

#[cfg(feature = "relay-server")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn devices_without_direct_paths_sync_through_a_helper_relay() {
    use kt_net::helper_relay::{HelperRelay, HelperRelayConfig};
    let helper = HelperRelay::start(HelperRelayConfig::default()).await.unwrap();
    let url = helper.url().unwrap();
    let nostr = TestRelay::start(None).await.unwrap();
    let (mut ra, mut rb) = (replica("ra"), replica("rb"));
    let (g, d) = group(&mut ra, &mut [&mut rb]);
    let cfg = P2pConfig { relays: vec![url.clone()], relay_only: true, ..Default::default() };
    let a = dev_with(ra, &nostr.url(), cfg.clone()).await;
    let b = dev_with(rb, &nostr.url(), cfg).await;
    online(&a).await;
    online(&b).await;
    assert!(a.p.where_data().ips.is_empty() && !a.p.where_data().relays.is_empty(), "relay only: {:?}", a.p.where_data());
    for _ in 0..2 {
        a.s.sync_once().await;
        b.s.sync_once().await;
    }
    nostr.set_knobs(RelayKnobs { offline: true, ..Default::default() });
    set(&a, &g, &d, "t1", "via helper relay");
    let rep = a.p.sync_round().await;
    assert_eq!(rep.peers_reached, 1, "{rep:?}");
    assert!(value(&b, &g, &d).contains("via helper relay"));
}

#[cfg(feature = "relay-server")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_gated_helper_relay_refuses_others() {
    use kt_net::helper_relay::{HelperRelay, HelperRelayConfig};
    let helper = HelperRelay::start(HelperRelayConfig { tokens: Some(["group-secret".to_string()].into()), ..Default::default() }).await.unwrap();
    let url = helper.url().unwrap();
    let nostr = TestRelay::start(None).await.unwrap();
    let good = dev_with(replica("tg"), &nostr.url(), P2pConfig { relays: vec![url.clone()], relay_only: true, relay_token: Some("group-secret".into()), ..Default::default() }).await;
    let bad = dev_with(replica("tb"), &nostr.url(), P2pConfig { relays: vec![url], relay_only: true, relay_token: Some("wrong".into()), ..Default::default() }).await;
    let ok = tokio::time::timeout(Duration::from_secs(10), good.p.endpoint().online()).await.is_ok();
    let refused = tokio::time::timeout(Duration::from_secs(5), bad.p.endpoint().online()).await.is_err();
    assert!(ok, "token holder gets online through the helper relay");
    assert!(refused, "a wrong token never gets a home relay");
}

/// Helper relays over TLS without a CA (relay_tls): the relay's certificate is made from
/// its endpoint key; devices add it at runtime with that key pinned and sync through it.
#[cfg(feature = "relay-server")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pinned_tls_helper_relay_added_at_runtime() {
    use kt_net::helper_relay::{HelperRelay, HelperRelayConfig};
    let secret = [11u8; 32];
    let key = *iroh::SecretKey::from_bytes(&secret).public().as_bytes();
    let helper = HelperRelay::start(HelperRelayConfig { tls_secret: Some(secret), ..Default::default() }).await.unwrap();
    let url = helper.url().unwrap();
    assert!(url.starts_with("https://127.0.0.1:"), "{url}");
    let nostr = TestRelay::start(None).await.unwrap();
    let (mut ra, mut rb) = (replica("pa"), replica("pb"));
    let (g, d) = group(&mut ra, &mut [&mut rb]);
    // No relays at start: the helper's relay comes from its announcement, at runtime.
    let cfg = P2pConfig { relay_only: true, ..Default::default() };
    let a = dev_with(ra, &nostr.url(), cfg.clone()).await;
    let b = dev_with(rb, &nostr.url(), cfg).await;
    a.p.use_helper_relay(&url, key).await.unwrap();
    b.p.use_helper_relay(&url, key).await.unwrap();
    online(&a).await;
    online(&b).await;
    assert_eq!(a.p.where_data().relays, vec![format!("{url}/")], "home relay is the pinned helper relay");
    for _ in 0..2 {
        a.s.sync_once().await;
        b.s.sync_once().await;
    }
    nostr.set_knobs(RelayKnobs { offline: true, ..Default::default() });
    set(&a, &g, &d, "t1", "via pinned TLS relay");
    let rep = a.p.sync_round().await;
    assert_eq!(rep.peers_reached, 1, "{rep:?}");
    assert!(value(&b, &g, &d).contains("via pinned TLS relay"));
}

/// The same relay with the wrong pin (an impostor at that address, or a stale key): the
/// TLS check fails and the device never gets online through it.
#[cfg(feature = "relay-server")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_pin_is_refused() {
    use kt_net::helper_relay::{HelperRelay, HelperRelayConfig};
    let helper = HelperRelay::start(HelperRelayConfig { tls_secret: Some([12u8; 32]), ..Default::default() }).await.unwrap();
    let url = helper.url().unwrap();
    let nostr = TestRelay::start(None).await.unwrap();
    let wrong = *iroh::SecretKey::from_bytes(&[13u8; 32]).public().as_bytes();
    let d = dev_with(replica("wp"), &nostr.url(), P2pConfig { relay_only: true, ..Default::default() }).await;
    d.p.use_helper_relay(&url, wrong).await.unwrap();
    let online = tokio::time::timeout(Duration::from_secs(8), d.p.endpoint().online()).await.is_ok();
    assert!(!online, "a relay whose key doesn't match the pin is never used");
}

