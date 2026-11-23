//! Which public Blossom servers take our blobs? (Run on request: `cargo test -p kt-net
//! --test blossom_probe -- --ignored --nocapture`.) Per server, like one real device would:
//! upload one sealed blob of a small and a typical attachment size (BUD-02, signed kind
//! 24242 authorization), read it back, delete it.

use base64::Engine as _;
use nostr::prelude::*;
use sha2::{Digest, Sha256};

// 2026-09-25: only these two took application/octet-stream from any key; the others were
// media-only (blossom.primal.net, blossom.band, cdn.nostrcheck.me, blossom.nostr.build),
// or allowlists (nostrmedia.com, blossom.oxtr.dev, blossom.f7z.io).
const SERVERS: [&str; 2] = ["https://nostr.download", "https://cdn.hzrd149.com"];

fn auth(keys: &Keys, verb: &str, sha: &str) -> String {
    let exp = Timestamp::now() + 300;
    let ev = EventBuilder::new(Kind::Custom(24242), format!("{verb} blob"))
        .tags([
            Tag::parse(["t", verb]).unwrap(),
            Tag::parse(["x", sha]).unwrap(),
            Tag::expiration(exp),
        ])
        .finalize(keys)
        .unwrap();
    format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(ev.as_json()))
}

#[tokio::test]
#[ignore]
async fn which_blossom_servers_take_our_blobs() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().unwrap();
    let keys = Keys::generate();
    for server in SERVERS {
        let mut line = format!("{server}:");
        for size in [2_000usize, 300_000, 3_000_000] {
            // What a sealed blob looks like: random-looking bytes.
            let (_, data) = kt_engine::blobs::seal(&(0..size).map(|_| rand::random::<u8>()).collect::<Vec<u8>>()).unwrap();
            let sha = hex::encode(Sha256::digest(&data));
            let put = client
                .put(format!("{server}/upload"))
                .header("Authorization", auth(&keys, "upload", &sha))
                .header("Content-Type", "application/octet-stream")
                .body(data.clone())
                .send()
                .await;
            let (status, reason) = match &put {
                Ok(r) => (r.status().as_u16(), r.headers().get("x-reason").and_then(|v| v.to_str().ok()).unwrap_or("").to_string()),
                Err(e) => (0, e.to_string().chars().take(60).collect()),
            };
            let mut read = "-";
            if (200..300).contains(&status) {
                read = match client.get(format!("{server}/{sha}")).send().await {
                    Ok(r) if r.status().is_success() => match kt_net::http::bytes_capped(r, data.len() + 1024).await {
                        Some(b) if Sha256::digest(&b).as_slice() == Sha256::digest(&data).as_slice() => "read back",
                        _ => "read WRONG",
                    },
                    Ok(r) if r.status().as_u16() == 404 => "not found",
                    _ => "read failed",
                };
                let _ = client.delete(format!("{server}/{sha}")).header("Authorization", auth(&keys, "delete", &sha)).send().await;
            }
            line += &format!(" [{} KB: {status} {reason} {read}]", data.len() / 1024);
        }
        println!("{line}");
    }
}

/// The Blossom layer as devices use it, against the seed servers: offer, find it there on
/// the next offer (no second upload), fetch it back exactly.
#[tokio::test]
#[ignore]
async fn blossom_layer_round_trip() {
    let b = kt_net::blossom::Blossom::seeded().unwrap();
    let (r, sealed) = kt_engine::blobs::seal(format!("probe {}", rand::random::<u64>()).repeat(50).as_bytes()).unwrap();
    assert_eq!(b.offer(&sealed).await, kt_net::blossom::COPIES, "both seed servers took it");
    assert_eq!(b.offer(&sealed).await, kt_net::blossom::COPIES, "already there (HEAD), no upload");
    let got = b.get(&r.id).await.expect("fetched back");
    assert_eq!(kt_engine::blobs::open(&r, &got).unwrap().len() as u64, r.size);
    assert!(b.get(&[1; 32]).await.is_none());
}

/// Discovery as devices run it, against real relays: rank the servers Nostr users list,
/// try the top new ones (each trial is one small upload, read back, deleted).
#[tokio::test]
#[ignore]
async fn discovery_finds_working_servers() {
    let relays: Vec<_> = ["wss://relay.damus.io", "wss://nos.lol", "wss://relay.primal.net"].iter().map(|u| kt_net::nostr::RelayConn::new(u, kt_net::nostr::NostrPolicy::default())).collect();
    let mut events = Vec::new();
    for r in &relays {
        let f = Filter::new().kind(Kind::Custom(kt_net::blossom::SERVER_LIST_KIND)).limit(300);
        events.extend(r.query(vec![f]).await.unwrap_or_default());
    }
    let ranked = kt_net::blossom::rank_candidates(&events);
    println!("{} server lists, top candidates: {:?}", events.len(), ranked.iter().take(12).collect::<Vec<_>>());
    let b = kt_net::blossom::Blossom::seeded().unwrap();
    let d = kt_net::blossom::BlossomDirectory::new(None);
    println!("will try: {:?}", d.fresh(ranked.clone(), &b.servers()));
    let passed = d.discover(&b, &relays).await;
    println!("passed: {passed:?}");
    assert!(!events.is_empty(), "relays returned server lists");
}
