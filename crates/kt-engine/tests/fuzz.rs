//! Fuzzing every engine parser of untrusted input (DESIGN §9, principle 12): whatever
//! bytes arrive (from relays, helpers, peers, invite boards, LAN, backup files), the
//! engine may reject them but must never panic or hang.
//!
//! Seeds are valid inputs made by the real code; encrypted layers are fuzzed from the
//! inside too (mutated plaintext sealed with real keys), so the parsers behind the AEAD
//! see the mutations. `KT_FUZZ_ITERS=100000 cargo test -p kt-engine --test fuzz --release`
//! for a long run.

use std::path::PathBuf;
use std::time::Duration;

use kt_engine::crypto;
use kt_engine::envelope::Envelope;
use kt_engine::epochs::EpochRecord;
use kt_engine::invite::{self, Code, CodeKind};
use kt_engine::replica::{backup, Message, Replica};
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{wire, EpochId, GroupId};

fn dev(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-fuzz-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-import-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

/// A shares group `g` (with one document and an edit) with B.
struct World {
    a: Replica,
    b: Replica,
    g: GroupId,
    bundle: Vec<Vec<u8>>,
    edits: Vec<Vec<u8>>,
}

fn world(tag: &str) -> World {
    let (mut a, mut b) = (dev(&format!("{tag}a")), dev(&format!("{tag}b")));
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    a.doc(&g, &d).unwrap().get_map("meta").insert("kind", "list").unwrap();
    a.commit(&g, &d).unwrap();
    a.add_member(&g, b.device_public()).unwrap();
    let bundle = a.export_group(&g).unwrap();
    b.accept_group(g);
    for m in &bundle {
        let _ = b.receive(m);
    }
    a.doc(&g, &d).unwrap().get_map("tasks").insert("t1", "milk").unwrap();
    a.commit(&g, &d).unwrap();
    let edits = a.outbox().unwrap();
    World { a, b, g, bundle, edits }
}

#[test]
fn raw_decoders() {
    let w = world("raw");
    let seeds: Vec<Vec<u8>> = w.bundle.iter().chain(w.edits.iter()).cloned().collect();
    kt_fuzz::run("raw-decoders", &seeds, 5.0, |b| {
        if let Ok(Message::Epoch(r) | Message::Envelope(r)) = Message::decode(b) {
            let _ = EpochRecord::decode(&r);
            let _ = Envelope::decode(&r);
        }
        let _ = EpochRecord::decode(b);
        let _ = Envelope::decode(b);
        let _ = wire::decode::<(GroupId, EpochId, Vec<Vec<u8>>)>(*b"SL", 1, b);
    });
}

#[test]
fn messages_from_any_transport() {
    let mut w = world("msg");
    let seeds: Vec<Vec<u8>> = w.bundle.iter().chain(w.edits.iter()).cloned().collect();
    kt_fuzz::run("receive", &seeds, 1.0, |bytes| {
        let _ = w.b.receive(bytes);
    });
}

#[test]
fn slot_contents_behind_the_encryption() {
    #[derive(serde::Serialize)]
    struct Payload {
        group: GroupId,
        epoch: EpochId,
        messages: Vec<Vec<u8>>,
    }
    let mut w = world("slot");
    let t = w.b.read_topics().into_iter().find(|t| t.group == w.g).expect("a topic for g");
    let plain = wire::encode(*b"SL", 1, &Payload { group: w.g, epoch: t.epoch, messages: w.bundle.clone() }).unwrap();
    let blob = w.b.fuzz_seal_slot(&t, &plain).unwrap();
    // Whole blobs (ciphertext) and the decrypted payload.
    kt_fuzz::run("slot-blob", &[blob], 0.5, |b| {
        let _ = w.b.open_slots(&t, &[b]);
    });
    let mut decoded = 0;
    kt_fuzz::run("slot-payload", &[plain], 1.0, |p| {
        let blob = w.b.fuzz_seal_slot(&t, p).unwrap();
        if w.b.open_slots(&t, &[&blob]).is_ok_and(|r| r.messages > 0) {
            decoded += 1;
        }
    });
    assert!(decoded > 10, "mutated payloads reach message handling ({decoded})");
}

#[test]
fn envelope_contents_behind_the_encryption() {
    let mut w = world("env");
    let seeds: Vec<Vec<u8>> = w.edits.iter().filter_map(|m| w.a.fuzz_envelope_plaintext(&w.g, m)).collect();
    assert!(!seeds.is_empty());
    // Decoded updates go through the import sandbox (a process each): fewer iterations.
    kt_fuzz::run("envelope-inner", &seeds, 0.1, |inner| {
        let m = w.a.fuzz_seal_envelope(&w.g, inner).unwrap();
        let _ = w.b.receive(&m);
    });
}

#[test]
fn invite_records_and_join_requests() {
    let mut w = world("inv");
    let (code, _) = w.a.create_invite(&w.g, CodeKind::Invite, 30 * 86_400_000).unwrap();
    let day = invite::day_of(kt_engine::replica::now_ms());
    let (lookup, key) = invite::derive(&code, CodeKind::Invite, day).unwrap();
    let pubs = w.a.invite_publications(day, &|_| ("Family".into(), "Alex".into())).unwrap();
    let sealed = pubs.iter().find(|(l, _)| *l == lookup).map(|(_, s)| s.clone()).expect("the record");
    let plain = crypto::open(&key, &lookup, &sealed).unwrap();
    kt_fuzz::run("invite-sealed", &[sealed], 0.5, |s| {
        let _ = invite::open_record(s, &lookup, &key);
    });
    let mut opened = 0;
    kt_fuzz::run("invite-record", &[plain], 1.0, |p| {
        if let Ok(rec) = invite::open_record(&crypto::seal(&key, &lookup, p), &lookup, &key) {
            opened += 1;
            // A fresh joiner acts on whatever the record says.
            let mut c = dev("invc");
            let _ = c.begin_join(&rec);
            let _ = c.pending_join_request(&rec.group);
        }
    });
    assert!(opened > 10, "mutated records decode and reach begin_join ({opened})");
    // A real join request, then mutations of it at the member's side.
    let rec = invite::open_record(&pubs.iter().find(|(l, _)| *l == lookup).unwrap().1, &lookup, &key).unwrap();
    let mut c = dev("invj");
    c.begin_join(&rec).unwrap();
    let jr = c.pending_join_request(&w.g).expect("join request");
    kt_fuzz::run("join-request", &[jr], 0.5, |b| {
        let _ = w.a.accept_join_request(b);
    });
}

#[test]
fn whereabouts_and_lan_beacons() {
    let mut w = world("where");
    w.b.set_whereabouts(Some(b"endpoint-and-addresses".to_vec()));
    let rec = w.b.own_whereabouts_record(&w.g).expect("record");
    kt_fuzz::run("whereabouts", &[rec], 1.0, |b| {
        let _ = w.a.accept_whereabouts_record(b);
    });
    let window = 12_345;
    let entries = w.b.whereabouts_entries(window);
    let (_, _, tag, sealed) = entries.into_iter().find(|e| e.0 == w.g).expect("lan entry");
    kt_fuzz::run("lan-entry", &[sealed], 1.0, |b| {
        let _ = w.a.open_lan_entry(window, &tag, b);
    });
}

#[test]
fn backup_files() {
    let mut w = world("bk");
    let file = backup::seal(&w.a.make_backup(30 * 86_400_000, b"app".to_vec()).unwrap()).unwrap();
    let body = backup::open_file(b"KTB1", b"kt/backup/v1", &file).unwrap();
    kt_fuzz::run("backup-file", &[file], 0.5, |b| {
        let _ = backup::open(b);
    });
    // Decoded backups are also restored onto a fresh device now and then (slow: the
    // snapshots go through the sandbox).
    let mut n = 0u32;
    kt_fuzz::run("backup-body", &[body], 1.0, |p| {
        if let Ok(bk) = backup::open(&backup::seal_file(b"KTB1", b"kt/backup/v1", p)) {
            n += 1;
            if n % 25 == 1 {
                let _ = dev("bkr").restore_backup(&bk);
            }
        }
    });
    assert!(n > 10, "mutated backups decode and get restored ({n})");
}

#[test]
fn typed_codes() {
    let code = Code::generate();
    let text = code.to_text();
    let words = code.to_words().join(" ");
    let prefixes: String = code.to_words().iter().map(|w| &w[..3]).collect::<Vec<_>>().join("-");
    let seeds = vec![text.into_bytes(), words.into_bytes(), prefixes.into_bytes(), "  a,b.c+d_e/f-g h ".as_bytes().to_vec()];
    kt_fuzz::run("code-parse", &seeds, 5.0, |b| {
        let _ = Code::parse(&String::from_utf8_lossy(b));
        let _ = invite::complete(&String::from_utf8_lossy(b));
    });
}
