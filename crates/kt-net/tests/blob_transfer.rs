//! Blob transfer (text attachments): straight from a device that has the blob, or from the
//! group's helpers while the author is offline. Only the exact bytes are ever accepted.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::blobs::{self, BlobStore};
use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_net::helper::{HelperLimits, HelperMailbox};
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;

fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kt-blobnet-{tag}-{}-{}", std::process::id(), rand::random::<u32>()))
}

fn replica(tag: &str) -> Replica {
    let root = tmp(tag);
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

async fn node(tag: &str, helper: bool) -> Arc<P2p> {
    let s = Syncer::new(Arc::new(Mutex::new(replica(tag))), vec![]);
    let cfg = P2pConfig { bind: Some("127.0.0.1:0".parse().unwrap()), helper: helper.then(|| HelperLimits { base_pow_bits: 4, ..Default::default() }), ..Default::default() };
    P2p::start(s, cfg).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blobs_travel_directly_and_through_helpers() {
    let text = "A long note that became an attachment. ".repeat(4000);
    let (r, sealed) = blobs::seal(text.as_bytes()).unwrap();
    let store_a = BlobStore::new(tmp("sa"));
    store_a.put(&sealed).unwrap();

    // Directly from a device that has it.
    let (a, b) = (node("a", false).await, node("b", false).await);
    a.set_blob_store(store_a.clone());
    let a_addr = a.where_data().to_addr().unwrap();
    let got = b.fetch_blob(a_addr.clone(), r.id).await.expect("served by ID");
    assert_eq!(blobs::open(&r, &got).unwrap(), text.as_bytes());
    assert!(b.fetch_blob(a_addr, [9; 32]).await.is_none(), "unknown ID: nothing");

    // Through a helper: A offers it, goes offline; B fetches it later.
    let h = node("h", true).await;
    let h_addr = h.where_data().to_addr().unwrap();
    let amb = HelperMailbox::new(a.clone(), vec![h_addr.clone()]);
    let rep = kt_net::blobs::round(None, Some(&amb), None, &store_a, &[], &[r.id]).await;
    assert_eq!(rep.offered, 1, "{rep:?}");
    let rep = kt_net::blobs::round(None, Some(&amb), None, &store_a, &[], &[r.id]).await;
    assert_eq!(rep.offered, 0, "offered again only when due");
    assert_eq!(h.helper().unwrap().blobs.held().0, 1);
    a.close().await;

    let store_b = BlobStore::new(tmp("sb"));
    let bmb = HelperMailbox::new(b.clone(), vec![h_addr]);
    let rep = kt_net::blobs::round(Some(&b), Some(&bmb), None, &store_b, &[r.id], &[]).await;
    assert_eq!((rep.fetched, rep.missing), (1, 0), "{rep:?}");
    assert_eq!(blobs::open(&r, &store_b.get(&r.id).unwrap()).unwrap(), text.as_bytes());

    // Something nobody has stays missing, without failing the round.
    let rep = kt_net::blobs::round(Some(&b), Some(&bmb), None, &store_b, &[[7; 32]], &[]).await;
    assert_eq!((rep.fetched, rep.missing), (0, 1));
}
