//! "Undo changes by [person] since [time]" (DESIGN §4, D27), across a compaction: the
//! list is compacted after Sam's edits, and Alex can still undo them from the archived
//! generation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use kt_engine::replica::CompactionPolicy;
use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_tasks as tk;
use kt_tasks::app::App;

fn open(tag: &str) -> App {
    let root = std::env::temp_dir().join(format!("kt-undo-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
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

fn titles(app: &App, list: &tk::app::ListInfo) -> Vec<String> {
    let mut v: Vec<String> = app.read(list, tk::tasks).unwrap().into_iter().map(|t| t.title).collect();
    v.sort();
    v
}

#[test]
fn undo_a_persons_changes_even_after_a_compaction() {
    let mut mb = Mailbox::default();
    let (alex, sam) = (open("alex"), open("sam"));
    alex.set_display_name("Alex").unwrap();
    sam.set_display_name("Sam").unwrap();
    let fam = alex.create_group("Family").unwrap();
    let list = alex.create_list(Some(fam.group), "Groceries").unwrap();
    let milk = alex.edit(&list, |d| tk::add_task(d, None, "milk").map_err(Into::into)).unwrap();
    alex.edit(&list, |d| tk::add_task(d, None, "eggs").map_err(Into::into)).unwrap();
    let bundle = alex.share_group_with_device(fam.group, sam.with(|r| r.identity().public())).unwrap();
    sam.join_from_bundle(fam.group, &bundle).unwrap();
    mb.sync(&[&alex, &sam]);

    std::thread::sleep(Duration::from_millis(1100));
    let since = kt_engine::replica::now_ms();
    std::thread::sleep(Duration::from_millis(1100));

    // Sam's edits.
    let l = sam.find_list("Groceries").unwrap();
    sam.edit(&l, |d| tk::set_title(d, &milk, "oat milk?!").map_err(Into::into)).unwrap();
    let eggs = sam.read(&l, tk::tasks).unwrap().into_iter().find(|t| t.title == "eggs").unwrap().id;
    sam.edit(&l, |d| tk::delete_task(d, &eggs).map_err(Into::into)).unwrap();
    sam.edit(&l, |d| tk::add_task(d, None, "spam").map_err(Into::into)).unwrap();
    mb.sync(&[&alex, &sam]);
    assert_eq!(titles(&alex, &list), ["oat milk?!", "spam"]);

    // The list gets compacted (history of Sam's edits leaves the live document).
    let eager = CompactionPolicy { min_bytes: 0, ratio: 0, active_ms: 30 * 86_400_000, cooldown_ms: 0 };
    for a in [&alex, &sam] {
        a.with(|r| r.set_compaction_policy(eager.clone()));
    }
    for _ in 0..4 {
        for a in [&alex, &sam] {
            a.with(|r| {
                let _ = r.auto_compact();
            });
        }
        mb.sync(&[&alex, &sam]);
    }
    assert!(!alex.with(|r| r.doc_archive(&list.group, &list.doc)).is_empty(), "compacted, with the old generation archived");
    for a in [&alex, &sam] {
        a.with(|r| r.set_compaction_policy(CompactionPolicy { min_bytes: usize::MAX, ..Default::default() }));
    }

    assert_eq!(alex.lists().len(), 1, "compaction kept the group's list index");
    // Alex undoes Sam's changes since then.
    let sam_person = alex.people(&list).into_iter().find(|p| p.name == "Sam").expect("Sam");
    assert!(!sam_person.is_me);
    let n = alex.undo_changes_by(&list, &sam_person, since).unwrap();
    assert_eq!(n, 3, "rename, deletion and addition undone");
    assert_eq!(titles(&alex, &list), ["eggs", "milk"]);
    mb.sync(&[&alex, &sam]);
    assert_eq!(titles(&sam, &l), ["eggs", "milk"], "for everyone");
}
