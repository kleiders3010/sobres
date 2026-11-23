//! kt-tasks: the task-list domain on top of kt-engine (thin, app-specific).
//!
//! Schema of one list document:
//! - map `meta`: `name` (string), `hue` (i64 0–360 or absent), `icon` (string or absent),
//!   `type` ("tasks" or "shopping"; anything else reads as "tasks")
//! - tree `tasks`: each node's meta map holds
//!   - `id`: stable task ID (random hex); survives document epochs, which recreate tree nodes
//!   - `title` (string), `done` (bool), `notes` (string): inline text up to the attachment
//!     threshold; longer notes are a text attachment, stored as [`BLOB_MARK`] + blob
//!     reference + newline + a preview, in this one field so every edit, undo and merge
//!     moves reference and preview together
//!   - `image:<blob id>` (string, one field per image): "<added ms> <blob ref>" for a
//!     sealed image blob, so concurrent additions on two devices both survive a merge
//!     (the first builds' single `images` field, one ref per line, is still read)
//!
//! Task order is the tree's fractional-index order. Unknown fields are preserved
//! (principle 8): the domain never deletes keys it doesn't know.

use std::collections::BTreeMap;

pub mod app;
pub mod budget;
pub mod appearance;
pub mod export;
pub mod history;

use kt_engine::limits::TEXT_ATTACHMENT_THRESHOLD;
use loro::{LoroDoc, LoroMap, LoroTree, LoroValue, TreeID, ValueOrContainer};
use rand::RngCore;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("task not found")]
    NotFound,
    #[error("loro: {0}")]
    Loro(String),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn loro<E: std::fmt::Display>(e: E) -> Error {
    Error::Loro(e.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskView {
    pub id: String,
    pub title: String,
    pub done: bool,
    pub notes: String,
    /// A timed reminder (ms since the epoch), shared like any field.
    pub remind_ms: Option<u64>,
    /// How the reminder repeats ([`valid_repeat`]): ticking it off moves it to its next date.
    pub repeat: Option<String>,
    /// Starred (Google Tasks' star), and when: the Starred view and "Starred recently" sort by it.
    pub starred_ms: Option<u64>,
    /// My Day (Microsoft To Do's): (whose, day "YYYY-MM-DD") per person who put it in their day.
    /// Each person's own (the account's personal group), so a shared list's members each have theirs.
    pub my_day: Vec<(String, String)>,
    /// Who it's assigned to (Microsoft To Do's "Assign to" in shared lists): that person's devices
    /// (hex) when it was assigned, and their name then. One person at most.
    pub assignee: Option<(Vec<String>, String)>,
    /// Attached images (sealed blobs; the bytes travel like text attachments).
    pub images: Vec<kt_engine::blobs::BlobRef>,
    pub children: Vec<TaskView>,
}

/// How a list is shown. Both behave the same underneath; the type is shared with members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListType {
    Tasks,
    /// Completed items drop to the bottom (the apps do it a moment after checking).
    Shopping,
}

impl ListType {
    pub fn as_str(self) -> &'static str {
        match self {
            ListType::Tasks => "tasks",
            ListType::Shopping => "shopping",
        }
    }

    /// Unknown values (a newer version's types) read as a plain task list.
    pub fn parse(s: &str) -> ListType {
        if s == "shopping" { ListType::Shopping } else { ListType::Tasks }
    }
}

pub fn list_type(doc: &LoroDoc) -> ListType {
    ListType::parse(&get_str(&doc.get_map("meta"), "type").unwrap_or_default())
}

pub fn set_list_type(doc: &LoroDoc, t: ListType) -> Result<()> {
    doc.get_map("meta").insert("type", t.as_str()).map_err(loro)?;
    doc.commit();
    Ok(())
}

/// Most images per task.
pub const MAX_IMAGES: usize = 20;

/// Each image is its own field, `image:<blob id>` = "<added ms> <blob ref>", so two devices
/// adding images to the same task at the same time both keep theirs when they merge.
const IMAGE_KEY: &str = "image:";
/// The first builds kept all refs in one `images` field (one line each): still read.
const LEGACY_IMAGES: &str = "images";

fn images_of(meta: &LoroMap) -> Vec<kt_engine::blobs::BlobRef> {
    let mut out: Vec<(u64, kt_engine::blobs::BlobRef)> = Vec::new();
    for line in get_str(meta, LEGACY_IMAGES).unwrap_or_default().lines() {
        if let Ok(r) = kt_engine::blobs::BlobRef::parse(line.trim()) {
            out.push((0, r));
        }
    }
    meta.for_each(|k, v| {
        let (Some(_), ValueOrContainer::Value(LoroValue::String(s))) = (k.strip_prefix(IMAGE_KEY), v) else { return };
        let (ms, r) = s.split_once(' ').unwrap_or(("0", &s));
        if let Ok(r) = kt_engine::blobs::BlobRef::parse(r) {
            out.push((ms.parse().unwrap_or(0), r));
        }
    });
    out.sort_by(|a, b| (a.0, a.1.id).cmp(&(b.0, b.1.id))); // in the order they were added
    let mut seen = std::collections::HashSet::new();
    out.into_iter().map(|x| x.1).filter(|r| seen.insert(r.id)).collect()
}

/// Attach a sealed image blob to a task (the caller stores the blob).
pub fn add_image(doc: &LoroDoc, task: &str, image: &kt_engine::blobs::BlobRef) -> Result<()> {
    let n = find(doc, task)?;
    let meta = tree(doc).get_meta(n).map_err(loro)?;
    let images = images_of(&meta);
    if images.iter().any(|r| r.id == image.id) {
        return Ok(());
    }
    if images.len() >= MAX_IMAGES {
        return Err(Error::Invalid(format!("a task holds at most {MAX_IMAGES} images")));
    }
    let key = format!("{IMAGE_KEY}{}", hex::encode(image.id));
    meta.insert(&key, format!("{} {}", now_ms(), image.encode()).as_str()).map_err(loro)?;
    doc.commit();
    Ok(())
}

pub fn remove_image(doc: &LoroDoc, task: &str, id: &kt_engine::blobs::BlobId) -> Result<()> {
    let n = find(doc, task)?;
    let meta = tree(doc).get_meta(n).map_err(loro)?;
    meta.delete(&format!("{IMAGE_KEY}{}", hex::encode(id))).map_err(loro)?;
    // An image from the first builds' single field: rewrite that field without it.
    let legacy: Vec<String> = get_str(&meta, LEGACY_IMAGES).unwrap_or_default().lines().map(str::trim).filter(|l| kt_engine::blobs::BlobRef::parse(l).is_ok_and(|r| &r.id != id)).map(String::from).collect();
    if meta.get(LEGACY_IMAGES).is_some() {
        if legacy.is_empty() { meta.delete(LEGACY_IMAGES) } else { meta.insert(LEGACY_IMAGES, legacy.join("\n").as_str()) }.map_err(loro)?;
    }
    doc.commit();
    Ok(())
}

fn tree(doc: &LoroDoc) -> LoroTree {
    let t = doc.get_tree("tasks");
    t.enable_fractional_index(0);
    t
}

fn new_task_id() -> String {
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

fn get_str(m: &LoroMap, key: &str) -> Option<String> {
    match m.get(key)? {
        ValueOrContainer::Value(LoroValue::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn get_bool(m: &LoroMap, key: &str) -> Option<bool> {
    match m.get(key)? {
        ValueOrContainer::Value(LoroValue::Bool(b)) => Some(b),
        _ => None,
    }
}

/// Initialize a new list document.
pub fn init_list(doc: &LoroDoc, name: &str) -> Result<()> {
    let meta = doc.get_map("meta");
    meta.insert("name", name).map_err(loro)?;
    tree(doc);
    doc.commit();
    Ok(())
}

pub fn list_name(doc: &LoroDoc) -> String {
    clean_name(&get_str(&doc.get_map("meta"), "name").unwrap_or_default())
}

/// The longest name kept (a person, list or group), in characters.
pub const NAME_MAX: usize = 80;

/// A name other people will read (APP-RULES safety 15): control, direction-changing and invisible characters go,
/// so nobody can pose as someone else in "X joined" or an assignee; emoji joiners stay (family and flag emoji),
/// runs of spaces become one, and it is cut to [`NAME_MAX`]. Applied where names are written and read.
pub fn clean_name(s: &str) -> String {
    clean_to(s, NAME_MAX)
}

/// [`clean_name`] with another length: also used for task titles on read (second-pass audit T2-14g, rule 15).
pub fn clean_to(s: &str, max: usize) -> String {
    let kept: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control() && !is_invisible(*c))
        .collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

/// Invisible and direction-changing characters (rule 15): they go from names, titles and every CSV cell (QR Q2-7:
/// one of them in front of `=` hid a formula start).
pub fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{061C}' | '\u{200B}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{2069}' | '\u{FEFF}' | '\u{180E}' | '\u{00AD}'
        // as the web apps' cleaner (Sumate pass 2): grapheme joiner, line/paragraph separators, blank braille
        | '\u{034F}' | '\u{2028}' | '\u{2029}' | '\u{2800}' | '\u{E0001}')
}

pub fn rename_list(doc: &LoroDoc, name: &str) -> Result<()> {
    doc.get_map("meta").insert("name", name).map_err(loro)?;
    doc.commit();
    Ok(())
}

/// One task (with its subtasks), if it exists.
pub(crate) fn find_view(doc: &LoroDoc, task_id: &str) -> Option<TaskView> {
    let n = find(doc, task_id).ok()?;
    view_of(&tree(doc), n)
}

fn find(doc: &LoroDoc, task_id: &str) -> Result<TreeID> {
    let t = tree(doc);
    for n in t.nodes() {
        if t.is_node_deleted(&n).unwrap_or(true) {
            continue;
        }
        if let Ok(meta) = t.get_meta(n) {
            if get_str(&meta, "id").as_deref() == Some(task_id) {
                return Ok(n);
            }
        }
    }
    Err(Error::NotFound)
}

/// Add a task at the end of `parent` (or the top level). Returns its stable ID.
pub fn add_task(doc: &LoroDoc, parent: Option<&str>, title: &str) -> Result<String> {
    let t = tree(doc);
    let parent_node = match parent {
        Some(p) => Some(find(doc, p)?),
        None => None,
    };
    let index = t.children_num(parent_node).unwrap_or(0);
    let node = t.create_at(parent_node, index).map_err(loro)?;
    let meta = t.get_meta(node).map_err(loro)?;
    let id = new_task_id();
    meta.insert("id", id.as_str()).map_err(loro)?;
    meta.insert("title", title).map_err(loro)?;
    meta.insert("done", false).map_err(loro)?;
    meta.insert("created_ms", now_ms() as i64).map_err(loro)?;
    doc.commit();
    Ok(id)
}

pub fn set_title(doc: &LoroDoc, task: &str, title: &str) -> Result<()> {
    let n = find(doc, task)?;
    tree(doc).get_meta(n).map_err(loro)?.insert("title", title).map_err(loro)?;
    doc.commit();
    Ok(())
}

pub fn set_done(doc: &LoroDoc, task: &str, done: bool) -> Result<()> {
    let n = find(doc, task)?;
    let meta = tree(doc).get_meta(n).map_err(loro)?;
    meta.insert("done", done).map_err(loro)?;
    if done {
        meta.insert("done_ms", now_ms() as i64).map_err(loro)?;
    }
    doc.commit();
    Ok(())
}

/// Set a task's notes: inline text, or an attachment's notes value ([`attachment_notes`]).
/// Text above the attachment threshold (D4) goes through `App::set_notes`, which makes the
/// blob; here it's refused, never silently cut.
pub fn set_notes(doc: &LoroDoc, task: &str, notes: &str) -> Result<()> {
    let value = if notes.starts_with(BLOB_MARK) {
        match notes_of(notes) {
            Notes::Attachment { .. } => notes.to_string(),
            Notes::Inline(_) => return Err(Error::Invalid("not a valid attachment".into())),
        }
    } else if notes.len() > TEXT_ATTACHMENT_THRESHOLD {
        return Err(Error::Invalid("notes this long go through App::set_notes (a text attachment)".into()));
    } else {
        notes.replace('\u{0}', "") // the marker can't be typed in
    };
    let n = find(doc, task)?;
    tree(doc).get_meta(n).map_err(loro)?.insert("notes", value.as_str()).map_err(loro)?;
    doc.commit();
    Ok(())
}

/// Starts a notes value that is a text attachment.
pub const BLOB_MARK: &str = "\u{0}blob:";
/// Characters of an attachment kept inline as its preview (lists, exports, widgets).
pub const PREVIEW_CHARS: usize = 2000;

/// A task's notes, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notes<'a> {
    Inline(&'a str),
    Attachment { blob: kt_engine::blobs::BlobRef, preview: &'a str },
}

/// Read a notes value. A malformed attachment reads as no notes, never as the marker text.
pub fn notes_of(raw: &str) -> Notes<'_> {
    let Some(rest) = raw.strip_prefix(BLOB_MARK) else { return Notes::Inline(raw) };
    let (r, preview) = rest.split_once('\n').unwrap_or((rest, ""));
    match kt_engine::blobs::BlobRef::parse(r) {
        Ok(blob) => Notes::Attachment { blob, preview },
        Err(_) => Notes::Inline(""),
    }
}

/// The notes value for a text attachment of `full`.
pub fn attachment_notes(blob: &kt_engine::blobs::BlobRef, full: &str) -> String {
    let preview: String = full.chars().take(PREVIEW_CHARS).collect();
    format!("{BLOB_MARK}{}\n{preview}", blob.encode())
}

impl TaskView {
    /// Notes to show without the attachment: inline notes, or an attachment's preview.
    pub fn notes_preview(&self) -> &str {
        match notes_of(&self.notes) {
            Notes::Inline(t) => t,
            Notes::Attachment { preview, .. } => preview,
        }
    }

    pub fn attachment(&self) -> Option<kt_engine::blobs::BlobRef> {
        match notes_of(&self.notes) {
            Notes::Attachment { blob, .. } => Some(blob),
            Notes::Inline(_) => None,
        }
    }
}

/// Set or clear a task's reminder (ms since the epoch).
pub fn set_reminder(doc: &LoroDoc, task: &str, at_ms: Option<u64>) -> Result<()> {
    let n = find(doc, task)?;
    let meta = tree(doc).get_meta(n).map_err(loro)?;
    match at_ms {
        Some(t) => meta.insert("remind_ms", t as i64).map_err(loro)?,
        None => meta.delete("remind_ms").map_err(loro)?,
    }
    doc.commit();
    Ok(())
}

/// A repeat rule: "wd" (Monday to Friday) or a count and a unit, "1d", "2w", "1m", "1y" (like
/// Todoist's "every": the next date counts from the date it was due, not from when it was done).
pub fn valid_repeat(rule: &str) -> bool {
    if rule == "wd" {
        return true;
    }
    let (n, u) = rule.split_at(rule.len().saturating_sub(1));
    matches!(u, "d" | "w" | "m" | "y") && !n.is_empty() && n.len() <= 2 && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit())
}

/// Set or clear how a task repeats.
pub fn set_repeat(doc: &LoroDoc, task: &str, rule: Option<&str>) -> Result<()> {
    let n = find(doc, task)?;
    let meta = tree(doc).get_meta(n).map_err(loro)?;
    match rule {
        Some(r) if !valid_repeat(r) => return Err(Error::Invalid(format!("not a repeat rule: {r}"))),
        Some(r) => meta.insert("repeat", r).map_err(loro)?,
        None => meta.delete("repeat").map_err(loro)?,
    }
    doc.commit();
    Ok(())
}

/// A calendar day, "YYYY-MM-DD".
pub fn valid_day(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// Put a task in someone's My Day for `day` ("YYYY-MM-DD"), or take it out. `who` is the person
/// (their account's personal group): the mark is theirs only, the other members of a shared list
/// don't see it in their day.
pub fn set_my_day(doc: &LoroDoc, task: &str, who: &str, day: Option<&str>) -> Result<()> {
    if who.is_empty() || who.len() > 64 || !who.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Error::Invalid("who".into()));
    }
    let n = find(doc, task)?;
    let meta = tree(doc).get_meta(n).map_err(loro)?;
    let key = format!("day:{who}");
    match day {
        Some(d) if !valid_day(d) => return Err(Error::Invalid(format!("not a day: {d}"))),
        Some(d) => meta.insert(&key, d).map_err(loro)?,
        None => meta.delete(&key).map_err(loro)?,
    }
    doc.commit();
    Ok(())
}

/// Star or unstar a task (shared like any field; the time it was starred orders the Starred view).
pub fn set_star(doc: &LoroDoc, task: &str, on: bool) -> Result<()> {
    let n = find(doc, task)?;
    let meta = tree(doc).get_meta(n).map_err(loro)?;
    if on {
        meta.insert("star_ms", now_ms() as i64).map_err(loro)?;
    } else {
        meta.delete("star_ms").map_err(loro)?;
    }
    doc.commit();
    Ok(())
}

/// Assign a task to someone in the list (their devices, hex, and their name), or nobody. Shared
/// like any field: the last assignment wins.
pub fn set_assignee(doc: &LoroDoc, task: &str, who: Option<(&[String], &str)>) -> Result<()> {
    let n = find(doc, task)?;
    let meta = tree(doc).get_meta(n).map_err(loro)?;
    match who {
        Some((devices, name)) => {
            if devices.is_empty() || devices.len() > 32 || !devices.iter().all(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit())) {
                return Err(Error::Invalid("devices".into()));
            }
            meta.insert("assign", devices.join(",")).map_err(loro)?;
            meta.insert("assign_name", name.chars().take(80).collect::<String>()).map_err(loro)?;
        }
        None => {
            meta.delete("assign").map_err(loro)?;
            meta.delete("assign_name").map_err(loro)?;
        }
    }
    doc.commit();
    Ok(())
}

/// A repeating task was ticked off: it stays, due again at `next_ms`, with its steps unticked
/// (Todoist's recurring tasks: the same task moves to its next date). One commit, one undo.
pub fn repeat_done(doc: &LoroDoc, task: &str, next_ms: u64) -> Result<()> {
    let n = find(doc, task)?;
    let t = tree(doc);
    let meta = t.get_meta(n).map_err(loro)?;
    meta.insert("remind_ms", next_ms as i64).map_err(loro)?;
    meta.insert("done", false).map_err(loro)?;
    meta.insert("last_done_ms", now_ms() as i64).map_err(loro)?;
    fn untick(t: &LoroTree, n: TreeID) -> Result<()> {
        for c in t.children(Some(n)).unwrap_or_default() {
            if let Ok(m) = t.get_meta(c) {
                if get_bool(&m, "done").unwrap_or(false) {
                    m.insert("done", false).map_err(loro)?;
                }
            }
            untick(t, c)?;
        }
        Ok(())
    }
    untick(&t, n)?;
    doc.commit();
    Ok(())
}

pub fn delete_task(doc: &LoroDoc, task: &str) -> Result<()> {
    let n = find(doc, task)?;
    tree(doc).delete(n).map_err(loro)?;
    doc.commit();
    Ok(())
}

/// Move a task under `parent` (or the top level) at position `index`.
pub fn move_task(doc: &LoroDoc, task: &str, parent: Option<&str>, index: usize) -> Result<()> {
    let n = find(doc, task)?;
    let p = match parent {
        Some(p) => Some(find(doc, p)?),
        None => None,
    };
    tree(doc).mov_to(n, p, index).map_err(loro)?;
    doc.commit();
    Ok(())
}

/// Move a task right before (or after) another one, under that one's parent: what a drop in
/// a list means, whatever the list shows in between (a shopping list hides checked items at
/// the bottom).
pub fn move_task_next_to(doc: &LoroDoc, task: &str, other: &str, after: bool) -> Result<()> {
    let (n, o) = (find(doc, task)?, find(doc, other)?);
    if n == o {
        return Ok(());
    }
    let t = tree(doc);
    if after { t.mov_after(n, o) } else { t.mov_before(n, o) }.map_err(loro)?;
    doc.commit();
    Ok(())
}

/// Read-side limits on what other devices wrote (second-pass audit T2-3, APP-RULES 60): subtasks deeper than this are
/// not shown (the screen shows a few levels; a tree hundreds deep overflowed the reader's stack), titles and notes are
/// cut, and a reminder time must be before the year 2200.
pub const MAX_DEPTH: usize = 32;
pub const TITLE_MAX: usize = 2_000;
pub const NOTES_MAX: usize = 50_000;
const REMIND_MAX_MS: i64 = 7_258_118_400_000;

/// A task id in the app's own shape (24 hex characters, `new_task_id`).
pub fn valid_task_id(s: &str) -> bool {
    s.len() == 24 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn cut(s: String, max: usize) -> String {
    if s.len() <= max { s } else { s.chars().take(max).collect() }
}

fn view_of(t: &LoroTree, n: TreeID) -> Option<TaskView> {
    view_at(t, n, 0, &mut std::collections::HashSet::new())
}

/// One task and its subtasks, at most [`MAX_DEPTH`] levels down (so the recursion is bounded); a task without a
/// valid id, or with an id already seen in this read, is left out with its subtasks.
fn view_at(t: &LoroTree, n: TreeID, depth: usize, seen: &mut std::collections::HashSet<String>) -> Option<TaskView> {
    let meta = t.get_meta(n).ok()?;
    let id = get_str(&meta, "id").filter(|i| valid_task_id(i))?;
    if !seen.insert(id.clone()) {
        return None;
    }
    let children = if depth + 1 >= MAX_DEPTH {
        vec![]
    } else {
        t.children(Some(n)).unwrap_or_default().into_iter().filter_map(|c| view_at(t, c, depth + 1, seen)).collect()
    };
    Some(TaskView {
        remind_ms: match meta.get("remind_ms") {
            Some(ValueOrContainer::Value(LoroValue::I64(v))) if v > 0 && v < REMIND_MAX_MS => Some(v as u64),
            _ => None,
        },
        repeat: get_str(&meta, "repeat").filter(|r| valid_repeat(r)),
        starred_ms: match meta.get("star_ms") {
            Some(ValueOrContainer::Value(LoroValue::I64(v))) if v > 0 => Some(v as u64),
            _ => None,
        },
        my_day: match meta.get_deep_value() {
            LoroValue::Map(m) => m
                .iter()
                .filter_map(|(k, v)| match (k.strip_prefix("day:"), v) {
                    (Some(who), LoroValue::String(d)) if valid_day(d) => Some((who.to_string(), d.to_string())),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        },
        assignee: get_str(&meta, "assign")
            .map(|s| s.split(',').filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_string).collect::<Vec<_>>())
            .filter(|d| !d.is_empty())
            .map(|d| (d, get_str(&meta, "assign_name").unwrap_or_default())),
        id,
        title: clean_to(&get_str(&meta, "title").unwrap_or_default(), TITLE_MAX),
        done: get_bool(&meta, "done").unwrap_or(false),
        notes: cut(get_str(&meta, "notes").unwrap_or_default(), NOTES_MAX),
        images: images_of(&meta),
        children,
    })
}

/// All tasks in display order, nested.
/// Who did what when, per task (for notifications about others' changes). Times are as
/// the editing device stated them; peers are Loro peer IDs (see `kt_engine::peer_id_for`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskActivity {
    pub id: String,
    pub title: String,
    pub done: bool,
    pub created_ms: u64,
    pub created_by: u64,
    pub done_ms: u64,
    pub done_by: Option<u64>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn get_ms(m: &LoroMap, k: &str) -> u64 {
    match m.get(k) {
        Some(ValueOrContainer::Value(LoroValue::I64(v))) => v.max(0) as u64,
        _ => 0,
    }
}

/// Every live task (any depth) with its authorship.
pub fn activity(doc: &LoroDoc) -> Vec<TaskActivity> {
    let t = tree(doc);
    let mut out = Vec::new();
    for n in t.nodes() {
        if t.is_node_deleted(&n).unwrap_or(true) {
            continue;
        }
        let Ok(meta) = t.get_meta(n) else { continue };
        let Some(id) = get_str(&meta, "id") else { continue };
        let done = matches!(meta.get("done"), Some(ValueOrContainer::Value(LoroValue::Bool(true))));
        out.push(TaskActivity {
            id,
            title: get_str(&meta, "title").unwrap_or_default(),
            done,
            created_ms: get_ms(&meta, "created_ms"),
            created_by: n.peer,
            done_ms: get_ms(&meta, "done_ms"),
            done_by: meta.get_last_editor("done"),
        });
    }
    out
}

pub fn tasks(doc: &LoroDoc) -> Vec<TaskView> {
    let t = tree(doc);
    let mut seen = std::collections::HashSet::new();
    t.roots().into_iter().filter_map(|n| view_at(&t, n, 0, &mut seen)).collect()
}

/// Canonical, ID-stable representation of a list's live state. Two documents with equal
/// normalized states show the same list, even if their Loro internals differ.
pub fn normalized(doc: &LoroDoc) -> Vec<u8> {
    let mut out = String::new();
    // Every root map (list meta; group docs' lists, people, ...; profile, index), not just
    // the task tree: a rebuild that dropped any of them must not verify.
    for (name, v) in root_maps(doc) {
        out.push_str(&format!("map {name}|{}\n", canonical(&v)));
    }
    let t = tree(doc);
    // A loop, not recursion (T2-3): same pre-order as before.
    let mut stack: Vec<(TreeID, usize)> = t.roots().into_iter().map(|r| (r, 0)).rev().collect();
    while let Some((n, depth)) = stack.pop() {
        if let Ok(meta) = t.get_meta(n) {
            out.push_str(&format!("{depth}|{}\n", canonical(&meta.get_deep_value())));
        }
        let kids: Vec<_> = t.children(Some(n)).unwrap_or_default().into_iter().map(|c| (c, depth + 1)).collect();
        stack.extend(kids.into_iter().rev());
    }
    out.into_bytes()
}

/// A value as text with map keys sorted (Loro's maps iterate in hash order).
fn canonical(v: &LoroValue) -> String {
    match v {
        LoroValue::Map(m) => {
            let sorted: BTreeMap<&String, &LoroValue> = m.iter().collect();
            let inner: Vec<String> = sorted.into_iter().map(|(k, v)| format!("{k:?}:{}", canonical(v))).collect();
            format!("{{{}}}", inner.join(","))
        }
        LoroValue::List(l) => format!("[{}]", l.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => format!("{other:?}"),
    }
}

/// The document's root maps by name (everything except the task tree).
fn root_maps(doc: &LoroDoc) -> BTreeMap<String, LoroValue> {
    match doc.get_deep_value() {
        LoroValue::Map(m) => m.iter().filter(|(k, v)| k.as_str() != "tasks" && matches!(v, LoroValue::Map(_))).map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => BTreeMap::new(),
    }
}

/// Write `v` under `key` in `map`, recreating nested maps and lists as containers.
fn put_deep(map: &LoroMap, key: &str, v: &LoroValue) -> Result<()> {
    match v {
        LoroValue::Map(m) => {
            let child = map.insert_container(key, LoroMap::new()).map_err(loro)?;
            for (k, x) in m.iter() {
                put_deep(&child, k, x)?;
            }
        }
        LoroValue::List(l) => {
            let child = map.insert_container(key, loro::LoroList::new()).map_err(loro)?;
            for x in l.iter() {
                child.push(x.clone()).map_err(loro)?;
            }
        }
        other => {
            map.insert(key, other.clone()).map_err(loro)?;
        }
    }
    Ok(())
}

/// Build a fresh document with the same live state (document epoch, D24): no tombstones,
/// no history. Stable task IDs, order and unknown fields are preserved.
pub fn rebuild(doc: &LoroDoc, peer: u64) -> Result<LoroDoc> {
    let fresh = LoroDoc::new();
    fresh.set_peer_id(peer).map_err(loro)?;
    // Every root map (a group doc's lists and people, a profile, an index...), not just meta.
    for (name, v) in root_maps(doc) {
        if let LoroValue::Map(m) = v {
            let dst = fresh.get_map(name.as_str());
            for (k, x) in m.iter() {
                put_deep(&dst, k, x)?;
            }
        }
    }
    let st = tree(doc);
    let dt = tree(&fresh);
    // A loop, not recursion: the tree comes from other devices and may be any depth (T2-3).
    let mut stack: Vec<(TreeID, Option<TreeID>, usize)> = st.roots().into_iter().enumerate().map(|(i, r)| (r, None, i)).rev().collect();
    while let Some((src, dst_parent, index)) = stack.pop() {
        let node = dt.create_at(dst_parent, index).map_err(loro)?;
        let dm = dt.get_meta(node).map_err(loro)?;
        if let Ok(sm) = st.get_meta(src) {
            if let LoroValue::Map(m) = sm.get_deep_value() {
                for (k, v) in m.iter() {
                    dm.insert(k, v.clone()).map_err(loro)?;
                }
            }
        }
        let kids: Vec<_> = st.children(Some(src)).unwrap_or_default().into_iter().enumerate().map(|(i, c)| (c, Some(node), i)).collect();
        stack.extend(kids.into_iter().rev());
    }
    fresh.commit();
    Ok(fresh)
}

/// Flatten into id → (parent id, title, done, notes) for diffing.
fn flat(doc: &LoroDoc) -> BTreeMap<String, (Option<String>, String, bool, String)> {
    fn walk(v: &TaskView, parent: Option<String>, out: &mut BTreeMap<String, (Option<String>, String, bool, String)>) {
        out.insert(v.id.clone(), (parent, v.title.clone(), v.done, v.notes.clone()));
        for c in &v.children {
            walk(c, Some(v.id.clone()), out);
        }
    }
    let mut out = BTreeMap::new();
    for t in tasks(doc) {
        walk(&t, None, &mut out);
    }
    out
}

/// Rebase (D24): re-apply the changes that turned `base` into `local` onto `target`, as new
/// ops. Used by a device that had unsynced edits when a document epoch arrived.
pub fn rebase(base: &LoroDoc, local: &LoroDoc, target: &LoroDoc) -> Result<()> {
    let b = flat(base);
    let l = flat(local);
    // Root maps (list meta, group lists/people, profile, index): keys changed locally.
    let (bm, lm) = (root_maps(base), root_maps(local));
    for name in bm.keys().chain(lm.keys()).collect::<std::collections::BTreeSet<_>>() {
        let empty = LoroValue::Map(Default::default());
        let (LoroValue::Map(bv), LoroValue::Map(lv)) = (bm.get(name).unwrap_or(&empty), lm.get(name).unwrap_or(&empty)) else { continue };
        let dst = target.get_map(name.as_str());
        for (k, v) in lv.iter() {
            if bv.get(k) != Some(v) {
                put_deep(&dst, k, v)?;
            }
        }
        for k in bv.keys().filter(|k| !lv.contains_key(*k)) {
            dst.delete(k).map_err(loro)?;
        }
    }
    if list_name(local) != list_name(base) {
        rename_list(target, &list_name(local))?;
    }
    // Added tasks (parents before children: BTreeMap order isn't topological, so loop).
    let mut pending: Vec<&String> = l.keys().filter(|k| !b.contains_key(*k)).collect();
    let mut progress = true;
    while !pending.is_empty() && progress {
        progress = false;
        pending.retain(|id| {
            let (parent, title, done, notes) = &l[*id];
            let parent_ok = parent.as_ref().map(|p| find(target, p).is_ok()).unwrap_or(true);
            if !parent_ok {
                return true;
            }
            let t = tree(target);
            let pn = parent.as_ref().and_then(|p| find(target, p).ok());
            let index = t.children_num(pn).unwrap_or(0);
            if let Ok(node) = t.create_at(pn, index) {
                if let Ok(meta) = t.get_meta(node) {
                    // Every field as it is locally (times included), not just the known ones.
                    let local_meta = find(local, id).ok().and_then(|n| tree(local).get_meta(n).ok());
                    match local_meta.map(|m| m.get_deep_value()) {
                        Some(LoroValue::Map(m)) => {
                            for (k, v) in m.iter() {
                                let _ = meta.insert(k, v.clone());
                            }
                        }
                        _ => {
                            let _ = meta.insert("id", id.as_str());
                            let _ = meta.insert("title", title.as_str());
                            let _ = meta.insert("done", *done);
                            if !notes.is_empty() {
                                let _ = meta.insert("notes", notes.as_str());
                            }
                        }
                    }
                }
            }
            progress = true;
            false
        });
    }
    // Modified and deleted tasks.
    for (id, (bp, bt, bd, bn)) in &b {
        match l.get(id) {
            None => {
                let _ = delete_task(target, id);
            }
            Some((lp, lt, ld, ln)) => {
                if lt != bt {
                    let _ = set_title(target, id, lt);
                }
                if ld != bd {
                    let _ = set_done(target, id, *ld);
                    // Keep when it was done locally, not when it was rebased.
                    if let (Ok(ln), Ok(tn)) = (find(local, id), find(target, id)) {
                        if let (Ok(lm), Ok(tm)) = (tree(local).get_meta(ln), tree(target).get_meta(tn)) {
                            if let Some(ValueOrContainer::Value(v)) = lm.get("done_ms") {
                                let _ = tm.insert("done_ms", v);
                                target.commit();
                            }
                        }
                    }
                }
                if ln != bn {
                    let _ = set_notes(target, id, ln);
                }
                if lp != bp {
                    let _ = move_task(target, id, lp.as_deref(), usize::MAX);
                }
                // Every other field changed here (reminder, repeat, pictures, ...), set or removed.
                let fields = |d: &LoroDoc| match find(d, id).ok().and_then(|n| tree(d).get_meta(n).ok()).map(|m| m.get_deep_value()) {
                    Some(LoroValue::Map(m)) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<BTreeMap<_, _>>(),
                    _ => BTreeMap::new(),
                };
                let (bf, lf) = (fields(base), fields(local));
                const DONE_ABOVE: [&str; 5] = ["id", "title", "done", "done_ms", "notes"];
                if let Ok(tm) = find(target, id).and_then(|n| tree(target).get_meta(n).map_err(loro)) {
                    for (k, v) in lf.iter().filter(|(k, _)| !DONE_ABOVE.contains(&k.as_str())) {
                        if bf.get(k) != Some(v) {
                            let _ = match v {
                                LoroValue::Map(_) | LoroValue::List(_) => put_deep(&tm, k, v),
                                other => tm.insert(k, other.clone()).map_err(loro),
                            };
                        }
                    }
                    for k in bf.keys().filter(|k| !lf.contains_key(*k) && !DONE_ABOVE.contains(&k.as_str())) {
                        let _ = tm.delete(k);
                    }
                }
            }
        }
    }
    target.commit();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use loro::ExportMode;

    fn doc() -> LoroDoc {
        let d = LoroDoc::new();
        d.set_peer_id(1).unwrap();
        init_list(&d, "Groceries").unwrap();
        d
    }

    #[test]
    fn repeat_rules() {
        for ok in ["wd", "1d", "2w", "1m", "1y", "12w"] {
            assert!(valid_repeat(ok), "{ok}");
        }
        for bad in ["", "d", "0d", "01w", "123d", "1h", "w1", "weekly"] {
            assert!(!valid_repeat(bad), "{bad}");
        }
    }

    #[test]
    fn repeating_task_moves_on_with_steps_unticked() {
        let d = doc();
        let bins = add_task(&d, None, "bins out").unwrap();
        let step = add_task(&d, Some(&bins), "recycling").unwrap();
        set_reminder(&d, &bins, Some(1_000)).unwrap();
        set_repeat(&d, &bins, Some("1w")).unwrap();
        assert!(set_repeat(&d, &bins, Some("weekly")).is_err());
        set_done(&d, &step, true).unwrap();
        repeat_done(&d, &bins, 2_000).unwrap();
        let v = tasks(&d);
        assert_eq!((v[0].done, v[0].remind_ms, v[0].repeat.as_deref()), (false, Some(2_000), Some("1w")));
        assert!(!v[0].children[0].done);
        set_repeat(&d, &bins, None).unwrap();
        assert_eq!(tasks(&d)[0].repeat, None);
    }

    #[test]
    fn my_day_is_each_persons_own_and_merges() {
        let base = doc();
        let a = add_task(&base, None, "call mum").unwrap();
        let other = LoroDoc::new();
        other.import(&base.export(ExportMode::Snapshot).unwrap()).unwrap();
        other.set_peer_id(7).unwrap();
        set_my_day(&base, &a, "aa11", Some("2026-11-19")).unwrap();
        set_my_day(&other, &a, "bb22", Some("2026-11-20")).unwrap();
        base.import(&other.export(ExportMode::Snapshot).unwrap()).unwrap();
        let mut days = tasks(&base)[0].my_day.clone();
        days.sort();
        assert_eq!(days, vec![("aa11".into(), "2026-11-19".into()), ("bb22".into(), "2026-11-20".into())]);
        assert!(set_my_day(&base, &a, "aa11", Some("tomorrow")).is_err());
        assert!(set_my_day(&base, &a, "a:b", Some("2026-11-19")).is_err());
        set_my_day(&base, &a, "aa11", None).unwrap();
        assert_eq!(tasks(&base)[0].my_day, vec![("bb22".to_string(), "2026-11-20".to_string())]);
        let copy = rebuild(&base, 9).unwrap();
        assert_eq!(tasks(&copy)[0].my_day.len(), 1, "kept by a rebuild");
    }

    #[test]
    fn assignee_is_shared_one_person_and_survives_a_rebuild() {
        let base = doc();
        let a = add_task(&base, None, "call mum").unwrap();
        let (ana, beto) = (vec!["a".repeat(64), "b".repeat(64)], vec!["c".repeat(64)]);
        set_assignee(&base, &a, Some((&ana, "Ana"))).unwrap();
        assert_eq!(tasks(&base)[0].assignee, Some((ana.clone(), "Ana".to_string())));
        set_assignee(&base, &a, Some((&beto, "Beto"))).unwrap();
        assert_eq!(tasks(&base)[0].assignee, Some((beto.clone(), "Beto".to_string())), "one person at a time");
        assert!(set_assignee(&base, &a, Some((&[], "x"))).is_err());
        assert!(set_assignee(&base, &a, Some((&["zz".to_string()], "x"))).is_err());
        let copy = rebuild(&base, 9).unwrap();
        assert_eq!(tasks(&copy)[0].assignee.as_ref().map(|x| x.1.as_str()), Some("Beto"), "kept by a rebuild");
        set_assignee(&base, &a, None).unwrap();
        assert_eq!(tasks(&base)[0].assignee, None);
    }

    #[test]
    fn star_is_shared_and_survives_a_rebase() {
        let base = doc();
        let a = add_task(&base, None, "call mum").unwrap();
        set_star(&base, &a, true).unwrap();
        assert!(tasks(&base)[0].starred_ms.is_some());
        let local = LoroDoc::new();
        local.import(&base.export(ExportMode::Snapshot).unwrap()).unwrap();
        local.set_peer_id(3).unwrap();
        let b = add_task(&local, None, "buy stamps").unwrap();
        set_star(&local, &b, true).unwrap();
        set_star(&local, &a, false).unwrap();
        let target = rebuild(&base, 2).unwrap();
        rebase(&base, &local, &target).unwrap();
        let v = tasks(&target);
        let star = |t: &str| v.iter().find(|x| x.title == t).unwrap().starred_ms.is_some();
        assert!(!star("call mum") && star("buy stamps"));
    }

    #[test]
    fn rebase_carries_reminder_and_repeat() {
        // A device changed a reminder and a repeat while a document epoch was being made.
        let base = doc();
        let a = add_task(&base, None, "pay rent").unwrap();
        let b = add_task(&base, None, "water plants").unwrap();
        set_reminder(&base, &b, Some(5)).unwrap();
        let local = LoroDoc::new();
        local.import(&base.export(ExportMode::Snapshot).unwrap()).unwrap();
        local.set_peer_id(3).unwrap();
        set_reminder(&local, &a, Some(7)).unwrap();
        set_repeat(&local, &a, Some("1m")).unwrap();
        set_reminder(&local, &b, None).unwrap();
        let target = rebuild(&base, 2).unwrap();
        rebase(&base, &local, &target).unwrap();
        let v = tasks(&target);
        assert_eq!((v[0].remind_ms, v[0].repeat.as_deref()), (Some(7), Some("1m")));
        assert_eq!(v[1].remind_ms, None);
    }

    #[test]
    fn crud_and_nesting() {
        let d = doc();
        let milk = add_task(&d, None, "milk").unwrap();
        let bread = add_task(&d, None, "bread").unwrap();
        let wg = add_task(&d, Some(&bread), "whole grain").unwrap();
        set_done(&d, &milk, true).unwrap();
        set_title(&d, &bread, "bread!").unwrap();
        let v = tasks(&d);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].title, "milk");
        assert!(v[0].done);
        assert_eq!(v[1].children[0].id, wg);
        delete_task(&d, &milk).unwrap();
        assert_eq!(tasks(&d).len(), 1);
        assert_eq!(list_name(&d), "Groceries");
    }

    #[test]
    fn rebuild_keeps_state_drops_tombstones() {
        let d = doc();
        for i in 0..300 {
            let id = add_task(&d, None, &format!("task {i}")).unwrap();
            if i % 10 != 0 {
                delete_task(&d, &id).unwrap();
            }
        }
        let fresh = rebuild(&d, 2).unwrap();
        assert_eq!(normalized(&fresh), normalized(&d));
        let old = d.export(ExportMode::Snapshot).unwrap().len();
        let new = fresh.export(ExportMode::Snapshot).unwrap().len();
        assert!(new * 3 < old, "rebuild should shrink the document: {old} -> {new}");
    }

    #[test]
    fn compaction_keeps_every_map() {
        // A group document: meta, a list index and people (not just a task tree).
        let d = doc();
        d.get_map("meta").insert("kind", "group").unwrap();
        d.get_map("lists").insert("aa11", true).unwrap();
        d.get_map("lists").insert("bb22", true).unwrap();
        d.get_map("people").insert("dev1", "Alex").unwrap();
        d.commit();
        let fresh = rebuild(&d, 9).unwrap();
        assert_eq!(normalized(&fresh), normalized(&d), "nothing dropped");
        assert_eq!(fresh.get_map("people").get_deep_value(), d.get_map("people").get_deep_value());
        // Same content, other insertion order: equal.
        let other = doc();
        other.get_map("people").insert("dev1", "Alex").unwrap();
        other.get_map("lists").insert("bb22", true).unwrap();
        other.get_map("lists").insert("aa11", true).unwrap();
        other.get_map("meta").insert("kind", "group").unwrap();
        other.commit();
        assert_eq!(normalized(&other), normalized(&d));
        // A dropped map doesn't verify.
        let lossy = doc();
        lossy.get_map("meta").insert("kind", "group").unwrap();
        lossy.commit();
        assert_ne!(normalized(&lossy), normalized(&d));
        // Rebase carries map edits made before the switch.
        let local = d.fork();
        local.set_peer_id(3).unwrap();
        local.get_map("people").insert("dev2", "Sam").unwrap();
        local.get_map("lists").delete("bb22").unwrap();
        local.commit();
        let target = rebuild(&d, 2).unwrap();
        target.set_peer_id(3).unwrap();
        rebase(&d, &local, &target).unwrap();
        assert_eq!(normalized(&target), normalized(&local));
    }

    #[test]
    fn rebase_applies_local_changes() {
        let base = doc();
        let a = add_task(&base, None, "a").unwrap();
        let b = add_task(&base, None, "b").unwrap();
        let local = base.fork();
        local.set_peer_id(3).unwrap();
        set_done(&local, &a, true).unwrap();
        delete_task(&local, &b).unwrap();
        let c = add_task(&local, None, "c").unwrap();
        add_task(&local, Some(&c), "c-child").unwrap();
        let target = rebuild(&base, 2).unwrap();
        target.set_peer_id(3).unwrap();
        rebase(&base, &local, &target).unwrap();
        assert_eq!(normalized(&target), normalized(&local));
    }
}

/// Document semantics for the engine's document epochs (D24).
pub struct TaskSemantics;

impl kt_engine::replica::DocSemantics for TaskSemantics {
    fn normalized(&self, doc: &LoroDoc) -> Vec<u8> {
        normalized(doc)
    }
    fn rebuild(&self, doc: &LoroDoc, peer: u64) -> kt_engine::Result<LoroDoc> {
        rebuild(doc, peer).map_err(|e| kt_engine::Error::Invalid(e.to_string()))
    }
    fn rebase(&self, base: &LoroDoc, local: &LoroDoc, target: &LoroDoc) -> kt_engine::Result<()> {
        rebase(base, local, target).map_err(|e| kt_engine::Error::Invalid(e.to_string()))
    }
    fn kept_authors(&self, before: &LoroDoc, prior: &kt_engine::replica::Authors) -> Vec<u8> {
        budget::kept_authors(before, prior)
    }
}
