//! Health observations shared within a group (DESIGN §7/§7a): each member device publishes
//! what it has seen of helpers and relays (reliability records, relay health), signed,
//! in the group's slots. Only members can publish; only members can read (sealed slots).
//! The engine doesn't interpret the report: the transport layer builds and uses it.

use serde::{Deserialize, Serialize};

use super::Replica;
use crate::crypto;
use crate::ids::{DeviceId, GroupId};
use crate::{wire, Error, Result};

const TAG: [u8; 2] = *b"HR";
/// Reports go out with the slot's real changes, and at least this often on their own.
pub const HEALTH_INTERVAL_MS: u64 = 86_400_000;

/// A report larger than this is refused (it's a summary, not a log).
pub const MAX_HEALTH: usize = 16 * 1024;

#[derive(Serialize, Deserialize)]
struct Body {
    group: GroupId,
    device: DeviceId,
    created_ms: u64,
    data: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct Signed {
    body: Vec<u8>,
    sig: Vec<u8>,
}

/// One member's latest report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberHealth {
    pub device: DeviceId,
    pub created_ms: u64,
    pub data: Vec<u8>,
}

impl Replica {
    /// What this device reports to its groups (opaque, from the transport layer); `None`
    /// stops reporting.
    pub fn set_health_report(&mut self, data: Option<Vec<u8>>) {
        self.health = data.filter(|d| d.len() <= MAX_HEALTH);
    }

    fn health_path(&self, g: &GroupId) -> std::path::PathBuf {
        self.layout.group_dir(g).join("health")
    }

    /// Other members' latest reports for `g` (verified when received).
    pub fn member_health(&self, g: &GroupId) -> Vec<MemberHealth> {
        std::fs::read(self.health_path(g)).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default()
    }

    pub(super) fn health_record(&self, g: &GroupId) -> Result<Option<Vec<u8>>> {
        let Some(data) = &self.health else { return Ok(None) };
        let body = wire::encode(TAG, 1, &Body { group: *g, device: self.identity.id(), created_ms: super::now_ms(), data: data.clone() })?;
        let sig = self.identity.sign(&body).to_vec();
        Ok(Some(postcard::to_stdvec(&Signed { body, sig }).map_err(|e| Error::Encoding(e.to_string()))?))
    }

    pub(super) fn receive_health(&mut self, bytes: &[u8]) -> Result<()> {
        let s: Signed = postcard::from_bytes(bytes).map_err(|e| Error::Encoding(e.to_string()))?;
        let body: Body = wire::decode(TAG, 1, &s.body)?.1;
        let sig: [u8; 64] = s.sig.as_slice().try_into().map_err(|_| Error::Crypto("bad signature length"))?;
        crypto::verify(&body.device.0, &s.body, &sig)?;
        let state = self.groups.get(&body.group).ok_or_else(|| Error::NotFound("group".into()))?;
        if !state.epochs.is_current_member(&body.device) || body.data.len() > MAX_HEALTH {
            return Err(Error::Rejected("health report from a non-member or too large".into()));
        }
        if body.device == self.identity.id() {
            return Ok(()); // our own, echoed back
        }
        let mut all = self.member_health(&body.group);
        if all.iter().any(|h| h.device == body.device && h.created_ms >= body.created_ms) {
            return Ok(());
        }
        all.retain(|h| h.device != body.device && state.epochs.is_current_member(&h.device));
        all.push(MemberHealth { device: body.device, created_ms: body.created_ms, data: body.data });
        let b = postcard::to_stdvec(&all).map_err(|e| Error::Encoding(e.to_string()))?;
        crate::storage::write_atomic(&self.health_path(&body.group), &b)
    }
}
