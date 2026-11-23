//! Second-pass audit T2-13 (rule 17): a device let into a shared list without a person pressing anything is shown to
//! the members ("Sam joined Groceries · Remove · OK"), and Remove takes it out.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_tasks::app::App;

fn open(tag: &str) -> App {
    let root = std::env::temp_dir().join(format!("kt-joins-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sandbox = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-tasks-worker")), timeout: Duration::from_secs(20) };
    App::open(&root.join("identity"), &root.join("data"), sandbox).unwrap()
}

#[derive(Default)]
struct Mailbox(HashMap<[u8; 32], HashMap<[u8; 32], Vec<u8>>>);

impl Mailbox {
    fn sync(&mut self, apps: &[&App]) {
        for a in apps {
            a.publish_names().unwrap();
        }
        for _ in 0..3 {
            for a in apps {
                a.with(|r: &mut Replica| {
                    for t in r.slot_targets() {
                        let blob = r.slot_blob(&t).unwrap();
                        self.0.entry(t.topic).or_default().insert(t.writer_seed, blob);
                    }
                });
            }
            for a in apps {
                a.with(|r: &mut Replica| {
                    for t in r.read_topics() {
                        if let Some(ws) = self.0.get(&t.topic) {
                            let blobs: Vec<&[u8]> = ws.values().map(|b| b.as_slice()).collect();
                            r.open_slots(&t, &blobs).unwrap();
                        }
                    }
                });
            }
        }
    }
}

// pinned: rule 17, anything a machine lets in is shown to people with an undo
#[test]
fn a_new_member_is_shown_with_remove_and_ok() {
    let mut mb = Mailbox::default();
    let (alex, sam, kim) = (open("alex"), open("sam"), open("kim"));
    alex.set_display_name("Alex").unwrap();
    sam.set_display_name("Sam").unwrap();
    kim.set_display_name("Kim").unwrap();
    let fam = alex.create_group("Family").unwrap();
    alex.create_list(Some(fam.group), "Groceries").unwrap();
    assert!(alex.recent_joins().is_empty());

    let bundle = alex.share_group_with_device(fam.group, sam.with(|r| r.identity().public())).unwrap();
    sam.join_from_bundle(fam.group, &bundle).unwrap();
    mb.sync(&[&alex, &sam]);
    let joins = alex.recent_joins();
    assert_eq!(joins.len(), 1, "{joins:?}");
    let sam_dev = sam.with(|r| r.device_id());
    assert_eq!(joins[0].device, sam_dev);
    assert_eq!(joins[0].by, "Alex");
    assert!(sam.recent_joins().iter().all(|j| j.device != sam_dev), "nobody is told about their own join");

    alex.dismiss_join(&fam.group, &sam_dev).unwrap();
    assert!(alex.recent_joins().is_empty(), "OK hides it on this device");

    let bundle = alex.share_group_with_device(fam.group, kim.with(|r| r.identity().public())).unwrap();
    kim.join_from_bundle(fam.group, &bundle).unwrap();
    mb.sync(&[&alex, &sam, &kim]);
    let kim_dev = kim.with(|r| r.device_id());
    let on_sam = sam.recent_joins();
    assert!(on_sam.iter().any(|j| j.device == kim_dev), "every member is told: {on_sam:?}");
    sam.remove_joined(&fam.group, &kim_dev).unwrap();
    assert!(!sam.with(|r| r.members(&fam.group)).contains(&kim_dev), "Remove takes the device out");
    assert!(sam.recent_joins().iter().all(|j| j.device != kim_dev));
}
