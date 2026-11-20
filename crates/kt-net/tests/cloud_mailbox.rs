//! Cloud mailbox (Cloudflare Worker, `cloud/mailbox-worker`) against a running instance:
//! `KT_CLOUD_URL=http://127.0.0.1:8787` with `npm run dev` there (local simulator), or the
//! deployed URL. Skipped when unset.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_net::cloud::{put_message, CloudMailbox, CloudMode};
use kt_net::mailbox::{Mailbox, SlotWrite, WriteOutcome};
use kt_net::sync::Syncer;

fn url() -> Option<String> {
    std::env::var("KT_CLOUD_URL").ok()
}

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-cloud-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn members_sync_through_the_cloud_mailbox_alone() {
    let Some(url) = url() else { return };
    let (mut ra, mut rb) = (replica("a"), replica("b"));
    let g = ra.create_group().unwrap();
    let d = ra.create_doc(&g).unwrap();
    ra.add_member(&g, rb.device_public()).unwrap();
    rb.accept_group(g);
    for m in ra.export_group(&g).unwrap() {
        let _ = rb.receive(&m);
    }
    let (a_rep, b_rep) = (Arc::new(Mutex::new(ra)), Arc::new(Mutex::new(rb)));
    let cloud = |_| Arc::new(CloudMailbox::new(&url, CloudMode::Always).unwrap()) as Arc<dyn Mailbox>;
    let a = Syncer::new(a_rep.clone(), vec![cloud(0)]);
    let b = Syncer::new(b_rep.clone(), vec![cloud(1)]);

    a_rep.lock().unwrap().doc(&g, &d).unwrap().get_map("tasks").insert("t1", "through the cloud").unwrap();
    a_rep.lock().unwrap().commit(&g, &d).unwrap();
    let w = a.write_round().await;
    assert!(w.written >= 1, "{w:?}");
    let r = b.read_round().await;
    assert!(r.slots_opened >= 1, "{r:?}");
    let v = format!("{:?}", b_rep.lock().unwrap().doc(&g, &d).unwrap().get_deep_value());
    assert!(v.contains("through the cloud"), "{v}");

    // A newer edit: the reader gets only what changed since its last revision.
    a_rep.lock().unwrap().doc(&g, &d).unwrap().get_map("tasks").insert("t2", "and again").unwrap();
    a_rep.lock().unwrap().commit(&g, &d).unwrap();
    a.write_round().await;
    b.read_round().await;
    let v = format!("{:?}", b_rep.lock().unwrap().doc(&g, &d).unwrap().get_deep_value());
    assert!(v.contains("and again"), "{v}");
}

#[tokio::test]
async fn the_worker_enforces_the_rules() {
    let Some(url) = url() else { return };
    let mb = CloudMailbox::new(&url, CloudMode::Always).unwrap();
    let topic: [u8; 32] = rand::random();
    let w = SlotWrite { topic, writer_seed: [1; 32], blob: b"sealed slot".to_vec() };
    assert_eq!(mb.write(&w).await, WriteOutcome::Stored { stored: 1, tried: 1 }, "first write: proof of work, then stored");
    assert_eq!(mb.write(&w).await, WriteOutcome::Stored { stored: 1, tried: 1 }, "a refresh needs none");
    let got = mb.read(&[topic]).await;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].blob, b"sealed slot");
    assert_eq!(got[0].writer, mb.writer_id(&[1; 32]));

    // Too large: refused before sending; forged or stale puts: refused by the Worker.
    let big = SlotWrite { blob: vec![0; kt_net::cloud::MAX_SLOT + 1], ..w.clone() };
    assert!(matches!(mb.write(&big).await, WriteOutcome::Refused(_)));
    let client = reqwest::Client::new();
    let key = iroh::SecretKey::from_bytes(&[5; 32]);
    let put = |version: u64, sign_version: u64, blob: &'static [u8]| {
        let mut body = key.public().as_bytes().to_vec();
        body.extend_from_slice(&version.to_le_bytes());
        body.extend_from_slice(&kt_net::cloud::solve_pow(&topic, key.public().as_bytes(), version, 16).to_le_bytes());
        body.extend_from_slice(&key.sign(&put_message(&topic, sign_version, blob)).to_bytes());
        body.extend_from_slice(blob);
        client.put(format!("{url}/v1/t/{}", hex::encode(topic))).body(body).send()
    };
    let forged = put(10, 11, b"x").await.unwrap();
    assert_eq!(forged.status().as_u16(), 400, "signature must match");
    assert_eq!(put(10, 10, b"x").await.unwrap().text().await.unwrap(), "stored");
    assert_eq!(put(9, 9, b"older").await.unwrap().text().await.unwrap(), "stale");
    assert_eq!(mb.read(&[topic]).await.len(), 2, "two writers on the topic");
}
