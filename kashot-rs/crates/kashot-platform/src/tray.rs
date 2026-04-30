//! Tray icon. Uses `tray-icon` which speaks `Shell_NotifyIcon` (Win32),
//! `StatusNotifierItem` (Linux DBus / KDE / GNOME w/ extension), and
//! `NSStatusBar` (macOS).
//!
//! The tray runs on the main thread's event loop; drain `try_recv` periodically
//! from the iced subscription that owns the loop.

use crate::{Error, Result};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon as TrayIconImage, TrayIcon, TrayIconBuilder};

pub struct Tray {
    _icon: TrayIcon,
    pub capture_id:  tray_icon::menu::MenuId,
    pub delay3_id:   tray_icon::menu::MenuId,
    pub delay5_id:   tray_icon::menu::MenuId,
    pub delay10_id:  tray_icon::menu::MenuId,
    pub settings_id: tray_icon::menu::MenuId,
    pub about_id:    tray_icon::menu::MenuId,
    pub exit_id:     tray_icon::menu::MenuId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    None,
    Capture,
    /// Capture after N seconds. Lets the user dismiss menus, position
    /// windows, etc. before the screenshot fires.
    CaptureDelayed(u32),
    Settings,
    About,
    Exit,
}

impl Tray {
    /// Build the tray icon with the default Kashot menu. `tooltip` shows the
    /// current hotkey, e.g. `"Kashot — press PrintScreen to capture"`.
    ///
    /// On Linux this calls `gtk::init()` first — the tray-icon backend uses
    /// libayatana-appindicator which requires GTK to be initialized on the
    /// main thread before any menu item is constructed. `gtk::init()` is
    /// idempotent and returns Err only if no display server is reachable, in
    /// which case we surface that as a regular `Tray` init failure.
    pub fn new(tooltip: impl Into<String>) -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            gtk::init().map_err(|e| Error::Tray(format!("gtk::init: {e}")))?;
        }

        let menu = Menu::new();

        let capture  = MenuItem::new("Capture Screen",   true, None);
        let settings = MenuItem::new("Settings…",        true, None);
        let about    = MenuItem::new("About Kashot…",    true, None);
        let exit     = MenuItem::new("Exit",             true, None);
        let sep1     = PredefinedMenuItem::separator();
        let sep2     = PredefinedMenuItem::separator();

        // "Capture after delay…" submenu — three preset durations covering
        // the common screenshot-tool delay use cases (open a menu, focus a
        // window, dismiss a tooltip, etc.) without a free-form input UI.
        let delay_menu = Submenu::new("Capture after delay…", true);
        let delay_3s   = MenuItem::new("3 seconds",  true, None);
        let delay_5s   = MenuItem::new("5 seconds",  true, None);
        let delay_10s  = MenuItem::new("10 seconds", true, None);

        let capture_id  = capture.id().clone();
        let delay3_id   = delay_3s.id().clone();
        let delay5_id   = delay_5s.id().clone();
        let delay10_id  = delay_10s.id().clone();
        let settings_id = settings.id().clone();
        let about_id    = about.id().clone();
        let exit_id     = exit.id().clone();

        delay_menu.append(&delay_3s ).map_err(|e| Error::Tray(e.to_string()))?;
        delay_menu.append(&delay_5s ).map_err(|e| Error::Tray(e.to_string()))?;
        delay_menu.append(&delay_10s).map_err(|e| Error::Tray(e.to_string()))?;

        menu.append(&capture).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&delay_menu).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&sep1).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&settings).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&about).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&sep2).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&exit).map_err(|e| Error::Tray(e.to_string()))?;

        let img = build_icon();
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(img)
            .with_tooltip(tooltip.into())
            .build()
            .map_err(|e| Error::Tray(e.to_string()))?;

        Ok(Tray {
            _icon: tray_icon,
            capture_id,
            delay3_id,
            delay5_id,
            delay10_id,
            settings_id,
            about_id,
            exit_id,
        })
    }

    /// Drain the next pending menu event into a `TrayEvent`. Returns `None`
    /// when the queue is empty for this tick.
    pub fn try_recv(&self) -> TrayEvent {
        match MenuEvent::receiver().try_recv() {
            Ok(ev) if ev.id == self.capture_id  => TrayEvent::Capture,
            Ok(ev) if ev.id == self.delay3_id   => TrayEvent::CaptureDelayed(3),
            Ok(ev) if ev.id == self.delay5_id   => TrayEvent::CaptureDelayed(5),
            Ok(ev) if ev.id == self.delay10_id  => TrayEvent::CaptureDelayed(10),
            Ok(ev) if ev.id == self.settings_id => TrayEvent::Settings,
            Ok(ev) if ev.id == self.about_id    => TrayEvent::About,
            Ok(ev) if ev.id == self.exit_id     => TrayEvent::Exit,
            _ => TrayEvent::None,
        }
    }

    /// Pump platform-native event sources that the tray relies on but that
    /// aren't otherwise driven by our winit loop. On Linux this drains GTK's
    /// main context so menu-click signals get delivered into the channel that
    /// `try_recv` reads. No-op on Windows / macOS — the platform event loop
    /// already drives those backends through winit.
    pub fn pump_events(&self) {
        #[cfg(target_os = "linux")]
        {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
        }
    }
}

/// Build the tray icon from the brand PNG that ships in `icons/`. Embedding
/// the bytes at compile time means the running binary's tray icon, the
/// installed launcher icon (`icons/linux_hicolor/...`), and the master file
/// in `icons/` are all the *same* artwork — no procedural fallback that
/// drifts visually from the brand.
///
/// 64×64 is the source size: large enough that panel resampling (Linux tray
/// panels typically render at 16–22 px) stays clean, small enough that
/// embedding doesn't bloat the binary. The decoded RGBA is handed to
/// `tray-icon`, which resamples per-platform.
fn build_icon() -> TrayIconImage {
    const ICON_PNG: &[u8] = include_bytes!(
        "../../../../icons/linux_hicolor/64x64/apps/kashot.png"
    );
    let img = image::load_from_memory(ICON_PNG)
        .expect("embedded brand PNG must decode")
        .into_rgba8();
    let (w, h) = img.dimensions();
    TrayIconImage::from_rgba(img.into_raw(), w, h)
        .expect("decoded RGBA must build a tray Icon")
}
