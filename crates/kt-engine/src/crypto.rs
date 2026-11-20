//! Cryptographic primitives used by the engine.
//!
//! - Ed25519 for signatures (device identity, envelopes, epoch records).
//! - X25519 + HKDF + XChaCha20-Poly1305 to wrap epoch keys for one recipient device.
//! - XChaCha20-Poly1305 with associated data for all payload encryption.
//! - HKDF-SHA256 for key derivation, BLAKE3 for content hashes.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey as XPublic, StaticSecret};
use zeroize::Zeroize;

use crate::Error;

/// A 32-byte symmetric key that is wiped from memory when dropped.
#[derive(Clone, PartialEq, Eq, Zeroize)]
#[zeroize(drop)]
pub struct SymKey(pub [u8; 32]);

impl SymKey {
    pub fn random() -> Self {
        let mut k = [0u8; 32];
        OsRng.fill_bytes(&mut k);
        SymKey(k)
    }
}

impl std::fmt::Debug for SymKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SymKey(..)")
    }
}

/// Derive a 32-byte key from input key material, a context label and extra info.
pub fn derive(ikm: &[u8], label: &str, info: &[u8]) -> SymKey {
    let hk = Hkdf::<Sha256>::new(Some(label.as_bytes()), ikm);
    let mut out = [0u8; 32];
    hk.expand(info, &mut out).expect("32 bytes is a valid HKDF output length");
    SymKey(out)
}

/// BLAKE3 hash of `data`, domain-separated by `context`.
pub fn hash(context: &str, data: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key(context);
    h.update(data);
    *h.finalize().as_bytes()
}

const NONCE_LEN: usize = 24;

/// Encrypt with a random 24-byte nonce prepended to the ciphertext.
pub fn seal(key: &SymKey, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new((&key.0).into());
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .expect("encryption with a valid key cannot fail");
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Decrypt data produced by [`seal`]. Fails on tampering or a wrong key / aad.
pub fn open(key: &SymKey, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, Error> {
    if sealed.len() < NONCE_LEN + 16 {
        return Err(Error::Crypto("ciphertext too short"));
    }
    let cipher = XChaCha20Poly1305::new((&key.0).into());
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| Error::Crypto("decryption failed"))
}

/// Public half of a device: signing key and key-exchange key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DevicePublic {
    pub sign: [u8; 32],
    pub kex: [u8; 32],
}

/// Secret keys of this device. Never serialized into backups (D1).
pub struct DeviceSecret {
    sign: SigningKey,
    kex: StaticSecret,
}

impl DeviceSecret {
    pub fn generate() -> Self {
        DeviceSecret { sign: SigningKey::generate(&mut OsRng), kex: StaticSecret::random_from_rng(OsRng) }
    }

    pub fn public(&self) -> DevicePublic {
        DevicePublic { sign: self.sign.verifying_key().to_bytes(), kex: XPublic::from(&self.kex).to_bytes() }
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.sign.sign(msg).to_bytes()
    }

    pub fn to_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&self.sign.to_bytes());
        out[32..].copy_from_slice(&self.kex.to_bytes());
        out
    }

    pub fn from_bytes(b: &[u8; 64]) -> Self {
        let mut s = [0u8; 32];
        s.copy_from_slice(&b[..32]);
        let mut k = [0u8; 32];
        k.copy_from_slice(&b[32..]);
        let out = DeviceSecret { sign: SigningKey::from_bytes(&s), kex: StaticSecret::from(k) };
        s.zeroize();
        k.zeroize();
        out
    }

    fn kex_secret(&self) -> &StaticSecret {
        &self.kex
    }
}

/// Verify an Ed25519 signature made by `signer`.
pub fn verify(signer: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> Result<(), Error> {
    let vk = VerifyingKey::from_bytes(signer).map_err(|_| Error::Crypto("bad public key"))?;
    vk.verify(msg, &Signature::from_bytes(sig)).map_err(|_| Error::Crypto("bad signature"))
}

/// Key-wrap suite: X25519 + HKDF-SHA256 + XChaCha20-Poly1305.
pub const SUITE_X25519: u8 = 1;
/// Reserved (DESIGN §5): hybrid X25519 + ML-KEM-768, for when devices carry post-quantum
/// keys. This build doesn't write it; reading it is [`Error::Newer`], never garbage.
pub const SUITE_X25519_MLKEM768: u8 = 2;

/// A symmetric key encrypted for one recipient device (ECIES-style).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrappedKey {
    /// Which suite wrapped it ([`SUITE_X25519`] today).
    pub suite: u8,
    pub ephemeral: [u8; 32],
    pub sealed: Vec<u8>,
}

/// Encrypt `key` so only the owner of `recipient_kex` can recover it. `aad` binds it to a context.
pub fn wrap_key(recipient_kex: &[u8; 32], key: &SymKey, aad: &[u8]) -> WrappedKey {
    let eph = StaticSecret::random_from_rng(OsRng);
    let eph_pub = XPublic::from(&eph).to_bytes();
    let shared = eph.diffie_hellman(&XPublic::from(*recipient_kex));
    let mut info = Vec::with_capacity(64);
    info.extend_from_slice(&eph_pub);
    info.extend_from_slice(recipient_kex);
    let kek = derive(shared.as_bytes(), "kt/wrap/v1", &info);
    WrappedKey { suite: SUITE_X25519, ephemeral: eph_pub, sealed: seal(&kek, aad, &key.0) }
}

/// Recover a key wrapped for this device.
pub fn unwrap_key(me: &DeviceSecret, wrapped: &WrappedKey, aad: &[u8]) -> Result<SymKey, Error> {
    if wrapped.suite != SUITE_X25519 {
        return Err(Error::Newer(format!("key-wrap suite {}", wrapped.suite)));
    }
    let shared = me.kex_secret().diffie_hellman(&XPublic::from(wrapped.ephemeral));
    let mut info = Vec::with_capacity(64);
    info.extend_from_slice(&wrapped.ephemeral);
    info.extend_from_slice(&me.public().kex);
    let kek = derive(shared.as_bytes(), "kt/wrap/v1", &info);
    let mut raw = open(&kek, aad, &wrapped.sealed)?;
    if raw.len() != 32 {
        raw.zeroize();
        return Err(Error::Crypto("wrapped key has wrong length"));
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&raw);
    raw.zeroize();
    Ok(SymKey(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip_and_tamper() {
        let k = SymKey::random();
        let ct = seal(&k, b"ad", b"hello");
        assert_eq!(open(&k, b"ad", &ct).unwrap(), b"hello");
        assert!(open(&k, b"other-ad", &ct).is_err());
        let mut bad = ct.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(open(&k, b"ad", &bad).is_err());
        assert!(open(&SymKey::random(), b"ad", &ct).is_err());
    }

    #[test]
    fn sign_verify() {
        let d = DeviceSecret::generate();
        let sig = d.sign(b"msg");
        assert!(verify(&d.public().sign, b"msg", &sig).is_ok());
        assert!(verify(&d.public().sign, b"msh", &sig).is_err());
        let other = DeviceSecret::generate();
        assert!(verify(&other.public().sign, b"msg", &sig).is_err());
    }

    #[test]
    fn unknown_suite_is_newer() {
        let d = DeviceSecret::generate();
        let mut w = wrap_key(&d.public().kex, &SymKey::random(), b"ad");
        assert!(unwrap_key(&d, &w, b"ad").is_ok());
        w.suite = SUITE_X25519_MLKEM768;
        assert!(matches!(unwrap_key(&d, &w, b"ad"), Err(Error::Newer(_))));
    }

    #[test]
    fn wrap_unwrap_only_for_recipient() {
        let alice = DeviceSecret::generate();
        let eve = DeviceSecret::generate();
        let k = SymKey::random();
        let w = wrap_key(&alice.public().kex, &k, b"epoch-1");
        assert_eq!(unwrap_key(&alice, &w, b"epoch-1").unwrap(), k);
        assert!(unwrap_key(&eve, &w, b"epoch-1").is_err());
        assert!(unwrap_key(&alice, &w, b"epoch-2").is_err());
    }

    #[test]
    fn secret_bytes_roundtrip() {
        let d = DeviceSecret::generate();
        let d2 = DeviceSecret::from_bytes(&d.to_bytes());
        assert_eq!(d.public(), d2.public());
    }
}
