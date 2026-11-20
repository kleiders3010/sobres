//! Simulated-network tests: several replicas exchange messages with shuffling,
//! duplication and loss, and must converge; attacks must be contained.

use std::path::PathBuf;
use std::time::Duration;

use kt_engine::envelope::{Envelope, Inner, MessageKind};
use kt_engine::replica::{Event, Message, Replica};
use kt_engine::sandbox::SandboxConfig;
use loro::ExportMode;
use rand::{rngs::StdRng, Rng, SeedableRng};

fn sandbox() -> SandboxConfig {
    SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-import-worker")), timeout: Duration::from_secs(20) }
}

struct Dev {
    r: Replica,
    root: PathBuf,
}

impl Dev {
    fn new(tag: &str) -> Dev {
        let root = std::env::temp_dir().join(format!("kt-sim-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
        let _ = std::fs::remove_dir_all(&root);
        let r = Replica::open(&root.join("identity"), &root.join("data"), sandbox()).unwrap();
        Dev { r, root }
    }
    fn reopen(self) -> Dev {
        let root = self.root.clone();
        drop(self.r);
        let r = Replica::open(&root.join("identity"), &root.join("data"), sandbox()).unwrap();
        Dev { r, root }
    }
}

/// Deliver every device's outbox to every other device, in shuffled order with duplicates.
fn exchange(devs: &mut [Dev], rng: &mut StdRng, rounds: usize) {
    for _ in 0..rounds {
        let mut all: Vec<(usize, Vec<u8>)> = Vec::new();
        for (i, d) in devs.iter_mut().enumerate() {
            for m in d.r.outbox().unwrap() {
                all.push((i, m));
            }
        }
        if all.is_empty() {
            break;
        }
        for (from, m) in &all {
            for (j, d) in devs.iter_mut().enumerate() {
                if j == *from {
                    continue;
                }
                let copies = if rng.gen_bool(0.2) { 2 } else { 1 };
                for _ in 0..copies {
                    let _ = d.r.receive(m);
                }
            }
        }
    }
}

fn value(d: &Dev, g: &kt_engine::GroupId, doc: &kt_engine::DocId) -> String {
    format!("{:?}", d.r.doc(g, doc).map(|x| x.get_deep_value()))
}

fn join(owner: &mut Dev, joiner: &mut Dev, g: kt_engine::GroupId) {
    owner.r.add_member(&g, joiner.r.device_public()).unwrap();
    joiner.r.accept_group(g);
    for m in owner.r.export_group(&g).unwrap() {
        let _ = joiner.r.receive(&m);
    }
}

#[test]
fn two_devices_converge_and_persist() {
    let mut rng = StdRng::seed_from_u64(1);
    let mut a = Dev::new("a");
    let mut b = Dev::new("b");
    let g = a.r.create_group().unwrap();
    let doc = a.r.create_doc(&g).unwrap();
    a.r.doc(&g, &doc).unwrap().get_map("tasks").insert("t1", "buy milk").unwrap();
    a.r.commit(&g, &doc).unwrap();
    join(&mut a, &mut b, g);
    let mut devs = vec![a, b];
    exchange(&mut devs, &mut rng, 5);
    assert_eq!(value(&devs[0], &g, &doc), value(&devs[1], &g, &doc));
    assert!(value(&devs[1], &g, &doc).contains("buy milk"));

    // concurrent edits
    devs[0].r.doc(&g, &doc).unwrap().get_map("tasks").insert("t2", "from A").unwrap();
    devs[0].r.commit(&g, &doc).unwrap();
    devs[1].r.doc(&g, &doc).unwrap().get_map("tasks").insert("t3", "from B").unwrap();
    devs[1].r.commit(&g, &doc).unwrap();
    exchange(&mut devs, &mut rng, 5);
    let v = value(&devs[0], &g, &doc);
    assert_eq!(v, value(&devs[1], &g, &doc));
    assert!(v.contains("from A") && v.contains("from B"));

    // restart persistence
    let b = devs.pop().unwrap();
    let b = b.reopen();
    assert_eq!(value(&b, &g, &doc), v);
}

#[test]
fn removed_device_cannot_read_or_write_later() {
    let mut rng = StdRng::seed_from_u64(2);
    let mut a = Dev::new("ra");
    let mut b = Dev::new("rb");
    let mut c = Dev::new("rc");
    let g = a.r.create_group().unwrap();
    let doc = a.r.create_doc(&g).unwrap();
    a.r.doc(&g, &doc).unwrap().get_map("tasks").insert("t1", "hello").unwrap();
    a.r.commit(&g, &doc).unwrap();
    join(&mut a, &mut b, g);
    join(&mut a, &mut c, g);
    let mut devs = vec![a, b, c];
    exchange(&mut devs, &mut rng, 5);
    assert!(value(&devs[2], &g, &doc).contains("hello"));

    // A removes C. C keeps editing (it doesn't know yet).
    let c_id = devs[2].r.device_id();
    devs[0].r.remove_member(&g, c_id).unwrap();
    devs[2].r.doc(&g, &doc).unwrap().get_map("tasks").insert("evil", "late edit").unwrap();
    devs[2].r.commit(&g, &doc).unwrap();
    // Deliver A's removal first to B, then C's late edit.
    let a_out = devs[0].r.outbox().unwrap();
    for m in &a_out {
        let _ = devs[1].r.receive(m);
        let _ = devs[2].r.receive(m);
    }
    let c_out = devs[2].r.outbox().unwrap();
    for m in &c_out {
        let _ = devs[0].r.receive(m);
        let _ = devs[1].r.receive(m);
    }
    assert!(!value(&devs[0], &g, &doc).contains("late edit"), "late op from removed device rejected");
    assert!(!value(&devs[1], &g, &doc).contains("late edit"));

    // A and B continue; C can't read the new epoch.
    devs[0].r.doc(&g, &doc).unwrap().get_map("tasks").insert("t2", "secret after removal").unwrap();
    devs[0].r.commit(&g, &doc).unwrap();
    exchange(&mut devs, &mut rng, 5);
    assert!(value(&devs[1], &g, &doc).contains("secret after removal"));
    assert!(!value(&devs[2], &g, &doc).contains("secret after removal"));
}

#[test]
fn impersonation_is_rejected_by_the_sandbox() {
    let mut rng = StdRng::seed_from_u64(3);
    let mut a = Dev::new("ia");
    let mut m = Dev::new("im");
    let g = a.r.create_group().unwrap();
    let doc = a.r.create_doc(&g).unwrap();
    a.r.doc(&g, &doc).unwrap().get_map("tasks").insert("t1", "real").unwrap();
    a.r.commit(&g, &doc).unwrap();
    join(&mut a, &mut m, g);
    let mut devs = vec![a, m];
    exchange(&mut devs, &mut rng, 5);

    // M squats A's *future* op counters (the A5 attack): it forks its copy (which has A's
    // real ops), switches to A's PeerID, and wraps the new ops in its own signed envelope.
    let a_peer = devs[0].r.identity().peer_id();
    let forged = devs[1].r.doc(&g, &doc).unwrap().fork();
    let before = forged.oplog_vv();
    forged.set_peer_id(a_peer).unwrap();
    forged.get_map("tasks").insert("t1", "FORGED").unwrap();
    forged.commit();
    let payload = forged.export(ExportMode::updates(&before)).unwrap();
    let epochs = devs[1].r.epochs(&g).unwrap();
    let head = epochs.head().unwrap();
    let key = epochs.key_for(&head, devs[1].r.identity()).unwrap();
    let inner = Inner { group: g, doc, generation: 0, kind: MessageKind::DocUpdate, peer: devs[1].r.identity().peer_id(), created_ms: 0, payload };
    let env = Envelope::seal(devs[1].r.identity(), head, &key, &inner).unwrap();
    let msg = Message::Envelope(env.encode().unwrap()).encode().unwrap();
    assert!(devs[0].r.receive(&msg).is_err());
    assert!(!value(&devs[0], &g, &doc).contains("FORGED"));
    assert!(devs[0].r.take_events().iter().any(|e| matches!(e, Event::Rejected { reason } if reason.contains("impersonation"))));
}

#[test]
fn poison_update_is_contained_and_quarantined() {
    let mut a = Dev::new("pa");
    let mut m = Dev::new("pm");
    let g = a.r.create_group().unwrap();
    join(&mut a, &mut m, g);
    // A has not created the document yet: its copy is empty, which is the state the
    // research crash input kills (A6).
    let poison = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/poison_empty_doc.bin")).unwrap();
    let epochs = m.r.epochs(&g).unwrap();
    let head = epochs.head().unwrap();
    let key = epochs.key_for(&head, m.r.identity()).unwrap();
    let doc = kt_engine::DocId::random();
    let inner = Inner { group: g, doc, generation: 0, kind: MessageKind::DocUpdate, peer: m.r.identity().peer_id(), created_ms: 0, payload: poison };
    let env = Envelope::seal(m.r.identity(), head, &key, &inner).unwrap();
    let msg = Message::Envelope(env.encode().unwrap()).encode().unwrap();
    // The host process survives and quarantines the update.
    assert!(a.r.receive(&msg).is_err());
    assert!(a.r.take_events().iter().any(|e| matches!(e, Event::Quarantined { .. })));
    // Receiving it again is refused without running it.
    assert!(a.r.receive(&msg).is_err());
    // After a restart the quarantine persists.
    let mut a = a.reopen();
    assert!(a.r.receive(&msg).is_err());
    assert!(!a.r.take_events().iter().any(|e| matches!(e, Event::Imported { .. })));
}

#[test]
fn randomized_convergence() {
    for seed in 0..4u64 {
        let mut rng = StdRng::seed_from_u64(100 + seed);
        let n = 3;
        let mut devs: Vec<Dev> = (0..n).map(|i| Dev::new(&format!("rnd{seed}-{i}"))).collect();
        let g = devs[0].r.create_group().unwrap();
        let doc = devs[0].r.create_doc(&g).unwrap();
        devs[0].r.doc(&g, &doc).unwrap().get_map("meta").insert("created", true).unwrap();
        devs[0].r.commit(&g, &doc).unwrap();
        for i in 1..n {
            let (left, right) = devs.split_at_mut(i);
            join(&mut left[0], &mut right[0], g);
        }
        exchange(&mut devs, &mut rng, 4);
        for step in 0..12 {
            let who = rng.gen_range(0..n);
            if devs[who].r.doc(&g, &doc).is_none() {
                continue;
            }
            let key = format!("k{}", rng.gen_range(0..5));
            devs[who].r.doc(&g, &doc).unwrap().get_map("m").insert(&key, format!("v{step}-{who}")).unwrap();
            devs[who].r.commit(&g, &doc).unwrap();
            if rng.gen_bool(0.4) {
                exchange(&mut devs, &mut rng, 1);
            }
        }
        exchange(&mut devs, &mut rng, 6);
        let v0 = value(&devs[0], &g, &doc);
        for d in &devs[1..] {
            assert_eq!(value(d, &g, &doc), v0, "seed {seed}: replicas diverged");
        }
    }
}
