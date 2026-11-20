//! Each layer taken away in turn (Kleider, 2026-11-18: "all parts of the app should work even if one the pieces
//! break down"). Two members of a list sync through the three mailbox layers the app writes to: Nostr relays,
//! helpers and the cloud mailbox. Each one is made to fail in turn, in the worst way: a server that accepts the
//! connection and then never answers (Nostr, cloud) or a helper that is gone; the others must still carry changes
//! both ways, in bounded time. Then each layer alone must carry them too.
//!
//! The cloud layer needs the mailbox Worker running: `KT_CLOUD_URL=http://127.0.0.1:8787` with `npm run dev` in
//! `cloud/mailbox-worker` (without it the cloud is counted as down in every case and "cloud alone" is skipped).
//! Direct paths are covered elsewhere: devices syncing with every relay gone (`p2p_sync.rs`), the same Wi-Fi
//! (`lan_discovery.rs`), finding each other on the DHT with no Nostr (`dht_discovery.rs`, `no_nostr.rs`).
//! Loopback only.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};
use kt_net::cloud::{CloudMailbox, CloudMode};
use kt_net::helper::{HelperLimits, HelperMailbox};
use kt_net::mailbox::{Mailbox, RateBudget};
use kt_net::nostr::{NostrMailbox, NostrPolicy};
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;
use kt_net::testrelay::TestRelay;

/// A whole write round plus a whole read round with one layer hanging may take this long, at most.
const ROUND_LIMIT: Duration = Duration::from_secs(60);

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-down-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

fn cfg() -> P2pConfig {
    P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), ..Default::default() }
}

/// A server that takes connections and never says a word: the worst kind of "down".
async fn silent() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = l.accept().await {
            held.push(s);
        }
    });
    port
}

struct Net {
    _relay: TestRelay,
    relay_url: String,
    helper: iroh::EndpointAddr,
    gone_helper: iroh::EndpointAddr,
    silent_port: u16,
    cloud: Option<String>,
    _helper_node: Arc<P2p>,
}

async fn net() -> Net {
    let relay = TestRelay::start(None).await.unwrap();
    let relay_url = relay.url();
    let limits = || HelperLimits { base_pow_bits: 8, ..Default::default() };
    let hs = Syncer::new(Arc::new(Mutex::new(replica("h"))), vec![]);
    let hp = P2p::start(hs, P2pConfig { helper: Some(limits()), ..cfg() }).await.unwrap();
    let helper = hp.where_data().to_addr().unwrap();
    let gs = Syncer::new(Arc::new(Mutex::new(replica("g"))), vec![]);
    let gp = P2p::start(gs, P2pConfig { helper: Some(limits()), ..cfg() }).await.unwrap();
    let gone_helper = gp.where_data().to_addr().unwrap();
    gp.close().await;
    let cloud = std::env::var("KT_CLOUD_URL").ok();
    Net { _relay: relay, relay_url, helper, gone_helper, silent_port: silent().await, cloud, _helper_node: hp }
}

/// Which layers work: [Nostr, helpers, cloud].
type Alive = [bool; 3];
const NAMES: [&str; 3] = ["Nostr", "helpers", "cloud"];

struct Member {
    s: Arc<Syncer>,
    p: Arc<P2p>,
}

async fn member(r: Replica, n: &Net, alive: Alive) -> Member {
    let rep = Arc::new(Mutex::new(r));
    let p = P2p::start(Syncer::new(rep.clone(), vec![]), cfg()).await.unwrap();
    let policy = NostrPolicy { budget: RateBudget { per_minute: 600.0, burst: 50 }, ..NostrPolicy::default() };
    let silent_ws = format!("ws://127.0.0.1:{}", n.silent_port);
    let nostr = NostrMailbox::new(&[if alive[0] { n.relay_url.as_str() } else { silent_ws.as_str() }], policy);
    let helper = HelperMailbox::new(p.clone(), vec![if alive[1] { n.helper.clone() } else { n.gone_helper.clone() }]);
    let silent_http = format!("http://127.0.0.1:{}", n.silent_port);
    let cloud_url = match (&n.cloud, alive[2]) {
        (Some(u), true) => u.clone(),
        _ => silent_http,
    };
    let cloud = CloudMailbox::new(&cloud_url, CloudMode::Always).unwrap();
    let s = Syncer::new(rep, vec![Arc::new(nostr) as Arc<dyn Mailbox>, Arc::new(helper), Arc::new(cloud)]);
    Member { s, p }
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
    let _ = a.outbox();
    (g, d)
}

fn set(m: &Member, g: &GroupId, d: &DocId, k: &str, v: &str) {
    let r = m.s.replica();
    let mut r = r.lock().unwrap();
    r.doc(g, d).unwrap().get_map("tasks").insert(k, v).unwrap();
    r.commit(g, d).unwrap();
}

fn value(m: &Member, g: &GroupId, d: &DocId) -> String {
    let r = m.s.replica();
    let r = r.lock().unwrap();
    format!("{:?}", r.doc(g, d).map(|x| x.get_deep_value()))
}

/// A writes, B reads; then B writes, A reads. Returns what went wrong, if anything.
async fn case(n: &Net, alive: Alive, what: &str) -> Option<String> {
    let (mut ra, mut rb) = (replica("a"), replica("b"));
    let (g, d) = group(&mut ra, &mut rb);
    let a = member(ra, n, alive).await;
    let b = member(rb, n, alive).await;
    let mut bad = None;
    for (from, to, key) in [(&a, &b, "t1"), (&b, &a, "t2")] {
        let text = format!("{what} {key}");
        set(from, &g, &d, key, &text);
        let t = Instant::now();
        let got = tokio::time::timeout(ROUND_LIMIT, async {
            from.s.write_round().await;
            // a read round may need a second try while a layer's first write is still landing
            for _ in 0..3 {
                to.s.read_round().await;
                if value(to, &g, &d).contains(&text) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            false
        })
        .await;
        let secs = t.elapsed().as_secs_f32();
        println!("{what}: {key} {} in {secs:.1} s", if got == Ok(true) { "arrived" } else { "DID NOT ARRIVE" });
        if got != Ok(true) {
            bad = Some(format!("{what}: {key} did not arrive ({})", if got.is_err() { "the round hung" } else { "not there" }));
        }
    }
    a.p.close().await;
    b.p.close().await;
    bad
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_layer_can_be_taken_away() {
    let n = net().await;
    let have_cloud = n.cloud.is_some();
    let mut failed = Vec::new();
    for down in 0..3 {
        let mut alive = [true, true, have_cloud];
        alive[down] = false;
        if let Some(e) = case(&n, alive, &format!("{} down", NAMES[down])).await {
            failed.push(e);
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_layer_alone_carries_changes() {
    let n = net().await;
    let mut failed = Vec::new();
    for only in 0..3 {
        if only == 2 && n.cloud.is_none() {
            println!("cloud alone: skipped (KT_CLOUD_URL not set)");
            continue;
        }
        let mut alive = [false; 3];
        alive[only] = true;
        if let Some(e) = case(&n, alive, &format!("{} alone", NAMES[only])).await {
            failed.push(e);
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
