//! Invites end to end through slots (DESIGN §5, D6, D7, D55): a joiner with only the
//! 8 words gets in; invites are multi-use; any member accepts; link codes are refused on
//! the join path; cancelling and removals close invites.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use kt_engine::invite::{self, Code, CodeKind};
use kt_engine::replica::{Event, Replica};
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};

fn dev(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-inv-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-import-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

#[derive(Default)]
struct Net {
    slots: HashMap<[u8; 32], HashMap<[u8; 32], Vec<u8>>>,
    /// Invite records as any layer would hold them: lookup key -> sealed records.
    board: HashMap<[u8; 32], Vec<Vec<u8>>>,
}

const DAY: u32 = 20_600;
const MONTH: u64 = 30 * 86_400_000;

impl Net {
    fn sync(&mut self, devs: &mut [&mut Replica]) {
        for _ in 0..3 {
            for r in devs.iter_mut() {
                for t in r.slot_targets() {
                    let blob = r.slot_blob(&t).unwrap();
                    self.slots.entry(t.topic).or_default().insert(t.writer_seed, blob);
                }
                for (lookup, sealed) in r.invite_publications(DAY, &|_| ("Family".into(), "Alex".into())).unwrap() {
                    let v = self.board.entry(lookup).or_default();
                    if !v.contains(&sealed) {
                        v.push(sealed);
                    }
                }
            }
            for r in devs.iter_mut() {
                for _ in 0..2 {
                    for t in r.read_topics() {
                        if let Some(ws) = self.slots.get(&t.topic) {
                            let blobs: Vec<&[u8]> = ws.values().map(|b| b.as_slice()).collect();
                            r.open_slots(&t, &blobs).unwrap();
                        }
                    }
                }
            }
        }
    }

    /// What a joiner does with the words: derive, look up, open.
    fn lookup(&self, words: &str, kind: CodeKind) -> Option<invite::InviteRecord> {
        let code = Code::parse(words).ok()?;
        let (lookup, key) = invite::derive(&code, kind, DAY).ok()?;
        // Several copies (re-posted by members over time): take the newest.
        self.board.get(&lookup)?.iter().filter_map(|s| invite::open_record(s, &lookup, &key).ok()).max_by_key(|r| r.published_ms)
    }
}

fn value(r: &Replica, g: &GroupId, d: &DocId) -> String {
    format!("{:?}", r.doc(g, d).map(|x| x.get_deep_value()))
}

#[test]
fn joining_with_only_the_words() {
    let mut net = Net::default();
    let (mut a, mut b, mut c) = (dev("a"), dev("b"), dev("c"));
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    a.doc(&g, &d).unwrap().get_map("tasks").insert("t1", "milk").unwrap();
    a.commit(&g, &d).unwrap();
    let (code, expires) = a.create_invite(&g, CodeKind::Invite, MONTH).unwrap();
    let words = code.to_text();
    net.sync(&mut [&mut a]);

    // B types the words (3-letter prefixes are enough).
    let short: Vec<String> = words.split(' ').map(|w| w[..3].to_string()).collect();
    let rec = net.lookup(&short.join(" "), CodeKind::Invite).expect("record found");
    assert_eq!((rec.group_name.as_str(), rec.inviter_name.as_str(), rec.expires_ms), ("Family", "Alex", expires));
    b.begin_join(&rec).unwrap();
    net.sync(&mut [&mut a, &mut b]);
    assert!(b.epochs(&g).unwrap().is_current_member(&b.device_id()), "B was added");
    assert!(value(&b, &g, &d).contains("milk"), "and reads the group");
    assert!(a.take_events().iter().any(|e| matches!(e, Event::DeviceJoined { via_link: false, .. })));
    assert_eq!(b.settle_joins().len(), 1);

    // Multi-use, and any member accepts: A is offline, B (who learned the open invite
    // through slots) adds C.
    // Worst case: C got an old copy carrying the group's first epoch key.
    let code = Code::parse(&words).unwrap();
    let (lookup, key) = invite::derive(&code, CodeKind::Invite, DAY).unwrap();
    let rec = net.board[&lookup].iter().filter_map(|s| invite::open_record(s, &lookup, &key).ok()).min_by_key(|r| r.published_ms).unwrap();
    c.begin_join(&rec).unwrap();
    net.sync(&mut [&mut b, &mut c]);
    assert!(c.epochs(&g).unwrap().is_current_member(&c.device_id()), "B accepted C");
    assert!(value(&c, &g, &d).contains("milk"));
}

#[test]
fn link_codes_are_refused_on_the_join_path() {
    let mut net = Net::default();
    let mut a = dev("la");
    let mut b = dev("lb");
    let personal = a.create_group().unwrap();
    let (code, _) = a.create_invite(&personal, CodeKind::Link, 4 * 3_600_000).unwrap();
    net.sync(&mut [&mut a]);
    assert!(net.lookup(&code.to_text(), CodeKind::Invite).is_none(), "a link code isn't found as an invite");
    let rec = net.lookup(&code.to_text(), CodeKind::Link).expect("found as a link");
    let err = b.begin_join(&rec).unwrap_err().to_string();
    assert!(err.contains("device link code"), "{err}");
    b.begin_link(&rec).unwrap();
    net.sync(&mut [&mut a, &mut b]);
    assert!(b.epochs(&personal).unwrap().is_current_member(&b.device_id()));
    assert!(a.take_events().iter().any(|e| matches!(e, Event::DeviceJoined { via_link: true, .. })));
}

#[test]
fn cancelled_invites_and_removals_close_them() {
    let mut net = Net::default();
    let (mut a, mut b, mut c, mut e) = (dev("ca"), dev("cb"), dev("cc"), dev("ce"));
    let g = a.create_group().unwrap();
    let (code1, _) = a.create_invite(&g, CodeKind::Invite, MONTH).unwrap();
    let (code2, _) = a.create_invite(&g, CodeKind::Invite, MONTH).unwrap();
    net.sync(&mut [&mut a]);
    let rec1 = net.lookup(&code1.to_text(), CodeKind::Invite).unwrap();
    let rec2 = net.lookup(&code2.to_text(), CodeKind::Invite).unwrap();

    // Cancelled: a joiner with that code isn't added.
    a.cancel_invite(&g, &code1.id()).unwrap();
    b.begin_join(&rec1).unwrap();
    net.sync(&mut [&mut a, &mut b]);
    assert!(!b.epochs(&g).is_some_and(|x| x.is_current_member(&b.device_id())));

    // Invite 2 works until a removal rotates the key.
    c.begin_join(&rec2).unwrap();
    net.sync(&mut [&mut a, &mut c]);
    assert!(c.epochs(&g).unwrap().is_current_member(&c.device_id()));
    let c_id = c.device_id();
    a.remove_member(&g, c_id).unwrap();
    e.begin_join(&rec2).unwrap();
    net.sync(&mut [&mut a, &mut e]);
    assert!(!e.epochs(&g).is_some_and(|x| x.is_current_member(&e.device_id())), "the removal closed invite 2");
}

#[test]
fn a_joiner_restarting_before_being_let_in_still_gets_in() {
    let mut net = Net::default();
    let mut a = dev("pa");
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    a.doc(&g, &d).unwrap().get_map("tasks").insert("t1", "milk").unwrap();
    a.commit(&g, &d).unwrap();
    let (code, _) = a.create_invite(&g, CodeKind::Invite, MONTH).unwrap();
    net.sync(&mut [&mut a]);
    let root = std::env::temp_dir().join(format!("kt-inv-restart-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-import-worker")), timeout: Duration::from_secs(20) };
    {
        let mut b = Replica::open(&root.join("identity"), &root.join("data"), sb.clone()).unwrap();
        let rec = net.lookup(&code.to_text(), CodeKind::Invite).unwrap();
        b.begin_join(&rec).unwrap();
        net.sync(&mut [&mut b]); // posts the join request, then the app is closed
    }
    // The app starts again (new process): the invite key must still be there.
    let mut b = Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap();
    net.sync(&mut [&mut a, &mut b]);
    assert!(b.epochs(&g).unwrap().is_current_member(&b.device_id()));
    assert!(value(&b, &g, &d).contains("milk"));
}

/// Periodic key rotation (D5 rule 7): when the key is due, exactly one member rotates
/// (the others would follow hours later if it were away); everyone moves to the new key
/// and keeps syncing; an invite made before the rotation still works (only removals
/// close invites).
#[test]
fn keys_rotate_periodically_without_closing_invites() {
    let mut net = Net::default();
    let (mut a, mut b, mut c) = (dev("ra"), dev("rb"), dev("rc"));
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    a.doc(&g, &d).unwrap().get_map("tasks").insert("t1", "before").unwrap();
    a.commit(&g, &d).unwrap();
    a.add_member(&g, b.device_public()).unwrap();
    b.accept_group(g);
    for m in a.export_group(&g).unwrap() {
        let _ = b.receive(&m);
    }
    let (code, _) = a.create_invite(&g, CodeKind::Invite, MONTH).unwrap();
    net.sync(&mut [&mut a, &mut b]);
    let before = a.epochs(&g).unwrap().head().unwrap();

    // Not due yet: nobody rotates.
    assert!(a.auto_rotate().unwrap().is_empty() && b.auto_rotate().unwrap().is_empty());
    // Due now (period 0): only the first in turn rotates; the other waits its 6 h.
    a.set_rotation_interval_ms(0);
    b.set_rotation_interval_ms(0);
    let rotated = a.auto_rotate().unwrap().len() + b.auto_rotate().unwrap().len();
    assert_eq!(rotated, 1, "exactly one member rotates");
    net.sync(&mut [&mut a, &mut b]);
    let after = a.epochs(&g).unwrap().head().unwrap();
    assert_ne!(after, before, "a new key");
    assert_eq!(b.epochs(&g).unwrap().head(), Some(after), "both on it");
    a.set_rotation_interval_ms(u64::MAX);
    b.set_rotation_interval_ms(u64::MAX);

    // Still syncing under the new key.
    b.doc(&g, &d).unwrap().get_map("tasks").insert("t2", "after").unwrap();
    b.commit(&g, &d).unwrap();
    net.sync(&mut [&mut a, &mut b]);
    assert!(value(&a, &g, &d).contains("after"));

    // The invite from before the rotation still lets someone in.
    let rec = net.lookup(&code.to_text(), CodeKind::Invite).expect("record");
    c.begin_join(&rec).unwrap();
    net.sync(&mut [&mut a, &mut b, &mut c]);
    assert!(c.epochs(&g).unwrap().is_current_member(&c.device_id()), "joined after the rotation");
    assert!(value(&c, &g, &d).contains("before") && value(&c, &g, &d).contains("after"), "with the whole history");
}

