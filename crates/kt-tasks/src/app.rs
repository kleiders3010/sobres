//! App layer: a person's groups and lists on one device.
//!
//! Model (DESIGN §4/§5):
//! - A **group** is a circle of people (e.g. "Family"). Each group has one **group document**
//!   (`kind = group`: name + list index) and **any number of list documents**.
//! - The **personal group** contains only this person's devices. It holds the `profile`
//!   document (display name), the `index` document (the person's groups, so linked devices
//!   know what to join), and the person's **solo lists**.
//! - Moving a list into another group copies it there (lists in different groups are
//!   encrypted with different keys) and removes it from the old group.
//!
//! Every document carries `meta.kind` so the app can tell lists from group/profile/index docs.
//!
//! The replica is shared (`Arc<Mutex<_>>`) with the transports. Every public method locks
//! it exactly once and passes it to internal helpers, so nothing locks twice.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use kt_engine::crypto::DevicePublic;
use kt_engine::replica::Replica;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{storage, wire, DeviceId, DocId, GroupId};
use loro::{LoroDoc, LoroValue, ValueOrContainer};
use serde::{Deserialize, Serialize};

use crate::{Error, Result, TaskSemantics};

mod attachments;
pub use attachments::{NotesText, MAX_IMAGE_BYTES};
mod linking;
mod notify;
mod undo;
pub use undo::Person;
pub use notify::{Change, ChangeKind, STALE_MS};
pub use linking::{AccountChanges, DeviceInfo, RecentJoin, RecentLink, LINK_CODE_MS, RECENT_LINK_MS};

#[derive(Serialize, Deserialize, Clone, Copy)]
struct AppMeta {
    personal: GroupId,
    profile: DocId,
    index: DocId,
}

pub struct App {
    replica: Arc<Mutex<Replica>>,
    /// Which account this device is in (changes once, when it links into another).
    meta: RwLock<AppMeta>,
    data_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupInfo {
    pub group: GroupId,
    pub name: String,
    pub personal: bool,
}

/// What a restore did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreSummary {
    /// Shared groups: readable now; back in as soon as a member's device sees the request.
    pub rejoining: Vec<GroupId>,
    /// Lists from groups nobody else can let us back into, copied into our personal group.
    pub rehomed_lists: usize,
    /// The device the backup came from (still a member of its groups until removed).
    pub old_device: Option<DeviceId>,
}

/// A list as the UI sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListInfo {
    pub group: GroupId,
    pub doc: DocId,
    pub name: String,
    pub group_name: String,
}

fn eng<E: std::fmt::Display>(e: E) -> Error {
    Error::Invalid(e.to_string())
}

fn str_of(v: Option<ValueOrContainer>) -> Option<String> {
    match v? {
        ValueOrContainer::Value(LoroValue::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn doc_kind(doc: &LoroDoc) -> Option<String> {
    str_of(doc.get_map("meta").get("kind"))
}

fn create_group_doc(r: &mut Replica, g: GroupId, name: &str) -> Result<DocId> {
    let d = r.create_doc(&g).map_err(eng)?;
    {
        let doc = r.doc(&g, &d).unwrap();
        doc.get_map("meta").insert("kind", "group").map_err(eng)?;
        doc.get_map("meta").insert("name", name).map_err(eng)?;
        doc.get_map("lists").insert("_", true).map_err(eng)?;
    }
    r.commit(&g, &d).map_err(eng)?;
    Ok(d)
}

fn group_doc(r: &Replica, g: &GroupId) -> Option<DocId> {
    r.docs(g).into_iter().filter(|d| r.doc(g, d).and_then(doc_kind).as_deref() == Some("group")).min()
}

fn groups_of(r: &Replica, personal: GroupId) -> Vec<GroupInfo> {
    let mut out: Vec<GroupInfo> = r
        .groups()
        .into_iter()
        .filter_map(|g| {
            let gd = group_doc(r, &g)?;
            let name = crate::clean_name(&r.doc(&g, &gd).and_then(|d| str_of(d.get_map("meta").get("name"))).unwrap_or_default());
            Some(GroupInfo { group: g, name, personal: g == personal })
        })
        .collect();
    out.sort_by(|a, b| b.personal.cmp(&a.personal).then(a.name.cmp(&b.name)));
    out
}

fn lists_of(r: &Replica, personal: GroupId) -> Vec<ListInfo> {
    let mut out = Vec::new();
    for gi in groups_of(r, personal) {
        let Some(gd) = group_doc(r, &gi.group) else { continue };
        let Some(gdoc) = r.doc(&gi.group, &gd) else { continue };
        let listed: Vec<DocId> = match gdoc.get_map("lists").get_deep_value() {
            LoroValue::Map(m) => m.iter().filter(|(_, v)| matches!(v, LoroValue::Bool(true))).filter_map(|(k, _)| DocId::from_hex(k)).collect(),
            _ => Vec::new(),
        };
        for d in listed {
            if let Some(doc) = r.doc(&gi.group, &d) {
                out.push(ListInfo { group: gi.group, doc: d, name: crate::list_name(doc), group_name: gi.name.clone() });
            }
        }
    }
    out.sort_by(|a, b| a.group_name.cmp(&b.group_name).then(a.name.cmp(&b.name)));
    out
}

/// A fresh personal group (a person without an account, §5), recorded as this device's.
fn new_account(r: &mut Replica, data_dir: &Path) -> Result<AppMeta> {
    let personal = r.create_group().map_err(eng)?;
    let profile = r.create_doc(&personal).map_err(eng)?;
    let index = r.create_doc(&personal).map_err(eng)?;
    {
        let p = r.doc(&personal, &profile).unwrap();
        p.get_map("meta").insert("kind", "profile").map_err(eng)?;
        p.get_map("profile").insert("name", "").map_err(eng)?;
        let i = r.doc(&personal, &index).unwrap();
        i.get_map("meta").insert("kind", "index").map_err(eng)?;
        i.get_map("groups").insert(&personal.to_hex(), true).map_err(eng)?;
    }
    r.commit(&personal, &profile).map_err(eng)?;
    r.commit(&personal, &index).map_err(eng)?;
    create_group_doc(r, personal, "Personal")?;
    let meta = AppMeta { personal, profile, index };
    storage::write_atomic(&data_dir.join("app.meta"), &wire::encode(*b"AM", 1, &meta).map_err(eng)?).map_err(eng)?;
    Ok(meta)
}

fn register_group(r: &mut Replica, meta: &AppMeta, g: GroupId) -> Result<()> {
    r.doc(&meta.personal, &meta.index).ok_or(Error::NotFound)?.get_map("groups").insert(&g.to_hex(), true).map_err(eng)?;
    r.commit(&meta.personal, &meta.index).map_err(eng)
}

impl App {
    /// Open the app. `identity_dir` must be excluded from backups (D1).
    pub fn open(identity_dir: &Path, data_dir: &Path, sandbox: SandboxConfig) -> Result<App> {
        let mut replica = Replica::open(identity_dir, data_dir, sandbox).map_err(eng)?;
        replica.set_semantics(Box::new(TaskSemantics));
        App::with_replica(replica, data_dir)
    }

    fn with_replica(mut replica: Replica, data_dir: &Path) -> Result<App> {
        let meta_path: PathBuf = data_dir.join("app.meta");
        let meta = match std::fs::read(&meta_path) {
            Ok(b) => wire::decode::<AppMeta>(*b"AM", 1, &b).map_err(eng)?.1,
            Err(_) => new_account(&mut replica, data_dir)?,
        };
        Ok(App { replica: Arc::new(Mutex::new(replica)), meta: RwLock::new(meta), data_dir: data_dir.to_path_buf() })
    }

    fn meta(&self) -> AppMeta {
        *self.meta.read().expect("meta lock")
    }

    /// The shared replica (for transports).
    pub fn replica(&self) -> Arc<Mutex<Replica>> {
        self.replica.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Replica> {
        self.replica.lock().expect("replica lock")
    }

    /// Run something against the replica (one lock).
    pub fn with<T>(&self, f: impl FnOnce(&mut Replica) -> T) -> T {
        f(&mut self.lock())
    }

    pub fn personal_group(&self) -> GroupId {
        self.meta().personal
    }

    pub fn display_name(&self) -> String {
        let r = self.lock();
        r.doc(&self.meta().personal, &self.meta().profile).and_then(|d| str_of(d.get_map("profile").get("name"))).unwrap_or_default()
    }

    pub fn set_display_name(&self, name: &str) -> Result<()> {
        let mut r = self.lock();
        let (g, d) = (self.meta().personal, self.meta().profile);
        r.doc(&g, &d).ok_or(Error::NotFound)?.get_map("profile").insert("name", crate::clean_name(name).as_str()).map_err(eng)?;
        r.commit(&g, &d).map_err(eng)
    }

    /// Create a shared group (a circle of people) with its group document.
    pub fn create_group(&self, name: &str) -> Result<GroupInfo> {
        let mut r = self.lock();
        let g = r.create_group().map_err(eng)?;
        create_group_doc(&mut r, g, name)?;
        register_group(&mut r, &self.meta(), g)?;
        Ok(GroupInfo { group: g, name: name.to_string(), personal: false })
    }

    pub fn groups(&self) -> Vec<GroupInfo> {
        groups_of(&self.lock(), self.meta().personal)
    }

    /// Create a list in `group`, or a solo list in the personal group.
    pub fn create_list(&self, group: Option<GroupId>, name: &str) -> Result<ListInfo> {
        let mut r = self.lock();
        let g = group.unwrap_or(self.meta().personal);
        let gd = group_doc(&r, &g).ok_or(Error::NotFound)?;
        let d = r.create_doc(&g).map_err(eng)?;
        {
            let doc = r.doc(&g, &d).unwrap();
            crate::init_list(doc, name)?;
            doc.get_map("meta").insert("kind", "list").map_err(eng)?;
        }
        r.commit(&g, &d).map_err(eng)?;
        r.doc(&g, &gd).unwrap().get_map("lists").insert(&d.to_hex(), true).map_err(eng)?;
        r.commit(&g, &gd).map_err(eng)?;
        let group_name = groups_of(&r, self.meta().personal).into_iter().find(|x| x.group == g).map(|x| x.name).unwrap_or_default();
        Ok(ListInfo { group: g, doc: d, name: name.to_string(), group_name })
    }

    /// Nothing made on this device yet (no lists, no other groups, no pending joins): a
    /// backup can still be restored onto it.
    pub fn is_pristine(&self) -> bool {
        let r = self.lock();
        groups_of(&r, self.meta().personal).len() <= 1 && lists_of(&r, self.meta().personal).is_empty() && r.pending_joins().is_empty()
    }

    /// All lists on this device, across all groups.
    pub fn lists(&self) -> Vec<ListInfo> {
        lists_of(&self.lock(), self.meta().personal)
    }

    /// Find a list by exact name or by a prefix of its document ID.
    pub fn find_list(&self, key: &str) -> Result<ListInfo> {
        let lists = self.lists();
        for pred in [
            Box::new(|l: &ListInfo| l.name == key) as Box<dyn Fn(&ListInfo) -> bool>,
            Box::new(|l: &ListInfo| l.doc.to_hex().starts_with(key)),
        ] {
            let m: Vec<&ListInfo> = lists.iter().filter(|l| pred(l)).collect();
            if m.len() == 1 {
                return Ok(m[0].clone());
            }
        }
        Err(Error::Invalid(format!("no single list matches '{key}'")))
    }

    /// Find a group by exact name or by a prefix of its ID.
    pub fn find_group(&self, key: &str) -> Result<GroupInfo> {
        let gs = self.groups();
        let m: Vec<&GroupInfo> = gs.iter().filter(|g| g.name == key || g.group.to_hex().starts_with(key)).collect();
        if m.len() == 1 {
            Ok(m[0].clone())
        } else {
            Err(Error::Invalid(format!("no single group matches '{key}'")))
        }
    }

    /// Read a list's document.
    pub fn read<T>(&self, list: &ListInfo, f: impl FnOnce(&LoroDoc) -> T) -> Result<T> {
        let r = self.lock();
        Ok(f(r.doc(&list.group, &list.doc).ok_or(Error::NotFound)?))
    }

    /// Read a list's document with how its writers can be told (rebuilds re-write everything under one device).
    pub fn read_authored<T>(&self, list: &ListInfo, f: impl FnOnce(&LoroDoc, &kt_engine::replica::Authors) -> T) -> Result<T> {
        let r = self.lock();
        let doc = r.doc(&list.group, &list.doc).ok_or(Error::NotFound)?;
        Ok(f(doc, r.doc_authors(&list.group, &list.doc).ok_or(Error::NotFound)?))
    }

    /// Run an edit on a list's document and commit it.
    pub fn edit<T>(&self, list: &ListInfo, f: impl FnOnce(&LoroDoc) -> Result<T>) -> Result<T> {
        let mut r = self.lock();
        let out = f(r.doc(&list.group, &list.doc).ok_or(Error::NotFound)?)?;
        r.commit(&list.group, &list.doc).map_err(eng)?;
        Ok(out)
    }

    /// Move a list into another group: copy its live state (stable task IDs kept) and
    /// remove it from the old group's index.
    pub fn move_list(&self, list: &ListInfo, to: GroupId) -> Result<ListInfo> {
        if list.group == to {
            return Ok(list.clone());
        }
        let mut r = self.lock();
        let peer = r.identity().peer_id();
        let copy = crate::rebuild(r.doc(&list.group, &list.doc).ok_or(Error::NotFound)?, peer)?;
        let snapshot = copy.export(loro::ExportMode::Snapshot).map_err(eng)?;
        let gd_to = group_doc(&r, &to).ok_or(Error::NotFound)?;
        let d = r.create_doc(&to).map_err(eng)?;
        r.doc(&to, &d).unwrap().import(&snapshot).map_err(eng)?;
        r.commit(&to, &d).map_err(eng)?;
        r.doc(&to, &gd_to).unwrap().get_map("lists").insert(&d.to_hex(), true).map_err(eng)?;
        r.commit(&to, &gd_to).map_err(eng)?;
        let gd_from = group_doc(&r, &list.group).ok_or(Error::NotFound)?;
        r.doc(&list.group, &gd_from).unwrap().get_map("lists").insert(&list.doc.to_hex(), false).map_err(eng)?;
        r.commit(&list.group, &gd_from).map_err(eng)?;
        let group_name = groups_of(&r, self.meta().personal).into_iter().find(|x| x.group == to).map(|x| x.name).unwrap_or_default();
        Ok(ListInfo { group: to, doc: d, name: list.name.clone(), group_name })
    }

    /// Rename a list (shared with everyone in its group like any edit).
    pub fn rename_list(&self, list: &ListInfo, name: &str) -> Result<()> {
        self.edit(list, |d| crate::rename_list(d, name))
    }

    /// Take a list off its group's index (or put it back, for Undo). The document stays,
    /// so putting it back restores everything; every member's app stops showing it.
    pub fn set_list_listed(&self, group: GroupId, doc: DocId, listed: bool) -> Result<()> {
        let mut r = self.lock();
        let gd = group_doc(&r, &group).ok_or(Error::NotFound)?;
        r.doc(&group, &gd).unwrap().get_map("lists").insert(&doc.to_hex(), listed).map_err(eng)?;
        r.commit(&group, &gd).map_err(eng)?;
        Ok(())
    }

    /// A sealed backup of everything on this device (DESIGN §11). Includes a recovery
    /// invite per shared group, valid `recovery_days`.
    pub fn backup(&self, recovery_days: u32) -> Result<Vec<u8>> {
        // The app part: our meta, plus the attachments of lists only our own devices have
        // (shared lists' attachments come back from the group).
        let meta = wire::encode(*b"AM", 1, &self.meta()).map_err(eng)?;
        let personal = self.meta().personal;
        let lists: Vec<ListInfo> = self.lists().into_iter().filter(|l| l.group == personal).collect();
        let app = wire::encode(*b"AB", 1, &(meta, self.blobs_for(&lists, attachments::BACKUP_BLOB_BYTES))).map_err(eng)?;
        let b = self.lock().make_backup(u64::from(recovery_days.clamp(1, 30)) * 86_400_000, app).map_err(eng)?;
        let lock = self.lock().backup_lock();
        kt_engine::replica::backup::seal_locked(&b, lock.as_ref()).map_err(eng)
    }

    /// Set (`Some`) or remove (`None`) the backup password (audit T11). Stretched once
    /// here (~0.5 s); later backups are locked with it without asking again.
    pub fn set_backup_password(&self, password: Option<&str>) -> Result<()> {
        let lock = match password {
            Some(p) if p.chars().count() < kt_engine::replica::backup::PW_MIN_CHARS => return Err(Error::Invalid("password too short".into())),
            Some(p) => Some(kt_engine::replica::backup::BackupLock::new(p).map_err(eng)?),
            None => None,
        };
        self.lock().set_backup_lock(lock.as_ref()).map_err(eng)
    }

    /// Backups from this device need a password to open.
    pub fn has_backup_password(&self) -> bool {
        self.lock().backup_lock().is_some()
    }

    /// Restore a backup onto a fresh device (new identity, D1). Shared groups: readable at
    /// once and back in when a member's device sees the recovery request. Groups nobody
    /// else can let us into (solo lists): their lists are copied into our new personal group.
    pub fn restore(identity_dir: &Path, data_dir: &Path, sandbox: SandboxConfig, backup: &[u8]) -> Result<(App, RestoreSummary)> {
        Self::restore_with(identity_dir, data_dir, sandbox, backup, None)
    }

    /// [`App::restore`] with the backup's password (a locked backup). The new device keeps
    /// the password for its own backups.
    pub fn restore_with(identity_dir: &Path, data_dir: &Path, sandbox: SandboxConfig, backup: &[u8], password: Option<&str>) -> Result<(App, RestoreSummary)> {
        let b = kt_engine::replica::backup::open_with(backup, password).map_err(eng)?;
        let locked = kt_engine::replica::backup::needs_password(backup);
        // Backups made before attachments hold the meta alone.
        let (meta, blobs): (Vec<u8>, Vec<Vec<u8>>) = if b.app.starts_with(b"AB") { wire::decode(*b"AB", 1, &b.app).map_err(eng)?.1 } else { (b.app.clone(), Vec::new()) };
        let old: AppMeta = wire::decode(*b"AM", 1, &meta).map_err(eng)?.1;
        if data_dir.join("app.meta").exists() {
            // Opened once already (first-launch screen): fine while nothing was made yet.
            let pristine = App::open(identity_dir, data_dir, sandbox.clone())?.is_pristine();
            if !pristine {
                return Err(Error::Invalid("this device already has lists; restore onto a fresh install".into()));
            }
            std::fs::remove_dir_all(data_dir).map_err(eng)?;
            std::fs::create_dir_all(data_dir).map_err(eng)?;
        }
        let mut replica = Replica::open(identity_dir, data_dir, sandbox).map_err(eng)?;
        replica.set_semantics(Box::new(TaskSemantics));
        let results = replica.restore_backup(&b).map_err(eng)?;
        // Other devices of ours still have the personal group (device linking): it comes
        // back like any shared group, and it stays our personal group.
        let adopt = results.contains(&kt_engine::replica::backup::Restored::Rejoining(old.personal));
        if adopt {
            storage::write_atomic(&data_dir.join("app.meta"), &wire::encode(*b"AM", 1, &old).map_err(eng)?).map_err(eng)?;
        }
        let app = App::with_replica(replica, data_dir)?;
        if let (true, Some(p)) = (locked, password) {
            app.set_backup_password(Some(p))?;
        }
        for blob in &blobs {
            let _ = app.blob_store().put(blob); // stored under their own hash: nothing to trust
        }
        let mut summary = RestoreSummary { old_device: Some(b.old_device), ..Default::default() };
        // Remembered so the app can offer to remove it once we're back in (lost or stolen
        // vs. just reset is the user's call, asked once).
        storage::write_atomic(&data_dir.join("replaced_device"), &b.old_device.0).map_err(eng)?;
        {
            let mut r = app.lock();
            // The display name comes along (already there when the old personal group is ours).
            let name = if adopt { None } else { r.doc(&old.personal, &old.profile).and_then(|d| str_of(d.get_map("profile").get("name"))) };
            if let Some(name) = name {
                let (g, d) = (app.meta().personal, app.meta().profile);
                r.doc(&g, &d).ok_or(Error::NotFound)?.get_map("profile").insert("name", name.as_str()).map_err(eng)?;
                r.commit(&g, &d).map_err(eng)?;
            }
            for res in results {
                match res {
                    kt_engine::replica::backup::Restored::Rejoining(g) => summary.rejoining.push(g),
                    kt_engine::replica::backup::Restored::ReadOnly(g) => {
                        // Copy the lists into our personal group (nobody can let us back in).
                        let personal = app.meta().personal;
                        let lists: Vec<DocId> = r.docs(&g).into_iter().filter(|d| r.doc(&g, d).and_then(doc_kind).as_deref() == Some("list")).collect();
                        for d in lists {
                            linking::copy_list(&mut r, &g, &d, &personal)?;
                            summary.rehomed_lists += 1;
                        }
                        // Copied: this device doesn't keep the old group around.
                        r.forget_group(&g).map_err(eng)?;
                    }
                }
            }
        }
        Ok((app, summary))
    }

    /// The list as Markdown (Obsidian Tasks compatible; one-way).
    pub fn export_markdown(&self, list: &ListInfo) -> Result<String> {
        let ts = self.read(list, crate::tasks)?;
        Ok(crate::export::markdown(&list.name, &ts))
    }

    /// The list as a `.tasklist` file (sealed, with history; re-importable).
    pub fn export_list_file(&self, list: &ListInfo) -> Result<Vec<u8>> {
        let r = self.lock();
        let doc = r.doc(&list.group, &list.doc).ok_or(Error::NotFound)?;
        let snap = doc.export(loro::ExportMode::Snapshot).map_err(eng)?;
        drop(r);
        // Format 2 carries the list's attachments: whoever imports it isn't in our group.
        let blobs = self.blobs_for(std::slice::from_ref(list), kt_engine::limits::ABSURD_SIZE);
        let body = wire::encode(*b"LF", 2, &(list.name.clone(), snap, blobs)).map_err(eng)?;
        Ok(kt_engine::replica::backup::seal_file(b"KTL1", b"kt/tasklist/v1", &body))
    }

    /// Import a `.tasklist` file as a new list in `group` (or the personal group). The
    /// history stays in the file; the new list starts from its current state.
    pub fn import_list_file(&self, group: Option<GroupId>, bytes: &[u8]) -> Result<ListInfo> {
        let body = kt_engine::replica::backup::open_file(b"KTL1", b"kt/tasklist/v1", bytes).map_err(|_| Error::Invalid("not a .tasklist file".into()))?;
        let (name, snap, blobs): (String, Vec<u8>, Vec<Vec<u8>>) = match body.get(2..4) {
            Some([1, 0]) => {
                let (n, s): (String, Vec<u8>) = wire::decode(*b"LF", 1, &body).map_err(eng)?.1;
                (n, s, Vec::new())
            }
            _ => wire::decode(*b"LF", 2, &body).map_err(eng)?.1,
        };
        for b in &blobs {
            let _ = self.blob_store().put(b);
        }
        let g = group.unwrap_or(self.meta().personal);
        let mut r = self.lock();
        let src = r.load_untrusted_snapshot(&snap).map_err(eng)?;
        if doc_kind(&src).as_deref() != Some("list") {
            return Err(Error::Invalid("not a list".into()));
        }
        let copy = crate::rebuild(&src, r.identity().peer_id())?;
        let gd = group_doc(&r, &g).ok_or(Error::NotFound)?;
        let d = r.create_doc(&g).map_err(eng)?;
        r.doc(&g, &d).unwrap().import(&copy.export(loro::ExportMode::Snapshot).map_err(eng)?).map_err(eng)?;
        r.commit(&g, &d).map_err(eng)?;
        r.doc(&g, &gd).unwrap().get_map("lists").insert(&d.to_hex(), true).map_err(eng)?;
        r.commit(&g, &gd).map_err(eng)?;
        let group_name = groups_of(&r, self.meta().personal).into_iter().find(|x| x.group == g).map(|x| x.name).unwrap_or_default();
        Ok(ListInfo { group: g, doc: d, name, group_name })
    }

    /// After a restore: the previous device, and the groups we're back in where it's still
    /// a member (we can only remove it where we're a member ourselves).
    pub fn replaced_device(&self) -> Option<(DeviceId, Vec<GroupId>)> {
        let b = std::fs::read(self.data_dir.join("replaced_device")).ok()?;
        let old = DeviceId(b.as_slice().try_into().ok()?);
        let r = self.lock();
        let me = r.device_id();
        let groups: Vec<GroupId> = r
            .groups()
            .into_iter()
            .filter(|g| r.epochs(g).is_some_and(|e| e.is_current_member(&me) && e.is_current_member(&old)))
            .collect();
        Some((old, groups))
    }

    /// Remove the previous device from every group it's still in with us (keys rotate).
    /// Returns how many groups it left; the offer ends when none are left.
    pub fn remove_replaced_device(&self) -> Result<usize> {
        let Some((old, groups)) = self.replaced_device() else { return Ok(0) };
        let mut r = self.lock();
        for g in &groups {
            r.remove_member(g, old).map_err(eng)?;
        }
        Ok(groups.len())
    }

    /// Keep the previous device (it was reset, not lost): stop offering.
    pub fn keep_replaced_device(&self) {
        let _ = std::fs::remove_file(self.data_dir.join("replaced_device"));
    }

    /// Manual sharing (file bundles): add another device to a group and return the
    /// messages it needs to join. Invites (`create_invite`) are the normal way.
    pub fn share_group_with_device(&self, group: GroupId, device: DevicePublic) -> Result<Vec<Vec<u8>>> {
        if group == self.meta().personal {
            return Err(Error::Invalid("the personal group is only for your own devices (device linking)".into()));
        }
        let mut r = self.lock();
        r.add_member(&group, device).map_err(eng)?;
        r.export_group(&group).map_err(eng)
    }

    /// Accept a join bundle produced by [`App::share_group_with_device`].
    pub fn join_from_bundle(&self, group: GroupId, messages: &[Vec<u8>]) -> Result<()> {
        let mut r = self.lock();
        r.accept_group(group);
        for m in messages {
            let _ = r.receive(m);
        }
        register_group(&mut r, &self.meta(), group)
    }
}
