//! Document epochs (D24) through mailbox slots: the proposal travels in the proposer's
//! slot, members switch, and slot sizes drop back to the live state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_tasks as tk;

fn sandbox() -> SandboxConfig {
    SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-tasks-worker")), timeout: Duration::from_secs(20) }
}

fn dev(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-se-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let mut r = Replica::open(&root.join("identity"), &root.join("data"), sandbox()).unwrap();
    r.set_semantics(Box::new(tk::TaskSemantics));
    r
}

#[derive(Default)]
struct Mailbox {
    slots: HashMap<[u8; 32], HashMap<[u8; 32], Vec<u8>>>,
}

impl Mailbox {
    fn sync(&mut self, devs: &mut [&mut Replica]) {
        for _ in 0..3 {
            for r in devs.iter_mut() {
                for t in r.slot_targets() {
                    let blob = r.slot_blob(&t).unwrap();
                    self.slots.entry(t.topic).or_default().insert(t.writer_seed, blob);
                }
            }
            for r in devs.iter_mut() {
                for t in r.read_topics() {
                    if let Some(ws) = self.slots.get(&t.topic) {
                        let blobs: Vec<&[u8]> = ws.values().map(|b| b.as_slice()).collect();
                        r.open_slots(&t, &blobs).unwrap();
                    }
                }
            }
        }
    }
    fn bytes(&self) -> usize {
        self.slots.values().flat_map(|w| w.values()).map(|b| b.len()).sum()
    }
}

fn titles(r: &Replica, g: &kt_engine::GroupId, d: &kt_engine::DocId) -> Vec<String> {
    let mut v: Vec<String> = tk::tasks(r.doc(g, d).unwrap()).into_iter().map(|t| t.title).collect();
    v.sort();
    v
}

#[test]
fn document_epoch_through_slots_shrinks_them() {
    let mut mb = Mailbox::default();
    let (mut a, mut b) = (dev("a"), dev("b"));
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    tk::init_list(a.doc(&g, &d).unwrap(), "Groceries").unwrap();
    a.commit(&g, &d).unwrap();
    a.add_member(&g, b.device_public()).unwrap();
    b.accept_group(g);
    for m in a.export_group(&g).unwrap() {
        let _ = b.receive(&m);
    }
    let _ = a.outbox();

    // Lots of churn: history grows, the live list stays small.
    for i in 0..150 {
        let id = tk::add_task(a.doc(&g, &d).unwrap(), None, &format!("item {i}")).unwrap();
        a.commit(&g, &d).unwrap();
        if i % 10 != 0 {
            tk::delete_task(a.doc(&g, &d).unwrap(), &id).unwrap();
            a.commit(&g, &d).unwrap();
        }
    }
    tk::add_task(b.doc(&g, &d).unwrap(), None, "from B").unwrap();
    b.commit(&g, &d).unwrap();
    mb.sync(&mut [&mut a, &mut b]);
    assert_eq!(titles(&a, &g, &d), titles(&b, &g, &d));
    let before = mb.bytes();

    a.propose_doc_epoch(&g, &d).unwrap();
    mb.sync(&mut [&mut a, &mut b]);
    assert_eq!(b.doc_generation(&g, &d), Some(1), "B switched through A's slot");
    assert_eq!(titles(&a, &g, &d), titles(&b, &g, &d));
    let after = mb.bytes();
    assert!(after * 2 < before, "slots shrink after a document epoch: {before} -> {after}");

    // Editing continues in the new generation, both ways.
    tk::add_task(b.doc(&g, &d).unwrap(), None, "after epoch B").unwrap();
    b.commit(&g, &d).unwrap();
    tk::add_task(a.doc(&g, &d).unwrap(), None, "after epoch A").unwrap();
    a.commit(&g, &d).unwrap();
    mb.sync(&mut [&mut a, &mut b]);
    let t = titles(&a, &g, &d);
    assert_eq!(t, titles(&b, &g, &d));
    assert!(t.contains(&"after epoch A".to_string()) && t.contains(&"after epoch B".to_string()));
    println!("slot bytes: {before} before the epoch, {after} after");
}

#[test]
fn edits_made_right_before_a_compaction_still_arrive() {
    // A edits and compacts without syncing in between: its last generation-0 ops never
    // reached B. A's slot keeps carrying them (the "prevgen" envelope), so B can verify the
    // proposal and switch.
    let mut mb = Mailbox::default();
    let (mut a, mut b) = (dev("pa"), dev("pb"));
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    tk::init_list(a.doc(&g, &d).unwrap(), "L").unwrap();
    tk::add_task(a.doc(&g, &d).unwrap(), None, "first").unwrap();
    a.commit(&g, &d).unwrap();
    a.add_member(&g, b.device_public()).unwrap();
    b.accept_group(g);
    for m in a.export_group(&g).unwrap() {
        let _ = b.receive(&m);
    }
    let _ = a.outbox();
    mb.sync(&mut [&mut a, &mut b]);

    tk::add_task(a.doc(&g, &d).unwrap(), None, "unsynced").unwrap();
    a.commit(&g, &d).unwrap();
    a.propose_doc_epoch(&g, &d).unwrap();
    // B also edits concurrently in generation 0 and must rebase.
    tk::add_task(b.doc(&g, &d).unwrap(), None, "from B").unwrap();
    b.commit(&g, &d).unwrap();
    mb.sync(&mut [&mut a, &mut b]);
    assert_eq!(b.doc_generation(&g, &d), Some(1));
    let t = titles(&b, &g, &d);
    assert!(t.contains(&"unsynced".to_string()), "{t:?}");
    assert_eq!(t, titles(&a, &g, &d));
    assert!(t.contains(&"from B".to_string()));
}

fn churn(r: &mut Replica, g: &kt_engine::GroupId, d: &kt_engine::DocId, n: usize, tag: &str) {
    for i in 0..n {
        let id = tk::add_task(r.doc(g, d).unwrap(), None, &format!("{tag} {i}")).unwrap();
        r.commit(g, d).unwrap();
        if i % 10 != 0 {
            tk::delete_task(r.doc(g, d).unwrap(), &id).unwrap();
            r.commit(g, d).unwrap();
        }
    }
}

fn small_policy(r: &mut Replica, active_ms: u64) {
    r.set_compaction_policy(kt_engine::replica::CompactionPolicy { min_bytes: 4 * 1024, ratio: 3, active_ms, cooldown_ms: 0 });
    r.set_ack_interval_ms(0);
}

fn round(mb: &mut Mailbox, devs: &mut [&mut Replica]) {
    mb.sync(devs);
    for r in devs.iter_mut() {
        r.auto_compact().unwrap();
    }
    mb.sync(devs);
}

#[test]
fn lists_compact_automatically_when_everyone_active_has_acknowledged() {
    let mut mb = Mailbox::default();
    let (mut a, mut b, mut c) = (dev("aa"), dev("ab"), dev("ac"));
    for r in [&mut a, &mut b, &mut c] {
        small_policy(r, 3_600_000);
    }
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    tk::init_list(a.doc(&g, &d).unwrap(), "L").unwrap();
    a.commit(&g, &d).unwrap();
    for other in [&mut b, &mut c] {
        a.add_member(&g, other.device_public()).unwrap();
        other.accept_group(g);
        for m in a.export_group(&g).unwrap() {
            let _ = other.receive(&m);
        }
    }
    let _ = a.outbox();
    round(&mut mb, &mut [&mut a, &mut b, &mut c]);

    // Lots of churn, but C (active) is away: nothing is compacted without C's ack.
    churn(&mut a, &g, &d, 120, "a");
    for _ in 0..2 {
        round(&mut mb, &mut [&mut a, &mut b]);
    }
    assert_eq!(a.doc_generation(&g, &d), Some(0), "C hasn't acknowledged: no compaction");

    // C catches up: the lowest-ID device compacts, everyone follows.
    for _ in 0..3 {
        round(&mut mb, &mut [&mut a, &mut b, &mut c]);
    }
    for r in [&a, &b, &c] {
        assert_eq!(r.doc_generation(&g, &d), Some(1), "compacted automatically");
    }
    assert_eq!(titles(&a, &g, &d), titles(&c, &g, &d));
}

#[test]
fn a_long_absent_member_doesnt_block_compaction_and_rebases_on_return() {
    let mut mb = Mailbox::default();
    let (mut a, mut b, mut c) = (dev("la"), dev("lb"), dev("lc"));
    for r in [&mut a, &mut b, &mut c] {
        small_policy(r, 400); // "active" = seen in the last 0.4 s
    }
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    tk::init_list(a.doc(&g, &d).unwrap(), "L").unwrap();
    a.commit(&g, &d).unwrap();
    for other in [&mut b, &mut c] {
        a.add_member(&g, other.device_public()).unwrap();
        other.accept_group(g);
        for m in a.export_group(&g).unwrap() {
            let _ = other.receive(&m);
        }
    }
    let _ = a.outbox();
    round(&mut mb, &mut [&mut a, &mut b, &mut c]);
    // C goes away (and edits offline); later A and B churn.
    tk::add_task(c.doc(&g, &d).unwrap(), None, "offline edit by C").unwrap();
    c.commit(&g, &d).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(600));
    churn(&mut a, &g, &d, 120, "a");
    for _ in 0..3 {
        round(&mut mb, &mut [&mut a, &mut b]);
    }
    assert_eq!(a.doc_generation(&g, &d), Some(1), "compacted without the absent member");
    // C returns: switches to the new generation, keeps its offline edit.
    for _ in 0..3 {
        round(&mut mb, &mut [&mut a, &mut b, &mut c]);
    }
    assert_eq!(c.doc_generation(&g, &d), Some(1));
    let t = titles(&a, &g, &d);
    assert_eq!(t, titles(&c, &g, &d));
    assert!(t.contains(&"offline edit by C".to_string()), "{t:?}");
}

/// Second-pass audit T2-5: a removed device that still holds the old epoch's key seals a rebuild ("document epoch
/// proposal") of the list as it last saw it. pinned: members refuse a rebuild from a device that is not a member now.
#[test]
fn a_removed_device_cannot_make_members_rebuild_a_list() {
    let (mut a, mut b, mut c) = (dev("ra"), dev("rb"), dev("rc"));
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    tk::init_list(a.doc(&g, &d).unwrap(), "L").unwrap();
    tk::add_task(a.doc(&g, &d).unwrap(), None, "milk").unwrap();
    a.commit(&g, &d).unwrap();
    for other in [&mut b, &mut c] {
        a.add_member(&g, other.device_public()).unwrap();
        other.accept_group(g);
        for m in a.export_group(&g).unwrap() {
            let _ = other.receive(&m);
        }
    }
    let _ = a.outbox();
    let _ = c.outbox();
    // A removes C; B hears of it, C does not.
    a.remove_member(&g, c.device_id()).unwrap();
    for m in a.export_group(&g).unwrap() {
        let _ = b.receive(&m);
    }
    tk::add_task(a.doc(&g, &d).unwrap(), None, "bread").unwrap();
    a.commit(&g, &d).unwrap();
    for m in a.outbox().unwrap() {
        let _ = b.receive(&m);
    }
    assert!(!b.members(&g).contains(&c.device_id()), "B knows C is out");
    // C, unaware, proposes a rebuild under the old epoch.
    c.propose_doc_epoch(&g, &d).unwrap();
    for m in c.outbox().unwrap() {
        let _ = a.receive(&m);
        let _ = b.receive(&m);
    }
    for r in [&a, &b] {
        assert_eq!(r.doc_generation(&g, &d), Some(0), "no rebuild from a removed device");
    }
    assert_eq!(titles(&a, &g, &d), titles(&b, &g, &d));
}