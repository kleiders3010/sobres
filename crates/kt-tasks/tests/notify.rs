//! Notifications about others' changes, from merged state (DESIGN §8): who did what,
//! nothing about our own changes, nothing twice, nothing for a first look.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_tasks as tk;
use kt_tasks::app::{App, ChangeKind};

fn open(tag: &str) -> App {
    let root = std::env::temp_dir().join(format!("kt-notify-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
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

#[test]
fn others_changes_become_notifications() {
    let mut mb = Mailbox::default();
    let (alex, sam) = (open("alex"), open("sam"));
    alex.set_display_name("Alex").unwrap();
    sam.set_display_name("Sam").unwrap();
    let fam = alex.create_group("Family").unwrap();
    let list = alex.create_list(Some(fam.group), "Groceries").unwrap();
    let bread = alex.edit(&list, |d| tk::add_task(d, None, "bread").map_err(Into::into)).unwrap();
    let bundle = alex.share_group_with_device(fam.group, sam.with(|r| r.identity().public())).unwrap();
    sam.join_from_bundle(fam.group, &bundle).unwrap();
    mb.sync(&[&alex, &sam]);
    assert!(sam.take_changes().unwrap().is_empty(), "a first look reports nothing");
    assert!(alex.take_changes().unwrap().is_empty());

    // Sam adds two tasks and completes one of Alex's; Alex adds one of their own.
    let l = sam.find_list("Groceries").unwrap();
    sam.edit(&l, |d| tk::add_task(d, None, "milk").map_err(Into::into)).unwrap();
    sam.edit(&l, |d| tk::add_task(d, None, "eggs").map_err(Into::into)).unwrap();
    sam.edit(&l, |d| tk::set_done(d, &bread, true).map_err(Into::into)).unwrap();
    alex.edit(&list, |d| tk::add_task(d, None, "apples").map_err(Into::into)).unwrap();
    mb.sync(&[&alex, &sam]);

    let changes = alex.take_changes().unwrap();
    let got: Vec<(String, ChangeKind, String)> = changes.iter().map(|c| (c.who.clone(), c.kind, c.title.clone())).collect();
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(got.contains(&("Sam".into(), ChangeKind::Added, "milk".into())));
    assert!(got.contains(&("Sam".into(), ChangeKind::Added, "eggs".into())));
    assert!(got.contains(&("Sam".into(), ChangeKind::Completed, "bread".into())));
    assert!(changes.iter().all(|c| c.list.as_ref().unwrap().name == "Groceries" && c.at_ms > 0));
    assert!(alex.take_changes().unwrap().is_empty(), "nothing twice");

    let sams = sam.take_changes().unwrap();
    assert_eq!(sams.len(), 1);
    assert_eq!((sams[0].who.as_str(), sams[0].title.as_str()), ("Alex", "apples"));
}

#[test]
fn every_join_notifies_the_group() {
    let mut mb = Mailbox::default();
    let (alex, sam, kim) = (open("j-alex"), open("j-sam"), open("j-kim"));
    alex.set_display_name("Alex").unwrap();
    sam.set_display_name("Sam").unwrap();
    kim.set_display_name("Kim").unwrap();
    let fam = alex.create_group("Family").unwrap();
    let b = alex.share_group_with_device(fam.group, sam.with(|r| r.identity().public())).unwrap();
    sam.join_from_bundle(fam.group, &b).unwrap();
    mb.sync(&[&alex, &sam]);
    alex.take_changes().unwrap();
    sam.take_changes().unwrap();

    // Kim joins; Kim's name arrives with Kim's first sync.
    let b = alex.share_group_with_device(fam.group, kim.with(|r| r.identity().public())).unwrap();
    kim.join_from_bundle(fam.group, &b).unwrap();
    mb.sync(&[&alex, &sam, &kim]);
    mb.sync(&[&alex, &sam, &kim]);
    let got: Vec<(String, ChangeKind, String)> = sam.take_changes().unwrap().into_iter().map(|c| (c.who, c.kind, c.group_name)).collect();
    assert_eq!(got, [("Kim".to_string(), ChangeKind::Joined, "Family".to_string())]);
    assert!(sam.take_changes().unwrap().is_empty(), "once");
    assert!(kim.take_changes().unwrap().is_empty(), "not about joining yourself");
}
