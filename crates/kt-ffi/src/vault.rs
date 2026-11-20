//! Device keys in OS secure storage (DESIGN §9). Android passes its Keystore in through
//! [`KeyVault`]; Windows uses DPAPI, sealing with the signed-in user's key.

use std::sync::Arc;

use kt_engine::identity::Vault;

use crate::KtError;

/// A platform key store: seals with a key the OS holds and never hands out.
#[uniffi::export(with_foreign)]
pub trait KeyVault: Send + Sync {
    fn seal(&self, plain: Vec<u8>) -> Result<Vec<u8>, KtError>;
    fn open(&self, sealed: Vec<u8>) -> Result<Vec<u8>, KtError>;
}

struct Foreign(Arc<dyn KeyVault>);

impl Vault for Foreign {
    fn seal(&self, plain: &[u8]) -> kt_engine::Result<Vec<u8>> {
        self.0.seal(plain.to_vec()).map_err(|e| kt_engine::Error::Invalid(format!("key store: {e}")))
    }
    fn open(&self, sealed: &[u8]) -> kt_engine::Result<Vec<u8>> {
        self.0.open(sealed.to_vec()).map_err(|e| kt_engine::Error::Invalid(format!("key store: {e}")))
    }
}

/// Use this key store for the device identity. Call before opening a node.
#[uniffi::export]
pub fn set_key_vault(vault: Arc<dyn KeyVault>) {
    kt_engine::identity::set_vault(Some(Arc::new(Foreign(vault))));
}

/// Where the platform has one built in (Windows), use it unless the app set its own.
pub(crate) fn use_builtin() {
    #[cfg(windows)]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            if !kt_engine::identity::has_vault() {
                kt_engine::identity::set_vault(Some(Arc::new(dpapi::Dpapi)));
            }
        });
    }
}

#[cfg(windows)]
pub(crate) mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};

    /// DPAPI with the current user's key: only this Windows account can open it.
    pub struct Dpapi;

    fn run(data: &[u8], protect: bool) -> kt_engine::Result<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: input points to `data` for the call; DPAPI allocates `out` with LocalAlloc,
        // which is copied and freed below.
        let ok = unsafe {
            if protect {
                CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            } else {
                CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            }
        };
        if ok == 0 || out.pbData.is_null() {
            return Err(kt_engine::Error::Crypto("Windows data protection failed"));
        }
        // SAFETY: DPAPI returned `cbData` bytes at `pbData`.
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        // SAFETY: allocated by DPAPI with LocalAlloc.
        unsafe { LocalFree(out.pbData as _) };
        Ok(v)
    }

    impl kt_engine::identity::Vault for Dpapi {
        fn seal(&self, plain: &[u8]) -> kt_engine::Result<Vec<u8>> {
            run(plain, true)
        }
        fn open(&self, sealed: &[u8]) -> kt_engine::Result<Vec<u8>> {
            run(sealed, false)
        }
    }

    #[cfg(test)]
    mod tests {
        use kt_engine::identity::Vault;

        #[test]
        fn dpapi_round_trip() {
            let sealed = super::Dpapi.seal(b"device secret").unwrap();
            assert!(!sealed.windows(13).any(|w| w == b"device secret"));
            assert_eq!(super::Dpapi.open(&sealed).unwrap(), b"device secret");
            let mut bad = sealed.clone();
            *bad.last_mut().unwrap() ^= 1;
            assert!(super::Dpapi.open(&bad).is_err());
        }
    }
}
