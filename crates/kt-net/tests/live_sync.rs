//! Live mode: pushed slots arrive without polling; knocks teach addresses so online
//! members dial each other at once.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};
use kt_net::live::{knock_all, run_live};
use kt_net::mailbox::{Mailbox, RateBudget};
use kt_net::nostr::{NostrMailbox, NostrPolicy};
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;
use kt_net::testrelay::TestRelay;

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-live-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

fn pair() -> (Replica, Replica, GroupId, DocId) {
    let (mut a, mut b) = (replica("a"), replica("b"));
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    a.doc(&g, &d).unwrap().get_map("tasks").insert("t0", "start").unwrap();
    a.commit(&g, &d).unwrap();
    a.add_member(&g, b.device_public()).unwrap();
    b.accept_group(g);
    for m in a.export_group(&g).unwrap() {
        let _ = b.receive(&m);
    }
    let _ = a.outbox();
    (a, b, g, d)
}

fn mailbox(url: &str) -> Arc<NostrMailbox> {
    Arc::new(NostrMailbox::new(&[url], NostrPolicy { budget: RateBudget { per_minute: 600.0, burst: 50 }, ..NostrPolicy::default() }))
}

fn value(s: &Syncer, g: &GroupId, d: &DocId) -> String {
    format!("{:?}", s.replica().lock().unwrap().doc(g, d).unwrap().get_deep_value())
}

async fn wait_for(s: &Syncer, g: &GroupId, d: &DocId, needle: &str, secs: u64) -> Option<Duration> {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        if value(s, g, d).contains(needle) {
            return Some(t.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_apps_receive_changes_within_seconds_without_polling() {
    let relay = TestRelay::start(None).await.unwrap();
    let (ra, rb, g, d) = pair();
    let (ma, mb) = (mailbox(&relay.url()), mailbox(&relay.url()));
    let a = Syncer::new(Arc::new(Mutex::new(ra)), vec![ma as Arc<dyn Mailbox>]);
    let b = Syncer::new(Arc::new(Mutex::new(rb)), vec![mb.clone() as Arc<dyn Mailbox>]);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(run_live(b.clone(), mb, None, stop_rx));
    tokio::time::sleep(Duration::from_millis(300)).await; // subscription in place
    a.replica().lock().unwrap().doc(&g, &d).unwrap().get_map("tasks").insert("t1", "pushed live").unwrap();
    a.replica().lock().unwrap().commit(&g, &d).unwrap();
    a.write_round().await; // A only writes; B never polls
    let took = wait_for(&b, &g, &d, "pushed live", 5).await;
    assert!(took.is_some(), "B got the change from its live subscription");
    println!("live delivery took {:?}", took.unwrap());
    let _ = stop_tx.send(true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_knock_makes_online_members_dial_each_other() {
    let relay = TestRelay::start(None).await.unwrap();
    let (ra, rb, g, d) = pair();
    let (ma, mb) = (mailbox(&relay.url()), mailbox(&relay.url()));
    // No mailbox slots at all for this test: whereabouts can only come from the knock.
    let a = Syncer::new(Arc::new(Mutex::new(ra)), vec![]);
    let b = Syncer::new(Arc::new(Mutex::new(rb)), vec![]);
    let cfg = P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), ..Default::default() };
    let pa = P2p::start(a.clone(), cfg.clone()).await.unwrap();
    let pb = P2p::start(b.clone(), cfg).await.unwrap();
    pa.publish_whereabouts();
    pb.publish_whereabouts();
    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(run_live(b.clone(), mb, Some(pb.clone()), stop_rx));
    tokio::time::sleep(Duration::from_millis(300)).await;
    // A has an edit and announces itself.
    a.replica().lock().unwrap().doc(&g, &d).unwrap().get_map("tasks").insert("t1", "after a knock").unwrap();
    a.replica().lock().unwrap().commit(&g, &d).unwrap();
    a.write_round().await; // into A's store (served over P2P)
    assert!(knock_all(&a, &ma).await > 0);
    let took = wait_for(&b, &g, &d, "after a knock", 10).await;
    assert!(took.is_some(), "B learned A's address from the knock, dialed, and pulled the change");
    let _ = pa;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_apps_stay_in_sync_without_nostr() {
    use kt_net::live::run_p2p_live;
    let (ra, rb, g, d) = pair();
    let a = Syncer::new(Arc::new(Mutex::new(ra)), vec![]);
    let b = Syncer::new(Arc::new(Mutex::new(rb)), vec![]);
    let cfg = P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), ..Default::default() };
    let pa = P2p::start(a.clone(), cfg.clone()).await.unwrap();
    let pb = P2p::start(b.clone(), cfg).await.unwrap();
    pa.publish_whereabouts();
    pb.publish_whereabouts();
    // They learned each other's address earlier (DHT, LAN, an old slot...): simulate one
    // exchange of slots, then no mailbox at all.
    for (from, to) in [(&a, &b), (&b, &a)] {
        let targets = from.replica().lock().unwrap().slot_targets();
        for t in targets {
            let blob = from.replica().lock().unwrap().slot_blob(&t).unwrap();
            let rt = to.replica().lock().unwrap().read_topics().into_iter().find(|x| x.topic == t.topic);
            if let Some(rt) = rt {
                let _ = to.replica().lock().unwrap().open_slot(&rt, &blob);
            }
        }
    }
    let (_s1, r1) = tokio::sync::watch::channel(false);
    let (_s2, r2) = tokio::sync::watch::channel(false);
    tokio::spawn(run_p2p_live(a.clone(), pa.clone(), r1));
    tokio::spawn(run_p2p_live(b.clone(), pb.clone(), r2));
    tokio::time::sleep(Duration::from_millis(500)).await;
    a.replica().lock().unwrap().doc(&g, &d).unwrap().get_map("tasks").insert("t9", "pushed directly").unwrap();
    a.replica().lock().unwrap().commit(&g, &d).unwrap();
    let took = wait_for(&b, &g, &d, "pushed directly", 5).await;
    assert!(took.is_some(), "pushed over P2P without any mailbox");
    println!("direct push took {:?}", took.unwrap());
}
