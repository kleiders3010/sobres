//! Measurement only (D23): the extra idle endpoints used to price a per-group network
//! identity start with the real endpoint's relay settings, each get a home relay of their
//! own under their own ID, and close cleanly.

#![cfg(feature = "relay-server")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_net::helper_relay::{HelperRelay, HelperRelayConfig};
use kt_net::measure::ExtraEndpoints;
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;

fn replica() -> Replica {
    let root = std::env::temp_dir().join(format!("kt-measure-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extra_endpoints_each_hold_a_home_relay() {
    let relay = HelperRelay::start(HelperRelayConfig::default()).await.unwrap();
    let cfg = P2pConfig { relays: vec![relay.url().unwrap()], relay_only: true, ..Default::default() };
    let p = P2p::start(Syncer::new(Arc::new(Mutex::new(replica())), vec![]), cfg).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), p.endpoint().online()).await.expect("real endpoint online");

    let extra = ExtraEndpoints::start(&p, 3).await.unwrap();
    assert_eq!(extra.len(), 3);
    let ids = extra.ids();
    let main = *p.endpoint().id().as_bytes();
    assert!(!ids.contains(&main) && ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2], "each has its own ID");
    tokio::time::timeout(Duration::from_secs(15), extra.all_online()).await.expect("every extra endpoint gets a home relay");
    assert_eq!(extra.online(), 3);
    extra.network_change().await; // survives the platform's network-change nudge
    extra.close().await;

    // Same device, same derived IDs next time (relays see stable identities).
    let again = ExtraEndpoints::start(&p, 1).await.unwrap();
    assert_eq!(again.ids(), vec![ids[0]]);
    again.close().await;
    assert!(p.is_online(), "the real endpoint is untouched");
}
