//! Hooks for the fuzz tests (`tests/fuzz.rs`): seal caller-provided bytes with this
//! replica's own keys, so mutated plaintext reaches the parsers behind the encryption.
//! They only ever use keys this replica already holds; nothing leaves the engine.

use super::slots::{ReadTopic, slot_aad};
use super::Replica;
use crate::crypto;
use crate::envelope::Envelope;
use crate::ids::GroupId;
use crate::{Error, Result};

impl Replica {
    /// A slot blob for `t` whose decrypted payload is exactly `plaintext`.
    #[doc(hidden)]
    pub fn fuzz_seal_slot(&self, t: &ReadTopic, plaintext: &[u8]) -> Result<Vec<u8>> {
        let key = self.groups.get(&t.group).and_then(|s| s.keys.get(&t.epoch)).ok_or_else(|| Error::NotFound("slot key".into()))?;
        Ok(crypto::seal(key, &slot_aad(&t.group, &t.epoch, &t.topic), plaintext))
    }

    /// An envelope message from this device in `g` (current head) whose decrypted inner
    /// bytes are exactly `inner`.
    #[doc(hidden)]
    pub fn fuzz_seal_envelope(&self, g: &GroupId, inner: &[u8]) -> Result<Vec<u8>> {
        let state = self.groups.get(g).ok_or_else(|| Error::NotFound("group".into()))?;
        let head = state.epochs.working_head().ok_or_else(|| Error::NotFound("head".into()))?;
        let key = state.keys.get(&head).ok_or_else(|| Error::NotFound("head key".into()))?;
        let env = Envelope::seal_bytes(&self.identity, head, key, g, inner)?;
        super::Message::Envelope(env.encode()?).encode()
    }

    /// The plaintext of an envelope this device would send in `g` now (a fuzz seed).
    #[doc(hidden)]
    pub fn fuzz_envelope_plaintext(&self, g: &GroupId, msg: &[u8]) -> Option<Vec<u8>> {
        let super::Message::Envelope(b) = super::Message::decode(msg).ok()? else { return None };
        let env = Envelope::decode(&b).ok()?;
        let state = self.groups.get(g)?;
        let key = state.keys.get(&env.epoch)?;
        env.open_bytes(g, key).ok()
    }
}
