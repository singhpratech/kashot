//! Global hotkey registration via the `global-hotkey` crate, which wraps
//! `RegisterHotKey` (Win32), X11 `XGrabKey` (Linux), and
//! `RegisterEventHotKey` (macOS).
//!
//! On Linux the X11 grab is the only backend `global-hotkey` 0.7 has -- there
//! is no Wayland implementation and no portal fallback. Under a Wayland
//! session registration still reports success (XWayland answers the grab) but
//! the key is never delivered, so the hotkey silently cannot fire; those users
//! have to trigger captures from the tray menu.

use crate::{Error, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers as GhMods},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
use kashot_core::hotkeys::HotkeyAction;
use kashot_core::settings::{Hotkey, Modifiers};

pub struct HotkeyManager {
    inner:    GlobalHotKeyManager,
    /// Every binding currently held with the OS, paired with the action it
    /// fires. A flat vec rather than a map: it holds at most three entries,
    /// so a linear scan per delivered event beats hashing.
    current:  Vec<(HotkeyAction, HotKey)>,
}

impl HotkeyManager {
    pub fn new() -> Result<Self> {
        let inner = GlobalHotKeyManager::new()
            .map_err(|e| Error::Hotkey(e.to_string()))?;
        Ok(HotkeyManager { inner, current: Vec::new() })
    }

    /// Make `bindings` the complete set of registered hotkeys, replacing
    /// whatever was registered before.
    ///
    /// Registration is per-binding best-effort: a chord the OS refuses
    /// (already owned by the desktop environment) or a VK this build cannot
    /// translate is skipped and reported, while the rest still take effect.
    /// One unavailable shortcut must not cost the user the other two, so the
    /// failures come back as a list instead of aborting the whole call --
    /// the caller logs them and carries on.
    pub fn register_all(&mut self, bindings: &[(HotkeyAction, Hotkey)])
        -> Vec<(HotkeyAction, Error)>
    {
        self.unregister_all();
        let mut failed = Vec::new();
        for &(action, hk) in bindings {
            if let Err(e) = self.register_one(action, hk) {
                failed.push((action, e));
            }
        }
        failed
    }

    /// Register a single binding and remember which action it belongs to.
    fn register_one(&mut self, action: HotkeyAction, hk: Hotkey) -> Result<()> {
        let mods = translate_mods(hk.modifiers);
        let code = vk_to_code(hk.virtual_key)
            .ok_or_else(|| Error::Hotkey(format!("unknown vk 0x{:X}", hk.virtual_key)))?;
        let item = HotKey::new(Some(mods), code);
        self.inner.register(item).map_err(|e| Error::Hotkey(e.to_string()))?;
        self.current.push((action, item));
        Ok(())
    }

    /// Drop every registered binding. Used while the Settings dialog is open
    /// so the rebind widget can see the keys the user presses instead of
    /// them firing a capture.
    pub fn unregister_all(&mut self) {
        for (_, prev) in self.current.drain(..) {
            let _ = self.inner.unregister(prev);
        }
    }

    /// Drain pending press events and report which actions they fired, in
    /// the order the presses arrived.
    ///
    /// A given action is reported at most once per drain: holding the key
    /// down queues several `Pressed` events between two polls of the event
    /// loop, and one keypress has to mean one capture.
    pub fn drain_pressed(&self) -> Vec<HotkeyAction> {
        let mut fired: Vec<HotkeyAction> = Vec::new();
        while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
            if ev.state != global_hotkey::HotKeyState::Pressed { continue; }
            let Some(&(action, _)) = self.current.iter().find(|(_, h)| h.id() == ev.id)
                else { continue; };
            if !fired.contains(&action) { fired.push(action); }
        }
        fired
    }
}

fn translate_mods(m: Modifiers) -> GhMods {
    let mut out = GhMods::empty();
    if m.contains(Modifiers::ALT)     { out |= GhMods::ALT; }
    if m.contains(Modifiers::CONTROL) { out |= GhMods::CONTROL; }
    if m.contains(Modifiers::SHIFT)   { out |= GhMods::SHIFT; }
    if m.contains(Modifiers::SUPER)   { out |= GhMods::SUPER; }
    out
}

/// Map a Win32 virtual-key code to a `global-hotkey` `Code`.
///
/// The Win32 VK space is the canonical wire format we share with the C# version
/// (settings.json stores `HotkeyVirtualKey` as a Win32 VK). On non-Windows
/// platforms `global-hotkey` re-translates these to the OS's native codes
/// internally. That translation is purely mechanical: `0x2C` becomes Carbon
/// keycode `0x46` on macOS, which no Apple keyboard can produce, which is why
/// the default hotkey there is Cmd+Shift+`7` (`0x37`) rather than Print Screen.
fn vk_to_code(vk: u32) -> Option<Code> {
    Some(match vk {
        0x08 => Code::Backspace,
        0x09 => Code::Tab,
        0x0D => Code::Enter,
        0x10 => Code::ShiftLeft,
        0x11 => Code::ControlLeft,
        0x12 => Code::AltLeft,
        0x14 => Code::CapsLock,
        0x1B => Code::Escape,
        0x20 => Code::Space,
        0x21 => Code::PageUp,
        0x22 => Code::PageDown,
        0x23 => Code::End,
        0x24 => Code::Home,
        0x25 => Code::ArrowLeft,
        0x26 => Code::ArrowUp,
        0x27 => Code::ArrowRight,
        0x28 => Code::ArrowDown,
        0x2C => Code::PrintScreen,
        0x2D => Code::Insert,
        0x2E => Code::Delete,
        0x30..=0x39 => match vk {
            0x30 => Code::Digit0, 0x31 => Code::Digit1, 0x32 => Code::Digit2,
            0x33 => Code::Digit3, 0x34 => Code::Digit4, 0x35 => Code::Digit5,
            0x36 => Code::Digit6, 0x37 => Code::Digit7, 0x38 => Code::Digit8,
            0x39 => Code::Digit9, _ => unreachable!(),
        },
        0x41..=0x5A => match vk {
            0x41 => Code::KeyA, 0x42 => Code::KeyB, 0x43 => Code::KeyC,
            0x44 => Code::KeyD, 0x45 => Code::KeyE, 0x46 => Code::KeyF,
            0x47 => Code::KeyG, 0x48 => Code::KeyH, 0x49 => Code::KeyI,
            0x4A => Code::KeyJ, 0x4B => Code::KeyK, 0x4C => Code::KeyL,
            0x4D => Code::KeyM, 0x4E => Code::KeyN, 0x4F => Code::KeyO,
            0x50 => Code::KeyP, 0x51 => Code::KeyQ, 0x52 => Code::KeyR,
            0x53 => Code::KeyS, 0x54 => Code::KeyT, 0x55 => Code::KeyU,
            0x56 => Code::KeyV, 0x57 => Code::KeyW, 0x58 => Code::KeyX,
            0x59 => Code::KeyY, 0x5A => Code::KeyZ, _ => unreachable!(),
        },
        0x70..=0x7B => match vk {
            0x70 => Code::F1,  0x71 => Code::F2,  0x72 => Code::F3,
            0x73 => Code::F4,  0x74 => Code::F5,  0x75 => Code::F6,
            0x76 => Code::F7,  0x77 => Code::F8,  0x78 => Code::F9,
            0x79 => Code::F10, 0x7A => Code::F11, 0x7B => Code::F12,
            _ => unreachable!(),
        },
        _ => return None,
    })
}
