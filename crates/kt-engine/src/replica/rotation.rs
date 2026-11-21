//! Periodic key rotation (D5 rule 7): a group's key rotates about every 30 days, which
//! heals silent leaks (a lost backup, a copied key) without anyone doing anything.
//!
//! One member rotates, decided without talking: members are ranked by a hash of the
//! current epoch and their device, the first rotates once the epoch is
//! [`DEFAULT_ROTATE_AFTER_MS`] old, the k-th after k × [`ROTATE_CASCADE_MS`] more (someone
//! away). A double rotation is harmless: concurrent rotations merge (rule 2). Additions
//! already create a fresh key, so "rotate on every device add" needs nothing extra.
//! Periodic rotations don't close open invites (only removals do): invites expire within
//! the same 30 days anyway.

use super::Replica;
use crate::crypto;
use crate::ids::GroupId;
use crate::Result;

pub const DEFAULT_ROTATE_AFTER_MS: u64 = 30 * 86_400_000;
/// Delay between successive members' turns.
pub const ROTATE_CASCADE_MS: u64 = 6 * 3_600_000;

impl Replica {
    /// Change the rotation period (tests; `u64::MAX` turns periodic rotation off).
    pub fn set_rotation_interval_ms(&mut self, ms: u64) {
        self.rotate_after_ms = ms;
    }

    /// Rotate every group whose key is due, if it's this device's turn. Returns the groups
    /// rotated. Called by background sync.
    pub fn auto_rotate(&mut self) -> Result<Vec<GroupId>> {
        let now = super::now_ms();
        let me = self.identity.id();
        let mut due = Vec::new();
        for (g, state) in &self.groups {
            let Some(head) = state.epochs.head() else { continue }; // a fork: the merge comes first
            if !state.epochs.is_current_member(&me) {
                continue;
            }
            let Some(rec) = state.epochs.record(&head) else { continue };
            let Some(recipients) = state.epochs.recipients(&head) else { continue };
            let mut order: Vec<[u8; 32]> = recipients.keys().map(|d| crypto::hash("kt/rotate-turn/v1", &[head.0.as_slice(), d.0.as_slice()].concat())).collect();
            order.sort();
            let mine = crypto::hash("kt/rotate-turn/v1", &[head.0.as_slice(), me.0.as_slice()].concat());
            let rank = order.iter().position(|h| *h == mine).unwrap_or(0) as u64;
            let wait = self.rotate_after_ms.saturating_add(rank * ROTATE_CASCADE_MS);
            // Someone else removed a device and nobody but the remover has built on it yet: the first other member
            // confirms it now, so the removed device can't undo it from an older branch (epochs.rs, T2).
            if now.saturating_sub(rec.body.created_ms) >= wait || state.epochs.removal_to_confirm(&me) {
                due.push(*g);
            }
        }
        for g in &due {
            self.rotate(g)?;
        }
        Ok(due)
    }
}
