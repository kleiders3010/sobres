//! Invite and device-link codes (DESIGN §5, D6, D7, D8, D55).
//!
//! - A code is **8 words** from a curated EFF short list 2.0 (`data/invite_words.txt`,
//!   1,106 words, 80.9 bits): every word is identified by its first 3 letters and no
//!   single typo turns one word into another valid word (E7).
//! - **Derivation:** Argon2id (m = 64 MiB, t = 3, p = 1) over the code with a **date salt**
//!   (UTC day) gives the day's lookup key and record key. Guessing only works against
//!   records of the same day (A1).
//! - **Records** are sealed with the record key; the lookup key names where they live on
//!   each layer. Invite and link records use separate derivation domains *and* carry
//!   their kind, so a link code is never accepted on the join path (D6) and gets a plain
//!   explanation instead.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::crypto::{self, SymKey};
use crate::ids::{EpochId, GroupId};
use crate::{wire, Error, Result};

pub const CODE_WORDS: usize = 8;
const RECORD_TAG: [u8; 2] = *b"IR";
/// Argon2id parameters, versioned (D9: 64 MiB floor).
pub const ARGON2_M_KIB: u32 = 64 * 1024;
pub const ARGON2_T: u32 = 3;
pub const ARGON2_P: u32 = 1;

static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();

pub fn words() -> &'static [&'static str] {
    WORDS.get_or_init(|| include_str!("../data/invite_words.txt").lines().map(str::trim).filter(|w| !w.is_empty()).collect())
}

/// An invite or link code: 8 word indices.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Code(pub [u16; CODE_WORDS]);

impl std::fmt::Debug for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Code(..)")
    }
}

impl Code {
    pub fn generate() -> Code {
        use rand_core::{OsRng, RngCore};
        let n = words().len() as u32;
        let mut out = [0u16; CODE_WORDS];
        for w in out.iter_mut() {
            // Rejection sampling: uniform over the list.
            let limit = u32::MAX - (u32::MAX % n);
            loop {
                let x = OsRng.next_u32();
                if x < limit {
                    *w = (x % n) as u16;
                    break;
                }
            }
        }
        Code(out)
    }

    pub fn to_words(&self) -> Vec<&'static str> {
        self.0.iter().map(|i| words()[*i as usize]).collect()
    }

    pub fn to_text(&self) -> String {
        self.to_words().join(" ")
    }

    /// Parse user input: full words or their first 3+ letters, any case, separated by
    /// spaces, dashes, dots, commas or plus signs (so links can carry `a+b+c`).
    pub fn parse(text: &str) -> Result<Code> {
        let parts: Vec<&str> = text.split(|c: char| c.is_whitespace() || ",.+-_/".contains(c)).filter(|p| !p.is_empty()).collect();
        if parts.len() != CODE_WORDS {
            return Err(Error::Invalid(format!("a code has {CODE_WORDS} words, this has {}", parts.len())));
        }
        let mut out = [0u16; CODE_WORDS];
        for (i, p) in parts.iter().enumerate() {
            out[i] = complete(p).ok_or_else(|| Error::Invalid(format!("word {} (\"{p}\") isn't in the list", i + 1)))?.0;
        }
        Ok(Code(out))
    }

    fn canonical(&self) -> Vec<u8> {
        self.0.iter().flat_map(|i| i.to_le_bytes()).collect()
    }

    /// Stable identifier of the invite (not secret-derived beyond the code itself).
    pub fn id(&self) -> [u8; 32] {
        crypto::hash("kt/invite-id/v1", &self.canonical())
    }
}

/// Autocomplete: the word a prefix (≥ 3 letters) or full word identifies.
pub fn complete(prefix: &str) -> Option<(u16, &'static str)> {
    let p = prefix.to_ascii_lowercase();
    if p.len() < 3 {
        return None;
    }
    let ws = words();
    let hits: Vec<usize> = (0..ws.len()).filter(|i| ws[*i].starts_with(&p)).collect();
    if hits.len() == 1 && (p.len() == 3 || ws[hits[0]].starts_with(&p)) {
        Some((hits[0] as u16, ws[hits[0]]))
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CodeKind {
    Invite,
    Link,
}

impl CodeKind {
    fn domain(self) -> &'static str {
        match self {
            CodeKind::Invite => "kt/invite/v1",
            CodeKind::Link => "kt/link/v1",
        }
    }
}

/// UTC day number (days since 1970-01-01).
pub fn day_of(ms: u64) -> u32 {
    (ms / 86_400_000) as u32
}

/// The day's lookup key (where the record lives) and record key (how it's sealed).
/// Costs one Argon2id run (~0.2–0.8 s on a phone).
pub fn derive(code: &Code, kind: CodeKind, day: u32) -> Result<([u8; 32], SymKey)> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(ARGON2_M_KIB, ARGON2_T, ARGON2_P, Some(64)).map_err(|e| Error::Invalid(e.to_string()))?;
    let a = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut salt = kind.domain().as_bytes().to_vec();
    salt.extend_from_slice(&day.to_le_bytes());
    let mut out = [0u8; 64];
    a.hash_password_into(&code.canonical(), &salt, &mut out).map_err(|e| Error::Invalid(e.to_string()))?;
    let lookup = crypto::hash("kt/invite-lookup/v1", &out[..32]);
    let key = crypto::derive(&out[32..], "kt/invite-key/v1", &day.to_le_bytes());
    Ok((lookup, key))
}

/// What an invite (or link) record tells the joiner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRecord {
    pub kind: CodeKind,
    pub group: GroupId,
    pub epoch: EpochId,
    pub epoch_key: [u8; 32],
    pub invite_id: [u8; 32],
    pub expires_ms: u64,
    pub group_name: String,
    pub inviter_name: String,
    /// The inviter's (or re-poster's) whereabouts, for a direct connection.
    pub whereabouts: Option<Vec<u8>>,
    /// When this copy was made: joiners prefer the newest (most current epoch key).
    pub published_ms: u64,
    /// A few of the group's endpoints per layer (e.g. ("nostr", relay URL), ("helpers",
    /// helper)), so the joiner writes its request where members look and knows a helper
    /// from the start, even without Nostr.
    pub hints: Vec<(String, String)>,
}

pub fn seal_record(r: &InviteRecord, lookup: &[u8; 32], key: &SymKey) -> Result<Vec<u8>> {
    let b = wire::encode(RECORD_TAG, 1, r)?;
    Ok(crypto::seal(key, lookup, &b))
}

pub fn open_record(sealed: &[u8], lookup: &[u8; 32], key: &SymKey) -> Result<InviteRecord> {
    let b = crypto::open(key, lookup, sealed)?;
    Ok(wire::decode(RECORD_TAG, 1, &b)?.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_parse_from_words_and_prefixes() {
        assert_eq!(words().len(), 1106);
        let c = Code::generate();
        let text = c.to_text();
        assert_eq!(Code::parse(&text).unwrap(), c);
        let prefixes: Vec<String> = c.to_words().iter().map(|w| w[..3].to_uppercase()).collect();
        assert_eq!(Code::parse(&prefixes.join("-")).unwrap(), c);
        assert!(Code::parse("abc def").is_err());
        assert!(complete("zz").is_none());
        assert_eq!(complete("aar").map(|x| x.1), Some("aardvark"));
        // Removed words are gone.
        assert!(!words().contains(&"zombie") && !words().contains(&"john"));
    }

    #[test]
    fn records_derive_per_day_and_kind() {
        let c = Code::generate();
        let (l1, k1) = derive(&c, CodeKind::Invite, 20_000).unwrap();
        let (l2, _) = derive(&c, CodeKind::Invite, 20_001).unwrap();
        let (l3, _) = derive(&c, CodeKind::Link, 20_000).unwrap();
        assert!(l1 != l2 && l1 != l3, "lookup keys differ by day and kind");
        let r = InviteRecord {
            kind: CodeKind::Invite,
            group: GroupId([1; 32]),
            epoch: EpochId([2; 32]),
            epoch_key: [3; 32],
            invite_id: c.id(),
            expires_ms: 5,
            group_name: "Family".into(),
            inviter_name: "Alex".into(),
            whereabouts: None,
            published_ms: 1,
            hints: vec![],
        };
        let s = seal_record(&r, &l1, &k1).unwrap();
        assert_eq!(open_record(&s, &l1, &k1).unwrap(), r);
        assert!(open_record(&s, &l2, &k1).is_err());
    }
}
