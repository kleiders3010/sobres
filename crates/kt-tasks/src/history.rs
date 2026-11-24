//! "Undo changes by [person] since [time]" (DESIGN §4, D27).
//!
//! Works at the level of tasks, on the list's history: what the list looked like at the
//! chosen time, what that person's devices did afterwards (tasks they added, tasks they
//! deleted, fields they edited, tasks they moved), and puts it back as new edits. Other
//! people's work is untouched: a field someone else changed since isn't reverted.
//!
//! History comes from the current document and, across a compaction, from the archived
//! previous generations (kept 30 days, `Replica::doc_archive`). Changes carry the author's
//! timestamp (per minute); changes without one count as older than any time.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use loro::{Container, ContainerID, ContainerTrait, LoroDoc, LoroMap, LoroValue, TreeID, ValueOrContainer, VersionVector, ID};

use super::{add_task, delete_task, flat, move_task, set_done, set_notes, set_title, tree, Result};

/// The document's version as of `since_ms`: every peer's changes stamped at or before it.
fn version_at(doc: &LoroDoc, since_ms: u64) -> VersionVector {
    let mut vv = VersionVector::default();
    for (peer, end) in doc.oplog_vv().iter() {
        let mut counter = 0;
        while counter < *end {
            let Some(ch) = doc.get_change(ID::new(*peer, counter)) else { break };
            if (ch.timestamp.max(0) as u64).saturating_mul(1000) > since_ms {
                break;
            }
            counter = ch.id.counter + ch.len as i32;
        }
        if counter > 0 {
            vv.insert(*peer, counter);
        }
    }
    vv
}

/// Task ID -> tree node, including deleted tasks (their last move says who deleted them).
fn nodes(doc: &LoroDoc) -> HashMap<String, TreeID> {
    let t = tree(doc);
    let mut out = HashMap::new();
    for n in t.nodes() {
        if let Ok(meta) = t.get_meta(n) {
            if let Some(ValueOrContainer::Value(LoroValue::String(id))) = meta.get("id") {
                out.insert(id.to_string(), n);
            }
        }
    }
    out
}

/// Undo what `authors` (Loro peers: one person's devices) did in `history` after
/// `since_ms`, as new edits to `now` (the current document). `history` is `now` itself,
/// or an archived earlier generation. Returns how many things were put back.
pub fn undo_by(now: &LoroDoc, history: &LoroDoc, authors: &HashSet<u64>, since_ms: u64) -> Result<usize> {
    let vv = version_at(history, since_ms);
    let then = history.fork_at(&history.vv_to_frontiers(&vv)).unwrap_or_else(|_| LoroDoc::new());
    let (t_then, t_hist, t_now) = (flat(&then), flat(history), flat(now));
    let hist_nodes = nodes(history);
    let ht = tree(history);
    let by_them = |id: Option<ID>| id.is_some_and(|i| authors.contains(&i.peer));
    let mut undone = 0;

    // Tasks they added since: gone again.
    for id in t_hist.keys().filter(|k| !t_then.contains_key(*k)) {
        let created_by_them = hist_nodes.get(id).is_some_and(|n| authors.contains(&n.peer));
        if created_by_them && t_now.contains_key(id) {
            delete_task(now, id)?;
            undone += 1;
        }
    }
    // Tasks they deleted since: back, where they were (parents first).
    let mut restore: Vec<&String> = t_then.keys().filter(|k| !t_hist.contains_key(*k) && !t_now.contains_key(*k)).collect();
    restore.retain(|id| hist_nodes.get(*id).is_some_and(|n| by_them(ht.get_last_move_id(n))));
    let mut restored: BTreeMap<String, String> = BTreeMap::new(); // old id -> new id
    let mut progress = true;
    while !restore.is_empty() && progress {
        progress = false;
        let cur = flat(now);
        let waiting: HashSet<String> = restore.iter().map(|s| (*s).clone()).collect();
        restore.retain(|id| {
            let (parent, title, done, notes) = &t_then[*id];
            let parent_now = parent.as_ref().map(|p| restored.get(p).cloned().unwrap_or_else(|| p.clone()));
            if parent_now.as_ref().is_some_and(|p| !cur.contains_key(p)) && parent.as_ref().is_some_and(|p| waiting.contains(p)) {
                return true; // its parent comes back first
            }
            let parent_ok = parent_now.filter(|p| cur.contains_key(p));
            if let Ok(new_id) = add_task(now, parent_ok.as_deref(), title) {
                let _ = set_done(now, &new_id, *done);
                if !notes.is_empty() {
                    let _ = set_notes(now, &new_id, notes);
                }
                restored.insert((*id).clone(), new_id);
                undone += 1;
            }
            progress = true;
            false
        });
    }
    // Fields they changed since (only where nobody changed them again), and moves.
    for (id, (p_then, title_then, done_then, notes_then)) in &t_then {
        let (Some((p_hist, title_hist, done_hist, notes_hist)), Some((p_now, title_now, done_now, notes_now))) = (t_hist.get(id), t_now.get(id)) else { continue };
        let Some(n) = hist_nodes.get(id) else { continue };
        let Ok(meta) = ht.get_meta(*n) else { continue };
        let edited = |field: &str| meta.get_last_editor(field).is_some_and(|p| authors.contains(&p));
        if title_then != title_hist && title_now == title_hist && edited("title") {
            set_title(now, id, title_then)?;
            undone += 1;
        }
        if done_then != done_hist && done_now == done_hist && edited("done") {
            set_done(now, id, *done_then)?;
            undone += 1;
        }
        if notes_then != notes_hist && notes_now == notes_hist && edited("notes") {
            set_notes(now, id, notes_then)?;
            undone += 1;
        }
        if p_then != p_hist && p_now == p_hist && by_them(ht.get_last_move_id(n)) {
            let parent_ok = p_then.as_ref().filter(|p| t_now.contains_key(*p));
            if move_task(now, id, parent_ok.map(String::as_str), usize::MAX).is_ok() {
                undone += 1;
            }
        }
    }
    undone += undo_budget(now, history, &then, authors)?;
    Ok(undone)
}

/// A map entry that is itself a map.
fn child_map(m: &LoroMap, key: &str) -> Option<LoroMap> {
    match m.get(key) {
        Some(ValueOrContainer::Container(Container::Map(c))) => Some(c),
        _ => None,
    }
}

/// A field's value, a map field as its whole content (a split's parts).
fn deep(m: &LoroMap, key: &str) -> Option<LoroValue> {
    m.get(key).map(|v| match v {
        ValueOrContainer::Value(v) => v,
        ValueOrContainer::Container(Container::Map(c)) => c.get_deep_value(),
        ValueOrContainer::Container(_) => LoroValue::Null,
    })
}

fn copy_into(from: &LoroMap, to: &LoroMap) -> Result<()> {
    for k in from.keys() {
        match from.get(&k) {
            Some(ValueOrContainer::Value(v)) => to.insert(&k, v).map_err(crate::loro)?,
            Some(ValueOrContainer::Container(Container::Map(c))) => copy_into(&c, &to.insert_container(&k, LoroMap::new()).map_err(crate::loro)?)?,
            _ => {}
        }
    }
    Ok(())
}

/// Fields of one map `authors` changed since (and nobody changed again): back to `then`'s value.
fn undo_fields(then: &LoroMap, hist: &LoroMap, now: &LoroMap, authors: &HashSet<u64>) -> Result<usize> {
    let keys: BTreeSet<String> = then.keys().chain(hist.keys()).map(|k| k.to_string()).collect();
    let mut n = 0;
    for f in keys {
        let (vt, vh) = (deep(then, &f), deep(hist, &f));
        if vt == vh || deep(now, &f) != vh || !hist.get_last_editor(&f).is_some_and(|p| authors.contains(&p)) {
            continue;
        }
        match then.get(&f) {
            None => now.delete(&f).map_err(crate::loro)?,
            Some(ValueOrContainer::Value(v)) => now.insert(&f, v).map_err(crate::loro)?,
            Some(ValueOrContainer::Container(Container::Map(c))) => copy_into(&c, &now.insert_container(&f, LoroMap::new()).map_err(crate::loro)?)?,
            Some(ValueOrContainer::Container(_)) => continue,
        }
        n += 1;
    }
    Ok(n)
}

/// A budget (safety audit 2026-11-24 S4-1: undo walked only the task tree, so a budget always had "nothing to
/// undo"): envelopes, accounts and entries they added since go (envelopes and accounts the way the app removes
/// them), entries they deleted come back with who entered them, and fields they changed (a split, a deletion mark
/// on an envelope, the currency) go back. A task list has none of these maps: nothing happens there.
fn undo_budget(now: &LoroDoc, history: &LoroDoc, then: &LoroDoc, authors: &HashSet<u64>) -> Result<usize> {
    let mut undone = undo_fields(&then.get_map("meta"), &history.get_map("meta"), &now.get_map("meta"), authors)?;
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(1);
    for root in ["env", "acct", "tx"] {
        let (r_then, r_hist, r_now) = (then.get_map(root), history.get_map(root), now.get_map(root));
        let keys: BTreeSet<String> = r_then.keys().chain(r_hist.keys()).map(|k| k.to_string()).collect();
        for k in keys {
            let c_now = child_map(&r_now, &k);
            match (child_map(&r_then, &k), child_map(&r_hist, &k)) {
                (None, Some(h)) => {
                    let ContainerID::Normal { peer, .. } = h.id() else { continue };
                    if !authors.contains(&peer) || c_now.is_none() {
                        continue;
                    }
                    match root {
                        "env" => crate::budget::delete_envelope(now, &k, now_ms, false)?,
                        "acct" => crate::budget::delete_account(now, &k, now_ms, false)?,
                        _ => crate::budget::delete_tx(now, &k)?,
                    }
                    undone += 1;
                }
                (Some(t), None) => {
                    if c_now.is_some() || !r_hist.get_last_editor(&k).is_some_and(|p| authors.contains(&p)) {
                        continue;
                    }
                    match crate::budget::transactions(then).into_iter().find(|x| x.id == k).filter(|_| root == "tx") {
                        Some(x) => crate::budget::restore_tx(now, &x)?,
                        None => copy_into(&t, &r_now.insert_container(&k, LoroMap::new()).map_err(crate::loro)?)?,
                    }
                    undone += 1;
                }
                (Some(t), Some(h)) => {
                    if let Some(n) = c_now {
                        if undo_fields(&t, &h, &n, authors)? > 0 {
                            undone += 1;
                        }
                    }
                }
                (None, None) => {}
            }
        }
    }
    now.commit();
    Ok(undone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{add_task, init_list, tasks};

    fn titles(doc: &LoroDoc) -> Vec<String> {
        let mut v: Vec<String> = flat(doc).values().map(|x| x.1.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn a_persons_changes_since_a_time_come_undone() {
        let doc = LoroDoc::new();
        doc.set_record_timestamp(true);
        doc.set_change_merge_interval(0);
        doc.set_peer_id(1).unwrap(); // Alex
        init_list(&doc, "Groceries").unwrap();
        let milk = add_task(&doc, None, "milk").unwrap();
        let bread = add_task(&doc, None, "bread").unwrap();
        let eggs = add_task(&doc, None, "eggs").unwrap();
        doc.commit();
        // "Since" is between the setup and the edits (timestamps are in seconds).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Sam (peer 2) makes a mess: renames, completes, deletes, adds.
        let sam = doc.fork();
        sam.set_peer_id(2).unwrap();
        sam.set_record_timestamp(true);
        set_title(&sam, &milk, "MILK!!!").unwrap();
        set_done(&sam, &bread, true).unwrap();
        delete_task(&sam, &eggs).unwrap();
        add_task(&sam, None, "spam").unwrap();
        sam.commit();
        doc.import(&sam.export(loro::ExportMode::all_updates()).unwrap()).unwrap();
        // Alex also edits after that time: kept.
        add_task(&doc, None, "apples").unwrap();
        doc.commit();

        let n = undo_by(&doc, &doc.fork(), &HashSet::from([2]), since).unwrap();
        assert_eq!(n, 4, "rename, completion, deletion and addition undone");
        assert_eq!(titles(&doc), ["apples", "bread", "eggs", "milk"]);
        assert!(!tasks(&doc).iter().any(|t| t.done), "bread isn't done anymore");
    }
}
