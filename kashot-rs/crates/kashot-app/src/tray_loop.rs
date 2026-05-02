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
use crate::pin::PinView;
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
        /// Floating "Pin" windows the user has chosen to keep on screen.
        /// Each is its own borderless winit window; the tray app routes
        /// their `WindowEvent`s by `WindowId` and drops them when their
        /// handler signals close.
        pinned:     Vec<PinView>,
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
                    TrayEvent::CancelPending         => {} // handled inline by the delay loop
                    TrayEvent::StartRecording(opts)  => self.start_recording(opts),
                    TrayEvent::StopRecording         => self.stop_recording(),
                    TrayEvent::OpenSaveFolder        => self.open_save_folder(),
                    TrayEvent::OpenRecordingsFolder  => self.open_recordings_folder(),
                    TrayEvent::Settings              => self.show_settings(),
                    TrayEvent::About                 => self.show_about(),
                    TrayEvent::CheckForUpdates       => open_url("https://github.com/singhpratech/kashot/releases"),
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
                    self.capture(loop_target);
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
                if let Some(t) = &self.tray { t.set_pending(true); }
                let until = Instant::now() + user_delay;
                let mut cancelled = false;
                while Instant::now() < until {
                    if let Some(t) = &self.tray {
                        t.pump_events();
                        // Drain pending tray events — if the user clicked
                        // "Cancel pending capture", abort the countdown.
                        if let TrayEvent::CancelPending = t.try_recv() {
                            eprintln!("Cancel pending capture — aborting countdown.");
                            cancelled = true;
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                if let Some(t) = &self.tray { t.set_pending(false); }
                if cancelled { self.capturing = false; return; }
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
        /// the overlay editor) to the configured save directory. Applies
        /// the watermark first if `WatermarkEnabled` is set in settings.
        fn save_final(&mut self, mut img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) {
            apply_watermark(&mut img, &self.settings);
            match save_capture(&mut self.settings, &img) {
                Ok(path) => eprintln!("Saved {}", path.display()),
                Err(e)   => eprintln!("Save failed: {e}"),
            }
        }

        /// Push the final cropped image into the system clipboard. Uses
        /// arboard, which speaks the right protocol on every platform (X11
        /// selection on Linux, NSPasteboard on macOS, OpenClipboard on
        /// Windows). On Linux arboard runs a background thread to keep the
        /// selection alive while the source process exits — that's fine
        /// because Kashot stays resident. Watermark is applied first so the
        /// pasted bitmap matches what `save_final` writes to disk.
        fn copy_final(&mut self, mut img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) {
            apply_watermark(&mut img, &self.settings);
            let (w, h) = (img.width() as usize, img.height() as usize);
            let bytes  = img.into_raw();
            match arboard::Clipboard::new() {
                Ok(mut clip) => {
                    let data = arboard::ImageData {
                        width:  w,
                        height: h,
                        bytes:  std::borrow::Cow::Owned(bytes),
                    };
                    if let Err(e) = clip.set_image(data) {
                        eprintln!("Clipboard copy failed: {e}");
                    } else {
                        eprintln!("Copied {w}×{h} to clipboard");
                    }
                }
                Err(e) => eprintln!("Clipboard unavailable: {e}"),
            }
        }

        /// Start recording the primary display. Output lands in the user's
        /// Videos directory (or a fallback) as `kashot_<timestamp>.mp4`.
        /// Shows a desktop notification so the user knows recording is live
        /// (the tray menu's "Stop Recording" item is the canonical control,
        /// but the notification also reminds where to click).
        fn start_recording(&mut self, opts: kashot_platform::recorder::RecordingOptions) {
            if self.recorder.is_recording() {
                eprintln!("Already recording.");
                return;
            }
            let dir   = recordings_directory();
            let stamp = Local::now().format("%Y%m%d_%H%M%S");
            let out   = dir.join(format!("kashot_{stamp}.mp4"));
            let audio_label = match (opts.mic, opts.system_audio) {
                (false, false) => "video only",
                (true,  false) => "with microphone",
                (false, true)  => "with system audio",
                (true,  true)  => "with mic + system audio",
            };
            match self.recorder.start(out.clone(), opts) {
                Ok(()) => {
                    eprintln!("Recording ({audio_label}) → {}", out.display());
                    if let Some(t) = &self.tray { t.set_recording(true); }
                    notify("Kashot — recording started",
                        &format!("{audio_label}\nSaving to {}\n\nClick the tray icon → \"Stop Recording\" to finish.",
                            out.display()),
                        true);
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
                    notify("Kashot — recording saved",
                        &format!("{}", path.display()),
                        false);
                }
                Err(e) => eprintln!("Stop recording failed: {e}"),
            }
        }

        /// Open the configured screenshot save directory in the user's
        /// default file manager (xdg-open / open / Explorer). Mirrors C#
        /// `TrayContext.OpenSaveFolder` so the tray menu item lands the
        /// user where their screenshots actually live.
        fn open_save_folder(&self) {
            let dir = save_directory(&self.settings);
            open_url(&dir.to_string_lossy());
        }

        fn open_recordings_folder(&self) {
            let dir = recordings_directory();
            std::fs::create_dir_all(&dir).ok();
            open_url(&dir.to_string_lossy());
        }

        /// Settings entry point. The full Windows `SettingsForm` has fields
        /// for hotkey / save folder / start-with-OS / theme / watermark — a
        /// proper custom dialog is queued behind the iced UI port. Until
        /// then this opens the on-disk `settings.json` in the user's default
        /// editor so every option is editable, *and* offers the save-folder
        /// picker as the most-used quick path. Three-way YesNoCancel:
        ///   Yes    → open settings.json in default editor (covers all keys)
        ///   No     → quick-pick a new save folder via file dialog
        ///   Cancel → no-op
        fn show_settings(&mut self) {
            let cfg_path = AppSettings::settings_path().unwrap_or_else(|| std::path::PathBuf::from("settings.json"));
            let save_dir = save_directory(&self.settings);
            let rec_dir  = recordings_directory();
            let watermark = if self.settings.watermark_enabled {
                format!("ON (\"{}\")", self.settings.watermark_text)
            } else { "OFF".to_owned() };
            let theme = if self.settings.theme.is_empty() {
                "Light".to_owned()
            } else { self.settings.theme.clone() };
            let res = rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Info)
                .set_title("Kashot — Settings")
                .set_description(format!(
                    "Capture hotkey:   {}\n\
                     Watermark:        {}\n\
                     Theme:            {}\n\
                     Palette:          {}\n\n\
                     ── Paths ──────────────────────────────\n\
                     Settings file:    {}\n\
                     Screenshots:      {}\n\
                     Recordings:       {}\n\n\
                     Yes:    Open settings.json in default editor\n         \
                             (every option editable: hotkey, theme, watermark, …)\n\n\
                     No:     Change screenshot save folder\n\n\
                     Cancel: Close",
                    describe_hotkey(&self.settings),
                    watermark,
                    theme,
                    self.settings.palette_index,
                    cfg_path.display(),
                    save_dir.display(),
                    rec_dir.display(),
                ))
                .set_buttons(rfd::MessageButtons::YesNoCancel)
                .show();
            match res {
                rfd::MessageDialogResult::Yes => {
                    open_url(&cfg_path.to_string_lossy());
                }
                rfd::MessageDialogResult::No => {
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
                _ => {}
            }
        }

        /// Native About modal — mirrors `Kashot/AboutForm.cs` text 1:1:
        /// name, version, "With love from PrateekSingh ❤", copyright, link
        /// to the project, and a Yes/No to jump to the releases page.
        fn show_about(&self) {
            let year = chrono::Local::now().format("%Y");
            // Pure info dialog — single Ok button. The "Check for updates"
            // tray item is the canonical way to reach the releases page,
            // so About stays clean instead of asking a Yes/No question.
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Info)
                .set_title("About Kashot")
                .set_description(format!(
                    "Kashot v{}\n\
                     The lightweight screenshot tool.\n\n\
                     With love from PrateekSingh ❤\n\
                     © {} PrateekSingh. All rights reserved.\n\n\
                     github.com/singhpratech/kashot · kashot.org · MIT",
                    env!("CARGO_PKG_VERSION"),
                    year
                ))
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
        }
    }

    impl ApplicationHandler for TrayApp {
        fn resumed(&mut self, _: &ActiveEventLoop) {}

        fn window_event(&mut self, _loop_target: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
            // Route the event into the overlay editor if it owns the window.
            // Anything else (close-requested on a phantom window, stray events
            // from a destroyed surface) is silently dropped.
            // Decide the routing target *before* consuming the event so we
            // only dispatch once. Overlay wins if it owns the id; otherwise
            // search the pinned-window list.
            enum Target { Overlay, Pinned(usize), Unknown }
            let target = if self.overlay.as_ref().map(|ov| ov.window_id() == id).unwrap_or(false) {
                Target::Overlay
            } else if let Some((i, _)) = self.pinned.iter().enumerate().find(|(_, p)| p.window_id() == id) {
                Target::Pinned(i)
            } else {
                Target::Unknown
            };

            let mut drop_overlay = false;
            let mut accepted:     Option<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>> = None;
            let mut copied:       Option<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>> = None;
            let mut pinned_payload: Option<(image::ImageBuffer<image::Rgba<u8>, Vec<u8>>, (i32, i32))> = None;
            let mut drop_pin: Option<usize> = None;

            match target {
                Target::Overlay => {
                    if let Some(ov) = self.overlay.as_mut() {
                        match ov.handle_event(ev) {
                            OverlayOutcome::Continue        => {}
                            OverlayOutcome::Cancelled       => { drop_overlay = true; }
                            OverlayOutcome::Accepted(img)   => { drop_overlay = true; accepted = Some(img); }
                            OverlayOutcome::Copied(img)     => { drop_overlay = true; copied   = Some(img); }
                            OverlayOutcome::Pinned(img, p)  => { drop_overlay = true; pinned_payload = Some((img, p)); }
                        }
                    }
                }
                Target::Pinned(i) => {
                    if let Some(p) = self.pinned.get_mut(i) {
                        if p.handle_event(ev) {
                            drop_pin = Some(i);
                        }
                    }
                }
                Target::Unknown => {}
            }

            if drop_overlay { self.overlay = None; }
            if let Some(i) = drop_pin { self.pinned.swap_remove(i); }
            if let Some(img) = accepted { self.save_final(img); }
            if let Some(img) = copied   { self.copy_final(img); }
            if let Some((mut img, pos)) = pinned_payload {
                apply_watermark(&mut img, &self.settings);
                match PinView::new(_loop_target, img, pos) {
                    Ok(pv)  => self.pinned.push(pv),
                    Err(e)  => eprintln!("Pin failed: {e}"),
                }
            }
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
        pinned:  Vec::new(),
        last_tick: Instant::now(),
        capturing: false,
    };

    event_loop.run_app(&mut app).map_err(|e| anyhow!("run_app: {e}"))?;

    let _ = app.settings.save();
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Stamp the configured watermark text onto the bottom-right of the final
/// bitmap, mirroring the C# `OverlayForm.GetFinalImage` watermark pass. No-op
/// when `WatermarkEnabled` is false or the text is empty.
///
/// Uses the in-tree 5×7 bitmap font through `painter::draw_text`, which
/// alpha-blends so the watermark sits on top of whatever pixels the user
/// captured. The text is painted twice — once in semi-opaque black (offset
/// by 1 px shadow) and once in white — so it stays legible on both light
/// and dark screenshots.
fn apply_watermark(img: &mut image::ImageBuffer<image::Rgba<u8>, Vec<u8>>, settings: &AppSettings) {
    if !settings.watermark_enabled { return; }
    let text = settings.watermark_text.trim();
    if text.is_empty() { return; }
    use kashot_core::color::Rgba;
    let scale = 2;
    let text_w = crate::bitmap_font::measure(text, scale);
    let text_h = crate::bitmap_font::GLYPH_H * scale;
    let pad    = 8;
    let img_w  = img.width()  as i32;
    let img_h  = img.height() as i32;
    if text_w + pad * 2 > img_w || text_h + pad * 2 > img_h {
        // Watermark bigger than the saved frame — drop it rather than mangle.
        return;
    }
    let x = img_w - text_w - pad;
    let y = img_h - text_h - pad;
    let mut surf = crate::painter::ImageSurface(img);
    // Drop shadow.
    crate::painter::draw_text(&mut surf, x + 1, y + 1, scale, text, Rgba::new(0, 0, 0, 180));
    // Highlight.
    crate::painter::draw_text(&mut surf, x,     y,     scale, text, Rgba::new(255, 255, 255, 220));
}

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
/// Send a desktop notification via `notify-send` on Linux, `osascript` on
/// macOS, `powershell BurntToast` on Windows. Best-effort — silent failure
/// if the binary isn't available. `urgent=true` keeps the toast on screen
/// 5 s instead of 3 s.
fn notify(title: &str, body: &str, urgent: bool) {
    let timeout = if urgent { "5000" } else { "3000" };
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args(["-a", "Kashot", "-t", timeout, title, body])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            body.replace('"', "\\\""),
            title.replace('"', "\\\""),
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        return;
    }
    #[cfg(target_os = "windows")]
    {
        // PowerShell toast — no extra dep on the user side.
        let script = format!(
            "[Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime] | Out-Null; \
             [Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom.XmlDocument,ContentType=WindowsRuntime] | Out-Null; \
             $template = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
             $template.GetElementsByTagName('text').Item(0).InnerText = '{}'; \
             $template.GetElementsByTagName('text').Item(1).InnerText = '{}'; \
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Kashot').Show($template);",
            title.replace('\'', "''"),
            body.replace('\'', "''"),
        );
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let _ = timeout;
    }
}

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
