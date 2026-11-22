//! Document epochs (D24) end to end: rebuild, verify, switch, rebase, removal cutoffs,
//! concurrent proposals.

use std::path::PathBuf;
use std::time::Duration;

use kt_engine::replica::{Event, Replica};
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};
use kt_tasks as tk;
use loro::ExportMode;

fn sandbox() -> SandboxConfig {
    SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-tasks-worker")), timeout: Duration::from_secs(20) }
}

fn dev(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-de-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let mut r = Replica::open(&root.join("identity"), &root.join("data"), sandbox()).unwrap();
    r.set_semantics(Box::new(tk::TaskSemantics));
    r
}

fn send(from: &mut Replica, to: &mut [&mut Replica]) {
    for m in from.outbox().unwrap() {
        for t in to.iter_mut() {
            let _ = t.receive(&m);
        }
    }
}

fn setup(n: usize) -> (Vec<Replica>, GroupId, DocId) {
    let mut ds: Vec<Replica> = (0..n).map(|i| dev(&format!("d{i}"))).collect();
    let g = ds[0].create_group().unwrap();
    let d = ds[0].create_doc(&g).unwrap();
    tk::init_list(ds[0].doc(&g, &d).unwrap(), "Groceries").unwrap();
    ds[0].commit(&g, &d).unwrap();
    for i in 1..n {
        let p = ds[i].device_public();
        ds[0].add_member(&g, p).unwrap();
        ds[i].accept_group(g);
    }
    for i in 1..n {
        for m in ds[0].export_group(&g).unwrap() {
            let _ = ds[i].receive(&m);
        }
    }
    sync_all(&mut ds);
    (ds, g, d)
}

fn sync_all(ds: &mut [Replica]) {
    for _ in 0..4 {
        for i in 0..ds.len() {
            let (left, rest) = ds.split_at_mut(i);
            let (me, right) = rest.split_first_mut().unwrap();
            let mut others: Vec<&mut Replica> = left.iter_mut().chain(right.iter_mut()).collect();
            send(me, &mut others);
        }
    }
}

fn norm(r: &Replica, g: &GroupId, d: &DocId) -> Vec<u8> {
    tk::normalized(r.doc(g, d).unwrap())
}

#[test]
fn basic_epoch_shrinks_and_keeps_syncing() {
    let (mut ds, g, d) = setup(2);
    for i in 0..200 {
        let id = tk::add_task(ds[0].doc(&g, &d).unwrap(), None, &format!("t{i}")).unwrap();
        if i % 20 != 0 {
            tk::delete_task(ds[0].doc(&g, &d).unwrap(), &id).unwrap();
        }
    }
    ds[0].commit(&g, &d).unwrap();
    sync_all(&mut ds);
    assert_eq!(norm(&ds[0], &g, &d), norm(&ds[1], &g, &d));
    let before = ds[1].doc(&g, &d).unwrap().export(ExportMode::Snapshot).unwrap().len();
    ds[0].propose_doc_epoch(&g, &d).unwrap();
    sync_all(&mut ds);
    assert_eq!(ds[1].doc_generation(&g, &d), Some(1));
    assert_eq!(norm(&ds[0], &g, &d), norm(&ds[1], &g, &d));
    let after = ds[1].doc(&g, &d).unwrap().export(ExportMode::Snapshot).unwrap().len();
    assert!(after * 3 < before, "epoch should shrink the doc: {before} -> {after}");
    // Editing continues in the new generation.
    tk::add_task(ds[1].doc(&g, &d).unwrap(), None, "after epoch").unwrap();
    ds[1].commit(&g, &d).unwrap();
    sync_all(&mut ds);
    assert_eq!(norm(&ds[0], &g, &d), norm(&ds[1], &g, &d));
    assert!(tk::tasks(ds[0].doc(&g, &d).unwrap()).iter().any(|t| t.title == "after epoch"));
}

#[test]
fn device_with_unsent_edits_rebases() {
    let (mut ds, g, d) = setup(2);
    let milk = tk::add_task(ds[0].doc(&g, &d).unwrap(), None, "milk").unwrap();
    ds[0].commit(&g, &d).unwrap();
    sync_all(&mut ds);
    // B edits offline (not sent yet); A proposes an epoch meanwhile.
    tk::set_done(ds[1].doc(&g, &d).unwrap(), &milk, true).unwrap();
    tk::add_task(ds[1].doc(&g, &d).unwrap(), None, "eggs (offline)").unwrap();
    ds[1].commit(&g, &d).unwrap();
    ds[0].propose_doc_epoch(&g, &d).unwrap();
    let (a, b) = ds.split_at_mut(1);
    send(&mut a[0], &mut [&mut b[0]]); // B receives the proposal before sending its edits
    assert!(b[0].take_events().iter().any(|e| matches!(e, Event::DocEpochSwitched { rebased: true, .. })));
    sync_all(&mut ds);
    assert_eq!(norm(&ds[0], &g, &d), norm(&ds[1], &g, &d));
    let tasks = tk::tasks(ds[0].doc(&g, &d).unwrap());
    assert!(tasks.iter().any(|t| t.title == "eggs (offline)"), "offline edit survived the epoch");
    assert!(tasks.iter().any(|t| t.title == "milk" && t.done));
}

#[test]
fn proposal_waits_for_missing_ops() {
    let (mut ds, g, d) = setup(2);
    tk::add_task(ds[0].doc(&g, &d).unwrap(), None, "x").unwrap();
    ds[0].commit(&g, &d).unwrap();
    let mut msgs = ds[0].outbox().unwrap(); // the update, not delivered yet
    ds[0].propose_doc_epoch(&g, &d).unwrap();
    let proposal = ds[0].outbox().unwrap();
    // Deliver the proposal first, then the update it depends on.
    for m in &proposal {
        let _ = ds[1].receive(m);
    }
    assert_eq!(ds[1].doc_generation(&g, &d), Some(0), "must wait for the missing ops");
    for m in msgs.drain(..) {
        let _ = ds[1].receive(&m);
    }
    assert_eq!(ds[1].doc_generation(&g, &d), Some(1));
    assert_eq!(norm(&ds[0], &g, &d), norm(&ds[1], &g, &d));
}

#[test]
fn removal_cutoff_makes_members_converge() {
    let (mut ds, g, d) = setup(3);
    tk::add_task(ds[0].doc(&g, &d).unwrap(), None, "shared").unwrap();
    ds[0].commit(&g, &d).unwrap();
    sync_all(&mut ds);
    // A removes C. C makes a late edit; B imports it BEFORE learning of the removal.
    let c_id = ds[2].device_id();
    ds[0].remove_member(&g, c_id).unwrap();
    let removal = ds[0].outbox().unwrap();
    tk::add_task(ds[2].doc(&g, &d).unwrap(), None, "late from C").unwrap();
    ds[2].commit(&g, &d).unwrap();
    let late = ds[2].outbox().unwrap();
    for m in &late {
        let _ = ds[1].receive(m);
        let _ = ds[0].receive(m); // A knows the removal already and rejects it
    }
    for m in &removal {
        let _ = ds[1].receive(m);
    }
    assert!(tk::tasks(ds[1].doc(&g, &d).unwrap()).iter().any(|t| t.title == "late from C"));
    assert!(!tk::tasks(ds[0].doc(&g, &d).unwrap()).iter().any(|t| t.title == "late from C"));
    // A document epoch at the cutoff brings B back in line.
    ds[0].propose_doc_epoch(&g, &d).unwrap();
    let (a, rest) = ds.split_at_mut(1);
    send(&mut a[0], &mut [&mut rest[0]]);
    assert_eq!(norm(&ds[0], &g, &d), norm(&ds[1], &g, &d));
    assert!(!tk::tasks(ds[1].doc(&g, &d).unwrap()).iter().any(|t| t.title == "late from C"));
}

/// pinned: after a rebuild every device still names who entered each entry, from its own copy, and keeps it
/// after a restart (round 5, S5).
#[test]
fn rebuild_keeps_who_entered_entries() {
    use tk::budget as b;
    let root = std::env::temp_dir().join(format!("kt-de-auth-{}-{}", std::process::id(), rand::random::<u32>()));
    let open = || {
        let mut r = Replica::open(&root.join("identity"), &root.join("data"), sandbox()).unwrap();
        r.set_semantics(Box::new(tk::TaskSemantics));
        r
    };
    let (mut ds, g, d) = setup(2);
    let mut third = open();
    ds[0].add_member(&g, third.device_public()).unwrap();
    third.accept_group(g);
    for m in ds[0].export_group(&g).unwrap() {
        let _ = third.receive(&m);
    }
    ds.push(third);
    sync_all(&mut ds);
    let doc = ds[1].doc(&g, &d).unwrap();
    let bank = b::set_account(doc, None, "Bank", "bank", 10_000).unwrap();
    let food = b::set_envelope(doc, None, "Food", 1000, "m", None, "").unwrap();
    let input = b::TxInput { kind: "exp".into(), date: "2026-11-10".into(), amount: 250, account: bank, parts: vec![(food, 250)], ..Default::default() };
    let t = b::set_tx(doc, None, &input, "Someone else", 7).unwrap();
    ds[1].commit(&g, &d).unwrap();
    sync_all(&mut ds);
    let author = ds[1].identity().peer_id();
    let who = |r: &Replica| b::transactions_by(r.doc(&g, &d).unwrap(), r.doc_authors(&g, &d).unwrap()).into_iter().find(|x| x.id == t).unwrap().by_peer;
    assert!(ds.iter().all(|r| who(r) == Some(author)));
    ds[0].propose_doc_epoch(&g, &d).unwrap();
    sync_all(&mut ds);
    assert!(ds.iter().all(|r| r.doc_generation(&g, &d) == ds[0].doc_generation(&g, &d)));
    assert_ne!(b::transactions(ds[2].doc(&g, &d).unwrap())[0].by_peer, Some(author), "the rebuild re-wrote it");
    for r in &ds {
        assert_eq!(who(r), Some(author));
    }
    drop(ds.pop());
    let again = open();
    assert_eq!(who(&again), Some(author), "kept across a restart");
}

#[test]
fn concurrent_proposals_converge() {
    let (mut ds, g, d) = setup(2);
    tk::add_task(ds[0].doc(&g, &d).unwrap(), None, "a").unwrap();
    ds[0].commit(&g, &d).unwrap();
    sync_all(&mut ds);
    ds[0].propose_doc_epoch(&g, &d).unwrap();
    ds[1].propose_doc_epoch(&g, &d).unwrap();
    tk::add_task(ds[1].doc(&g, &d).unwrap(), None, "b after its own switch").unwrap();
    ds[1].commit(&g, &d).unwrap();
    sync_all(&mut ds);
    sync_all(&mut ds);
    assert_eq!(ds[0].doc_generation(&g, &d), ds[1].doc_generation(&g, &d));
    assert_eq!(norm(&ds[0], &g, &d), norm(&ds[1], &g, &d));
    assert!(tk::tasks(ds[0].doc(&g, &d).unwrap()).iter().any(|t| t.title == "b after its own switch"));
}
