//! Whereabouts beyond slots (DESIGN §6 discovery): records a transport can publish to
//! public lookup systems (the Mainline DHT) without revealing anything to non-members.
//!
//! - Record key: derived from the group's current epoch key and the device ID, so only
//!   members can compute where to look, and the key rotates with every key epoch.
//! - Value: the device-signed whereabouts record, sealed with an epoch-derived key. Other
//!   members could overwrite a record (they can derive the key) but can't forge its content.

use super::Replica;
use crate::crypto::{self, SymKey};
use crate::ids::{DeviceId, EpochId, GroupId};
use crate::{Error, Result};

/// LAN beacon tags rotate every this many seconds (D57: no stable identifiers on the LAN).
pub const LAN_WINDOW_SECS: u64 = 600;

/// Max sealed value size (BEP44 mutable items hold up to 1000 bytes).
pub const DHT_VALUE_MAX: usize = 1000;

/// A record this device should publish.
#[derive(Clone, Debug)]
pub struct DhtPublish {
    pub group: GroupId,
    /// Seed of the record's signing key (Ed25519), shared by the group's members.
    pub key_seed: [u8; 32],
    pub value: Vec<u8>,
}

/// A member record this device should look up.
#[derive(Clone, Debug)]
pub struct DhtLookup {
    pub group: GroupId,
    pub device: DeviceId,
    pub epoch: EpochId,
    pub key_seed: [u8; 32],
}

fn record_seed(epoch_key: &SymKey, epoch: &EpochId, device: &DeviceId) -> [u8; 32] {
    let mut info = Vec::with_capacity(64);
    info.extend_from_slice(&epoch.0);
    info.extend_from_slice(&device.0);
    crypto::derive(&epoch_key.0, "kt/dht-record/v1", &info).0
}

fn value_key(epoch_key: &SymKey, epoch: &EpochId) -> SymKey {
    crypto::derive(&epoch_key.0, "kt/dht-value/v1", &epoch.0)
}

fn value_aad(group_epoch: &EpochId, device: &DeviceId) -> Vec<u8> {
    let mut v = b"kt/dht/v1".to_vec();
    v.extend_from_slice(&group_epoch.0);
    v.extend_from_slice(&device.0);
    v
}

impl Replica {
    /// Our own records to publish (one per group we're a current member of).
    pub fn dht_publish(&self) -> Vec<DhtPublish> {
        let me = self.identity.id();
        let mut out = Vec::new();
        for (g, state) in &self.groups {
            let Some(head) = state.epochs.working_head() else { continue };
            // Nobody to find us in a group of one (a newly linked device makes it two).
            if !state.epochs.is_current_member(&me) || state.epochs.recipients(&head).is_none_or(|r| r.len() < 2) {
                continue;
            }
            let (Some(key), Ok(Some(rec))) = (state.keys.get(&head), self.whereabouts_record(g)) else { continue };
            let value = crypto::seal(&value_key(key, &head), &value_aad(&head, &me), &rec);
            if value.len() <= DHT_VALUE_MAX {
                out.push(DhtPublish { group: *g, key_seed: record_seed(key, &head, &me), value });
            }
        }
        out
    }

    /// Records of the other current members to look up.
    pub fn dht_lookups(&self) -> Vec<DhtLookup> {
        let me = self.identity.id();
        let mut out = Vec::new();
        for (g, state) in &self.groups {
            let Some(head) = state.epochs.working_head() else { continue };
            let (Some(key), Some(recipients)) = (state.keys.get(&head), state.epochs.recipients(&head)) else { continue };
            for d in recipients.keys().filter(|d| **d != me) {
                out.push(DhtLookup { group: *g, device: *d, epoch: head, key_seed: record_seed(key, &head, d) });
            }
        }
        out
    }

    /// Apply a value found for a lookup (untrusted: decrypted, then the inner record must
    /// be signed by that member device).
    pub fn open_dht_value(&mut self, l: &DhtLookup, value: &[u8]) -> Result<()> {
        let key = self
            .groups
            .get(&l.group)
            .and_then(|s| s.keys.get(&l.epoch))
            .cloned()
            .ok_or_else(|| Error::NotFound("epoch key".into()))?;
        let rec = crypto::open(&value_key(&key, &l.epoch), &value_aad(&l.epoch, &l.device), value)?;
        if !self.whereabouts_signed_by(&rec, &l.device) {
            return Err(Error::Rejected("whereabouts record not from that device".into()));
        }
        self.receive_whereabouts(&rec)
    }
}

fn lan_tag(epoch_key: &SymKey, epoch: &EpochId, window: u64) -> [u8; 16] {
    let mut m = Vec::with_capacity(72);
    m.extend_from_slice(&epoch_key.0);
    m.extend_from_slice(&epoch.0);
    m.extend_from_slice(&window.to_le_bytes());
    crypto::hash("kt/lan-tag/v1", &m)[..16].try_into().expect("16")
}

fn lan_key(epoch_key: &SymKey, epoch: &EpochId) -> SymKey {
    crypto::derive(&epoch_key.0, "kt/lan-value/v1", &epoch.0)
}

fn lan_aad(tag: &[u8; 16]) -> Vec<u8> {
    let mut v = b"kt/lan/v1".to_vec();
    v.extend_from_slice(tag);
    v
}

impl Replica {
    /// Entries for a LAN beacon in time window `window` (unix seconds / LAN_WINDOW_SECS):
    /// (rotating tag, sealed device-signed whereabouts), one per group with other members.
    pub fn lan_beacon_entries(&self, window: u64) -> Vec<([u8; 16], Vec<u8>)> {
        self.whereabouts_entries(window).into_iter().map(|(_, _, t, v)| (t, v)).collect()
    }

    /// Like [`Replica::lan_beacon_entries`], with each entry's group and current head topic
    /// (for sending an entry as a Nostr knock under that topic).
    pub fn whereabouts_entries(&self, window: u64) -> Vec<(GroupId, [u8; 32], [u8; 16], Vec<u8>)> {
        let me = self.identity.id();
        let mut out = Vec::new();
        for (g, state) in &self.groups {
            let Some(head) = state.epochs.working_head() else { continue };
            if !state.epochs.is_current_member(&me) || state.epochs.recipients(&head).is_none_or(|r| r.len() < 2) {
                continue;
            }
            let (Some(key), Ok(Some(rec))) = (state.keys.get(&head), self.whereabouts_record(g)) else { continue };
            let tag = lan_tag(key, &head, window);
            out.push((*g, super::topic_for(key, &head), tag, crypto::seal(&lan_key(key, &head), &lan_aad(&tag), &rec)));
        }
        out
    }

    /// Try one beacon entry (untrusted). Accepts tags of the current and previous window
    /// (clock skew, window edges). Returns true if it was ours to open and was applied.
    pub fn open_lan_entry(&mut self, window: u64, tag: &[u8; 16], sealed: &[u8]) -> bool {
        let me = self.identity.id();
        let mut hit: Option<(GroupId, SymKey, EpochId)> = None;
        'outer: for (g, state) in &self.groups {
            let Some(head) = state.epochs.working_head() else { continue };
            let Some(key) = state.keys.get(&head) else { continue };
            for w in [window, window.saturating_sub(1), window + 1] {
                if lan_tag(key, &head, w) == *tag {
                    hit = Some((*g, key.clone(), head));
                    break 'outer;
                }
            }
        }
        let Some((_, key, head)) = hit else { return false };
        let Ok(rec) = crypto::open(&lan_key(&key, &head), &lan_aad(tag), sealed) else { return false };
        // Must be a current member's own signed record (and not ours echoed back).
        let Some(device) = self.whereabouts_device(&rec) else { return false };
        if device == me || !self.whereabouts_signed_by(&rec, &device) {
            return false;
        }
        self.receive_whereabouts(&rec).is_ok()
    }
}

impl Replica {
    /// A secret-keyed, daily-rotating point for `g` (HMAC-like hash of the current epoch
    /// key and the day): new helper candidates are preferred by closeness to it, so
    /// outsiders can't pre-position fake helpers for a group (§7a).
    pub fn group_point(&self, g: &GroupId, day: u32) -> Option<[u8; 32]> {
        let state = self.groups.get(g)?;
        // Members: the working head's key. A joiner without the epoch records yet: the key
        // its invite carried (the same epoch, unless the group rotated since), or with a
        // door-only invite (T10) the point its record carried for that day.
        let key = match state.epochs.working_head().and_then(|h| state.keys.get(&h)) {
            Some(k) => k,
            None => {
                let p = self.pending_joins().into_iter().find(|p| p.group == *g)?;
                match state.keys.get(&p.epoch) {
                    Some(k) => k,
                    None => return std::fs::read(self.layout.group_dir(g).join("join_point")).ok()?.try_into().ok(),
                }
            }
        };
        let mut m = key.0.to_vec();
        m.extend_from_slice(&day.to_le_bytes());
        Some(crypto::hash("kt/helper-point/v1", &m))
    }
}
