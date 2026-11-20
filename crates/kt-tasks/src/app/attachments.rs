//! Text attachments (D4): notes above the threshold are sealed into a blob held in
//! `data/blobs`, and the task's notes field points at it. The network layer fetches the
//! blobs this device lacks ([`App::wanted_blobs`]) and serves the ones it has.

use std::collections::HashSet;

use kt_engine::blobs::{self, BlobId, BlobStore};
use kt_engine::limits::TEXT_ATTACHMENT_THRESHOLD;

use super::{eng, App, ListInfo};
use crate::{Error, Notes, Result};

/// Unreferenced blobs are kept this long: undo and history can bring a reference back.
pub const BLOB_KEEP_MS: u64 = 30 * 86_400_000;
/// Largest image accepted (the apps shrink photos well below this first).
pub const MAX_IMAGE_BYTES: usize = 3 * 1024 * 1024;
/// Attachment bytes a backup carries at most (Android backup allows 25 MB in all).
pub const BACKUP_BLOB_BYTES: usize = 16 * 1024 * 1024;

/// A task's full notes, as far as this device has them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotesText {
    pub text: String,
    /// False while an attachment hasn't arrived yet: `text` is then its preview.
    pub complete: bool,
    /// The whole notes' size in bytes.
    pub size: u64,
}

impl App {
    pub fn blob_store(&self) -> BlobStore {
        BlobStore::new(self.data_dir.join("blobs"))
    }

    /// Set a task's notes; long ones become a text attachment.
    pub fn set_notes(&self, list: &ListInfo, task: &str, text: &str) -> Result<()> {
        let value = if text.len() > TEXT_ATTACHMENT_THRESHOLD {
            let (r, sealed) = blobs::seal(text.as_bytes()).map_err(eng)?;
            self.blob_store().put(&sealed).map_err(eng)?;
            crate::attachment_notes(&r, text)
        } else {
            text.to_string()
        };
        self.edit(list, |d| crate::set_notes(d, task, &value))
    }

    /// A task's full notes, opening its attachment if it's here.
    pub fn notes(&self, list: &ListInfo, task: &str) -> Result<NotesText> {
        let raw = self.read(list, |d| crate::find_view(d, task))?.ok_or(Error::NotFound)?.notes;
        Ok(match crate::notes_of(&raw) {
            Notes::Inline(t) => NotesText { text: t.to_string(), complete: true, size: t.len() as u64 },
            Notes::Attachment { blob, preview } => {
                let full = self.blob_store().get(&blob.id).and_then(|b| blobs::open(&blob, &b).ok()).and_then(|v| String::from_utf8(v).ok());
                match full {
                    Some(text) => NotesText { text, complete: true, size: blob.size },
                    None => NotesText { text: preview.to_string(), complete: false, size: blob.size },
                }
            }
        })
    }

    /// The sealed attachments of these lists that are here, up to `max_bytes` in all.
    pub(crate) fn blobs_for(&self, lists: &[ListInfo], max_bytes: usize) -> Vec<Vec<u8>> {
        let store = self.blob_store();
        let mut ids: Vec<BlobId> = self.blobs_of(lists).into_iter().collect();
        ids.sort();
        let mut out = Vec::new();
        let mut total = 0;
        for id in ids {
            if let Some(b) = store.get(&id) {
                if total + b.len() <= max_bytes {
                    total += b.len();
                    out.push(b);
                }
            }
        }
        out
    }

    /// Attach an image (already compressed by the app, e.g. JPEG) to a task.
    pub fn add_image(&self, list: &ListInfo, task: &str, bytes: &[u8]) -> Result<blobs::BlobRef> {
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(Error::Invalid(format!("images up to {} MB", MAX_IMAGE_BYTES / (1024 * 1024))));
        }
        let (r, sealed) = blobs::seal(bytes).map_err(eng)?;
        self.blob_store().put(&sealed).map_err(eng)?;
        self.edit(list, |d| crate::add_image(d, task, &r))?;
        Ok(r)
    }

    /// An image's bytes, if this device has it yet (it may still be on its way).
    pub fn image(&self, list: &ListInfo, id: &BlobId) -> Result<Option<Vec<u8>>> {
        let r = self.read(list, |d| {
            fn find(ts: &[crate::TaskView], id: &BlobId) -> Option<blobs::BlobRef> {
                ts.iter().find_map(|t| t.images.iter().find(|r| &r.id == id).cloned().or_else(|| find(&t.children, id)))
            }
            find(&crate::tasks(d), id)
        })?;
        let Some(r) = r else { return Err(Error::NotFound) };
        Ok(self.blob_store().get(&r.id).and_then(|b| blobs::open(&r, &b).ok()))
    }

    /// Every blob the lists on this device refer to.
    pub fn referenced_blobs(&self) -> HashSet<BlobId> {
        self.blobs_of(&self.lists())
    }

    fn blobs_of(&self, lists: &[ListInfo]) -> HashSet<BlobId> {
        let mut out = HashSet::new();
        for l in lists {
            let _ = self.read(l, |d| {
                fn walk(ts: &[crate::TaskView], out: &mut HashSet<BlobId>) {
                    for t in ts {
                        if let Some(b) = t.attachment() {
                            out.insert(b.id);
                        }
                        out.extend(t.images.iter().map(|r| r.id));
                        walk(&t.children, out);
                    }
                }
                walk(&crate::tasks(d), &mut out);
            });
        }
        out
    }

    /// Blobs the lists refer to that this device doesn't have yet.
    pub fn wanted_blobs(&self) -> Vec<BlobId> {
        let store = self.blob_store();
        let mut v: Vec<BlobId> = self.referenced_blobs().into_iter().filter(|id| !store.has(id)).collect();
        v.sort();
        v
    }

    /// Delete blobs no list has referred to for [`BLOB_KEEP_MS`].
    pub fn collect_blob_garbage(&self) -> usize {
        self.blob_store().collect_garbage(&self.referenced_blobs(), BLOB_KEEP_MS)
    }
}
