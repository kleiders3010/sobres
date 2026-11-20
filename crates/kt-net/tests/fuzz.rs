//! Fuzzing kt-net's public parsers (DESIGN §9): share links, the update manifest, the
//! operator manifest, helper announcements and refs, and the files it keeps on disk.

use kt_engine::invite::{Code, CodeKind};

#[test]
fn share_links() {
    let code = Code::generate();
    let seeds = vec![
        kt_net::invite_net::share_url(&code, CodeKind::Invite, None).into_bytes(),
        kt_net::invite_net::share_url(&code, CodeKind::Link, None).into_bytes(),
        format!("https://apps.jurisgames.com/join#{}~AAECAwQ", code.to_words().join("-")).into_bytes(),
        code.to_text().into_bytes(),
    ];
    kt_fuzz::run("share-links", &seeds, 5.0, |b| {
        let _ = kt_net::invite_net::parse_shared(&String::from_utf8_lossy(b));
    });
}

#[test]
fn update_manifest() {
    let seed = br#"{"format":1,"apps":{"tasks":{"version_name":"0.2.0","version_code":2,"android":{"play":"https://play.google.com/store/apps/details?id=com.jurisgames.tasks","apk":"https://github.com/kleiders3010/jurisgames/releases"},"desktop":{"windows":"https://github.com/kleiders3010/jurisgames/releases","linux":"https://github.com/kleiders3010/jurisgames/releases"}}}}"#;
    use kt_net::update::{parse, Channel};
    kt_fuzz::run("update-manifest", &[seed.to_vec()], 5.0, |b| {
        for ch in [Channel::Play, Channel::Apk, Channel::Windows, Channel::Linux] {
            if let Some(u) = parse(b, "tasks", 1, ch) {
                assert!(u.link.starts_with("https://"), "only our own links survive: {}", u.link);
            }
        }
    });
}

#[test]
fn operator_manifest() {
    use kt_net::manifest::{sign, verify, Manifest, ManifestBody};
    let op = kt_engine::crypto::DeviceSecret::generate();
    let op_pub = op.public().sign;
    let body = ManifestBody { version: 2, issued_ms: 1, ..Default::default() };
    let signed = sign(&body, &op);
    let m = Manifest::new(op_pub, ManifestBody { version: 1, ..Default::default() }, None);
    kt_fuzz::run("operator-manifest", &[signed], 2.0, |b| {
        let _ = verify(b, &op_pub);
        let _ = m.consider(b);
    });
}

#[test]
fn helper_announcements() {
    let r = kt_net::helper::HelperRef { endpoint: [4; 32], ips: vec!["192.0.2.4:1".parse().unwrap()], relays: vec!["https://relay.example/".into()], relay: Some("https://192.0.2.4:4443".into()) };
    let a = kt_net::helper_dir::Announce { helper: r.clone(), capacity: 1 << 20, session_start_ms: 5 };
    let seeds = vec![postcard::to_stdvec(&a).unwrap(), hex::encode(postcard::to_stdvec(&r).unwrap()).into_bytes()];
    kt_fuzz::run("helper-announce", &seeds, 2.0, |b| {
        let _ = postcard::from_bytes::<kt_net::helper_dir::Announce>(b).map(|a| a.helper.to_addr());
        let _ = kt_net::helper_dir::decode_ref(&String::from_utf8_lossy(b));
    });
}

#[test]
fn files_on_disk() {
    // Local files can still be damaged: loading them must never fail hard.
    let dir = std::env::temp_dir().join(format!("kt-fuzz-files-{}-{}", std::process::id(), rand::random::<u32>()));
    std::fs::create_dir_all(&dir).unwrap();
    let (slots, relays, helpers) = (dir.join("slots.store"), dir.join("relays.dir"), dir.join("helpers.dir"));
    let seeds = vec![vec![0u8; 8], postcard::to_stdvec(&(vec![([1u8; 32], [2u8; 32], 3u64)], 7u64)).unwrap()];
    kt_fuzz::run("net-files", &seeds, 0.3, |b| {
        for p in [&slots, &relays, &helpers] {
            let _ = std::fs::write(p, b);
        }
        let _ = kt_net::slotstore::SlotStore::open(Some(slots.clone()));
        let _ = kt_net::relays::RelayDirectory::new(&["wss://relay.example"], Some(relays.clone()));
        let _ = kt_net::helper_dir::HelperDirectory::new(Some(helpers.clone()));
    });
}
