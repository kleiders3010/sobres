//! Blobs (DESIGN §4, D4): content too large for a document, starting with text
//! attachments (notes above [`crate::limits::TEXT_ATTACHMENT_THRESHOLD`]).
//!
//! - A blob is compressed, then sealed with **its own random key**. Its ID is the SHA-256
//!   of the sealed bytes (as Blossom addresses blobs), so any holder can check what it
//!   serves, and holders only ever see ciphertext.
//! - The reference ([`BlobRef`]: ID, key, size) lives in the document, which is already
//!   encrypted for the group: knowing an ID means having read the document, so blobs are
//!   served to anyone who asks by ID.
//! - [`BlobStore`] keeps blobs on disk, one file per blob, verified on the way in.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::crypto::{self, SymKey};
use crate::limits::ABSURD_SIZE;
use crate::{Error, Result};

/// Blob body format: magic, then the compression used.
const MAGIC: &[u8; 4] = b"KTX1";
const COMPRESS_NONE: u8 = 0;
const COMPRESS_LZ4: u8 = 1;
const AAD: &[u8] = b"kt/blob/v1";

pub type BlobId = [u8; 32];

pub fn blob_id(sealed: &[u8]) -> BlobId {
    Sha256::digest(sealed).into()
}

/// What a document stores to point at a blob.
#[derive(Clone, PartialEq, Eq)]
pub struct BlobRef {
    pub id: BlobId,
    pub key: [u8; 32],
    /// Size of the original content in bytes.
    pub size: u64,
}

impl std::fmt::Debug for BlobRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BlobRef({}, {} bytes)", hex::encode(&self.id[..6]), self.size)
    }
}

impl BlobRef {
    /// Text form for a document field: `v1.<id>.<key>.<size>`.
    pub fn encode(&self) -> String {
        format!("v1.{}.{}.{}", hex::encode(self.id), hex::encode(self.key), self.size)
    }

    /// Parse [`BlobRef::encode`]'s form. A newer version's form is [`Error::Newer`].
    pub fn parse(s: &str) -> Result<BlobRef> {
        let parts: Vec<&str> = s.split('.').collect();
        match parts.as_slice() {
            ["v1", id, key, size] => {
                let id: BlobId = hex::decode(id).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| Error::Encoding("blob id".into()))?;
                let key: [u8; 32] = hex::decode(key).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| Error::Encoding("blob key".into()))?;
                let size: u64 = size.parse().map_err(|_| Error::Encoding("blob size".into()))?;
                Ok(BlobRef { id, key, size })
            }
            [v, ..] if v.starts_with('v') && v[1..].parse::<u32>().is_ok_and(|n| n > 1) => Err(Error::Newer(format!("blob reference {v}"))),
            _ => Err(Error::Encoding("not a blob reference".into())),
        }
    }
}

/// Compress and seal content as a new blob.
pub fn seal(content: &[u8]) -> Result<(BlobRef, Vec<u8>)> {
    if content.len() > ABSURD_SIZE {
        return Err(Error::Rejected(format!("{} MB is too large", content.len() / (1024 * 1024))));
    }
    let packed = lz4_flex::block::compress(content);
    let mut body = MAGIC.to_vec();
    if packed.len() < content.len() {
        body.push(COMPRESS_LZ4);
        body.extend_from_slice(&(content.len() as u32).to_le_bytes());
        body.extend_from_slice(&packed);
    } else {
        body.push(COMPRESS_NONE);
        body.extend_from_slice(content);
    }
    let key = SymKey::random();
    let sealed = crypto::seal(&key, AAD, &body);
    Ok((BlobRef { id: blob_id(&sealed), key: key.0, size: content.len() as u64 }, sealed))
}

/// Check, open and decompress a blob. Anything that isn't exactly what the reference
/// names is refused.
pub fn open(r: &BlobRef, sealed: &[u8]) -> Result<Vec<u8>> {
    if blob_id(sealed) != r.id {
        return Err(Error::Rejected("blob doesn't match its ID".into()));
    }
    let body = crypto::open(&SymKey(r.key), AAD, sealed)?;
    if body.len() < 5 || &body[..4] != MAGIC {
        return Err(Error::Encoding("not a blob".into()));
    }
    let content = match body[4] {
        COMPRESS_NONE => body[5..].to_vec(),
        COMPRESS_LZ4 => {
            let n = body.get(5..9).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize).ok_or_else(|| Error::Encoding("blob size".into()))?;
            if n > ABSURD_SIZE || n as u64 != r.size {
                return Err(Error::Rejected("blob size doesn't match".into()));
            }
            let out = lz4_flex::block::decompress(&body[9..], n).map_err(|e| Error::Encoding(e.to_string()))?;
            if out.len() != n {
                return Err(Error::Encoding("blob size doesn't match".into()));
            }
            out
        }
        c => return Err(Error::Newer(format!("blob compression {c}"))),
    };
    if content.len() as u64 != r.size {
        return Err(Error::Rejected("blob size doesn't match".into()));
    }
    Ok(content)
}

/// Blobs on disk: `<dir>/<id hex>`, verified against their ID on the way in.
#[derive(Clone, Debug)]
pub struct BlobStore {
    dir: PathBuf,
}

impl BlobStore {
    pub fn new(dir: impl Into<PathBuf>) -> BlobStore {
        BlobStore { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &BlobId) -> PathBuf {
        self.dir.join(hex::encode(id))
    }

    /// Store sealed bytes under their ID. Returns the ID.
    pub fn put(&self, sealed: &[u8]) -> Result<BlobId> {
        if sealed.len() > ABSURD_SIZE + 1024 {
            return Err(Error::Rejected("blob too large".into()));
        }
        let id = blob_id(sealed);
        let p = self.path(&id);
        if !p.exists() {
            std::fs::create_dir_all(&self.dir)?;
            crate::storage::write_atomic(&p, sealed)?;
        }
        Ok(id)
    }

    /// Store bytes someone sent for `id`: refused unless they are that blob.
    pub fn put_expected(&self, id: &BlobId, sealed: &[u8]) -> Result<()> {
        if blob_id(sealed) != *id {
            return Err(Error::Rejected("blob doesn't match its ID".into()));
        }
        self.put(sealed).map(|_| ())
    }

    pub fn has(&self, id: &BlobId) -> bool {
        self.path(id).exists()
    }

    /// The sealed bytes, if held and intact (a damaged file is deleted).
    pub fn get(&self, id: &BlobId) -> Option<Vec<u8>> {
        let b = std::fs::read(self.path(id)).ok()?;
        if blob_id(&b) == *id {
            Some(b)
        } else {
            let _ = std::fs::remove_file(self.path(id));
            None
        }
    }

    pub fn remove(&self, id: &BlobId) {
        let _ = std::fs::remove_file(self.path(id));
    }

    /// Every blob held, with its file's age in milliseconds.
    pub fn list(&self) -> Vec<(BlobId, u64)> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        rd.flatten()
            .filter_map(|e| {
                let id: BlobId = hex::decode(e.file_name().to_string_lossy().as_ref()).ok()?.try_into().ok()?;
                let age = e.metadata().ok()?.modified().ok()?.elapsed().map(|d| d.as_millis() as u64).unwrap_or(0);
                Some((id, age))
            })
            .collect()
    }

    /// Delete blobs nothing refers to any more, once older than `keep_ms` (history can
    /// still bring a reference back within that time).
    pub fn collect_garbage(&self, referenced: &std::collections::HashSet<BlobId>, keep_ms: u64) -> usize {
        let mut n = 0;
        for (id, age) in self.list() {
            if !referenced.contains(&id) && age > keep_ms {
                self.remove(&id);
                n += 1;
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_and_refusals() {
        let text = "a long note, ".repeat(20_000);
        let (r, sealed) = seal(text.as_bytes()).unwrap();
        assert!(sealed.len() < text.len() / 5, "text compresses: {} -> {}", text.len(), sealed.len());
        assert_eq!(open(&r, &sealed).unwrap(), text.as_bytes());
        assert_eq!(BlobRef::parse(&r.encode()).unwrap(), r);
        // Wrong bytes, wrong key, wrong size claim: refused.
        let mut bad = sealed.clone();
        bad[40] ^= 1;
        assert!(open(&r, &bad).is_err());
        assert!(open(&BlobRef { key: [7; 32], ..r.clone() }, &sealed).is_err());
        assert!(open(&BlobRef { size: r.size + 1, ..r.clone() }, &sealed).is_err());
        // Incompressible content is stored as-is.
        let noise: Vec<u8> = (0..5000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let (r2, s2) = seal(&noise).unwrap();
        assert_eq!(open(&r2, &s2).unwrap(), noise);
        assert!(matches!(BlobRef::parse("v2.whatever"), Err(Error::Newer(_))));
        assert!(BlobRef::parse("v1.00.11.2").is_err());
    }

    #[test]
    fn store_verifies_and_collects() {
        let dir = std::env::temp_dir().join(format!("kt-blobs-{}-{}", std::process::id(), rand::random::<u32>()));
        let s = BlobStore::new(&dir);
        let (r, sealed) = seal(b"hello").unwrap();
        assert!(s.put_expected(&[9; 32], &sealed).is_err(), "bytes must match the ID");
        s.put_expected(&r.id, &sealed).unwrap();
        assert!(s.has(&r.id));
        assert_eq!(s.get(&r.id).unwrap(), sealed);
        // A damaged file is dropped, not served.
        std::fs::write(dir.join(hex::encode(r.id)), b"junk").unwrap();
        assert!(s.get(&r.id).is_none() && !s.has(&r.id));
        s.put(&sealed).unwrap();
        let mut keep = std::collections::HashSet::new();
        assert_eq!(s.collect_garbage(&keep, 60_000), 0, "young blobs stay");
        assert_eq!(s.collect_garbage(&keep, 0), 1);
        s.put(&sealed).unwrap();
        keep.insert(r.id);
        assert_eq!(s.collect_garbage(&keep, 0), 0, "referenced blobs stay");
    }
}
