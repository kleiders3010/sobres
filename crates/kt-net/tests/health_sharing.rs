//! Health observations shared within a group (DESIGN §7, §7a): what one member device
//! learns about helpers and relays reaches the group's other devices through their
//! normal sync, counts for less than their own observations, and never echoes back.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_net::helper::{HelperMailbox, Reliability};
use kt_net::mailbox::{Mailbox, RateBudget};
use kt_net::nostr::{NostrMailbox, NostrPolicy};
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;
use kt_net::testrelay::TestRelay;

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-hs-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

struct Dev {
    s: Arc<Syncer>,
    nostr: Arc<NostrMailbox>,
    hmb: Arc<HelperMailbox>,
}

async fn dev(r: Replica, relay: &str) -> Dev {
    let rep = Arc::new(Mutex::new(r));
    let nostr = Arc::new(NostrMailbox::new(&[relay], NostrPolicy { budget: RateBudget { per_minute: 6000.0, burst: 500 }, ..NostrPolicy::default() }));
    let p = P2p::start(Syncer::new(rep.clone(), vec![]), P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), ..Default::default() }).await.unwrap();
    let hmb = Arc::new(HelperMailbox::new(p, vec![]));
    let s = Syncer::new(rep, vec![nostr.clone() as Arc<dyn Mailbox>]);
    Dev { s, nostr, hmb }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_one_device_learns_reaches_its_group() {
    let relay = TestRelay::start(None).await.unwrap();
    let (mut ra, mut rb, outsider) = (replica("a"), replica("b"), replica("x"));
    let g = ra.create_group().unwrap();
    ra.add_member(&g, rb.device_public()).unwrap();
    rb.accept_group(g);
    for m in ra.export_group(&g).unwrap() {
        let _ = rb.receive(&m);
    }
    let a = dev(ra, &relay.url()).await;
    let b = dev(rb, &relay.url()).await;
    let x = dev(outsider, &relay.url()).await;

    // A caught a helper returning data that fails verification, and knows another as good.
    let bad = iroh::SecretKey::from_bytes(&[21; 32]).public();
    let good = iroh::SecretKey::from_bytes(&[22; 32]).public();
    a.hmb.note_for_tests(bad, Reliability { reachable: 3, excluded: true, ..Default::default() });
    a.hmb.note_for_tests(good, Reliability { reachable: 40, ..Default::default() });
    assert!(!b.hmb.is_failing(&bad));

    for _ in 0..3 {
        for d in [&a, &b, &x] {
            kt_net::health::exchange(&d.s, Some(&d.hmb), &d.nostr);
            d.s.sync_once().await;
        }
    }
    // Reports ride along with real changes (and go out daily): A edits something.
    {
        let r = a.s.replica();
        let mut r = r.lock().unwrap();
        let d = r.create_doc(&g).unwrap();
        r.doc(&g, &d).unwrap().get_map("tasks").insert("t", "x").unwrap();
        r.commit(&g, &d).unwrap();
    }
    for d in [&a, &b, &x] {
        kt_net::health::exchange(&d.s, Some(&d.hmb), &d.nostr);
        d.s.sync_once().await;
    }
    for d in [&a, &b, &x] {
        kt_net::health::exchange(&d.s, Some(&d.hmb), &d.nostr);
    }

    // B now avoids the bad helper and knows the good one, at half weight.
    assert!(b.hmb.is_failing(&bad), "a member's exclusion reaches the group");
    let rec = b.hmb.records();
    assert_eq!(rec.get(&good).map(|r| r.reachable), Some(20), "others' observations count less");
    assert!(b.hmb.own_records().get(&good).is_none(), "only merged, never re-shared as B's own");
    // Relay health: B's view includes A's read-back evidence for the relay they share.
    let own_hits = b.nostr.health().into_iter().find(|(u, _)| *u == relay.url()).map(|(_, h)| h.readback_hits).unwrap_or(0);
    let merged_hits = b.nostr.health_merged().into_iter().find(|(u, _)| *u == relay.url()).map(|(_, h)| h.readback_hits).unwrap_or(0);
    assert!(merged_hits > own_hits, "A's relay observations count at B: own {own_hits}, merged {merged_hits}");
    // An outsider learns nothing (it can't read the group's slots).
    assert!(!x.hmb.is_failing(&bad));
    // No echo: A's own view isn't inflated by its own report coming back.
    assert_eq!(a.hmb.records().get(&good).map(|r| r.reachable), Some(40));
}
