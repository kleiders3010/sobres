//! Always backward compatible (principle 8): data from a newer app version isn't an error.
//! An older device keeps it, passes it on untouched, and knows to offer the update; the
//! feature floor tells a group what every device supports.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use kt_engine::replica::{Replica, FEATURE_LEVEL};
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{wire, EpochId, GroupId};

fn dev(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-ver-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-import-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

#[derive(Default)]
struct Net(HashMap<[u8; 32], HashMap<[u8; 32], Vec<u8>>>);

impl Net {
    fn write(&mut self, r: &mut Replica) {
        for t in r.slot_targets() {
            let blob = r.slot_blob(&t).unwrap();
            self.0.entry(t.topic).or_default().insert(t.writer_seed, blob);
        }
    }
    fn read(&self, r: &mut Replica) {
        for t in r.read_topics() {
            if let Some(ws) = self.0.get(&t.topic) {
                let blobs: Vec<&[u8]> = ws.values().map(|b| b.as_slice()).collect();
                r.open_slots(&t, &blobs).unwrap();
            }
        }
    }
}

#[test]
fn newer_data_is_kept_and_passed_on() {
    let (mut a, mut b, mut c) = (dev("a"), dev("b"), dev("c"));
    let g = a.create_group().unwrap();
    for x in [&mut b, &mut c] {
        a.add_member(&g, x.device_public()).unwrap();
    }
    let bundle = a.export_group(&g).unwrap();
    for x in [&mut b, &mut c] {
        x.accept_group(g);
        for m in &bundle {
            let _ = x.receive(m);
        }
    }
    // A newer app's message: a message kind this build doesn't know (kinds are appended).
    let future = {
        let mut m = b"MS".to_vec();
        m.extend_from_slice(&1u16.to_le_bytes());
        m.push(42); // kind 42
        m.extend_from_slice(b"whatever a future version says");
        m
    };
    assert!(matches!(kt_engine::replica::Message::decode(&future), Err(kt_engine::Error::Newer(_))));

    // It arrives in A's slot (as a newer A would write it).
    #[derive(serde::Serialize)]
    struct Payload {
        group: GroupId,
        epoch: EpochId,
        messages: Vec<Vec<u8>>,
    }
    let mut net = Net::default();
    let t = a.read_topics().into_iter().find(|t| t.group == g).unwrap();
    let wt = a.slot_targets().into_iter().find(|w| w.group == g && w.topic == t.topic).unwrap();
    let plain = wire::encode(*b"SL", 1, &Payload { group: g, epoch: t.epoch, messages: vec![future.clone()] }).unwrap();
    net.0.entry(t.topic).or_default().insert(wt.writer_seed, a.fuzz_seal_slot(&t, &plain).unwrap());

    // B reads it: not a failure, kept, and B knows a newer version exists.
    assert!(!b.newer_version_seen());
    let rep = b.open_slots(&t, &net.0[&t.topic].values().map(|x| x.as_slice()).collect::<Vec<_>>()).unwrap();
    assert_eq!(rep.failed, 0, "newer data isn't an error");
    assert!(b.newer_version_seen());

    // B passes it on: C, who only reads B's slot, gets it too.
    let mut only_b = Net::default();
    only_b.write(&mut b);
    only_b.read(&mut c);
    assert!(c.newer_version_seen(), "passed on untouched through an older device");

    // Everyone here is at this build's feature level.
    net.write(&mut a);
    net.write(&mut b);
    net.read(&mut c);
    assert_eq!(c.feature_floor(&g), FEATURE_LEVEL);
}
