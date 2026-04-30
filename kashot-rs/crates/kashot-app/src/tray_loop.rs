//! Tray-resident lifecycle.
//!
//! Uses `winit` directly because both `tray-icon` and `global-hotkey` deliver
//! events via channels that need the OS event loop to be pumped on the main
//! thread. We poll those channels each tick and dispatch to the capture
//! pipeline.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::Local;
use kashot_core::AppSettings;
use kashot_platform::{
    capture::capture_all_screens,
    hotkey::HotkeyManager,
    recorder::Recorder,
    tray::{Tray, TrayEvent},
};

use crate::editor::{Overlay, OverlayOutcome};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

pub fn run() -> Result<()> {
    let settings = AppSettings::load();

    // Tray + hotkey init are best-effort. If either fails (no DBus / no desktop
    // env / hotkey already taken) the app stays running so the user can fix the
    // issue and try again from the menu later.
    let tray = match Tray::new(tray_tooltip(&settings)) {
        Ok(t) => Some(t),
        Err(e) => { eprintln!("tray init failed: {e}"); None }
    };

    let mut hotkeys = match HotkeyManager::new() {
        Ok(mut hk) => {
            if let Err(e) = hk.register(settings.hotkey()) {
                eprintln!("hotkey register failed: {e} — use tray menu to capture");
            }
            Some(hk)
        }
        Err(e) => { eprintln!("hotkey init failed: {e} — use tray menu to capture"); None }
    };

    eprintln!("Kashot is running. Press {} or use the tray menu to capture.",
        describe_hotkey(&settings));

    struct TrayApp {
        settings:   AppSettings,
        hotkeys:    Option<HotkeyManager>,
        tray:       Option<Tray>,
        recorder:   Recorder,
        /// Active overlay editor window, if a capture-and-edit is in flight.
        /// Holds the captured screenshot and the user's selection state until
        /// they accept (Enter / right-click) or cancel (Esc).
        overlay:    Option<Overlay>,
        last_tick:  Instant,
        capturing:  bool,
    }

    impl TrayApp {
        fn poll(&mut self, loop_target: &ActiveEventLoop) {
            // Drive any platform-native loop the tray depends on (GTK on
            // Linux). Must run before try_recv so menu-click signals have a
            // chance to land in the channel.
            if let Some(tray) = &self.tray {
                tray.pump_events();
                match tray.try_recv() {
                    TrayEvent::None                  => {}
                    TrayEvent::Capture               => self.capture(loop_target),
                    TrayEvent::CaptureDelayed(secs)  => self.capture_after(loop_target, Duration::from_secs(secs as u64)),
                    TrayEvent::StartRecording        => self.start_recording(),
                    TrayEvent::StopRecording         => self.stop_recording(),
                    TrayEvent::Settings              => self.show_settings(),
                    TrayEvent::About                 => self.show_about(),
                    TrayEvent::Exit                  => {
                        // Stop any active recording before tearing down so
                        // the MP4 moov atom gets finalized. Best-effort —
                        // if it errors we still exit.
                        let _ = self.recorder.stop();
                        loop_target.exit();
                    }
                }
            }
            if let Some(hk) = &self.hotkeys {
                if hk.drain_pressed() {
                    self.capture();
                }
            }
        }

        fn capture(&mut self, loop_target: &ActiveEventLoop) {
            self.capture_after(loop_target, Duration::ZERO);
        }

        /// Fire a capture after a user-facing delay, then open the overlay
        /// editor so the user can drag a region. Used by the plain "Capture
        /// Screen" menu/hotkey path (delay = 0) and the "Capture after delay…"
        /// submenu entries (3 / 5 / 10 s).
        ///
        /// During any countdown the tray's GTK main context is still pumped
        /// (on Linux) so the icon stays responsive.
        fn capture_after(&mut self, loop_target: &ActiveEventLoop, user_delay: Duration) {
            if self.capturing || self.overlay.is_some() { return; }
            self.capturing = true;

            if !user_delay.is_zero() {
                eprintln!("Capturing in {} second{}…",
                    user_delay.as_secs(),
                    if user_delay.as_secs() == 1 { "" } else { "s" });
                let until = Instant::now() + user_delay;
                while Instant::now() < until {
                    if let Some(t) = &self.tray { t.pump_events(); }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }

            // Brief settle delay so the tray menu / flyout can fully dismiss
            // before xcap shoots the desktop. On top of any user-facing delay.
            std::thread::sleep(Duration::from_millis(250));

            match capture_all_screens() {
                Ok(shot) => match Overlay::new(loop_target, shot.bitmap) {
                    Ok(ov) => self.overlay = Some(ov),
                    Err(e) => eprintln!("Overlay open failed: {e}"),
                },
                Err(e) => eprintln!("Capture failed: {e}"),
            }

            self.capturing = false;
        }

        /// Persist a final image (already cropped to the user's selection in
        /// the overlay editor) to the configured save directory.
        fn save_final(&mut self, img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) {
            match save_capture(&mut self.settings, &img) {
                Ok(path) => eprintln!("Saved {}", path.display()),
                Err(e)   => eprintln!("Save failed: {e}"),
            }
        }

        /// Start recording the primary display. Output lands in the user's
        /// Videos directory (or a fallback) as `kashot_<timestamp>.mp4`.
        fn start_recording(&mut self) {
            if self.recorder.is_recording() {
                eprintln!("Already recording.");
                return;
            }
            let dir   = recordings_directory();
            let stamp = Local::now().format("%Y%m%d_%H%M%S");
            let out   = dir.join(format!("kashot_{stamp}.mp4"));
            match self.recorder.start(out.clone()) {
                Ok(()) => {
                    eprintln!("Recording → {}", out.display());
                    if let Some(t) = &self.tray { t.set_recording(true); }
                }
                Err(e) => {
                    eprintln!("Recording failed to start: {e}");
                    rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Error)
                        .set_title("Kashot — recording failed")
                        .set_description(format!("{e}"))
                        .show();
                }
            }
        }

        /// Stop the active recording and finalize the file.
        fn stop_recording(&mut self) {
            match self.recorder.stop() {
                Ok(path) => {
                    eprintln!("Saved recording {}", path.display());
                    if let Some(t) = &self.tray { t.set_recording(false); }
                }
                Err(e) => eprintln!("Stop recording failed: {e}"),
            }
        }

        /// Native folder picker for the save directory. Title is kept short
        /// so it fits in panel-truncated dialog headers; the file dialog
        /// itself shows "Save to:" semantically via its own UI.
        fn show_settings(&mut self) {
            let starting = save_directory(&self.settings);
            let picked = rfd::FileDialog::new()
                .set_title("Kashot — Save folder")
                .set_directory(&starting)
                .pick_folder();
            if let Some(p) = picked {
                self.settings.save_directory = p.to_string_lossy().to_string();
                if let Err(e) = self.settings.save() {
                    eprintln!("Failed to persist settings: {e}");
                } else {
                    eprintln!("Saved screenshots will now go to {}", p.display());
                }
            }
        }

        /// Native About modal — version + repo + "View releases" affordance.
        /// Two-button (Yes/No) so the user can jump straight to the GitHub
        /// releases page to check for updates without leaving the app.
        fn show_about(&self) {
            let res = rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Info)
                .set_title("About Kashot")
                .set_description(format!(
                    "Kashot v{}\n\
                     The lightweight screenshot tool.\n\n\
                     github.com/singhpratech/kashot · MIT\n\n\
                     Open the releases page to check for updates?",
                    env!("CARGO_PKG_VERSION")
                ))
                .set_buttons(rfd::MessageButtons::YesNo)
                .show();
            if res == rfd::MessageDialogResult::Yes {
                open_url("https://github.com/singhpratech/kashot/releases");
            }
        }
    }

    impl ApplicationHandler for TrayApp {
        fn resumed(&mut self, _: &ActiveEventLoop) {}

        fn window_event(&mut self, _loop_target: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
            // Route the event into the overlay editor if it owns the window.
            // Anything else (close-requested on a phantom window, stray events
            // from a destroyed surface) is silently dropped.
            let drop_overlay;
            let mut accepted: Option<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>> = None;
            if let Some(ov) = self.overlay.as_mut() {
                if ov.window_id() == id {
                    match ov.handle_event(ev) {
                        OverlayOutcome::Continue       => { drop_overlay = false; }
                        OverlayOutcome::Cancelled      => { drop_overlay = true; }
                        OverlayOutcome::Accepted(img)  => { drop_overlay = true; accepted = Some(img); }
                    }
                } else { drop_overlay = false; }
            } else { drop_overlay = false; }

            if drop_overlay { self.overlay = None; }
            if let Some(img) = accepted { self.save_final(img); }
        }

        fn about_to_wait(&mut self, loop_target: &ActiveEventLoop) {
            // Poll roughly 30Hz when idle.
            let now = Instant::now();
            if now.duration_since(self.last_tick) >= Duration::from_millis(33) {
                self.poll(loop_target);
                self.last_tick = now;
            }
            loop_target.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(33)));
        }
    }

    let event_loop = EventLoop::new().map_err(|e| anyhow!("EventLoop::new: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = TrayApp {
        settings,
        hotkeys: hotkeys.take(),
        tray,
        recorder: Recorder::new(),
        overlay: None,
        last_tick: Instant::now(),
        capturing: false,
    };

    event_loop.run_app(&mut app).map_err(|e| anyhow!("run_app: {e}"))?;

    let _ = app.settings.save();
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn save_capture(
    settings: &mut AppSettings,
    bmp: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
) -> Result<PathBuf> {
    let dir = save_directory(settings);
    std::fs::create_dir_all(&dir)?;
    let stamp = Local::now().format("%Y%m%d_%H%M%S");
    let path  = dir.join(format!("kashot_{stamp}.png"));
    bmp.save(&path)?;
    if settings.save_directory.is_empty() {
        settings.save_directory = dir.to_string_lossy().to_string();
    }
    Ok(path)
}

fn save_directory(s: &AppSettings) -> PathBuf {
    if !s.save_directory.is_empty() {
        let p = PathBuf::from(&s.save_directory);
        if p.is_dir() { return p; }
    }
    if let Some(d) = directories::UserDirs::new().and_then(|u| u.picture_dir().map(|p| p.to_path_buf())) {
        return d;
    }
    std::env::temp_dir()
}

/// Where MP4 recordings land. Prefers the XDG Videos dir, falls back to
/// $HOME, then the OS temp dir. Distinct from `save_directory` because
/// ~/Pictures is the wrong place for video clips on every desktop env.
fn recordings_directory() -> PathBuf {
    if let Some(d) = directories::UserDirs::new()
        .and_then(|u| u.video_dir().map(|p| p.to_path_buf())) {
        return d;
    }
    if let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()) {
        return home;
    }
    std::env::temp_dir()
}

fn describe_hotkey(s: &AppSettings) -> String {
    let vk = s.hotkey_virtual_key;
    let mut parts = Vec::new();
    if s.hotkey_modifiers & 0x0002 != 0 { parts.push("Ctrl"); }
    if s.hotkey_modifiers & 0x0004 != 0 { parts.push("Shift"); }
    if s.hotkey_modifiers & 0x0001 != 0 { parts.push("Alt"); }
    if s.hotkey_modifiers & 0x0008 != 0 { parts.push("Win"); }
    let key = vk_name(vk);
    if parts.is_empty() { key.into() } else { format!("{} + {}", parts.join(" + "), key) }
}

fn vk_name(vk: u32) -> &'static str {
    match vk {
        0x2C => "PrintScreen",
        0x70 => "F1",  0x71 => "F2",  0x72 => "F3",
        0x73 => "F4",  0x74 => "F5",  0x75 => "F6",
        0x76 => "F7",  0x77 => "F8",  0x78 => "F9",
        0x79 => "F10", 0x7A => "F11", 0x7B => "F12",
        _    => "(custom)",
    }
}

fn tray_tooltip(s: &AppSettings) -> String {
    format!("Kashot — press {} to capture", describe_hotkey(s))
}

/// Open `url` in the user's default browser. Best-effort — failures are
/// logged but the dialog already gave them the URL as plain text, so they
/// can copy it manually.
fn open_url(url: &str) {
    use std::process::Command;
    let opener = if cfg!(target_os = "windows") {
        ("cmd",  vec!["/C", "start", "", url])
    } else if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else {
        ("xdg-open", vec![url])
    };
    let mut cmd = Command::new(opener.0);
    cmd.args(opener.1);
    if let Err(e) = cmd.spawn() {
        eprintln!("Couldn't open {url}: {e}");
    }
}
