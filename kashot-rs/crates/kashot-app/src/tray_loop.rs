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
    tray::{Tray, TrayEvent},
};
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
                    TrayEvent::None     => {}
                    TrayEvent::Capture  => self.capture(),
                    TrayEvent::Settings => self.show_settings_placeholder(),
                    TrayEvent::About    => self.show_about_placeholder(),
                    TrayEvent::Exit     => loop_target.exit(),
                }
            }
            if let Some(hk) = &self.hotkeys {
                if hk.drain_pressed() {
                    self.capture();
                }
            }
        }

        fn capture(&mut self) {
            if self.capturing { return; }
            self.capturing = true;

            // Brief delay so the tray menu / flyout can dismiss before we shoot.
            std::thread::sleep(Duration::from_millis(250));

            match capture_all_screens() {
                Ok(shot) => match save_capture(&mut self.settings, &shot.bitmap) {
                    Ok(path) => eprintln!("Saved {}", path.display()),
                    Err(e)   => eprintln!("Save failed: {e}"),
                },
                Err(e) => eprintln!("Capture failed: {e}"),
            }

            self.capturing = false;
        }

        fn show_settings_placeholder(&self) {
            eprintln!("Settings dialog: pending overlay-editor port (PLAN.md § R7).");
        }
        fn show_about_placeholder(&self) {
            eprintln!("Kashot v{} — github.com/singhpratech/kashot",
                env!("CARGO_PKG_VERSION"));
        }
    }

    impl ApplicationHandler for TrayApp {
        fn resumed(&mut self, _: &ActiveEventLoop) {}
        fn window_event(&mut self, _: &ActiveEventLoop, _id: WindowId, _ev: WindowEvent) {}

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
