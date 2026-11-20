//! Relaying for others, self-sustaining (D51): a desktop helper runs a relay and announces
//! it; phones that can't connect directly find it through the normal helper upkeep, pin
//! its key, get online through it and sync through it. Nothing is configured by hand.
//! (Loopback: the helper's reachability is assumed; the checks are unit-tested.)

#![cfg(feature = "relay-server")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};
use kt_net::helper::{HelperLimits, HelperMailbox};
use kt_net::helper_dir::{announce, maintain, HelperDirectory};
use kt_net::mailbox::{Mailbox, RateBudget};
use kt_net::nostr::{NostrMailbox, NostrPolicy};
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::relay_runner::RelayForOthers;
use kt_net::sync::Syncer;
use kt_net::testrelay::TestRelay;

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-rfo-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

fn nostr(url: &str) -> Arc<NostrMailbox> {
    Arc::new(NostrMailbox::new(&[url], NostrPolicy { budget: RateBudget { per_minute: 6000.0, burst: 500 }, ..NostrPolicy::default() }))
}

struct Phone {
    s: Arc<Syncer>,
    p: Arc<P2p>,
    hmb: Arc<HelperMailbox>,
    mb: Arc<NostrMailbox>,
    dir: HelperDirectory,
}

/// A phone that can't connect directly to anyone (relay only) and has no relays of its own.
async fn phone(r: Replica, relay: &str) -> Phone {
    let rep = Arc::new(Mutex::new(r));
    let cfg = P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), relay_only: true, ..Default::default() };
    let p = P2p::start(Syncer::new(rep.clone(), vec![]), cfg).await.unwrap();
    let hmb = Arc::new(HelperMailbox::new(p.clone(), vec![]));
    let s = Syncer::new(rep, vec![hmb.clone() as Arc<dyn Mailbox>]);
    kt_net::helper_dir::install_resolver(&s, &hmb);
    Phone { s, p, hmb, mb: nostr(relay), dir: HelperDirectory::new(None) }
}

fn group(a: &mut Replica, b: &mut Replica) -> (GroupId, DocId) {
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    a.doc(&g, &d).unwrap().get_map("tasks").insert("t0", "start").unwrap();
    a.commit(&g, &d).unwrap();
    a.add_member(&g, b.device_public()).unwrap();
    b.accept_group(g);
    for m in a.export_group(&g).unwrap() {
        let _ = b.receive(&m);
    }
    (g, d)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phones_find_a_desktop_relay_and_sync_through_it() {
    let nostr_relay = TestRelay::start(None).await.unwrap();
    // A stranger's desktop: helps others and relays for them.
    let dcfg = P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), helper: Some(HelperLimits { base_pow_bits: 4, ..Default::default() }), ..Default::default() };
    let desk = P2p::start(Syncer::new(Arc::new(Mutex::new(replica("desk"))), vec![]), dcfg).await.unwrap();
    let rfo = RelayForOthers::start(&desk, "127.0.0.1".parse().unwrap(), Some("127.0.0.1".parse().unwrap())).await.unwrap();
    let url = rfo.announced_url().expect("announced");
    assert_eq!(desk.helper_ref().unwrap().relay.as_deref(), Some(url.as_str()));
    // Its own relay becomes its home relay; then it announces (with that address).
    tokio::time::timeout(Duration::from_secs(10), desk.endpoint().online()).await.expect("desktop online through its own relay");
    assert_eq!(desk.where_data().relays, vec![format!("{url}/")]);
    assert!(announce(&HelperMailbox::new(desk.clone(), vec![]), &nostr(&nostr_relay.url())).await > 0);

    // Two phones in a group, relay only, no relays configured.
    let (mut ra, mut rb) = (replica("pa"), replica("pb"));
    let (g, d) = group(&mut ra, &mut rb);
    let a = phone(ra, &nostr_relay.url()).await;
    let b = phone(rb, &nostr_relay.url()).await;
    let desk_id = desk.endpoint().id();
    for ph in [&a, &b] {
        // Still offline here (no relay yet): calls to the desktop fail, which must not
        // count against it, or the phone would drop the relay it needs to get online.
        let rep = maintain(&ph.s, &ph.hmb, &ph.mb, &ph.dir).await;
        assert!(!ph.hmb.is_failing(&desk_id), "our own offline start isn't the helper's fault");
        assert_eq!(rep.relays_in_use, 1, "the desktop's relay, found through its announcement: {rep:?}");
        tokio::time::timeout(Duration::from_secs(10), ph.p.endpoint().online()).await.expect("online through the desktop's relay");
        ph.p.publish_whereabouts();
        assert_eq!(ph.p.where_data().relays, vec![format!("{url}/")]);
    }
    // Whereabouts travel in the group's slots (at the desktop, as a helper); then direct
    // P2P between the phones, through the desktop's relay.
    for _ in 0..3 {
        for ph in [&a, &b] {
            maintain(&ph.s, &ph.hmb, &ph.mb, &ph.dir).await;
            let (w, _) = ph.s.sync_once().await;
            assert_eq!(w.failed, 0, "slots stored at the desktop, through its relay: {w:?}");
        }
    }
    {
        let r = a.s.replica();
        let mut r = r.lock().unwrap();
        r.doc(&g, &d).unwrap().get_map("tasks").insert("t1", "through the desktop's relay").unwrap();
        r.commit(&g, &d).unwrap();
    }
    let rep = a.p.sync_round().await;
    assert_eq!(rep.peers_reached, 1, "{rep:?}");
    let got = format!("{:?}", b.s.replica().lock().unwrap().doc(&g, &d).unwrap().get_deep_value());
    assert!(got.contains("through the desktop's relay"), "{got}");

    // The desktop stops relaying: it's no longer announced.
    rfo.stop(&desk).await;
    assert_eq!(desk.helper_ref().unwrap().relay, None);
}

/// Helper relays are probed before use: a dead one and an impostor (a real relay whose
/// key doesn't match the pin) are skipped, and the next candidate takes their place.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unreachable_or_impostor_relays_are_skipped() {
    use kt_net::helper_relay::{HelperRelay, HelperRelayConfig};
    let genuine_secret = [31u8; 32];
    let genuine_key = *iroh::SecretKey::from_bytes(&genuine_secret).public().as_bytes();
    let genuine = HelperRelay::start(HelperRelayConfig { tls_secret: Some(genuine_secret), ..Default::default() }).await.unwrap();
    let impostor = HelperRelay::start(HelperRelayConfig { tls_secret: Some([32u8; 32]), ..Default::default() }).await.unwrap();
    let nostr_relay = TestRelay::start(None).await.unwrap();
    let ph = phone(replica("probe"), &nostr_relay.url()).await;
    let dead = ("https://127.0.0.1:9".to_string(), [33u8; 32]);
    let fake = (impostor.url().unwrap(), genuine_key); // claims the genuine helper's key
    let real = (genuine.url().unwrap(), genuine_key);
    let used = ph.p.sync_helper_relays(vec![dead, fake, real.clone()], 1).await;
    assert_eq!(used, 1);
    assert_eq!(ph.p.helper_relays(), vec![real.0.clone()], "only the genuine relay is used");
    tokio::time::timeout(Duration::from_secs(10), ph.p.endpoint().online()).await.expect("online through it");
}

