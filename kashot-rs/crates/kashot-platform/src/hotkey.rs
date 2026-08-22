//! Global hotkey registration.
//!
//! Two backends behind one type, chosen at runtime by session:
//!
//! * **Grab** — the `global-hotkey` crate, which wraps `RegisterHotKey`
//!   (Win32), X11 `XGrabKey` (Linux/X11) and `RegisterEventHotKey` (macOS).
//!   Used everywhere except a Linux Wayland session.
//! * **Portal** — `org.freedesktop.portal.GlobalShortcuts` via
//!   [`crate::hotkey_portal`]. Used on Linux Wayland only.
//!
//! The split exists because `global-hotkey` 0.7 has no Wayland implementation
//! and the X11 one is *worse than nothing* there: XWayland answers the grab,
//! so registration reports success, and then the key is never delivered.
//! A hotkey that silently cannot fire is indistinguishable from a broken app,
//! so on Wayland we never fall back to the grab — if the portal isn't
//! available the error says so, and the tray menu remains the way to capture.
//!
//! Callers see one API either way: construct, [`register`](HotkeyManager::register)
//! a [`Hotkey`], re-register to rebind, and poll
//! [`drain_pressed`](HotkeyManager::drain_pressed) from the event loop.

use crate::{Error, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers as GhMods},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
use kashot_core::settings::{Hotkey, Modifiers};

/// Which mechanism is delivering presses. Surfaced so the app can say the
/// right thing about a hotkey that isn't firing — the two backends fail for
/// completely different reasons and have completely different fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyBackend {
    /// A window-system key grab: what we asked for is what fires.
    Grab,
    /// An xdg-desktop-portal request: the compositor decides the real binding
    /// and can show it to the user under its own keyboard settings.
    Portal,
}

impl HotkeyBackend {
    pub fn is_portal(self) -> bool { matches!(self, HotkeyBackend::Portal) }
}

pub struct HotkeyManager {
    backend: Backend,
}

enum Backend {
    Grab {
        inner:   GlobalHotKeyManager,
        current: Option<HotKey>,
    },
    #[cfg(target_os = "linux")]
    Portal(crate::hotkey_portal::PortalHotkeys),
}

impl HotkeyManager {
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "linux")]
        if crate::session::is_wayland() {
            let portal = crate::hotkey_portal::PortalHotkeys::new()?;
            return Ok(HotkeyManager { backend: Backend::Portal(portal) });
        }

        let inner = GlobalHotKeyManager::new()
            .map_err(|e| Error::Hotkey(e.to_string()))?;
        Ok(HotkeyManager { backend: Backend::Grab { inner, current: None } })
    }

    /// Which mechanism this manager ended up using.
    pub fn backend(&self) -> HotkeyBackend {
        match &self.backend {
            Backend::Grab { .. } => HotkeyBackend::Grab,
            #[cfg(target_os = "linux")]
            Backend::Portal(_)   => HotkeyBackend::Portal,
        }
    }

    /// Register `hk` as the active hotkey. Replaces any previously registered one.
    ///
    /// On the portal backend `Ok` means the request was accepted, not that the
    /// key is live — only the compositor can decide that. See
    /// [`crate::hotkey_portal`].
    pub fn register(&mut self, hk: Hotkey) -> Result<HotkeyHandle> {
        match &mut self.backend {
            Backend::Grab { inner, current } => {
                if let Some(prev) = current.take() {
                    let _ = inner.unregister(prev);
                }

                let mods = translate_mods(hk.modifiers);
                let code = vk_to_code(hk.virtual_key)
                    .ok_or_else(|| Error::Hotkey(format!("unknown vk 0x{:X}", hk.virtual_key)))?;
                let item = HotKey::new(Some(mods), code);

                inner.register(item).map_err(|e| Error::Hotkey(e.to_string()))?;
                *current = Some(item);
                Ok(HotkeyHandle { id: item.id() })
            }
            #[cfg(target_os = "linux")]
            Backend::Portal(p) => {
                p.register(hk)?;
                // The portal identifies shortcuts by a string id, not a
                // numeric handle. Nothing consumes the id (presses are matched
                // inside the backend), so a constant keeps the shared shape.
                Ok(HotkeyHandle { id: 0 })
            }
        }
    }

    pub fn unregister(&mut self) {
        match &mut self.backend {
            Backend::Grab { inner, current } => {
                if let Some(prev) = current.take() {
                    let _ = inner.unregister(prev);
                }
            }
            #[cfg(target_os = "linux")]
            Backend::Portal(p) => p.unregister(),
        }
    }

    /// Drain pending press events. Returns `true` if any of them matches the
    /// currently registered hotkey.
    pub fn drain_pressed(&self) -> bool {
        match &self.backend {
            Backend::Grab { current, .. } => {
                let mut hit = false;
                let target = current.map(|h| h.id());
                while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
                    if ev.state == global_hotkey::HotKeyState::Pressed
                        && Some(ev.id) == target
                    {
                        hit = true;
                    }
                }
                hit
            }
            #[cfg(target_os = "linux")]
            Backend::Portal(p) => p.drain_pressed(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HotkeyHandle {
    pub id: u32,
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
