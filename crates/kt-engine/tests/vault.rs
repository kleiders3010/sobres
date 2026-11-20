//! Device keys in OS secure storage (DESIGN §9): with a vault the identity is only stored
//! sealed, an older plain identity is sealed on first load (same device, same groups), and
//! the sealed file is useless without the OS key.

use std::sync::Arc;

use kt_engine::crypto::{self, SymKey};
use kt_engine::identity::{set_vault, DeviceIdentity, Vault};

/// Stands in for the Android Keystore / DPAPI: a key that never leaves this object.
struct TestVault(SymKey);

impl Vault for TestVault {
    fn seal(&self, plain: &[u8]) -> kt_engine::Result<Vec<u8>> {
        Ok(crypto::seal(&self.0, b"vault", plain))
    }
    fn open(&self, sealed: &[u8]) -> kt_engine::Result<Vec<u8>> {
        crypto::open(&self.0, b"vault", sealed)
    }
}

// One test: the vault is process-wide.
#[test]
fn identity_sealed_by_the_os_key_store() {
    let dir = std::env::temp_dir().join(format!("kt-vault-{}-{}", std::process::id(), rand::random::<u32>()));

    // An install from before secure storage: a plain identity.
    set_vault(None);
    let before = DeviceIdentity::load_or_create(&dir).unwrap().public();
    assert!(dir.join("device.key").exists());

    // With the OS key store: sealed on first load, the plain file gone, same device.
    let os = Arc::new(TestVault(SymKey::random()));
    set_vault(Some(os.clone()));
    assert_eq!(DeviceIdentity::load_or_create(&dir).unwrap().public(), before);
    assert!(!dir.join("device.key").exists(), "no plain copy left");
    let sealed = std::fs::read(dir.join("device.sealed")).unwrap();
    assert!(!sealed.windows(32).any(|w| w == &before.sign[..]), "sealed, not stored as-is");
    assert_eq!(DeviceIdentity::load_or_create(&dir).unwrap().public(), before, "stable across loads");

    // Copied elsewhere (another machine or user): unreadable without that OS key.
    set_vault(Some(Arc::new(TestVault(SymKey::random()))));
    assert!(DeviceIdentity::load_or_create(&dir).is_err());
    set_vault(None);
    assert!(DeviceIdentity::load_or_create(&dir).is_err(), "never silently replaced");

    // New installs are sealed from the start.
    set_vault(Some(os));
    let fresh = dir.join("fresh");
    let id = DeviceIdentity::load_or_create(&fresh).unwrap().public();
    assert!(fresh.join("device.sealed").exists() && !fresh.join("device.key").exists());
    assert_eq!(DeviceIdentity::load_or_create(&fresh).unwrap().public(), id);
}
