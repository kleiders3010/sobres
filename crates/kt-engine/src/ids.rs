//! Identifiers.
//!
//! - [`DeviceId`] = the device's Ed25519 public key.
//! - [`peer_id_for`] binds a Loro PeerID to a device key (D1/D2), so a device can only
//!   ever author ops under its own PeerID and PeerIDs can't collide by accident.
//! - [`GroupId`] / [`DocId`] are random and internal: they never appear on the wire in
//!   clear (network-visible tags are derived from epoch keys and rotate, D5 rule 6).
//! - [`EpochId`] is the content hash of a signed epoch record.

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

macro_rules! id32 {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub [u8; 32]);

        impl $name {
            pub fn random() -> Self {
                let mut b = [0u8; 32];
                OsRng.fill_bytes(&mut b);
                $name(b)
            }
            pub fn to_hex(&self) -> String {
                hex::encode(self.0)
            }
            pub fn short(&self) -> String {
                hex::encode(&self.0[..4])
            }
            pub fn from_hex(s: &str) -> Option<Self> {
                let v = hex::decode(s).ok()?;
                let arr: [u8; 32] = v.try_into().ok()?;
                Some($name(arr))
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.short())
            }
        }
    };
}

id32!(DeviceId);
id32!(GroupId);
id32!(DocId);
id32!(EpochId);

/// Loro PeerID bound to a device key. Never 0 and never above 2^63 (JS-safe range).
pub fn peer_id_for(device: &DeviceId) -> u64 {
    let h = crate::crypto::hash("kt/peer-id/v1", &device.0);
    let mut b = [0u8; 8];
    b.copy_from_slice(&h[..8]);
    let v = u64::from_le_bytes(b) & 0x7FFF_FFFF_FFFF_FFFF;
    if v == 0 { 1 } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_is_stable_and_distinct() {
        let a = DeviceId::random();
        let b = DeviceId::random();
        assert_eq!(peer_id_for(&a), peer_id_for(&a));
        assert_ne!(peer_id_for(&a), peer_id_for(&b));
        assert!(peer_id_for(&a) > 0 && peer_id_for(&a) < (1u64 << 63));
    }

    #[test]
    fn hex_roundtrip() {
        let g = GroupId::random();
        assert_eq!(GroupId::from_hex(&g.to_hex()), Some(g));
    }
}
