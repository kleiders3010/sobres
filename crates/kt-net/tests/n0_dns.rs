//! n0's DNS as a best-effort discovery layer (DESIGN §6, D50), live: B knows only A's
//! endpoint ID (no addresses, no relay), finds A through n0's DNS and reaches it through
//! n0's relays. Public services, so `#[ignore]`: `cargo test -p kt-net --test n0_dns -- --ignored`.
//! Both nodes bind to loopback (no firewall prompt); relay traffic is outbound TCP.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::EndpointAddr;
use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::sync::Syncer;

fn node(tag: &str) -> Arc<Syncer> {
    let root = std::env::temp_dir().join(format!("kt-n0-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Syncer::new(Arc::new(Mutex::new(Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap())), vec![])
}

/// Loopback by default (no firewall prompt on the dev PC, but then no home relay, so
/// nothing reaches n0's DNS); `KT_N0_BIND=all` on a device with real networking.
fn bind() -> Option<std::net::SocketAddr> {
    if std::env::var("KT_N0_BIND").as_deref() == Ok("all") { None } else { Some("127.0.0.1:0".parse().unwrap()) }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn found_by_endpoint_id_alone() {
    let cfg = P2pConfig { bind: bind(), n0_relays: true, n0_dns: true, ..Default::default() };
    let a = P2p::start(node("a"), cfg.clone()).await.unwrap();
    let b = P2p::start(node("b"), cfg).await.unwrap();
    let lookup = [7u8; 32];
    a.set_invite_board(vec![(lookup, b"sealed invite record".to_vec())]);
    let id = iroh::EndpointId::from_bytes(&a.where_data().endpoint).unwrap();
    // A's record reaches n0's DNS after it has a home relay; retry for up to a minute.
    let started = std::time::Instant::now();
    let mut got = None;
    while got.is_none() && started.elapsed() < Duration::from_secs(60) {
        got = b.fetch_invite(EndpointAddr::new(id), lookup).await;
        if got.is_none() {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
    assert_eq!(got.as_deref(), Some(&b"sealed invite record"[..]), "found by ID through n0 DNS");
    eprintln!("found and fetched in {:?}", started.elapsed());
    a.close().await;
    b.close().await;
}

/// Diagnostics: does A get an n0 home relay, and can B reach A through it directly?
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn diagnose_relay_path() {
    let cfg = P2pConfig { bind: bind(), n0_relays: true, n0_dns: true, ..Default::default() };
    let a = P2p::start(node("da"), cfg.clone()).await.unwrap();
    let b = P2p::start(node("db"), cfg).await.unwrap();
    let lookup = [8u8; 32];
    a.set_invite_board(vec![(lookup, b"x".to_vec())]);
    for i in 0..10 {
        let w = a.where_data();
        eprintln!("t={}s relays={:?} ips={:?}", i * 2, w.relays, w.ips);
        if !w.relays.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let full = a.where_data().to_addr().unwrap();
    eprintln!("direct via relay: {:?}", b.fetch_invite(full, lookup).await.is_some());
}
