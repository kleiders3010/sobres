//! A shared list says when its newest changes are still only on this device (cold UX review U-T4): the write
//! round marks a group as waiting while no mailbox holds its current slot, and clears it once one does.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_net::mailbox::{BoxFut, Capabilities, Mailbox, SlotRead, SlotWrite, WriteOutcome};
use kt_net::sync::Syncer;

fn replica(tag: &str) -> Replica {
    let root = std::env::temp_dir().join(format!("kt-here-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-net-worker")), timeout: Duration::from_secs(20) };
    Replica::open(&root.join("identity"), &root.join("data"), sb).unwrap()
}

/// A mailbox that is reachable or not, as the test says.
struct Switch(AtomicBool);

impl Mailbox for Switch {
    fn name(&self) -> String {
        "switch".into()
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { replace: true, max_slot: 1 << 20, ttl: None, read_auth: false, subscribe: false, write_budget: None }
    }
    fn writer_id(&self, writer_seed: &[u8; 32]) -> [u8; 32] {
        *writer_seed
    }
    fn write<'a>(&'a self, _w: &'a SlotWrite) -> BoxFut<'a, WriteOutcome> {
        let up = self.0.load(Ordering::SeqCst);
        Box::pin(async move { if up { WriteOutcome::Stored { stored: 1, tried: 1 } } else { WriteOutcome::Unavailable } })
    }
    fn read<'a>(&'a self, _topics: &'a [[u8; 32]]) -> BoxFut<'a, Vec<SlotRead>> {
        Box::pin(async { Vec::new() })
    }
}

#[tokio::test]
async fn a_change_no_mailbox_took_is_shown_as_only_here() {
    let mut a = replica("a");
    let b = replica("b");
    let g = a.create_group().unwrap();
    let d = a.create_doc(&g).unwrap();
    a.doc(&g, &d).unwrap().get_map("tasks").insert("t0", "start").unwrap();
    a.commit(&g, &d).unwrap();
    a.add_member(&g, b.device_public()).unwrap();
    let rep = Arc::new(Mutex::new(a));
    let mb = Arc::new(Switch(AtomicBool::new(false)));
    let s = Syncer::new(rep.clone(), vec![mb.clone() as Arc<dyn Mailbox>]);

    assert!(!s.is_waiting(&g), "nothing is claimed before the first round");
    s.write_round().await;
    assert!(s.is_waiting(&g), "offline: the change is only here");

    mb.0.store(true, Ordering::SeqCst);
    s.write_round().await;
    assert!(!s.is_waiting(&g), "a mailbox took it");
    s.write_round().await;
    assert!(!s.is_waiting(&g), "nothing new: still sent");

    mb.0.store(false, Ordering::SeqCst);
    {
        let mut r = rep.lock().unwrap();
        r.doc(&g, &d).unwrap().get_map("tasks").insert("t1", "more").unwrap();
        r.commit(&g, &d).unwrap();
    }
    s.write_round().await;
    assert!(s.is_waiting(&g), "a new change while offline is only here again");
}
