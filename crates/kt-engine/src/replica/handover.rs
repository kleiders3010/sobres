//! Handing a group over to another device of the same account (device linking, D6).
//!
//! A device that adds a linked device to one of the account's groups also puts the
//! group's epoch records into its slot of the *account* group (the personal group, which
//! only the account's own devices read). The linked device can't find the group's slots
//! on its own (it knows no key yet); with the records it unwraps its key and reads the
//! group like any member. A handover stays in the slot until the device's status shows up
//! in that group, for at most [`HANDOVER_MS`].

use serde::{Deserialize, Serialize};

use super::Replica;
use crate::crypto::{self, DevicePublic};
use crate::epochs::EpochRecord;
use crate::ids::{DeviceId, GroupId};
use crate::{wire, Error, Result};

const TAG: [u8; 2] = *b"HO";
pub const HANDOVER_MS: u64 = 14 * 86_400_000;

#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    via: GroupId,
    group: GroupId,
    to: DeviceId,
    created_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct Body {
    via: GroupId,
    group: GroupId,
    to: DeviceId,
    records: Vec<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
struct Signed {
    body: Vec<u8>,
    author: DeviceId,
    sig: Vec<u8>,
}

impl Replica {
    fn handovers_path(&self) -> std::path::PathBuf {
        self.layout.root().join("handovers")
    }

    fn handovers(&self) -> Vec<Pending> {
        std::fs::read(self.handovers_path()).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default()
    }

    fn save_handovers(&self, all: &[Pending]) -> Result<()> {
        let b = postcard::to_stdvec(all).map_err(|e| Error::Encoding(e.to_string()))?;
        crate::storage::write_atomic(&self.handovers_path(), &b)
    }

    /// A current member's public keys, from the group's epoch records.
    pub fn member_public(&self, g: &GroupId, d: &DeviceId) -> Option<DevicePublic> {
        let state = self.groups.get(g)?;
        let head = state.epochs.head()?;
        let kex = *state.epochs.recipients(&head)?.get(d)?;
        Some(DevicePublic { sign: d.0, kex })
    }

    /// Current members of `g` (by the head epoch's recipients).
    pub fn members(&self, g: &GroupId) -> Vec<DeviceId> {
        self.groups
            .get(g)
            .and_then(|s| s.epochs.head().and_then(|h| s.epochs.recipients(&h).map(|r| r.keys().copied().collect())))
            .unwrap_or_default()
    }

    /// Add `device` (a current member of the account group `via`) to `group` and hand the
    /// group over to it through `via`'s slots.
    pub fn add_and_hand_over(&mut self, via: &GroupId, group: &GroupId, device: &DeviceId) -> Result<()> {
        let public = self.member_public(via, device).ok_or_else(|| Error::NotFound("device in the account group".into()))?;
        if !self.members(group).contains(device) {
            self.add_member(group, public)?;
        }
        let mut all = self.live_handovers();
        all.retain(|p| !(p.group == *group && p.to == *device));
        all.push(Pending { via: *via, group: *group, to: *device, created_ms: super::now_ms() });
        self.save_handovers(&all)
    }

    /// Handovers still needed: the device hasn't shown up in the group yet.
    fn live_handovers(&self) -> Vec<Pending> {
        let now = super::now_ms();
        self.handovers()
            .into_iter()
            .filter(|p| now < p.created_ms + HANDOVER_MS && !self.member_status(&p.group).contains_key(&p.to) && self.members(&p.group).contains(&p.to))
            .collect()
    }

    /// Handover records for `via`'s slot.
    pub(super) fn handover_messages(&self, via: &GroupId) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        for p in self.live_handovers().iter().filter(|p| p.via == *via) {
            let records: Vec<Vec<u8>> = self.groups[&p.group].epochs.all_records().map(|r| r.encode()).collect::<Result<_>>()?;
            let body = wire::encode(TAG, 1, &Body { via: p.via, group: p.group, to: p.to, records })?;
            let sig = self.identity.sign(&body).to_vec();
            out.push(postcard::to_stdvec(&Signed { body, author: self.identity.id(), sig }).map_err(|e| Error::Encoding(e.to_string()))?);
        }
        Ok(out)
    }

    /// A handover from another device of the account: take the group in.
    pub(super) fn receive_handover(&mut self, bytes: &[u8]) -> Result<()> {
        let s: Signed = postcard::from_bytes(bytes).map_err(|e| Error::Encoding(e.to_string()))?;
        let sig: [u8; 64] = s.sig.as_slice().try_into().map_err(|_| Error::Crypto("bad signature length"))?;
        crypto::verify(&s.author.0, &s.body, &sig)?;
        let body: Body = wire::decode(TAG, 1, &s.body)?.1;
        if body.to != self.identity.id() {
            return Ok(()); // for another device of the account
        }
        let via = self.groups.get(&body.via).ok_or_else(|| Error::NotFound("account group".into()))?;
        if !via.epochs.is_current_member(&s.author) || !via.epochs.is_current_member(&body.to) {
            return Err(Error::Rejected("handover from outside the account".into()));
        }
        if self.groups.get(&body.group).is_some_and(|g| g.epochs.is_current_member(&body.to)) {
            return Ok(()); // already in
        }
        self.accept_group(body.group);
        for r in body.records {
            if EpochRecord::decode(&r).is_ok_and(|rec| rec.body.group == body.group) {
                let _ = self.receive(&super::Message::Epoch(r).encode()?);
            }
        }
        Ok(())
    }
}
