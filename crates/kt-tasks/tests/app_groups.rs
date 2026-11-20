//! App layer: a group holds several lists; sharing a group shares all of them; lists move
//! between groups; solo lists live in the personal group.

use std::path::PathBuf;
use std::time::Duration;

use kt_engine::sandbox::SandboxConfig;
use kt_tasks::app::App;
use kt_tasks as tk;

fn sandbox() -> SandboxConfig {
    SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-tasks-worker")), timeout: Duration::from_secs(20) }
}

fn root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kt-app-{tag}-{}-{}", std::process::id(), rand::random::<u32>()))
}

fn open(r: &PathBuf) -> App {
    App::open(&r.join("identity"), &r.join("data"), sandbox()).unwrap()
}

fn titles(app: &App, list: &str) -> Vec<String> {
    let l = app.find_list(list).unwrap();
    app.read(&l, tk::tasks).unwrap().into_iter().map(|t| t.title).collect()
}

fn names(app: &App, group: &str) -> Vec<String> {
    let g = app.find_group(group).unwrap();
    let mut v: Vec<String> = app.lists().into_iter().filter(|l| l.group == g.group).map(|l| l.name).collect();
    v.sort();
    v
}

#[test]
fn group_with_several_lists_is_shared_as_a_whole() {
    let (ra, rb) = (root("a"), root("b"));
    let a = open(&ra);
    let b = open(&rb);

    let solo = a.create_list(None, "Mine").unwrap();
    assert!(solo.group == a.personal_group());
    let fam = a.create_group("Family").unwrap();
    for (name, task) in [("Groceries", "milk"), ("Chores", "laundry")] {
        let l = a.create_list(Some(fam.group), name).unwrap();
        a.edit(&l, |d| tk::add_task(d, None, task).map_err(Into::into)).unwrap();
    }
    assert_eq!(names(&a, "Family"), ["Chores", "Groceries"]);

    // Sharing the group gives B every list in it, and nothing from A's personal group.
    let mut bundle = a.share_group_with_device(fam.group, b.with(|r| r.identity().public())).unwrap();
    b.join_from_bundle(fam.group, &bundle).unwrap();
    assert_eq!(names(&b, "Family"), ["Chores", "Groceries"]);
    assert_eq!(titles(&b, "Groceries"), ["milk"]);
    assert!(b.find_list("Mine").is_err());

    // A list created later in the group reaches B too, and so does a list moved into it.
    let later = a.create_list(Some(fam.group), "Trips").unwrap();
    a.edit(&later, |d| tk::add_task(d, None, "Rome").map_err(Into::into)).unwrap();
    let mv = a.find_list("Mine").unwrap();
    a.edit(&mv, |d| tk::add_task(d, None, "dentist").map_err(Into::into)).unwrap();
    a.move_list(&mv, fam.group).unwrap();
    assert!(a.lists().iter().filter(|l| l.name == "Mine").all(|l| l.group == fam.group));
    bundle = a.with(|r| r.outbox()).unwrap();
    for m in &bundle {
        let _ = b.with(|r| r.receive(m));
    }
    assert_eq!(names(&b, "Family"), ["Chores", "Groceries", "Mine", "Trips"]);
    assert_eq!(titles(&b, "Mine"), ["dentist"]);

    // B edits flow back; state survives reopening.
    let g = b.find_list("Groceries").unwrap();
    b.edit(&g, |d| tk::add_task(d, None, "eggs").map_err(Into::into)).unwrap();
    for m in b.with(|r| r.outbox()).unwrap() {
        let _ = a.with(|r| r.receive(&m));
    }
    drop(a);
    let a = open(&ra);
    assert_eq!(titles(&a, "Groceries"), ["milk", "eggs"]);
    assert_eq!(names(&a, "Family"), ["Chores", "Groceries", "Mine", "Trips"]);
    assert_eq!(names(&a, "Personal"), Vec::<String>::new());
}

#[test]
fn personal_group_cannot_be_shared() {
    let (ra, rb) = (root("pa"), root("pb"));
    let a = open(&ra);
    let b = open(&rb);
    let p = a.personal_group();
    assert!(a.share_group_with_device(p, b.with(|r| r.identity().public())).is_err());
}
