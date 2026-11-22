//! "Nostr is completely down": every Nostr relay is dead for everyone. Helpers are found
//! on the DHT, invites travel over the DHT, helpers and devices, joins complete directly or
//! through helpers, and open apps stay in sync live. Loopback only.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::invite::{Code, CodeKind};
use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};
use kt_net::dht::DhtDiscovery;
use kt_net::helper::{HelperLimits, HelperMailbox, HelperRef};
use kt_net::helper_dir::{maintain_with_dht, HelperDirectory};
use kt_net::invite_net::{fetch, join, publish, InviteLayers};
use kt_net::live::run_p2p_live;
use kt_net::mailbox::Mailbox;
use kt_net::nostr::{NostrMailbox, NostrPolicy};
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;
use mainline::Testnet;

const DEAD_NOSTR: &str = "ws://127.0.0.1:9";
const MONTH: u64 = 30 * 86_400_000;

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-nn-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
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

struct Device {
    s: Arc<Syncer>,
    p2p: Arc<P2p>,
    hmb: Arc<HelperMailbox>,
    dht: Arc<DhtDiscovery>,
    nostr: Arc<NostrMailbox>,
    dir: HelperDirectory,
}

impl Device {
    async fn new(tag: &str, bootstrap: &[String]) -> Device {
        Self::from_replica(Arc::new(Mutex::new(replica(tag))), bootstrap).await
    }

    /// The same device starting again (new endpoint, same data), e.g. after being offline.
    async fn from_replica(rep: Arc<Mutex<Replica>>, bootstrap: &[String]) -> Device {
        // One syncer for everything: the helper mailbox is added once the endpoint exists.
        let s = Syncer::new(rep.clone(), vec![]);
        let p2p = P2p::start(s.clone(), cfg(false)).await.unwrap();
        p2p.publish_whereabouts();
        let hmb = Arc::new(HelperMailbox::new(p2p.clone(), vec![]));
        s.add_mailbox(hmb.clone() as Arc<dyn Mailbox>);
        kt_net::helper_dir::install_resolver(&s, &hmb);
        let dht = Arc::new(DhtDiscovery::new(rep, Some(bootstrap.to_vec()), Some(Ipv4Addr::LOCALHOST)).unwrap());
        Device { s, p2p, hmb, dht, nostr: Arc::new(NostrMailbox::new(&[DEAD_NOSTR], NostrPolicy::default())), dir: HelperDirectory::new(None) }
    }

    fn layers(&self) -> InviteLayers {
        InviteLayers { p2p: Some(self.p2p.clone()), dht: Some(self.dht.clone()), nostr: Some(self.nostr.clone()), helpers: Some(self.hmb.clone()), lan: None }
    }

    async fn maintain(&self) {
        maintain_with_dht(&self.s, &self.hmb, &self.nostr, &self.dir, Some(&self.dht)).await;
    }

    fn value(&self, g: &GroupId, d: &DocId) -> String {
        format!("{:?}", self.s.replica().lock().unwrap().doc(g, d).map(|x| x.get_deep_value()))
    }

    fn member_of(&self, g: &GroupId) -> bool {
        let r = self.s.replica();
        let r = r.lock().unwrap();
        r.epochs(g).is_some_and(|e| e.is_current_member(&r.device_id()))
    }
}

async fn wait(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let t = std::time::Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn everything_works_with_nostr_completely_down() {
    let testnet = Testnet::builder(10).build().unwrap();
    let bs = testnet.bootstrap.clone();
    let day = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 / 86_400_000) as u32;

    // Strangers' helpers, posted in the DHT slots.
    let mut helpers = Vec::new();
    for i in 0..3 {
        let s = Syncer::new(Arc::new(Mutex::new(replica(&format!("h{i}")))), vec![]);
        let p = P2p::start(s.clone(), cfg(true)).await.unwrap();
        let d = DhtDiscovery::new(s.replica(), Some(bs.clone()), Some(Ipv4Addr::LOCALHOST)).unwrap();
        assert!(d.announce_helper(&HelperRef::from_addr(&p.where_data().to_addr().unwrap()), p.endpoint().secret_key(), day).await);
        helpers.push(p);
    }

    // A: a group with a list; finds helpers on the DHT; invites.
    let a = Device::new("a", &bs).await;
    let (g, d) = {
        let r = a.s.replica();
        let mut r = r.lock().unwrap();
        let g = r.create_group().unwrap();
        let d = r.create_doc(&g).unwrap();
        r.doc(&g, &d).unwrap().get_map("tasks").insert("t1", "milk").unwrap();
        r.commit(&g, &d).unwrap();
        (g, d)
    };
    a.maintain().await;
    assert!(a.dir.candidates().len() >= 3, "A found helpers on the DHT");
    let (code, _) = a.s.replica().lock().unwrap().create_invite(&g, CodeKind::Invite, MONTH).unwrap();
    a.maintain().await; // records the group's helper set (goes into the invite's hints)
    let rep = publish(&a.s, &a.layers(), &|_| ("Family".into(), "Alex".into())).await;
    assert!(rep.records == 1 && rep.dht == 1 && rep.helpers == 1, "{rep:?}");
    a.s.write_round().await;

    // B: a fresh device with only the 8 words.
    let b = Device::new("b", &bs).await;
    b.maintain().await; // finds helpers on the DHT too
    let words = Code::parse(&code.to_text()).unwrap();
    let (rec, source) = fetch(&b.layers(), &words, CodeKind::Invite, &[], Duration::from_millis(0)).await.expect("record found without Nostr");
    println!("B found the invite via {source:?}");
    assert_eq!(rec.group_name, "Family");
    assert!(join(&b.s, Some(&b.p2p), &rec).await.unwrap(), "joined directly through A");
    assert!(b.member_of(&g));
    assert!(wait(10, || b.value(&g, &d).contains("milk")).await, "B reads the list");

    // Both apps open: live over P2P.
    let (_s1, r1) = tokio::sync::watch::channel(false);
    let (_s2, r2) = tokio::sync::watch::channel(false);
    tokio::spawn(run_p2p_live(a.s.clone(), a.p2p.clone(), r1));
    tokio::spawn(run_p2p_live(b.s.clone(), b.p2p.clone(), r2));
    a.s.replica().lock().unwrap().doc(&g, &d).unwrap().get_map("tasks").insert("t2", "eggs").unwrap();
    a.s.replica().lock().unwrap().commit(&g, &d).unwrap();
    assert!(wait(10, || b.value(&g, &d).contains("eggs")).await, "live update without Nostr");

    // C joins while A is offline for it: the request waits at the helpers.
    let c = Device::new("c", &bs).await;
    c.maintain().await;
    let (rec, _) = fetch(&c.layers(), &words, CodeKind::Invite, &[], Duration::from_millis(0)).await.expect("C finds the record");
    {
        let r = c.s.replica();
        r.lock().unwrap().begin_join(&rec).unwrap();
    }
    c.s.write_round().await; // join request -> the group's helpers (from the record's hints)
    // B (a member now) reads the helpers and accepts C, then answers where C listens.
    for _ in 0..3 {
        b.maintain().await;
        b.s.sync_once().await;
        c.s.sync_once().await;
        if c.member_of(&g) {
            break;
        }
    }
    assert!(c.member_of(&g), "C was added through helpers while A was away");
    assert!(wait(10, || c.value(&g, &d).contains("eggs")).await || {
        c.s.sync_once().await;
        c.value(&g, &d).contains("eggs")
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn joining_days_later_while_the_inviter_is_offline() {
    use kt_net::invite_net::{fetch_on_day, publish_ahead};
    let testnet = Testnet::builder(10).build().unwrap();
    let bs = testnet.bootstrap.clone();
    let day = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 / 86_400_000) as u32;
    let mut helpers = Vec::new();
    for i in 0..3 {
        let s = Syncer::new(Arc::new(Mutex::new(replica(&format!("oh{i}")))), vec![]);
        let p = P2p::start(s.clone(), cfg(true)).await.unwrap();
        let d = DhtDiscovery::new(s.replica(), Some(bs.clone()), Some(Ipv4Addr::LOCALHOST)).unwrap();
        assert!(d.announce_helper(&HelperRef::from_addr(&p.where_data().to_addr().unwrap()), p.endpoint().secret_key(), day).await);
        helpers.push(p);
    }

    // A invites (valid 6 days), publishes every day's copy, then goes offline.
    let a = Device::new("oa", &bs).await;
    let a_rep = a.s.replica();
    let (g, d) = {
        let mut r = a_rep.lock().unwrap();
        let g = r.create_group().unwrap();
        let d = r.create_doc(&g).unwrap();
        r.doc(&g, &d).unwrap().get_map("tasks").insert("t1", "milk").unwrap();
        r.commit(&g, &d).unwrap();
        (g, d)
    };
    a.maintain().await;
    let (code, _) = a_rep.lock().unwrap().create_invite(&g, CodeKind::Invite, 6 * 86_400_000 - 60_000).unwrap();
    a.maintain().await;
    let ahead = publish_ahead(&a.s, &a.layers(), &|_| ("Family".into(), "Alex".into())).await;
    assert!(ahead >= 6, "a copy for each day: {ahead}");
    a.s.write_round().await;
    a.p2p.close().await;
    drop(a);

    // Three days later, B finds that day's copy on the helpers.
    let b = Device::new("ob", &bs).await;
    b.maintain().await;
    let (rec, source) = fetch_on_day(&b.layers(), &code, CodeKind::Invite, &[], Duration::from_millis(0), day + 3).await.expect("found days later");
    assert!(matches!(source, kt_net::invite_net::Source::Helpers), "{source:?}");
    assert!(!join(&b.s, Some(&b.p2p), &rec).await.unwrap(), "A is offline: not in yet");
    b.s.sync_once().await;
    // pinned: the words only let B ask (audit T10): nothing of the list until a member lets B in.
    assert!(!b.value(&g, &d).contains("milk"), "B reads nothing while waiting");
    assert!(!b.member_of(&g));

    // A comes back: its device reads the request at the helpers and lets B in.
    let a = Device::from_replica(a_rep, &bs).await;
    for _ in 0..3 {
        a.maintain().await;
        a.s.sync_once().await;
        b.maintain().await;
        b.s.sync_once().await;
        if b.member_of(&g) && b.value(&g, &d).contains("milk") {
            break;
        }
    }
    assert!(b.member_of(&g), "A accepted B when it came back");
    assert!(b.value(&g, &d).contains("milk"), "and B reads the list");
    // B's first edit as a member reaches A.
    {
        let r = b.s.replica();
        let mut r = r.lock().unwrap();
        r.doc(&g, &d).unwrap().get_map("tasks").insert("t2", "from B").unwrap();
        r.commit(&g, &d).unwrap();
    }
    for _ in 0..3 {
        b.s.sync_once().await;
        a.s.sync_once().await;
        if a.value(&g, &d).contains("from B") {
            break;
        }
    }
    assert!(a.value(&g, &d).contains("from B"), "B's edit arrived");
}
