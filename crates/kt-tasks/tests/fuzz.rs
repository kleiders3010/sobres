//! Fuzzing the app layer's file formats (DESIGN §9): `.tasklist` files and backups as a
//! whole app restores them (the engine parts are fuzzed in kt-engine). Both go through the
//! import sandbox, so fewer iterations.

use std::path::PathBuf;
use std::time::Duration;

use kt_engine::replica::backup;
use kt_engine::sandbox::SandboxConfig;
use kt_tasks as tk;
use kt_tasks::app::App;

fn sandbox() -> SandboxConfig {
    SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-tasks-worker")), timeout: Duration::from_secs(20) }
}

fn root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kt-fuzzapp-{tag}-{}-{}", std::process::id(), rand::random::<u32>()))
}

fn open(tag: &str) -> App {
    let r = root(tag);
    App::open(&r.join("identity"), &r.join("data"), sandbox()).unwrap()
}

#[test]
fn tasklist_files() {
    let a = open("tl");
    let l = a.create_list(None, "Trip").unwrap();
    let t = a.edit(&l, |d| tk::add_task(d, None, "passport").map_err(Into::into)).unwrap();
    a.edit(&l, |d| tk::add_task(d, Some(&t), "check expiry").map_err(Into::into)).unwrap();
    let file = a.export_list_file(&l).unwrap();
    let body = backup::open_file(b"KTL1", b"kt/tasklist/v1", &file).unwrap();
    let b = open("tlb");
    let mut imported = 0;
    kt_fuzz::run("tasklist-body", &[body], 0.1, |p| {
        if b.import_list_file(None, &backup::seal_file(b"KTL1", b"kt/tasklist/v1", p)).is_ok() {
            imported += 1;
        }
    });
    assert!(imported > 0, "some mutated files still import");
    kt_fuzz::run("tasklist-file", &[file], 0.5, |f| {
        let _ = b.import_list_file(None, f);
    });
}

#[test]
fn backups_restored_by_the_app() {
    let a = open("bk");
    a.set_display_name("Alex").unwrap();
    let l = a.create_list(None, "Diary").unwrap();
    a.edit(&l, |d| tk::add_task(d, None, "note").map_err(Into::into)).unwrap();
    let body = backup::open_file(b"KTB1", b"kt/backup/v1", &a.backup(30).unwrap()).unwrap();
    kt_fuzz::run("app-restore", &[body], 0.02, |p| {
        let r = root("rs");
        let _ = App::restore(&r.join("identity"), &r.join("data"), sandbox(), &backup::seal_file(b"KTB1", b"kt/backup/v1", p));
    });
}

/// File names made from list titles (D60): whatever the title, the name stays one plain
/// file inside the chosen folder, on Windows, Linux and Android alike.
#[test]
fn export_file_names() {
    // Found by this target: a device name near the length cap ("_" pushed it over).
    let long_device = format!("con.{}", "x".repeat(80));
    let seeds: Vec<Vec<u8>> = [long_device.as_str(), "Groceries", "../../x", "..\\..\\x", "C:\\Windows", "/etc/passwd", "con.txt", "a\u{0}b", ".  .", "\u{202e}exe.md", "Ünïcödé 日本 🛒"]
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect();
    let dir = std::path::Path::new("exports");
    kt_fuzz::run("file-stem", &seeds, 1.0, |b| {
        let title = String::from_utf8_lossy(b);
        let stem = tk::export::file_stem(&title);
        let name = format!("{stem}.tasklist");
        assert!(!stem.is_empty() && stem.chars().count() <= 60, "{title:?} -> {stem:?}");
        assert!(!stem.starts_with('.') && !stem.ends_with('.') && !stem.ends_with(' '), "{title:?} -> {stem:?}");
        assert!(stem.chars().all(|c| c.is_alphanumeric() || " -_.()".contains(c)), "{title:?} -> {stem:?}");
        assert!(!tk::export::is_device_name(&stem), "{title:?} -> {stem:?}");
        let p = dir.join(&name);
        assert_eq!(p.parent(), Some(dir), "{title:?} stays in the folder");
        assert_eq!(p.components().count(), 2, "{title:?} is one plain name");
        assert_eq!(p.file_name().and_then(|n| n.to_str()), Some(name.as_str()));
    });
}
