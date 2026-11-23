//! Kotlin bridge (UniFFI) for Kleiders Tasks.
//!
//! One [`KtNode`] per device: the app layer (groups, lists, tasks, invites) plus every
//! transport (Nostr with the self-sustaining relay list, P2P, DHT, helpers), running on an
//! internal tokio runtime. Kotlin calls are blocking and short; network work happens in
//! [`KtNode::sync_now`] (background jobs) or in live mode ([`KtNode::start_live`], app open),
//! which calls [`KtListener::on_change`] whenever something arrived.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kt_engine::invite::CodeKind;
use kt_engine::replica::Event;
use kt_engine::sandbox::SandboxConfig;
use kt_engine::{DocId, GroupId};
use kt_net::dht::DhtDiscovery;
use kt_net::helper::HelperMailbox;
use kt_net::helper_dir::HelperDirectory;
use kt_net::invite_net::InviteLayers;
use kt_net::mailbox::Mailbox;
use kt_net::nostr::{NostrMailbox, NostrPolicy};
use kt_net::p2p::{P2p, P2pConfig};
use kt_net::relays::RelayDirectory;
use kt_net::settings::{NetSettings, Platform as NetPlatform};
use kt_net::sync::Syncer;
use kt_tasks::app::{App, ListInfo};

uniffi::setup_scaffolding!();

#[cfg(target_os = "android")]
mod android;
mod vault;
mod budget;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum KtError {
    #[error("{msg}")]
    Failed { msg: String },
}

fn fail<E: std::fmt::Display>(e: E) -> KtError {
    KtError::Failed { msg: e.to_string() }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Platform {
    Desktop,
    Phone,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct NodeOptions {
    pub platform: Platform,
    /// Nostr seed relays (empty = the shipped seed list).
    pub relays: Vec<String>,
    /// Direct device-to-device sync (iroh). Off in some tests.
    pub p2p: bool,
    /// Mainline DHT discovery.
    pub dht: bool,
    /// Bind address for P2P ("" = all interfaces; tests use "127.0.0.1:0").
    pub bind: String,
    /// This device's name for the account's other devices ("Pixel 7", the host name).
    pub device_name: String,
    /// Where the daily sealed backup goes ("" = none). Android: a folder that Android
    /// backup includes (and nothing else). A backup found here on a fresh install is
    /// restored automatically.
    pub backup_dir: String,
}

/// What a link code joins, shown before the user confirms (D6).
#[derive(Clone, Debug, uniffi::Record)]
pub struct LinkPreview {
    /// The account's display name.
    pub account_name: String,
    /// Its devices and lists: "Pixel 7 · Mark's PC · 12 lists (Work, Groceries, …)".
    pub summary: String,
}

/// A device of this account.
#[derive(Clone, Debug, uniffi::Record)]
pub struct DeviceItem {
    pub id: String,
    pub name: String,
    pub this_device: bool,
}

/// A device linked recently ("New device linked · Undo").
#[derive(Clone, Debug, uniffi::Record)]
pub struct RecentLinkItem {
    pub device_id: String,
    pub name: String,
    pub linked_ms: u64,
}

/// Something another person did (for notifications; batched per list by the platform).
#[derive(Clone, Debug, uniffi::Record)]
pub struct ChangeItem {
    pub group_name: String,
    /// Empty for joins.
    pub list_id: String,
    pub list_name: String,
    pub who: String,
    /// "added", "completed", "entered" (budget money, see `cents`) or "joined".
    pub kind: String,
    pub title: String,
    pub at_ms: u64,
    /// "entered": the amount in cents (income negative) and the budget's currency symbol.
    pub cents: i64,
    pub currency: String,
}

/// The settings screen (DESIGN settings table). A setting that doesn't exist on this
/// platform is `None` and isn't shown at all.
#[derive(Clone, Debug, uniffi::Record)]
pub struct Settings {
    /// Off = sync only while the app is open.
    pub sync_in_background: bool,
    /// "Notify me when others change a list".
    pub notify_changes: bool,
    /// "Help others sync": one of `help_options`.
    pub help_others: String,
    /// "wifi_and_charging", "wifi_while_open" (phones), "on" (desktops), "off".
    pub help_options: Vec<String>,
    /// "Relay connections for others" (D51): desktops only.
    pub relay_for_others: Option<bool>,
    /// "Start with computer": desktops only.
    pub start_with_computer: Option<bool>,
    /// Appearance: a theme ID from [`themes`].
    pub theme: String,
    /// Material You (wallpaper colors): phones only; the app shows it on Android 12+.
    pub dynamic_color: Option<bool>,
}

/// This device's settings file (format 2: adds the appearance choice).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StoredSettings {
    sync_in_background: bool,
    notify_changes: bool,
    net: NetSettings,
    start_with_computer: bool,
    /// Per device, never synced (DESIGN §2 appearance).
    theme: String,
    dynamic_color: bool,
}

/// Format 1, still read.
#[derive(serde::Deserialize)]
struct StoredSettingsV1 {
    sync_in_background: bool,
    notify_changes: bool,
    net: NetSettings,
    start_with_computer: bool,
}

impl StoredSettings {
    fn defaults(p: NetPlatform) -> StoredSettings {
        StoredSettings {
            sync_in_background: true,
            notify_changes: true,
            net: NetSettings::defaults(p),
            start_with_computer: true,
            theme: "default".into(),
            dynamic_color: false,
        }
    }

    fn read(b: &[u8]) -> Option<StoredSettings> {
        match b.get(2..4).map(|v| u16::from_le_bytes([v[0], v[1]])) {
            Some(1) => kt_engine::wire::decode::<StoredSettingsV1>(*b"SE", 1, b).ok().map(|(_, o)| StoredSettings {
                sync_in_background: o.sync_in_background,
                notify_changes: o.notify_changes,
                start_with_computer: o.start_with_computer,
                ..StoredSettings::defaults(o.net.platform())
            }
            .with_net(o.net)),
            _ => kt_engine::wire::decode::<StoredSettings>(*b"SE", 2, b).ok().map(|x| x.1),
        }
    }

    fn with_net(mut self, net: NetSettings) -> StoredSettings {
        self.net = net;
        self
    }
}

fn help_name(h: kt_net::settings::HelpOthers) -> &'static str {
    use kt_net::settings::HelpOthers::*;
    match h {
        WifiAndCharging => "wifi_and_charging",
        WifiWhileOpen => "wifi_while_open",
        On => "on",
        Off => "off",
    }
}

/// A newer version is out (from the static update manifest).
#[derive(Clone, Debug, uniffi::Record)]
pub struct UpdateItem {
    pub version_name: String,
    /// Our own store listing or release page, nothing else.
    pub link: String,
}

/// Someone who edits a list (for "Undo changes by [person] since [time]").
#[derive(Clone, Debug, uniffi::Record)]
pub struct PersonItem {
    pub name: String,
    /// This account's own devices.
    pub is_me: bool,
    /// Generated avatar: initials on this hue, plus a short tag telling apart people with
    /// the same name ("Alex · 4F2A").
    pub initials: String,
    pub hue: f64,
    pub tag: String,
}

/// What a restore did.
#[derive(Clone, Debug, uniffi::Record)]
pub struct RestoreInfo {
    /// Shared groups: readable now, back in when a member's device sees the request.
    pub rejoining_groups: u32,
    /// Solo lists copied into this device's own lists.
    pub rehomed_lists: u32,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GroupItem {
    pub id: String,
    pub name: String,
    pub personal: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ListItem {
    pub id: String,
    pub group_id: String,
    pub name: String,
    pub group_name: String,
    /// HCT hue 0–360 (the theme picks the tone: [`accent_color`]); `None` is grey.
    pub hue: Option<f64>,
    /// 0 soft, 1 normal, 2 vivid.
    pub vivid: u8,
    /// An icon ID from [`list_icons`], if set.
    pub icon: Option<String>,
    /// "tasks" or "shopping" (shopping lists show completed items at the bottom).
    pub list_type: String,
}

/// A task in display order, with its depth (children follow their parent).
#[derive(Clone, Debug, uniffi::Record)]
pub struct TaskItem {
    pub id: String,
    pub parent_id: Option<String>,
    pub depth: u32,
    pub title: String,
    pub done: bool,
    /// Inline notes, or an attachment's preview (full text: [`KtNode::notes`]).
    pub notes: String,
    /// The notes continue in a text attachment.
    pub has_attachment: bool,
    /// Attached images, by ID (bytes from [`KtNode::task_image`]).
    pub images: Vec<String>,
    /// A timed reminder (ms since the epoch).
    pub remind_ms: Option<u64>,
    /// How it repeats ("wd", "1d", "2w", "1m", "1y"); none for a one-off.
    pub repeat: Option<String>,
    /// When it was starred (ms since the epoch); none if it isn't.
    pub starred_ms: Option<u64>,
    /// The day ("YYYY-MM-DD") this person put it in their My Day; none if they didn't.
    pub my_day: Option<String>,
    /// Who it's assigned to (shared lists), by their current name when they're still in the list.
    pub assigned: Option<PersonItem>,
    /// Assigned to this account (any of its devices).
    pub assigned_to_me: bool,
}

/// A task's full notes, as far as this device has them.
#[derive(Clone, Debug, uniffi::Record)]
pub struct NotesItem {
    pub text: String,
    /// False while an attachment is still on its way: `text` is its preview.
    pub complete: bool,
    pub size: u64,
}

/// An upcoming reminder (the platform schedules an alarm for each).
#[derive(Clone, Debug, uniffi::Record)]
pub struct ReminderItem {
    pub list_id: String,
    pub list_name: String,
    pub task_id: String,
    pub title: String,
    pub at_ms: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct InviteInfo {
    pub words: Vec<String>,
    pub expires_ms: u64,
    /// The same code as a link (and QR content), with this device's addresses when known.
    pub link: String,
}

/// A QR code as a square of modules (the UI draws it).
#[derive(Clone, Debug, uniffi::Record)]
pub struct QrMatrix {
    pub size: u32,
    /// Row by row, `size * size` entries; true = dark.
    pub dark: Vec<bool>,
}

/// Android: is the app Context registered for iroh's DNS resolver and network monitor?
/// (Always true elsewhere.)
#[uniffi::export]
pub fn android_context_ready() -> bool {
    #[cfg(target_os = "android")]
    return android::context_ready();
    #[cfg(not(target_os = "android"))]
    true
}

/// A theme's color roles as ARGB, plus its other design tokens.
#[derive(Clone, Debug, uniffi::Record)]
pub struct ThemeColors {
    pub primary: u32,
    pub on_primary: u32,
    pub primary_container: u32,
    pub on_primary_container: u32,
    pub secondary: u32,
    pub on_secondary: u32,
    pub secondary_container: u32,
    pub on_secondary_container: u32,
    pub tertiary: u32,
    pub on_tertiary: u32,
    pub error: u32,
    pub on_error: u32,
    pub background: u32,
    pub on_background: u32,
    pub surface: u32,
    pub on_surface: u32,
    pub surface_variant: u32,
    pub on_surface_variant: u32,
    pub surface_container: u32,
    pub surface_container_high: u32,
    pub outline: u32,
    pub outline_variant: u32,
    pub text_scale: f32,
    pub density: i8,
    pub corner_dp: u8,
}

fn theme_colors_of(t: &kt_tasks::appearance::ThemeTokens, dark: bool) -> ThemeColors {
    let c = kt_tasks::appearance::colors(t, dark);
    ThemeColors {
        primary: c.primary,
        on_primary: c.on_primary,
        primary_container: c.primary_container,
        on_primary_container: c.on_primary_container,
        secondary: c.secondary,
        on_secondary: c.on_secondary,
        secondary_container: c.secondary_container,
        on_secondary_container: c.on_secondary_container,
        tertiary: c.tertiary,
        on_tertiary: c.on_tertiary,
        error: c.error,
        on_error: c.on_error,
        background: c.background,
        on_background: c.on_background,
        surface: c.surface,
        on_surface: c.on_surface,
        surface_variant: c.surface_variant,
        on_surface_variant: c.on_surface_variant,
        surface_container: c.surface_container,
        surface_container_high: c.surface_container_high,
        outline: c.outline,
        outline_variant: c.outline_variant,
        text_scale: t.text_scale,
        density: t.density,
        corner_dp: t.corner_dp,
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ThemeItem {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct IconItem {
    pub id: String,
    pub symbol: String,
}

/// The built-in themes.
#[uniffi::export]
pub fn themes() -> Vec<ThemeItem> {
    kt_tasks::appearance::builtin_themes().into_iter().map(|t| ThemeItem { id: t.id, name: t.name }).collect()
}

/// The curated list hues for the color picker.
#[uniffi::export]
pub fn list_hues() -> Vec<f64> {
    kt_tasks::appearance::LIST_HUES.to_vec()
}

#[uniffi::export]
pub fn list_icons() -> Vec<IconItem> {
    kt_tasks::appearance::LIST_ICONS.iter().map(|(id, s)| IconItem { id: id.to_string(), symbol: s.to_string() }).collect()
}

/// The symbol for an icon ID (empty if unknown).
#[uniffi::export]
pub fn icon_symbol(id: String) -> String {
    kt_tasks::appearance::icon_symbol(&id).unwrap_or("").into()
}

/// A list's or avatar's accent color (ARGB) in a theme: the hue at the theme's tone.
#[uniffi::export]
pub fn accent_color(theme_id: String, hue: Option<f64>, vivid: u8, dark: bool) -> u32 {
    kt_tasks::appearance::accent(&kt_tasks::appearance::find_theme(&theme_id), hue, vivid, dark)
}

/// A safe file name (without extension) for exporting a list with this title (D60).
/// The backup file needs a password to open (audit T11).
#[uniffi::export]
pub fn backup_needs_password(backup: Vec<u8>) -> bool {
    kt_engine::replica::backup::needs_password(&backup)
}

/// The password opens this backup file (one Argon2id run, ~0.5 s: call off the UI thread).
#[uniffi::export]
pub fn backup_password_opens(backup: Vec<u8>, password: String) -> bool {
    kt_engine::replica::backup::open_with(&backup, Some(&password)).is_ok()
}

/// Shortest backup password accepted.
#[uniffi::export]
pub fn backup_password_min_chars() -> u32 {
    kt_engine::replica::backup::PW_MIN_CHARS as u32
}

#[uniffi::export]
pub fn export_file_stem(title: String) -> String {
    kt_tasks::export::file_stem(&title)
}

/// Encode text (a share link) as a QR code.
#[uniffi::export]
pub fn qr_matrix(text: String) -> Result<QrMatrix, KtError> {
    let q = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).map_err(fail)?;
    let size = q.width() as u32;
    let dark = q.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();
    Ok(QrMatrix { size, dark })
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct JoinResult {
    pub group_name: String,
    pub inviter_name: String,
    /// True if already in; false if waiting for a member's device to let us in.
    pub joined: bool,
    /// Not found yet: this device keeps looking (in the background too) and says when it's
    /// in. The names are empty until then.
    pub still_looking: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SyncReport {
    pub written: u32,
    pub received: u32,
    pub rejected: u32,
    pub peers_reached: u32,
    /// Groups this device got into this round (a join that was waiting, or a code that was
    /// still being looked for).
    pub joined_groups: Vec<String>,
    /// Codes given up on this round: never found within [`LOOKUP_FOR`].
    pub lookups_given_up: u32,
}

/// A code the device couldn't find the invite for yet (terrible connection, or the inviter
/// not online yet): rounds keep looking, in the background too.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PendingLookup {
    /// As entered: words or a link (links may carry the inviter's addresses).
    entered: String,
    started_ms: u64,
}

/// How long a code is looked for before giving up.
const LOOKUP_FOR: Duration = Duration::from_secs(24 * 3600);

/// One relay's health as this device sees it (hidden diagnostics screen).
#[derive(Clone, Debug, uniffi::Record)]
pub struct RelayStat {
    pub url: String,
    pub published_ok: u64,
    pub connect_failures: u64,
    pub failures_in_a_row: u64,
    pub readback_hits: u64,
    pub readback_misses: u64,
}

/// One layer's state, in words (hidden diagnostics screen).
#[derive(Clone, Debug, uniffi::Record)]
pub struct LayerStatus {
    pub name: String,
    pub state: String,
}

/// A storage helper as this device (and its groups) see it.
#[derive(Clone, Debug, uniffi::Record)]
pub struct HelperStat {
    /// Short form of the helper's endpoint ID.
    pub id: String,
    pub reachable: u32,
    pub unreachable: u32,
    pub latency_ms: f64,
    pub failing: bool,
    pub excluded: bool,
}

/// Disk used by one group's data on this device.
#[derive(Clone, Debug, uniffi::Record)]
pub struct GroupStorage {
    pub name: String,
    pub bytes: u64,
}

/// A device that joined one of our shared lists recently ([`KtNode::recent_joins`]).
#[derive(Clone, Debug, uniffi::Record)]
pub struct RecentJoinItem {
    pub group_id: String,
    pub device_id: String,
    /// The name it gave itself (may be empty) and who let it in.
    pub name: String,
    pub by: String,
    pub list_name: String,
    pub joined_ms: u64,
}

/// A shared group's sync state for the list header ([`KtNode::group_sync`]).
#[derive(Clone, Debug, uniffi::Record)]
pub struct GroupSync {
    /// This device's newest changes reached no shared mailbox in the last round.
    pub only_here: bool,
    /// When another current member last wrote (ms), if any has.
    pub others_last_ms: Option<u64>,
}

/// One sync round, for the sync log.
#[derive(Clone, Debug, uniffi::Record)]
pub struct RoundLog {
    pub at_ms: u64,
    pub took_ms: u64,
    pub written: u32,
    pub received: u32,
    pub rejected: u32,
    pub peers_reached: u32,
}

/// The hidden diagnostics screen (DESIGN §2): engineering state, never secrets or task
/// content, so all of it may go into "Export log".
#[derive(Clone, Debug, uniffi::Record)]
pub struct Diagnostics {
    pub version: String,
    pub feature_level: u32,
    pub layers: Vec<LayerStatus>,
    pub relays: Vec<RelayStat>,
    pub helper_relays: Vec<String>,
    pub helpers: Vec<HelperStat>,
    pub storage: Vec<GroupStorage>,
    pub rounds: Vec<RoundLog>,
}

/// Rounds kept for the sync log.
const ROUND_LOG: usize = 50;

fn dir_bytes(p: &std::path::Path) -> u64 {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .map(|e| match e.file_type() {
                    Ok(t) if t.is_dir() => dir_bytes(&e.path()),
                    Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
                    _ => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Something changed that the UI should show (lists, tasks, membership).
#[uniffi::export(with_foreign)]
pub trait KtListener: Send + Sync {
    fn on_change(&self);
}

struct Net {
    syncer: Arc<Syncer>,
    nostr: Arc<NostrMailbox>,
    relays: RelayDirectory,
    p2p: Option<Arc<P2p>>,
    helpers: Option<(Arc<HelperMailbox>, HelperDirectory)>,
    dht: Option<Arc<DhtDiscovery>>,
    /// Public Blossom servers for attachments (best effort; off for loopback tests).
    blossom: Option<kt_net::blossom::Blossom>,
    /// The operator manifest (off for loopback tests).
    manifest: Option<Arc<kt_net::manifest::Manifest>>,
    /// Cloud mailboxes in use, by URL.
    clouds: Mutex<std::collections::HashMap<String, Arc<kt_net::cloud::CloudMailbox>>>,
    /// Blossom servers found and verified by this device (beyond the manifest's).
    blossom_dir: Option<kt_net::blossom::BlossomDirectory>,
}

impl Net {
    /// Use what the manifest lists (baseline ∪ the newest signed copy): add cloud mailboxes
    /// not yet in use, set their mode, and the Blossom servers.
    fn apply_manifest(&self) {
        let Some(m) = &self.manifest else { return };
        let e = m.effective();
        let mode = e.cloud_mode.unwrap_or(kt_net::cloud::CloudMode::Always);
        let mut clouds = self.clouds.lock().expect("lock");
        for w in &e.cloud_mailboxes {
            if !w.item.starts_with("https://") || clouds.contains_key(&w.item) {
                continue;
            }
            if let Some(c) = kt_net::cloud::CloudMailbox::new(&w.item, mode) {
                let c = Arc::new(c);
                if let Some((h, _)) = &self.helpers {
                    c.set_helpers(h.clone());
                }
                self.syncer.add_mailbox(c.clone() as Arc<dyn Mailbox>);
                clouds.insert(w.item.clone(), c);
            }
        }
        for c in clouds.values() {
            c.set_mode(mode);
        }
        if let Some(b) = &self.blossom {
            // The manifest's first (weighted), then the ones this device discovered.
            let mut servers: Vec<String> = e.blossom_servers.iter().filter(|w| w.item.starts_with("https://")).map(|w| w.item.clone()).collect();
            for s in self.blossom_dir.as_ref().map(|d| d.verified()).unwrap_or_default() {
                if !servers.contains(&s) {
                    servers.push(s);
                }
            }
            b.set_servers(servers);
        }
    }

    /// Daily: fetch the manifest from every source (HTTPS, Nostr); apply it if it changed.
    async fn refresh_manifest(&self, last: &Mutex<Option<std::time::Instant>>) {
        let Some(m) = &self.manifest else { return };
        if last.lock().expect("lock").is_some_and(|t| t.elapsed() < MANIFEST_EVERY) {
            return;
        }
        *last.lock().expect("lock") = Some(std::time::Instant::now());
        let mut n = m.fetch_http(&kt_net::manifest::MANIFEST_URLS).await;
        let conns: Vec<_> = self.relays.ordered(&self.nostr).into_iter().take(3).map(|u| self.nostr.conn(&u)).collect();
        n += m.fetch_nostr(&conns).await;
        // Blossom servers published on Nostr (weekly, a few trials at a time).
        if let (Some(b), Some(d)) = (&self.blossom, &self.blossom_dir) {
            n += d.discover(b, &conns).await.len();
        }
        if n > 0 {
            self.apply_manifest();
        }
    }
}

/// Local edits go to the mailboxes once they pause this long...
const WRITE_PAUSE: Duration = Duration::from_secs(2);
/// ...but no more often than this (relays rate-limit, and a long note is many edits).
const WRITE_GAP: Duration = Duration::from_secs(10);

/// Whether local edits (last one at `changed_at`) should be written to the mailboxes now.
fn write_due(changed_at: Option<std::time::Instant>, last_write: std::time::Instant, now: std::time::Instant) -> bool {
    changed_at.is_some_and(|t| now.duration_since(t) >= WRITE_PAUSE) && now.duration_since(last_write) >= WRITE_GAP
}

/// Storage lines of the diagnostics (sent with feedback): "Your own" and "Shared 1", "Shared 2"..., never a name.
fn storage_labels(personal: &[bool]) -> Vec<String> {
    let mut shared = 0;
    personal
        .iter()
        .map(|p| {
            if *p {
                "Your own".to_string()
            } else {
                shared += 1;
                format!("Shared {shared}")
            }
        })
        .collect()
}

#[cfg(test)]
mod diagnostics_privacy {
    /// pinned: diagnostics carry no budget, list or group names (round 5, S7) and report the app's own version.
    #[test]
    fn no_names_and_the_apps_version() {
        assert_eq!(super::storage_labels(&[true, false, false]), ["Your own", "Shared 1", "Shared 2"]);
        let src = include_str!("lib.rs");
        let start = src.find("pub fn diagnostics(&self)").unwrap();
        let body = &src[start..start + src[start..].find("        Diagnostics {").unwrap()];
        assert!(!body.contains("g.name") && !body.contains("list_name"), "diagnostics must not read a name");
        let gradle = include_str!("../../../app/android/build.gradle.kts");
        assert!(gradle.contains(&format!("versionName = \"{}\"", env!("CARGO_PKG_VERSION"))), "Cargo version = app version");
        // The desktop's own code is the Android one: otherwise the update notice offers the version already running.
        let code = gradle.split("versionCode = ").nth(1).unwrap().split_whitespace().next().unwrap();
        let main = include_str!("../../../app/desktop/src/main/kotlin/com/jurisgames/sobres/desktop/Main.kt");
        assert!(main.contains(&format!("const val VERSION_CODE = {code}uL")), "desktop VERSION_CODE = versionCode {code}");
    }
}

#[cfg(test)]
mod write_timing {
    use super::*;
    use std::time::Instant;

    #[test]
    fn edits_are_written_after_a_pause_but_not_too_often() {
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        let long_ago = t0;
        assert!(!write_due(None, long_ago, s(100)), "nothing changed");
        assert!(!write_due(Some(s(100)), long_ago, s(101)), "still typing");
        assert!(write_due(Some(s(100)), long_ago, s(102)), "paused 2 s: write");
        assert!(!write_due(Some(s(103)), s(102), s(106)), "just wrote: wait for the gap");
        assert!(write_due(Some(s(103)), s(102), s(112)), "gap over: write");
    }
}

/// How often devices look for a newer operator manifest.
const MANIFEST_EVERY: Duration = Duration::from_secs(24 * 3600);

#[derive(uniffi::Object)]
pub struct KtNode {
    /// The device key is sealed by the system key store (false: the store failed and it is a plain file).
    key_in_store: bool,
    app: App,
    rt: tokio::runtime::Runtime,
    net: Net,
    data_dir: PathBuf,
    listener: Mutex<Option<Arc<dyn KtListener>>>,
    /// The link record last previewed (confirmed with the same words).
    previewed: Mutex<Option<(String, kt_engine::invite::InviteRecord)>>,
    /// Live mode: the stop signal and the task holding a reference to this node.
    live: Mutex<Option<(tokio::sync::watch::Sender<bool>, tokio::task::JoinHandle<()>)>>,
    backup_dir: Option<PathBuf>,
    restored: Option<RestoreInfo>,
    settings: Mutex<StoredSettings>,
    /// (on Wi-Fi, charging, app open), as the platform last reported (phones).
    device_state: Mutex<(bool, bool, bool)>,
    /// Battery saver, low battery, heat or data saver (phones): no helping (§7a).
    constrained: std::sync::atomic::AtomicBool,
    /// Same-Wi-Fi discovery (rotating beacons, §6) while the app is open.
    lan: Mutex<Option<Arc<kt_net::lan::Lan>>>,
    /// A full round has finished since the app opened (the splash waits for it).
    first_round: std::sync::atomic::AtomicBool,
    /// The last [`ROUND_LOG`] rounds (diagnostics).
    rounds: Mutex<std::collections::VecDeque<RoundLog>>,
    /// When the operator manifest was last fetched.
    manifest_checked: Mutex<Option<std::time::Instant>>,
    /// Real networking (not a loopback test node).
    lan_enabled: bool,
    /// MEASUREMENT ONLY (D23): idle extra endpoints pricing a per-group network identity
    /// ([`KtNode::measure_extra_endpoints`]). Empty unless the measurement switch set them.
    measure: Mutex<Option<kt_net::measure::ExtraEndpoints>>,
    /// Desktops: the relay run for others (D51), when the setting is on.
    #[cfg(feature = "desktop")]
    relay_for_others: tokio::sync::Mutex<Option<kt_net::relay_runner::RelayForOthers>>,
    /// Real networking (not a loopback test node): only then does a desktop relay for others.
    #[cfg(feature = "desktop")]
    public_net: bool,
}

const BACKUP_FILE: &str = "tasks.ktbackup";
/// A password-locked backup found on a fresh install waits here until the user enters the password.
const WAITING_FILE: &str = "locked.waiting.ktbackup";
const BACKUP_EVERY: Duration = Duration::from_secs(24 * 3600);
const RECOVERY_DAYS: u32 = 30;

fn gid(s: &str) -> Result<GroupId, KtError> {
    GroupId::from_hex(s).ok_or_else(|| fail("bad group id"))
}

/// Who a task's assignee is now: the list's person with any of those devices (current name),
/// else the name it was assigned under (someone who left).
fn assignee_of(devices: &[String], name: &str, people: &[(Vec<String>, PersonItem)]) -> PersonItem {
    if let Some((_, p)) = people.iter().find(|(ds, _)| ds.iter().any(|d| devices.contains(d))) {
        return p.clone();
    }
    let key = devices.iter().min().and_then(|d| kt_engine::DeviceId::from_hex(d)).map(|d| d.0).unwrap_or_default();
    let (hue, tag) = kt_tasks::appearance::avatar(&key);
    PersonItem { initials: kt_tasks::appearance::initials(name), name: name.to_string(), is_me: false, hue, tag }
}

fn person_item(p: &kt_tasks::app::Person) -> PersonItem {
    let key = p.devices.iter().min().map(|d| d.0).unwrap_or_default();
    let (hue, tag) = kt_tasks::appearance::avatar(&key);
    PersonItem { initials: kt_tasks::appearance::initials(&p.name), name: p.name.clone(), is_me: p.is_me, hue, tag }
}

fn ts_assigned(ts: &[kt_tasks::TaskView]) -> bool {
    ts.iter().any(|t| t.assignee.is_some() || ts_assigned(&t.children))
}

struct Who<'a> {
    me: &'a str,
    my_devices: &'a [String],
    people: &'a [(Vec<String>, PersonItem)],
}

/// Reminder caps (second-pass audit T2-3): Android refuses more than 500 alarms per app.
pub const REMINDERS_PER_LIST: usize = 100;
pub const REMINDERS_MAX: usize = 400;
const REMINDER_TITLE_MAX: usize = 200;

fn flatten(ts: &[kt_tasks::TaskView], who: &Who, parent: Option<&str>, depth: u32, out: &mut Vec<TaskItem>) {
    let me = who.me;
    for t in ts {
        out.push(TaskItem {
            images: t.images.iter().map(|r| hex::encode(r.id)).collect(),
            id: t.id.clone(),
            parent_id: parent.map(str::to_string),
            depth,
            title: t.title.clone(),
            done: t.done,
            notes: t.notes_preview().to_string(),
            has_attachment: t.attachment().is_some(),
            remind_ms: t.remind_ms,
            repeat: t.repeat.clone(),
            starred_ms: t.starred_ms,
            my_day: t.my_day.iter().find(|(who, _)| who == me).map(|(_, d)| d.clone()),
            assigned: t.assignee.as_ref().map(|(ds, name)| assignee_of(ds, name, who.people)),
            assigned_to_me: t.assignee.as_ref().is_some_and(|(ds, _)| ds.iter().any(|d| who.my_devices.contains(d))),
        });
        flatten(&t.children, who, Some(&t.id), depth + 1, out);
    }
}

impl KtNode {
    fn list_item(&self, l: ListInfo) -> ListItem {
        let st = self.app.read(&l, kt_tasks::appearance::list_style).unwrap_or_default();
        let list_type = self.app.read(&l, kt_tasks::list_type).map(|t| t.as_str()).unwrap_or("tasks").to_string();
        ListItem { id: l.doc.to_hex(), group_id: l.group.to_hex(), name: l.name, group_name: l.group_name, hue: st.hue, vivid: st.vivid, icon: st.icon, list_type }
    }

    fn list(&self, list_id: &str) -> Result<ListInfo, KtError> {
        let d = DocId::from_hex(list_id).ok_or_else(|| fail("bad list id"))?;
        self.app.lists().into_iter().find(|l| l.doc == d).ok_or_else(|| fail("no such list"))
    }

    /// (name, our display name) per group for invite records. The personal group is
    /// advertised by its summary: that's what a link code's confirmation screen shows.
    fn names(&self) -> std::collections::HashMap<GroupId, (String, String)> {
        let me = self.app.display_name();
        let personal = self.app.personal_group();
        let summary = self.app.account_summary();
        self.app.groups().into_iter().map(|g| (g.group, (if g.group == personal { summary.clone() } else { g.name }, me.clone()))).collect()
    }

    fn pending_lookups(&self) -> Vec<PendingLookup> {
        std::fs::read(self.data_dir.join("pending_lookups")).ok().and_then(|b| kt_engine::wire::decode::<Vec<PendingLookup>>(*b"PL", 1, &b).ok()).map(|x| x.1).unwrap_or_default()
    }

    fn save_pending_lookups(&self, v: &[PendingLookup]) {
        if let Ok(b) = kt_engine::wire::encode(*b"PL", 1, &v) {
            let _ = kt_engine::storage::write_atomic(&self.data_dir.join("pending_lookups"), &b);
        }
    }

    fn add_pending_lookup(&self, entered: &str) {
        let mut v = self.pending_lookups();
        if !v.iter().any(|p| p.entered == entered) {
            v.push(PendingLookup { entered: entered.to_string(), started_ms: kt_engine::replica::now_ms() });
            self.save_pending_lookups(&v);
        }
    }

    /// Look again for every code not found yet. Returns (groups joined or asked to join,
    /// codes given up on).
    async fn retry_lookups(&self) -> (Vec<String>, u32) {
        let pending = self.pending_lookups();
        if pending.is_empty() {
            return (Vec::new(), 0);
        }
        let layers = self.layers();
        let (mut keep, mut joined, mut gave_up) = (Vec::new(), Vec::new(), 0);
        for p in pending {
            let Ok(shared) = kt_net::invite_net::parse_shared(&p.entered) else { continue };
            let direct: Vec<_> = shared.direct.into_iter().collect();
            match kt_net::invite_net::fetch(&layers, &shared.code, CodeKind::Invite, &direct, Duration::from_millis(0)).await {
                Some((rec, _)) => match kt_net::invite_net::join(&self.net.syncer, self.net.p2p.as_ref(), &rec).await {
                    Ok(true) => joined.push(rec.group_name),
                    // Asked to join: the pending join reports it once a member lets us in.
                    Ok(false) => {}
                    Err(_) => keep.push(p),
                },
                None if kt_engine::replica::now_ms().saturating_sub(p.started_ms) > LOOKUP_FOR.as_millis() as u64 => gave_up += 1,
                None => keep.push(p),
            }
        }
        self.save_pending_lookups(&keep);
        (joined, gave_up)
    }

    /// Put open invites out on every layer (records for today and ahead).
    async fn publish_invites(&self) {
        let names = self.names();
        let names_fn = |g: &GroupId| names.get(g).cloned().unwrap_or_default();
        let layers = self.layers();
        if kt_net::invite_net::publish(&self.net.syncer, &layers, &names_fn).await.records > 0 {
            kt_net::invite_net::publish_ahead(&self.net.syncer, &layers, &names_fn).await;
        }
    }

    fn layers(&self) -> InviteLayers {
        InviteLayers {
            p2p: self.net.p2p.clone(),
            dht: self.net.dht.clone(),
            nostr: Some(self.net.nostr.clone()),
            helpers: self.net.helpers.as_ref().map(|h| h.0.clone()),
            lan: self.lan.lock().expect("lock").clone(),
        }
    }

    /// The daily sealed backup (the previous one is kept next to it).
    fn backup_if_due(&self) {
        let Some(dir) = &self.backup_dir else { return };
        let path = dir.join(BACKUP_FILE);
        let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        if age.is_some_and(|a| a < BACKUP_EVERY) {
            return;
        }
        let _ = self.write_backup_to(&path);
    }

    fn write_backup_to(&self, path: &std::path::Path) -> Result<(), KtError> {
        let bytes = self.app.backup(RECOVERY_DAYS).map_err(fail)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(fail)?;
        }
        if path.exists() {
            let _ = std::fs::copy(path, path.with_extension("prev.ktbackup"));
        }
        kt_engine::storage::write_atomic(path, &bytes).map_err(fail)
    }

    fn invite_info(&self, code: &kt_engine::invite::Code, kind: CodeKind, expires_ms: u64) -> InviteInfo {
        let here = self.net.p2p.as_ref().map(|p| p.where_data());
        InviteInfo {
            words: code.to_words().into_iter().map(str::to_string).collect(),
            expires_ms,
            link: kt_net::invite_net::share_url(code, kind, here.as_ref()),
        }
    }

    /// Helping follows the setting and the device's state.
    fn apply_helping(&self) {
        let (wifi, charging, open) = *self.device_state.lock().expect("lock");
        let constrained = self.constrained.load(std::sync::atomic::Ordering::Relaxed);
        let on = !constrained && self.settings.lock().expect("lock").net.helper_limits(wifi, charging, open).is_some();
        if let Some(p) = &self.net.p2p {
            p.set_helping(on);
        }
    }

    /// "Relay connections for others" (desktop, D51): run the relay while the setting is
    /// on. It's announced only if strangers can reach it (relay_runner); checking that
    /// takes a few seconds, so it happens in the background.
    fn apply_relay_for_others(self: &Arc<Self>) {
        #[cfg(feature = "desktop")]
        {
            let want = self.public_net && self.settings.lock().expect("lock").net.relay_config().is_some();
            let Some(p2p) = self.net.p2p.clone() else { return };
            let me = self.clone();
            self.rt.spawn(async move {
                let mut cur = me.relay_for_others.lock().await;
                match (want, cur.is_some()) {
                    (true, false) => {
                        let any: std::net::IpAddr = std::net::Ipv4Addr::UNSPECIFIED.into();
                        *cur = kt_net::relay_runner::RelayForOthers::start(&p2p, any, None).await.ok();
                    }
                    (false, true) => {
                        if let Some(r) = cur.take() {
                            r.stop(&p2p).await;
                        }
                    }
                    _ => {}
                }
            });
        }
    }

    fn save_settings(&self, s: &StoredSettings) -> Result<(), KtError> {
        kt_engine::storage::write_atomic(&self.data_dir.join("settings"), &kt_engine::wire::encode(*b"SE", 2, s).map_err(fail)?).map_err(fail)
    }

    fn notify(&self) {
        if let Some(l) = self.listener.lock().expect("lock").clone() {
            l.on_change();
        }
    }

    /// One full round on every layer: maintenance, sync, invites.
    async fn round(&self) -> SyncReport {
        let started = std::time::Instant::now();
        let rep = self.round_inner().await;
        let mut log = self.rounds.lock().expect("lock");
        if log.len() == ROUND_LOG {
            log.pop_front();
        }
        log.push_back(RoundLog {
            at_ms: kt_engine::replica::now_ms(),
            took_ms: started.elapsed().as_millis() as u64,
            written: rep.written,
            received: rep.received,
            rejected: rep.rejected,
            peers_reached: rep.peers_reached,
        });
        rep
    }

    async fn round_inner(&self) -> SyncReport {
        let net = &self.net;
        net.refresh_manifest(&self.manifest_checked).await;
        // First, so a new recovery invite goes out with this same round.
        self.backup_if_due();
        kt_net::relays::maintain(&net.syncer, &net.nostr, &net.relays).await;
        if let Some((h, dir)) = &net.helpers {
            kt_net::helper_dir::maintain_with_dht(&net.syncer, h, &net.nostr, dir, net.dht.as_deref()).await;
        }
        if let (Some(d), Some(p)) = (&net.dht, &net.p2p) {
            p.set_dht_entry(d.entry_point().await); // members bootstrap through each other
        }
        if let Some(d) = &net.dht {
            d.publish().await;
            d.lookup().await;
            d.save_cache().await;
        }
        let (w, r) = net.syncer.sync_once().await;
        let mut peers = 0;
        if let Some(p) = &net.p2p {
            p.publish_whereabouts();
            let pr = p.sync_round().await;
            peers = pr.peers_reached as u32;
        }
        self.publish_invites().await;
        let (mut joined, lookups_given_up) = self.retry_lookups().await;
        joined.extend(self.app.with(|r| r.settle_joins()).into_iter().map(|p| p.group_name));
        let _ = self.app.settle_account();
        let _ = self.app.publish_names();
        kt_net::health::exchange(&net.syncer, net.helpers.as_ref().map(|h| h.0.as_ref()), &net.nostr);
        // Text attachments: fetch what's missing, offer what's in use to helpers.
        let store = self.app.blob_store();
        let used = self.app.referenced_blobs();
        let (have, wanted): (Vec<_>, Vec<_>) = used.into_iter().partition(|id| store.has(id));
        let blobs = kt_net::blobs::round(net.p2p.as_deref(), net.helpers.as_ref().map(|h| h.0.as_ref()), net.blossom.as_ref(), &store, &wanted, &have).await;
        if blobs.fetched > 0 {
            self.notify();
        }
        self.app.collect_blob_garbage();
        SyncReport { written: w.written as u32, received: r.slots_opened as u32, rejected: r.rejected as u32, peers_reached: peers, joined_groups: joined, lookups_given_up }
    }
}

#[uniffi::export]
impl KtNode {
    /// Open this device. `identity_dir` must be excluded from backups; `worker_path` is the
    /// import sandbox executable (Android: `nativeLibraryDir/libkt_import.so`).
    #[uniffi::constructor]
    pub fn open(identity_dir: String, data_dir: String, worker_path: String, options: NodeOptions) -> Result<Arc<KtNode>, KtError> {
        // A fresh install with a backup waiting (Android restored it): restore it.
        let found = (!options.backup_dir.is_empty() && !PathBuf::from(&data_dir).join("app.meta").exists())
            .then(|| std::fs::read(PathBuf::from(&options.backup_dir).join(BACKUP_FILE)).ok())
            .flatten();
        // Locked with a password: kept aside (daily backups don't overwrite it) until the user enters it.
        let found = match found {
            Some(b) if kt_engine::replica::backup::needs_password(&b) => {
                let dir = PathBuf::from(&options.backup_dir);
                let _ = std::fs::rename(dir.join(BACKUP_FILE), dir.join(WAITING_FILE));
                None
            }
            other => other,
        };
        KtNode::start(identity_dir, data_dir, worker_path, options, found, None, false)
    }

    /// Restore a backup file the user picked (first launch, nothing made on this device yet).
    #[uniffi::constructor]
    /// `password`: for a password-locked backup (wrong or missing: the error says "wrong backup password").
    pub fn open_restoring(identity_dir: String, data_dir: String, worker_path: String, options: NodeOptions, backup: Vec<u8>, password: Option<String>) -> Result<Arc<KtNode>, KtError> {
        match kt_engine::replica::backup::open_with(&backup, password.as_deref()) {
            Err(_) if kt_engine::replica::backup::needs_password(&backup) => return Err(fail(kt_engine::replica::backup::WRONG_PASSWORD)),
            Err(_) => return Err(fail("This isn't a Sobres backup file.")),
            Ok(_) => {}
        }
        KtNode::start(identity_dir, data_dir, worker_path, options, Some(backup), password, true)
    }
}

impl KtNode {
    fn start(identity_dir: String, data_dir: String, worker_path: String, options: NodeOptions, restore: Option<Vec<u8>>, password: Option<String>, picked: bool) -> Result<Arc<KtNode>, KtError> {
        vault::use_builtin();
        // Sobres codes are never found by Tasks and the other way round (round 5, S13).
        kt_engine::invite::set_app_tag("sobres");
        let sandbox = SandboxConfig { worker: PathBuf::from(worker_path), timeout: Duration::from_secs(20) };
        let data = PathBuf::from(&data_dir);
        let identity = PathBuf::from(identity_dir);
        let (app, restored) = match restore.map(|b| App::restore_with(&identity, &data, sandbox.clone(), &b, password.as_deref())) {
            Some(Ok((app, s))) => (app, Some(RestoreInfo { rejoining_groups: s.rejoining.len() as u32, rehomed_lists: s.rehomed_lists as u32 })),
            // A file the user picked: say what went wrong.
            Some(Err(e)) if picked => return Err(fail(e)),
            Some(Err(_)) => {
                // An unreadable backup never keeps the app from starting, and isn't
                // overwritten by the next daily backup.
                if let Some(dir) = (!options.backup_dir.is_empty()).then(|| PathBuf::from(&options.backup_dir)) {
                    let _ = std::fs::rename(dir.join(BACKUP_FILE), dir.join("tasks.unrestored.ktbackup"));
                }
                if !data.join("app.meta").exists() {
                    let _ = std::fs::remove_dir_all(&data);
                    std::fs::create_dir_all(&data).map_err(fail)?;
                }
                (App::open(&identity, &data, sandbox).map_err(fail)?, None)
            }
            None => (App::open(&identity, &data, sandbox).map_err(fail)?, None),
        };
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(2).build().map_err(fail)?;
        let seeds: Vec<String> = if options.relays.is_empty() { kt_net::relays::SEED_RELAYS.iter().map(|s| s.to_string()).collect() } else { options.relays.clone() };
        let replica = app.replica();
        let platform = match options.platform {
            Platform::Desktop => NetPlatform::Desktop,
            Platform::Phone => NetPlatform::Phone,
        };
        let stored = std::fs::read(data.join("settings"))
            .ok()
            .and_then(|b| StoredSettings::read(&b))
            .filter(|s| s.net.platform() == platform)
            .unwrap_or_else(|| StoredSettings::defaults(platform));
        let settings = stored.net.clone();
        let net = rt.block_on(async {
            let refs: Vec<&str> = seeds.iter().map(|s| s.as_str()).collect();
            let nostr = Arc::new(NostrMailbox::new(&refs, NostrPolicy::default()));
            let syncer = Syncer::build(
                replica.clone(),
                vec![nostr.clone() as Arc<dyn Mailbox>],
                Some(data.join("sync.state")),
                Some(data.join("slots.store")),
            );
            let relays = RelayDirectory::new(&refs, Some(data.join("relays.dir")));
            if seeds.iter().all(|u| u.starts_with("wss://")) {
                kt_net::relays::install_resolver(&syncer, &nostr);
            }
            let mut p2p = None;
            let mut helpers = None;
            if options.p2p {
                let key = kt_engine::crypto::derive(&replica.lock().expect("lock").identity().secret().to_bytes(), "kt/helper-disk-key/v1", b"");
                // The store exists whenever helping is on at all; whether it serves right now
                // follows the device's state (set_device_state).
                let helper = settings.helper_limits_with_disk(true, true, true, &data.join("helping"), key);
                let cfg = P2pConfig {
                    bind: (!options.bind.is_empty()).then(|| options.bind.parse().ok()).flatten(),
                    portmap: options.bind.is_empty(),
                    helper,
                    // Best-effort public layers (D50), off for loopback tests.
                    n0_relays: options.bind.is_empty(),
                    // Not published to n0's public DNS (D23): addresses only travel inside
                    // group-sealed data, so ex-members can't watch when a device is online.
                    n0_dns: false,
                    ..Default::default()
                };
                if let Ok(p) = P2p::start(syncer.clone(), cfg).await {
                    p.set_blob_store(app.blob_store()); // attachments, served by ID
                    p.publish_whereabouts();
                    let hmb = Arc::new(HelperMailbox::new(p.clone(), vec![]));
                    syncer.add_mailbox(hmb.clone() as Arc<dyn Mailbox>);
                    kt_net::helper_dir::install_resolver(&syncer, &hmb);
                    helpers = Some((hmb, HelperDirectory::new(Some(data.join("helpers.dir")))));
                    p2p = Some(p);
                }
            }
            let dht = if options.dht { DhtDiscovery::with_cache(replica.clone(), data.join("dht_nodes.txt"), None).ok().map(Arc::new) } else { None };
            // The operator manifest (D56) and what it lists: developer-run cloud mailboxes and
            // Blossom servers. Real networking only, never from loopback tests.
            let real = options.bind.is_empty();
            let manifest = real.then(|| Arc::new(kt_net::manifest::Manifest::new(kt_net::manifest::OPERATOR_PUBLIC_KEY, kt_net::manifest::baseline(), Some(data.join("operator.manifest")))));
            let blossom = if real { kt_net::blossom::Blossom::new(Vec::new()) } else { None };
            let blossom_dir = real.then(|| kt_net::blossom::BlossomDirectory::new(Some(data.join("blossom.dir"))));
            let net = Net { syncer, nostr, relays, p2p, helpers, dht, blossom, manifest, clouds: Mutex::new(std::collections::HashMap::new()), blossom_dir };
            net.apply_manifest();
            net
        });
        if !options.device_name.is_empty() {
            let _ = app.set_device_name(&options.device_name);
        }
        let backup_dir = (!options.backup_dir.is_empty()).then(|| PathBuf::from(&options.backup_dir));
        if restored.is_some() {
            if let Some(dir) = &backup_dir {
                let _ = std::fs::remove_file(dir.join(WAITING_FILE));
            }
        }
        let node = Arc::new(KtNode {
            key_in_store: identity.join("device.sealed").exists(),
            app,
            rt,
            net,
            data_dir: data,
            listener: Mutex::new(None),
            previewed: Mutex::new(None),
            live: Mutex::new(None),
            backup_dir,
            restored,
            settings: Mutex::new(stored),
            // Phones start as "not on Wi-Fi" until the platform says otherwise.
            device_state: Mutex::new((platform == NetPlatform::Desktop, false, false)),
            constrained: std::sync::atomic::AtomicBool::new(false),
            lan: Mutex::new(None),
            first_round: std::sync::atomic::AtomicBool::new(false),
            rounds: Mutex::new(std::collections::VecDeque::new()),
            manifest_checked: Mutex::new(None),
            lan_enabled: options.bind.is_empty(),
            measure: Mutex::new(None),
            #[cfg(feature = "desktop")]
            relay_for_others: tokio::sync::Mutex::new(None),
            #[cfg(feature = "desktop")]
            public_net: options.bind.is_empty(),
        });
        node.apply_helping();
        node.apply_relay_for_others();
        Ok(node)
    }
}

#[uniffi::export]
impl KtNode {
    /// Set when this node was opened from a backup.
    pub fn restored(&self) -> Option<RestoreInfo> {
        self.restored.clone()
    }

    /// After a restore: how many groups the previous device is still in with us (0: nothing
    /// to offer). The app asks once: remove it (lost or stolen) or keep it (just reset).
    pub fn replaced_device_groups(&self) -> u32 {
        self.app.replaced_device().map(|(_, g)| g.len() as u32).unwrap_or(0)
    }

    /// Remove the previous device from those groups (their keys rotate).
    pub fn remove_replaced_device(&self) -> Result<u32, KtError> {
        let n = self.app.remove_replaced_device().map_err(fail)? as u32;
        self.notify();
        Ok(n)
    }

    /// The previous device was only reset: stop asking.
    pub fn keep_replaced_device(&self) {
        self.app.keep_replaced_device();
        self.notify();
    }

    /// Nothing made on this device yet: the first-launch screen may offer a restore.
    pub fn is_pristine(&self) -> bool {
        self.app.is_pristine()
    }

    /// The list as Markdown (Obsidian Tasks format).
    pub fn export_markdown(&self, list_id: String) -> Result<String, KtError> {
        let l = self.list(&list_id)?;
        self.app.export_markdown(&l).map_err(fail)
    }

    /// The list as a sealed `.tasklist` file (with history; re-importable).
    pub fn export_list_file(&self, list_id: String) -> Result<Vec<u8>, KtError> {
        let l = self.list(&list_id)?;
        self.app.export_list_file(&l).map_err(fail)
    }

    /// Import a `.tasklist` file as a new list (in `group_id`, or among your own lists).
    pub fn import_list_file(&self, group_id: Option<String>, file: Vec<u8>) -> Result<ListItem, KtError> {
        let g = group_id.as_deref().map(gid).transpose()?;
        let l = self.app.import_list_file(g, &file).map_err(fail)?;
        self.notify();
        Ok(self.list_item(l))
    }

    /// Write a sealed backup now (export, or before a risky operation).
    pub fn write_backup(&self, path: String) -> Result<(), KtError> {
        self.write_backup_to(&PathBuf::from(path))
    }

    /// Set (`Some`) or remove (`None`) the backup password (audit T11; ~0.5 s, call off the
    /// UI thread). The daily backup is rewritten at once and the previous copy (made under
    /// the old setting) removed.
    pub fn set_backup_password(&self, password: Option<String>) -> Result<(), KtError> {
        self.app.set_backup_password(password.as_deref()).map_err(fail)?;
        if let Some(dir) = &self.backup_dir {
            let path = dir.join(BACKUP_FILE);
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(path.with_extension("prev.ktbackup"));
            self.write_backup_to(&path)?;
        }
        Ok(())
    }

    /// Backups from this device need the password to open.
    pub fn has_backup_password(&self) -> bool {
        self.app.has_backup_password()
    }

    /// A password-locked backup came with this fresh install (Android restored it) and
    /// waits for its password: its bytes, for `open_restoring`. None once anything is made here.
    pub fn waiting_backup(&self) -> Option<Vec<u8>> {
        let dir = self.backup_dir.as_ref()?;
        self.app.is_pristine().then(|| std::fs::read(dir.join(WAITING_FILE)).ok()).flatten()
    }

    pub fn device_key(&self) -> String {
        let p = self.app.with(|r| r.device_public());
        format!("{}{}", hex::encode(p.sign), hex::encode(p.kex))
    }

    pub fn display_name(&self) -> String {
        self.app.display_name()
    }

    pub fn set_display_name(&self, name: String) -> Result<(), KtError> {
        self.app.set_display_name(&name).map_err(fail)
    }

    pub fn groups(&self) -> Vec<GroupItem> {
        self.app.groups().into_iter().map(|g| GroupItem { id: g.group.to_hex(), name: g.name, personal: g.personal }).collect()
    }

    pub fn create_group(&self, name: String) -> Result<GroupItem, KtError> {
        let g = self.app.create_group(&name).map_err(fail)?;
        Ok(GroupItem { id: g.group.to_hex(), name: g.name, personal: false })
    }

    pub fn lists(&self) -> Vec<ListItem> {
        self.app.lists().into_iter().map(|l| self.list_item(l)).collect()
    }

    /// Set a list's color and icon (shared with everyone in the list's group).
    pub fn set_list_style(&self, list_id: String, hue: Option<f64>, vivid: u8, icon: Option<String>) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        let style = kt_tasks::appearance::ListStyle { hue, vivid, icon };
        self.app.edit(&l, |d| kt_tasks::appearance::set_list_style(d, &style)).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// The colors of the theme this device uses (per device, never synced).
    pub fn theme_colors(&self, dark: bool) -> ThemeColors {
        let t = kt_tasks::appearance::find_theme(&self.settings.lock().expect("lock").theme);
        theme_colors_of(&t, dark)
    }

    /// Create a list in a group (`None` = a solo list).
    pub fn create_list(&self, group_id: Option<String>, name: String) -> Result<ListItem, KtError> {
        let g = group_id.map(|s| gid(&s)).transpose()?;
        let l = self.app.create_list(g, &name).map_err(fail)?;
        Ok(self.list_item(l))
    }

    pub fn tasks(&self, list_id: String) -> Result<Vec<TaskItem>, KtError> {
        let l = self.list(&list_id)?;
        let ts = self.app.read(&l, kt_tasks::tasks).map_err(fail)?;
        let mut out = Vec::new();
        let me = self.app.personal_group().to_hex();
        let assigned = ts_assigned(&ts);
        let (people, mine) = if assigned {
            let ps = self.app.people(&l);
            let mine: Vec<String> = ps.iter().filter(|p| p.is_me).flat_map(|p| p.devices.iter().map(|d| d.to_hex())).collect();
            (ps.iter().map(|p| (p.devices.iter().map(|d| d.to_hex()).collect(), person_item(p))).collect(), mine)
        } else {
            (vec![], vec![])
        };
        flatten(&ts, &Who { me: &me, my_devices: &mine, people: &people }, None, 0, &mut out);
        Ok(out)
    }

    pub fn add_task(&self, list_id: String, title: String, parent_id: Option<String>) -> Result<String, KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| kt_tasks::add_task(d, parent_id.as_deref(), &title)).map_err(fail)
    }

    /// Set a task's notes; long ones become a text attachment (D4).
    pub fn set_notes(&self, list_id: String, task_id: String, text: String) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.set_notes(&l, &task_id, &text).map_err(fail)
    }

    /// A task's full notes (an attachment still on its way shows its preview).
    pub fn notes(&self, list_id: String, task_id: String) -> Result<NotesItem, KtError> {
        let l = self.list(&list_id)?;
        let n = self.app.notes(&l, &task_id).map_err(fail)?;
        Ok(NotesItem { text: n.text, complete: n.complete, size: n.size })
    }

    pub fn set_done(&self, list_id: String, task_id: String, done: bool) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| kt_tasks::set_done(d, &task_id, done)).map_err(fail)
    }

    /// Set or clear a task's reminder (shared with everyone in the list, like any field).
    pub fn set_reminder(&self, list_id: String, task_id: String, at_ms: Option<u64>) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| kt_tasks::set_reminder(d, &task_id, at_ms)).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// Star or unstar a task (shared like any field).
    pub fn set_star(&self, list_id: String, task_id: String, on: bool) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| kt_tasks::set_star(d, &task_id, on)).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// Assign a task to someone in the list (Microsoft To Do's "Assign to": any member of a shared
    /// list, yourself too, one person), or to nobody.
    pub fn set_assignee(&self, list_id: String, task_id: String, person: Option<PersonItem>) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        let who = match person {
            Some(person) => {
                let p = self
                    .app
                    .people(&l)
                    .into_iter()
                    .find(|p| p.name == person.name && p.is_me == person.is_me && person_item(p).tag == person.tag)
                    .ok_or_else(|| fail("nobody by that name in this list's group"))?;
                Some((p.devices.iter().map(|d| d.to_hex()).collect::<Vec<_>>(), p.name))
            }
            None => None,
        };
        self.app.edit(&l, |d| kt_tasks::set_assignee(d, &task_id, who.as_ref().map(|(ds, n)| (ds.as_slice(), n.as_str())))).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// Put a task in My Day for `day` ("YYYY-MM-DD", the device's today), or take it out. It's this
    /// person's (all their linked devices'), not the other members' of a shared list.
    pub fn set_my_day(&self, list_id: String, task_id: String, day: Option<String>) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        let me = self.app.personal_group().to_hex();
        self.app.edit(&l, |d| kt_tasks::set_my_day(d, &task_id, &me, day.as_deref())).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// Set or clear how a task repeats (shared like any field).
    pub fn set_repeat(&self, list_id: String, task_id: String, rule: Option<String>) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| kt_tasks::set_repeat(d, &task_id, rule.as_deref())).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// A repeating task ticked off: due again at `next_ms`, steps unticked, still in the list.
    pub fn repeat_done(&self, list_id: String, task_id: String, next_ms: u64) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| kt_tasks::repeat_done(d, &task_id, next_ms)).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// Reminders of open tasks from an hour ago on, across all lists, soonest first: at most
    /// [`REMINDERS_PER_LIST`] from one list and [`REMINDERS_MAX`] in all (the soonest ones).
    pub fn upcoming_reminders(&self) -> Vec<ReminderItem> {
        fn walk(ts: &[kt_tasks::TaskView], l: &ListInfo, from: u64, out: &mut Vec<ReminderItem>) {
            for t in ts {
                if let (Some(at), false) = (t.remind_ms, t.done) {
                    if at >= from {
                        let title: String = t.title.chars().take(REMINDER_TITLE_MAX).collect();
                        out.push(ReminderItem { list_id: l.doc.to_hex(), list_name: l.name.clone(), task_id: t.id.clone(), title, at_ms: at });
                    }
                }
                walk(&t.children, l, from, out);
            }
        }
        let from = kt_engine::replica::now_ms().saturating_sub(3_600_000);
        let mut out = Vec::new();
        for l in self.app.lists() {
            if let Ok(ts) = self.app.read(&l, kt_tasks::tasks) {
                let mut mine = Vec::new();
                walk(&ts, &l, from, &mut mine);
                // T2-3: one list (anyone in it can write) can't use up the phone's alarms (Android: 500 per app).
                mine.sort_by_key(|r| r.at_ms);
                mine.truncate(REMINDERS_PER_LIST);
                out.extend(mine);
            }
        }
        out.sort_by_key(|r| r.at_ms);
        out.truncate(REMINDERS_MAX);
        out
    }

    pub fn rename_task(&self, list_id: String, task_id: String, title: String) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| kt_tasks::set_title(d, &task_id, &title)).map_err(fail)
    }

    /// Move a task under `parent_id` (or the top level) to position `index` among its new
    /// siblings. The order is shared with everyone in the list.
    pub fn move_task(&self, list_id: String, task_id: String, parent_id: Option<String>, index: u32) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| kt_tasks::move_task(d, &task_id, parent_id.as_deref(), index as usize)).map_err(fail)
    }

    /// Move a task right before (or after) `other_id`, under its parent (a drop in the list).
    pub fn move_task_next_to(&self, list_id: String, task_id: String, other_id: String, after: bool) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| kt_tasks::move_task_next_to(d, &task_id, &other_id, after)).map_err(fail)
    }

    /// "tasks" or "shopping" for everyone in the list.
    pub fn set_list_type(&self, list_id: String, list_type: String) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        let t = match list_type.as_str() {
            "tasks" => kt_tasks::ListType::Tasks,
            "shopping" => kt_tasks::ListType::Shopping,
            _ => return Err(fail("unknown list type")),
        };
        self.app.edit(&l, |d| kt_tasks::set_list_type(d, t)).map_err(fail)?;
        self.notify();
        Ok(())
    }

    pub fn rename_list(&self, list_id: String, name: String) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.rename_list(&l, name.trim()).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// Delete a list for everyone in it. Undo with `restore_list(group_id, list_id)`.
    pub fn delete_list(&self, list_id: String) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.set_list_listed(l.group, l.doc, false).map_err(fail)?;
        self.notify();
        Ok(())
    }

    pub fn restore_list(&self, group_id: String, list_id: String) -> Result<(), KtError> {
        let d = DocId::from_hex(&list_id).ok_or_else(|| fail("bad list id"))?;
        self.app.set_list_listed(gid(&group_id)?, d, true).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// Share a list: a list in the personal group moves into a new shared group named
    /// after it (people never see "group"); a list already in a shared group stays.
    /// Returns the list as it is now (its ID changes when it moves).
    pub fn share_list(&self, list_id: String) -> Result<ListItem, KtError> {
        let l = self.list(&list_id)?;
        if l.group != self.app.personal_group() {
            return Ok(self.list_item(l));
        }
        let g = self.app.create_group(&l.name).map_err(fail)?;
        let moved = self.app.move_list(&l, g.group).map_err(fail)?;
        self.notify();
        Ok(self.list_item(moved))
    }

    /// Attach an image (the app compresses it first, e.g. JPEG). Returns its ID.
    pub fn add_image(&self, list_id: String, task_id: String, bytes: Vec<u8>) -> Result<String, KtError> {
        let l = self.list(&list_id)?;
        let r = self.app.add_image(&l, &task_id, &bytes).map_err(fail)?;
        Ok(hex::encode(r.id))
    }

    /// An image's bytes, or nothing while it's still on its way to this device.
    pub fn task_image(&self, list_id: String, image_id: String) -> Result<Option<Vec<u8>>, KtError> {
        let l = self.list(&list_id)?;
        let id: [u8; 32] = hex::decode(&image_id).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| fail("bad image id"))?;
        self.app.image(&l, &id).map_err(fail)
    }

    pub fn remove_image(&self, list_id: String, task_id: String, image_id: String) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        let id: [u8; 32] = hex::decode(&image_id).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| fail("bad image id"))?;
        self.app.edit(&l, |d| kt_tasks::remove_image(d, &task_id, &id)).map_err(fail)
    }

    pub fn delete_task(&self, list_id: String, task_id: String) -> Result<(), KtError> {
        let l = self.list(&list_id)?;
        self.app.edit(&l, |d| kt_tasks::delete_task(d, &task_id)).map_err(fail)
    }

    /// How a shared group stands (cold UX review U-T4): whether this device's newest changes are still only
    /// here, and when another member last wrote.
    pub fn group_sync(&self, group_id: String) -> Result<GroupSync, KtError> {
        let g = gid(&group_id)?;
        let others_last_ms = self.app.with(|r| r.others_last_status_ms(&g));
        Ok(GroupSync { only_here: self.net.syncer.is_waiting(&g), others_last_ms })
    }

    /// Create an invite to a group (8 words, valid `days`, 1–30).
    pub fn invite(self: Arc<Self>, group_id: String, days: u32) -> Result<InviteInfo, KtError> {
        let g = gid(&group_id)?;
        if g == self.app.personal_group() {
            return Err(fail("your personal group is for your own devices (device linking)"));
        }
        let ms = u64::from(days.clamp(1, 30)) * 86_400_000;
        let (code, expires) = self.app.with(|r| r.create_invite(&g, CodeKind::Invite, ms)).map_err(fail)?;
        // Out on every layer now, not at the next round: the inviter may close the app
        // right after showing the code.
        let me = self.clone();
        self.rt.spawn(async move { me.publish_invites().await });
        Ok(self.invite_info(&code, CodeKind::Invite, expires))
    }

    /// Before the app quits (or goes to the background): push what this device hasn't sent
    /// yet (its changes and open invites) on every layer, for at most `timeout_ms`. Unlike
    /// [`KtNode::sync_now`] it reads nothing and does no upkeep, so it's quick.
    pub fn flush(&self, timeout_ms: u64) -> u32 {
        let net = &self.net;
        let work = async {
            let writes = net.syncer.write_round();
            let direct = async {
                if let Some(p) = &net.p2p {
                    p.sync_round().await;
                }
            };
            let (w, _, _) = tokio::join!(writes, direct, self.publish_invites());
            w.written as u32
        };
        self.rt.block_on(async { tokio::time::timeout(Duration::from_millis(timeout_ms), work).await.unwrap_or(0) })
    }

    /// Autocomplete one word of a code (3+ letters): the full word, if unique.
    pub fn complete_word(&self, prefix: String) -> Option<String> {
        kt_engine::invite::complete(&prefix).map(|(_, w)| w.to_string())
    }

    /// Join a group with an invite's 8 words (network: seconds).
    /// Join with an invite's words or its link (typed, pasted or scanned).
    pub fn join_code(&self, words: String) -> Result<JoinResult, KtError> {
        let shared = kt_net::invite_net::parse_shared(&words).map_err(fail)?;
        if shared.kind == Some(CodeKind::Link) {
            return Err(fail("This is a device link code, not an invite. Open ☰ › Link to my other device."));
        }
        let code = shared.code;
        let direct: Vec<_> = shared.direct.into_iter().collect();
        self.rt.block_on(async {
            let layers = self.layers();
            let found = kt_net::invite_net::fetch(&layers, &code, CodeKind::Invite, &direct, Duration::from_millis(0)).await;
            let Some((rec, _)) = found else {
                if kt_net::invite_net::fetch(&layers, &code, CodeKind::Link, &[], Duration::from_millis(0)).await.is_some() {
                    return Err(fail("This is a device link code, not an invite. Open ☰ › Link to my other device."));
                }
                // Not out there yet, or no connection right now: keep looking.
                self.add_pending_lookup(&words);
                return Ok(JoinResult { group_name: String::new(), inviter_name: String::new(), joined: false, still_looking: true });
            };
            let joined = kt_net::invite_net::join(&self.net.syncer, self.net.p2p.as_ref(), &rec).await.map_err(fail)?;
            self.net.syncer.sync_once().await;
            Ok(JoinResult { group_name: kt_tasks::clean_name(&rec.group_name), inviter_name: kt_tasks::clean_name(&rec.inviter_name), joined, still_looking: false })
        })
    }

    /// Others' changes since the last call (background sync posts them as notifications).
    /// Changes older than a few days are dropped; none with "Notify me" off.
    pub fn take_changes(&self) -> Vec<ChangeItem> {
        let changes = self.app.take_changes().unwrap_or_default();
        if !self.settings.lock().expect("lock").notify_changes {
            return Vec::new();
        }
        changes
            .into_iter()
            .map(|c| ChangeItem {
                group_name: c.group_name,
                list_id: c.list.as_ref().map(|l| l.doc.to_hex()).unwrap_or_default(),
                list_name: c.list.map(|l| l.name).unwrap_or_default(),
                who: c.who,
                kind: match c.kind {
                    kt_tasks::app::ChangeKind::Added => "added",
                    kt_tasks::app::ChangeKind::Completed => "completed",
                    kt_tasks::app::ChangeKind::Entered => "entered",
                    kt_tasks::app::ChangeKind::Joined => "joined",
                }
                .into(),
                title: c.title,
                at_ms: c.at_ms,
                cents: c.cents,
                currency: c.currency,
            })
            .collect()
    }

    /// The settings screen's current values.
    pub fn settings(&self) -> Settings {
        let s = self.settings.lock().expect("lock").clone();
        let desktop = s.net.platform() == NetPlatform::Desktop;
        Settings {
            sync_in_background: s.sync_in_background,
            notify_changes: s.notify_changes,
            help_others: help_name(s.net.help_others).into(),
            help_options: s.net.help_options().into_iter().map(|h| help_name(h).into()).collect(),
            relay_for_others: s.net.relay_setting_available().then(|| s.net.relay_for_others()),
            start_with_computer: desktop.then_some(s.start_with_computer),
            theme: s.theme.clone(),
            dynamic_color: (!desktop).then_some(s.dynamic_color),
        }
    }

    /// Save the settings screen. Settings that don't exist on this platform are refused.
    pub fn set_settings(self: Arc<Self>, new: Settings) -> Result<(), KtError> {
        let mut s = self.settings.lock().expect("lock").clone();
        s.sync_in_background = new.sync_in_background;
        s.notify_changes = new.notify_changes;
        let h = s.net.help_options().into_iter().find(|h| help_name(*h) == new.help_others).ok_or_else(|| fail("not a choice on this device"))?;
        s.net.set_help_others(h).map_err(fail)?;
        if let Some(r) = new.relay_for_others {
            s.net.set_relay_for_others(r).map_err(fail)?;
        }
        match (new.start_with_computer, s.net.platform()) {
            (Some(v), NetPlatform::Desktop) => s.start_with_computer = v,
            (Some(_), _) => return Err(fail("desktop only")),
            (None, _) => {}
        }
        if !kt_tasks::appearance::builtin_themes().iter().any(|t| t.id == new.theme) {
            return Err(fail("no such theme"));
        }
        s.theme = new.theme;
        match (new.dynamic_color, s.net.platform()) {
            (Some(v), NetPlatform::Phone) => s.dynamic_color = v,
            (Some(_), _) => return Err(fail("phones only")),
            (None, _) => {}
        }
        if h == kt_net::settings::HelpOthers::Off {
            kt_net::helper::wipe_disk(&self.data_dir.join("helping")); // disk doesn't clear itself
        }
        self.save_settings(&s)?;
        *self.settings.lock().expect("lock") = s;
        self.apply_helping();
        self.apply_relay_for_others();
        Ok(())
    }

    /// Phones report Wi-Fi, charging, and whether the phone is constrained (battery saver,
    /// low battery, heat, data saver); the node knows when the app is open. Helping others
    /// follows them (§7a).
    pub fn set_device_state(&self, on_wifi: bool, charging: bool, constrained: bool) {
        self.device_state.lock().expect("lock").0 = on_wifi;
        self.device_state.lock().expect("lock").1 = charging;
        self.constrained.store(constrained, std::sync::atomic::Ordering::Relaxed);
        self.apply_helping();
    }

    /// The platform saw the network change (Wi-Fi to mobile, a new network, back online):
    /// P2P re-checks its paths now instead of when its timers notice.
    pub fn network_changed(&self) {
        if let Some(p) = &self.net.p2p {
            let ep = p.endpoint().clone();
            self.rt.spawn(async move { ep.network_change().await });
        }
        if let Some(eps) = self.measure.lock().expect("lock").as_ref().map(|m| m.endpoints()) {
            // Measurement only: the extra endpoints re-check too, as N real ones would.
            self.rt.spawn(async move {
                for ep in eps {
                    ep.network_change().await;
                }
            });
        }
    }

    /// MEASUREMENT ONLY, not a feature (D23, `kt_net::measure`): keep `extra` idle iroh
    /// endpoints alive next to the real one, with its relay settings, so the battery and
    /// connection cost of one endpoint per group (N groups = N - 1 extra) can be measured.
    /// 0 stops them; the same count again changes nothing. Returns how many are running.
    /// Only the adb measurement switch calls this (Android `MeasureReceiver`), never the app.
    pub fn measure_extra_endpoints(&self, extra: u32) -> u32 {
        let extra = extra.min(64);
        let mut cur = self.measure.lock().expect("lock");
        if cur.as_ref().map_or(0, |m| m.len() as u32) == extra {
            return extra; // already so (the node's opening and the switch may both ask)
        }
        if let Some(old) = cur.take() {
            self.rt.block_on(old.close());
        }
        let Some(p) = &self.net.p2p else { return 0 };
        if extra > 0 {
            *cur = self.rt.block_on(kt_net::measure::ExtraEndpoints::start(p, extra)).ok();
        }
        cur.as_ref().map_or(0, |m| m.len() as u32)
    }

    /// Measurement only: how many extra endpoints are running.
    pub fn measure_endpoints_running(&self) -> u32 {
        self.measure.lock().expect("lock").as_ref().map_or(0, |m| m.len() as u32)
    }

    /// Measurement only: how many of the extra endpoints hold a home relay right now.
    pub fn measure_endpoints_online(&self) -> u32 {
        self.measure.lock().expect("lock").as_ref().map_or(0, |m| m.online() as u32)
    }

    /// Is there a newer version than `current_version_code`? `channel`: "play", "apk",
    /// "windows" or "linux" (which link the notice opens). Reads the static manifest
    /// (apps.jurisgames.com, then GitHub).
    pub fn check_update(&self, current_version_code: u64, channel: String) -> Option<UpdateItem> {
        use kt_net::update::{check_since, Channel, MANIFEST_URLS};
        let ch = match channel.as_str() {
            "play" => Channel::Play,
            "apk" => Channel::Apk,
            "windows" => Channel::Windows,
            "linux" => Channel::Linux,
            _ => return None,
        };
        // The newest signed file's serial seen so far: an older signed copy is refused (round 5, S11).
        let path = self.data_dir.join("update_serial");
        let seen: u64 = std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        let (found, serial) = self.rt.block_on(check_since(&MANIFEST_URLS, "sobres", current_version_code, ch, seen));
        if serial > seen {
            let _ = kt_engine::storage::write_atomic(&path, serial.to_string().as_bytes());
        }
        found.map(|u| UpdateItem { version_name: u.version_name, link: u.link })
    }

    /// The user is looking: nothing so far needs a notification.
    pub fn mark_seen(&self) {
        let _ = self.app.mark_seen();
    }

    /// Data from a newer app version reached this device: the update notice says a newer
    /// version is available to use certain features (principle 8).
    pub fn newer_version_seen(&self) -> bool {
        self.app.with(|r| r.newer_version_seen())
    }

    /// The people who edit a list (its group's members, by name).
    pub fn people(&self, list_id: String) -> Result<Vec<PersonItem>, KtError> {
        let l = self.list(&list_id)?;
        Ok(self
            .app
            .people(&l)
            .into_iter()
            .map(|p| person_item(&p))
            .collect())
    }

    /// Undo what that person did in the list since `since_ms` (other people's work stays).
    /// Returns how many things were put back.
    pub fn undo_changes_by(&self, list_id: String, person: PersonItem, since_ms: u64) -> Result<u32, KtError> {
        let l = self.list(&list_id)?;
        let p = self.app.people(&l).into_iter().find(|p| p.name == person.name && p.is_me == person.is_me).ok_or_else(|| fail("nobody by that name in this list's group"))?;
        let n = self.app.undo_changes_by(&l, &p, since_ms).map_err(fail)? as u32;
        self.notify();
        Ok(n)
    }

    /// A link code for adding another device to this account (valid a few hours).
    pub fn link_code(&self) -> Result<InviteInfo, KtError> {
        let (code, expires_ms) = self.app.create_link_code().map_err(fail)?;
        Ok(self.invite_info(&code, CodeKind::Link, expires_ms))
    }

    /// Find the account a link code joins (the confirmation screen shows it).
    pub fn link_preview(&self, words: String) -> Result<LinkPreview, KtError> {
        let shared = kt_net::invite_net::parse_shared(&words).map_err(fail)?;
        if shared.kind == Some(CodeKind::Invite) {
            return Err(fail("This is an invite to a budget, not a device link. Open ☰ › Join a shared budget."));
        }
        let code = shared.code;
        let direct: Vec<_> = shared.direct.into_iter().collect();
        let rec = self.rt.block_on(async {
            let layers = self.layers();
            match kt_net::invite_net::fetch(&layers, &code, CodeKind::Link, &direct, Duration::from_millis(0)).await {
                Some((rec, _)) => Ok(rec),
                None if kt_net::invite_net::fetch(&layers, &code, CodeKind::Invite, &[], Duration::from_millis(0)).await.is_some() => {
                    Err(fail("This is an invite to a budget, not a device link. Open ☰ › Join a shared budget."))
                }
                None => Err(fail("no link code found for these words: check them, or it may have expired")),
            }
        })?;
        // The other side wrote these: cleaned like every name (no direction or invisible tricks).
        let preview = LinkPreview { account_name: kt_tasks::clean_name(&rec.inviter_name), summary: kt_tasks::clean_name(&rec.group_name) };
        *self.previewed.lock().expect("lock") = Some((code.to_text(), rec));
        Ok(preview)
    }

    /// The user confirmed the previewed account: link this device to it. Its lists merge
    /// into the account once a device of the account lets it in.
    pub fn link_confirm(&self, words: String) -> Result<(), KtError> {
        let code = kt_net::invite_net::parse_shared(&words).map_err(fail)?.code.to_text();
        let rec = match self.previewed.lock().expect("lock").take() {
            Some((c, rec)) if c == code => rec,
            _ => return Err(fail("preview the code first")),
        };
        self.app.begin_link(&rec).map_err(fail)?;
        self.rt.block_on(async {
            kt_net::invite_net::join_direct(self.net.p2p.as_ref(), &rec).await;
            self.net.syncer.sync_once().await;
        });
        self.notify();
        Ok(())
    }

    /// Linking into another account and not in yet.
    pub fn is_linking(&self) -> bool {
        self.app.linking().is_some()
    }

    /// This account's devices.
    pub fn devices(&self) -> Vec<DeviceItem> {
        self.app.devices().into_iter().map(|d| DeviceItem { id: d.device.to_hex(), name: d.name, this_device: d.this_device }).collect()
    }

    /// Devices let into our shared lists recently (second-pass audit T2-13): shown with Remove and OK.
    pub fn recent_joins(&self) -> Vec<RecentJoinItem> {
        let lists = self.app.lists();
        self.app
            .recent_joins()
            .into_iter()
            .map(|j| RecentJoinItem {
                group_id: j.group.to_hex(),
                device_id: j.device.to_hex(),
                name: j.name,
                by: j.by,
                list_name: lists.iter().find(|l| l.group == j.group).map(|l| l.name.clone()).unwrap_or_default(),
                joined_ms: j.joined_ms,
            })
            .collect()
    }

    pub fn dismiss_join(&self, group_id: String, device_id: String) -> Result<(), KtError> {
        let d = kt_engine::DeviceId::from_hex(&device_id).ok_or_else(|| fail("bad device id"))?;
        self.app.dismiss_join(&gid(&group_id)?, &d).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// Remove a device that joined (it leaves that list; keys rotate).
    pub fn remove_joined(&self, group_id: String, device_id: String) -> Result<(), KtError> {
        let d = kt_engine::DeviceId::from_hex(&device_id).ok_or_else(|| fail("bad device id"))?;
        self.app.remove_joined(&gid(&group_id)?, &d).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// Devices linked in the last day ("New device linked · Undo" on every device).
    pub fn recent_links(&self) -> Vec<RecentLinkItem> {
        self.app.recent_links().into_iter().map(|l| RecentLinkItem { device_id: l.device.to_hex(), name: l.name, linked_ms: l.linked_ms }).collect()
    }

    /// Undo a link: the device leaves the account and every group (keys rotate).
    pub fn unlink_device(&self, device_id: String) -> Result<(), KtError> {
        let d = kt_engine::DeviceId::from_hex(&device_id).ok_or_else(|| fail("bad device id"))?;
        self.app.unlink_device(d).map_err(fail)?;
        self.notify();
        Ok(())
    }

    /// One sync round on every layer (background jobs call this).
    pub fn sync_now(&self) -> SyncReport {
        let rep = self.rt.block_on(self.round());
        self.notify();
        rep
    }

    pub fn set_listener(&self, listener: Arc<dyn KtListener>) {
        *self.listener.lock().expect("lock") = Some(listener);
    }

    /// Whether a full sync round has finished since the app opened ([`KtNode::start_live`]).
    /// The splash screen is held until then, or about 1.5 s (DESIGN §8).
    pub fn first_round_done(&self) -> bool {
        self.first_round.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// App open: live updates on every layer until [`KtNode::stop_live`].
    pub fn start_live(self: Arc<Self>) {
        self.stop_live(); // first: it marks the app as closed
        self.first_round.store(false, std::sync::atomic::Ordering::Relaxed);
        self.device_state.lock().expect("lock").2 = true;
        self.apply_helping();
        let (tx, rx) = tokio::sync::watch::channel(false);
        // Same Wi-Fi: beacon while open; a member heard on the LAN gets a direct round.
        if let (true, Some(p)) = (self.lan_enabled, self.net.p2p.clone()) {
            let replica = self.app.replica();
            if let Ok(lan) = self.rt.block_on(kt_net::lan::Lan::start(replica, kt_net::lan::LanConfig::default())) {
                self.rt.spawn(lan.clone().run(rx.clone()));
                let (l, mut stop) = (lan.clone(), rx.clone());
                self.rt.spawn(async move {
                    loop {
                        tokio::select! {
                            _ = l.learned.notified() => {
                                p.sync_round().await;
                            }
                            _ = stop.changed() => return,
                        }
                    }
                });
                *self.lan.lock().expect("lock") = Some(lan);
            }
        }
        let net_syncer = self.net.syncer.clone();
        let nostr = self.net.nostr.clone();
        let p2p = self.net.p2p.clone();
        self.rt.spawn(kt_net::live::run_live(net_syncer.clone(), nostr, p2p.clone(), rx.clone()));
        if let Some(p) = p2p {
            self.rt.spawn(kt_net::live::run_p2p_live(net_syncer, p, rx.clone()));
        }
        // Periodic full rounds + change notifications.
        let me = self.clone();
        let mut stop = rx;
        let task = self.rt.spawn(async move {
            let mut last_round = std::time::Instant::now() - Duration::from_secs(3600);
            let mut seen = me.app.with(|r| r.local_changes());
            let (mut changed_at, mut last_write) = (None, std::time::Instant::now() - WRITE_GAP);
            loop {
                if last_round.elapsed() > Duration::from_secs(45) {
                    me.round().await;
                    me.first_round.store(true, std::sync::atomic::Ordering::Relaxed);
                    last_round = std::time::Instant::now();
                    last_write = last_round;
                    changed_at = None;
                }
                // Writes don't wait for the round: once edits pause, the mailboxes get them
                // (a write that never lands is how a change gets lost; reads can wait).
                let now_changes = me.app.with(|r| r.local_changes());
                if now_changes != seen {
                    seen = now_changes;
                    changed_at = Some(std::time::Instant::now());
                }
                if write_due(changed_at, last_write, std::time::Instant::now()) {
                    me.net.syncer.write_round().await;
                    last_write = std::time::Instant::now();
                    changed_at = None;
                }
                let changed = me.app.with(|r| r.take_events()).iter().any(|e| {
                    matches!(e, Event::Imported { .. } | Event::DeviceJoined { .. } | Event::DocEpochSwitched { .. } | Event::EpochAccepted { .. })
                });
                if changed {
                    let _ = me.app.mark_seen(); // the app is open: shown, not notified
                    me.notify();
                }
                tokio::select! {
                    _ = stop.changed() => return,
                    _ = tokio::time::sleep(Duration::from_millis(400)) => {}
                }
            }
        });
        *self.live.lock().expect("lock") = Some((tx, task));
    }

    /// Back to background mode. Returns once live mode has let go of this node (so the
    /// platform can close it, e.g. to restore a backup).
    pub fn stop_live(&self) {
        self.device_state.lock().expect("lock").2 = false;
        self.apply_helping();
        self.lan.lock().expect("lock").take(); // beacons stop with the live signal below
        if let Some((tx, task)) = self.live.lock().expect("lock").take() {
            let _ = tx.send(true);
            task.abort();
            let _ = self.rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), task).await });
        }
    }

    /// Relay health (diagnostics).
    pub fn relay_stats(&self) -> Vec<RelayStat> {
        let mut v: Vec<RelayStat> = self
            .net
            .nostr
            .health()
            .into_iter()
            .map(|(url, h)| RelayStat {
                url,
                published_ok: h.published_ok,
                connect_failures: h.connect_failures,
                failures_in_a_row: h.failures_in_a_row,
                readback_hits: h.readback_hits,
                readback_misses: h.readback_misses,
            })
            .collect();
        v.sort_by(|a, b| a.url.cmp(&b.url));
        v
    }

    /// Why this build must update, if the signed operator manifest says so (never from peer
    /// data). `channel` as for [`KtNode::check_update`].
    pub fn required_update(&self, version: u64, channel: String) -> Option<String> {
        self.net.manifest.as_ref()?.required_update(&channel, version)
    }

    /// Everything the hidden diagnostics screen shows (and "Export log" writes out).
    pub fn diagnostics(&self) -> Diagnostics {
        let relays = self.relay_stats();
        let working = relays.iter().filter(|r| r.failures_in_a_row < 3).count();
        let mut layers = vec![LayerStatus { name: "Nostr relays".into(), state: format!("{working} of {} working", relays.len()) }];
        // Said plainly when the system key store failed (safety audit round 4, T15d): then another program running
        // as this user could copy the device key.
        layers.push(LayerStatus {
            name: "Device key".into(),
            state: if self.key_in_store { "in the system key store".into() } else { "a plain file (the system key store failed on this device)".into() },
        });
        let p2p = self.net.p2p.as_ref();
        layers.push(LayerStatus {
            name: "Direct connections".into(),
            state: match p2p {
                None => "off".into(),
                Some(p) if p.is_online() => "online (home relay connected)".into(),
                Some(_) => "no home relay".into(),
            },
        });
        let helper_relays = p2p.map(|p| p.helper_relays()).unwrap_or_default();
        let mut helpers: Vec<HelperStat> = self
            .net
            .helpers
            .as_ref()
            .map(|(h, _)| {
                h.records()
                    .into_iter()
                    .map(|(id, r)| HelperStat {
                        id: hex::encode(&id.as_bytes()[..5]),
                        reachable: r.reachable,
                        unreachable: r.unreachable,
                        latency_ms: r.latency_ms,
                        failing: h.is_failing(&id),
                        excluded: r.excluded,
                    })
                    .collect()
            })
            .unwrap_or_default();
        helpers.sort_by(|a, b| a.id.cmp(&b.id));
        layers.push(LayerStatus {
            name: "Storage helpers".into(),
            state: match &self.net.helpers {
                None => "off".into(),
                Some(_) => format!("{} of {} working", helpers.iter().filter(|h| !h.failing).count(), helpers.len()),
            },
        });
        layers.push(LayerStatus {
            name: "DHT".into(),
            state: match &self.net.dht {
                None => "off".into(),
                Some(d) => format!("on, {} member entry points", d.member_entry_points().len()),
            },
        });
        layers.push(LayerStatus {
            name: "Same Wi-Fi".into(),
            state: if !self.lan_enabled { "off".into() } else if self.lan.lock().expect("lock").is_some() { "listening".into() } else { "only while open".into() },
        });
        // The address itself stays out: logs get shared.
        #[cfg(feature = "desktop")]
        let relaying = self.relay_for_others.try_lock().ok().map(|r| r.as_ref().map(|r| r.announced_url().is_some()));
        #[cfg(not(feature = "desktop"))]
        let relaying: Option<Option<bool>> = Some(None);
        layers.push(LayerStatus {
            name: "Relaying for others".into(),
            state: match relaying {
                None => "changing".into(),
                Some(None) => "off".into(),
                Some(Some(false)) => "on, not reachable from outside (not announced)".into(),
                Some(Some(true)) => "on, reachable and announced".into(),
            },
        });
        // Never a name (round 5, S7): these lines go out with feedback, and a shared group's name is what people
        // called their budget. Counters only.
        let groups = self.app.groups();
        let labels = storage_labels(&groups.iter().map(|g| g.personal).collect::<Vec<_>>());
        let storage = groups
            .iter()
            .zip(labels)
            .map(|(g, name)| GroupStorage { name, bytes: dir_bytes(&self.data_dir.join("groups").join(g.group.to_hex())) })
            .collect();
        Diagnostics {
            version: env!("CARGO_PKG_VERSION").into(),
            feature_level: kt_engine::replica::FEATURE_LEVEL,
            layers,
            relays,
            helper_relays,
            helpers,
            storage,
            rounds: self.rounds.lock().expect("lock").iter().rev().cloned().collect(),
        }
    }

    pub fn data_dir(&self) -> String {
        self.data_dir.display().to_string()
    }
}
