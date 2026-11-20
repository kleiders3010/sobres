//! Network settings and their platform defaults (DESIGN §2 settings table).
//!
//! Two separate helping settings:
//! - **Help others sync** (§7a): store other groups' slots in RAM. All platforms; phones
//!   only on Wi-Fi (and charging, by default).
//! - **Relay connections for others** (D51): carry encrypted live traffic for devices
//!   that can't connect directly. **Desktop only, on by default.** Only reachable desktops
//!   (public IPv4 or a UPnP router) actually serve; the rest silently don't.

use crate::helper::HelperLimits;
#[cfg(feature = "relay-server")]
use crate::helper_relay::HelperRelayConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Platform {
    Desktop,
    Phone,
}

/// Network settings the UI shows, per platform. A setting not listed here doesn't exist
/// on that platform: it's not shown at all (not greyed out).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingId {
    HelpOthers,
    RelayForOthers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HelpOthers {
    /// Phones: on Wi-Fi while charging, plus on Wi-Fi while the app is open (default).
    WifiAndCharging,
    /// Phones: on Wi-Fi only while the app is open.
    WifiWhileOpen,
    /// Desktops: whenever the app runs (default).
    On,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NetSettings {
    platform: Platform,
    pub help_others: HelpOthers,
    relay_for_others: bool,
}

impl NetSettings {
    pub fn defaults(platform: Platform) -> NetSettings {
        match platform {
            Platform::Desktop => NetSettings { platform, help_others: HelpOthers::On, relay_for_others: true },
            Platform::Phone => NetSettings { platform, help_others: HelpOthers::WifiAndCharging, relay_for_others: false },
        }
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The network settings to show on this platform, in order. Phones never see
    /// "Relay connections for others" (D51: completely hidden, not disabled).
    pub fn visible(&self) -> Vec<SettingId> {
        match self.platform {
            Platform::Desktop => vec![SettingId::HelpOthers, SettingId::RelayForOthers],
            Platform::Phone => vec![SettingId::HelpOthers],
        }
    }

    /// The "Help others sync" choices on this platform (DESIGN settings table).
    pub fn help_options(&self) -> Vec<HelpOthers> {
        match self.platform {
            Platform::Desktop => vec![HelpOthers::On, HelpOthers::Off],
            Platform::Phone => vec![HelpOthers::WifiAndCharging, HelpOthers::WifiWhileOpen, HelpOthers::Off],
        }
    }

    /// Change "Help others sync" to one of [`NetSettings::help_options`].
    pub fn set_help_others(&mut self, h: HelpOthers) -> Result<(), &'static str> {
        if !self.help_options().contains(&h) {
            return Err("not a choice on this platform");
        }
        self.help_others = h;
        Ok(())
    }

    /// Whether the "Relay connections for others" setting exists on this platform.
    pub fn relay_setting_available(&self) -> bool {
        self.platform == Platform::Desktop
    }

    pub fn relay_for_others(&self) -> bool {
        self.relay_for_others
    }

    /// Change the relay setting. Refused on phones (the setting doesn't exist there).
    pub fn set_relay_for_others(&mut self, on: bool) -> Result<(), &'static str> {
        if !self.relay_setting_available() {
            return Err("relaying for others is desktop only");
        }
        self.relay_for_others = on;
        Ok(())
    }

    /// Storage-helping limits to run with right now, if helping. `on_wifi` / `charging` /
    /// `app_open` describe the phone's state (ignored on desktops).
    pub fn helper_limits(&self, on_wifi: bool, charging: bool, app_open: bool) -> Option<HelperLimits> {
        let active = match (self.platform, self.help_others) {
            (_, HelpOthers::Off) => false,
            (Platform::Desktop, _) => true,
            (Platform::Phone, HelpOthers::WifiAndCharging) => on_wifi && (charging || app_open),
            (Platform::Phone, HelpOthers::WifiWhileOpen) => on_wifi && app_open,
            (Platform::Phone, HelpOthers::On) => on_wifi,
        };
        active.then(|| match self.platform {
            Platform::Desktop => HelperLimits::default(),
            Platform::Phone => HelperLimits { capacity: 24 * 1024 * 1024, ..HelperLimits::default() },
        })
    }

    /// [`NetSettings::helper_limits`] with the disk copy in `dir` (encrypted with `key`).
    /// With "Help others sync" off, the disk copy is deleted right away.
    pub fn helper_limits_with_disk(
        &self,
        on_wifi: bool,
        charging: bool,
        app_open: bool,
        dir: &std::path::Path,
        key: kt_engine::crypto::SymKey,
    ) -> Option<HelperLimits> {
        if self.help_others == HelpOthers::Off {
            crate::helper::wipe_disk(dir);
            return None;
        }
        self.helper_limits(on_wifi, charging, app_open).map(|mut l| {
            l.disk = Some(crate::helper::DiskConfig { dir: dir.to_path_buf(), key });
            l
        })
    }

    /// The live relay to run for strangers, if any (open access: D51). The caller still
    /// checks reachability (public IPv4 or UPnP) and provides the certificate.
    #[cfg(feature = "relay-server")]
    pub fn relay_config(&self) -> Option<HelperRelayConfig> {
        (self.relay_setting_available() && self.relay_for_others).then(HelperRelayConfig::default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_for_others_is_on_for_desktop_and_absent_on_phones() {
        let mut d = NetSettings::defaults(Platform::Desktop);
        assert!(d.relay_setting_available() && d.relay_for_others());
        #[cfg(feature = "relay-server")]
        assert!(d.relay_config().is_some());
        d.set_relay_for_others(false).unwrap();
        #[cfg(feature = "relay-server")]
        assert!(d.relay_config().is_none());
        assert!(d.helper_limits(false, false, false).is_some(), "storage helping is a separate setting");

        assert_eq!(d.visible(), vec![SettingId::HelpOthers, SettingId::RelayForOthers]);
        let mut p = NetSettings::defaults(Platform::Phone);
        assert_eq!(p.visible(), vec![SettingId::HelpOthers], "not shown at all on phones");
        assert!(!p.relay_setting_available() && !p.relay_for_others());
        assert!(p.set_relay_for_others(true).is_err());
        assert!(p.helper_limits(true, true, false).is_some());
        assert!(p.helper_limits(false, true, true).is_none(), "phones help only on Wi-Fi");

        // Turning helping off deletes the disk copy.
        let dir = std::env::temp_dir().join(format!("kt-settings-{}", rand::random::<u32>()));
        std::fs::create_dir_all(dir.join("ab")).unwrap();
        let key = kt_engine::crypto::SymKey([1; 32]);
        assert!(d.helper_limits_with_disk(true, true, true, &dir, key.clone()).unwrap().disk.is_some());
        d.help_others = HelpOthers::Off;
        assert!(d.helper_limits_with_disk(true, true, true, &dir, key).is_none());
        assert!(!dir.exists());
    }

    #[test]
    fn choices_per_platform() {
        let mut phone = NetSettings::defaults(Platform::Phone);
        assert_eq!(phone.help_options(), [HelpOthers::WifiAndCharging, HelpOthers::WifiWhileOpen, HelpOthers::Off]);
        assert!(phone.set_help_others(HelpOthers::On).is_err(), "not a phone choice");
        assert!(phone.set_help_others(HelpOthers::WifiWhileOpen).is_ok());
        assert!(phone.helper_limits(true, true, false).is_none(), "Wi-Fi while open: closed means no");
        assert!(phone.helper_limits(true, false, true).is_some());
        assert!(phone.set_relay_for_others(true).is_err(), "no relay setting on phones");
        let mut desk = NetSettings::defaults(Platform::Desktop);
        assert_eq!(desk.help_options(), [HelpOthers::On, HelpOthers::Off]);
        assert!(desk.set_help_others(HelpOthers::WifiAndCharging).is_err());
        assert!(desk.set_help_others(HelpOthers::Off).is_ok());
        assert!(desk.helper_limits(true, true, true).is_none());
    }
}
