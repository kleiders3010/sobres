//! Signed, encrypted document messages (D2).
//!
//! ```text
//! Envelope { epoch, sender, sealed, sig }
//!   sig    = Ed25519(sender, "kt/env/v1" || epoch || sender || sealed)
//!   sealed = XChaCha20-Poly1305(key = HKDF(epoch key, group), aad = epoch || sender, Inner)
//! Inner    { group, doc, kind, peer, created_ms, payload }
//! ```
//!
//! Receivers check, in order: signature, sender is a recipient of that epoch, decryption,
//! and that the claimed Loro peer equals the sender's bound PeerID. Only then does the
//! payload go to the import sandbox, which additionally proves the update advanced no
//! other peer (own-ops-only).

use serde::{Deserialize, Serialize};

use crate::crypto::{self, SymKey};
use crate::epochs::GroupEpochs;
use crate::identity::DeviceIdentity;
use crate::ids::{peer_id_for, DeviceId, DocId, EpochId, GroupId};
use crate::{wire, Error, Result};

const TAG: [u8; 2] = *b"EV";
const VERSION: u16 = 1;
const INNER_TAG: [u8; 2] = *b"EI";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    /// A Loro update containing only the sender's own ops.
    DocUpdate,
    /// A proposed fresh document for a document epoch (D24); accepted only if equal to local state.
    DocEpochProposal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inner {
    pub group: GroupId,
    pub doc: DocId,
    /// Document generation (document epochs, D24). Updates for an older generation are
    /// dropped by receivers (their author rebases); newer ones wait for the proposal.
    pub generation: u32,
    pub kind: MessageKind,
    pub peer: u64,
    pub created_ms: u64,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub epoch: EpochId,
    pub sender: DeviceId,
    pub sealed: Vec<u8>,
    pub sig: Vec<u8>,
}

fn env_key(epoch_key: &SymKey, group: &GroupId) -> SymKey {
    crypto::derive(&epoch_key.0, "kt/env/v1", &group.0)
}

fn signed_bytes(epoch: &EpochId, sender: &DeviceId, sealed: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(9 + 64 + sealed.len());
    v.extend_from_slice(b"kt/env/v1");
    v.extend_from_slice(&epoch.0);
    v.extend_from_slice(&sender.0);
    v.extend_from_slice(sealed);
    v
}

fn aad(epoch: &EpochId, sender: &DeviceId) -> Vec<u8> {
    let mut v = Vec::with_capacity(64);
    v.extend_from_slice(&epoch.0);
    v.extend_from_slice(&sender.0);
    v
}

impl Envelope {
    /// Seal a message from `me` under the given epoch.
    pub fn seal(me: &DeviceIdentity, epoch: EpochId, epoch_key: &SymKey, inner: &Inner) -> Result<Envelope> {
        if inner.peer != me.peer_id() {
            return Err(Error::Invalid("inner peer must be the sender's bound PeerID".into()));
        }
        let inner_bytes = wire::encode(INNER_TAG, VERSION, inner)?;
        Envelope::seal_bytes(me, epoch, epoch_key, &inner.group, &inner_bytes)
    }

    /// Seal already-encoded inner bytes (also used by the fuzz tests to reach the parser
    /// behind the encryption).
    pub fn seal_bytes(me: &DeviceIdentity, epoch: EpochId, epoch_key: &SymKey, group: &GroupId, inner_bytes: &[u8]) -> Result<Envelope> {
        let sealed = crypto::seal(&env_key(epoch_key, group), &aad(&epoch, &me.id()), inner_bytes);
        let sig = me.sign(&signed_bytes(&epoch, &me.id(), &sealed)).to_vec();
        Ok(Envelope { epoch, sender: me.id(), sealed, sig })
    }

    /// Decrypt without checks (fuzz seeds only; [`Envelope::open`] is the real path).
    pub fn open_bytes(&self, group: &GroupId, epoch_key: &SymKey) -> Result<Vec<u8>> {
        Ok(crypto::open(&env_key(epoch_key, group), &aad(&self.epoch, &self.sender), &self.sealed)?)
    }

    /// Verify and open an envelope for `group`, using the epoch graph and the epoch key.
    pub fn open(&self, group: &GroupId, epochs: &GroupEpochs, epoch_key: &SymKey) -> Result<Inner> {
        let sig: [u8; 64] = self.sig.as_slice().try_into().map_err(|_| Error::Crypto("bad signature length"))?;
        crypto::verify(&self.sender.0, &signed_bytes(&self.epoch, &self.sender, &self.sealed), &sig)?;
        let recipients = epochs.recipients(&self.epoch).ok_or_else(|| Error::NotFound("epoch of envelope".into()))?;
        if !recipients.contains_key(&self.sender) {
            return Err(Error::Rejected("sender is not a member of that epoch".into()));
        }
        if !epochs.is_valid(&self.epoch) {
            return Err(Error::Rejected("envelope epoch is not valid".into()));
        }
        let bytes = crypto::open(&env_key(epoch_key, group), &aad(&self.epoch, &self.sender), &self.sealed)?;
        let (_, inner): (u16, Inner) = wire::decode(INNER_TAG, VERSION, &bytes)?;
        if inner.group != *group {
            return Err(Error::Rejected("envelope for another group".into()));
        }
        if inner.peer != peer_id_for(&self.sender) {
            return Err(Error::Rejected("claimed PeerID is not bound to the sender (impersonation)".into()));
        }
        Ok(inner)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wire::encode(TAG, VERSION, self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(wire::decode(TAG, VERSION, bytes)?.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::epochs;

    fn group_with(n: usize) -> (Vec<DeviceIdentity>, GroupId, GroupEpochs) {
        let ds: Vec<DeviceIdentity> = (0..n).map(|_| DeviceIdentity::generate()).collect();
        let g = GroupId::random();
        let mut ge = GroupEpochs::new(g);
        ge.insert(epochs::genesis(&ds[0], g)).unwrap();
        for d in &ds[1..] {
            let add = ge.create_add(&ds[0], d.public(), &Default::default()).unwrap();
            ge.insert(add).unwrap();
        }
        (ds, g, ge)
    }

    fn inner(g: GroupId, me: &DeviceIdentity) -> Inner {
        Inner { group: g, doc: DocId::random(), generation: 0, kind: MessageKind::DocUpdate, peer: me.peer_id(), created_ms: 0, payload: b"x".to_vec() }
    }

    #[test]
    fn roundtrip_between_members() {
        let (ds, g, ge) = group_with(2);
        let h = ge.head().unwrap();
        let k0 = ge.key_for(&h, &ds[0]).unwrap();
        let env = Envelope::seal(&ds[0], h, &k0, &inner(g, &ds[0])).unwrap();
        let bytes = env.encode().unwrap();
        let env2 = Envelope::decode(&bytes).unwrap();
        let k1 = ge.key_for(&h, &ds[1]).unwrap();
        assert_eq!(env2.open(&g, &ge, &k1).unwrap().payload, b"x");
    }

    #[test]
    fn impersonation_and_tamper_rejected() {
        let (ds, g, ge) = group_with(2);
        let h = ge.head().unwrap();
        let k = ge.key_for(&h, &ds[0]).unwrap();
        // Claiming another device's PeerID is refused at seal time...
        let mut bad = inner(g, &ds[0]);
        bad.peer = ds[1].peer_id();
        assert!(Envelope::seal(&ds[0], h, &k, &bad).is_err());
        // ...and a hand-crafted envelope with a foreign sender fails the signature.
        let mut env = Envelope::seal(&ds[0], h, &k, &inner(g, &ds[0])).unwrap();
        env.sender = ds[1].id();
        assert!(env.open(&g, &ge, &k).is_err());
        // Tampered ciphertext fails.
        let mut env = Envelope::seal(&ds[0], h, &k, &inner(g, &ds[0])).unwrap();
        let n = env.sealed.len();
        env.sealed[n - 1] ^= 1;
        assert!(env.open(&g, &ge, &k).is_err());
    }

    #[test]
    fn non_member_rejected() {
        let (ds, g, ge) = group_with(2);
        let outsider = DeviceIdentity::generate();
        let h = ge.head().unwrap();
        let k = ge.key_for(&h, &ds[0]).unwrap(); // leaked key
        let env = Envelope::seal(&outsider, h, &k, &inner(g, &outsider)).unwrap();
        assert!(env.open(&g, &ge, &k).is_err());
    }
}
