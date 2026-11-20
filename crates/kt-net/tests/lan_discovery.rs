//! LAN beacons (loopback unicast stands in for multicast): members learn each other's
//! address and sync directly; an outsider hearing the beacons learns nothing.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_net::lan::{Lan, LanConfig};
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-lan-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

fn cfg() -> LanConfig {
    LanConfig { bind: "127.0.0.1:0".parse().unwrap(), targets: vec![], join_multicast: false, interval: Duration::from_millis(200) }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_on_the_same_network_find_each_other() {
    let (mut ra, mut rb, rx) = (replica("a"), replica("b"), replica("x"));
    let g = ra.create_group().unwrap();
    let d = ra.create_doc(&g).unwrap();
    ra.doc(&g, &d).unwrap().get_map("tasks").insert("t0", "start").unwrap();
    ra.commit(&g, &d).unwrap();
    ra.add_member(&g, rb.device_public()).unwrap();
    rb.accept_group(g);
    for m in ra.export_group(&g).unwrap() {
        let _ = rb.receive(&m);
    }
    let p2p = P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), ..Default::default() };
    let (sa, sb) = (Syncer::new(Arc::new(Mutex::new(ra)), vec![]), Syncer::new(Arc::new(Mutex::new(rb)), vec![]));
    let pa = P2p::start(sa.clone(), p2p.clone()).await.unwrap();
    let pb = P2p::start(sb.clone(), p2p).await.unwrap();
    pa.publish_whereabouts();
    pb.publish_whereabouts();

    // B and an outsider listen; A beacons to both.
    let lb = Lan::start(sb.replica(), cfg()).await.unwrap();
    let lx = Lan::start(Arc::new(Mutex::new(rx)), cfg()).await.unwrap();
    let la = Lan::start(sa.replica(), LanConfig { targets: vec![lb.local_addr().unwrap(), lx.local_addr().unwrap()], ..cfg() }).await.unwrap();
    let learned = lb.learned.notified();
    assert!(la.beacon().await > 0);
    tokio::time::timeout(Duration::from_secs(5), learned).await.expect("B learned A's address from the beacon");
    assert_eq!(sb.replica().lock().unwrap().member_whereabouts(&g).len(), 1);

    // B dials A directly with what it learned.
    sb.replica().lock().unwrap().doc(&g, &d).unwrap().get_map("tasks").insert("t1", "found on the LAN").unwrap();
    sb.replica().lock().unwrap().commit(&g, &d).unwrap();
    let rep = pb.sync_round().await;
    assert_eq!(rep.peers_reached, 1, "{rep:?}");
    let v = format!("{:?}", sa.replica().lock().unwrap().doc(&g, &d).unwrap().get_deep_value());
    assert!(v.contains("found on the LAN"));
    let _ = (pa, lx);
}
