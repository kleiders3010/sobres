//! Helper network: a stranger's device keeps a group's slots in RAM while the group's
//! members are never online at the same time.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_net::helper::{HelperLimits, HelperMailbox};
use kt_net::mailbox::Mailbox;
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-help-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

fn cfg() -> P2pConfig {
    P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), ..Default::default() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_strangers_helper_carries_changes_between_members_who_never_meet() {
    // The helper: someone else's device, helping by default.
    let hs = Syncer::new(Arc::new(Mutex::new(replica("h"))), vec![]);
    let hp = P2p::start(hs.clone(), P2pConfig { helper: Some(HelperLimits { base_pow_bits: 8, ..Default::default() }), ..cfg() }).await.unwrap();
    let helper_addr = hp.where_data().to_addr().unwrap();

    let (mut ra, mut rb) = (replica("a"), replica("b"));
    let g = ra.create_group().unwrap();
    let d = ra.create_doc(&g).unwrap();
    ra.doc(&g, &d).unwrap().get_map("tasks").insert("t0", "start").unwrap();
    ra.commit(&g, &d).unwrap();
    ra.add_member(&g, rb.device_public()).unwrap();
    rb.accept_group(g);
    for m in ra.export_group(&g).unwrap() {
        let _ = rb.receive(&m);
    }
    let _ = ra.outbox();

    // A: online, edits, writes to the helper, goes offline.
    let (a_rep, b_rep) = (Arc::new(Mutex::new(ra)), Arc::new(Mutex::new(rb)));
    let a0 = Syncer::new(a_rep.clone(), vec![]);
    let ap = P2p::start(a0.clone(), cfg()).await.unwrap();
    let amb = Arc::new(HelperMailbox::new(ap.clone(), vec![helper_addr.clone()]));
    let a = Syncer::new(a_rep.clone(), vec![amb.clone() as Arc<dyn Mailbox>]);
    a_rep.lock().unwrap().doc(&g, &d).unwrap().get_map("tasks").insert("t1", "left with a helper").unwrap();
    a_rep.lock().unwrap().commit(&g, &d).unwrap();
    let w = a.write_round().await;
    assert!(w.written >= 1, "stored on the helper (after solving its proof of work): {w:?}");
    ap.close().await;
    let used = hp.helper().unwrap().info().used;
    assert!(used > 0);

    // B: comes online later, reads from the helper.
    let b0 = Syncer::new(b_rep.clone(), vec![]);
    let bp = P2p::start(b0.clone(), cfg()).await.unwrap();
    let bmb = Arc::new(HelperMailbox::new(bp.clone(), vec![helper_addr]));
    let b = Syncer::new(b_rep.clone(), vec![bmb.clone() as Arc<dyn Mailbox>]);
    let r = b.read_round().await;
    assert!(r.slots_opened >= 1, "{r:?}");
    let v = format!("{:?}", b_rep.lock().unwrap().doc(&g, &d).unwrap().get_deep_value());
    assert!(v.contains("left with a helper"), "{v}");

    // The helper holds only ciphertext for a group it isn't in.
    assert!(hs.replica().lock().unwrap().groups().is_empty());
    let rec = bmb.records();
    assert!(rec.values().all(|r| r.reachable > 0 && r.withheld == 0), "{rec:?}");
}

use kt_net::helper::{HelperRef, SignedPut};
use kt_net::mailbox::SlotWrite;

async fn helper_node(tag: &str, limits: HelperLimits) -> (Arc<P2p>, iroh::EndpointAddr) {
    let s = Syncer::new(Arc::new(Mutex::new(replica(tag))), vec![]);
    let p = P2p::start(s, P2pConfig { helper: Some(limits), ..cfg() }).await.unwrap();
    let a = p.where_data().to_addr().unwrap();
    (p, a)
}

fn cheap() -> HelperLimits {
    HelperLimits { base_pow_bits: 4, ..Default::default() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn helpers_replicate_to_the_rest_of_the_set() {
    let mut nodes = Vec::new();
    for i in 0..4 {
        nodes.push(helper_node(&format!("r{i}"), cheap()).await);
    }
    let addrs: Vec<_> = nodes.iter().map(|n| n.1.clone()).collect();
    let client = P2p::start(Syncer::new(Arc::new(Mutex::new(replica("rc"))), vec![]), cfg()).await.unwrap();
    let mb = HelperMailbox::new(client, addrs).with_direct_writes(2);
    let topic = [5u8; 32];
    let out = mb.write(&SlotWrite { topic, writer_seed: [1; 32], blob: vec![7; 500] }).await;
    assert!(matches!(out, kt_net::mailbox::WriteOutcome::Stored { stored: 2, .. }), "{out:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let holding = nodes.iter().filter(|n| !n.0.helper().unwrap().get(&[topic], &[]).is_empty()).count();
    assert_eq!(holding, 4, "2 direct uploads, the other 2 by replication");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_helper_hands_off_before_going_offline() {
    let (h1, _) = helper_node("h1", cheap()).await;
    let (h2, a2) = helper_node("h2", cheap()).await;
    // A slot held only by h1 (its set includes h2), e.g. h2 was offline when it was written.
    let key = iroh::SecretKey::from_bytes(&[3; 32]);
    let topic = [8u8; 32];
    let blob = vec![1u8; 300];
    let mut m = b"kt/helper-put/v1".to_vec();
    m.extend_from_slice(&topic);
    m.extend_from_slice(&1u64.to_le_bytes());
    m.extend_from_slice(blake3::hash(&blob).as_bytes());
    let writer = *key.public().as_bytes();
    let nonce = kt_net::helper::solve_pow(&topic, &writer, 1, 20);
    let put = SignedPut { topic, writer, version: 1, blob, sig: key.sign(&m).to_bytes().to_vec(), nonce };
    assert_eq!(h1.helper().unwrap().put_signed(put, vec![HelperRef::from_addr(&a2)]), kt_net::helper::PutReply::Stored);
    assert!(h2.helper().unwrap().get(&[topic], &[]).is_empty());
    assert_eq!(h1.handoff().await, 1);
    h1.close().await;
    assert_eq!(h2.helper().unwrap().get(&[topic], &[]).len(), 1, "h2 has it after the handoff");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eviction_for_space_is_declared_not_scored_as_withholding() {
    let tiny = HelperLimits { capacity: 1500, per_topic: 1500, base_pow_bits: 2, young_age: Duration::from_secs(0), ..Default::default() };
    let (_h, a) = helper_node("ev", tiny).await;
    let client = P2p::start(Syncer::new(Arc::new(Mutex::new(replica("evc"))), vec![]), cfg()).await.unwrap();
    let mb = HelperMailbox::new(client.clone(), vec![a.clone()]);
    let mine = [1u8; 32];
    mb.write(&SlotWrite { topic: mine, writer_seed: [1; 32], blob: vec![1; 1000] }).await;
    // Someone else's slot pushes ours out.
    let other = HelperMailbox::new(client, vec![a]);
    other.write(&SlotWrite { topic: [2u8; 32], writer_seed: [2; 32], blob: vec![2; 1000] }).await;
    let got = mb.read(&[mine]).await;
    assert!(got.is_empty());
    assert_eq!(*mb.declared_evictions.lock().unwrap(), 1);
    assert!(mb.records().values().all(|r| r.withheld == 0));
}
