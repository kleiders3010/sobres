//! Self-sustaining relay list: from one seed, devices discover relays from monitor events,
//! agree on a group set, keep syncing, and replace relays that die, with no manual list.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};
use kt_net::mailbox::{Mailbox, RateBudget};
use kt_net::nostr::{NostrMailbox, NostrPolicy};
use kt_net::relays::{install_resolver, maintain, RelayDirectory, LAYER};
use kt_net::sync::Syncer;
use kt_net::testrelay::{RelayKnobs, TestRelay};
use nostr::prelude::*;

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-rd-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

fn policy() -> NostrPolicy {
    NostrPolicy { budget: RateBudget { per_minute: 6000.0, burst: 500 }, down_initial: Duration::from_millis(20), ..NostrPolicy::default() }
}

struct Dev {
    s: Arc<Syncer>,
    mb: Arc<NostrMailbox>,
    dir: RelayDirectory,
}

fn dev(r: Replica, seed: &str) -> Dev {
    let mb = Arc::new(NostrMailbox::new(&[seed], policy()));
    let s = Syncer::new(Arc::new(Mutex::new(r)), vec![mb.clone() as Arc<dyn Mailbox>]);
    install_resolver(&s, &mb);
    Dev { s, mb, dir: RelayDirectory::for_tests(&[seed]) }
}

async fn round(devs: &[&Dev]) {
    for d in devs {
        maintain(&d.s, &d.mb, &d.dir).await;
        d.s.sync_once().await;
    }
}

fn set(d: &Dev, g: &GroupId, doc: &DocId, k: &str) {
    let r = d.s.replica();
    let mut r = r.lock().unwrap();
    r.doc(g, doc).unwrap().get_map("tasks").insert(k, "x").unwrap();
    r.commit(g, doc).unwrap();
}

fn has(d: &Dev, g: &GroupId, doc: &DocId, k: &str) -> bool {
    format!("{:?}", d.s.replica().lock().unwrap().doc(g, doc).unwrap().get_deep_value()).contains(k)
}

/// A NIP-66 monitor announcing relays (as real monitors do on public relays).
async fn monitor_announces(on: &TestRelay, relays: &[(String, &[(&str, &str)])]) {
    let conn = kt_net::nostr::RelayConn::new(&on.url(), policy());
    let keys = Keys::generate();
    for (url, extra) in relays {
        let mut tags = vec![Tag::identifier(url.clone())];
        for (k, v) in extra.iter() {
            tags.push(Tag::parse([*k, *v]).unwrap());
        }
        let ev = EventBuilder::new(Kind::Custom(30166), "").tags(tags).finalize(&keys).unwrap();
        conn.publish(ev).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn from_one_seed_to_a_self_repairing_group_set() {
    let seed = TestRelay::start(None).await.unwrap();
    let mut others = Vec::new();
    for _ in 0..5 {
        others.push(TestRelay::start(None).await.unwrap());
    }
    let mut ann: Vec<(String, &[(&str, &str)])> = others.iter().map(|r| (r.url(), &[("n", "clearnet")][..])).collect();
    ann.push(("wss://hidden.onion".into(), &[("n", "tor")]));
    ann.push(("ws://127.0.0.1:1".into(), &[("n", "clearnet"), ("R", "auth")]));
    monitor_announces(&seed, &ann).await;

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
    let a = dev(ra, &seed.url());
    let b = dev(rb, &seed.url());

    // A discovers and records a set; B learns it (A also writes where B still reads).
    round(&[&a, &b]).await;
    round(&[&a, &b]).await;
    let cands = a.dir.candidates();
    assert!(cands.len() >= 6, "seed + 5 discovered: {:?}", cands.keys().collect::<Vec<_>>());
    assert!(!cands.keys().any(|u| u.contains("onion") || u.ends_with(":1")), "tor and auth-required skipped");
    let set_a = a.s.replica().lock().unwrap().mailbox_set(&g, LAYER).0.unwrap().endpoints;
    let set_b = b.s.replica().lock().unwrap().mailbox_set(&g, LAYER).0.map(|s| s.endpoints);
    assert_eq!(Some(set_a.clone()), set_b, "both devices use the same set");
    assert_eq!(set_a.len(), 4);

    set(&a, &g, &d, "before-failure");
    round(&[&a, &b]).await;
    assert!(has(&b, &g, &d, "before-failure"));

    // Half the set dies (the seed included, if it's in the set).
    let dead: Vec<&TestRelay> = std::iter::once(&seed).chain(others.iter()).filter(|r| set_a.contains(&r.url())).take(2).collect();
    for r in &dead {
        r.set_knobs(RelayKnobs { offline: true, ..Default::default() });
    }
    for _ in 0..6 {
        round(&[&a, &b]).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    let new_set = a.s.replica().lock().unwrap().mailbox_set(&g, LAYER).0.unwrap().endpoints;
    assert_ne!(new_set, set_a, "the set was repaired");
    for r in &dead {
        assert!(!new_set.contains(&r.url()), "dead relay replaced");
    }
    set(&b, &g, &d, "after-repair");
    for _ in 0..2 {
        round(&[&a, &b]).await;
    }
    assert!(has(&a, &g, &d, "after-repair"), "still syncing after the repair");
}
