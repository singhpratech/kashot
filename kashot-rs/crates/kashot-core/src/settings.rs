//! Persistent app settings. Stored as JSON at the platform's config dir,
//! e.g. `%APPDATA%/Kashot/settings.json` on Windows, `~/.config/Kashot/settings.json`
//! on Linux, `~/Library/Application Support/Kashot/settings.json` on macOS.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::theme::ThemeName;

bitflags::bitflags! {
    /// Hotkey modifier mask. Numeric values match Win32 `MOD_*` so the same
    /// settings.json works on the C# Windows build and the Rust build.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Modifiers: u32 {
        const ALT     = 0x0001;
        const CONTROL = 0x0002;
        const SHIFT   = 0x0004;
        const SUPER   = 0x0008;
    }
}

impl Serialize for Modifiers {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_u32(self.bits())
    }
}

impl<'de> Deserialize<'de> for Modifiers {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let bits = u32::deserialize(de)?;
        Ok(Modifiers::from_bits_truncate(bits))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub modifiers:   Modifiers,
    /// Win32 virtual-key code. `0x2C` (`VK_SNAPSHOT` / Print Screen) is the default.
    pub virtual_key: u32,
}

impl Default for Hotkey {
    fn default() -> Self {
        Hotkey {
            modifiers: Modifiers::empty(),
            virtual_key: 0x2C,
        }
    }
}

impl Hotkey {
    /// Human-readable rendering, e.g. `"Ctrl + Shift + P"` or `"PrintScreen"`.
    /// Unknown VKs render as `"(0xNN)"` so the user can at least see they have
    /// an unsupported key bound.
    pub fn describe(&self) -> String {
        let key = vk_label(self.virtual_key)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("(0x{:02X})", self.virtual_key));
        let mods = self.modifiers.describe();
        if mods.is_empty() { key } else { format!("{} + {}", mods, key) }
    }
}

impl Modifiers {
    /// Render the active modifier set as `"Ctrl + Shift + Alt + Win"` (in the
    /// order Windows users expect). Returns the empty string when no modifiers
    /// are set.
    pub fn describe(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.contains(Modifiers::CONTROL) { parts.push("Ctrl"); }
        if self.contains(Modifiers::SHIFT)   { parts.push("Shift"); }
        if self.contains(Modifiers::ALT)     { parts.push("Alt"); }
        if self.contains(Modifiers::SUPER)   { parts.push("Win"); }
        parts.join(" + ")
    }
}

/// Map a Win32 virtual-key code to a short, user-facing label.
///
/// Canonical source for VK → label rendering shared by the Settings dialog,
/// the tray tooltip, and any future UI surface. Returns `None` for codes the
/// rebind widget doesn't know how to display so callers can render an
/// "(0xNN)" fallback. Keep this list in sync with `vk_to_code` in
/// `kashot-platform::hotkey` — anything `vk_to_code` accepts should have a
/// label here.
pub fn vk_label(vk: u32) -> Option<&'static str> {
    Some(match vk {
        0x08 => "Backspace",
        0x09 => "Tab",
        0x0D => "Enter",
        0x14 => "CapsLock",
        0x1B => "Esc",
        0x20 => "Space",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0x2C => "PrintScreen",
        0x2D => "Insert",
        0x2E => "Delete",
        0x91 => "ScrollLock",
        0x13 => "Pause",
        0x30 => "0", 0x31 => "1", 0x32 => "2", 0x33 => "3", 0x34 => "4",
        0x35 => "5", 0x36 => "6", 0x37 => "7", 0x38 => "8", 0x39 => "9",
        0x41 => "A", 0x42 => "B", 0x43 => "C", 0x44 => "D", 0x45 => "E",
        0x46 => "F", 0x47 => "G", 0x48 => "H", 0x49 => "I", 0x4A => "J",
        0x4B => "K", 0x4C => "L", 0x4D => "M", 0x4E => "N", 0x4F => "O",
        0x50 => "P", 0x51 => "Q", 0x52 => "R", 0x53 => "S", 0x54 => "T",
        0x55 => "U", 0x56 => "V", 0x57 => "W", 0x58 => "X", 0x59 => "Y",
        0x5A => "Z",
        0x70 => "F1",  0x71 => "F2",  0x72 => "F3",  0x73 => "F4",
        0x74 => "F5",  0x75 => "F6",  0x76 => "F7",  0x77 => "F8",
        0x78 => "F9",  0x79 => "F10", 0x7A => "F11", 0x7B => "F12",
        _ => return None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(rename = "LastTool", default = "default_tool")]
    pub last_tool: String,

    #[serde(rename = "LastColorArgb", default = "default_color_argb")]
    pub last_color_argb: i32,

    #[serde(rename = "LastThickness", default = "default_thickness")]
    pub last_thickness: f32,

    #[serde(rename = "SaveDirectory", default)]
    pub save_directory: String,

    #[serde(rename = "RecordingsDirectory", default)]
    pub recordings_directory: String,

    #[serde(rename = "HotkeyModifiers", default)]
    pub hotkey_modifiers: u32,

    #[serde(rename = "HotkeyVirtualKey", default = "default_vk")]
    pub hotkey_virtual_key: u32,

    #[serde(rename = "StartWithWindows", default)]
    pub start_with_windows: bool,

    #[serde(rename = "WatermarkEnabled", default = "default_true")]
    pub watermark_enabled: bool,

    #[serde(rename = "WatermarkText", default = "default_watermark")]
    pub watermark_text: String,

    #[serde(rename = "WatermarkOpacity", default = "default_watermark_opacity")]
    pub watermark_opacity: f32,

    #[serde(rename = "WatermarkPosition", default = "default_watermark_position")]
    pub watermark_position: String,

    #[serde(rename = "PaletteIndex", default)]
    pub palette_index: i32,

    #[serde(rename = "Theme", default = "default_theme")]
    pub theme: String,

    /// Alpha (0..=255) applied to the Marker (highlighter) stroke. Default
    /// `0xC8` (200, ≈78 %) preserves the historical look; the editor's
    /// per-tool slider mutates this and persists it on mouseup.
    #[serde(rename = "MarkerOpacity", default = "default_marker_opacity")]
    pub marker_opacity: u8,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            last_tool:           default_tool(),
            last_color_argb:     default_color_argb(),
            last_thickness:      default_thickness(),
            save_directory:      String::new(),
            recordings_directory: String::new(),
            hotkey_modifiers:    0,
            hotkey_virtual_key:  default_vk(),
            start_with_windows:  false,
            watermark_enabled:   true,
            watermark_text:      default_watermark(),
            watermark_opacity:   default_watermark_opacity(),
            watermark_position:  default_watermark_position(),
            palette_index:       0,
            theme:               default_theme(),
            marker_opacity:      default_marker_opacity(),
        }
    }
}

fn default_tool()        -> String  { "Pen".to_owned() }
fn default_color_argb()  -> i32     { 0xFFFF_0000_u32 as i32 }
fn default_thickness()   -> f32     { 3.0 }
fn default_vk()          -> u32     { 0x2C }
fn default_true()        -> bool    { true }
fn default_watermark()   -> String  { "KAShot".to_owned() }
fn default_theme()       -> String  { "Light".to_owned() }
fn default_watermark_opacity() -> f32 { 0.85 }
fn default_watermark_position() -> String { "BottomRight".to_owned() }
fn default_marker_opacity() -> u8 { 0xC8 }

/// Anchor for the watermark inside the saved frame. JSON values are case-
/// insensitive `TopLeft` / `TopRight` / `BottomLeft` / `BottomRight`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatermarkAnchor {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl WatermarkAnchor {
    pub fn parse(s: &str) -> WatermarkAnchor {
        match s.trim().to_ascii_lowercase().as_str() {
            "topleft"     | "top_left"     | "top-left"     => WatermarkAnchor::TopLeft,
            "topright"    | "top_right"    | "top-right"    => WatermarkAnchor::TopRight,
            "bottomleft"  | "bottom_left"  | "bottom-left"  => WatermarkAnchor::BottomLeft,
            _                                                => WatermarkAnchor::BottomRight,
        }
    }
}

impl AppSettings {
    /// `~/.../Kashot/`, created if it doesn't exist.
    pub fn config_dir() -> Option<PathBuf> {
        let dirs = directories::ProjectDirs::from("org", "kashot", "Kashot")?;
        Some(dirs.config_dir().to_path_buf())
    }

    pub fn settings_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("settings.json"))
    }

    /// Load settings; missing or malformed file silently returns `Default`.
    pub fn load() -> AppSettings {
        let Some(path) = Self::settings_path() else { return AppSettings::default(); };
        match fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => AppSettings::default(),
        }
    }

    /// Best-effort save; never propagates I/O errors. Mirrors the C# behavior
    /// where `AppSettings.Save()` swallows exceptions ("the app should never
    /// crash because of settings persistence").
    pub fn save(&self) -> io::Result<()> {
        let dir  = Self::config_dir().ok_or_else(|| io::Error::other("no config dir"))?;
        fs::create_dir_all(&dir)?;
        let path = dir.join("settings.json");
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        fs::write(path, json)
    }

    pub fn theme(&self) -> ThemeName {
        ThemeName::parse(&self.theme)
    }

    pub fn hotkey(&self) -> Hotkey {
        Hotkey {
            modifiers:   Modifiers::from_bits_truncate(self.hotkey_modifiers),
            virtual_key: self.hotkey_virtual_key,
        }
    }

    pub fn set_hotkey(&mut self, hk: Hotkey) {
        self.hotkey_modifiers   = hk.modifiers.bits();
        self.hotkey_virtual_key = hk.virtual_key;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_through_json() {
        let s = AppSettings::default();
        let txt = serde_json::to_string_pretty(&s).unwrap();
        let s2: AppSettings = serde_json::from_str(&txt).unwrap();
        assert_eq!(s.last_tool, s2.last_tool);
        assert_eq!(s.last_color_argb, s2.last_color_argb);
        assert_eq!(s.hotkey_virtual_key, s2.hotkey_virtual_key);
    }

    #[test]
    fn missing_keys_are_filled_with_defaults() {
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.last_tool, "Pen");
        assert_eq!(s.theme, "Light");
        assert_eq!(s.hotkey_virtual_key, 0x2C);
    }

    #[test]
    fn modifiers_serialize_as_u32() {
        let m = Modifiers::CONTROL | Modifiers::SHIFT;
        let bits: u32 = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(bits, 0x0002 | 0x0004);
    }

    #[test]
    fn hotkey_ctrl_shift_p_round_trips() {
        let hk = Hotkey {
            modifiers:   Modifiers::CONTROL | Modifiers::SHIFT,
            virtual_key: 0x50,
        };
        let mut s = AppSettings::default();
        s.set_hotkey(hk);
        let txt = serde_json::to_string_pretty(&s).unwrap();
        let s2: AppSettings = serde_json::from_str(&txt).unwrap();
        let hk2 = s2.hotkey();
        assert_eq!(hk, hk2);
        assert_eq!(hk2.modifiers.bits(), 0x0002 | 0x0004);
        assert_eq!(hk2.virtual_key, 0x50);
        assert_eq!(hk2.describe(), "Ctrl + Shift + P");
    }

    #[test]
    fn default_hotkey_is_printscreen() {
        let s = AppSettings::default();
        let hk = s.hotkey();
        assert_eq!(hk.modifiers, Modifiers::empty());
        assert_eq!(hk.virtual_key, 0x2C);
        assert_eq!(hk.describe(), "PrintScreen");
        assert_eq!(Hotkey::default(), hk);
    }

    #[test]
    fn vk_label_covers_common_keys() {
        assert_eq!(vk_label(0x2C), Some("PrintScreen"));
        assert_eq!(vk_label(0x50), Some("P"));
        assert_eq!(vk_label(0x70), Some("F1"));
        assert_eq!(vk_label(0x7B), Some("F12"));
        assert_eq!(vk_label(0x25), Some("Left"));
        assert_eq!(vk_label(0xABCD), None);
    }

    #[test]
    fn marker_opacity_default_preserves_legacy_alpha() {
        assert_eq!(AppSettings::default().marker_opacity, 0xC8);
    }

    #[test]
    fn marker_opacity_round_trips_through_json() {
        let mut s = AppSettings::default();
        s.marker_opacity = 0x40;
        let txt = serde_json::to_string(&s).unwrap();
        assert!(txt.contains("\"MarkerOpacity\""), "JSON key should be MarkerOpacity: {txt}");
        let s2: AppSettings = serde_json::from_str(&txt).unwrap();
        assert_eq!(s2.marker_opacity, 0x40);
    }

    #[test]
    fn marker_opacity_missing_key_falls_back_to_default() {
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.marker_opacity, 0xC8);
    }
}
