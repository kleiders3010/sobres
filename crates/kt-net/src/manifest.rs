//! Operator manifest (DESIGN §7b, D56): optional developer-provided capacity.
//!
//! - A signed file listing *additional* candidates: Nostr relays, iroh relays, helper
//!   nodes, each with a priority weight. There is no field that can disable or remove
//!   anything: the effective lists are always baseline ∪ manifest.
//! - Signed with a separate **offline** Ed25519 operator key; only its public key ships in
//!   the app. A compromised website, GitHub account or Nostr key can't forge it.
//! - Fetched from several sources (HTTPS URLs, Nostr); the newest valid version wins and
//!   the last good copy is kept on disk.
//! - Nostr: anyone can publish events with the manifest tag, but only a valid operator
//!   signature inside counts, so spam can't displace it (readers take the highest version).

use std::path::PathBuf;
use std::sync::Mutex;

use kt_engine::crypto;
use serde::{Deserialize, Serialize};

pub const MANIFEST_KIND: u16 = 35712;
/// The operator key's public half (created 2026-09-25; the sealed private key and its tool
/// live offline, see `kt-cli/src/operator.rs`).
pub const OPERATOR_PUBLIC_KEY: [u8; 32] = [
    0x22, 0x0f, 0x32, 0xe5, 0xe4, 0x28, 0x84, 0x51, 0xe4, 0xb2, 0x31, 0x71, 0x90, 0x88, 0xdd, 0xf7, 0x72, 0x18, 0xbf, 0xdb, 0x96, 0x55, 0xcb, 0x7a, 0xde, 0xac, 0x59, 0x3d, 0x70, 0xc8,
    0xaf, 0x12,
];
pub const MANIFEST_TOPIC_TAG: &str = "kt-operator-manifest";
const TAG: [u8; 2] = *b"OM";
const MAX_MANIFEST: usize = 256 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Weighted<T> {
    pub item: T,
    /// Higher = preferred. Select nodes still earn trust like any helper (§7a).
    pub weight: u16,
}

/// Format 2 (format 1 is still read: the fields below `helpers` default to empty).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestBody {
    /// Monotonic; a lower version never replaces a higher one.
    pub version: u64,
    pub issued_ms: u64,
    pub nostr_relays: Vec<Weighted<String>>,
    pub iroh_relays: Vec<Weighted<String>>,
    /// Helper / select nodes by iroh endpoint ID.
    pub helpers: Vec<Weighted<[u8; 32]>>,
    /// Developer-run slot stores ([`crate::cloud`]).
    #[serde(default)]
    pub cloud_mailboxes: Vec<Weighted<String>>,
    /// How devices use them (none given: the baseline's).
    #[serde(default)]
    pub cloud_mode: Option<crate::cloud::CloudMode>,
    /// Blossom servers for attachments ([`crate::blossom`]).
    #[serde(default)]
    pub blossom_servers: Vec<Weighted<String>>,
    /// Versions that must update (security fixes, DESIGN principle 8): only ever from this
    /// signed file or Play's update priority, never from peer data.
    #[serde(default)]
    pub required: Vec<RequiredUpdate>,
}

/// "Builds on `channel` below `below_version` must update."
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredUpdate {
    /// As in the update manifest: "play", "apk", "windows", "linux".
    pub channel: String,
    pub below_version: u64,
    /// Shown to the user, e.g. "fixes a security problem".
    pub reason: String,
}

/// Format 1, still read.
#[derive(Deserialize)]
struct ManifestBodyV1 {
    version: u64,
    issued_ms: u64,
    nostr_relays: Vec<Weighted<String>>,
    iroh_relays: Vec<Weighted<String>>,
    helpers: Vec<Weighted<[u8; 32]>>,
}

/// Where devices fetch the manifest (all are tried; only the signature counts).
pub const MANIFEST_URLS: [&str; 2] = ["https://apps.jurisgames.com/operator.bin", "https://raw.githubusercontent.com/kleiders3010/jurisgames/main/operator.bin"];

/// What the app ships with: the cloud mailbox and the Blossom seed servers. Anything the
/// operator publishes later only adds to this.
pub fn baseline() -> ManifestBody {
    ManifestBody {
        cloud_mailboxes: vec![Weighted { item: crate::cloud::DEFAULT_URL.into(), weight: 100 }],
        cloud_mode: Some(crate::cloud::CloudMode::Always),
        blossom_servers: crate::blossom::SEED_SERVERS.iter().map(|s| Weighted { item: s.to_string(), weight: 10 }).collect(),
        ..Default::default()
    }
}

#[derive(Serialize, Deserialize)]
struct Signed {
    body: Vec<u8>,
    sig: Vec<u8>,
}

/// Sign a manifest (operator tooling only; the key never ships in the app).
pub fn sign(body: &ManifestBody, operator: &crypto::DeviceSecret) -> Vec<u8> {
    let b = kt_engine::wire::encode(TAG, 2, body).expect("encodable");
    let sig = operator.sign(&b).to_vec();
    postcard::to_stdvec(&Signed { body: b, sig }).expect("encodable")
}

/// Verify a manifest against the operator public key (untrusted input).
pub fn verify(bytes: &[u8], operator_pub: &[u8; 32]) -> Result<ManifestBody, String> {
    if bytes.len() > MAX_MANIFEST {
        return Err("manifest too large".into());
    }
    let s: Signed = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
    let sig: [u8; 64] = s.sig.as_slice().try_into().map_err(|_| "bad signature length".to_string())?;
    crypto::verify(operator_pub, &s.body, &sig).map_err(|e| e.to_string())?;
    match s.body.get(2..4) {
        Some([1, 0]) => {
            let v1: ManifestBodyV1 = kt_engine::wire::decode(TAG, 1, &s.body).map_err(|e| e.to_string())?.1;
            Ok(ManifestBody { version: v1.version, issued_ms: v1.issued_ms, nostr_relays: v1.nostr_relays, iroh_relays: v1.iroh_relays, helpers: v1.helpers, ..Default::default() })
        }
        _ => Ok(kt_engine::wire::decode(TAG, 2, &s.body).map_err(|e| e.to_string())?.1),
    }
}

/// The app's view: shipped baseline plus the newest valid manifest seen.
pub struct Manifest {
    operator_pub: [u8; 32],
    baseline: ManifestBody,
    current: Mutex<Option<(ManifestBody, Vec<u8>)>>,
    path: Option<PathBuf>,
}

impl Manifest {
    /// `path` keeps the last good copy across restarts.
    pub fn new(operator_pub: [u8; 32], baseline: ManifestBody, path: Option<PathBuf>) -> Manifest {
        let current = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| verify(&b, &operator_pub).ok().map(|m| (m, b)));
        Manifest { operator_pub, baseline, current: Mutex::new(current), path }
    }

    /// Consider a fetched manifest. Returns true if it became the current one.
    pub fn consider(&self, bytes: &[u8]) -> bool {
        let Ok(m) = verify(bytes, &self.operator_pub) else { return false };
        let mut cur = self.current.lock().expect("lock");
        let newer = cur.as_ref().is_none_or(|(c, _)| m.version > c.version) && m.version > self.baseline.version;
        if !newer {
            return false;
        }
        if let Some(p) = &self.path {
            let _ = kt_engine::storage::write_atomic(p, bytes);
        }
        *cur = Some((m, bytes.to_vec()));
        true
    }

    pub fn version(&self) -> u64 {
        self.current.lock().expect("lock").as_ref().map(|(m, _)| m.version).unwrap_or(self.baseline.version)
    }

    /// Baseline ∪ manifest, highest weight first, de-duplicated. Only ever adds.
    pub fn effective(&self) -> ManifestBody {
        let cur = self.current.lock().expect("lock").as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
        fn merge<T: Clone + PartialEq>(a: &[Weighted<T>], b: &[Weighted<T>]) -> Vec<Weighted<T>> {
            let mut out: Vec<Weighted<T>> = Vec::new();
            for w in a.iter().chain(b.iter()) {
                match out.iter_mut().find(|x| x.item == w.item) {
                    Some(x) => x.weight = x.weight.max(w.weight),
                    None => out.push(w.clone()),
                }
            }
            out.sort_by(|x, y| y.weight.cmp(&x.weight));
            out
        }
        ManifestBody {
            version: cur.version.max(self.baseline.version),
            issued_ms: cur.issued_ms.max(self.baseline.issued_ms),
            nostr_relays: merge(&self.baseline.nostr_relays, &cur.nostr_relays),
            iroh_relays: merge(&self.baseline.iroh_relays, &cur.iroh_relays),
            helpers: merge(&self.baseline.helpers, &cur.helpers),
            cloud_mailboxes: merge(&self.baseline.cloud_mailboxes, &cur.cloud_mailboxes),
            cloud_mode: cur.cloud_mode.or(self.baseline.cloud_mode),
            blossom_servers: merge(&self.baseline.blossom_servers, &cur.blossom_servers),
            required: cur.required.clone(),
        }
    }

    /// The reason this build must update, if the manifest says so.
    pub fn required_update(&self, channel: &str, version: u64) -> Option<String> {
        self.effective().required.into_iter().find(|r| r.channel == channel && version < r.below_version).map(|r| r.reason)
    }

    /// Fetch from HTTPS sources; every source is tried, failures are ignored.
    pub async fn fetch_http(&self, urls: &[&str]) -> usize {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let Some(client) = crate::http::client(std::time::Duration::from_secs(20)) else { return 0 };
        let mut accepted = 0;
        for u in urls {
            let Ok(resp) = client.get(*u).send().await else { continue };
            if !resp.status().is_success() {
                continue;
            }
            if let Some(b) = crate::http::bytes_capped(resp, 1024 * 1024).await {
                if self.consider(&b) {
                    accepted += 1;
                }
            }
        }
        accepted
    }

    /// Fetch from Nostr relays: events of [`MANIFEST_KIND`] tagged [`MANIFEST_TOPIC_TAG`],
    /// from any author (only the operator signature inside matters).
    pub async fn fetch_nostr(&self, relays: &[std::sync::Arc<crate::nostr::RelayConn>]) -> usize {
        use base64::Engine as _;
        use nostr::prelude::*;
        let filter = Filter::new().kind(Kind::Custom(MANIFEST_KIND)).hashtag(MANIFEST_TOPIC_TAG).limit(50);
        let mut accepted = 0;
        for r in relays {
            let Some(evs) = r.query(vec![filter.clone()]).await else { continue };
            for ev in evs {
                if ev.verify().is_err() {
                    continue;
                }
                if let Ok(b) = base64::engine::general_purpose::STANDARD.decode(&ev.content) {
                    if self.consider(&b) {
                        accepted += 1;
                    }
                }
            }
        }
        accepted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(v: u64, relay: &str) -> ManifestBody {
        ManifestBody {
            version: v,
            issued_ms: v,
            nostr_relays: vec![Weighted { item: relay.into(), weight: 5 }],
            iroh_relays: vec![],
            helpers: vec![Weighted { item: [7; 32], weight: 9 }],
            ..Default::default()
        }
    }

    #[test]
    fn format_1_still_reads_and_format_2_adds() {
        let op = crypto::DeviceSecret::generate();
        // A format-1 manifest, as the tool wrote them before.
        #[derive(Serialize)]
        struct V1 {
            version: u64,
            issued_ms: u64,
            nostr_relays: Vec<Weighted<String>>,
            iroh_relays: Vec<Weighted<String>>,
            helpers: Vec<Weighted<[u8; 32]>>,
        }
        let b = kt_engine::wire::encode(TAG, 1, &V1 { version: 4, issued_ms: 1, nostr_relays: vec![], iroh_relays: vec![], helpers: vec![] }).unwrap();
        let signed = postcard::to_stdvec(&Signed { sig: op.sign(&b).to_vec(), body: b }).unwrap();
        assert_eq!(verify(&signed, &op.public().sign).unwrap().version, 4);

        let m = Manifest::new(op.public().sign, baseline(), None);
        assert_eq!(m.effective().cloud_mailboxes[0].item, crate::cloud::DEFAULT_URL, "baseline ships the cloud mailbox");
        let v2 = ManifestBody {
            version: 5,
            cloud_mode: Some(crate::cloud::CloudMode::WhenHelpersWeak),
            cloud_mailboxes: vec![Weighted { item: "https://second.example".into(), weight: 50 }],
            required: vec![RequiredUpdate { channel: "play".into(), below_version: 7, reason: "fixes a security problem".into() }],
            ..Default::default()
        };
        assert!(m.consider(&sign(&v2, &op)));
        let e = m.effective();
        assert_eq!(e.cloud_mailboxes.len(), 2, "adds, never removes");
        assert_eq!(e.cloud_mode, Some(crate::cloud::CloudMode::WhenHelpersWeak));
        assert_eq!(m.required_update("play", 6).as_deref(), Some("fixes a security problem"));
        assert_eq!(m.required_update("play", 7), None);
        assert_eq!(m.required_update("github", 1), None);
    }

    #[test]
    fn operator_public_key_is_the_real_one() {
        assert_eq!(hex::encode(OPERATOR_PUBLIC_KEY), "220f32e5e4288451e4b231719088ddf77218bfdb9655cb7adeac593d70c8af12");
        assert!(iroh::PublicKey::from_bytes(&OPERATOR_PUBLIC_KEY).is_ok(), "a valid Ed25519 point");
    }

    #[test]
    fn only_newer_validly_signed_manifests_apply_and_they_only_add() {
        let op = crypto::DeviceSecret::generate();
        let other = crypto::DeviceSecret::generate();
        let baseline = body(1, "wss://base.example");
        let dir = std::env::temp_dir().join(format!("kt-manifest-{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest.bin");
        let m = Manifest::new(op.public().sign, baseline, Some(path.clone()));
        assert!(!m.consider(&sign(&body(5, "wss://evil"), &other)), "wrong key");
        assert!(!m.consider(b"garbage"));
        assert!(m.consider(&sign(&body(3, "wss://new.example"), &op)));
        assert!(!m.consider(&sign(&body(2, "wss://older"), &op)), "no downgrade");
        let e = m.effective();
        let relays: Vec<_> = e.nostr_relays.iter().map(|w| w.item.as_str()).collect();
        assert!(relays.contains(&"wss://base.example") && relays.contains(&"wss://new.example"), "baseline kept: {relays:?}");
        // Last good copy survives a restart.
        let m2 = Manifest::new(op.public().sign, body(1, "wss://base.example"), Some(path));
        assert_eq!(m2.version(), 3);
        // Tampering breaks the signature.
        let mut t = sign(&body(9, "wss://x"), &op);
        let n = t.len();
        t[n / 2] ^= 1;
        assert!(!m2.consider(&t));
    }
}
