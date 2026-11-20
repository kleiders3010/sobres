//! Golden corpus (principle 8, §12a): a recorded session per format generation, kept in
//! `corpus/vN/` forever. Every build must open all of them: the devices' own files
//! (identity, app data, documents), everything they sent each other (slot blobs: epoch
//! records, envelopes, statuses, whereabouts, invites, health...), a backup file and a
//! `.tasklist` file. `scripts/interop.sh <old-tag>` runs an older build's reader over the
//! newest corpus the same way, for the other direction.
//!
//! Writing a new generation (only when formats change): `KT_CORPUS_WRITE=vN cargo test -p
//! kt-tasks --test corpus -- --ignored write_corpus`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kt_engine::invite::CodeKind;
use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_tasks as tk;
use kt_tasks::app::App;

fn sandbox() -> SandboxConfig {
    SandboxConfig { worker: PathBuf::from(env!("CARGO_BIN_EXE_kt-tasks-worker")), timeout: Duration::from_secs(20) }
}

fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        let dst = to.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &dst);
        } else if !e.file_name().to_string_lossy().ends_with(".lock") {
            std::fs::copy(&p, &dst).unwrap();
        }
    }
}

/// What each device's lists hold: list name -> sorted task titles.
fn contents(app: &App) -> BTreeMap<String, Vec<String>> {
    app.lists()
        .into_iter()
        .map(|l| {
            let mut t: Vec<String> = app.read(&l, tk::tasks).unwrap().into_iter().map(|t| t.title).collect();
            t.sort();
            (l.name, t)
        })
        .collect()
}

type Slots = HashMap<[u8; 32], HashMap<[u8; 32], Vec<u8>>>;

fn exchange(slots: &mut Slots, apps: &[&App]) {
    for _ in 0..3 {
        for a in apps {
            a.with(|r: &mut Replica| {
                for t in r.slot_targets() {
                    let blob = r.slot_blob(&t).unwrap();
                    slots.entry(t.topic).or_default().insert(t.writer_seed, blob);
                }
            });
        }
        for a in apps {
            a.with(|r: &mut Replica| {
                for t in r.read_topics() {
                    if let Some(ws) = slots.get(&t.topic) {
                        let blobs: Vec<&[u8]> = ws.values().map(|b| b.as_slice()).collect();
                        r.open_slots(&t, &blobs).unwrap();
                    }
                }
            });
        }
    }
}

#[test]
#[ignore]
fn write_corpus() {
    let Ok(version) = std::env::var("KT_CORPUS_WRITE") else { return };
    let out = corpus_root().join(&version);
    assert!(!out.exists(), "{} exists: corpus generations are never rewritten", out.display());
    let tmp = std::env::temp_dir().join(format!("kt-corpus-w-{}", std::process::id()));
    let (ra, rb) = (tmp.join("a"), tmp.join("b"));
    let mut slots = Slots::new();
    let expected;
    {
        let a = App::open(&ra.join("identity"), &ra.join("data"), sandbox()).unwrap();
        let b = App::open(&rb.join("identity"), &rb.join("data"), sandbox()).unwrap();
        a.set_display_name("Alex").unwrap();
        b.set_display_name("Sam").unwrap();
        let solo = a.create_list(None, "Diary").unwrap();
        let entry = a.edit(&solo, |d| tk::add_task(d, None, "private note").map_err(Into::into)).unwrap();
        // v2+: text attachments (in the backup for solo lists, in .tasklist files).
        a.set_notes(&solo, &entry, &"Long diary entry. ".repeat(8000)).unwrap();
        let fam = a.create_group("Family").unwrap();
        let groceries = a.create_list(Some(fam.group), "Groceries").unwrap();
        let milk = a.edit(&groceries, |d| tk::add_task(d, None, "milk").map_err(Into::into)).unwrap();
        let oat = a.edit(&groceries, |d| tk::add_task(d, Some(&milk), "oat").map_err(Into::into)).unwrap();
        a.set_notes(&groceries, &oat, &"Which brand, which size, and why. ".repeat(4000)).unwrap();
        a.edit(&groceries, |d| tk::set_done(d, &milk, true).map_err(Into::into)).unwrap();
        a.edit(&groceries, |d| tk::set_reminder(d, &milk, Some(1_900_000_000_000)).map_err(Into::into)).unwrap();
        let style = tk::appearance::ListStyle { hue: Some(130.0), vivid: 2, icon: Some("cart".into()) };
        a.edit(&groceries, |d| tk::appearance::set_list_style(d, &style)).unwrap();
        let bundle = a.share_group_with_device(fam.group, b.with(|r| r.device_public())).unwrap();
        b.join_from_bundle(fam.group, &bundle).unwrap();
        a.with(|r| r.create_invite(&fam.group, CodeKind::Invite, 30 * 86_400_000)).unwrap();
        for x in [&a, &b] {
            x.with(|r| {
                r.set_whereabouts(Some(b"transport-defined whereabouts".to_vec()));
                r.set_health_report(Some(b"transport-defined health report".to_vec()));
            });
            x.publish_names().unwrap();
        }
        exchange(&mut slots, &[&a, &b]);
        let l = b.find_list("Groceries").unwrap();
        b.edit(&l, |d| tk::add_task(d, None, "eggs").map_err(Into::into)).unwrap();
        exchange(&mut slots, &[&a, &b]);
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("backup.ktbackup"), a.backup(30).unwrap()).unwrap();
        std::fs::write(out.join("groceries.tasklist"), a.export_list_file(&groceries).unwrap()).unwrap();
        expected = serde_json::json!({ "a": contents(&a), "b": contents(&b) });
    }
    copy_dir(&ra, &out.join("a"));
    copy_dir(&rb, &out.join("b"));
    for (topic, ws) in &slots {
        for (writer, blob) in ws {
            let d = out.join("slots").join(hex::encode(topic));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(hex::encode(writer)), blob).unwrap();
        }
    }
    std::fs::write(out.join("expected.json"), serde_json::to_vec_pretty(&expected).unwrap()).unwrap();
}

/// Every corpus generation opens with this build: files, messages, backups, list files.
#[test]
fn reads_every_corpus_generation() {
    let dir = std::env::var("KT_CORPUS_DIR").map(PathBuf::from).unwrap_or_else(|_| corpus_root());
    let mut gens: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    gens.sort();
    assert!(!gens.is_empty(), "no corpus in {}", dir.display());
    for g in gens {
        let expected: serde_json::Value = serde_json::from_slice(&std::fs::read(g.join("expected.json")).unwrap()).unwrap();
        let work = std::env::temp_dir().join(format!("kt-corpus-r-{}-{}", std::process::id(), rand::random::<u32>()));
        let mut apps = Vec::new();
        for who in ["a", "b"] {
            copy_dir(&g.join(who), &work.join(who));
            let app = App::open(&work.join(who).join("identity"), &work.join(who).join("data"), sandbox()).unwrap_or_else(|e| panic!("{}: {who} doesn't open: {e}", g.display()));
            let got = serde_json::to_value(contents(&app)).unwrap();
            assert_eq!(got, expected[who], "{}: {who}'s lists", g.display());
            apps.push(app);
        }
        // Everything they sent each other reads without a failure.
        let mut slots: Slots = HashMap::new();
        for t in std::fs::read_dir(g.join("slots")).unwrap().flatten() {
            let topic: [u8; 32] = hex::decode(t.file_name().to_string_lossy().as_ref()).unwrap().try_into().unwrap();
            for w in std::fs::read_dir(t.path()).unwrap().flatten() {
                let writer: [u8; 32] = hex::decode(w.file_name().to_string_lossy().as_ref()).unwrap().try_into().unwrap();
                slots.entry(topic).or_default().insert(writer, std::fs::read(w.path()).unwrap());
            }
        }
        for app in &apps {
            app.with(|r: &mut Replica| {
                for t in r.read_topics() {
                    if let Some(ws) = slots.get(&t.topic) {
                        let blobs: Vec<&[u8]> = ws.values().map(|b| b.as_slice()).collect();
                        let rep = r.open_slots(&t, &blobs).unwrap();
                        assert_eq!((rep.bad_slots, rep.failed), (0, 0), "{}: recorded slots read cleanly: {rep:?}", g.display());
                    }
                }
            });
        }
        // The backup restores and the list file imports.
        let fresh = work.join("restored");
        let (restored, _) = App::restore(&fresh.join("identity"), &fresh.join("data"), sandbox(), &std::fs::read(g.join("backup.ktbackup")).unwrap()).unwrap();
        assert!(restored.lists().iter().any(|l| l.name == "Groceries"), "{}: backup", g.display());
        let imported = apps[1].import_list_file(None, &std::fs::read(g.join("groceries.tasklist")).unwrap()).unwrap();
        assert_eq!(imported.name, "Groceries");
        let styled = apps[1].lists().into_iter().find(|l| l.name == "Groceries" && l.doc != imported.doc).unwrap();
        let st = apps[1].read(&styled, tk::appearance::list_style).unwrap();
        assert!(st.hue.is_some() && st.icon.is_some(), "{}: list style reads", g.display());
        // Attachments (generations that have them): whole on the author's device, in the
        // restored backup (solo list) and in the imported list file.
        let full = |app: &App, list: &tk::app::ListInfo| {
            fn all(ts: Vec<tk::TaskView>) -> Vec<tk::TaskView> {
                ts.into_iter().flat_map(|t| std::iter::once(t.clone()).chain(all(t.children))).collect()
            }
            let ts = all(app.read(list, tk::tasks).unwrap());
            ts.iter().filter(|t| t.attachment().is_some()).map(|t| app.notes(list, &t.id).unwrap().complete).collect::<Vec<bool>>()
        };
        for l in apps[0].lists() {
            assert!(full(&apps[0], &l).iter().all(|c| *c), "{}: author's attachments", g.display());
        }
        for l in restored.lists().into_iter().filter(|l| l.name == "Diary") {
            assert!(full(&restored, &l).iter().all(|c| *c), "{}: attachment in the backup", g.display());
        }
        assert!(full(&apps[1], &imported).iter().all(|c| *c), "{}: attachment in the list file", g.display());
    }
}
