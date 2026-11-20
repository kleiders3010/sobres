//! Automatic document epochs (D24, D26): compact a list when its history outgrows its live
//! content, only at a version every recently active member has acknowledged.
//!
//! - **When:** the document's snapshot exceeds [`CompactionPolicy::min_bytes`] and is more
//!   than [`CompactionPolicy::ratio`] × its rebuilt live state.
//! - **Acknowledged (D26):** every member whose status arrived within
//!   [`CompactionPolicy::active_ms`] has everything this device has of the document. Members
//!   away longer rebase their edits onto the new generation when they return.
//! - **Who:** the lowest-ID active member (a concurrent proposal from another member is
//!   resolved by the existing tie-break anyway).
//! - Only while the key graph has converged and not right after a switch.

use loro::{ExportMode, VersionVector};

use super::Replica;
use crate::ids::{DocId, GroupId};
use crate::Result;

#[derive(Clone, Debug)]
pub struct CompactionPolicy {
    pub min_bytes: usize,
    pub ratio: usize,
    pub active_ms: u64,
    /// Don't compact again within this long of a switch.
    pub cooldown_ms: u64,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        CompactionPolicy { min_bytes: 64 * 1024, ratio: 3, active_ms: 30 * 24 * 3600 * 1000, cooldown_ms: 24 * 3600 * 1000 }
    }
}

impl Replica {
    /// Tune compaction (tests use small thresholds).
    pub fn set_compaction_policy(&mut self, p: CompactionPolicy) {
        self.compaction = p;
    }

    /// Tune how often acknowledgments are republished on their own (tests use 0).
    pub fn set_ack_interval_ms(&mut self, ms: u64) {
        self.ack_interval_ms = ms;
    }

    /// Propose document epochs where due. Returns the documents compacted.
    pub fn auto_compact(&mut self) -> Result<Vec<(GroupId, DocId)>> {
        if self.semantics.is_none() {
            return Ok(Vec::new());
        }
        let now = super::now_ms();
        let me = self.identity.id();
        let mut due = Vec::new();
        for (g, state) in &self.groups {
            let Some(head) = state.epochs.head() else { continue }; // converged only
            if !state.epochs.is_current_member(&me) {
                continue;
            }
            let statuses = self.member_status(g);
            let members: Vec<_> = state.epochs.recipients(&head).map(|r| r.keys().copied().collect()).unwrap_or_default();
            let active: Vec<_> = members
                .iter()
                .filter(|m| **m != me && statuses.get(m).is_some_and(|s| now.saturating_sub(s.created_ms) < self.compaction.active_ms))
                .copied()
                .collect();
            if active.iter().any(|m| *m < me) {
                continue; // the lowest-ID active member proposes
            }
            for (d, ds) in &state.docs {
                if self.generation_switched_recently(g, d, now) {
                    continue;
                }
                let Ok(snap) = ds.doc.export(ExportMode::Snapshot) else { continue };
                if snap.len() < self.compaction.min_bytes {
                    continue;
                }
                let sem = self.semantics.as_ref().expect("checked");
                let Ok(live) = sem.rebuild(&ds.doc, self.identity.peer_id()).and_then(|x| x.export(ExportMode::Snapshot).map_err(|e| crate::Error::Loro(e.to_string()))) else { continue };
                if snap.len() <= live.len() * self.compaction.ratio {
                    continue;
                }
                let mine = ds.doc.oplog_vv();
                let all_acked = active.iter().all(|m| {
                    statuses.get(m).and_then(|s| s.acks.get(d)).is_some_and(|(gen_, vv)| {
                        *gen_ == ds.generation && VersionVector::decode(vv).is_ok_and(|v| mine.iter().all(|(p, c)| v.get(p).copied().unwrap_or(0) >= *c))
                    })
                });
                if all_acked {
                    due.push((*g, *d));
                }
            }
        }
        let mut done = Vec::new();
        for (g, d) in due {
            if self.propose_doc_epoch(&g, &d).is_ok() {
                let _ = crate::storage::write_atomic(&self.layout.doc_dir(&g, &d).join("switched_at"), &now.to_le_bytes());
                done.push((g, d));
            }
        }
        Ok(done)
    }

    fn generation_switched_recently(&self, g: &GroupId, d: &DocId, now: u64) -> bool {
        std::fs::read(self.layout.doc_dir(g, d).join("switched_at"))
            .ok()
            .and_then(|b| b.try_into().ok().map(u64::from_le_bytes))
            .is_some_and(|t| now.saturating_sub(t) < self.compaction.cooldown_ms)
    }
}
