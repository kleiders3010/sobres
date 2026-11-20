//! Sync through mailbox slots only (DESIGN §7): replaceable per-device slots under rotating
//! topics, forward slots across key epochs, removal through slots.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};

fn sandbox() -> SandboxConfig {
    SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-import-worker")), timeout: Duration::from_secs(20) }
}

fn dev(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-slot-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    Replica::open(&root.join("identity"), &root.join("data"), sandbox()).unwrap()
}

/// A replace-semantics mailbox: topic -> writer -> latest blob (like Nostr addressable events).
#[derive(Default)]
struct Mailbox {
    slots: HashMap<[u8; 32], HashMap<[u8; 32], Vec<u8>>>,
}

impl Mailbox {
    fn write(&mut self, r: &mut Replica) {
        for t in r.slot_targets() {
            let blob = r.slot_blob(&t).unwrap();
            self.slots.entry(t.topic).or_default().insert(t.writer_seed, blob);
        }
    }
    fn read(&self, r: &mut Replica) {
        // Two passes: the first can reveal new epochs (and so new topics) to read.
        for _ in 0..2 {
            for t in r.read_topics() {
                if let Some(ws) = self.slots.get(&t.topic) {
                    let blobs: Vec<&[u8]> = ws.values().map(|b| b.as_slice()).collect();
                    r.open_slots(&t, &blobs).unwrap();
                }
            }
        }
    }
    fn sync(&mut self, devs: &mut [&mut Replica]) {
        for _ in 0..2 {
            for d in devs.iter_mut() {
                self.write(d);
            }
            for d in devs.iter_mut() {
                self.read(d);
            }
        }
    }
    fn blobs(&self) -> usize {
        self.slots.values().map(|w| w.len()).sum()
    }
}

fn set(r: &mut Replica, g: &GroupId, d: &DocId, k: &str, v: &str) {
    r.doc(g, d).unwrap().get_map("tasks").insert(k, v).unwrap();
    r.commit(g, d).unwrap();
}

fn value(r: &Replica, g: &GroupId, d: &DocId) -> String {
    format!("{:?}", r.doc(g, d).map(|x| x.get_deep_value()))
}

/// Join directly (M3 replaces this with invites); everything after goes through slots.
fn join(owner: &mut Replica, joiner: &mut Replica, g: GroupId) {
    owner.add_member(&g, joiner.device_public()).unwrap();
    joiner.accept_group(g);
    for m in owner.export_group(&g).unwrap() {
        let _ = joiner.receive(&m);
    }
}

#[test]
fn devices_sync_through_slots_only() {
    let mut mb = Mailbox::default();
    let (mut a, mut b) = (dev("a"), dev("b"));
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    set(&mut a, &g, &d, "t1", "milk");
    join(&mut a, &mut b, g);
    let _ = a.outbox();

    set(&mut a, &g, &d, "t2", "from A");
    set(&mut b, &g, &d, "t3", "from B");
    mb.sync(&mut [&mut a, &mut b]);
    let v = value(&a, &g, &d);
    assert_eq!(v, value(&b, &g, &d));
    assert!(v.contains("milk") && v.contains("from A") && v.contains("from B"), "{v}");

    // Replacement: many more edits, still one slot per device and topic.
    let before = mb.blobs();
    for i in 0..20 {
        set(&mut a, &g, &d, &format!("a{i}"), "x");
        mb.sync(&mut [&mut a, &mut b]);
    }
    assert_eq!(mb.blobs(), before);
    assert_eq!(value(&a, &g, &d), value(&b, &g, &d));

    // A topic reveals nothing about the group: it isn't derived from its ID.
    for t in a.read_topics() {
        assert_ne!(t.topic, g.0);
    }
}

#[test]
fn epoch_changes_propagate_through_forward_slots_and_removal_holds() {
    let mut mb = Mailbox::default();
    let (mut a, mut b, mut c) = (dev("fa"), dev("fb"), dev("fc"));
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    set(&mut a, &g, &d, "t1", "hello");
    join(&mut a, &mut b, g);
    // C joins; B learns of C's Add epoch only through A's forward slot.
    join(&mut a, &mut c, g);
    let _ = a.outbox();
    mb.sync(&mut [&mut a, &mut b, &mut c]);
    let head = a.epochs(&g).unwrap().head();
    assert_eq!(b.epochs(&g).unwrap().head(), head, "B followed the epoch change via slots");
    set(&mut c, &g, &d, "t2", "from C");
    mb.sync(&mut [&mut a, &mut b, &mut c]);
    assert!(value(&b, &g, &d).contains("from C"));

    // A removes C; C keeps editing without knowing.
    let c_id = c.device_id();
    a.remove_member(&g, c_id).unwrap();
    set(&mut c, &g, &d, "evil", "late edit");
    mb.sync(&mut [&mut a, &mut b, &mut c]);
    set(&mut a, &g, &d, "t3", "after removal");
    mb.sync(&mut [&mut a, &mut b, &mut c]);
    assert!(value(&b, &g, &d).contains("after removal"));
    assert!(!value(&a, &g, &d).contains("late edit"));
    assert!(!value(&b, &g, &d).contains("late edit"));
    assert!(!value(&c, &g, &d).contains("after removal"), "C can't read after removal");
    // C can't even find the new topic.
    let new_topics: Vec<_> = a.read_topics().into_iter().map(|t| t.topic).collect();
    assert!(c.slot_targets().iter().all(|t| !new_topics[..1].contains(&t.topic)) || c.slot_targets().is_empty());
    assert!(c.slot_targets().is_empty(), "a removed device writes no slots");
}

#[test]
fn group_endpoint_sets_spread_through_slots_and_newest_wins() {
    let mut mb = Mailbox::default();
    let (mut a, mut b) = (dev("ma"), dev("mb"));
    let g = a.create_group().unwrap();
    let _d = a.create_doc(&g).unwrap();
    join(&mut a, &mut b, g);
    let _ = a.outbox();
    a.propose_mailbox_set(&g, "nostr", vec!["wss://one".into(), "wss://two".into()]).unwrap();
    a.propose_mailbox_set(&g, "helpers", vec!["h1".into()]).unwrap();
    mb.sync(&mut [&mut a, &mut b]);
    assert_eq!(b.mailbox_set(&g, "nostr").0.unwrap().endpoints, vec!["wss://one", "wss://two"]);
    assert_eq!(b.mailbox_set(&g, "helpers").0.unwrap().endpoints, vec!["h1"]);
    std::thread::sleep(std::time::Duration::from_millis(5));
    b.propose_mailbox_set(&g, "nostr", vec!["wss://two".into(), "wss://three".into()]).unwrap();
    mb.sync(&mut [&mut a, &mut b]);
    let (cur, prev) = a.mailbox_set(&g, "nostr");
    assert_eq!(cur.unwrap().endpoints, vec!["wss://two", "wss://three"]);
    assert_eq!(prev.unwrap().endpoints, vec!["wss://one", "wss://two"], "previous set still read for a while");
}

/// Sync through a mailbox among only some devices (the others are "offline").
fn sync_some(mb: &mut Mailbox, devs: &mut [&mut Replica]) {
    mb.sync(devs);
}

#[test]
fn a_fork_doesnt_stop_sync_while_the_merging_device_is_away() {
    let mut mb = Mailbox::default();
    let mut devs: Vec<Replica> = (0..3).map(|i| dev(&format!("fk{i}"))).collect();
    let g = devs[0].create_group().unwrap();
    let d = devs[0].create_doc(&g).unwrap();
    set(&mut devs[0], &g, &d, "t0", "start");
    let (first, rest) = devs.split_at_mut(1);
    join(&mut first[0], &mut rest[0], g);
    join(&mut first[0], &mut rest[1], g);
    let _ = first[0].outbox();
    {
        let mut all: Vec<&mut Replica> = devs.iter_mut().collect();
        sync_some(&mut mb, &mut all);
    }
    // The merging device is the lowest ID among the three: take it offline.
    let ids: Vec<_> = devs.iter().map(|r| r.device_id()).collect();
    let merger = (0..3).min_by_key(|i| ids[*i]).unwrap();
    let online: Vec<usize> = (0..3).filter(|i| *i != merger).collect();
    let (x, y) = (online[0], online[1]);
    // X and Y each add a new device at the same time (they can't see each other's add).
    let (nx, ny) = (dev("nx"), dev("ny"));
    devs[x].add_member(&g, nx.device_public()).unwrap();
    devs[y].add_member(&g, ny.device_public()).unwrap();
    // Existing members keep editing and syncing while the fork waits for the merge.
    set(&mut devs[x], &g, &d, "during", "fork edit from X");
    set(&mut devs[y], &g, &d, "during2", "fork edit from Y");
    {
        let (a, b) = if x < y { let (l, r) = devs.split_at_mut(y); (&mut l[x], &mut r[0]) } else { let (l, r) = devs.split_at_mut(x); (&mut r[0], &mut l[y]) };
        sync_some(&mut mb, &mut [a, b]);
        assert!(a.epochs(&g).unwrap().heads().len() == 2, "still forked (the merger is away)");
        let v = value(a, &g, &d);
        assert!(v.contains("fork edit from X") && v.contains("fork edit from Y"), "sync continued during the fork: {v}");
        assert_eq!(v, value(b, &g, &d));
    }
    // The merger comes back and merges; everyone converges.
    let mut all: Vec<&mut Replica> = devs.iter_mut().collect();
    sync_some(&mut mb, &mut all);
    for r in &devs {
        assert_eq!(r.epochs(&g).unwrap().heads().len(), 1, "merged");
        assert!(value(r, &g, &d).contains("fork edit from Y"));
    }
}

#[test]
fn during_a_fork_a_removed_device_reads_nothing_new() {
    let mut mb = Mailbox::default();
    let (mut a, mut b, mut c) = (dev("ra"), dev("rb"), dev("rc"));
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    set(&mut a, &g, &d, "t0", "start");
    join(&mut a, &mut b, g);
    join(&mut a, &mut c, g);
    let _ = a.outbox();
    mb.sync(&mut [&mut a, &mut b, &mut c]);
    // A removes C while B (unaware) adds D: two branches, one still includes C.
    let c_id = c.device_id();
    a.remove_member(&g, c_id).unwrap();
    let dd = dev("rd");
    b.add_member(&g, dd.device_public()).unwrap();
    mb.sync(&mut [&mut a, &mut b]); // C is away; the merge may or may not have happened
    set(&mut b, &g, &d, "secret", "written during the fork");
    mb.sync(&mut [&mut a, &mut b]);
    assert!(value(&a, &g, &d).contains("written during the fork"));
    mb.sync(&mut [&mut a, &mut b, &mut c]);
    assert!(!value(&c, &g, &d).contains("written during the fork"), "never sealed under a branch that includes C");
}
