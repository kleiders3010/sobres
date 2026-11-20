//! Appearance (DESIGN §2): list color and icon, generated avatars, and themes as data.
//!
//! A list stores a **hue** in the HCT color space (plus a vividness level), never a literal
//! color; each theme decides the tone, so list colors never clash with a theme. Grey has no
//! hue. Themes are versioned sets of design tokens; the built-in ones use the same format
//! custom ones will.

use loro::{LoroDoc, LoroValue};
use material_colors::color::Argb;
use material_colors::dynamic_color::Variant;
use material_colors::hct::Hct;
use material_colors::scheme::Scheme;
use material_colors::theme::ThemeBuilder;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The curated hues in the picker; "More colors…" allows any hue.
pub const LIST_HUES: [f64; 14] = [0.0, 25.0, 50.0, 75.0, 100.0, 130.0, 160.0, 190.0, 215.0, 240.0, 265.0, 290.0, 315.0, 340.0];

/// List icons, by ID (stored in the list). IDs are only ever added: an ID a newer version
/// added shows the default icon here.
pub const LIST_ICONS: [(&str, &str); 16] = [
    ("check", "✔"),
    ("cart", "🛒"),
    ("home", "🏠"),
    ("work", "💼"),
    ("star", "⭐"),
    ("heart", "❤"),
    ("book", "📚"),
    ("plane", "✈"),
    ("gift", "🎁"),
    ("food", "🍽"),
    ("tools", "🔧"),
    ("money", "💰"),
    ("health", "💊"),
    ("music", "🎵"),
    ("sport", "⚽"),
    ("idea", "💡"),
];

/// Chroma per vividness level (soft, normal, vivid); HCT clamps to what the screen can show.
const CHROMA: [f64; 3] = [16.0, 36.0, 60.0];

/// A list's look, shared with every member (it's in the list's document).
#[derive(Clone, Debug, PartialEq)]
pub struct ListStyle {
    /// HCT hue 0–360; `None` is grey.
    pub hue: Option<f64>,
    /// 0 soft, 1 normal, 2 vivid.
    pub vivid: u8,
    /// An ID from [`LIST_ICONS`].
    pub icon: Option<String>,
}

impl Default for ListStyle {
    fn default() -> Self {
        ListStyle { hue: None, vivid: 1, icon: None }
    }
}

/// Read a list's style. Anything malformed (another device's data) reads as the default.
pub fn list_style(doc: &LoroDoc) -> ListStyle {
    let meta = doc.get_map("meta");
    let num = |k: &str| match meta.get(k).and_then(|v| v.into_value().ok()) {
        Some(LoroValue::Double(d)) => Some(d),
        Some(LoroValue::I64(i)) => Some(i as f64),
        _ => None,
    };
    let hue = num("hue").filter(|h| h.is_finite() && (0.0..360.0).contains(h));
    let vivid = num("vivid").filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 2.0) as u8).unwrap_or(1);
    let icon = match meta.get("icon").and_then(|v| v.into_value().ok()) {
        Some(LoroValue::String(s)) if LIST_ICONS.iter().any(|(id, _)| *id == s.as_str()) => Some(s.to_string()),
        _ => None,
    };
    ListStyle { hue, vivid, icon }
}

pub fn set_list_style(doc: &LoroDoc, style: &ListStyle) -> Result<()> {
    let meta = doc.get_map("meta");
    let loro = |e: loro::LoroError| Error::Invalid(e.to_string());
    match style.hue {
        Some(h) if h.is_finite() => meta.insert("hue", h.rem_euclid(360.0)).map_err(loro)?,
        _ => meta.delete("hue").map_err(loro)?,
    }
    meta.insert("vivid", style.vivid.min(2) as i64).map_err(loro)?;
    match &style.icon {
        Some(i) if LIST_ICONS.iter().any(|(id, _)| id == i) => meta.insert("icon", i.as_str()).map_err(loro)?,
        Some(i) => return Err(Error::Invalid(format!("unknown icon {i}"))),
        None => meta.delete("icon").map_err(loro)?,
    }
    Ok(())
}

/// The icon's symbol for an ID.
pub fn icon_symbol(id: &str) -> Option<&'static str> {
    LIST_ICONS.iter().find(|(i, _)| *i == id).map(|(_, s)| *s)
}

/// A generated avatar's hue and short tag from a public key: the tag tells two people with
/// the same name apart ("Alex · 4F2A").
pub fn avatar(key: &[u8; 32]) -> (f64, String) {
    let h = kt_engine::crypto::hash("kt/avatar/v1", key);
    let hue = u16::from_le_bytes([h[0], h[1]]) as f64 % 360.0;
    (hue, hex::encode_upper(&h[2..4]))
}

/// Up to two initials for an avatar.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let first = |w: &str| w.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    match words.as_slice() {
        [] => "?".into(),
        [one] => first(one),
        [a, .., b] => first(a) + &first(b),
    }
}

/// Theme format version (principle 8: a newer format reads as "newer", not as damage).
pub const THEME_FORMAT: u16 = 1;

/// A theme: design tokens, no code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeTokens {
    pub format: u16,
    pub id: String,
    pub name: String,
    /// Seed hue for the palette (HCT); `None` is a grey theme.
    pub seed_hue: Option<f64>,
    pub seed_chroma: f64,
    /// "tonal_spot", "vibrant", "expressive", "neutral" or "monochrome".
    pub variant: String,
    /// Tone of list colors and avatars in light and dark mode.
    pub accent_tone_light: f64,
    pub accent_tone_dark: f64,
    /// Typography scale factor (on top of the system font size, which always wins).
    pub text_scale: f32,
    /// -1 compact, 0 normal, 1 roomy.
    pub density: i8,
    pub corner_dp: u8,
}

fn theme(id: &str, name: &str, hue: Option<f64>, chroma: f64, variant: &str) -> ThemeTokens {
    ThemeTokens {
        format: THEME_FORMAT,
        id: id.into(),
        name: name.into(),
        seed_hue: hue,
        seed_chroma: chroma,
        variant: variant.into(),
        accent_tone_light: 45.0,
        accent_tone_dark: 75.0,
        text_scale: 1.0,
        density: 0,
        corner_dp: 12,
    }
}

pub fn builtin_themes() -> Vec<ThemeTokens> {
    vec![
        theme("default", "Default", Some(265.0), 36.0, "tonal_spot"),
        theme("forest", "Forest", Some(140.0), 36.0, "tonal_spot"),
        theme("ocean", "Ocean", Some(220.0), 48.0, "vibrant"),
        theme("sunset", "Sunset", Some(40.0), 48.0, "expressive"),
        ThemeTokens { density: -1, corner_dp: 4, ..theme("paper", "Paper", None, 0.0, "monochrome") },
    ]
}

pub fn find_theme(id: &str) -> ThemeTokens {
    builtin_themes().into_iter().find(|t| t.id == id).unwrap_or_else(|| builtin_themes().remove(0))
}

fn argb(c: Argb) -> u32 {
    (c.alpha as u32) << 24 | (c.red as u32) << 16 | (c.green as u32) << 8 | c.blue as u32
}

/// A list's (or avatar's) accent color as ARGB: the stored hue at the theme's tone.
pub fn accent(theme: &ThemeTokens, hue: Option<f64>, vivid: u8, dark: bool) -> u32 {
    let tone = if dark { theme.accent_tone_dark } else { theme.accent_tone_light };
    let (h, c) = match hue {
        Some(h) => (h, CHROMA[vivid.min(2) as usize]),
        None => (0.0, 0.0),
    };
    argb(Argb::from(Hct::from(h, c, tone)))
}

/// The color roles a UI needs, as ARGB.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorRoles {
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
}

/// A theme's palette for light or dark mode.
pub fn colors(theme: &ThemeTokens, dark: bool) -> ColorRoles {
    let seed = Argb::from(Hct::from(theme.seed_hue.unwrap_or(0.0), if theme.seed_hue.is_some() { theme.seed_chroma } else { 0.0 }, 50.0));
    let variant = match (theme.seed_hue, theme.variant.as_str()) {
        (None, _) | (_, "monochrome") => Variant::Monochrome,
        (_, "vibrant") => Variant::Vibrant,
        (_, "expressive") => Variant::Expressive,
        (_, "neutral") => Variant::Neutral,
        _ => Variant::TonalSpot,
    };
    let t = ThemeBuilder::with_source(seed).variant(variant).build();
    let s: &Scheme = if dark { &t.schemes.dark } else { &t.schemes.light };
    ColorRoles {
        primary: argb(s.primary),
        on_primary: argb(s.on_primary),
        primary_container: argb(s.primary_container),
        on_primary_container: argb(s.on_primary_container),
        secondary: argb(s.secondary),
        on_secondary: argb(s.on_secondary),
        secondary_container: argb(s.secondary_container),
        on_secondary_container: argb(s.on_secondary_container),
        tertiary: argb(s.tertiary),
        on_tertiary: argb(s.on_tertiary),
        error: argb(s.error),
        on_error: argb(s.on_error),
        background: argb(s.background),
        on_background: argb(s.on_background),
        surface: argb(s.surface),
        on_surface: argb(s.on_surface),
        surface_variant: argb(s.surface_variant),
        on_surface_variant: argb(s.on_surface_variant),
        surface_container: argb(s.surface_container),
        surface_container_high: argb(s.surface_container_high),
        outline: argb(s.outline),
        outline_variant: argb(s.outline_variant),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(c: u32) -> f64 {
        Hct::new(Argb::from_u32(c)).get_tone()
    }

    #[test]
    fn list_style_round_trip_and_bad_data() {
        let d = LoroDoc::new();
        assert_eq!(list_style(&d), ListStyle::default());
        let s = ListStyle { hue: Some(130.0), vivid: 2, icon: Some("cart".into()) };
        set_list_style(&d, &s).unwrap();
        assert_eq!(list_style(&d), s);
        set_list_style(&d, &ListStyle::default()).unwrap();
        assert_eq!(list_style(&d), ListStyle::default(), "back to grey, no icon");
        assert!(set_list_style(&d, &ListStyle { icon: Some("../x".into()), ..Default::default() }).is_err());
        // What another (or newer, or broken) device might write:
        let m = d.get_map("meta");
        m.insert("hue", f64::NAN).unwrap();
        m.insert("vivid", 99i64).unwrap();
        m.insert("icon", "rocket-from-v9").unwrap();
        assert_eq!(list_style(&d), ListStyle { hue: None, vivid: 2, icon: None });
    }

    #[test]
    fn each_theme_decides_the_tone() {
        for t in builtin_themes() {
            for h in LIST_HUES {
                let light = accent(&t, Some(h), 1, false);
                let dark = accent(&t, Some(h), 1, true);
                assert!((tone(light) - t.accent_tone_light).abs() < 2.0, "{} hue {h}", t.id);
                assert!((tone(dark) - t.accent_tone_dark).abs() < 2.0, "{} hue {h}", t.id);
            }
            let c = colors(&t, false);
            let d = colors(&t, true);
            assert!(tone(c.background) > 80.0 && tone(d.background) < 20.0, "{}", t.id);
            assert!((tone(c.on_primary) - tone(c.primary)).abs() > 40.0, "{} text readable", t.id);
        }
        let grey = accent(&find_theme("default"), None, 2, false);
        let hct = Hct::new(Argb::from_u32(grey));
        assert!(hct.get_chroma() < 2.0, "grey has no hue");
    }

    #[test]
    fn avatars() {
        let (h1, t1) = avatar(&[1; 32]);
        let (h2, t2) = avatar(&[2; 32]);
        assert!((0.0..360.0).contains(&h1) && t1.len() == 4 && t1 != t2 && h1 != h2);
        assert_eq!(avatar(&[1; 32]), (h1, t1), "stable");
        assert_eq!(initials("alex"), "A");
        assert_eq!(initials("Mary Ann Smith"), "MS");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials("émile zola"), "ÉZ");
    }
}
