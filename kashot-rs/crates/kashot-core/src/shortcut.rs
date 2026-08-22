//! Rendering a [`Hotkey`] as an XDG "shortcuts" trigger string.
//!
//! Every platform Kashot ships on takes its global hotkey in a different
//! alphabet. Windows and macOS take a Win32 virtual-key code (which is what
//! `settings.json` stores) straight through `global-hotkey`. Wayland does not:
//! there is no key grab to take, so the shortcut is *requested* from the
//! compositor through the `org.freedesktop.portal.GlobalShortcuts` portal, and
//! the portal describes shortcuts as strings in the [shortcuts XDG
//! specification] — modifier names and an XKB keysym name joined by `+`:
//!
//! ```text
//! CTRL+SHIFT+p        SUPER+SHIFT+s        Print
//! ```
//!
//! [shortcuts XDG specification]: https://specifications.freedesktop.org/shortcuts-spec/latest/
//!
//! The translation is pure string work with no system call in it, so it lives
//! here rather than in `kashot-platform`: it can then be unit-tested on any
//! host, including the Windows and macOS CI runners that will never speak to a
//! portal. Keep the key table in sync with `vk_to_code` in
//! `kashot-platform::hotkey` and with `vk_label` in [`crate::settings`] —
//! anything those accept should resolve here too, so a hotkey the rebind widget
//! lets a user pick is a hotkey Wayland can actually be asked for.

use crate::settings::{Hotkey, Modifiers};

/// The XKB keysym name for a Win32 virtual-key code, as the shortcuts
/// specification wants it.
///
/// `None` for two distinct reasons, both of which mean "can't be a trigger":
///
/// * the code isn't one we know how to name (the rebind widget shouldn't be
///   able to produce these, but `settings.json` is user-editable), and
/// * the code is a bare modifier (`VK_SHIFT`, `VK_CONTROL`, `VK_MENU`). Those
///   are valid keys elsewhere in the codebase — `vk_to_code` maps them — but a
///   trigger whose key half is a modifier is not a shortcut, and the portal
///   would reject or silently drop it.
pub fn keysym_name(vk: u32) -> Option<&'static str> {
    /// Keysym names for `VK_0`..`VK_9` (0x30..=0x39). Digits are their own
    /// keysym names in XKB.
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    /// Keysym names for `VK_A`..`VK_Z` (0x41..=0x5A). XKB names the unshifted
    /// letter, so these are lowercase even though the Win32 codes are named
    /// after the capitals.
    const LETTERS: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m",
        "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    /// Keysym names for `VK_F1`..`VK_F12` (0x70..=0x7B).
    const FKEYS: [&str; 12] = [
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    ];

    Some(match vk {
        0x08 => "BackSpace",
        0x09 => "Tab",
        0x0D => "Return",
        0x13 => "Pause",
        0x14 => "Caps_Lock",
        0x1B => "Escape",
        0x20 => "space",
        // X11 kept the pre-PC names for the paging keys; "PageUp" is not a
        // keysym and a portal that validates the string will refuse it.
        0x21 => "Prior",
        0x22 => "Next",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0x2C => "Print",
        0x2D => "Insert",
        0x2E => "Delete",
        0x91 => "Scroll_Lock",
        0x30..=0x39 => DIGITS[(vk - 0x30) as usize],
        0x41..=0x5A => LETTERS[(vk - 0x41) as usize],
        0x70..=0x7B => FKEYS[(vk - 0x70) as usize],
        _ => return None,
    })
}

/// Render `hk` as a shortcuts-specification trigger string, or `None` when its
/// key half has no keysym name (see [`keysym_name`]).
///
/// Modifiers are emitted in the specification's own listing order — CTRL, ALT,
/// SHIFT, SUPER — rather than the Windows-facing order [`Modifiers::describe`]
/// uses. Nothing parses the string back, but compositors show it to the user in
/// their shortcut settings, and matching the spec's order is what makes it look
/// like every other shortcut in that list.
///
/// A trigger with no modifiers at all is legal (`Print` is the Linux default),
/// so an empty modifier set yields the bare key name rather than `None`.
pub fn portal_trigger(hk: &Hotkey) -> Option<String> {
    let key = keysym_name(hk.virtual_key)?;
    let mut parts: Vec<&str> = Vec::with_capacity(5);
    if hk.modifiers.contains(Modifiers::CONTROL) { parts.push("CTRL"); }
    if hk.modifiers.contains(Modifiers::ALT)     { parts.push("ALT"); }
    if hk.modifiers.contains(Modifiers::SHIFT)   { parts.push("SHIFT"); }
    if hk.modifiers.contains(Modifiers::SUPER)   { parts.push("SUPER"); }
    parts.push(key);
    Some(parts.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hk(mods: Modifiers, vk: u32) -> Hotkey {
        Hotkey { modifiers: mods, virtual_key: vk }
    }

    #[test]
    fn bare_key_needs_no_modifiers() {
        // The Linux/Windows default hotkey is an unmodified Print Screen.
        assert_eq!(portal_trigger(&hk(Modifiers::empty(), 0x2C)).unwrap(), "Print");
    }

    #[test]
    fn modifiers_render_in_spec_order() {
        // Requested in a deliberately "wrong" order to prove the output order
        // comes from the renderer and not from the caller.
        let all = Modifiers::SUPER | Modifiers::SHIFT | Modifiers::ALT | Modifiers::CONTROL;
        assert_eq!(portal_trigger(&hk(all, 0x50)).unwrap(), "CTRL+ALT+SHIFT+SUPER+p");
    }

    #[test]
    fn letters_are_lowercase_keysyms() {
        // Win32 names the codes after capitals; XKB names the unshifted key.
        assert_eq!(portal_trigger(&hk(Modifiers::CONTROL, 0x41)).unwrap(), "CTRL+a");
        assert_eq!(portal_trigger(&hk(Modifiers::CONTROL, 0x5A)).unwrap(), "CTRL+z");
    }

    #[test]
    fn digits_and_function_keys_span_their_whole_range() {
        assert_eq!(keysym_name(0x30), Some("0"));
        assert_eq!(keysym_name(0x39), Some("9"));
        assert_eq!(keysym_name(0x70), Some("F1"));
        assert_eq!(keysym_name(0x7B), Some("F12"));
    }

    #[test]
    fn paging_keys_use_the_x11_names() {
        // "PageUp"/"PageDown" are the Windows labels, not keysyms — emitting
        // them would produce a trigger no compositor can bind.
        assert_eq!(keysym_name(0x21), Some("Prior"));
        assert_eq!(keysym_name(0x22), Some("Next"));
    }

    #[test]
    fn bare_modifiers_are_not_triggers() {
        // `vk_to_code` maps these to real `Code`s, so the only thing stopping a
        // hand-edited settings.json from asking for "SHIFT" alone is this.
        for vk in [0x10u32, 0x11, 0x12] {
            assert_eq!(keysym_name(vk), None, "vk 0x{vk:02X} should not be a trigger key");
            assert_eq!(portal_trigger(&hk(Modifiers::CONTROL, vk)), None);
        }
    }

    #[test]
    fn unknown_key_has_no_trigger() {
        // 0xFF is not in any of the ranges; the caller must fall back to an
        // error rather than sending the portal a string it invented.
        assert_eq!(portal_trigger(&hk(Modifiers::CONTROL, 0xFF)), None);
    }

    #[test]
    fn every_labelled_key_can_be_a_trigger() {
        // The rebind widget only offers keys `vk_label` can name. If one of
        // those has no keysym here, a user could pick a hotkey that works on
        // X11 and silently can't be requested on Wayland.
        for vk in 0u32..=0xFF {
            if matches!(vk, 0x10 | 0x11 | 0x12) { continue; } // bare modifiers
            if crate::settings::vk_label(vk).is_some() {
                assert!(keysym_name(vk).is_some(),
                    "vk 0x{vk:02X} has a UI label but no keysym name");
            }
        }
    }
}
