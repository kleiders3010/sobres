//! Backups (DESIGN §11): a device's groups, keys and documents in one sealed file.
//!
//! - **Sealed; a password if the user sets one:** without one, a random key sits inside the
//!   file; automated scanning can't read it, deliberate effort can (§11). With a backup
//!   password (audit T11) the file is sealed with an Argon2id-stretched key and opens only
//!   with the password. Backups hold live group keys, so periodic key rotation (§5) limits
//!   how long a stolen one is useful.
//! - A restore creates a **fresh device identity** (D1: identities are never restored).
//!   The new device holds the keys (it can read at once) but isn't a member; to get back
//!   in, each backup carries a **recovery invite** per shared group, which members' devices
//!   accept like any invite. Groups with no other member are re-homed by the app layer.

use serde::{Deserialize, Serialize};

use super::Replica;
use crate::crypto::{self, SymKey};
use crate::epochs::EpochRecord;
use crate::ids::{DeviceId, DocId, EpochId, GroupId};
use crate::invite::{CodeKind, InviteRecord};
use crate::{storage, wire, Error, Result};
use loro::{ExportMode, LoroDoc};

const MAGIC: &[u8; 4] = b"KTB1";
const TAG: [u8; 2] = *b"BK";

#[derive(Clone, Serialize, Deserialize)]
pub struct GroupBackup {
    pub group: GroupId,
    pub records: Vec<Vec<u8>>,
    pub keys: Vec<(EpochId, [u8; 32])>,
    pub docs: Vec<(DocId, u32, Vec<u8>)>,
    /// Other current members (besides the backed-up device) when the backup was made.
    pub other_members: Vec<DeviceId>,
    /// A recovery invite (id, expiry): members accept it like any invite.
    pub recovery: Option<([u8; 32], u64)>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Backup {
    pub created_ms: u64,
    pub old_device: DeviceId,
    pub groups: Vec<GroupBackup>,
    /// Opaque app data (e.g. which group is personal).
    pub app: Vec<u8>,
}

/// Seal a file that leaves the device (§11): a random key inside, so automated scanning
/// can't read it and deliberate effort can.
pub fn seal_file(magic: &[u8; 4], context: &[u8], body: &[u8]) -> Vec<u8> {
    let key = SymKey::random();
    let mut out = magic.to_vec();
    out.extend_from_slice(&key.0);
    out.extend_from_slice(&crypto::seal(&key, context, body));
    out
}

/// Open a file made by [`seal_file`] (untrusted input).
pub fn open_file(magic: &[u8; 4], context: &[u8], bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 4 + 32 || &bytes[..4] != magic {
        return Err(Error::Invalid("not the expected file type".into()));
    }
    let key = SymKey(bytes[4..36].try_into().expect("32"));
    crypto::open(&key, context, &bytes[36..])
}

/// Seal a backup.
pub fn seal(b: &Backup) -> Result<Vec<u8>> {
    Ok(seal_file(MAGIC, b"kt/backup/v1", &wire::encode(TAG, 1, b)?))
}

/// Open a sealed backup file (untrusted input). A password-locked one needs [`open_with`].
pub fn open(bytes: &[u8]) -> Result<Backup> {
    open_with(bytes, None)
}

// Password-locked backups (audit T11): "KTB2" + salt + Argon2id cost + the body sealed with
// the key the password stretches to. Without the password the file opens nothing.
const PW_MAGIC: &[u8; 4] = b"KTB2";
const PW_CONTEXT: &[u8] = b"kt/backup/v2";
/// Argon2id cost for backup passwords (the invite floor, D9: 64 MiB, 3 passes).
pub const PW_M_KIB: u32 = 64 * 1024;
pub const PW_T: u32 = 3;
/// Shortest backup password the apps accept.
pub const PW_MIN_CHARS: usize = 8;
/// What an engine error says when the password doesn't open the file.
pub const WRONG_PASSWORD: &str = "wrong backup password";

/// A backup password stretched once (salt + key), kept on the device so daily backups
/// don't need the password again. Never put it in a backup.
#[derive(Clone)]
pub struct BackupLock {
    salt: [u8; 16],
    m_kib: u32,
    t: u32,
    key: SymKey,
}

impl BackupLock {
    /// Stretch `password` with a new salt (one Argon2id run, ~0.5 s on a phone).
    pub fn new(password: &str) -> Result<BackupLock> {
        let mut salt = [0u8; 16];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut salt);
        let key = stretch(password, &salt, PW_M_KIB, PW_T)?;
        Ok(BackupLock { salt, m_kib: PW_M_KIB, t: PW_T, key })
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.salt.to_vec();
        out.extend_from_slice(&self.m_kib.to_le_bytes());
        out.extend_from_slice(&self.t.to_le_bytes());
        out.extend_from_slice(&self.key.0);
        out
    }

    fn from_bytes(b: &[u8]) -> Option<BackupLock> {
        (b.len() == 16 + 8 + 32).then(|| BackupLock {
            salt: b[..16].try_into().expect("16"),
            m_kib: u32::from_le_bytes(b[16..20].try_into().expect("4")),
            t: u32::from_le_bytes(b[20..24].try_into().expect("4")),
            key: SymKey(b[24..56].try_into().expect("32")),
        })
    }
}

fn stretch(password: &str, salt: &[u8], m_kib: u32, t: u32) -> Result<SymKey> {
    use argon2::{Algorithm, Argon2, Params, Version};
    // Untrusted files name their own cost: never more than 4x ours (memory), never less than the floor.
    if !(PW_M_KIB..=4 * PW_M_KIB).contains(&m_kib) || !(1..=10).contains(&t) {
        return Err(Error::Invalid("not a backup file".into()));
    }
    let params = Params::new(m_kib, t, 1, Some(32)).map_err(|e| Error::Invalid(e.to_string()))?;
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(SymKey(out))
}

/// Seal a backup with a password lock (`None`: the plain sealed format).
pub fn seal_locked(b: &Backup, lock: Option<&BackupLock>) -> Result<Vec<u8>> {
    let Some(lock) = lock else { return seal(b) };
    let mut out = PW_MAGIC.to_vec();
    out.extend_from_slice(&lock.salt);
    out.extend_from_slice(&lock.m_kib.to_le_bytes());
    out.extend_from_slice(&lock.t.to_le_bytes());
    out.extend_from_slice(&crypto::seal(&lock.key, PW_CONTEXT, &wire::encode(TAG, 1, b)?));
    Ok(out)
}

/// The file is a password-locked backup.
pub fn needs_password(bytes: &[u8]) -> bool {
    bytes.len() > 4 + 24 && &bytes[..4] == PW_MAGIC
}

/// Open a backup file (untrusted input); `password` opens a locked one. A wrong or missing
/// password is `Invalid(WRONG_PASSWORD)`.
pub fn open_with(bytes: &[u8], password: Option<&str>) -> Result<Backup> {
    let body = if needs_password(bytes) {
        let password = password.ok_or_else(|| Error::Invalid(WRONG_PASSWORD.into()))?;
        let m_kib = u32::from_le_bytes(bytes[20..24].try_into().expect("4"));
        let t = u32::from_le_bytes(bytes[24..28].try_into().expect("4"));
        let key = stretch(password, &bytes[4..20], m_kib, t)?;
        crypto::open(&key, PW_CONTEXT, &bytes[28..]).map_err(|_| Error::Invalid(WRONG_PASSWORD.into()))?
    } else {
        open_file(MAGIC, b"kt/backup/v1", bytes).map_err(|_| Error::Invalid("not a backup file".into()))?
    };
    Ok(wire::decode(TAG, 1, &body)?.1)
}

const LOCK_FILE: &str = "backup_lock";

impl Replica {
    /// Set (`Some`) or remove (`None`) this device's backup password lock. Kept in the data
    /// folder, sealed with a key from this device's own key (never backed up, D1).
    pub fn set_backup_lock(&mut self, lock: Option<&BackupLock>) -> Result<()> {
        let path = self.layout.root().join(LOCK_FILE);
        match lock {
            Some(l) => storage::write_atomic(&path, &crypto::seal(&self.lock_key(), LOCK_FILE.as_bytes(), &l.to_bytes())),
            None => match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            },
        }
    }

    /// This device's backup password lock, if one is set.
    pub fn backup_lock(&self) -> Option<BackupLock> {
        let b = std::fs::read(self.layout.root().join(LOCK_FILE)).ok()?;
        BackupLock::from_bytes(&crypto::open(&self.lock_key(), LOCK_FILE.as_bytes(), &b).ok()?)
    }

    fn lock_key(&self) -> SymKey {
        crypto::derive(&self.identity.secret().to_bytes(), "kt/backup-lock-at-rest/v1", &[])
    }
}

/// What a restore did per group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Restored {
    /// Keys and documents in place; a recovery join is pending (members let us back in).
    Rejoining(GroupId),
    /// Keys and documents in place for reading; no way back in (no other member, or no
    /// recovery invite). The app re-homes such lists.
    ReadOnly(GroupId),
}

impl Replica {
    /// Make a backup of every group this device is in. Creates (or reuses) a recovery
    /// invite per group with other members, valid `recovery_ms`. Groups this device is
    /// still waiting to be let into (after a restore or an invite) carry that join along.
    pub fn make_backup(&mut self, recovery_ms: u64, app: Vec<u8>) -> Result<Backup> {
        let me = self.identity.id();
        let now = super::now_ms();
        let gids: Vec<GroupId> = self.groups.keys().copied().collect();
        let pending = self.pending_joins();
        let mut groups = Vec::new();
        for g in gids {
            let waiting = pending.iter().find(|p| p.group == g && p.kind == CodeKind::Invite);
            let (records, keys, docs, others) = {
                let state = &self.groups[&g];
                if !state.epochs.is_current_member(&me) && waiting.is_none() {
                    continue;
                }
                let records: Vec<Vec<u8>> = state.epochs.all_records().map(|r| r.encode()).collect::<Result<_>>()?;
                let keys: Vec<(EpochId, [u8; 32])> = state.keys.iter().map(|(e, k)| (*e, k.0)).collect();
                let docs: Vec<(DocId, u32, Vec<u8>)> = state
                    .docs
                    .iter()
                    .filter_map(|(d, ds)| ds.doc.export(ExportMode::Snapshot).ok().map(|s| (*d, ds.generation, s)))
                    .collect();
                let others: Vec<DeviceId> = state
                    .epochs
                    .head()
                    .and_then(|h| state.epochs.recipients(&h).map(|r| r.keys().filter(|d| **d != me).copied().collect()))
                    .unwrap_or_default();
                (records, keys, docs, others)
            };
            // Reuse an open recovery invite if there's one left with at least half its time.
            let recovery = if let Some(p) = waiting {
                Some((p.invite_id, now + recovery_ms))
            } else if others.is_empty() {
                None
            } else {
                let existing = self
                    .invites(&g)
                    .into_iter()
                    .filter(|i| i.kind == CodeKind::Invite && i.creator == me && self.invite_is_open(i) && i.expires_ms.saturating_sub(now) > recovery_ms / 2)
                    .max_by_key(|i| i.expires_ms);
                match existing {
                    Some(i) => Some((i.code.id(), i.expires_ms)),
                    None => {
                        let (code, expires) = self.create_invite(&g, CodeKind::Invite, recovery_ms)?;
                        Some((code.id(), expires))
                    }
                }
            };
            groups.push(GroupBackup { group: g, records, keys, docs, other_members: others, recovery });
        }
        Ok(Backup { created_ms: now, old_device: me, groups, app })
    }

    /// Restore a backup into this (fresh) device: keys and documents in place for reading,
    /// and a recovery join posted for every group with other members.
    pub fn restore_backup(&mut self, b: &Backup) -> Result<Vec<Restored>> {
        let mut out = Vec::new();
        for gb in &b.groups {
            let g = gb.group;
            self.accept_group(g);
            std::fs::create_dir_all(self.layout.group_dir(&g))?;
            for (e, k) in &gb.keys {
                self.add_invite_key(g, *e, SymKey(*k));
            }
            for r in &gb.records {
                if let Ok(rec) = EpochRecord::decode(r) {
                    let _ = self.insert_epoch(g, rec, false);
                }
            }
            for (d, generation, snap) in &gb.docs {
                self.restore_doc(&g, d, *generation, snap)?;
            }
            let head = self.groups.get(&g).and_then(|s| s.epochs.working_head());
            match (&gb.recovery, head) {
                (Some((invite_id, expires)), Some(head)) if !gb.other_members.is_empty() && super::now_ms() < *expires => {
                    let key = self.groups[&g].keys.get(&head).cloned().ok_or_else(|| Error::NotFound("head key".into()))?;
                    let rec = InviteRecord {
                        kind: CodeKind::Invite,
                        group: g,
                        epoch: head,
                        epoch_key: Some(key.0),
                        door: None,
                        invite_id: *invite_id,
                        expires_ms: *expires,
                        group_name: String::new(),
                        inviter_name: String::new(),
                        whereabouts: None,
                        published_ms: super::now_ms(),
                        hints: self.invite_hints_for_restore(&g),
                        point: None,
                    };
                    self.begin_join(&rec)?;
                    out.push(Restored::Rejoining(g));
                }
                _ => out.push(Restored::ReadOnly(g)),
            }
        }
        Ok(out)
    }

    /// Load a document snapshot from a file (untrusted input): validated in the import
    /// sandbox before this process parses it.
    pub fn load_untrusted_snapshot(&self, snapshot: &[u8]) -> Result<LoroDoc> {
        let verdict = crate::sandbox::validate(&self.sandbox, &[], snapshot)?;
        if !matches!(verdict, crate::sandbox::Verdict::Ok { .. }) {
            return Err(Error::Rejected("the file contains an invalid document".into()));
        }
        LoroDoc::from_snapshot(snapshot).map_err(|e| Error::Loro(e.to_string()))
    }

    fn restore_doc(&mut self, g: &GroupId, d: &DocId, generation: u32, snapshot: &[u8]) -> Result<()> {
        let doc = self.load_untrusted_snapshot(snapshot)?;
        doc.set_peer_id(self.identity.peer_id()).map_err(|e| Error::Loro(e.to_string()))?;
        std::fs::create_dir_all(self.layout.doc_dir(g, d))?;
        crate::storage::write_atomic(&self.layout.doc_snapshot(g, d), snapshot)?;
        crate::storage::write_atomic(&self.layout.doc_dir(g, d).join("generation"), &generation.to_le_bytes())?;
        // Who wrote what before the file's last rebuild is not in it (the device's copy is gone).
        let _ = std::fs::remove_file(self.layout.doc_dir(g, d).join("authors"));
        let state = self.groups.get_mut(g).ok_or_else(|| Error::NotFound("group".into()))?;
        state.docs.insert(*d, super::new_doc_state(doc, generation));
        Ok(())
    }

    fn invite_hints_for_restore(&self, g: &GroupId) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for layer in ["nostr", "helpers"] {
            if let Some(set) = self.mailbox_set(g, layer).0 {
                for e in set.endpoints.into_iter().take(2) {
                    out.push((layer.to_string(), e));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Backup {
        Backup { created_ms: 1, old_device: DeviceId([7; 32]), groups: Vec::new(), app: b"secret app data".to_vec() }
    }

    #[test]
    fn a_password_lock_opens_only_with_its_password() {
        let lock = BackupLock::new("correct horse").unwrap();
        let file = seal_locked(&sample(), Some(&lock)).unwrap();
        // pinned: format KTB2 is the password-locked backup (audit T11); KTB1 stays the plain one
        assert_eq!(&file[..4], b"KTB2");
        assert!(needs_password(&file));
        assert!(!file.windows(6).any(|w| w == b"secret"), "nothing in the clear");
        assert!(open(&file).is_err());
        assert!(open_with(&file, Some("wrong horse")).is_err());
        assert_eq!(open_with(&file, Some("correct horse")).unwrap().app, sample().app);
        // The lock survives being kept on the device; a plain file still opens without one.
        let kept = BackupLock::from_bytes(&lock.to_bytes()).unwrap();
        assert!(open_with(&seal_locked(&sample(), Some(&kept)).unwrap(), Some("correct horse")).is_ok());
        let plain = seal_locked(&sample(), None).unwrap();
        assert!(!needs_password(&plain));
        assert_eq!(open(&plain).unwrap().app, sample().app);
        // A file naming an absurd Argon2 cost is refused before any work.
        let mut greedy = file.clone();
        greedy[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(open_with(&greedy, Some("correct horse")).is_err());
    }
}
