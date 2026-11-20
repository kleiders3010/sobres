//! First joins when the invite's helper hints are gone (DESIGN §7a): the joiner computes
//! the group's secret daily point from the key its invite carried and looks at the
//! helpers closest to it, the same rule members use to choose their helper set, so it
//! lands on the group's helpers without any hint.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::invite::{self, CodeKind};
use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_net::helper::{HelperLimits, HelperMailbox};
use kt_net::helper_dir::{announce, decode_ref, maintain, HelperDirectory, HELPER_SET, LAYER};
use kt_net::mailbox::{Mailbox, RateBudget};
use kt_net::nostr::{NostrMailbox, NostrPolicy};
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;
use kt_net::testrelay::TestRelay;

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-jp-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

fn loopback(helper: bool) -> P2pConfig {
    P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), helper: helper.then(HelperLimits::default), ..Default::default() }
}

fn nostr(url: &str) -> Arc<NostrMailbox> {
    Arc::new(NostrMailbox::new(&[url], NostrPolicy { budget: RateBudget { per_minute: 6000.0, burst: 500 }, ..NostrPolicy::default() }))
}

struct Dev {
    s: Arc<Syncer>,
    hmb: Arc<HelperMailbox>,
    mb: Arc<NostrMailbox>,
    dir: HelperDirectory,
}

async fn dev(r: Replica, relay: &str) -> Dev {
    let rep = Arc::new(Mutex::new(r));
    let p = P2p::start(Syncer::new(rep.clone(), vec![]), loopback(false)).await.unwrap();
    let hmb = Arc::new(HelperMailbox::new(p, vec![]));
    let s = Syncer::new(rep, vec![hmb.clone() as Arc<dyn Mailbox>]);
    kt_net::helper_dir::install_resolver(&s, &hmb);
    Dev { s, hmb, mb: nostr(relay), dir: HelperDirectory::new(None) }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_joiner_without_hints_finds_the_groups_helpers() {
    let relay = TestRelay::start(None).await.unwrap();
    // More helpers than a set holds, so which ones are chosen depends on the point.
    let mut helpers = Vec::new();
    for i in 0..(HELPER_SET + 4) {
        let p = P2p::start(Syncer::new(Arc::new(Mutex::new(replica(&format!("h{i}")))), vec![]), loopback(true)).await.unwrap();
        assert!(announce(&HelperMailbox::new(p.clone(), vec![]), &nostr(&relay.url())).await > 0);
        helpers.push(p);
    }
    let mut ra = replica("a");
    let g = ra.create_group().unwrap();
    let (code, _) = ra.create_invite(&g, CodeKind::Invite, 30 * 86_400_000).unwrap();
    let a = dev(ra, &relay.url()).await;
    maintain(&a.s, &a.hmb, &a.mb, &a.dir).await;
    let members_set: BTreeSet<[u8; 32]> = {
        let (cur, _) = a.s.replica().lock().unwrap().mailbox_set(&g, LAYER);
        cur.expect("members chose a set").endpoints.iter().filter_map(|e| decode_ref(e)).map(|h| h.endpoint).collect()
    };
    assert_eq!(members_set.len(), HELPER_SET);

    // The invite record, with its hints gone (the hinted helpers left, say).
    let day = invite::day_of(kt_engine::replica::now_ms());
    let (lookup, key) = invite::derive(&code, CodeKind::Invite, day).unwrap();
    let pubs = a.s.replica().lock().unwrap().invite_publications(day, &|_| ("Family".into(), "Alex".into())).unwrap();
    let mut rec = invite::open_record(&pubs.iter().find(|(l, _)| *l == lookup).unwrap().1, &lookup, &key).unwrap();
    rec.hints.clear();

    let mut rc = replica("c");
    rc.begin_join(&rec).unwrap();
    assert!(rc.endpoint_hints(&g, LAYER).is_empty());
    let c = dev(rc, &relay.url()).await;
    maintain(&c.s, &c.hmb, &c.mb, &c.dir).await;
    let found: BTreeSet<[u8; 32]> = c.s.replica().lock().unwrap().endpoint_hints(&g, LAYER).iter().filter_map(|e| decode_ref(e)).map(|h| h.endpoint).collect();
    assert_eq!(found, members_set, "the joiner lands on the members' helper set by the point alone");
}
