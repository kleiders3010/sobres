//! Text attachments (D4): long notes become a sealed blob; the task keeps a reference and
//! a preview, which travel, merge and undo together as one field.

use std::path::PathBuf;
use std::time::Duration;

use kt_engine::sandbox::SandboxConfig;
use kt_tasks as tk;
use kt_tasks::app::App;

fn open(tag: &str) -> App {
    let r = std::env::temp_dir().join(format!("kt-att-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-tasks-worker")), timeout: Duration::from_secs(20) };
    App::open(&r.join("identity"), &r.join("data"), sb).unwrap()
}

#[test]
fn long_notes_become_an_attachment() {
    let a = open("a");
    let l = a.create_list(None, "Research").unwrap();
    let t = a.edit(&l, |d| tk::add_task(d, None, "read the paper").map_err(Into::into)).unwrap();
    let long = "Paragraph with some detail about the paper. ".repeat(5000); // ~220 KB
    a.set_notes(&l, &t, &long).unwrap();

    let n = a.notes(&l, &t).unwrap();
    assert!(n.complete && n.text == long && n.size == long.len() as u64);
    let view = a.read(&l, tk::tasks).unwrap().remove(0);
    assert!(view.attachment().is_some());
    assert!(view.notes.len() < 10_000, "the document holds a reference and a preview, not the text");
    assert!(long.starts_with(view.notes_preview()) && !view.notes_preview().is_empty());
    assert!(a.wanted_blobs().is_empty(), "made here, so it's here");
    let md = a.export_markdown(&l).unwrap();
    assert!(md.contains("(continues in the app)") && md.len() < 10_000);

    // Without the blob: the preview, marked incomplete, and the blob is wanted.
    let id = view.attachment().unwrap().id;
    let sealed = a.blob_store().get(&id).unwrap();
    a.blob_store().remove(&id);
    let n = a.notes(&l, &t).unwrap();
    assert!(!n.complete && long.starts_with(&n.text));
    assert_eq!(a.wanted_blobs(), vec![id]);
    a.blob_store().put_expected(&id, &sealed).unwrap();
    assert!(a.notes(&l, &t).unwrap().complete);

    // Short again: back inline; the old blob is garbage only after the keep time.
    a.set_notes(&l, &t, "short now").unwrap();
    assert_eq!(a.notes(&l, &t).unwrap().text, "short now");
    assert!(a.referenced_blobs().is_empty());
    assert_eq!(a.collect_blob_garbage(), 0, "kept for undo");
    assert!(a.blob_store().has(&id));

    // Typed text can't pretend to be an attachment.
    a.set_notes(&l, &t, "\u{0}blob:v1.ff.ee.1\nhi").unwrap_err();
    a.set_notes(&l, &t, "a\u{0}b").unwrap();
    assert_eq!(a.notes(&l, &t).unwrap().text, "ab");
}

#[test]
fn attachments_travel_in_list_files_and_backups() {
    let a = open("fa");
    let l = a.create_list(None, "Diary").unwrap();
    let t = a.edit(&l, |d| tk::add_task(d, None, "long entry").map_err(Into::into)).unwrap();
    let long = "Dear diary, today was long. ".repeat(6000);
    a.set_notes(&l, &t, &long).unwrap();

    // A .tasklist file carries it: the importer isn't in any group with us.
    let b = open("fb");
    let imported = b.import_list_file(None, &a.export_list_file(&l).unwrap()).unwrap();
    let bt = b.read(&imported, tk::tasks).unwrap().remove(0);
    let n = b.notes(&imported, &bt.id).unwrap();
    assert!(n.complete && n.text == long, "attachment came with the file");

    // A backup carries the attachments of solo lists.
    let root = std::env::temp_dir().join(format!("kt-att-restore-{}-{}", std::process::id(), rand::random::<u32>()));
    let sb = SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-tasks-worker")), timeout: Duration::from_secs(20) };
    let (c, _) = App::restore(&root.join("identity"), &root.join("data"), sb, &a.backup(30).unwrap()).unwrap();
    let cl = c.lists().into_iter().find(|x| x.name == "Diary").unwrap();
    let ct = c.read(&cl, tk::tasks).unwrap().remove(0);
    assert!(c.notes(&cl, &ct.id).unwrap().complete, "attachment restored with the backup");
}
