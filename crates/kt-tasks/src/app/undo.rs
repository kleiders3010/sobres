//! "Undo changes by [person] since [time]" on a list (DESIGN §4, D27): over the list's
//! current history and its archived earlier generations (kept 30 days).

use std::collections::{BTreeMap, HashSet};

use kt_engine::ids::peer_id_for;
use kt_engine::DeviceId;

use super::{eng, App, ListInfo};
use crate::Result;

/// Someone who edits a list: their name and their devices in the list's group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub devices: Vec<DeviceId>,
    /// This account (its own devices).
    pub is_me: bool,
}

impl App {
    /// The people of a list's group, by their published names (devices without one yet
    /// are grouped as "Someone").
    pub fn people(&self, list: &ListInfo) -> Vec<Person> {
        let meta = self.meta();
        let own_name = self.display_name();
        let r = self.lock();
        let names = super::notify::people(&r, &list.group);
        let mine: HashSet<DeviceId> = r.members(&meta.personal).into_iter().collect();
        let mut by_name: BTreeMap<(bool, String), Vec<DeviceId>> = BTreeMap::new();
        for d in r.members(&list.group) {
            let is_me = mine.contains(&d) || d == r.device_id();
            // Our own devices by our name (the personal group doesn't publish names).
            let name = if is_me && !own_name.is_empty() { own_name.clone() } else { names.get(&d.to_hex()).cloned().unwrap_or_else(|| "Someone".into()) };
            by_name.entry((is_me, name)).or_default().push(d);
        }
        by_name.into_iter().map(|((is_me, name), devices)| Person { name, devices, is_me }).collect()
    }

    /// Undo what `person` did in `list` since `since_ms`, as new edits (other people's
    /// work stays). Returns how many things were put back.
    pub fn undo_changes_by(&self, list: &ListInfo, person: &Person, since_ms: u64) -> Result<usize> {
        let authors: HashSet<u64> = person.devices.iter().map(peer_id_for).collect();
        let mut r = self.lock();
        // Earlier generations first (only those left after `since`), then the current one.
        let archived: Vec<loro::LoroDoc> = r.doc_archive(&list.group, &list.doc).into_iter().filter(|(_, left, _)| *left > since_ms).map(|x| x.2).collect();
        let now = r.doc(&list.group, &list.doc).ok_or(crate::Error::NotFound)?;
        let mut undone = 0;
        for h in &archived {
            undone += crate::history::undo_by(now, h, &authors, since_ms)?;
        }
        let current = now.fork();
        undone += crate::history::undo_by(now, &current, &authors, since_ms)?;
        if undone > 0 {
            r.commit(&list.group, &list.doc).map_err(eng)?;
        }
        Ok(undone)
    }
}
