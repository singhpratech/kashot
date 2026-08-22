//! Global-hotkey actions and the settings plumbing behind them.
//!
//! Kashot registers up to three independent global hotkeys: the region
//! capture that has always been there, plus an optional full-screen capture
//! and an optional record start/stop toggle. This module owns everything
//! about that set that is pure logic — the action enum, the mapping from
//! [`AppSettings`] fields to bindings, and conflict detection — so the
//! platform shim and the Settings dialog share one source of truth.
//!
//! Wire format: each action stores a Win32 modifier mask + virtual-key code
//! in `settings.json`, exactly like the original `HotkeyModifiers` /
//! `HotkeyVirtualKey` pair. A virtual key of [`UNSET_VK`] (`0`, which is not
//! a real Win32 VK) means "no binding", which is also what a settings file
//! written before these keys existed decodes to — so old files keep working
//! and only gain the two new actions once the user binds them.

use crate::settings::{AppSettings, Hotkey, Modifiers};

/// Virtual-key value that stands for "this action has no binding". `0` is
/// not assigned in the Win32 VK space, so it can never collide with a key a
/// user actually pressed.
pub const UNSET_VK: u32 = 0;

/// Something a global hotkey can trigger. Ordering is the order the Settings
/// dialog lists them and the order conflicts are reported in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    /// Capture, then open the overlay editor for a region selection.
    Capture,
    /// Grab the whole desktop and save it straight to disk.
    CaptureFullScreen,
    /// Start a screen recording, or stop the one that's running.
    ToggleRecording,
}

impl HotkeyAction {
    /// Every action, in dialog order.
    pub const ALL: [HotkeyAction; 3] = [
        HotkeyAction::Capture,
        HotkeyAction::CaptureFullScreen,
        HotkeyAction::ToggleRecording,
    ];

    /// Short user-facing name. Used as the Settings row label and inside
    /// conflict messages, so it has to read well mid-sentence.
    pub fn label(self) -> &'static str {
        match self {
            HotkeyAction::Capture           => "Capture region",
            HotkeyAction::CaptureFullScreen => "Capture full screen",
            HotkeyAction::ToggleRecording   => "Record screen",
        }
    }

    /// Whether the user is allowed to leave this action unbound. Region
    /// capture ships with a per-platform default and stays bound; the two
    /// newer actions are opt-in and start out unset.
    pub fn is_optional(self) -> bool {
        !matches!(self, HotkeyAction::Capture)
    }
}

impl AppSettings {
    /// The binding for `action`, or `None` when it has none.
    pub fn hotkey_for(&self, action: HotkeyAction) -> Option<Hotkey> {
        let (mods, vk) = match action {
            HotkeyAction::Capture =>
                (self.hotkey_modifiers, self.hotkey_virtual_key),
            HotkeyAction::CaptureFullScreen =>
                (self.fullscreen_hotkey_modifiers, self.fullscreen_hotkey_virtual_key),
            HotkeyAction::ToggleRecording =>
                (self.record_hotkey_modifiers, self.record_hotkey_virtual_key),
        };
        if vk == UNSET_VK { return None; }
        Some(Hotkey { modifiers: Modifiers::from_bits_truncate(mods), virtual_key: vk })
    }

    /// Bind or clear `action`. Clearing writes [`UNSET_VK`] and a zero
    /// modifier mask so the persisted file never carries a stale modifier
    /// set next to an unset key.
    pub fn set_hotkey_for(&mut self, action: HotkeyAction, hk: Option<Hotkey>) {
        let (mods, vk) = match hk {
            Some(h) => (h.modifiers.bits(), h.virtual_key),
            None    => (0, UNSET_VK),
        };
        match action {
            HotkeyAction::Capture => {
                self.hotkey_modifiers   = mods;
                self.hotkey_virtual_key = vk;
            }
            HotkeyAction::CaptureFullScreen => {
                self.fullscreen_hotkey_modifiers   = mods;
                self.fullscreen_hotkey_virtual_key = vk;
            }
            HotkeyAction::ToggleRecording => {
                self.record_hotkey_modifiers   = mods;
                self.record_hotkey_virtual_key = vk;
            }
        }
    }

    /// Every action that currently has a binding, in dialog order. This is
    /// exactly what gets handed to the platform hotkey manager.
    pub fn hotkey_bindings(&self) -> Vec<(HotkeyAction, Hotkey)> {
        HotkeyAction::ALL
            .iter()
            .filter_map(|&a| self.hotkey_for(a).map(|hk| (a, hk)))
            .collect()
    }

    /// First pair of actions bound to the same chord, if any.
    ///
    /// Two actions sharing a chord is not something the OS can resolve — on
    /// Windows the second `RegisterHotKey` just fails, and under X11 both
    /// grabs land on one key so whichever we happen to match first wins. The
    /// Settings dialog refuses to save in that state rather than shipping
    /// the ambiguity to the user.
    pub fn hotkey_conflict(&self) -> Option<(HotkeyAction, HotkeyAction)> {
        let bound = self.hotkey_bindings();
        for (i, (a, ha)) in bound.iter().enumerate() {
            for (b, hb) in &bound[i + 1..] {
                if ha == hb { return Some((*a, *b)); }
            }
        }
        None
    }
}

/// One-line explanation of a conflict, in the wording the Settings dialog
/// shows inline under the form.
///
/// Kept short on purpose: it renders as a single unwrapped line of 5x7
/// bitmap text in the dialog footer, which fits about 99 characters. The
/// longest this can get -- both optional actions, every modifier, and a
/// spelled-out key name -- is 96.
pub fn conflict_message(a: HotkeyAction, b: HotkeyAction, hk: Hotkey) -> String {
    format!("{} and {} share {} - rebind one.", a.label(), b.label(), hk.describe())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Modifiers;

    fn hk(mods: Modifiers, vk: u32) -> Hotkey {
        Hotkey { modifiers: mods, virtual_key: vk }
    }

    #[test]
    fn new_actions_start_unbound() {
        let s = AppSettings::default();
        assert!(s.hotkey_for(HotkeyAction::Capture).is_some());
        assert_eq!(s.hotkey_for(HotkeyAction::CaptureFullScreen), None);
        assert_eq!(s.hotkey_for(HotkeyAction::ToggleRecording), None);
        assert_eq!(s.hotkey_bindings().len(), 1);
    }

    #[test]
    fn primary_default_is_unchanged_by_the_new_fields() {
        // The shipped default binding must be byte-identical to what
        // v0.6.0 wrote, on every platform.
        let s = AppSettings::default();
        assert_eq!(s.hotkey_for(HotkeyAction::Capture), Some(s.hotkey()));
        assert_eq!(s.hotkey(), Hotkey::default());
    }

    #[test]
    fn all_three_round_trip_through_json() {
        let mut s = AppSettings::default();
        s.set_hotkey_for(HotkeyAction::Capture, Some(hk(Modifiers::CONTROL, 0x2C)));
        s.set_hotkey_for(HotkeyAction::CaptureFullScreen,
                         Some(hk(Modifiers::CONTROL | Modifiers::SHIFT, 0x46)));
        s.set_hotkey_for(HotkeyAction::ToggleRecording,
                         Some(hk(Modifiers::ALT | Modifiers::SUPER, 0x52)));

        let txt = serde_json::to_string_pretty(&s).unwrap();
        for key in ["FullScreenHotkeyModifiers", "FullScreenHotkeyVirtualKey",
                    "RecordHotkeyModifiers", "RecordHotkeyVirtualKey"] {
            assert!(txt.contains(&format!("\"{key}\"")), "missing {key} in {txt}");
        }

        let s2: AppSettings = serde_json::from_str(&txt).unwrap();
        assert_eq!(s2.hotkey_for(HotkeyAction::Capture), Some(hk(Modifiers::CONTROL, 0x2C)));
        assert_eq!(s2.hotkey_for(HotkeyAction::CaptureFullScreen),
                   Some(hk(Modifiers::CONTROL | Modifiers::SHIFT, 0x46)));
        assert_eq!(s2.hotkey_for(HotkeyAction::ToggleRecording),
                   Some(hk(Modifiers::ALT | Modifiers::SUPER, 0x52)));
        assert_eq!(s2.hotkey_bindings().len(), 3);
    }

    #[test]
    fn clearing_a_binding_round_trips_as_unset() {
        let mut s = AppSettings::default();
        s.set_hotkey_for(HotkeyAction::ToggleRecording,
                         Some(hk(Modifiers::ALT, 0x52)));
        s.set_hotkey_for(HotkeyAction::ToggleRecording, None);
        assert_eq!(s.record_hotkey_modifiers, 0);
        assert_eq!(s.record_hotkey_virtual_key, UNSET_VK);
        let s2: AppSettings =
            serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s2.hotkey_for(HotkeyAction::ToggleRecording), None);
    }

    /// A settings.json written before multiple hotkeys existed must load
    /// with its capture binding intact and the two new actions unbound.
    #[test]
    fn legacy_file_loads_unchanged() {
        let legacy = r#"{
            "LastTool": "Arrow",
            "LastColorArgb": -65536,
            "LastThickness": 3.0,
            "SaveDirectory": "",
            "RecordingsDirectory": "",
            "HotkeyModifiers": 6,
            "HotkeyVirtualKey": 80,
            "StartWithWindows": false,
            "WatermarkEnabled": true,
            "WatermarkText": "KAShot",
            "WatermarkOpacity": 0.85,
            "WatermarkPosition": "BottomRight",
            "PaletteIndex": 0,
            "Theme": "Dark",
            "MarkerOpacity": 200
        }"#;
        let s: AppSettings = serde_json::from_str(legacy).unwrap();
        assert_eq!(s.hotkey_for(HotkeyAction::Capture),
                   Some(hk(Modifiers::CONTROL | Modifiers::SHIFT, 0x50)));
        assert_eq!(s.hotkey_for(HotkeyAction::CaptureFullScreen), None);
        assert_eq!(s.hotkey_for(HotkeyAction::ToggleRecording), None);
        assert_eq!(s.hotkey_bindings(), vec![
            (HotkeyAction::Capture, hk(Modifiers::CONTROL | Modifiers::SHIFT, 0x50)),
        ]);
        // Untouched fields survive the added keys.
        assert_eq!(s.last_tool, "Arrow");
        assert_eq!(s.theme, "Dark");
        assert_eq!(s.marker_opacity, 200);
    }

    /// An empty object is the other legacy shape: nothing bound but the
    /// per-platform capture default.
    #[test]
    fn empty_file_binds_only_capture() {
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.hotkey_for(HotkeyAction::Capture), Some(Hotkey::default()));
        assert_eq!(s.hotkey_bindings().len(), 1);
        assert_eq!(s.hotkey_conflict(), None);
    }

    #[test]
    fn identical_chords_conflict() {
        let mut s = AppSettings::default();
        let chord = hk(Modifiers::CONTROL | Modifiers::SHIFT, 0x52);
        s.set_hotkey_for(HotkeyAction::CaptureFullScreen, Some(chord));
        s.set_hotkey_for(HotkeyAction::ToggleRecording, Some(chord));
        assert_eq!(s.hotkey_conflict(),
                   Some((HotkeyAction::CaptureFullScreen, HotkeyAction::ToggleRecording)));
    }

    #[test]
    fn conflict_reports_the_first_pair_in_dialog_order() {
        let mut s = AppSettings::default();
        let primary = s.hotkey();
        s.set_hotkey_for(HotkeyAction::ToggleRecording, Some(primary));
        s.set_hotkey_for(HotkeyAction::CaptureFullScreen, Some(primary));
        assert_eq!(s.hotkey_conflict(),
                   Some((HotkeyAction::Capture, HotkeyAction::CaptureFullScreen)));
    }

    #[test]
    fn same_key_with_different_modifiers_is_not_a_conflict() {
        let mut s = AppSettings::default();
        s.set_hotkey_for(HotkeyAction::Capture, Some(hk(Modifiers::CONTROL, 0x52)));
        s.set_hotkey_for(HotkeyAction::CaptureFullScreen,
                         Some(hk(Modifiers::CONTROL | Modifiers::SHIFT, 0x52)));
        s.set_hotkey_for(HotkeyAction::ToggleRecording, Some(hk(Modifiers::ALT, 0x52)));
        assert_eq!(s.hotkey_conflict(), None);
    }

    /// Two unbound actions are both "unset" but must never read as a clash.
    #[test]
    fn unbound_actions_never_conflict() {
        let mut s = AppSettings::default();
        s.set_hotkey_for(HotkeyAction::CaptureFullScreen, None);
        s.set_hotkey_for(HotkeyAction::ToggleRecording, None);
        assert_eq!(s.hotkey_conflict(), None);
    }

    #[test]
    fn labels_are_ascii_for_the_bitmap_font() {
        for a in HotkeyAction::ALL {
            assert!(a.label().is_ascii(), "{:?}", a);
        }
        let msg = conflict_message(HotkeyAction::Capture,
                                   HotkeyAction::ToggleRecording,
                                   Hotkey::default());
        assert!(msg.is_ascii(), "{msg}");
    }

    /// The dialog draws this as one unwrapped line of 5x7 bitmap text in a
    /// 640 px window with 22 px padding: 596 px of room, 6 px per glyph
    /// (5 wide plus a 1 px gap, the trailing gap trimmed), so 99 characters.
    /// The worst case is the two longest labels plus every modifier and the
    /// longest key name.
    #[test]
    fn worst_case_conflict_message_fits_the_footer() {
        let every_mod = Modifiers::CONTROL | Modifiers::SHIFT
            | Modifiers::ALT | Modifiers::SUPER;
        let widest = HotkeyAction::ALL.iter()
            .map(|a| a.label().len())
            .max().unwrap();
        let msg = conflict_message(HotkeyAction::CaptureFullScreen,
                                   HotkeyAction::ToggleRecording,
                                   hk(every_mod, 0x2C));
        assert_eq!(widest, HotkeyAction::CaptureFullScreen.label().len());
        assert!(msg.len() <= 99, "{} chars: {msg}", msg.len());
    }

    #[test]
    fn only_capture_is_mandatory() {
        assert!(!HotkeyAction::Capture.is_optional());
        assert!(HotkeyAction::CaptureFullScreen.is_optional());
        assert!(HotkeyAction::ToggleRecording.is_optional());
    }
}
