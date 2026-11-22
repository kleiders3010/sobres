//! Device linking, app side (DESIGN §5, D6): linking = joining the personal group.
//!
//! - The existing device makes a **link code** (a few hours, reusable). Its record is
//!   advertised with a summary of the account (devices and lists), which the linking
//!   device shows on its single confirmation screen.
//! - Once let in, the linking device **adopts the account**: its own lists are copied into
//!   the account's personal group, its other groups stay, its old personal group goes.
//! - Every device of an account keeps every other one in all its groups
//!   ([`App::settle_account`]); groups reach a new device through handovers in the
//!   personal group's slots. Removals are respected (a removed device is never re-added).
//! - **Undo** removes the device from the account and from every group, which rotates keys.

use kt_engine::invite::{Code, CodeKind, InviteRecord};
use kt_engine::replica::Replica;
use kt_engine::{storage, wire, DeviceId, DocId, GroupId};
use loro::LoroValue;

use super::{doc_kind, eng, group_doc, groups_of, lists_of, new_account, App, AppMeta};
use crate::{Error, Result};

/// Link codes are valid for a few hours (D6).
pub const LINK_CODE_MS: u64 = 4 * 3_600_000;
/// How long "New device linked · Undo" stays up.
pub const RECENT_LINK_MS: u64 = 24 * 3_600_000;

/// A device of this account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub device: DeviceId,
    /// As that device named itself ("Pixel 7"); empty until it has.
    pub name: String,
    pub this_device: bool,
}

/// A device that joined the account recently (for the Undo alert).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentLink {
    pub device: DeviceId,
    pub name: String,
    pub linked_ms: u64,
}

/// What [`App::settle_account`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountChanges {
    /// This device finished linking: it's now in the account it linked to.
    pub adopted: bool,
    /// This device was removed from its account (Undo on another device): it's on its
    /// own again, with copies of the lists it had.
    pub left_account: bool,
    /// Devices added to groups (and handed the group) this round.
    pub handed_over: usize,
}

/// Copy a list document into another group (a new document there, current state only).
pub(super) fn copy_list(r: &mut Replica, from: &GroupId, d: &DocId, to: &GroupId) -> Result<()> {
    let src = r.doc(from, d).ok_or(Error::NotFound)?;
    let copy = crate::rebuild(src, r.identity().peer_id())?;
    let snap = copy.export(loro::ExportMode::Snapshot).map_err(eng)?;
    let gd = group_doc(r, to).ok_or(Error::NotFound)?;
    let nd = r.create_doc(to).map_err(eng)?;
    r.doc(to, &nd).unwrap().import(&snap).map_err(eng)?;
    r.commit(to, &nd).map_err(eng)?;
    r.doc(to, &gd).unwrap().get_map("lists").insert(&nd.to_hex(), true).map_err(eng)?;
    r.commit(to, &gd).map_err(eng)
}

fn device_names(r: &Replica, meta: &AppMeta) -> std::collections::BTreeMap<String, String> {
    let Some(p) = r.doc(&meta.personal, &meta.profile) else { return Default::default() };
    match p.get_map("devices").get_deep_value() {
        LoroValue::Map(m) => m.iter().filter_map(|(k, v)| if let LoroValue::String(s) = v { Some((k.clone(), crate::clean_name(s))) } else { None }).collect(),
        _ => Default::default(),
    }
}

impl App {
    fn linking_path(&self) -> std::path::PathBuf {
        self.data_dir.join("linking")
    }

    /// Name this device for the account's other devices (the platform passes its model or
    /// host name). Written only when it changes.
    pub fn set_device_name(&self, name: &str) -> Result<()> {
        let meta = self.meta();
        let mut r = self.lock();
        let me = r.device_id().to_hex();
        if device_names(&r, &meta).get(&me).map(String::as_str) == Some(name) {
            return Ok(());
        }
        r.doc(&meta.personal, &meta.profile).ok_or(Error::NotFound)?.get_map("devices").insert(&me, name).map_err(eng)?;
        r.commit(&meta.personal, &meta.profile).map_err(eng)
    }

    /// The account's devices (current members of the personal group).
    pub fn devices(&self) -> Vec<DeviceInfo> {
        let meta = self.meta();
        let r = self.lock();
        let names = device_names(&r, &meta);
        let me = r.device_id();
        r.members(&meta.personal)
            .into_iter()
            .map(|d| DeviceInfo { device: d, name: names.get(&d.to_hex()).cloned().unwrap_or_default(), this_device: d == me })
            .collect()
    }

    /// One line naming the account, for the linking device's confirmation screen:
    /// "Pixel 7 · Mark's PC · 12 lists (Work, Groceries, Trip, …)".
    pub fn account_summary(&self) -> String {
        let meta = self.meta();
        let devices: Vec<String> = self.devices().into_iter().map(|d| if d.name.is_empty() { "a device".into() } else { d.name }).collect();
        let lists = lists_of(&self.lock(), meta.personal);
        let mut names: Vec<&str> = lists.iter().map(|l| l.name.as_str()).take(3).collect();
        if lists.len() > 3 {
            names.push("…");
        }
        let lists_part = match lists.len() {
            0 => "no lists yet".to_string(),
            1 => format!("1 list ({})", names.join(", ")),
            n => format!("{n} lists ({})", names.join(", ")),
        };
        format!("{} · {lists_part}", devices.join(" · "))
    }

    /// A link code for adding another device to this account.
    pub fn create_link_code(&self) -> Result<(Code, u64)> {
        let p = self.meta().personal;
        self.with(|r| r.create_invite(&p, CodeKind::Link, LINK_CODE_MS)).map_err(eng)
    }

    /// The linking device, after the user confirmed the account shown from `rec`.
    pub fn begin_link(&self, rec: &InviteRecord) -> Result<()> {
        if rec.group == self.meta().personal {
            return Err(Error::Invalid("this code is for this device's own account".into()));
        }
        self.with(|r| r.begin_link(rec)).map_err(eng)?;
        storage::write_atomic(&self.linking_path(), &wire::encode(*b"LK", 1, &rec.group).map_err(eng)?).map_err(eng)
    }

    /// The account this device is linking into, until it's in.
    pub fn linking(&self) -> Option<GroupId> {
        let b = std::fs::read(self.linking_path()).ok()?;
        wire::decode::<GroupId>(*b"LK", 1, &b).ok().map(|x| x.1)
    }

    /// Every round: finish linking once let in, and keep every device of the account in
    /// every group this device is in.
    pub fn settle_account(&self) -> Result<AccountChanges> {
        let mut changes = AccountChanges::default();
        if let Some(target) = self.linking() {
            if !self.adopt_account(target)? {
                return Ok(changes); // not in yet: don't hand our groups to the old account
            }
            changes.adopted = true;
        }
        if self.removed_from_account()? {
            changes.left_account = true;
        }
        let meta = self.meta();
        let mut r = self.lock();
        let me = r.device_id();
        let account: Vec<DeviceId> = r.members(&meta.personal).into_iter().filter(|d| *d != me).collect();
        if account.is_empty() {
            return Ok(changes);
        }
        for g in r.groups() {
            if g == meta.personal || !r.epochs(&g).is_some_and(|e| e.is_current_member(&me)) {
                continue;
            }
            let members = r.members(&g);
            let removed = r.epochs(&g).map(|e| e.removed_devices()).unwrap_or_default();
            for d in account.iter().filter(|d| !members.contains(d) && !removed.contains(d)) {
                if r.add_and_hand_over(&meta.personal, &g, d).is_ok() {
                    changes.handed_over += 1;
                }
            }
        }
        Ok(changes)
    }

    /// Move into the account `target` once we're a member and its documents arrived.
    fn adopt_account(&self, target: GroupId) -> Result<bool> {
        let old = self.meta();
        let mut r = self.lock();
        let me = r.device_id();
        if !r.epochs(&target).is_some_and(|e| e.is_current_member(&me)) || group_doc(&r, &target).is_none() {
            return Ok(false);
        }
        let find = |kind: &str| r.docs(&target).into_iter().filter(|d| r.doc(&target, d).and_then(doc_kind).as_deref() == Some(kind)).min();
        let (Some(profile), Some(index)) = (find("profile"), find("index")) else { return Ok(false) };
        let new = AppMeta { personal: target, profile, index };
        // Our own lists come along (merged into the account, D6).
        let own: Vec<DocId> = lists_of(&r, old.personal).into_iter().filter(|l| l.group == old.personal).map(|l| l.doc).collect();
        for d in own {
            copy_list(&mut r, &old.personal, &d, &target)?;
        }
        // Our name for this device carries over; the account's display name wins.
        if let Some(name) = device_names(&r, &old).get(&me.to_hex()).cloned() {
            r.doc(&target, &profile).ok_or(Error::NotFound)?.get_map("devices").insert(&me.to_hex(), name.as_str()).map_err(eng)?;
            r.commit(&target, &profile).map_err(eng)?;
        }
        // Our other groups stay, listed in the account's index.
        let others: Vec<GroupId> = groups_of(&r, old.personal).into_iter().map(|g| g.group).filter(|g| *g != old.personal && *g != target).collect();
        for g in others {
            r.doc(&target, &index).ok_or(Error::NotFound)?.get_map("groups").insert(&g.to_hex(), true).map_err(eng)?;
        }
        r.commit(&target, &index).map_err(eng)?;
        storage::write_atomic(&self.data_dir.join("app.meta"), &wire::encode(*b"AM", 1, &new).map_err(eng)?).map_err(eng)?;
        *self.meta.write().expect("meta lock") = new;
        // The old personal group goes (unless other devices still use it).
        if r.members(&old.personal) == [me] {
            r.forget_group(&old.personal).map_err(eng)?;
        }
        drop(r);
        let _ = std::fs::remove_file(self.linking_path());
        Ok(true)
    }

    /// Removed from our account by another of its devices: start an account of our own,
    /// keeping copies of the account's lists this device still has.
    fn removed_from_account(&self) -> Result<bool> {
        let old = self.meta();
        let mut r = self.lock();
        let me = r.device_id();
        if !r.epochs(&old.personal).is_some_and(|e| e.removed_devices().contains(&me)) {
            return Ok(false);
        }
        let name = r.doc(&old.personal, &old.profile).and_then(|d| super::str_of(d.get_map("profile").get("name"))).unwrap_or_default();
        let new = new_account(&mut r, &self.data_dir)?;
        let lists: Vec<DocId> = lists_of(&r, old.personal).into_iter().filter(|l| l.group == old.personal).map(|l| l.doc).collect();
        for d in lists {
            copy_list(&mut r, &old.personal, &d, &new.personal)?;
        }
        r.doc(&new.personal, &new.profile).ok_or(Error::NotFound)?.get_map("profile").insert("name", name.as_str()).map_err(eng)?;
        r.commit(&new.personal, &new.profile).map_err(eng)?;
        // Undo removed it from every group: nothing there it can read anymore. Shared lists are not copied: they
        // are other people's too, and an unlinked device (old or stolen) keeps none of them (round 5: kept so;
        // a wrongful removal is what S1's rules now prevent).
        for g in r.groups() {
            if r.epochs(&g).is_some_and(|e| e.removed_devices().contains(&me)) {
                r.forget_group(&g).map_err(eng)?;
            }
        }
        *self.meta.write().expect("meta lock") = new;
        Ok(true)
    }

    /// Devices that joined this account in the last [`RECENT_LINK_MS`] (every device of
    /// the account shows "New device linked · Undo" for them).
    pub fn recent_links(&self) -> Vec<RecentLink> {
        let meta = self.meta();
        let r = self.lock();
        let now = kt_engine::replica::now_ms();
        let names = device_names(&r, &meta);
        let current = r.members(&meta.personal);
        let me = r.device_id();
        let Some(e) = r.epochs(&meta.personal) else { return Vec::new() };
        e.all_records()
            .filter(|rec| now.saturating_sub(rec.body.created_ms) < RECENT_LINK_MS)
            .filter_map(|rec| match &rec.body.kind {
                kt_engine::epochs::EpochKind::Add { device, .. } => Some((DeviceId(device.sign), rec.body.created_ms)),
                _ => None,
            })
            .filter(|(d, _)| current.contains(d) && *d != me)
            .map(|(d, t)| RecentLink { device: d, name: names.get(&d.to_hex()).cloned().unwrap_or_default(), linked_ms: t })
            .collect()
    }

    /// Undo a link: the device leaves the account and every group (keys rotate).
    pub fn unlink_device(&self, device: DeviceId) -> Result<()> {
        let meta = self.meta();
        let mut r = self.lock();
        if device == r.device_id() {
            return Err(Error::Invalid("use another device to remove this one".into()));
        }
        for g in r.groups() {
            if g != meta.personal && r.members(&g).contains(&device) {
                r.remove_member(&g, device).map_err(eng)?;
            }
        }
        r.remove_member(&meta.personal, device).map_err(eng)
    }
}
