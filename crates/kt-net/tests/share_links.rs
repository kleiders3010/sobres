//! Share links and QR contents (DESIGN §5): same code as the words, kind from the path,
//! optional direct addresses, and nothing accepted that isn't a code.

use kt_engine::invite::{Code, CodeKind};
use kt_net::invite_net::{parse_shared, share_url};
use kt_net::p2p::WhereData;

#[test]
fn links_round_trip() {
    let code = Code::parse(&kt_engine::invite::words()[100..108].join(" ")).unwrap();
    let url = share_url(&code, CodeKind::Invite, None);
    assert!(url.starts_with("https://apps.jurisgames.com/join#"), "{url}");
    let s = parse_shared(&url).unwrap();
    assert_eq!(s.code.to_text(), code.to_text());
    assert_eq!(s.kind, Some(CodeKind::Invite));
    assert!(s.direct.is_none());

    let link = share_url(&code, CodeKind::Link, None);
    assert!(link.contains("/link#"));
    assert_eq!(parse_shared(&link).unwrap().kind, Some(CodeKind::Link));

    // Plain words (spaces or hyphens) still work; no kind.
    let words = code.to_words().join(" ");
    let s = parse_shared(&format!("  {words} ")).unwrap();
    assert_eq!(s.code.to_text(), code.to_text());
    assert_eq!(s.kind, None);

    // With the sharing device's addresses.
    let key = iroh::SecretKey::from_bytes(&[7u8; 32]);
    let w = WhereData { endpoint: *key.public().as_bytes(), relays: vec!["https://relay.example/".into()], ips: vec!["192.0.2.7:4444".parse().unwrap()], dht: None };
    let url = share_url(&code, CodeKind::Invite, Some(&w));
    let s = parse_shared(&url).unwrap();
    assert!(s.direct.is_some(), "{url}");

    // Garbage after `~` is ignored, not fatal; garbage words are refused.
    assert!(parse_shared(&format!("https://apps.jurisgames.com/join#{}~!!notbase64", code.to_words().join("-"))).unwrap().direct.is_none());
    assert!(parse_shared("https://apps.jurisgames.com/join#not-a-code").is_err());
    assert!(parse_shared("").is_err());
}
