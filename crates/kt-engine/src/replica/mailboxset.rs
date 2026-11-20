//! Each group's chosen endpoints per layer (DESIGN §6: every layer is self-sustaining).
//!
//! The engine stays transport-agnostic: a set is a list of opaque endpoint strings for one
//! layer (`"nostr"`: relay URLs; `"helpers"`: helper endpoint addresses; more later). A member device proposes a set as a signed record carried in its
//! slots; the newest (created time, then author) wins everywhere. The previous set is kept
//! so devices keep *reading* it for a while after a switch and nothing is lost in between.

use serde::{Deserialize, Serialize};

use super::Replica;
use crate::crypto;
use crate::ids::{DeviceId, GroupId};
use crate::{wire, Error, Result};

const SET_TAG: [u8; 2] = *b"MB";
/// Keep reading the previous set this long after a switch.
pub const PREVIOUS_SET_READ_MS: u64 = 14 * 24 * 3600 * 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxSet {
    pub group: GroupId,
    pub layer: String,
    pub endpoints: Vec<String>,
    pub created_ms: u64,
    pub author: DeviceId,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Layer {
    current: Option<(MailboxSet, Vec<u8>)>,
    previous: Option<(MailboxSet, u64)>,
}

type Stored = std::collections::BTreeMap<String, Layer>;

#[derive(Serialize, Deserialize)]
struct Signed {
    body: Vec<u8>,
    sig: Vec<u8>,
}

fn newer(a: &MailboxSet, b: &MailboxSet) -> bool {
    (a.created_ms, a.author) > (b.created_ms, b.author)
}

impl Replica {
    fn set_path(&self, g: &GroupId) -> std::path::PathBuf {
        self.layout.group_dir(g).join("mailbox_set")
    }

    fn stored_set(&self, g: &GroupId) -> Stored {
        std::fs::read(self.set_path(g)).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default()
    }

    /// The group's current set for `layer` and, while still relevant, the previous one.
    pub fn mailbox_set(&self, g: &GroupId, layer: &str) -> (Option<MailboxSet>, Option<MailboxSet>) {
        let s = self.stored_set(g).remove(layer).unwrap_or_default();
        let prev = s.previous.filter(|(_, since)| super::now_ms().saturating_sub(*since) < PREVIOUS_SET_READ_MS).map(|(p, _)| p);
        (s.current.map(|(c, _)| c), prev)
    }

    /// Propose a new set for `g` and `layer` (this device must be a current member).
    pub fn propose_mailbox_set(&mut self, g: &GroupId, layer: &str, endpoints: Vec<String>) -> Result<MailboxSet> {
        let me = self.identity.id();
        let state = self.groups.get(g).ok_or_else(|| Error::NotFound("group".into()))?;
        if !state.epochs.is_current_member(&me) {
            return Err(Error::Rejected("only members can change the group's mailboxes".into()));
        }
        let set = MailboxSet { group: *g, layer: layer.to_string(), endpoints, created_ms: super::now_ms(), author: me };
        let body = wire::encode(SET_TAG, 1, &set)?;
        let sig = self.identity.sign(&body).to_vec();
        let signed = postcard::to_stdvec(&Signed { body, sig }).map_err(|e| Error::Encoding(e.to_string()))?;
        self.adopt_set(set.clone(), signed)?;
        Ok(set)
    }

    fn adopt_set(&mut self, set: MailboxSet, signed: Vec<u8>) -> Result<()> {
        let mut all = self.stored_set(&set.group);
        let s = all.entry(set.layer.clone()).or_default();
        if let Some((cur, _)) = &s.current {
            if !newer(&set, cur) {
                return Ok(());
            }
            if cur.endpoints != set.endpoints {
                s.previous = Some((cur.clone(), super::now_ms()));
            }
        }
        let g = set.group;
        s.current = Some((set, signed));
        crate::storage::write_atomic(&self.set_path(&g), &postcard::to_stdvec(&all).map_err(|e| Error::Encoding(e.to_string()))?)
    }

    /// The signed current sets (all layers), to carry in our main slot (every member
    /// carries them, so they survive their author going away).
    pub(super) fn mailbox_set_messages(&self, g: &GroupId) -> Vec<Vec<u8>> {
        self.stored_set(g).into_values().filter_map(|l| l.current.map(|(_, b)| b)).collect()
    }

    pub(super) fn receive_mailbox_set(&mut self, bytes: &[u8]) -> Result<()> {
        let s: Signed = postcard::from_bytes(bytes).map_err(|e| Error::Encoding(e.to_string()))?;
        let set: MailboxSet = wire::decode(SET_TAG, 1, &s.body)?.1;
        let sig: [u8; 64] = s.sig.as_slice().try_into().map_err(|_| Error::Crypto("bad signature length"))?;
        crypto::verify(&set.author.0, &s.body, &sig)?;
        let state = self.groups.get(&set.group).ok_or_else(|| Error::NotFound("group".into()))?;
        if !state.epochs.is_current_member(&set.author) || set.endpoints.len() > 32 || set.layer.len() > 32 || set.endpoints.iter().any(|e| e.len() > 512) {
            return Err(Error::Rejected("mailbox set from a non-member or malformed".into()));
        }
        self.adopt_set(set, bytes.to_vec())
    }
}
