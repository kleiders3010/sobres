//! Device linking (D6), app side: a desktop with its own list links to a phone's account.
//! The confirmation shows the account; afterwards the desktop has everything (its own
//! lists merged in, the phone's shared groups handed over), and Undo takes it all back.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use kt_engine::invite::{self, Code, CodeKind, InviteRecord};
use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_tasks as tk;
use kt_tasks::app::App;

fn sandbox() -> SandboxConfig {
    SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-tasks-worker")), timeout: Duration::from_secs(20) }
}

fn open(tag: &str) -> App {
    let root = std::env::temp_dir().join(format!("kt-link-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    App::open(&root.join("identity"), &root.join("data"), sandbox()).unwrap()
}

/// Slots and invite records as the networks would hold them.
#[derive(Default)]
struct Net {
    slots: HashMap<[u8; 32], HashMap<[u8; 32], Vec<u8>>>,
    board: HashMap<[u8; 32], Vec<Vec<u8>>>,
}

impl Net {
    /// Sync rounds as the apps run them: slots, invite records, then account upkeep.
    fn round(&mut self, apps: &[&App]) {
        let day = invite::day_of(kt_engine::replica::now_ms());
        for _ in 0..3 {
            for a in apps {
                let summary = a.account_summary();
                let (personal, name) = (a.personal_group(), a.display_name());
                a.with(|r: &mut Replica| {
                    for t in r.slot_targets() {
                        let blob = r.slot_blob(&t).unwrap();
                        self.slots.entry(t.topic).or_default().insert(t.writer_seed, blob);
                    }
                    let names = |g: &kt_engine::GroupId| (if *g == personal { summary.clone() } else { String::new() }, name.clone());
                    for (lookup, sealed) in r.invite_publications(day, &names).unwrap() {
                        let v = self.board.entry(lookup).or_default();
                        if !v.contains(&sealed) {
                            v.push(sealed);
                        }
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
                    r.settle_joins();
                });
                a.settle_account().unwrap();
            }
        }
    }

    fn lookup(&self, code: &Code) -> InviteRecord {
        let (lookup, key) = invite::derive(code, CodeKind::Link, invite::day_of(kt_engine::replica::now_ms())).unwrap();
        self.board[&lookup].iter().filter_map(|s| invite::open_record(s, &lookup, &key).ok()).max_by_key(|r| r.published_ms).unwrap()
    }
}

fn titles(app: &App, list: &str) -> Vec<String> {
    let l = app.find_list(list).unwrap();
    app.read(&l, tk::tasks).unwrap().into_iter().map(|t| t.title).collect()
}

fn names(app: &App) -> Vec<String> {
    let mut v: Vec<String> = app.lists().into_iter().map(|l| l.name).collect();
    v.sort();
    v
}

#[test]
fn a_desktop_links_to_a_phone_and_undo_takes_it_back() {
    let mut net = Net::default();
    let (phone, desk, friend) = (open("phone"), open("desk"), open("friend"));
    phone.set_display_name("Alex").unwrap();
    phone.set_device_name("Pixel 7").unwrap();
    desk.set_device_name("Alex's PC").unwrap();
    // The phone: a solo list, and a group shared with a friend.
    let groceries = phone.create_list(None, "Groceries").unwrap();
    phone.edit(&groceries, |d| tk::add_task(d, None, "milk").map_err(Into::into)).unwrap();
    let family = phone.create_group("Family").unwrap();
    let chores = phone.create_list(Some(family.group), "Chores").unwrap();
    phone.edit(&chores, |d| tk::add_task(d, None, "dishes").map_err(Into::into)).unwrap();
    let bundle = phone.share_group_with_device(family.group, friend.with(|r| r.identity().public())).unwrap();
    friend.join_from_bundle(family.group, &bundle).unwrap();
    // The desktop already has a list of its own.
    let notes = desk.create_list(None, "Desk notes").unwrap();
    desk.edit(&notes, |d| tk::add_task(d, None, "fix lamp").map_err(Into::into)).unwrap();
    net.round(&[&phone, &friend]);

    // The phone makes a link code; the desktop sees whose account it is before joining.
    let (code, _) = phone.create_link_code().unwrap();
    net.round(&[&phone]);
    let rec = net.lookup(&code);
    assert_eq!(rec.inviter_name, "Alex");
    assert_eq!(rec.group_name, "Pixel 7 · 2 lists (Chores, Groceries)");
    desk.begin_link(&rec).unwrap();
    assert_eq!(desk.linking(), Some(phone.personal_group()));

    net.round(&[&phone, &desk, &friend]);
    net.round(&[&phone, &desk, &friend]);
    assert_eq!(desk.linking(), None, "linked");
    assert_eq!(desk.personal_group(), phone.personal_group(), "one account");
    assert_eq!(desk.display_name(), "Alex");
    assert_eq!(names(&desk), ["Chores", "Desk notes", "Groceries"], "everything, plus its own list");
    assert_eq!(names(&phone), ["Chores", "Desk notes", "Groceries"], "its own list reached the phone");
    assert_eq!(titles(&phone, "Desk notes"), ["fix lamp"]);
    assert_eq!(titles(&desk, "Chores"), ["dishes"]);
    let mut devices: Vec<String> = phone.devices().into_iter().map(|d| d.name).collect();
    devices.sort();
    assert_eq!(devices, ["Alex's PC", "Pixel 7"]);
    let recent = phone.recent_links();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].name, "Alex's PC");
    assert_eq!(desk.recent_links().len(), 0, "the new device itself isn't asked");

    // The desktop edits the shared list; the friend sees it.
    let c = desk.find_list("Chores").unwrap();
    desk.edit(&c, |d| tk::add_task(d, None, "laundry").map_err(Into::into)).unwrap();
    net.round(&[&phone, &desk, &friend]);
    assert_eq!(titles(&friend, "Chores"), ["dishes", "laundry"]);

    // Undo on the phone: the desktop leaves the account and the shared group.
    let desk_id = desk.with(|r| r.device_id());
    phone.unlink_device(desk_id).unwrap();
    net.round(&[&phone, &desk, &friend]);
    assert!(!friend.with(|r| r.members(&family.group)).contains(&desk_id), "out of the shared group");
    assert!(!phone.devices().iter().any(|d| d.device == desk_id), "out of the account");
    assert!(phone.recent_links().is_empty());
    assert!(desk.edit(&c, |d| tk::add_task(d, None, "after undo").map_err(Into::into)).is_err(), "the shared list is gone from it");
    net.round(&[&phone, &desk, &friend]);
    assert_eq!(titles(&friend, "Chores"), ["dishes", "laundry"]);
    assert_eq!(phone.with(|r| r.members(&family.group)).len(), 2, "not re-added by account upkeep");
    // The desktop is on its own again, with copies of the account's lists it had.
    assert_ne!(desk.personal_group(), phone.personal_group());
    assert_eq!(names(&desk), ["Desk notes", "Groceries"]);
    let g = desk.find_list("Groceries").unwrap();
    desk.edit(&g, |d| tk::add_task(d, None, "its own now").map_err(Into::into)).unwrap();
    net.round(&[&phone, &desk, &friend]);
    assert_eq!(titles(&phone, "Groceries"), ["milk"], "separate copies");
}
