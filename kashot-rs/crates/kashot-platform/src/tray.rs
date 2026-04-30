//! Tray icon. Uses `tray-icon` which speaks `Shell_NotifyIcon` (Win32),
//! `StatusNotifierItem` (Linux DBus / KDE / GNOME w/ extension), and
//! `NSStatusBar` (macOS).
//!
//! The tray runs on the main thread's event loop; drain `try_recv` periodically
//! from the iced subscription that owns the loop.

use crate::{Error, Result};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon as TrayIconImage, TrayIcon, TrayIconBuilder};

pub struct Tray {
    _icon: TrayIcon,
    pub capture_id: tray_icon::menu::MenuId,
    pub settings_id: tray_icon::menu::MenuId,
    pub about_id: tray_icon::menu::MenuId,
    pub exit_id:   tray_icon::menu::MenuId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    None,
    Capture,
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

        let capture_id  = capture.id().clone();
        let settings_id = settings.id().clone();
        let about_id    = about.id().clone();
        let exit_id     = exit.id().clone();

        menu.append(&capture).map_err(|e| Error::Tray(e.to_string()))?;
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

/// Procedurally render the Kashot tray icon (yellow → cyan-green gradient
/// rounded square + four corner brackets + center dot) into a 32×32 RGBA
/// buffer. Same look as the C# `IconPath` in `AboutForm`. No image asset
/// required — keeps the binary small and avoids file-system surprises.
fn build_icon() -> TrayIconImage {
    let size = 32u32;
    let mut buf = vec![0u8; (size * size * 4) as usize];

    let radius = 7;
    let pad = 1;

    // Gradient + rounded-rectangle silhouette
    for y in 0..size as i32 {
        for x in 0..size as i32 {
            let inside = inside_rounded_rect(x, y, pad, pad, size as i32 - 2 * pad, size as i32 - 2 * pad, radius);
            if !inside { continue; }

            // Diagonal gradient: yellow (#ffe600) → cyan-green (#00e6c0)
            let t = ((x + y) as f32) / ((2 * size as i32) as f32);
            let r = lerp(0xff, 0x00, t);
            let g = lerp(0xe6, 0xe6, t);
            let b = lerp(0x00, 0xc0, t);

            put(&mut buf, size, x as u32, y as u32, [r, g, b, 0xff]);
        }
    }

    // Four corner brackets in white
    let bracket = 5;
    let inset   = 6;
    let stroke  = 2;
    let s = size as i32;
    let strokes = [
        // top-left
        (inset, inset, inset + bracket, inset, true),  (inset, inset, inset, inset + bracket, false),
        // top-right
        (s - inset - bracket - 1, inset, s - inset - 1, inset, true),
        (s - inset - 1, inset, s - inset - 1, inset + bracket, false),
        // bottom-left
        (inset, s - inset - 1, inset + bracket, s - inset - 1, true),
        (inset, s - inset - bracket - 1, inset, s - inset - 1, false),
        // bottom-right
        (s - inset - bracket - 1, s - inset - 1, s - inset - 1, s - inset - 1, true),
        (s - inset - 1, s - inset - bracket - 1, s - inset - 1, s - inset - 1, false),
    ];
    for &(x0, y0, x1, y1, horiz) in &strokes {
        if horiz {
            for x in x0..=x1 {
                for w in 0..stroke {
                    put(&mut buf, size, x as u32, (y0 + w) as u32, [0xff, 0xff, 0xff, 0xff]);
                }
            }
        } else {
            for y in y0..=y1 {
                for w in 0..stroke {
                    put(&mut buf, size, (x0 + w) as u32, y as u32, [0xff, 0xff, 0xff, 0xff]);
                }
            }
        }
    }

    // Center dot
    let cx = size as i32 / 2;
    let cy = size as i32 / 2;
    for y in (cy - 2)..=(cy + 2) {
        for x in (cx - 2)..=(cx + 2) {
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= 4 {
                put(&mut buf, size, x as u32, y as u32, [0xff, 0xff, 0xff, 0xff]);
            }
        }
    }

    TrayIconImage::from_rgba(buf, size, size)
        .expect("32×32 RGBA → tray Icon should always succeed")
}

fn inside_rounded_rect(px: i32, py: i32, x: i32, y: i32, w: i32, h: i32, r: i32) -> bool {
    if px < x || py < y || px >= x + w || py >= y + h { return false; }
    let nx = px - x;
    let ny = py - y;
    if nx < r && ny < r {
        let dx = r - nx;
        let dy = r - ny;
        return dx * dx + dy * dy <= r * r;
    }
    if nx >= w - r && ny < r {
        let dx = nx - (w - 1 - r);
        let dy = r - ny;
        return dx * dx + dy * dy <= r * r;
    }
    if nx < r && ny >= h - r {
        let dx = r - nx;
        let dy = ny - (h - 1 - r);
        return dx * dx + dy * dy <= r * r;
    }
    if nx >= w - r && ny >= h - r {
        let dx = nx - (w - 1 - r);
        let dy = ny - (h - 1 - r);
        return dx * dx + dy * dy <= r * r;
    }
    true
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    let t = t.clamp(0.0, 1.0);
    ((a as f32) * (1.0 - t) + (b as f32) * t).round() as u8
}

fn put(buf: &mut [u8], stride: u32, x: u32, y: u32, rgba: [u8; 4]) {
    if x >= stride { return; }
    let idx = ((y * stride + x) * 4) as usize;
    if idx + 4 > buf.len() { return; }
    buf[idx..idx + 4].copy_from_slice(&rgba);
}
