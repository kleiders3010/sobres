//! Data from newer app versions (principle 8): an older device can't read it, but it
//! keeps it and passes it on untouched in its own slots, so a group's newer devices still
//! get everything even when an older device is the one carrying it. Seeing it also tells
//! the app to offer the update ("A newer version is available to use certain features").
//! The feature floor keeps new kinds of content unused until every device supports them.

use serde::{Deserialize, Serialize};

use super::{Replica, FEATURE_LEVEL};
use crate::ids::GroupId;
use crate::{Error, Result};

const MAX_KEPT: usize = 64;
const MAX_KEPT_BYTES: usize = 256 * 1024;
const KEEP_MS: u64 = 30 * 86_400_000;

#[derive(Serialize, Deserialize)]
struct Kept {
    at_ms: u64,
    bytes: Vec<u8>,
}

impl Replica {
    fn kept_path(&self, g: &GroupId) -> std::path::PathBuf {
        self.layout.group_dir(g).join("newer")
    }

    fn kept(&self, g: &GroupId) -> Vec<Kept> {
        std::fs::read(self.kept_path(g)).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default()
    }

    /// Keep a message this build can't read (bounded, expiring) and remember that newer
    /// data exists.
    pub(super) fn keep_newer(&mut self, g: &GroupId, bytes: &[u8]) -> Result<()> {
        let _ = crate::storage::write_atomic(&self.layout.root().join("newer_seen"), b"1");
        let now = super::now_ms();
        let mut all = self.kept(g);
        if all.iter().any(|k| k.bytes == bytes) {
            return Ok(());
        }
        all.retain(|k| now.saturating_sub(k.at_ms) < KEEP_MS);
        all.push(Kept { at_ms: now, bytes: bytes.to_vec() });
        while all.len() > MAX_KEPT || all.iter().map(|k| k.bytes.len()).sum::<usize>() > MAX_KEPT_BYTES {
            all.remove(0);
        }
        let b = postcard::to_stdvec(&all).map_err(|e| Error::Encoding(e.to_string()))?;
        crate::storage::write_atomic(&self.kept_path(g), &b)
    }

    /// Kept newer messages, passed on in our slots.
    pub(super) fn newer_messages(&self, g: &GroupId) -> Vec<Vec<u8>> {
        let now = super::now_ms();
        self.kept(g).into_iter().filter(|k| now.saturating_sub(k.at_ms) < KEEP_MS).map(|k| k.bytes).collect()
    }

    /// Data from a newer app version reached this device (offer the update).
    pub fn newer_version_seen(&self) -> bool {
        self.layout.root().join("newer_seen").exists()
    }

    /// The group's feature floor: the lowest feature level among its current members'
    /// devices (1 for those that haven't said). New kinds of content wait for it.
    pub fn feature_floor(&self, g: &GroupId) -> u32 {
        let me = self.identity.id();
        let statuses = self.member_status(g);
        self.members(g).iter().filter(|d| **d != me).map(|d| statuses.get(d).map(|s| s.features).unwrap_or(1)).fold(FEATURE_LEVEL, u32::min)
    }
}
