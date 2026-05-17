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

use crate::about_form::{AboutOutcome, AboutView};
use crate::convert_image_form::{ConvertImageOutcome, ConvertImageView};
use crate::convert_video_form::{ConvertVideoOutcome, ConvertVideoView};
use crate::editor::{Overlay, OverlayOutcome};
use crate::pin::PinView;
use crate::recording_indicator::RecordingIndicator;
use crate::settings_form::{SettingsOutcome, SettingsView};
use crate::updates_form::{UpdatesOutcome, UpdatesView};
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

    eprintln!("KAShot is running. Press {} or use the tray menu to capture.",
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
        /// Settings dialog window, present while the user is editing
        /// preferences. Routed by `WindowId` like the overlay + pinned
        /// windows. `None` when no settings dialog is open.
        settings_view: Option<SettingsView>,
        /// Floating recording-control window that appears while a screen
        /// recording is live. Lets the user stop the recording with one
        /// click instead of digging through the tray menu.
        recording_view: Option<RecordingIndicator>,
        /// Themed About modal — replaces the prior rfd MessageDialog.
        about_view:    Option<AboutView>,
        /// Themed Check-for-updates modal — replaces the prior open-url.
        updates_view:  Option<UpdatesView>,
        /// Themed Convert-image dialog.
        convert_image_view: Option<ConvertImageView>,
        /// Themed Convert-video dialog (shells out to bundled ffmpeg).
        convert_video_view: Option<ConvertVideoView>,
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
                    TrayEvent::StartRecording(opts)  => self.start_recording(opts, loop_target),
                    TrayEvent::StopRecording         => self.stop_recording(),
                    TrayEvent::OpenSaveFolder        => self.open_save_folder(),
                    TrayEvent::OpenRecordingsFolder  => self.open_recordings_folder(),
                    TrayEvent::Settings              => self.show_settings(loop_target),
                    TrayEvent::About                 => self.show_about(loop_target),
                    TrayEvent::CheckForUpdates       => self.show_updates(loop_target),
                    TrayEvent::ConvertImage          => self.show_convert_image(loop_target),
                    TrayEvent::ConvertVideo          => self.show_convert_video(loop_target),
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
            // Keep the recording indicator's flashing dot animating even
            // when there's no mouse activity over its window.
            if let Some(v) = &self.recording_view { v.tick(); }
            // Pump the updates dialog so its background-fetch result lands
            // and the "checking…" dots keep cycling.
            if let Some(v) = self.updates_view.as_mut() { v.tick(); }
            // Drive the convert-video dialog so its background ffmpeg
            // result lands and the "encoding…" dots keep moving.
            if let Some(v) = self.convert_video_view.as_mut() { v.tick(); }
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
                Ok(shot) => match Overlay::new(loop_target, shot.bitmap, self.settings.clone()) {
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
        /// Shows a desktop notification AND spawns a floating control window
        /// with a flashing red dot + timer + STOP button so the user has a
        /// one-click stop without hunting through the tray menu.
        fn start_recording(&mut self,
                           opts: kashot_platform::recorder::RecordingOptions,
                           loop_target: &ActiveEventLoop) {
            if self.recorder.is_recording() {
                eprintln!("Already recording.");
                return;
            }
            let dir   = recordings_directory_for(&self.settings);
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
                    // Float a small flashing control panel so the user can
                    // stop the recording without opening the tray menu.
                    // Best-effort — log + carry on if the OS won't give us
                    // another window.
                    match RecordingIndicator::new(loop_target) {
                        Ok(v)  => self.recording_view = Some(v),
                        Err(e) => eprintln!("Recording indicator failed: {e}"),
                    }
                    notify("KAShot — recording started",
                        &format!("{audio_label}\nSaving to {}\n\nClick the floating STOP button or use the tray menu to finish.",
                            out.display()),
                        true);
                }
                Err(e) => {
                    eprintln!("Recording failed to start: {e}");
                    rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Error)
                        .set_title("KAShot — recording failed")
                        .set_description(format!("{e}"))
                        .show();
                }
            }
        }

        /// Stop the active recording, finalize the file, and tear down the
        /// floating indicator if one is present.
        fn stop_recording(&mut self) {
            match self.recorder.stop() {
                Ok(path) => {
                    eprintln!("Saved recording {}", path.display());
                    if let Some(t) = &self.tray { t.set_recording(false); }
                    notify("KAShot — recording saved",
                        &format!("{}", path.display()),
                        false);
                }
                Err(e) => eprintln!("Stop recording failed: {e}"),
            }
            self.recording_view = None;
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
            let dir = recordings_directory_for(&self.settings);
            std::fs::create_dir_all(&dir).ok();
            open_url(&dir.to_string_lossy());
        }

        /// Open the native Settings dialog. Replaces the prior
        /// rfd-MessageDialog kludge with a real custom view that supports
        /// path pickers, a toggle pill, an editable watermark text field, and
        /// cycle controls for position / opacity / theme. The view is
        /// constructed lazily so the GTK/Wayland surface only spins up when
        /// the user actually clicks "Settings…". A second click while the
        /// dialog is open just raises focus to the existing window.
        fn show_settings(&mut self, loop_target: &ActiveEventLoop) {
            if self.settings_view.is_some() { return; }
            // Unregister the global hotkey while Settings is open so the
            // rebind capture widget can receive the user's keystrokes
            // (including the current PrintScreen) without triggering a
            // capture. The hotkey is re-registered when the dialog closes,
            // whether the user Saves or Cancels.
            if let Some(hk) = self.hotkeys.as_mut() {
                hk.unregister();
            }
            match SettingsView::new(loop_target, self.settings.clone()) {
                Ok(view) => self.settings_view = Some(view),
                Err(e)   => {
                    eprintln!("Settings dialog failed to open: {e}");
                    // Window failed to open — re-register the hotkey so
                    // the user isn't stranded with capture disabled.
                    if let Some(hk) = self.hotkeys.as_mut() {
                        if let Err(e) = hk.register(self.settings.hotkey()) {
                            eprintln!("Re-register hotkey failed: {e}");
                        }
                    }
                }
            }
        }

        /// Called from the window event router after the settings dialog
        /// produces an outcome. `Saved` persists + re-registers the hotkey +
        /// refreshes the tooltip; `Cancelled` drops the draft; `OpenJson`
        /// opens settings.json and leaves the dialog open.
        fn apply_settings_outcome(&mut self, outcome: SettingsOutcome) -> bool {
            match outcome {
                SettingsOutcome::Saved(new) => {
                    self.settings = new;
                    if let Err(e) = self.settings.save() {
                        eprintln!("Failed to persist settings: {e}");
                    }
                    if let Some(hk) = self.hotkeys.as_mut() {
                        hk.unregister();
                        if let Err(e) = hk.register(self.settings.hotkey()) {
                            eprintln!("Re-register hotkey failed: {e}");
                        }
                    }
                    // Tray tooltip is set at startup; the tray-icon crate has
                    // no live mutator, so any new hotkey text only appears
                    // after a restart. Not worth a tray rebuild for the
                    // tooltip alone.
                    let _ = tray_tooltip;
                    true
                }
                SettingsOutcome::Cancelled => {
                    // No persistence, but `show_settings` unregistered the
                    // global hotkey on open — re-register the previous
                    // binding so the user can capture again.
                    if let Some(hk) = self.hotkeys.as_mut() {
                        if let Err(e) = hk.register(self.settings.hotkey()) {
                            eprintln!("Re-register hotkey failed: {e}");
                        }
                    }
                    true
                }
                SettingsOutcome::OpenJson => {
                    if let Some(p) = AppSettings::settings_path() {
                        // Ensure the file exists so the editor has something
                        // to open; create it from current draft if missing.
                        if !p.exists() {
                            let _ = self.settings.save();
                        }
                        open_url(&p.to_string_lossy());
                    }
                    false
                }
            }
        }

        /// Themed About modal — mirrors the brand chrome of Settings.
        /// Lazily constructed; a second tray-click while the dialog is
        /// open is a no-op.
        fn show_about(&mut self, loop_target: &ActiveEventLoop) {
            if self.about_view.is_some() { return; }
            match AboutView::new(loop_target) {
                Ok(v)  => self.about_view = Some(v),
                Err(e) => eprintln!("About dialog failed to open: {e}"),
            }
        }

        /// Themed Check-for-updates modal. Spins up a background curl on
        /// open and shows the installed + latest tag side-by-side once the
        /// fetch resolves. The "Open releases page" button always works
        /// even if the fetch fails.
        fn show_updates(&mut self, loop_target: &ActiveEventLoop) {
            if self.updates_view.is_some() { return; }
            match UpdatesView::new(loop_target) {
                Ok(v)  => self.updates_view = Some(v),
                Err(e) => eprintln!("Updates dialog failed to open: {e}"),
            }
        }

        /// Open the themed image-conversion dialog. Lazy-constructed; a
        /// second click while it's already open is a no-op so the user
        /// can't accidentally stack windows.
        fn show_convert_image(&mut self, loop_target: &ActiveEventLoop) {
            if self.convert_image_view.is_some() { return; }
            match ConvertImageView::new(loop_target) {
                Ok(v)  => self.convert_image_view = Some(v),
                Err(e) => eprintln!("Convert-image dialog failed to open: {e}"),
            }
        }

        fn show_convert_video(&mut self, loop_target: &ActiveEventLoop) {
            if self.convert_video_view.is_some() { return; }
            match ConvertVideoView::new(loop_target) {
                Ok(v)  => self.convert_video_view = Some(v),
                Err(e) => eprintln!("Convert-video dialog failed to open: {e}"),
            }
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
            enum Target { Overlay, Pinned(usize), Settings, Recording, About, Updates, ConvertImage, ConvertVideo, Unknown }
            let target = if self.overlay.as_ref().map(|ov| ov.window_id() == id).unwrap_or(false) {
                Target::Overlay
            } else if self.settings_view.as_ref().map(|s| s.window_id() == id).unwrap_or(false) {
                Target::Settings
            } else if self.recording_view.as_ref().map(|r| r.window_id() == id).unwrap_or(false) {
                Target::Recording
            } else if self.about_view.as_ref().map(|a| a.window_id() == id).unwrap_or(false) {
                Target::About
            } else if self.updates_view.as_ref().map(|u| u.window_id() == id).unwrap_or(false) {
                Target::Updates
            } else if self.convert_image_view.as_ref().map(|c| c.window_id() == id).unwrap_or(false) {
                Target::ConvertImage
            } else if self.convert_video_view.as_ref().map(|c| c.window_id() == id).unwrap_or(false) {
                Target::ConvertVideo
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
            let mut settings_outcome: Option<SettingsOutcome> = None;

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
                Target::Settings => {
                    if let Some(view) = self.settings_view.as_mut() {
                        view.handle_event(ev);
                        if let Some(out) = view.outcome.take() {
                            settings_outcome = Some(out);
                        }
                    }
                }
                Target::Recording => {
                    let mut stop_now = false;
                    if let Some(view) = self.recording_view.as_mut() {
                        view.handle_event(ev);
                        stop_now = view.stop_requested;
                    }
                    if stop_now { self.stop_recording(); }
                }
                Target::About => {
                    let mut outcome: Option<AboutOutcome> = None;
                    if let Some(view) = self.about_view.as_mut() {
                        view.handle_event(ev);
                        outcome = view.outcome.take();
                    }
                    if let Some(o) = outcome {
                        match o {
                            AboutOutcome::Closed => { self.about_view = None; }
                            AboutOutcome::OpenProject => open_url("https://kashot.org"),
                            AboutOutcome::OpenAuthor  => open_url("https://theaivibe.org/about"),
                            AboutOutcome::OpenUpdates => {
                                self.about_view = None;
                                self.show_updates(_loop_target);
                            }
                        }
                    }
                }
                Target::Updates => {
                    let mut outcome: Option<UpdatesOutcome> = None;
                    if let Some(view) = self.updates_view.as_mut() {
                        view.handle_event(ev);
                        outcome = view.outcome.take();
                    }
                    if let Some(o) = outcome {
                        match o {
                            UpdatesOutcome::Closed => { self.updates_view = None; }
                            UpdatesOutcome::OpenReleases => open_url("https://github.com/singhpratech/kashot/releases"),
                        }
                    }
                }
                Target::ConvertImage => {
                    let mut outcome: Option<ConvertImageOutcome> = None;
                    if let Some(view) = self.convert_image_view.as_mut() {
                        view.handle_event(ev);
                        outcome = view.outcome.take();
                    }
                    if matches!(outcome, Some(ConvertImageOutcome::Closed)) {
                        self.convert_image_view = None;
                    }
                }
                Target::ConvertVideo => {
                    let mut outcome: Option<ConvertVideoOutcome> = None;
                    if let Some(view) = self.convert_video_view.as_mut() {
                        view.handle_event(ev);
                        outcome = view.outcome.take();
                    }
                    if matches!(outcome, Some(ConvertVideoOutcome::Closed)) {
                        self.convert_video_view = None;
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

            if drop_overlay {
                // Pull back any per-tool slider values the editor mutated
                // (currently just `marker_opacity`) so the next capture
                // session opens with the same value. The editor already
                // persisted it to settings.json on mouseup; this keeps the
                // tray's in-memory copy aligned.
                if let Some(ov) = self.overlay.as_ref() {
                    self.settings.marker_opacity = ov.settings().marker_opacity;
                }
                self.overlay = None;
            }
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
            if let Some(out) = settings_outcome {
                let close = self.apply_settings_outcome(out);
                if close { self.settings_view = None; }
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
        settings_view:  None,
        recording_view: None,
        about_view:     None,
        updates_view:   None,
        convert_image_view: None,
        convert_video_view: None,
        last_tick: Instant::now(),
        capturing: false,
    };

    event_loop.run_app(&mut app).map_err(|e| anyhow!("run_app: {e}"))?;

    let _ = app.settings.save();
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Stamp the configured watermark text onto the saved frame at the user-
/// chosen anchor. No-op when `WatermarkEnabled` is false or the text is empty.
///
/// Opacity (`WatermarkOpacity`, 0..1) scales the shadow + highlight alphas so
/// the watermark can be visibly bold or barely-there. Position
/// (`WatermarkPosition`, TopLeft / TopRight / BottomLeft / BottomRight)
/// picks the anchor corner inside the bitmap.
fn apply_watermark(img: &mut image::ImageBuffer<image::Rgba<u8>, Vec<u8>>, settings: &AppSettings) {
    if !settings.watermark_enabled { return; }
    let text = settings.watermark_text.trim();
    if text.is_empty() { return; }
    use kashot_core::color::Rgba;
    use kashot_core::settings::WatermarkAnchor;
    let scale = 2;
    let text_w = crate::bitmap_font::measure(text, scale);
    let text_h = crate::bitmap_font::GLYPH_H * scale;
    let pad    = 8;
    let img_w  = img.width()  as i32;
    let img_h  = img.height() as i32;
    if text_w + pad * 2 > img_w || text_h + pad * 2 > img_h {
        return;
    }
    let opacity = settings.watermark_opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 { return; }
    let (x, y) = match WatermarkAnchor::parse(&settings.watermark_position) {
        WatermarkAnchor::TopLeft     => (pad,                 pad),
        WatermarkAnchor::TopRight    => (img_w - text_w - pad, pad),
        WatermarkAnchor::BottomLeft  => (pad,                 img_h - text_h - pad),
        WatermarkAnchor::BottomRight => (img_w - text_w - pad, img_h - text_h - pad),
    };
    let shadow_a    = (180.0 * opacity).round().clamp(0.0, 255.0) as u8;
    let highlight_a = (220.0 * opacity).round().clamp(0.0, 255.0) as u8;
    let mut surf = crate::painter::ImageSurface(img);
    crate::painter::draw_text(&mut surf, x + 1, y + 1, scale, text, Rgba::new(0, 0, 0, shadow_a));
    crate::painter::draw_text(&mut surf, x,     y,     scale, text, Rgba::new(255, 255, 255, highlight_a));
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

/// Where MP4 recordings land. Honors `settings.recordings_directory` when set
/// and the path is an existing dir; otherwise falls back to XDG Videos, then
/// $HOME, then the OS temp dir. Distinct from `save_directory` because
/// ~/Pictures is the wrong place for video clips on every desktop env.
fn recordings_directory_for(s: &AppSettings) -> PathBuf {
    if !s.recordings_directory.is_empty() {
        let p = PathBuf::from(&s.recordings_directory);
        if p.is_dir() { return p; }
    }
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
    format!("KAShot — press {} to capture", describe_hotkey(s))
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
            .args(["-a", "KAShot", "-t", timeout, title, body])
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
