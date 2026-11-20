//! Device identity (D1).
//!
//! The device's secret keys live in a directory that is **excluded from backups**
//! (Android `noBackupFilesDir`; on desktop a separate `identity/` directory). A restored
//! or copied install therefore has no identity and creates a fresh one, which prevents
//! two live devices from sharing keys or a Loro PeerID.
//!
//! The desktop build must also hold an exclusive lock on the identity directory so two
//! instances can't run at once (see [`IdentityLock`]).

use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::crypto::{DevicePublic, DeviceSecret};
use crate::ids::{peer_id_for, DeviceId};
use crate::{Error, Result};
use zeroize::Zeroize;

const IDENTITY_FILE: &str = "device.key";
/// The identity sealed by the OS key store ([`Vault`]).
const SEALED_FILE: &str = "device.sealed";

/// OS secure storage for the device's secret keys (DESIGN §9): Android Keystore, Windows
/// DPAPI. It seals with a key the OS holds and never hands out, so a copied identity
/// directory is useless elsewhere. Group keys need no vault: on disk they are only ever
/// wrapped for this device's key.
pub trait Vault: Send + Sync {
    fn seal(&self, plain: &[u8]) -> Result<Vec<u8>>;
    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>>;
}

static VAULT: RwLock<Option<Arc<dyn Vault>>> = RwLock::new(None);

/// Use the platform's key store for identities loaded from now on (set once at startup).
pub fn set_vault(v: Option<Arc<dyn Vault>>) {
    *VAULT.write().expect("vault lock") = v;
}

/// Whether a key store has been set.
pub fn has_vault() -> bool {
    VAULT.read().expect("vault lock").is_some()
}

fn vault() -> Option<Arc<dyn Vault>> {
    VAULT.read().expect("vault lock").clone()
}

fn secret_from(bytes: &[u8]) -> Result<DeviceSecret> {
    let arr: [u8; 64] = bytes.try_into().map_err(|_| Error::Invalid("identity file has wrong length".into()))?;
    Ok(DeviceSecret::from_bytes(&arr))
}
const LOCK_FILE: &str = "instance.lock";

pub struct DeviceIdentity {
    secret: DeviceSecret,
    public: DevicePublic,
}

impl DeviceIdentity {
    pub fn generate() -> Self {
        let secret = DeviceSecret::generate();
        let public = secret.public();
        DeviceIdentity { secret, public }
    }

    pub fn id(&self) -> DeviceId {
        DeviceId(self.public.sign)
    }

    pub fn public(&self) -> DevicePublic {
        self.public
    }

    pub fn peer_id(&self) -> u64 {
        peer_id_for(&self.id())
    }

    pub fn secret(&self) -> &DeviceSecret {
        &self.secret
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.secret.sign(msg)
    }

    /// Load the identity from `dir`, or create one if none exists. With a [`Vault`], the
    /// identity is kept sealed by it, and an older plain identity file is sealed on first
    /// load.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let (plain, sealed) = (dir.join(IDENTITY_FILE), dir.join(SEALED_FILE));
        let vault = vault();
        let secret = if sealed.exists() {
            let v = vault.as_ref().ok_or_else(|| Error::Invalid("the identity is sealed by a key store this build doesn't have".into()))?;
            let mut bytes = v.open(&fs::read(&sealed)?)?;
            let s = secret_from(&bytes);
            bytes.zeroize();
            s?
        } else if plain.exists() {
            let mut bytes = fs::read(&plain)?;
            let s = secret_from(&bytes);
            bytes.zeroize();
            let s = s?;
            // A key store that fails (they do, on some phones) leaves it as it was.
            if let Some(v) = &vault {
                if Self::write_sealed(v.as_ref(), &sealed, &s).is_ok() {
                    fs::remove_file(&plain)?;
                }
            }
            s
        } else {
            let s = DeviceSecret::generate();
            let sealed_ok = vault.as_ref().is_some_and(|v| Self::write_sealed(v.as_ref(), &sealed, &s).is_ok());
            if !sealed_ok {
                crate::storage::write_atomic(&plain, &s.to_bytes())?;
            }
            s
        };
        let public = secret.public();
        Ok(DeviceIdentity { secret, public })
    }

    /// Seal and write, then read it back through the vault before relying on it.
    fn write_sealed(v: &dyn Vault, path: &Path, s: &DeviceSecret) -> Result<()> {
        let mut raw = s.to_bytes();
        let sealed = v.seal(&raw);
        raw.zeroize();
        let sealed = sealed?;
        let mut back = v.open(&sealed)?;
        let same = secret_from(&back).map(|b| b.public() == s.public()).unwrap_or(false);
        back.zeroize();
        if !same {
            return Err(Error::Crypto("the key store didn't give the identity back"));
        }
        crate::storage::write_atomic(path, &sealed)
    }
}

/// Exclusive single-instance lock (D1). Held for the lifetime of the engine.
///
/// An OS file lock, not a marker file: the OS releases it when the process dies, so a
/// killed app (Android kills processes all the time) never locks itself out.
pub struct IdentityLock {
    _file: File,
}

impl IdentityLock {
    /// Acquire the lock. Fails if another running instance holds it.
    pub fn acquire(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(LOCK_FILE))?;
        match f.try_lock() {
            Ok(()) => Ok(IdentityLock { _file: f }),
            Err(std::fs::TryLockError::WouldBlock) => Err(Error::Rejected("another instance is already running".into())),
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("kt-identity-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn load_or_create_is_stable() {
        let d = tmp("stable");
        let a = DeviceIdentity::load_or_create(&d).unwrap();
        let b = DeviceIdentity::load_or_create(&d).unwrap();
        assert_eq!(a.id(), b.id());
        assert_eq!(a.peer_id(), b.peer_id());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_identity_means_new_device() {
        // Simulates a restore where the identity dir was excluded from backup.
        let d1 = tmp("orig");
        let d2 = tmp("restored");
        let a = DeviceIdentity::load_or_create(&d1).unwrap();
        let b = DeviceIdentity::load_or_create(&d2).unwrap();
        assert_ne!(a.id(), b.id());
        assert_ne!(a.peer_id(), b.peer_id());
        let _ = fs::remove_dir_all(&d1);
        let _ = fs::remove_dir_all(&d2);
    }

    #[test]
    fn single_instance_lock() {
        let d = tmp("lock");
        let l1 = IdentityLock::acquire(&d).unwrap();
        assert!(IdentityLock::acquire(&d).is_err());
        drop(l1);
        assert!(IdentityLock::acquire(&d).is_ok());
        // A lock file left behind by a killed process doesn't lock the app out.
        fs::write(d.join(LOCK_FILE), b"12345
").unwrap();
        assert!(IdentityLock::acquire(&d).is_ok(), "stale lock file ignored");
        let _ = fs::remove_dir_all(&d);
    }
}
