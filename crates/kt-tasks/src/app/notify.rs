//! Notifications about others' changes, built from merged state (DESIGN §8).
//!
//! After each background sync the app compares every list with what this device last
//! showed or notified, and reports what *other people* did since: tasks added and
//! completed, and people joining a group ("every join notifies the group", §5), with who
//! and when. Changes by the account's own devices aren't reported,
//! stale ones (older than [`STALE_MS`]) are dropped, and nothing is reported for a list
//! seen for the first time. People's names come from each group's document, where every
//! member device publishes its display name.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use kt_engine::ids::peer_id_for;
use kt_engine::{storage, wire, DeviceId, GroupId};
use loro::LoroValue;

use super::{eng, group_doc, lists_of, App, ListInfo};
use crate::Result;

/// Changes older than this aren't worth a notification anymore.
pub const STALE_MS: u64 = 3 * 86_400_000;

/// In [`Seen`]'s per-list map: budget entries are "tx:<id>", and "tx:" alone says they were recorded.
const TX_MARK: &str = "tx:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Completed,
    /// Someone entered money in a budget (U-S2): `title` is the payee, [`Change::cents`] the amount (negative = income).
    Entered,
    /// Someone joined the group (`list` is empty, `title` too).
    Joined,
}

/// One change by someone else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub group: GroupId,
    pub group_name: String,
    /// The list it happened in (none for joins).
    pub list: Option<ListInfo>,
    /// Their display name ("Someone" if they haven't published one).
    pub who: String,
    pub kind: ChangeKind,
    pub title: String,
    /// When they made it (their device's clock).
    pub at_ms: u64,
    /// [`ChangeKind::Entered`]: the amount in cents (income negative) and the budget's currency symbol.
    pub cents: i64,
    pub currency: String,
}

/// What this device last showed or notified.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Seen {
    /// Per list: task ID → done.
    lists: BTreeMap<String, BTreeMap<String, bool>>,
    /// Per group: member devices.
    groups: BTreeMap<String, BTreeSet<String>>,
}

/// A joiner's name usually arrives a round after the join; wait this long for it before
/// saying "Someone joined".
const NAME_WAIT_MS: u64 = 86_400_000;

pub(super) fn people(r: &kt_engine::replica::Replica, g: &GroupId) -> HashMap<String, String> {
    let Some(d) = group_doc(r, g).and_then(|gd| r.doc(g, &gd)) else { return HashMap::new() };
    let people = d.get_map("people");
    // Second-pass Sobres audit S3-4 (b): a name counts only when that device wrote it itself (each change is signed
    // by its device, so the last writer of an entry can't be faked); another member's app can't rename anyone.
    let own = |k: &str| kt_engine::ids::DeviceId::from_hex(k).is_some_and(|dev| people.get_last_editor(k) == Some(peer_id_for(&dev)));
    match people.get_deep_value() {
        LoroValue::Map(m) => m
            .iter()
            .filter(|(k, _)| own(k))
            .filter_map(|(k, v)| if let LoroValue::String(s) = v { Some((k.clone(), crate::clean_name(s))) } else { None })
            .filter(|(_, s)| !s.is_empty())
            .collect(),
        _ => HashMap::new(),
    }
}

impl App {
    fn seen_path(&self) -> std::path::PathBuf {
        self.data_dir.join("seen")
    }

    fn load_seen(&self) -> Option<Seen> {
        let b = std::fs::read(self.seen_path()).ok()?;
        wire::decode::<Seen>(*b"SN", 2, &b).ok().map(|x| x.1)
    }

    /// Publish our display name in every group we're in (for others' notifications and
    /// history). Written only when missing or changed.
    pub fn publish_names(&self) -> Result<()> {
        let meta = self.meta();
        let name = self.display_name();
        if name.is_empty() {
            return Ok(());
        }
        let mut r = self.lock();
        let me = r.device_id();
        for g in r.groups() {
            if g == meta.personal || !r.epochs(&g).is_some_and(|e| e.is_current_member(&me)) {
                continue;
            }
            if people(&r, &g).get(&me.to_hex()) == Some(&name) {
                continue;
            }
            let Some(gd) = group_doc(&r, &g) else { continue };
            let people = r.doc(&g, &gd).unwrap().get_map("people");
            // After another member compacted the group, it is the last writer of every entry and ours no longer counts
            // (S3-4). Writing the same text again is no change at all in Loro, so the name never came back: the entry
            // is removed and written again, which makes this device its writer.
            if people.get(&me.to_hex()).is_some() {
                people.delete(&me.to_hex()).map_err(eng)?;
            }
            people.insert(&me.to_hex(), name.as_str()).map_err(eng)?;
            r.commit(&g, &gd).map_err(eng)?;
        }
        Ok(())
    }

    /// Others' changes since the last call (or [`App::mark_seen`]); then all of it counts
    /// as seen. The first call only records the current state.
    pub fn take_changes(&self) -> Result<Vec<Change>> {
        let before = self.load_seen();
        let meta = self.meta();
        let now = kt_engine::replica::now_ms();
        let r = self.lock();
        let ours: BTreeSet<u64> = r.members(&meta.personal).iter().map(peer_id_for).collect();
        let mut seen = Seen::default();
        let mut out = Vec::new();
        let group_names: HashMap<GroupId, String> = super::groups_of(&r, meta.personal).into_iter().map(|g| (g.group, g.name)).collect();
        // Joins.
        for g in r.groups() {
            if g == meta.personal || !r.epochs(&g).is_some_and(|e| e.is_current_member(&r.device_id())) {
                continue;
            }
            let names = people(&r, &g);
            let known = before.as_ref().and_then(|b| b.groups.get(&g.to_hex()));
            let mut now_seen = BTreeSet::new();
            for d in r.members(&g) {
                let hex = d.to_hex();
                if known.is_none_or(|k| k.contains(&hex)) || ours.contains(&peer_id_for(&d)) {
                    now_seen.insert(hex);
                    continue;
                }
                let at = r
                    .epochs(&g)
                    .map(|e| {
                        e.all_records()
                            .filter(|rec| matches!(&rec.body.kind, kt_engine::epochs::EpochKind::Add { device, .. } if device.sign == d.0))
                            .map(|rec| rec.body.created_ms)
                            .max()
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);
                let who = match names.get(&hex) {
                    Some(n) => n.clone(),
                    None if now.saturating_sub(at) < NAME_WAIT_MS => continue, // not seen yet: ask again later
                    None => "Someone".into(),
                };
                now_seen.insert(hex);
                if now.saturating_sub(at) <= STALE_MS {
                    let group_name = group_names.get(&g).cloned().unwrap_or_default();
                    out.push(Change { group: g, group_name, list: None, who, kind: ChangeKind::Joined, title: String::new(), at_ms: at, cents: 0, currency: String::new() });
                }
            }
            seen.groups.insert(g.to_hex(), now_seen);
        }
        // Lists.
        for list in lists_of(&r, meta.personal) {
            let Some(doc) = r.doc(&list.group, &list.doc) else { continue };
            let tasks = crate::activity(doc);
            let known = before.as_ref().and_then(|b| b.lists.get(&list.doc.to_hex()));
            if let Some(known) = known {
                let names = people(&r, &list.group);
                let who = |peer: u64| -> String {
                    r.members(&list.group)
                        .into_iter()
                        .find(|d: &DeviceId| peer_id_for(d) == peer)
                        .and_then(|d| names.get(&d.to_hex()).cloned())
                        .unwrap_or_else(|| "Someone".into())
                };
                for t in &tasks {
                    let (kind, by, at) = match known.get(&t.id) {
                        None => (ChangeKind::Added, Some(t.created_by), t.created_ms),
                        Some(false) if t.done => (ChangeKind::Completed, t.done_by, t.done_ms),
                        _ => continue,
                    };
                    let Some(by) = by else { continue };
                    if ours.contains(&by) || now.saturating_sub(at) > STALE_MS {
                        continue;
                    }
                    out.push(Change { group: list.group, group_name: list.group_name.clone(), list: Some(list.clone()), who: who(by), kind, title: t.title.clone(), at_ms: at, cents: 0, currency: String::new() });
                }
            }
            let mut ids: BTreeMap<String, bool> = tasks.into_iter().map(|t| (t.id, t.done)).collect();
            // Budget entries (U-S2): a new expense or income by someone else. Entries are kept as "tx:<id>"; the
            // "tx:" marker says they were recorded, so the first run after an update doesn't report old entries.
            let txs = r.doc_authors(&list.group, &list.doc).map(|a| crate::budget::transactions_by(doc, a)).unwrap_or_default();
            if let Some(known) = known.filter(|k| k.contains_key(TX_MARK)) {
                let names = people(&r, &list.group);
                let mut currency = None;
                for t in txs.iter().filter(|t| t.kind == "exp" || t.kind == "inc") {
                    let Some(by) = t.by_peer else { continue };
                    if known.contains_key(&format!("{TX_MARK}{}", t.id)) || ours.contains(&by) || now.saturating_sub(t.at) > STALE_MS {
                        continue;
                    }
                    let who = r.members(&list.group).into_iter().find(|d| peer_id_for(d) == by).and_then(|d| names.get(&d.to_hex()).cloned()).unwrap_or_else(|| "Someone".into());
                    let cents = if t.kind == "inc" { -t.amount } else { t.amount };
                    let currency = currency.get_or_insert_with(|| crate::budget::currency_of(doc)).clone();
                    let title = if t.payee.is_empty() { t.note.clone() } else { t.payee.clone() };
                    out.push(Change { group: list.group, group_name: list.group_name.clone(), list: Some(list.clone()), who, kind: ChangeKind::Entered, title, at_ms: t.at, cents, currency });
                }
            }
            ids.insert(TX_MARK.into(), false);
            ids.extend(txs.into_iter().map(|t| (format!("{TX_MARK}{}", t.id), false)));
            seen.lists.insert(list.doc.to_hex(), ids);
        }
        drop(r);
        storage::write_atomic(&self.seen_path(), &wire::encode(*b"SN", 2, &seen).map_err(eng)?).map_err(eng)?;
        out.sort_by_key(|c| c.at_ms);
        Ok(out)
    }

    /// The user looked (the app is open): nothing up to now needs a notification.
    pub fn mark_seen(&self) -> Result<()> {
        self.take_changes().map(|_| ())
    }
}
