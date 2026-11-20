//! Whereabouts over a local Mainline DHT testnet (loopback), then P2P using them: no
//! Nostr involved at all after joining.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_net::dht::DhtDiscovery;
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;
use mainline::Testnet;

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-dht-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_find_each_other_on_the_dht_and_sync_directly() {
    let testnet = Testnet::builder(10).build().unwrap();
    let (mut ra, mut rb, outsider) = (replica("a"), replica("b"), replica("x"));
    let g = ra.create_group().unwrap();
    let d = ra.create_doc(&g).unwrap();
    ra.doc(&g, &d).unwrap().get_map("tasks").insert("t0", "start").unwrap();
    ra.commit(&g, &d).unwrap();
    ra.add_member(&g, rb.device_public()).unwrap();
    rb.accept_group(g);
    for m in ra.export_group(&g).unwrap() {
        let _ = rb.receive(&m);
    }
    let loopback = P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), ..Default::default() };
    let (sa, sb) = (Syncer::new(Arc::new(Mutex::new(ra)), vec![]), Syncer::new(Arc::new(Mutex::new(rb)), vec![]));
    let pa = P2p::start(sa.clone(), loopback.clone()).await.unwrap();
    let pb = P2p::start(sb.clone(), loopback).await.unwrap();
    pa.publish_whereabouts();
    pb.publish_whereabouts();
    let bs = Some(testnet.bootstrap.clone());
    let da = DhtDiscovery::new(sa.replica(), bs.clone(), Some(Ipv4Addr::LOCALHOST)).unwrap();
    let db = DhtDiscovery::new(sb.replica(), bs.clone(), Some(Ipv4Addr::LOCALHOST)).unwrap();
    assert_eq!(da.publish().await.published, 1);
    assert_eq!(db.publish().await.published, 1);
    let r = db.lookup().await;
    assert_eq!((r.found, r.applied), (1, 1), "B found A's sealed record");
    da.lookup().await;

    // An outsider in the same DHT can't compute the location, so it looks up nothing.
    let dx = DhtDiscovery::new(Arc::new(Mutex::new(outsider)), bs, Some(Ipv4Addr::LOCALHOST)).unwrap();
    assert_eq!(dx.lookup().await.found, 0);

    // With addresses from the DHT alone, A and B sync directly.
    sa.replica().lock().unwrap().doc(&g, &d).unwrap().get_map("tasks").insert("t1", "found via DHT").unwrap();
    sa.replica().lock().unwrap().commit(&g, &d).unwrap();
    let rep = pa.sync_round().await;
    assert_eq!(rep.peers_reached, 1, "{rep:?}");
    let v = format!("{:?}", sb.replica().lock().unwrap().doc(&g, &d).unwrap().get_deep_value());
    assert!(v.contains("found via DHT"), "{v}");
    let _ = pb;
}

fn cfg(helper: bool) -> P2pConfig {
    P2pConfig {
        bind: Some("127.0.0.1:0".parse().unwrap()),
        helper: helper.then(|| kt_net::helper::HelperLimits { base_pow_bits: 4, ..Default::default() }),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn helpers_are_found_on_the_dht_with_no_nostr_and_no_known_helper() {
    use kt_net::dht::DhtDiscovery;
    use std::net::Ipv4Addr;
    let testnet = mainline::Testnet::builder(10).build().unwrap();
    let bs = Some(testnet.bootstrap.clone());
    let day = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 / 86_400_000) as u32;
    // Three helpers post themselves in the DHT slots.
    let mut helpers = Vec::new();
    for i in 0..3 {
        let s = Syncer::new(Arc::new(Mutex::new(replica(&format!("dh{i}")))), vec![]);
        let p = P2p::start(s.clone(), cfg(true)).await.unwrap();
        let d = DhtDiscovery::new(s.replica(), bs.clone(), Some(Ipv4Addr::LOCALHOST)).unwrap();
        let me = kt_net::helper::HelperRef::from_addr(&p.where_data().to_addr().unwrap());
        assert!(d.announce_helper(&me, p.endpoint().secret_key(), day).await);
        helpers.push((p, d));
    }
    // A device that knows no helper and has no Nostr finds them.
    let rep = Arc::new(Mutex::new(replica("dx")));
    let ap = P2p::start(Syncer::new(rep.clone(), vec![]), cfg(false)).await.unwrap();
    let hmb = kt_net::helper::HelperMailbox::new(ap, vec![]);
    let s = Syncer::new(rep, vec![]);
    let mb = kt_net::nostr::NostrMailbox::new(&["ws://127.0.0.1:9"], kt_net::nostr::NostrPolicy::default());
    let dir = kt_net::helper_dir::HelperDirectory::new(None);
    let d = DhtDiscovery::new(s.replica(), bs, Some(Ipv4Addr::LOCALHOST)).unwrap();
    let found = d.find_helpers(day).await;
    assert_eq!(found.len(), 3, "all helpers found in the DHT slots");
    let r = kt_net::helper_dir::maintain_with_dht(&s, &hmb, &mb, &dir, Some(&d)).await;
    assert!(r.discovered >= 3 && dir.candidates().len() == 3, "{r:?}");
}

/// Members as DHT entry points: A's node is reachable and shared in its whereabouts; B
/// joins the DHT through A alone (none of the usual bootstrap nodes) and finds A's
/// records there. So the DHT layer survives the community bootstrap nodes disappearing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_bootstrap_the_dht_through_each_other() {
    let testnet = Testnet::builder(10).build().unwrap();
    let (mut ra, mut rb) = (replica("ea"), replica("eb"));
    let g = ra.create_group().unwrap();
    ra.add_member(&g, rb.device_public()).unwrap();
    rb.accept_group(g);
    for m in ra.export_group(&g).unwrap() {
        let _ = rb.receive(&m);
    }
    let ra = Arc::new(Mutex::new(ra));
    let da = DhtDiscovery::new(ra.clone(), Some(testnet.bootstrap.clone()), Some(Ipv4Addr::LOCALHOST)).unwrap();
    assert!(da.bootstrapped().await);
    let entry = da.entry_point().await.expect("A's node is reachable");
    // A's whereabouts (with its DHT node) reach B the usual way.
    let w = kt_net::p2p::WhereData { endpoint: [9; 32], relays: vec![], ips: vec![], dht: Some(entry) };
    let rec = {
        let mut r = ra.lock().unwrap();
        r.set_whereabouts(Some(postcard::to_stdvec(&w).unwrap()));
        r.own_whereabouts_record(&g).unwrap()
    };
    rb.accept_whereabouts_record(&rec).unwrap();
    assert_eq!(da.publish().await.published, 1);

    let rb = Arc::new(Mutex::new(rb));
    let only_a = DhtDiscovery::new(rb.clone(), Some(vec![entry.to_string()]), Some(Ipv4Addr::LOCALHOST)).unwrap();
    assert_eq!(only_a.member_entry_points(), vec![entry.to_string()], "B knows A as an entry point");
    assert!(only_a.bootstrapped().await, "joined the DHT through a member");
    let rep = only_a.lookup().await;
    assert_eq!(rep.found, 1, "found A's record through the DHT: {rep:?}");
}

