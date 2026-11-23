//! Import sandbox and crash-loop breaker (D3).
//!
//! Loro 1.16.2 can abort the whole process on crafted input (research A6: 4 crash sites,
//! 85-byte minimum, and a poisoned lock turns the panic into an abort that `catch_unwind`
//! cannot stop). So untrusted updates are first imported into a *copy of the live
//! document* inside a disposable worker process. Only if that succeeds, and the update
//! advanced nothing but the sender's own PeerID, does the host import it.
//!
//! The quarantine journal records "importing X from Y" before any import into the live
//! document; if the app dies mid-import, X is quarantined on next start and never retried.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use loro::{ExportMode, LoroDoc};
use serde::{Deserialize, Serialize};

use crate::storage;
use crate::{wire, Error, Result};

/// How the host finds the worker executable.
#[derive(Clone, Debug)]
pub struct SandboxConfig {
    pub worker: PathBuf,
    pub timeout: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The update imported cleanly. Per PeerID, the counter range `[start, end)` of the ops
    /// it contained that the copy didn't have (applied, or pending on missing
    /// dependencies). Empty: nothing new. The host never parses untrusted blobs itself; it
    /// decides from this report.
    Ok { ranges: BTreeMap<u64, (i32, i32)> },
    /// Loro returned an error (malformed or not applicable). Safe to drop.
    Rejected(String),
    /// The worker crashed, aborted or hung: a poison pill. Must be quarantined.
    Crashed(String),
}

/// Validate `update` against a copy of the live document (`snapshot`) in the worker process.
pub fn validate(cfg: &SandboxConfig, snapshot: &[u8], update: &[u8]) -> Result<Verdict> {
    let mut child = Command::new(&cfg.worker)
        .arg("--kt-import-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    {
        let mut stdin = child.stdin.take().ok_or_else(|| Error::Invalid("no stdin".into()))?;
        stdin.write_all(&(snapshot.len() as u32).to_le_bytes())?;
        stdin.write_all(snapshot)?;
        stdin.write_all(update)?;
    }
    let start = Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => {
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    let _ = so.read_to_string(&mut out);
                }
                if !status.success() {
                    return Ok(Verdict::Crashed(format!("worker exited with {status}")));
                }
                return Ok(parse_worker_output(&out));
            }
            None if start.elapsed() > cfg.timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(Verdict::Crashed("worker timed out".into()));
            }
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

fn parse_worker_output(out: &str) -> Verdict {
    if let Some(rest) = out.strip_prefix("OK") {
        let mut ranges = BTreeMap::new();
        for item in rest.split(',').map(str::trim).filter(|x| !x.is_empty()) {
            let parsed = item.split_once(':').and_then(|(p, r)| {
                let (a, b) = r.split_once('-')?;
                Some((p.parse::<u64>().ok()?, a.parse::<i32>().ok()?, b.parse::<i32>().ok()?))
            });
            match parsed {
                Some((p, a, b)) => {
                    let e = ranges.entry(p).or_insert((a, b));
                    *e = (e.0.min(a), e.1.max(b));
                }
                None => return Verdict::Crashed(format!("unexpected worker output: {:.80}", out)),
            }
        }
        Verdict::Ok { ranges }
    } else if let Some(rest) = out.strip_prefix("ERR") {
        Verdict::Rejected(rest.trim().to_string())
    } else {
        Verdict::Crashed(format!("unexpected worker output: {:.80}", out))
    }
}

/// Entry point for the worker process. The host binary calls this when started with
/// `--kt-import-worker`. Reads `[u32 snapshot len][snapshot][update]` from stdin.
pub fn worker_main() -> ! {
    worker_main_with(|_| {})
}

/// [`worker_main`] that also runs the app's own reader on the merged document (second-pass audit T2-3): input that
/// crashes the app's reader then crashes here, in the throwaway process, and is quarantined, not in the app.
pub fn worker_main_with(read: fn(&LoroDoc)) -> ! {
    let mut buf = Vec::new();
    if std::io::stdin().read_to_end(&mut buf).is_err() || buf.len() < 4 {
        print!("ERR bad input");
        std::process::exit(0);
    }
    let n = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if 4 + n > buf.len() {
        print!("ERR bad input");
        std::process::exit(0);
    }
    let (snap, update) = (&buf[4..4 + n], &buf[4 + n..]);
    let doc = if snap.is_empty() {
        LoroDoc::new()
    } else {
        match LoroDoc::from_snapshot(snap) {
            Ok(d) => d,
            Err(e) => {
                print!("ERR snapshot: {e}");
                std::process::exit(0);
            }
        }
    };
    match doc.import(update) {
        Ok(status) => {
            // Force materialization: some Loro bugs only panic on first read (#1088).
            let _ = doc.get_deep_value();
            let _ = doc.export(ExportMode::Snapshot);
            read(&doc);
            // Peers whose ops the update contains: applied now or parked as pending
            // (waiting for dependencies). Both must be the sender's own PeerID.
            let mut items: Vec<String> = status.success.iter().map(|(p, (a, b))| format!("{p}:{a}-{b}")).collect();
            if let Some(pending) = &status.pending {
                items.extend(pending.iter().map(|(p, (a, b))| format!("{p}:{a}-{b}")));
            }
            print!("OK {}", items.join(","));
        }
        Err(e) => print!("ERR {e}"),
    }
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

/// Crash-loop breaker: records in-flight imports; leftover entries at startup are poison.
pub struct Quarantine {
    journal: PathBuf,
    list: PathBuf,
    quarantined: BTreeSet<[u8; 32]>,
}

#[derive(Serialize, Deserialize)]
struct JournalEntry {
    payload_hash: [u8; 32],
    author: [u8; 32],
}

impl Quarantine {
    /// Open the quarantine; any in-flight entry left by a crash is moved to the quarantine list.
    pub fn open(journal: &Path, list: &Path) -> Result<(Self, Vec<([u8; 32], [u8; 32])>)> {
        let mut newly = Vec::new();
        let mut quarantined = BTreeSet::new();
        for rec in storage::read_records(list)? {
            if let Ok((_, e)) = wire::decode::<JournalEntry>(*b"QE", 1, &rec) {
                quarantined.insert(e.payload_hash);
            }
        }
        for rec in storage::read_records(journal)? {
            if let Ok((_, e)) = wire::decode::<JournalEntry>(*b"QE", 1, &rec) {
                if quarantined.insert(e.payload_hash) {
                    storage::append_record(list, &rec)?;
                    newly.push((e.payload_hash, e.author));
                }
            }
        }
        if journal.exists() {
            std::fs::remove_file(journal)?;
        }
        Ok((Quarantine { journal: journal.to_path_buf(), list: list.to_path_buf(), quarantined }, newly))
    }

    pub fn is_quarantined(&self, payload_hash: &[u8; 32]) -> bool {
        self.quarantined.contains(payload_hash)
    }

    /// Record an import about to happen. Must be followed by [`Quarantine::end`].
    pub fn begin(&self, payload_hash: [u8; 32], author: [u8; 32]) -> Result<()> {
        let rec = wire::encode(*b"QE", 1, &JournalEntry { payload_hash, author })?;
        storage::write_atomic(&self.journal, &{
            let mut v = (rec.len() as u32).to_le_bytes().to_vec();
            v.extend_from_slice(&rec);
            v
        })
    }

    pub fn end(&self) -> Result<()> {
        if self.journal.exists() {
            std::fs::remove_file(&self.journal)?;
        }
        Ok(())
    }

    /// Quarantine a payload immediately (e.g. the sandbox reported a crash).
    pub fn add(&mut self, payload_hash: [u8; 32], author: [u8; 32]) -> Result<()> {
        if self.quarantined.insert(payload_hash) {
            let rec = wire::encode(*b"QE", 1, &JournalEntry { payload_hash, author })?;
            storage::append_record(&self.list, &rec)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_outputs() {
        assert_eq!(
            parse_worker_output("OK 5:0-3,7:2-9,5:3-4"),
            Verdict::Ok { ranges: [(5u64, (0, 4)), (7, (2, 9))].into_iter().collect() }
        );
        assert_eq!(parse_worker_output("OK "), Verdict::Ok { ranges: BTreeMap::new() });
        assert!(matches!(parse_worker_output("OK 5,7"), Verdict::Crashed(_)));
        assert!(matches!(parse_worker_output("ERR nope"), Verdict::Rejected(_)));
        assert!(matches!(parse_worker_output(""), Verdict::Crashed(_)));
    }

    #[test]
    fn leftover_journal_entry_is_quarantined() {
        let d = std::env::temp_dir().join(format!("kt-quar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (j, l) = (d.join("q.journal"), d.join("q.list"));
        {
            let (q, newly) = Quarantine::open(&j, &l).unwrap();
            assert!(newly.is_empty());
            q.begin([1; 32], [2; 32]).unwrap();
            // simulated crash: no end()
        }
        let (q, newly) = Quarantine::open(&j, &l).unwrap();
        assert_eq!(newly, vec![([1; 32], [2; 32])]);
        assert!(q.is_quarantined(&[1; 32]));
        let (q2, newly2) = Quarantine::open(&j, &l).unwrap();
        assert!(newly2.is_empty());
        assert!(q2.is_quarantined(&[1; 32]));
        let _ = std::fs::remove_dir_all(&d);
    }
}
