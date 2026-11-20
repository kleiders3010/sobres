//! Crash-safe local storage.
//!
//! Layout under the data directory (which *is* backed up, unlike the identity directory):
//!
//! ```text
//! data/
//!   groups/<group-hex>/
//!     control.log          signed epoch records (append-only, one per line block)
//!     docs/<doc-hex>/
//!       snapshot.bin       Loro snapshot (opaque, D10)
//!       updates.log        received/produced updates since the snapshot
//!   quarantine.journal     crash-loop breaker (D3)
//! ```
//!
//! All whole-file writes go through [`write_atomic`] (write temp + fsync + rename).
//! Logs are length-prefixed records appended with fsync; a torn tail record is ignored.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::ids::{DocId, GroupId};
use crate::{Error, Result};

/// Write a whole file atomically: a crash leaves either the old or the new content.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().ok_or_else(|| Error::Invalid("path has no parent".into()))?;
    fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::Invalid("path has no file name".into()))?
        .to_string_lossy()
        .into_owned();
    let tmp = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Maximum size of one log record (defense against corrupted length prefixes).
const MAX_RECORD: usize = 16 * 1024 * 1024;

/// Append one length-prefixed record to a log file and fsync it.
pub fn append_record(path: &Path, record: &[u8]) -> Result<()> {
    if record.len() > MAX_RECORD {
        return Err(Error::Rejected("record too large".into()));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let mut buf = Vec::with_capacity(4 + record.len());
    buf.extend_from_slice(&(record.len() as u32).to_le_bytes());
    buf.extend_from_slice(record);
    f.write_all(&buf)?;
    f.sync_all()?;
    Ok(())
}

/// Read all complete records of a log. A torn or corrupted tail is ignored (crash during append).
pub fn read_records(path: &Path) -> Result<Vec<Vec<u8>>> {
    let mut data = Vec::new();
    match File::open(path) {
        Ok(mut f) => {
            f.read_to_end(&mut data)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let len = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        if len > MAX_RECORD || i + 4 + len > data.len() {
            break;
        }
        out.push(data[i + 4..i + 4 + len].to_vec());
        i += 4 + len;
    }
    Ok(out)
}

/// Paths inside the data directory. All names are generated from IDs, never from
/// user- or peer-supplied strings (D60: no path injection).
#[derive(Clone, Debug)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Layout { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn group_dir(&self, g: &GroupId) -> PathBuf {
        self.root.join("groups").join(g.to_hex())
    }
    pub fn control_log(&self, g: &GroupId) -> PathBuf {
        self.group_dir(g).join("control.log")
    }
    pub fn group_meta(&self, g: &GroupId) -> PathBuf {
        self.group_dir(g).join("group.meta")
    }
    pub fn doc_dir(&self, g: &GroupId, d: &DocId) -> PathBuf {
        self.group_dir(g).join("docs").join(d.to_hex())
    }
    pub fn doc_snapshot(&self, g: &GroupId, d: &DocId) -> PathBuf {
        self.doc_dir(g, d).join("snapshot.bin")
    }
    pub fn doc_updates(&self, g: &GroupId, d: &DocId) -> PathBuf {
        self.doc_dir(g, d).join("updates.log")
    }
    pub fn quarantine_journal(&self) -> PathBuf {
        self.root.join("quarantine.journal")
    }
    pub fn quarantine_list(&self) -> PathBuf {
        self.root.join("quarantine.list")
    }
    pub fn groups(&self) -> Result<Vec<GroupId>> {
        let dir = self.root.join("groups");
        let mut out = Vec::new();
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for e in rd {
            let e = e?;
            if let Some(g) = e.file_name().to_str().and_then(GroupId::from_hex) {
                out.push(g);
            }
        }
        out.sort();
        Ok(out)
    }
    pub fn docs(&self, g: &GroupId) -> Result<Vec<DocId>> {
        let dir = self.group_dir(g).join("docs");
        let mut out = Vec::new();
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for e in rd {
            let e = e?;
            if let Some(d) = e.file_name().to_str().and_then(DocId::from_hex) {
                out.push(d);
            }
        }
        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("kt-storage-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn atomic_write_replaces() {
        let d = tmp("atomic");
        let p = d.join("a/b.bin");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn log_ignores_torn_tail() {
        let d = tmp("log");
        let p = d.join("x.log");
        append_record(&p, b"first").unwrap();
        append_record(&p, b"second").unwrap();
        // simulate a crash mid-append: length says 100 but only 3 bytes follow
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&100u32.to_le_bytes()).unwrap();
        f.write_all(b"abc").unwrap();
        drop(f);
        let recs = read_records(&p).unwrap();
        assert_eq!(recs, vec![b"first".to_vec(), b"second".to_vec()]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn layout_uses_hex_ids_only() {
        let l = Layout::new("/data");
        let g = GroupId([0xab; 32]);
        let p = l.control_log(&g);
        assert!(p.to_string_lossy().contains(&"ab".repeat(32)));
    }
}
