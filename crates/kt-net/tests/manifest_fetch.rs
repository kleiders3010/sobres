//! Operator manifest fetched over Nostr (spam and forgeries ignored) and over HTTP.

use base64::Engine as _;
use kt_engine::crypto::DeviceSecret;
use kt_net::manifest::{sign, Manifest, ManifestBody, Weighted, MANIFEST_KIND, MANIFEST_TOPIC_TAG};
use kt_net::nostr::{NostrPolicy, RelayConn};
use kt_net::testrelay::TestRelay;
use nostr::prelude::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn body(v: u64, relay: &str) -> ManifestBody {
    ManifestBody { version: v, issued_ms: 0, nostr_relays: vec![Weighted { item: relay.into(), weight: 1 }], ..Default::default() }
}

async fn publish(relay: &RelayConn, content: &[u8]) {
    let keys = Keys::generate();
    let ev = EventBuilder::new(Kind::Custom(MANIFEST_KIND), base64::engine::general_purpose::STANDARD.encode(content))
        .tags([Tag::identifier("m"), Tag::hashtag(MANIFEST_TOPIC_TAG)])
        .finalize(&keys)
        .unwrap();
    relay.publish(ev).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_over_nostr_ignores_spam_and_forgeries() {
    let op = DeviceSecret::generate();
    let forger = DeviceSecret::generate();
    let tr = TestRelay::start(None).await.unwrap();
    let conn = RelayConn::new(&tr.url(), NostrPolicy::default());
    publish(&conn, &sign(&body(4, "wss://real.example"), &op)).await;
    publish(&conn, &sign(&body(99, "wss://forged.example"), &forger)).await;
    publish(&conn, b"spam spam spam").await;
    let m = Manifest::new(op.public().sign, body(1, "wss://base.example"), None);
    assert_eq!(m.fetch_nostr(&[conn]).await, 1);
    assert_eq!(m.version(), 4);
    let relays: Vec<String> = m.effective().nostr_relays.into_iter().map(|w| w.item).collect();
    assert!(relays.contains(&"wss://real.example".to_string()) && !relays.iter().any(|r| r.contains("forged")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_over_http() {
    let op = DeviceSecret::generate();
    let signed = sign(&body(7, "wss://http.example"), &op);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let signed = signed.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", signed.len());
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(&signed).await;
            });
        }
    });
    let m = Manifest::new(op.public().sign, body(1, "wss://base.example"), None);
    let url = format!("http://{addr}/manifest");
    assert_eq!(m.fetch_http(&[&url, "http://127.0.0.1:9/nothing"]).await, 1);
    assert_eq!(m.version(), 7);
}

/// The published manifest, fetched the way devices do and checked against the real key
/// (network; run on request: `cargo test -p kt-net --test manifest_fetch -- --ignored`).
#[tokio::test]
#[ignore]
async fn the_published_manifest_verifies() {
    for url in kt_net::manifest::MANIFEST_URLS {
        let m = kt_net::manifest::Manifest::new(kt_net::manifest::OPERATOR_PUBLIC_KEY, ManifestBody::default(), None);
        assert_eq!(m.fetch_http(&[url]).await, 1, "{url}");
        let e = m.effective();
        assert!(e.cloud_mailboxes.iter().any(|w| w.item == kt_net::cloud::DEFAULT_URL), "{url}: {e:?}");
    }
}
