//! Sync over the Nostr mailbox against local test relays: convergence, epoch changes,
//! relays that drop silently, rate-limit, or go offline.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};
use kt_net::mailbox::{Mailbox, RateBudget};
use kt_net::nostr::{NostrMailbox, NostrPolicy};
use kt_net::sync::Syncer;
use kt_net::testrelay::{RelayKnobs, TestRelay};

fn sandbox() -> SandboxConfig {
    SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) }
}

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-net-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    Replica::open(&root.join("identity"), &root.join("data"), sandbox()).unwrap()
}

/// Generous budget for functional tests (the real one is exercised separately).
fn test_policy() -> NostrPolicy {
    NostrPolicy { budget: RateBudget { per_minute: 600.0, burst: 50 }, ..NostrPolicy::default() }
}

fn syncer(r: Replica, urls: &[String], policy: NostrPolicy) -> (Arc<Syncer>, Arc<NostrMailbox>) {
    let urls: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
    let mb = Arc::new(NostrMailbox::new(&urls, policy));
    let s = Syncer::new(Arc::new(Mutex::new(r)), vec![mb.clone() as Arc<dyn Mailbox>]);
    (s, mb)
}

fn set(s: &Syncer, g: &GroupId, d: &DocId, k: &str, v: &str) {
    let r = s.replica();
    let mut r = r.lock().unwrap();
    r.doc(g, d).unwrap().get_map("tasks").insert(k, v).unwrap();
    r.commit(g, d).unwrap();
}

fn value(s: &Syncer, g: &GroupId, d: &DocId) -> String {
    let r = s.replica();
    let r = r.lock().unwrap();
    format!("{:?}", r.doc(g, d).map(|x| x.get_deep_value()))
}

/// Group with A and B (B joined directly; everything after goes over Nostr).
fn setup(a: &mut Replica, b: &mut Replica) -> (GroupId, DocId) {
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
    (g, d)
}

async fn relays(n: usize) -> (Vec<TestRelay>, Vec<String>) {
    let mut rs = Vec::new();
    for _ in 0..n {
        rs.push(TestRelay::start(None).await.unwrap());
    }
    let urls = rs.iter().map(|r| r.url()).collect();
    (rs, urls)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_devices_sync_over_nostr() {
    let (relays, urls) = relays(5).await;
    let (mut ra, mut rb) = (replica("a"), replica("b"));
    let (g, d) = setup(&mut ra, &mut rb);
    let (a, _) = syncer(ra, &urls, test_policy());
    let (b, _) = syncer(rb, &urls, test_policy());

    set(&a, &g, &d, "t1", "from A");
    set(&b, &g, &d, "t2", "from B");
    a.sync_once().await;
    b.sync_once().await;
    a.sync_once().await;
    let v = value(&a, &g, &d);
    assert_eq!(v, value(&b, &g, &d));
    assert!(v.contains("from A") && v.contains("from B"), "{v}");

    // Each topic lands on 3 of the 5 relays; unchanged slots aren't rewritten.
    let used = relays.iter().filter(|r| r.stored_events() > 0).count();
    assert!(used >= 3, "slots spread over relays: {used}");
    let before: u64 = relays.iter().map(|r| r.received_events()).sum();
    let (w, _) = a.sync_once().await;
    assert_eq!(w.written, 0, "no change, no write");
    let after: u64 = relays.iter().map(|r| r.received_events()).sum();
    assert_eq!(before, after);

    // Reading the same versions again doesn't reopen them.
    let (_, r) = b.sync_once().await;
    assert_eq!(r.slots_opened, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survives_dropping_rate_limiting_and_offline_relays() {
    let (relays, urls) = relays(6).await;
    let (mut ra, mut rb) = (replica("ha"), replica("hb"));
    let (g, d) = setup(&mut ra, &mut rb);
    let (a, amb) = syncer(ra, &urls, test_policy());
    let (b, _) = syncer(rb, &urls, test_policy());
    relays[0].set_knobs(RelayKnobs { drop_silently: true, ..Default::default() });
    relays[1].set_knobs(RelayKnobs { events_per_minute: Some(0), ..Default::default() });
    relays[2].set_knobs(RelayKnobs { offline: true, ..Default::default() });

    for i in 0..3 {
        set(&a, &g, &d, &format!("a{i}"), "x");
        set(&b, &g, &d, &format!("b{i}"), "y");
        a.sync_once().await;
        b.sync_once().await;
    }
    a.sync_once().await;
    let v = value(&a, &g, &d);
    assert_eq!(v, value(&b, &g, &d));
    assert!(v.contains("a2") && v.contains("b2"));

    // The rate-limiting relay was backed off after its first refusal, not hammered.
    assert!(relays[1].received_events() <= 2, "rate-limited relay got {} events", relays[1].received_events());
    let health: std::collections::HashMap<String, _> = amb.health().into_iter().collect();
    println!("health: {health:#?}");
    // Relays are contacted on demand: check the ones this device actually used.
    if let Some(dropper) = health.get(&urls[0]).filter(|h| h.published_ok > 0) {
        assert!(dropper.readback_misses > 0, "a relay that says OK but drops is noticed: {dropper:?}");
    }
    if let Some(offline) = health.get(&urls[2]) {
        assert_eq!(offline.published_ok, 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_write_budget_is_respected() {
    // The launch policy: ~6 events/min per relay, burst 5, shared across groups.
    let (relays, urls) = relays(3).await;
    let mut ra = replica("ba");
    let mut groups = Vec::new();
    for _ in 0..8 {
        let g = ra.create_group().unwrap();
        let d = ra.create_doc(&g).unwrap();
        ra.doc(&g, &d).unwrap().get_map("tasks").insert("k", "v").unwrap();
        ra.commit(&g, &d).unwrap();
        groups.push((g, d));
    }
    let (a, _) = syncer(ra, &urls, NostrPolicy::default());
    let w = a.write_round().await;
    // 8 groups x 3 relays each (all 3 here) = 24 wanted writes; burst 5 per relay allows 15.
    for r in &relays {
        assert!(r.received_events() <= 5, "burst respected: {}", r.received_events());
    }
    assert!(w.deferred > 0, "the rest waits for budget: {w:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_limited_relay_is_backed_off() {
    // Three relays: every topic uses all three, so the limiting one is always involved.
    let (relays, urls) = relays(3).await;
    let (mut ra, mut rb) = (replica("rla"), replica("rlb"));
    let (g, d) = setup(&mut ra, &mut rb);
    let (a, amb) = syncer(ra, &urls, test_policy());
    let (b, _) = syncer(rb, &urls, test_policy());
    relays[0].set_knobs(RelayKnobs { events_per_minute: Some(1), ..Default::default() });
    for i in 0..4 {
        set(&a, &g, &d, &format!("a{i}"), "x");
        a.sync_once().await;
        b.sync_once().await;
    }
    assert!(value(&b, &g, &d).contains("a3"), "the other relays carried it");
    let h: std::collections::HashMap<String, _> = amb.health().into_iter().collect();
    assert_eq!(h[&urls[0]].rate_limited, 1, "stopped at the first rate-limited reply: {:?}", h[&urls[0]]);
    // Per device (A and B both write): 1 accepted + 1 refused, then silence for the whole
    // backoff (at least 45 s), across all 4 rounds.
    assert!(relays[0].received_events() <= 4, "got {}", relays[0].received_events());
    assert!(relays[0].rate_limited_count() <= 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn devices_with_different_pools_still_meet_on_topics_outside_groups() {
    use nostr::prelude::*;
    let (_relays, urls) = relays(6).await;
    let seeds: Vec<&str> = urls[..3].iter().map(|s| s.as_str()).collect();
    let a = NostrMailbox::new(&seeds, test_policy());
    let b = NostrMailbox::new(&seeds, test_policy());
    // Their own pools have evolved differently (relay discovery, health).
    a.set_pool(vec![urls[3].clone(), urls[4].clone()]);
    b.set_pool(vec![urls[5].clone()]);
    let topic = [42u8; 32];
    let keys = Keys::generate();
    let ev = EventBuilder::new(Kind::Custom(35713), "invite").tags([Tag::hashtag(kt_net::nostr::topic_tag(&topic))]).finalize(&keys).unwrap();
    assert!(a.publish_on(&topic, ev).await > 0);
    let got = b.query_on(&topic, Filter::new().kind(Kind::Custom(35713)).hashtag(kt_net::nostr::topic_tag(&topic))).await;
    assert_eq!(got.len(), 1, "met through the shared seed list");
}
