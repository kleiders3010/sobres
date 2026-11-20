//! Backups (DESIGN §11): a device is lost; a fresh device restores the sealed file. Solo
//! lists come back as its own; shared groups are readable at once and it's let back in by
//! a member's device through the recovery invite.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_tasks::app::App;
use kt_tasks as tk;

fn sandbox() -> SandboxConfig {
    SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-tasks-worker")), timeout: Duration::from_secs(20) }
}

fn root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kt-bk-{tag}-{}-{}", std::process::id(), rand::random::<u32>()))
}

#[derive(Default)]
struct Mailbox {
    slots: HashMap<[u8; 32], HashMap<[u8; 32], Vec<u8>>>,
}

impl Mailbox {
    fn sync(&mut self, apps: &[&App]) {
        for _ in 0..3 {
            for a in apps {
                a.with(|r: &mut Replica| {
                    for t in r.slot_targets() {
                        let blob = r.slot_blob(&t).unwrap();
                        self.slots.entry(t.topic).or_default().insert(t.writer_seed, blob);
                    }
                });
            }
            for a in apps {
                a.with(|r: &mut Replica| {
                    for _ in 0..2 {
                        for t in r.read_topics() {
                            if let Some(ws) = self.slots.get(&t.topic) {
                                let blobs: Vec<&[u8]> = ws.values().map(|b| b.as_slice()).collect();
                                r.open_slots(&t, &blobs).unwrap();
                            }
                        }
                    }
                });
            }
        }
    }
}

fn titles(app: &App, list: &str) -> Vec<String> {
    let l = app.find_list(list).unwrap();
    app.read(&l, tk::tasks).unwrap().into_iter().map(|t| t.title).collect()
}

#[test]
fn a_lost_device_comes_back_from_its_backup() {
    let mut mb = Mailbox::default();
    let (ra, rb) = (root("a"), root("b"));
    let a = App::open(&ra.join("identity"), &ra.join("data"), sandbox()).unwrap();
    let b = App::open(&rb.join("identity"), &rb.join("data"), sandbox()).unwrap();
    a.set_display_name("Alex").unwrap();
    // A solo list and a shared group with B.
    let solo = a.create_list(None, "Diary").unwrap();
    a.edit(&solo, |d| tk::add_task(d, None, "private note").map_err(Into::into)).unwrap();
    let fam = a.create_group("Family").unwrap();
    let groceries = a.create_list(Some(fam.group), "Groceries").unwrap();
    a.edit(&groceries, |d| tk::add_task(d, None, "milk").map_err(Into::into)).unwrap();
    let bundle = a.share_group_with_device(fam.group, b.with(|r| r.identity().public())).unwrap();
    b.join_from_bundle(fam.group, &bundle).unwrap();
    let _ = a.with(|r| r.outbox());
    mb.sync(&[&a, &b]);

    // A makes a backup; its recovery invite reaches B with the next sync.
    let file = a.backup(30).unwrap();
    assert_eq!(&file[..4], b"KTB1");
    mb.sync(&[&a, &b]);
    drop(a); // the phone is lost

    // A fresh device restores the file.
    let rn = root("new");
    let (n, summary) = App::restore(&rn.join("identity"), &rn.join("data"), sandbox(), &file).unwrap();
    assert_eq!(summary.rejoining, vec![fam.group]);
    assert_eq!(summary.rehomed_lists, 1, "the solo list came back as ours");
    assert_eq!(n.display_name(), "Alex");
    assert_eq!(titles(&n, "Diary"), ["private note"]);
    assert_eq!(n.groups().iter().filter(|g| g.personal || g.name == "Personal").count(), 1, "one personal group");
    assert_eq!(titles(&n, "Groceries"), ["milk"], "shared lists readable at once");
    // A backup made while still waiting to be let back in keeps the shared group.
    let early = kt_engine::replica::backup::open(&n.backup(30).unwrap()).unwrap();
    assert!(early.groups.iter().any(|g| g.group == fam.group && g.recovery.is_some()));
    let diary = n.find_list("Diary").unwrap();
    n.edit(&diary, |d| tk::add_task(d, None, "after restore").map_err(Into::into)).unwrap();

    // B's device lets the restored device back in; editing flows both ways again.
    mb.sync(&[&n, &b]);
    assert!(n.with(|r| r.epochs(&fam.group).unwrap().is_current_member(&r.device_id())), "back in the group");
    let g = n.find_list("Groceries").unwrap();
    n.edit(&g, |d| tk::add_task(d, None, "eggs").map_err(Into::into)).unwrap();
    mb.sync(&[&n, &b]);
    assert_eq!(titles(&b, "Groceries"), ["milk", "eggs"]);

    // The old phone is still in the group: the restored device can remove it.
    let old = summary.old_device.expect("old device known");
    let (who, groups) = n.replaced_device().expect("offer");
    assert_eq!((who, groups.clone()), (old, vec![fam.group]));
    assert_eq!(n.remove_replaced_device().unwrap(), 1);
    mb.sync(&[&n, &b]);
    assert!(!b.with(|r| r.epochs(&fam.group).unwrap().is_current_member(&old)), "removed for everyone");
    assert!(n.replaced_device().unwrap().1.is_empty(), "nothing left to offer");
}

/// With another device of ours still around (device linking), the personal group comes
/// back as ours instead of being copied; edits made before being let back in arrive too.
#[test]
fn restoring_next_to_a_linked_device_keeps_the_personal_group() {
    let mut mb = Mailbox::default();
    let (rp, rd) = (root("phone"), root("desk"));
    let phone = App::open(&rp.join("identity"), &rp.join("data"), sandbox()).unwrap();
    let desk = App::open(&rd.join("identity"), &rd.join("data"), sandbox()).unwrap();
    phone.set_display_name("Alex").unwrap();
    let todo = phone.create_list(None, "Todo").unwrap();
    phone.edit(&todo, |d| tk::add_task(d, None, "one").map_err(Into::into)).unwrap();
    // Linked at the engine level (the app-level linking flow is separate).
    let personal = phone.personal_group();
    let desk_pub = desk.with(|r| r.identity().public());
    let bundle = phone.with(|r| {
        r.add_member(&personal, desk_pub).unwrap();
        r.export_group(&personal).unwrap()
    });
    desk.with(|r| {
        r.accept_group(personal);
        for m in &bundle {
            let _ = r.receive(m);
        }
    });
    let _ = phone.with(|r| r.outbox());
    mb.sync(&[&phone, &desk]);
    let desk_titles = |desk: &App| desk.with(|r| tk::tasks(r.doc(&personal, &todo.doc).unwrap()).into_iter().map(|t| t.title).collect::<Vec<_>>());
    assert_eq!(desk_titles(&desk), ["one"]);

    let file = phone.backup(30).unwrap();
    mb.sync(&[&phone, &desk]);
    drop(phone);

    let rn = root("phone2");
    let (n, summary) = App::restore(&rn.join("identity"), &rn.join("data"), sandbox(), &file).unwrap();
    assert_eq!(summary.rehomed_lists, 0);
    assert_eq!(summary.rejoining.len(), 1);
    assert_eq!(n.display_name(), "Alex");
    assert_eq!(n.groups().len(), 1, "one personal group, the old one");
    let l = n.find_list("Todo").unwrap();
    n.edit(&l, |d| tk::add_task(d, None, "before rejoin").map_err(Into::into)).unwrap();
    mb.sync(&[&n, &desk]);
    mb.sync(&[&n, &desk]);
    assert_eq!(desk_titles(&desk), ["one", "before rejoin"]);
}

#[test]
fn a_tasklist_file_round_trips() {
    let (ra, rb) = (root("x"), root("y"));
    let a = App::open(&ra.join("identity"), &ra.join("data"), sandbox()).unwrap();
    let b = App::open(&rb.join("identity"), &rb.join("data"), sandbox()).unwrap();
    let l = a.create_list(None, "Trip").unwrap();
    let passport = a.edit(&l, |d| tk::add_task(d, None, "passport").map_err(Into::into)).unwrap();
    a.edit(&l, |d| tk::add_task(d, Some(&passport), "check expiry").map_err(Into::into)).unwrap();
    a.edit(&l, |d| tk::set_done(d, &passport, true).map_err(Into::into)).unwrap();
    assert_eq!(a.export_markdown(&l).unwrap(), "# Trip\n\n- [x] passport\n\t- [ ] check expiry\n");

    let file = a.export_list_file(&l).unwrap();
    assert!(!file.windows(8).any(|w| w == b"passport"), "sealed");
    let imported = b.import_list_file(None, &file).unwrap();
    assert_eq!(imported.name, "Trip");
    assert_eq!(b.export_markdown(&imported).unwrap(), a.export_markdown(&l).unwrap());
    let back = b.edit(&imported, |d| tk::add_task(d, None, "tickets").map_err(Into::into));
    assert!(back.is_ok(), "the imported list is ours to edit");
    assert!(b.import_list_file(None, b"KTL1 not really").is_err());
    assert!(b.import_list_file(None, &a.backup(30).unwrap()).is_err(), "a backup isn't a list file");
}
