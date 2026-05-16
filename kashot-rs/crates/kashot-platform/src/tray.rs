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
    pub capture_id:    tray_icon::menu::MenuId,
    pub delay3_id:     tray_icon::menu::MenuId,
    pub delay5_id:     tray_icon::menu::MenuId,
    pub delay10_id:    tray_icon::menu::MenuId,
    pub cancel_id:     tray_icon::menu::MenuId,
    pub rec_none_id:   tray_icon::menu::MenuId,
    pub rec_mic_id:    tray_icon::menu::MenuId,
    pub rec_sys_id:    tray_icon::menu::MenuId,
    pub rec_both_id:   tray_icon::menu::MenuId,
    pub stop_rec_id:   tray_icon::menu::MenuId,
    pub open_folder_id:tray_icon::menu::MenuId,
    pub open_recs_id:  tray_icon::menu::MenuId,
    pub settings_id:   tray_icon::menu::MenuId,
    pub about_id:      tray_icon::menu::MenuId,
    pub updates_id:    tray_icon::menu::MenuId,
    pub convert_img_id: tray_icon::menu::MenuId,
    pub convert_vid_id: tray_icon::menu::MenuId,
    pub exit_id:       tray_icon::menu::MenuId,
    rec_none_item:     MenuItem,
    rec_mic_item:      MenuItem,
    rec_sys_item:      MenuItem,
    rec_both_item:     MenuItem,
    stop_rec_item:     MenuItem,
    cancel_item:       MenuItem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    None,
    Capture,
    /// Capture after N seconds. Lets the user dismiss menus, position
    /// windows, etc. before the screenshot fires.
    CaptureDelayed(u32),
    /// Cancel the in-flight delay capture. Polled during the delay loop —
    /// resets `_capturing` and skips the screenshot.
    CancelPending,
    /// Begin recording with the given audio sources mixed in.
    StartRecording(crate::recorder::RecordingOptions),
    StopRecording,
    /// Open the configured screenshot save folder in the user's default
    /// file manager. Mirrors C# TrayContext "Open Save Folder".
    OpenSaveFolder,
    /// Open the recordings folder (typically `~/Videos`).
    OpenRecordingsFolder,
    Settings,
    About,
    /// Open the GitHub Releases page so the user can grab the latest build.
    /// Mirrors C# TrayContext "Check for updates".
    CheckForUpdates,
    /// Open the themed "Convert image" dialog (PNG ↔ JPG ↔ WEBP ↔ BMP).
    ConvertImage,
    /// Open the themed "Convert video" dialog (MP4 → MOV / WEBM / MKV / GIF).
    /// Requires a bundled or system-installed `ffmpeg` binary at runtime.
    ConvertVideo,
    Exit,
}

impl Tray {
    /// Build the tray icon with the default Kashot menu. `tooltip` shows the
    /// current hotkey, e.g. `"KAShot — press PrintScreen to capture"`.
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

        // Flat menu — no submenus. Cinnamon / KDE / a few GNOME extensions
        // mis-render scrollable submenus from `StatusNotifierItem` (items
        // truncate or overlap), so we hoist every preset to a top-level row.
        // The trade is a slightly taller menu in exchange for menu items that
        // actually render correctly on every Linux DE we've tested.
        // Labels intentionally kept tight — Cinnamon's DBusMenu renderer
        // (and a few KDE plasmoids) truncates wider strings with an ellipsis.
        // Every label here is one or two readable phrases at most.
        let capture   = MenuItem::new("Capture",           true,  None);
        let delay_3s  = MenuItem::new("Capture in 3s",     true,  None);
        let delay_5s  = MenuItem::new("Capture in 5s",     true,  None);
        let delay_10s = MenuItem::new("Capture in 10s",    true,  None);
        // Disabled until a delay is actually in flight — the tray loop calls
        // `set_pending` to enable/disable as `capture_after` enters/exits.
        let cancel    = MenuItem::new("Cancel pending",    false, None);

        // Four record modes flattened to siblings. Plain text labels (no
        // emoji) so they render identically on every backend.
        let rec_none  = MenuItem::new("Record",            true,  None);
        let rec_mic   = MenuItem::new("Record + mic",      true,  None);
        let rec_sys   = MenuItem::new("Record + audio",    true,  None);
        let rec_both  = MenuItem::new("Record + mic+audio",true,  None);
        let stop_rec  = MenuItem::new("Stop recording",    false, None);

        let open_fold = MenuItem::new("Open shots",        true,  None);
        let open_recs = MenuItem::new("Open recordings",   true,  None);
        let convert_img = MenuItem::new("Convert image",   true,  None);
        let convert_vid = MenuItem::new("Convert video",   true,  None);
        let settings  = MenuItem::new("Settings",          true,  None);
        let about     = MenuItem::new("About",             true,  None);
        let updates   = MenuItem::new("Check for updates", true,  None);
        let exit      = MenuItem::new("Exit",              true,  None);

        let capture_id  = capture.id().clone();
        let delay3_id   = delay_3s.id().clone();
        let delay5_id   = delay_5s.id().clone();
        let delay10_id  = delay_10s.id().clone();
        let cancel_id   = cancel.id().clone();
        let rec_none_id = rec_none.id().clone();
        let rec_mic_id  = rec_mic.id().clone();
        let rec_sys_id  = rec_sys.id().clone();
        let rec_both_id = rec_both.id().clone();
        let stop_rec_id = stop_rec.id().clone();
        let open_folder_id = open_fold.id().clone();
        let open_recs_id   = open_recs.id().clone();
        let settings_id = settings.id().clone();
        let about_id    = about.id().clone();
        let updates_id  = updates.id().clone();
        let convert_img_id = convert_img.id().clone();
        let convert_vid_id = convert_vid.id().clone();
        let exit_id     = exit.id().clone();

        menu.append(&capture).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&delay_3s).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&delay_5s).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&delay_10s).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&cancel).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&PredefinedMenuItem::separator()).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&rec_none).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&rec_mic).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&rec_sys).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&rec_both).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&stop_rec).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&PredefinedMenuItem::separator()).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&open_fold).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&open_recs).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&PredefinedMenuItem::separator()).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&convert_img).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&convert_vid).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&PredefinedMenuItem::separator()).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&settings).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&about).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&updates).map_err(|e| Error::Tray(e.to_string()))?;
        menu.append(&PredefinedMenuItem::separator()).map_err(|e| Error::Tray(e.to_string()))?;
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
            cancel_id,
            rec_none_id,
            rec_mic_id,
            rec_sys_id,
            rec_both_id,
            stop_rec_id,
            open_folder_id,
            open_recs_id,
            settings_id,
            about_id,
            updates_id,
            convert_img_id,
            convert_vid_id,
            exit_id,
            rec_none_item: rec_none,
            rec_mic_item:  rec_mic,
            rec_sys_item:  rec_sys,
            rec_both_item: rec_both,
            stop_rec_item: stop_rec,
            cancel_item:   cancel,
        })
    }

    /// Drain the next pending menu event into a `TrayEvent`. Returns `None`
    /// when the queue is empty for this tick.
    pub fn try_recv(&self) -> TrayEvent {
        match MenuEvent::receiver().try_recv() {
            Ok(ev) if ev.id == self.capture_id   => TrayEvent::Capture,
            Ok(ev) if ev.id == self.delay3_id    => TrayEvent::CaptureDelayed(3),
            Ok(ev) if ev.id == self.delay5_id    => TrayEvent::CaptureDelayed(5),
            Ok(ev) if ev.id == self.delay10_id   => TrayEvent::CaptureDelayed(10),
            Ok(ev) if ev.id == self.cancel_id    => TrayEvent::CancelPending,
            Ok(ev) if ev.id == self.rec_none_id  => TrayEvent::StartRecording(crate::recorder::RecordingOptions::NONE),
            Ok(ev) if ev.id == self.rec_mic_id   => TrayEvent::StartRecording(crate::recorder::RecordingOptions::MIC_ONLY),
            Ok(ev) if ev.id == self.rec_sys_id   => TrayEvent::StartRecording(crate::recorder::RecordingOptions::SYSTEM_ONLY),
            Ok(ev) if ev.id == self.rec_both_id  => TrayEvent::StartRecording(crate::recorder::RecordingOptions::MIC_AND_SYS),
            Ok(ev) if ev.id == self.stop_rec_id  => TrayEvent::StopRecording,
            Ok(ev) if ev.id == self.open_folder_id => TrayEvent::OpenSaveFolder,
            Ok(ev) if ev.id == self.open_recs_id  => TrayEvent::OpenRecordingsFolder,
            Ok(ev) if ev.id == self.settings_id  => TrayEvent::Settings,
            Ok(ev) if ev.id == self.about_id     => TrayEvent::About,
            Ok(ev) if ev.id == self.updates_id   => TrayEvent::CheckForUpdates,
            Ok(ev) if ev.id == self.convert_img_id => TrayEvent::ConvertImage,
            Ok(ev) if ev.id == self.convert_vid_id => TrayEvent::ConvertVideo,
            Ok(ev) if ev.id == self.exit_id      => TrayEvent::Exit,
            _ => TrayEvent::None,
        }
    }

    /// Toggle the "Cancel pending capture" item — enabled only while a
    /// delay capture is in flight, mirroring how `Stop Recording` is gated
    /// by recording state.
    pub fn set_pending(&self, pending: bool) {
        self.cancel_item.set_enabled(pending);
    }

    /// Reflect recording state in the menu — only one of "Record Screen" /
    /// "Stop Recording" is enabled at a time, mirroring the model of the
    /// `Recorder` shim.
    pub fn set_recording(&self, recording: bool) {
        let enabled = !recording;
        self.rec_none_item.set_enabled(enabled);
        self.rec_mic_item .set_enabled(enabled);
        self.rec_sys_item .set_enabled(enabled);
        self.rec_both_item.set_enabled(enabled);
        self.stop_rec_item.set_enabled(recording);
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
