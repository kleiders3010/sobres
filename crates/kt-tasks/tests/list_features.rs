//! Shopping lists, images on tasks, reordering and renaming: all in the list's document,
//! so every member's device gets them like any other change.

use std::path::PathBuf;
use std::time::Duration;

use kt_engine::sandbox::SandboxConfig;
use kt_tasks as tk;
use kt_tasks::app::App;
use loro::{ExportMode, LoroDoc};

fn open(tag: &str) -> App {
    let r = std::env::temp_dir().join(format!("kt-feat-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-tasks-worker")), timeout: Duration::from_secs(20) };
    App::open(&r.join("identity"), &r.join("data"), sb).unwrap()
}

fn titles(d: &LoroDoc) -> Vec<String> {
    tk::tasks(d).into_iter().map(|t| t.title).collect()
}

#[test]
fn list_type_is_shared_data() {
    let d = LoroDoc::new();
    tk::init_list(&d, "Groceries").unwrap();
    assert_eq!(tk::list_type(&d), tk::ListType::Tasks, "a task list unless said otherwise");
    tk::set_list_type(&d, tk::ListType::Shopping).unwrap();
    assert_eq!(tk::list_type(&d), tk::ListType::Shopping);
    // What a newer version might write reads as a plain task list, not as damage.
    d.get_map("meta").insert("type", "kanban-v9").unwrap();
    assert_eq!(tk::list_type(&d), tk::ListType::Tasks);
    tk::set_list_type(&d, tk::ListType::Shopping).unwrap();

    // Another member's copy follows.
    let other = LoroDoc::new();
    other.import(&d.export(ExportMode::all_updates()).unwrap()).unwrap();
    assert_eq!(tk::list_type(&other), tk::ListType::Shopping);
}

#[test]
fn order_and_titles_merge_between_devices() {
    let a = LoroDoc::new();
    a.set_peer_id(1).unwrap();
    tk::init_list(&a, "Trip").unwrap();
    let ids: Vec<String> = ["passport", "tickets", "charger"].iter().map(|t| tk::add_task(&a, None, t).unwrap()).collect();
    let b = LoroDoc::new();
    b.set_peer_id(2).unwrap();
    b.import(&a.export(ExportMode::all_updates()).unwrap()).unwrap();

    // A drags "charger" to the top while B renames "tickets".
    tk::move_task(&a, &ids[2], None, 0).unwrap();
    tk::set_title(&b, &ids[1], "train tickets").unwrap();
    a.import(&b.export(ExportMode::all_updates()).unwrap()).unwrap();
    b.import(&a.export(ExportMode::all_updates()).unwrap()).unwrap();
    assert_eq!(titles(&a), vec!["charger", "passport", "train tickets"]);
    assert_eq!(titles(&a), titles(&b), "both devices show the same order");

    // A drop next to another task, before or after it.
    tk::move_task_next_to(&a, &ids[0], &ids[2], false).unwrap();
    assert_eq!(titles(&a), vec!["passport", "charger", "train tickets"]);
    tk::move_task_next_to(&a, &ids[0], &ids[1], true).unwrap();
    assert_eq!(titles(&a), vec!["charger", "train tickets", "passport"]);
}

#[test]
fn images_on_tasks() {
    let app = open("img");
    let l = app.create_list(None, "Garden").unwrap();
    let t = app.edit(&l, |d| tk::add_task(d, None, "fix the fence").map_err(Into::into)).unwrap();
    let photo: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let r = app.add_image(&l, &t, &photo).unwrap();

    let view = app.read(&l, tk::tasks).unwrap().remove(0);
    assert_eq!(view.images.len(), 1);
    assert_eq!(app.image(&l, &r.id).unwrap().unwrap(), photo, "the bytes, opened");
    assert!(app.referenced_blobs().contains(&r.id), "travels to other devices like attachments");
    assert!(app.add_image(&l, &t, &vec![0; tk::app::MAX_IMAGE_BYTES + 1]).is_err(), "too large");

    // Missing here (another device's image not arrived yet): known, not an error.
    let sealed = app.blob_store().get(&r.id).unwrap();
    app.blob_store().remove(&r.id);
    assert_eq!(app.image(&l, &r.id).unwrap(), None);
    assert_eq!(app.wanted_blobs(), vec![r.id]);
    app.blob_store().put_expected(&r.id, &sealed).unwrap();

    app.edit(&l, |d| tk::remove_image(d, &t, &r.id).map_err(Into::into)).unwrap();
    assert!(app.read(&l, tk::tasks).unwrap()[0].images.is_empty());
}

/// Two devices add a picture to the same task before hearing from each other: both stay.
#[test]
fn concurrent_images_both_survive() {
    let a = LoroDoc::new();
    a.set_peer_id(1).unwrap();
    tk::init_list(&a, "Garden").unwrap();
    let t = tk::add_task(&a, None, "fence").unwrap();
    let b = LoroDoc::new();
    b.set_peer_id(2).unwrap();
    b.import(&a.export(ExportMode::all_updates()).unwrap()).unwrap();

    let (ra, _) = kt_engine::blobs::seal(b"photo from A").unwrap();
    let (rb, _) = kt_engine::blobs::seal(b"photo from B").unwrap();
    tk::add_image(&a, &t, &ra).unwrap();
    tk::add_image(&b, &t, &rb).unwrap();
    a.import(&b.export(ExportMode::all_updates()).unwrap()).unwrap();
    b.import(&a.export(ExportMode::all_updates()).unwrap()).unwrap();
    for d in [&a, &b] {
        let ids: Vec<_> = tk::tasks(d)[0].images.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 2, "both pictures kept");
        assert!(ids.contains(&ra.id) && ids.contains(&rb.id));
    }
    // Removing one on one device leaves the other everywhere.
    tk::remove_image(&a, &t, &ra.id).unwrap();
    b.import(&a.export(ExportMode::all_updates()).unwrap()).unwrap();
    assert_eq!(tk::tasks(&b)[0].images, vec![rb]);

    // Images from the first builds (one field for all) still read, and can be removed.
    let (old, _) = kt_engine::blobs::seal(b"older photo").unwrap();
    let n = a.get_tree("tasks").nodes()[0];
    a.get_tree("tasks").get_meta(n).unwrap().insert("images", old.encode().as_str()).unwrap();
    assert!(tk::tasks(&a)[0].images.iter().any(|r| r.id == old.id));
    tk::remove_image(&a, &t, &old.id).unwrap();
    assert!(tk::tasks(&a)[0].images.iter().all(|r| r.id != old.id));
}
