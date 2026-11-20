//! `kt-operator`: offline tooling for the operator key (DESIGN §7b, D56, D58). The key signs
//! the operator manifest and the update manifest; the app only ships its public half.
//!
//! The key file is sealed with a password (Argon2id, 256 MiB, then XChaCha20-Poly1305).
//! The password comes from the `KT_PW` environment variable only (never an argument or a
//! prompt), so wrappers can pass it from Credential Manager without it being shown.
//!
//! - `kt-operator new <keyfile>`: make a key (refuses to overwrite); prints the public key
//! - `kt-operator pub <keyfile>`: print the public key (checks the password)
//! - `kt-operator sign-manifest <keyfile> <manifest.json> <out.bin>`: sign an operator
//!   manifest (JSON of `kt_net::manifest::ManifestBody`); checks it against the key's
//!   public half and the app's built-in operator key before writing

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use kt_engine::crypto::{self, DeviceSecret, SymKey};
use rand::RngCore;
use zeroize::Zeroize;

const MAGIC: &[u8; 5] = b"KTOP1";
const AAD: &[u8] = b"kt/operator-key/v1";
// Argon2id: 256 MiB, 3 passes (offline tool on a desktop; slow on purpose).
const M_KIB: u32 = 256 * 1024;
const T: u32 = 3;

fn password() -> Result<Vec<u8>> {
    let pw = std::env::var("KT_PW").map_err(|_| anyhow!("set KT_PW (the key file's password)"))?;
    if pw.len() < 16 {
        bail!("the password is too short");
    }
    Ok(pw.into_bytes())
}

fn stretch(pw: &[u8], salt: &[u8]) -> Result<SymKey> {
    let params = Params::new(M_KIB, T, 1, Some(32)).map_err(|e| anyhow!("{e}"))?;
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params).hash_password_into(pw, salt, &mut out).map_err(|e| anyhow!("{e}"))?;
    Ok(SymKey(out))
}

pub fn seal_key(secret: &DeviceSecret, pw: &[u8]) -> Result<Vec<u8>> {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let mut raw = secret.to_bytes();
    let sealed = crypto::seal(&stretch(pw, &salt)?, AAD, &raw);
    raw.zeroize();
    Ok([&MAGIC[..], &salt, &sealed].concat())
}

pub fn open_key(file: &[u8], pw: &[u8]) -> Result<DeviceSecret> {
    if file.len() < 5 + 16 || &file[..5] != MAGIC {
        bail!("not an operator key file");
    }
    let (salt, sealed) = file[5..].split_at(16);
    let mut raw = crypto::open(&stretch(pw, salt)?, AAD, sealed).map_err(|_| anyhow!("wrong password or damaged key file"))?;
    let arr: [u8; 64] = raw.as_slice().try_into().map_err(|_| anyhow!("damaged key file"))?;
    raw.zeroize();
    Ok(DeviceSecret::from_bytes(&arr))
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, file] if cmd == "new" => {
            let path = Path::new(file);
            if path.exists() {
                bail!("{file} exists: refusing to overwrite a key");
            }
            let pw = password()?;
            let secret = DeviceSecret::generate();
            let sealed = seal_key(&secret, &pw)?;
            // Read it back before trusting it.
            if open_key(&sealed, &pw)?.public() != secret.public() {
                bail!("the sealed key didn't read back");
            }
            kt_engine::storage::write_atomic(path, &sealed).context("writing the key file")?;
            println!("{}", hex::encode(secret.public().sign));
        }
        [cmd, file] if cmd == "pub" => {
            let secret = open_key(&std::fs::read(file)?, &password()?)?;
            println!("{}", hex::encode(secret.public().sign));
        }
        [cmd, file, json, out] if cmd == "sign-manifest" => {
            let secret = open_key(&std::fs::read(file)?, &password()?)?;
            if secret.public().sign != kt_net::manifest::OPERATOR_PUBLIC_KEY {
                bail!("this key isn't the one the app trusts");
            }
            let body: kt_net::manifest::ManifestBody = serde_json::from_slice(&std::fs::read(json)?).context("reading the manifest JSON")?;
            let signed = kt_net::manifest::sign(&body, &secret);
            let back = kt_net::manifest::verify(&signed, &kt_net::manifest::OPERATOR_PUBLIC_KEY).map_err(|e| anyhow!(e))?;
            if back != body {
                bail!("the signed manifest didn't read back the same");
            }
            kt_engine::storage::write_atomic(Path::new(out), &signed).context("writing the signed manifest")?;
            println!("signed manifest version {} ({} bytes)", body.version, signed.len());
        }
        _ => bail!("usage: kt-operator new <keyfile> | pub <keyfile> | sign-manifest <keyfile> <manifest.json> <out.bin>   (password in KT_PW)"),
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("kt-operator: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_key_round_trip() {
        let s = DeviceSecret::generate();
        let file = seal_key(&s, b"correct horse battery staple").unwrap();
        assert_eq!(open_key(&file, b"correct horse battery staple").unwrap().public(), s.public());
        assert!(open_key(&file, b"wrong password, sixteen+").is_err());
        let mut bad = file.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(open_key(&bad, b"correct horse battery staple").is_err());
    }
}
