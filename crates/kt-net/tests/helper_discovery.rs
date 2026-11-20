//! Self-sustaining helper list: helpers announce themselves; members that sync only
//! through helpers discover them, agree on a group helper set, and repair it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};
use kt_net::helper::{HelperLimits, HelperMailbox};
use kt_net::helper_dir::{announce, maintain, HelperDirectory, LAYER};
use kt_net::mailbox::{Mailbox, RateBudget};
use kt_net::nostr::{NostrMailbox, NostrPolicy};
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;
use kt_net::testrelay::TestRelay;

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-hd-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

fn cfg(helper: bool) -> P2pConfig {
    P2pConfig {
        bind: Some("127.0.0.1:0".parse().unwrap()),
        helper: helper.then(|| HelperLimits { base_pow_bits: 4, ..Default::default() }),
        ..Default::default()
    }
}

fn nostr(url: &str) -> Arc<NostrMailbox> {
    Arc::new(NostrMailbox::new(&[url], NostrPolicy { budget: RateBudget { per_minute: 6000.0, burst: 500 }, ..NostrPolicy::default() }))
}

struct Member {
    s: Arc<Syncer>,
    hmb: Arc<HelperMailbox>,
    mb: Arc<NostrMailbox>,
    dir: HelperDirectory,
}

async fn member(r: Replica, relay: &str) -> Member {
    let rep = Arc::new(Mutex::new(r));
    let p = P2p::start(Syncer::new(rep.clone(), vec![]), cfg(false)).await.unwrap();
    let hmb = Arc::new(HelperMailbox::new(p, vec![]));
    let s = Syncer::new(rep, vec![hmb.clone() as Arc<dyn Mailbox>]);
    kt_net::helper_dir::install_resolver(&s, &hmb);
    Member { s, hmb, mb: nostr(relay), dir: HelperDirectory::new(None) }
}

async fn round(ms: &[&Member]) {
    for m in ms {
        maintain(&m.s, &m.hmb, &m.mb, &m.dir).await;
        m.s.sync_once().await;
    }
}

fn set(m: &Member, g: &GroupId, d: &DocId, k: &str) {
    let r = m.s.replica();
    let mut r = r.lock().unwrap();
    r.doc(g, d).unwrap().get_map("tasks").insert(k, "x").unwrap();
    r.commit(g, d).unwrap();
}

fn has(m: &Member, g: &GroupId, d: &DocId, k: &str) -> bool {
    format!("{:?}", m.s.replica().lock().unwrap().doc(g, d).unwrap().get_deep_value()).contains(k)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_discover_helpers_agree_on_a_set_and_repair_it() {
    let relay = TestRelay::start(None).await.unwrap();
    // Four strangers' devices help and announce themselves.
    let mut helpers = Vec::new();
    for i in 0..4 {
        let p = P2p::start(Syncer::new(Arc::new(Mutex::new(replica(&format!("h{i}")))), vec![]), cfg(true)).await.unwrap();
        let hm = HelperMailbox::new(p.clone(), vec![]);
        assert!(announce(&hm, &nostr(&relay.url())).await > 0);
        helpers.push(p);
    }

    let (mut ra, mut rb) = (replica("a"), replica("b"));
    let g = ra.create_group().unwrap();
    let d = ra.create_doc(&g).unwrap();
    ra.doc(&g, &d).unwrap().get_map("tasks").insert("t0", "x").unwrap();
    ra.commit(&g, &d).unwrap();
    ra.add_member(&g, rb.device_public()).unwrap();
    rb.accept_group(g);
    for m in ra.export_group(&g).unwrap() {
        let _ = rb.receive(&m);
    }
    let _ = ra.outbox();
    let a = member(ra, &relay.url()).await;
    let b = member(rb, &relay.url()).await;

    set(&a, &g, &d, "via-helpers");
    round(&[&a, &b]).await;
    round(&[&a, &b]).await;
    assert_eq!(a.dir.candidates().len(), 4, "all helpers discovered");
    let set_a = a.s.replica().lock().unwrap().mailbox_set(&g, LAYER).0.unwrap().endpoints;
    assert_eq!(set_a.len(), 4);
    assert_eq!(b.s.replica().lock().unwrap().mailbox_set(&g, LAYER).0.map(|s| s.endpoints), Some(set_a.clone()));
    assert!(has(&b, &g, &d, "via-helpers"), "synced through strangers' helpers only");

    // Two helpers go away; the set is repaired and sync continues.
    helpers[0].close().await;
    helpers[1].close().await;
    for i in 0..6 {
        set(&a, &g, &d, &format!("after-{i}"));
        round(&[&a, &b]).await;
    }
    let new_set = a.s.replica().lock().unwrap().mailbox_set(&g, LAYER).0.unwrap().endpoints;
    assert!(new_set.len() < set_a.len(), "dead helpers dropped: {} -> {}", set_a.len(), new_set.len());
    assert!(has(&b, &g, &d, "after-5"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn helpers_are_found_without_nostr() {
    // No Nostr at all: every relay URL is dead.
    let dead = "ws://127.0.0.1:9";
    let mut helpers = Vec::new();
    for i in 0..4 {
        let p = P2p::start(Syncer::new(Arc::new(Mutex::new(replica(&format!("nh{i}")))), vec![]), cfg(true)).await.unwrap();
        helpers.push(p);
    }
    let h1 = kt_net::helper::HelperRef::from_addr(&helpers[0].where_data().to_addr().unwrap());
    // The others introduce themselves to h1 (as helpers do with any helper they know).
    for p in &helpers[1..] {
        let hm = HelperMailbox::new(p.clone(), vec![]);
        let me = kt_net::helper::HelperRef::from_addr(&p.where_data().to_addr().unwrap());
        assert!(hm.introduce(&h1.to_addr().unwrap(), me).await);
    }

    let (mut ra, mut rb) = (replica("na"), replica("nb"));
    let g = ra.create_group().unwrap();
    let d = ra.create_doc(&g).unwrap();
    ra.doc(&g, &d).unwrap().get_map("tasks").insert("t0", "x").unwrap();
    ra.commit(&g, &d).unwrap();
    ra.add_member(&g, rb.device_public()).unwrap();
    rb.accept_group(g);
    for m in ra.export_group(&g).unwrap() {
        let _ = rb.receive(&m);
    }
    let _ = ra.outbox();
    let a = member(ra, dead).await;
    let b = member(rb, dead).await;
    // Each knows only h1 (an invite record carries a few of the group's helpers).
    a.dir.add(h1.clone());
    b.dir.add(h1);

    set(&a, &g, &d, "no-nostr");
    for _ in 0..3 {
        round(&[&a, &b]).await;
    }
    assert!(a.dir.candidates().len() >= 4, "peer exchange found the others: {}", a.dir.candidates().len());
    let set_a = a.s.replica().lock().unwrap().mailbox_set(&g, LAYER).0.unwrap().endpoints;
    assert_eq!(set_a.len(), 4);
    assert!(has(&b, &g, &d, "no-nostr"), "synced through helpers found without Nostr");
}
