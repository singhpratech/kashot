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
}
