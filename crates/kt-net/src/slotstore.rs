//! Local store of the latest slot per (topic, writer) for this device's own groups.
//!
//! Every slot a device reads (from any mailbox or peer) or writes lands here, so member
//! devices can hand each other's slots on: a member device is a mailbox for its groups
//! (DESIGN §6: "member devices" first in the offline order, and "relay through a member").
//! Kept on disk: these are the device's own groups (strangers' slots, for helping, are RAM
//! only and live elsewhere).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::mailbox::SlotRead;

type Key = ([u8; 32], [u8; 32]);

#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    slots: Vec<(Key, u64, Vec<u8>)>,
}

pub struct SlotStore {
    map: Mutex<HashMap<Key, (u64, Vec<u8>)>>,
    path: Option<PathBuf>,
    dirty: Mutex<bool>,
}

/// (topic, writer, version) triples: what a store holds, without the data.
pub type Summary = Vec<([u8; 32], [u8; 32], u64)>;

impl SlotStore {
    pub fn open(path: Option<PathBuf>) -> SlotStore {
        let p: Persisted = path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default();
        SlotStore {
            map: Mutex::new(p.slots.into_iter().map(|(k, v, b)| (k, (v, b))).collect()),
            path,
            dirty: Mutex::new(false),
        }
    }

    /// Store if newer than what we have. Returns true if stored.
    pub fn put(&self, s: &SlotRead) -> bool {
        let mut m = self.map.lock().expect("lock");
        let k = (s.topic, s.writer);
        if m.get(&k).is_some_and(|(v, _)| *v >= s.version) {
            return false;
        }
        m.insert(k, (s.version, s.blob.clone()));
        *self.dirty.lock().expect("lock") = true;
        true
    }

    pub fn summary(&self, topics: &[[u8; 32]]) -> Summary {
        self.map
            .lock()
            .expect("lock")
            .iter()
            .filter(|((t, _), _)| topics.contains(t))
            .map(|((t, w), (v, _))| (*t, *w, *v))
            .collect()
    }

    /// Slots under `topics` that are newer than the given summary.
    pub fn newer_than(&self, topics: &[[u8; 32]], have: &Summary) -> Vec<SlotRead> {
        let have: HashMap<Key, u64> = have.iter().map(|(t, w, v)| ((*t, *w), *v)).collect();
        self.map
            .lock()
            .expect("lock")
            .iter()
            .filter(|((t, _), _)| topics.contains(t))
            .filter(|(k, (v, _))| have.get(*k).is_none_or(|hv| hv < v))
            .map(|((t, w), (v, b))| SlotRead { topic: *t, writer: *w, version: *v, blob: b.clone() })
            .collect()
    }

    /// Drop slots whose topics are no longer read (old epochs, left groups).
    pub fn retain_topics(&self, topics: &[[u8; 32]]) {
        let mut m = self.map.lock().expect("lock");
        let before = m.len();
        m.retain(|(t, _), _| topics.contains(t));
        if m.len() != before {
            *self.dirty.lock().expect("lock") = true;
        }
    }

    pub fn save(&self) {
        let Some(p) = &self.path else { return };
        let mut dirty = self.dirty.lock().expect("lock");
        if !*dirty {
            return;
        }
        let slots = self.map.lock().expect("lock").iter().map(|(k, (v, b))| (*k, *v, b.clone())).collect();
        if let Ok(b) = postcard::to_stdvec(&Persisted { slots }) {
            if kt_engine::storage::write_atomic(p, &b).is_ok() {
                *dirty = false;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.map.lock().expect("lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
