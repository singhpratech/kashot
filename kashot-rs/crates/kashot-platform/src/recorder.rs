//! Screen recording via system tools.
//!
//! Cross-platform recording isn't a one-binary affair — every OS has its own
//! capture stack and the high-quality choices live outside the Rust ecosystem.
//! We deliberately route every platform through `ffmpeg` (or the platform's
//! built-in equivalent on macOS) so the **output container is identical
//! everywhere**: MP4, H.264 video, AAC stereo audio. Downstream tools
//! (`convert_video_form`, the pin preview, anything users do after the fact)
//! only ever need to know one format.
//!
//! Per-platform capture stack:
//!
//! * Linux  (X11)     : `ffmpeg -f x11grab` — needs `ffmpeg` installed.
//!                      Audio: PulseAudio mic + monitor source.
//! * Linux  (Wayland) : not supported here yet — proper screen capture on
//!                      Wayland goes through `xdg-desktop-portal` (PipeWire),
//!                      which is a substantial integration and queued
//!                      separately. `start()` detects a Wayland session
//!                      up-front (via `XDG_SESSION_TYPE` / `WAYLAND_DISPLAY`)
//!                      and returns a clear error rather than spawning ffmpeg
//!                      into a black `-f x11grab` capture that XWayland
//!                      silently produces. TODO(v0.3): wire `ashpd` /
//!                      xdg-desktop-portal.
//! * Windows          : `ffmpeg -f gdigrab` for video + `-f dshow` for the
//!                      default DirectShow microphone. See approach
//!                      discussion in the PR — gdigrab is CPU-heavy and does
//!                      not pick up DWM-composited surfaces as cleanly as
//!                      `Windows.Graphics.Capture` would, but it's the
//!                      smallest possible delta from the existing Linux
//!                      pattern and ships a working recorder in v0.2.1
//!                      without pulling in the `windows` crate, Media
//!                      Foundation FFI, or a hand-rolled H.264 encoder.
//!                      TODO(v0.3): port to `Windows.Graphics.Capture` +
//!                      MediaFoundation for per-window + per-monitor capture
//!                      and hardware-accelerated encoding.
//!                      TODO(v0.3): wire WASAPI loopback for system audio —
//!                      DirectShow has no general loopback device, and
//!                      "Stereo Mix" is disabled by default on modern
//!                      Windows installs, so on this PR Windows ships
//!                      mic-only audio (`system_audio: true` is silently
//!                      treated the same as mic-only when no loopback is
//!                      available).
//! * macOS            : video-only recordings use the built-in
//!                      `screencapture -v` (no ffmpeg dependency). Any audio
//!                      request routes through `ffmpeg -f avfoundation`
//!                      instead — the only way to fold an audio device into
//!                      the capture. Mic works directly; system audio needs a
//!                      loopback device (BlackHole / Aggregate), mirroring the
//!                      Windows "Stereo Mix" situation, otherwise it degrades
//!                      to mic or surfaces an actionable error.
//!
//! Stop is graceful per platform: write `q` to `ffmpeg`'s stdin (Linux,
//! Windows) or send SIGINT to `screencapture` (macOS) so the MP4 moov atom
//! is finalized. `Drop` polls `try_wait` for up to ~2 s after the graceful
//! signal — only if the child is still alive at that point do we fall back
//! to SIGKILL, so the file is playable on every normal teardown. ffmpeg
//! treats `q` on stdin the same on Windows as it does on Linux, so the
//! `recording_indicator` STOP button needs no platform-specific tweaks.

use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// What audio sources to mix into the recording. Mirrors the C#
/// `KashotRecorder.Start(path, micEnabled, systemAudioEnabled)` triple.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecordingOptions {
    pub mic:          bool,
    pub system_audio: bool,
}

impl RecordingOptions {
    pub const NONE:        Self = Self { mic: false, system_audio: false };
    pub const MIC_ONLY:    Self = Self { mic: true,  system_audio: false };
    pub const SYSTEM_ONLY: Self = Self { mic: false, system_audio: true  };
    pub const MIC_AND_SYS: Self = Self { mic: true,  system_audio: true  };
    pub fn has_audio(self) -> bool { self.mic || self.system_audio }
}

pub struct Recorder {
    child:  Option<Child>,
    output: Option<PathBuf>,
}

impl Recorder {
    pub fn new() -> Self {
        Self { child: None, output: None }
    }

    pub fn is_recording(&self) -> bool { self.child.is_some() }
    pub fn output_path(&self) -> Option<&Path> { self.output.as_deref() }

    /// Begin recording the primary display to `output`. Errors if a recording
    /// is already in progress, if the parent directory can't be created, or
    /// if the platform's recording tool isn't available.
    pub fn start(&mut self, output: PathBuf, options: RecordingOptions) -> Result<()> {
        if self.is_recording() {
            return Err(Error::Recording("a recording is already in progress".into()));
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let child = spawn_recorder(&output, options)?;
        self.child  = Some(child);
        self.output = Some(output);
        Ok(())
    }

    /// Stop the active recording. Returns the output file path on success.
    /// The OS recorder needs a moment to flush the trailing frames + finalize
    /// the container — we wait on the child so the file is playable when
    /// this returns.
    pub fn stop(&mut self) -> Result<PathBuf> {
        let mut child = self.child.take()
            .ok_or_else(|| Error::Recording("not currently recording".into()))?;
        let path = self.output.take()
            .unwrap_or_else(PathBuf::new);

        graceful_signal(&mut child);
        let _ = child.wait();
        Ok(path)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            graceful_signal(&mut c);
            let mut exited = false;
            for _ in 0..20 {
                match c.try_wait() {
                    Ok(Some(_)) => { exited = true; break; }
                    Ok(None)    => std::thread::sleep(std::time::Duration::from_millis(100)),
                    Err(_)      => break,
                }
            }
            if !exited {
                let _ = c.kill();
            }
            let _ = c.wait();
        }
    }
}

// ── ffmpeg lookup (shared by Linux + Windows) ───────────────────────────────

/// Locate a usable `ffmpeg` binary. Mirrors `kashot-app`'s `locate_ffmpeg`
/// but lives in `kashot-platform` so the recorder doesn't pull a reverse
/// dependency on the app crate. Search order:
///
///   1. Next to our own executable (installer bundle layout).
///   2. macOS `.app/Contents/Resources/ffmpeg`.
///   3. `PATH`.
///
/// Returns `None` if no candidate is found — callers fall back to plain
/// `"ffmpeg"` so the existing "ffmpeg not found in PATH" error message still
/// surfaces from `Command::spawn`.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn locate_ffmpeg() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let bundle_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let next_to_us = dir.join(bundle_name);
    if next_to_us.is_file() { return Some(next_to_us); }

    if cfg!(target_os = "macos") {
        if let Some(contents) = dir.parent() {
            let mac_resources = contents.join("Resources").join("ffmpeg");
            if mac_resources.is_file() { return Some(mac_resources); }
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ";" } else { ":" };
        for p in path_var.split(sep) {
            let candidate = std::path::Path::new(p).join(bundle_name);
            if candidate.is_file() { return Some(candidate); }
        }
    }
    None
}

// ── platform spawn / signal ─────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn spawn_recorder(output: &Path, options: RecordingOptions) -> Result<Child> {
    // Reject Wayland up-front — `-f x11grab` against XWayland silently
    // captures only XWayland clients (typically a black frame), and on
    // Wayland-only sessions DISPLAY may be unset entirely.
    let wayland_typed = std::env::var("XDG_SESSION_TYPE")
        .map(|s| s.eq_ignore_ascii_case("wayland"))
        .unwrap_or(false);
    let wayland_socket = std::env::var("WAYLAND_DISPLAY")
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if wayland_typed || wayland_socket {
        return Err(Error::Recording(
            "screen recording on Wayland isn't wired up yet \
             (xdg-desktop-portal / PipeWire path is planned — see PLAN.md R10). \
             To record now, log into an X11 / Xorg session from your display manager.".into()
        ));
    }
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into());
    let path = output.to_str().ok_or_else(||
        Error::Recording("non-UTF-8 output path".into()))?;

    // Pulse must be reachable for either audio source to work — `pactl info`
    // returns 0 when a server is up. If it isn't reachable we silently drop
    // back to video-only so headless / no-audio boxes still record cleanly.
    let pulse_ok = Command::new("pactl")
        .arg("info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let opt = if pulse_ok { options } else { RecordingOptions::NONE };

    // System-audio source is the default sink's monitor (`<sink>.monitor`).
    let monitor_source: Option<String> = if opt.system_audio {
        Command::new("pactl")
            .arg("get-default-sink")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| format!("{}.monitor", s.trim()))
    } else { None };

    let args = build_linux_ffmpeg_args(&display, path, opt, monitor_source.as_deref());
    let ffmpeg = locate_ffmpeg().unwrap_or_else(|| PathBuf::from("ffmpeg"));
    let res = Command::new(&ffmpeg)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match res {
        Ok(c) => Ok(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::Recording(
            "ffmpeg not found in PATH — install with: sudo apt install ffmpeg".into()
        )),
        Err(e) => Err(Error::Recording(format!("failed to spawn ffmpeg: {e}"))),
    }
}

/// Build the ffmpeg argv for Linux X11 capture. Pure function so the test
/// suite can assert exact argv composition without spawning a process.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn build_linux_ffmpeg_args(
    display:        &str,
    output_path:    &str,
    options:        RecordingOptions,
    monitor_source: Option<&str>,
) -> Vec<String> {
    let mut a: Vec<String> = Vec::with_capacity(32);
    let push = |a: &mut Vec<String>, s: &str| a.push(s.to_string());
    push(&mut a, "-y");
    push(&mut a, "-f"); push(&mut a, "x11grab");
    push(&mut a, "-framerate"); push(&mut a, "30");
    push(&mut a, "-i"); push(&mut a, display);
    if options.mic {
        push(&mut a, "-f"); push(&mut a, "pulse");
        push(&mut a, "-i"); push(&mut a, "default");
    }
    if let Some(m) = monitor_source {
        push(&mut a, "-f"); push(&mut a, "pulse");
        push(&mut a, "-i"); push(&mut a, m);
    }
    push(&mut a, "-c:v"); push(&mut a, "libx264");
    push(&mut a, "-preset"); push(&mut a, "ultrafast");
    push(&mut a, "-pix_fmt"); push(&mut a, "yuv420p");
    push(&mut a, "-vf"); push(&mut a, "pad=ceil(iw/2)*2:ceil(ih/2)*2");
    match (options.mic, monitor_source.is_some()) {
        (true, true) => {
            push(&mut a, "-filter_complex");
            push(&mut a, "[1:a][2:a]amix=inputs=2:duration=longest:dropout_transition=0[aout]");
            push(&mut a, "-map"); push(&mut a, "0:v");
            push(&mut a, "-map"); push(&mut a, "[aout]");
            push(&mut a, "-c:a"); push(&mut a, "aac");
            push(&mut a, "-b:a"); push(&mut a, "160k");
        }
        (true, false) | (false, true) => {
            push(&mut a, "-c:a"); push(&mut a, "aac");
            push(&mut a, "-b:a"); push(&mut a, "160k");
        }
        (false, false) => {}
    }
    push(&mut a, output_path);
    a
}

// ── macOS: screencapture (video-only) / ffmpeg -f avfoundation (with audio) ──
//
// `screencapture -v` has no audio control at all, so any mic / system-audio
// request routes through ffmpeg's avfoundation input instead — the only way
// to pull an audio device into the recording. Video-only recordings keep
// using `screencapture` so the common case needs no ffmpeg on the box and
// can't regress. System audio on macOS, like Stereo Mix on Windows, needs a
// virtual loopback device (BlackHole / Soundflower / an Aggregate device);
// without one we degrade to mic or surface an actionable error rather than
// silently shipping a muted track.
#[cfg(target_os = "macos")]
fn spawn_recorder(output: &Path, options: RecordingOptions) -> Result<Child> {
    let path = output.to_str().ok_or_else(||
        Error::Recording("non-UTF-8 output path".into()))?;

    // Video-only: keep the dependency-free built-in. stdin stays null, which
    // is how `graceful_signal` tells the two backends apart.
    if !options.has_audio() {
        return Command::new("screencapture")
            .args(["-v", path])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Recording(format!("failed to spawn screencapture: {e}")));
    }

    // Audio requested → ffmpeg avfoundation.
    let ffmpeg = locate_ffmpeg().ok_or_else(|| Error::Recording(
        "recording audio on macOS needs ffmpeg, which wasn't found next to \
         Kashot or on your PATH. Install it with: brew install ffmpeg — then \
         retry. (Video-only recording works without ffmpeg.)".into()
    ))?;

    let listing = list_avfoundation_devices(&ffmpeg);
    let (video_devs, audio_devs) = parse_avfoundation_devices(&listing);
    let screen_idx = pick_macos_screen_index(&video_devs)?;
    let audio_idx  = pick_macos_audio_device(&audio_devs, options)?;

    let args = build_macos_ffmpeg_args(screen_idx, audio_idx, path, options);
    let res = Command::new(&ffmpeg)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match res {
        Ok(c) => Ok(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::Recording(
            "ffmpeg not found — install it with: brew install ffmpeg".into()
        )),
        Err(e) => Err(Error::Recording(format!("failed to spawn ffmpeg: {e}"))),
    }
}

/// Build the ffmpeg argv for macOS avfoundation capture. avfoundation takes a
/// single `-i "<video>:<audio>"` input that fuses one video and one audio
/// device, so unlike Linux there's no second `-i` / amix — `audio_idx` is the
/// one source already chosen by `pick_macos_audio_device`. Pure function so
/// the suite can assert argv shape without a Mac or a real device.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn build_macos_ffmpeg_args(
    screen_idx: usize,
    audio_idx:  Option<usize>,
    output_path: &str,
    _options:    RecordingOptions,
) -> Vec<String> {
    let mut a: Vec<String> = Vec::with_capacity(20);
    let push = |a: &mut Vec<String>, s: &str| a.push(s.to_string());
    push(&mut a, "-y");
    push(&mut a, "-f"); push(&mut a, "avfoundation");
    push(&mut a, "-framerate"); push(&mut a, "30");
    // Input spec is one token: "<video>:<audio>"; empty audio half = no audio.
    let input = match audio_idx {
        Some(ai) => format!("{screen_idx}:{ai}"),
        None      => format!("{screen_idx}:"),
    };
    push(&mut a, "-i"); a.push(input);
    push(&mut a, "-c:v"); push(&mut a, "libx264");
    push(&mut a, "-preset"); push(&mut a, "ultrafast");
    push(&mut a, "-pix_fmt"); push(&mut a, "yuv420p");
    push(&mut a, "-vf"); push(&mut a, "pad=ceil(iw/2)*2:ceil(ih/2)*2");
    if audio_idx.is_some() {
        push(&mut a, "-c:a"); push(&mut a, "aac");
        push(&mut a, "-b:a"); push(&mut a, "160k");
        // Match the stereo AAC container the other platforms emit.
        push(&mut a, "-ac"); push(&mut a, "2");
    }
    push(&mut a, output_path);
    a
}

/// Run `ffmpeg -f avfoundation -list_devices true -i ""`. avfoundation writes
/// the device table to stderr and exits non-zero (it never actually opens a
/// stream) — that's expected, we only want the stderr text.
#[cfg(target_os = "macos")]
fn list_avfoundation_devices(ffmpeg: &Path) -> String {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stderr).into_owned(),
        Err(_) => String::new(),
    }
}

/// Parse the avfoundation `-list_devices` stderr into `(video, audio)` device
/// lists of `(index, name)`. Pure so it's unit-testable off a Mac. The table
/// looks like:
///   [AVFoundation indev @ ..] AVFoundation video devices:
///   [AVFoundation indev @ ..] [0] FaceTime HD Camera
///   [AVFoundation indev @ ..] [1] Capture screen 0
///   [AVFoundation indev @ ..] AVFoundation audio devices:
///   [AVFoundation indev @ ..] [0] MacBook Pro Microphone
///   [AVFoundation indev @ ..] [1] BlackHole 2ch
#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_avfoundation_devices(stderr: &str) -> (Vec<(usize, String)>, Vec<(usize, String)>) {
    #[derive(PartialEq)]
    enum Section { None, Video, Audio }
    let mut sect = Section::None;
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for line in stderr.lines() {
        let low = line.to_ascii_lowercase();
        if low.contains("video devices:") { sect = Section::Video; continue; }
        if low.contains("audio devices:") { sect = Section::Audio; continue; }
        // Pull "[N] Name" — N is the device index ffmpeg expects in the -i spec.
        let Some(open) = line.find('[') else { continue };
        // Skip the leading "[AVFoundation indev @ 0x..]" log prefix bracket(s);
        // the device-index bracket is the last "[<digits>]" on the line.
        let Some(idx_open) = line.rfind('[') else { continue };
        let _ = open;
        let Some(idx_close) = line[idx_open..].find(']').map(|p| idx_open + p) else { continue };
        let inner = &line[idx_open + 1..idx_close];
        let Ok(idx) = inner.trim().parse::<usize>() else { continue };
        let name = line[idx_close + 1..].trim().to_string();
        if name.is_empty() { continue; }
        match sect {
            Section::Video => video.push((idx, name)),
            Section::Audio => audio.push((idx, name)),
            Section::None  => {}
        }
    }
    (video, audio)
}

/// Choose the screen-capture video device index. avfoundation exposes the
/// display as a "Capture screen N" pseudo-camera; pick the first one. Errors
/// if none is present (e.g. Screen Recording permission not granted, which
/// makes the screen devices vanish from the listing).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn pick_macos_screen_index(video: &[(usize, String)]) -> Result<usize> {
    video.iter()
        .find(|(_, n)| n.to_ascii_lowercase().contains("capture screen"))
        .map(|(i, _)| *i)
        .ok_or_else(|| Error::Recording(
            "no screen-capture device found. Grant Kashot Screen Recording \
             permission in System Settings > Privacy & Security > Screen \
             Recording, then reopen Kashot and try again.".into()
        ))
}

/// Pick the avfoundation audio device given what the user asked for. Mirrors
/// `pick_windows_audio_device`: a single source per recording (avfoundation
/// fuses one audio device into the `-i` spec), system audio needs a loopback
/// device, and a system-audio-only request with no loopback is a hard error
/// rather than a silently muted file.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn pick_macos_audio_device(
    audio:   &[(usize, String)],
    options: RecordingOptions,
) -> Result<Option<usize>> {
    if !options.mic && !options.system_audio {
        return Ok(None);
    }
    // Loopback / virtual devices that carry system output back as an input.
    let is_loopback = |n: &str| {
        let l = n.to_ascii_lowercase();
        l.contains("blackhole") || l.contains("soundflower")
            || l.contains("loopback") || l.contains("aggregate")
            || l.contains("multi-output") || l.contains("ishowu")
    };
    if options.system_audio {
        if let Some((i, _)) = audio.iter().find(|(_, n)| is_loopback(n)) {
            return Ok(Some(*i));
        }
        // No loopback. If mic was also asked for, degrade to mic (best effort,
        // same as Windows). Otherwise it's a hard error.
        if !options.mic {
            return Err(Error::Recording(
                "system-audio recording on macOS needs a loopback device \
                 (e.g. BlackHole: brew install blackhole-2ch) or an Aggregate \
                 device routing your output back as an input. None was found. \
                 Install one, or choose 'Record + mic' to capture the \
                 microphone instead.".into()
            ));
        }
    }
    // Mic path: prefer something that looks like a microphone, else first.
    let mic = audio.iter()
        .find(|(_, n)| {
            let l = n.to_ascii_lowercase();
            l.contains("microphone") || l.contains("mic") || l.contains("built-in")
                || l.contains("macbook") || l.contains("headset")
        })
        .or_else(|| audio.first())
        .map(|(i, _)| *i);
    Ok(mic)
}

// ── Windows: ffmpeg -f gdigrab (+ -f dshow for mic) ─────────────────────────

#[cfg(target_os = "windows")]
fn spawn_recorder(output: &Path, options: RecordingOptions) -> Result<Child> {
    let path = output.to_str().ok_or_else(||
        Error::Recording("non-UTF-8 output path".into()))?;
    let ffmpeg = locate_ffmpeg().unwrap_or_else(|| PathBuf::from("ffmpeg.exe"));

    // Pick the right audio capture device(s) given what the user asked for.
    // Returns Err with an actionable message if `system_audio` was requested
    // but no loopback device (Stereo Mix / What U Hear / VoiceMeeter / etc.)
    // is available — so the toast doesn't lie about what's being recorded.
    let devices = list_dshow_audio_devices(&ffmpeg);
    let audio_dev: Option<String> = pick_windows_audio_device(&devices, options)?;

    // Pre-flight probe: catch the Windows-Privacy "microphone access denied"
    // case BEFORE we start a recording that would silently produce a muted
    // track. Skipped for video-only.
    if let Some(dev) = &audio_dev {
        probe_dshow_audio_device(&ffmpeg, dev)?;
    }

    let args = build_windows_ffmpeg_args(path, options, audio_dev.as_deref());
    let res = Command::new(&ffmpeg)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match res {
        Ok(c) => Ok(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::Recording(
            "ffmpeg.exe not found — the Kashot installer normally ships it \
             next to kashot.exe. Reinstall, or drop ffmpeg.exe into the same \
             folder as kashot.exe and retry.".into()
        )),
        Err(e) => Err(Error::Recording(format!("failed to spawn ffmpeg: {e}"))),
    }
}

/// Pick the right DirectShow audio device from the discovered list given
/// what the user asked for. Pure function — testable without a host that
/// actually has any of these devices installed.
///
/// Selection rules:
/// - `options.system_audio` (loopback): require one of the loopback-style
///   device names (Stereo Mix, What U Hear, Wave Out Mix, VoiceMeeter…).
///   If `mic` is *also* set we still return the loopback device; ffmpeg's
///   dshow input can only consume one audio source per `-i`, so picking
///   loopback is the closest thing to "both" until Phase-3 WASAPI lands.
/// - `options.mic` (mic only): prefer names that look like microphones
///   ("Microphone (…)", "Headset", "Array"), fall back to whatever's first.
/// - neither: returns `None` (video-only recording).
///
/// When `system_audio` was requested but no loopback device exists we
/// return Err with a Windows-specific actionable message. That bubbles up
/// to the tray-loop and the user sees a real dialog rather than a silent
/// muted MP4.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn pick_windows_audio_device(
    devices: &[String],
    options: RecordingOptions,
) -> Result<Option<String>> {
    if !options.mic && !options.system_audio {
        return Ok(None);
    }

    if options.system_audio {
        // Loopback-style devices vary by audio chipset and driver. The names
        // below cover the common shapes (English locale defaults) plus
        // VoiceMeeter, which is the most popular virtual audio cable on
        // Windows.
        const LOOPBACK_NEEDLES: &[&str] = &[
            "stereo mix", "what u hear", "what you hear", "wave out mix",
            "voicemeeter", "vb-audio", "virtual audio",
        ];
        let loopback = devices.iter().find(|d| {
            let n = d.to_lowercase();
            LOOPBACK_NEEDLES.iter().any(|needle| n.contains(needle))
        });
        if let Some(d) = loopback {
            return Ok(Some(d.clone()));
        }
        if !options.mic {
            return Err(Error::Recording(
                "System-audio capture needs a loopback device, but none is \
                 enabled. Right-click the Windows speaker icon → Sounds → \
                 Recording → right-click empty area → \"Show Disabled \
                 Devices\" → enable \"Stereo Mix\" — then retry. (No system \
                 audio was captured; nothing was recorded.)".into()
            ));
        }
        // mic=true && system_audio=true && no loopback: fall through to mic.
    }

    // Mic-only path (or system_audio fallback when both were requested).
    const MIC_NEEDLES: &[&str] = &[
        "microphone", "headset", "array mic", "internal mic",
    ];
    let mic = devices.iter().find(|d| {
        let n = d.to_lowercase();
        MIC_NEEDLES.iter().any(|needle| n.contains(needle))
    });
    match mic.or_else(|| devices.iter().next()) {
        Some(d) => Ok(Some(d.clone())),
        None => Err(Error::Recording(
            // Empty device list on Windows almost always means the OS
            // Privacy gate is blocking dshow enumeration entirely (no
            // device data leaks to apps without mic permission). The
            // "no microphone plugged in" case is extremely rare on
            // desktop / laptop hardware. Lead with the common fix.
            "No microphone detected by ffmpeg.\n\n\
             This is almost always Windows Privacy blocking microphone \
             access for desktop apps. Fix it:\n\n\
             1. Open Settings → Privacy & Security → Microphone\n\
             2. Turn ON \"Microphone access\"\n\
             3. Turn ON \"Let desktop apps access your microphone\"\n\
             4. Retry recording\n\n\
             If you genuinely have no mic plugged in, skip audio in \
             the tray menu's Record submenu instead.".into()
        )),
    }
}

/// Build the ffmpeg argv for Windows GDI capture (+ optional DirectShow mic).
/// Pure function so the test suite can assert exact argv composition without
/// spawning a process or having an actual mic plugged in.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn build_windows_ffmpeg_args(
    output_path: &str,
    options:     RecordingOptions,
    mic_device:  Option<&str>,
) -> Vec<String> {
    let mut a: Vec<String> = Vec::with_capacity(24);
    let push = |a: &mut Vec<String>, s: &str| a.push(s.to_string());
    push(&mut a, "-y");
    // Video: GDI grab of the whole desktop at 30 fps. `desktop` is gdigrab's
    // pseudo-device name for the full virtual screen.
    push(&mut a, "-f"); push(&mut a, "gdigrab");
    push(&mut a, "-framerate"); push(&mut a, "30");
    push(&mut a, "-i"); push(&mut a, "desktop");
    // Audio: DirectShow input named after the discovered default mic.
    // If `options.mic` is unset OR no device was found, we ship video-only.
    let have_audio = options.mic || options.system_audio;
    let use_mic    = have_audio && mic_device.is_some();
    if use_mic {
        let dev = mic_device.unwrap();
        push(&mut a, "-f"); push(&mut a, "dshow");
        push(&mut a, "-i"); push(&mut a, &format!("audio={}", dev));
    }
    // Video encode: H.264 ultrafast preset, yuv420p so the result plays in
    // every consumer player. Same even-dimension `pad` as Linux because
    // gdigrab on odd-sized monitor layouts otherwise fails the same way.
    push(&mut a, "-c:v"); push(&mut a, "libx264");
    push(&mut a, "-preset"); push(&mut a, "ultrafast");
    push(&mut a, "-pix_fmt"); push(&mut a, "yuv420p");
    push(&mut a, "-vf"); push(&mut a, "pad=ceil(iw/2)*2:ceil(ih/2)*2");
    if use_mic {
        // AAC stereo at 160 kbps — same as Linux so converted files behave
        // identically downstream. `-ac 2` upmixes mono mics so the file
        // always carries a stereo track and our convert pipeline doesn't
        // need a per-platform branch.
        push(&mut a, "-c:a"); push(&mut a, "aac");
        push(&mut a, "-b:a"); push(&mut a, "160k");
        push(&mut a, "-ac"); push(&mut a, "2");
    }
    push(&mut a, output_path);
    a
}

/// Parse `ffmpeg -list_devices true -f dshow -i dummy` output for the names
/// of audio capture devices. Returns names in the order ffmpeg reports them
/// (which is the OS-default-first order we want).
///
/// Output we're parsing looks like:
///
/// ```text
/// [dshow @ 0000…] DirectShow video devices (some may be both video and audio devices)
/// [dshow @ 0000…]  "USB Camera"
/// [dshow @ 0000…]     Alternative name "@device_pnp_…"
/// [dshow @ 0000…] DirectShow audio devices
/// [dshow @ 0000…]  "Microphone (Realtek(R) Audio)"
/// [dshow @ 0000…]     Alternative name "@device_cm_…"
/// ```
///
/// We only care about names listed *after* the "DirectShow audio devices"
/// header and *not* under an "Alternative name" line.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn parse_dshow_audio_devices(stderr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_audio = false;
    for line in stderr.lines() {
        let l = line.trim_start();
        // Strip ffmpeg's `[dshow @ ...]` prefix if present.
        let body = if let Some(rest) = l.strip_prefix('[') {
            rest.splitn(2, ']').nth(1).unwrap_or(rest).trim_start()
        } else { l };
        if body.starts_with("DirectShow audio devices") {
            in_audio = true;
            continue;
        }
        if body.starts_with("DirectShow video devices") {
            in_audio = false;
            continue;
        }
        if !in_audio { continue; }
        if body.starts_with("Alternative name") { continue; }
        // Device-name lines are `"Name (...)"` (with the quotes). Anything
        // else (blank lines, error footer, `Immediate exit requested`) we
        // skip.
        if let Some(start) = body.find('"') {
            if let Some(end_rel) = body[start + 1..].find('"') {
                let name = &body[start + 1..start + 1 + end_rel];
                if !name.is_empty() {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

/// Pre-flight: open `device` for a 1-second probe to confirm Windows lets
/// this process actually read samples from it. Catches the most common
/// "silent muted MP4" causes:
///   - Windows Settings → Privacy → Microphone → "Let desktop apps access
///     your microphone" is OFF (very common after Win11 fresh install).
///   - The mic device is exclusive-mode held by another app.
///   - The mic was renamed/unplugged between device-list and start time.
///
/// Returns `Ok(())` if ffmpeg opens the device successfully within ~3s.
/// On failure we shape the stderr into an actionable message and let the
/// tray-loop's existing dialog code surface it. NO MP4 is produced; the
/// user gets a real error before the recording "starts".
#[cfg(target_os = "windows")]
fn probe_dshow_audio_device(ffmpeg: &Path, device: &str) -> Result<()> {
    use std::time::Duration;
    let mut child = Command::new(ffmpeg)
        .args([
            "-hide_banner", "-nostats",
            "-rtbufsize", "32M",
            "-f", "dshow",
            "-i", &format!("audio={device}"),
            "-t", "0.5",
            "-f", "null", "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Recording(format!("audio probe spawn failed: {e}")))?;

    // 3s ceiling — a healthy probe finishes in <500ms; anything longer is
    // a device that's either denied or exclusive-locked.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let exit = loop {
        if let Some(status) = child.try_wait().ok().flatten() {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let stderr = child.stderr.take()
        .map(|mut s| { let mut b = String::new(); use std::io::Read as _; let _ = s.read_to_string(&mut b); b })
        .unwrap_or_default();

    let ok = exit.map(|s| s.success()).unwrap_or(false);
    if ok {
        return Ok(());
    }

    let lower = stderr.to_lowercase();
    let msg = if lower.contains("access is denied") || lower.contains("0x80070005") {
        format!(
            "Windows blocked microphone access for KAShot.\n\n\
             Open Settings → Privacy & Security → Microphone, turn on \
             \"Microphone access\" AND \"Let desktop apps access your \
             microphone\", then retry.\n\n\
             (Device: {device})"
        )
    } else if lower.contains("could not find") || lower.contains("i/o error")
            || lower.contains("no such") {
        format!(
            "The audio device \"{device}\" is no longer available. \
             Did it get unplugged or renamed? Re-open the Record menu \
             to refresh the device list."
        )
    } else if lower.contains("device or resource busy")
            || lower.contains("exclusive") {
        format!(
            "The audio device \"{device}\" is held in exclusive mode by \
             another app (often Zoom / Teams / Discord). Close that app \
             or right-click the speaker icon → Sounds → Recording → pick \
             the device → Properties → Advanced → uncheck \"Allow \
             applications to take exclusive control\", then retry."
        )
    } else if exit.is_none() {
        format!(
            "The microphone probe didn't finish in 3 s — \"{device}\" is \
             likely deadlocked. Unplug + replug the device, or pick a \
             different one from the Record menu."
        )
    } else {
        // Generic non-zero exit — pass through the last line of ffmpeg's
        // stderr so support has something to work with.
        let tail = stderr.lines().rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("(no ffmpeg output)");
        format!("Could not open audio device \"{device}\": {tail}")
    };
    Err(Error::Recording(msg))
}

/// Probe ffmpeg for available DirectShow audio devices. Returns an empty Vec
/// on any error (missing ffmpeg, no DirectShow, no mics). ffmpeg writes the
/// device list to stderr and exits non-zero — that's expected and not an
/// error we want to surface to the user.
#[cfg(target_os = "windows")]
fn list_dshow_audio_devices(ffmpeg: &Path) -> Vec<String> {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-list_devices", "true",
               "-f", "dshow", "-i", "dummy"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match out {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stderr);
            parse_dshow_audio_devices(&s)
        }
        Err(_) => Vec::new(),
    }
}

// ── unreachable on the platforms above, kept so non-tier-1 OSes still build ──

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn spawn_recorder(_output: &Path, _options: RecordingOptions) -> Result<Child> {
    Err(Error::Recording(
        "screen recording is not implemented on this platform yet".into()))
}

/// Send the platform-appropriate "please finish gracefully" signal so the
/// container is finalized before the process exits.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn graceful_signal(child: &mut Child) {
    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        // ffmpeg interprets 'q' on stdin as "stop and finalize" on every
        // platform — including Windows, where there's no SIGINT to send to
        // a console-less child.
        let _ = writeln!(stdin, "q");
    }
}

#[cfg(target_os = "macos")]
fn graceful_signal(child: &mut Child) {
    use std::io::Write;
    // Two backends: ffmpeg (audio recordings) is spawned with a piped stdin
    // and stops on 'q'; screencapture (video-only) has a null stdin and stops
    // on SIGINT. Presence of the stdin pipe is how we tell them apart.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "q");
        return;
    }
    // We don't depend on libc, so shell out to /bin/kill — part of base macOS.
    let pid = child.id().to_string();
    let _ = Command::new("/bin/kill").args(["-INT", &pid]).status();
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn graceful_signal(_child: &mut Child) {}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Linux argv-builder: assert the shape of the command we hand to ffmpeg
    // for every audio combination, without spawning anything.

    #[test]
    fn linux_argv_video_only() {
        let a = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", RecordingOptions::NONE, None);
        assert!(a.windows(2).any(|w| w == ["-f", "x11grab"]),
                "missing -f x11grab in: {:?}", a);
        assert!(a.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(!a.iter().any(|s| s == "pulse"), "pulse should be absent when audio off");
        assert!(!a.iter().any(|s| s == "-c:a"), "audio codec should be absent when audio off");
        assert_eq!(a.last().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn linux_argv_mic_only() {
        let a = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", RecordingOptions::MIC_ONLY, None);
        // mic is stream index 1 with `-i default`.
        let i_default_pos = a.iter().position(|s| s == "default")
            .expect("missing pulse mic input");
        assert_eq!(a[i_default_pos - 1], "-i");
        assert!(a.windows(2).any(|w| w == ["-f", "pulse"]));
        assert!(a.windows(2).any(|w| w == ["-c:a", "aac"]));
    }

    #[test]
    fn linux_argv_mic_and_sys_uses_amix() {
        let a = build_linux_ffmpeg_args(":0", "/tmp/out.mp4",
            RecordingOptions::MIC_AND_SYS, Some("alsa_output.0.monitor"));
        let fc = a.iter().position(|s| s == "-filter_complex").expect("missing -filter_complex");
        assert!(a[fc + 1].contains("amix=inputs=2"),
                "expected amix filter, got {:?}", a[fc + 1]);
        assert!(a.iter().any(|s| s == "alsa_output.0.monitor"));
    }

    // Windows argv-builder: same shape of assertions. These tests are
    // gated on `#[cfg(any(target_os = "windows", test))]` for the builder
    // and run on every host so a Linux CI agent will catch a Windows-specific
    // regression in the argv we hand to ffmpeg.

    #[test]
    fn windows_argv_video_only_no_mic_available() {
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4", RecordingOptions::NONE, None);
        assert!(a.windows(2).any(|w| w == ["-f", "gdigrab"]),
                "missing -f gdigrab in: {:?}", a);
        assert!(a.windows(2).any(|w| w == ["-i", "desktop"]));
        assert!(a.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(!a.iter().any(|s| s == "dshow"), "dshow should be absent when audio off");
        assert!(!a.iter().any(|s| s == "-c:a"), "audio codec should be absent when audio off");
        assert_eq!(a.last().unwrap(), "C:/tmp/out.mp4");
    }

    #[test]
    fn windows_argv_mic_only_uses_dshow_aac() {
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4",
            RecordingOptions::MIC_ONLY, Some("Microphone (Realtek)"));
        assert!(a.windows(2).any(|w| w == ["-f", "dshow"]));
        // The audio input arg is `audio=<name>`.
        assert!(a.iter().any(|s| s == "audio=Microphone (Realtek)"),
                "missing audio= input in: {:?}", a);
        assert!(a.windows(2).any(|w| w == ["-c:a", "aac"]));
        assert!(a.windows(2).any(|w| w == ["-ac", "2"]),
                "should always upmix to stereo so output container matches Linux: {:?}", a);
    }

    #[test]
    fn windows_argv_mic_requested_but_none_available_falls_back_to_video_only() {
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4",
            RecordingOptions::MIC_ONLY, None);
        assert!(a.windows(2).any(|w| w == ["-f", "gdigrab"]));
        assert!(!a.iter().any(|s| s == "dshow"),
                "should drop dshow when no mic device exists, got: {:?}", a);
        assert!(!a.iter().any(|s| s == "-c:a"),
                "should drop audio codec when there's no audio stream, got: {:?}", a);
    }

    #[test]
    fn windows_argv_system_audio_alone_degrades_to_mic_when_loopback_unavailable() {
        // Until WASAPI loopback lands, system-audio requests use the
        // discovered mic as a best-effort fallback. The argv should still
        // produce a working AAC-stereo MP4.
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4",
            RecordingOptions::SYSTEM_ONLY, Some("Stereo Mix (Realtek)"));
        assert!(a.iter().any(|s| s == "audio=Stereo Mix (Realtek)"));
        assert!(a.windows(2).any(|w| w == ["-c:a", "aac"]));
    }

    // dshow device-list parser.

    #[test]
    fn parse_dshow_picks_audio_section_only() {
        let sample = r#"
[dshow @ 0000020D] DirectShow video devices (some may be both video and audio devices)
[dshow @ 0000020D]  "Integrated Camera"
[dshow @ 0000020D]     Alternative name "@device_pnp_\\?\usb#vid_..."
[dshow @ 0000020D] DirectShow audio devices
[dshow @ 0000020D]  "Microphone (Realtek(R) Audio)"
[dshow @ 0000020D]     Alternative name "@device_cm_{...}"
[dshow @ 0000020D]  "Headset Microphone (Plantronics)"
[dshow @ 0000020D]     Alternative name "@device_cm_{...}"
dummy: Immediate exit requested
"#;
        let devs = parse_dshow_audio_devices(sample);
        assert_eq!(devs, vec![
            "Microphone (Realtek(R) Audio)".to_string(),
            "Headset Microphone (Plantronics)".to_string(),
        ]);
    }

    #[test]
    fn parse_dshow_handles_empty_or_garbage_output() {
        assert!(parse_dshow_audio_devices("").is_empty());
        assert!(parse_dshow_audio_devices("ffmpeg version 6.1\nbuilt with gcc").is_empty());
    }

    #[test]
    fn parse_dshow_skips_video_devices() {
        let sample = r#"
[dshow @ 1] DirectShow video devices
[dshow @ 1]  "Integrated Camera"
[dshow @ 1]     Alternative name "@device_pnp_..."
"#;
        assert!(parse_dshow_audio_devices(sample).is_empty(),
                "must not return video devices as audio");
    }

    // pick_windows_audio_device — selection rules under each
    // (mic, system_audio, available-devices) combo.

    #[test]
    fn pick_audio_video_only_returns_none() {
        let got = pick_windows_audio_device(&[
            "Microphone (Realtek)".to_string(),
        ], RecordingOptions::NONE).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn pick_audio_mic_only_prefers_microphone_named_device() {
        let devs = vec![
            "Line In (Realtek)".to_string(),
            "Microphone (Realtek)".to_string(),
        ];
        let got = pick_windows_audio_device(&devs, RecordingOptions::MIC_ONLY).unwrap();
        assert_eq!(got, Some("Microphone (Realtek)".to_string()));
    }

    #[test]
    fn pick_audio_mic_only_falls_back_to_first_if_no_mic_named() {
        let devs = vec![
            "Line In (Realtek)".to_string(),
            "Aux In".to_string(),
        ];
        let got = pick_windows_audio_device(&devs, RecordingOptions::MIC_ONLY).unwrap();
        assert_eq!(got, Some("Line In (Realtek)".to_string()));
    }

    #[test]
    fn pick_audio_mic_only_errors_when_no_devices() {
        let err = pick_windows_audio_device(&[], RecordingOptions::MIC_ONLY).unwrap_err();
        let Error::Recording(msg) = err else { panic!("wrong error variant") };
        // Should lead with the Privacy fix, not the "plug in a mic" red
        // herring — empty dshow audio list on Windows almost always means
        // the OS Privacy gate is blocking enumeration.
        let lower = msg.to_lowercase();
        assert!(lower.contains("privacy"), "should mention Privacy: {msg}");
        assert!(lower.contains("microphone access"), "should name the toggle: {msg}");
    }

    #[test]
    fn pick_audio_system_audio_finds_stereo_mix() {
        let devs = vec![
            "Microphone (Realtek)".to_string(),
            "Stereo Mix (Realtek)".to_string(),
        ];
        let got = pick_windows_audio_device(&devs, RecordingOptions::SYSTEM_ONLY).unwrap();
        assert_eq!(got, Some("Stereo Mix (Realtek)".to_string()));
    }

    #[test]
    fn pick_audio_system_audio_finds_voicemeeter() {
        let devs = vec![
            "VoiceMeeter Output (VB-Audio VoiceMeeter VAIO)".to_string(),
        ];
        let got = pick_windows_audio_device(&devs, RecordingOptions::SYSTEM_ONLY).unwrap();
        assert_eq!(got.unwrap().contains("VoiceMeeter"), true);
    }

    #[test]
    fn pick_audio_system_only_errors_when_no_loopback_device() {
        // mic exists but no Stereo Mix / VoiceMeeter / etc. — system_audio
        // alone must surface the actionable Stereo Mix instructions, NOT
        // silently downgrade to mic.
        let devs = vec!["Microphone (Realtek)".to_string()];
        let err = pick_windows_audio_device(&devs, RecordingOptions::SYSTEM_ONLY)
            .unwrap_err();
        let Error::Recording(msg) = err else { panic!("wrong error variant") };
        assert!(msg.to_lowercase().contains("stereo mix"),
                "actionable message should name Stereo Mix; got: {msg}");
    }

    #[test]
    fn pick_audio_mic_and_sys_falls_back_to_mic_when_no_loopback() {
        // With both requested and no loopback device, we degrade to mic
        // (the toast already says "with mic + system audio" — the truthful
        // user-visible label will be tightened in a follow-up).
        let devs = vec!["Microphone (Realtek)".to_string()];
        let got = pick_windows_audio_device(&devs, RecordingOptions::MIC_AND_SYS).unwrap();
        assert_eq!(got, Some("Microphone (Realtek)".to_string()));
    }

    // macOS avfoundation: argv-builder, device-table parser, and picker.
    // Same `#[cfg(any(target_os = "macos", test))]` strategy so a Linux CI
    // agent catches regressions in the command we hand to ffmpeg on a Mac.

    #[test]
    fn macos_argv_mic_uses_avfoundation_fused_input() {
        let a = build_macos_ffmpeg_args(1, Some(0), "/tmp/out.mp4", RecordingOptions::MIC_ONLY);
        assert!(a.windows(2).any(|w| w == ["-f", "avfoundation"]),
                "missing -f avfoundation in: {:?}", a);
        // Video+audio fuse into one "-i video:audio" token.
        assert!(a.windows(2).any(|w| w == ["-i", "1:0"]),
                "expected fused -i 1:0 in: {:?}", a);
        assert!(a.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(a.windows(2).any(|w| w == ["-c:a", "aac"]));
        assert!(a.windows(2).any(|w| w == ["-ac", "2"]),
                "should upmix to stereo to match other platforms: {:?}", a);
        assert_eq!(a.last().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn macos_argv_no_audio_index_omits_audio_codec() {
        // Defensive: if the builder is ever called with no audio device the
        // input's audio half is empty and no AAC stream is requested.
        let a = build_macos_ffmpeg_args(2, None, "/tmp/out.mp4", RecordingOptions::NONE);
        assert!(a.windows(2).any(|w| w == ["-i", "2:"]),
                "expected video-only fused input '2:' in: {:?}", a);
        assert!(!a.iter().any(|s| s == "-c:a"),
                "audio codec should be absent with no audio index: {:?}", a);
    }

    #[test]
    fn parse_avfoundation_splits_video_and_audio() {
        let sample = r#"
[AVFoundation indev @ 0x7f] AVFoundation video devices:
[AVFoundation indev @ 0x7f] [0] FaceTime HD Camera
[AVFoundation indev @ 0x7f] [1] Capture screen 0
[AVFoundation indev @ 0x7f] AVFoundation audio devices:
[AVFoundation indev @ 0x7f] [0] MacBook Pro Microphone
[AVFoundation indev @ 0x7f] [1] BlackHole 2ch
"#;
        let (video, audio) = parse_avfoundation_devices(sample);
        assert_eq!(video, vec![(0, "FaceTime HD Camera".to_string()),
                               (1, "Capture screen 0".to_string())]);
        assert_eq!(audio, vec![(0, "MacBook Pro Microphone".to_string()),
                               (1, "BlackHole 2ch".to_string())]);
    }

    #[test]
    fn parse_avfoundation_handles_empty() {
        let (v, a) = parse_avfoundation_devices("");
        assert!(v.is_empty() && a.is_empty());
    }

    #[test]
    fn pick_macos_screen_finds_capture_screen() {
        let video = vec![(0, "FaceTime HD Camera".to_string()),
                         (3, "Capture screen 0".to_string())];
        assert_eq!(pick_macos_screen_index(&video).unwrap(), 3);
    }

    #[test]
    fn pick_macos_screen_errors_without_capture_device() {
        // Screen Recording permission not granted → screen devices vanish.
        let video = vec![(0, "FaceTime HD Camera".to_string())];
        let err = pick_macos_screen_index(&video).unwrap_err();
        let Error::Recording(msg) = err else { panic!("wrong error variant") };
        assert!(msg.to_lowercase().contains("screen recording"),
                "should name the permission to grant: {msg}");
    }

    #[test]
    fn pick_macos_audio_video_only_returns_none() {
        let audio = vec![(0, "MacBook Pro Microphone".to_string())];
        assert_eq!(pick_macos_audio_device(&audio, RecordingOptions::NONE).unwrap(), None);
    }

    #[test]
    fn pick_macos_audio_mic_prefers_microphone() {
        let audio = vec![(0, "Aggregate Device".to_string()),
                         (1, "MacBook Pro Microphone".to_string())];
        assert_eq!(pick_macos_audio_device(&audio, RecordingOptions::MIC_ONLY).unwrap(), Some(1));
    }

    #[test]
    fn pick_macos_audio_system_finds_blackhole() {
        let audio = vec![(0, "MacBook Pro Microphone".to_string()),
                         (1, "BlackHole 2ch".to_string())];
        assert_eq!(pick_macos_audio_device(&audio, RecordingOptions::SYSTEM_ONLY).unwrap(), Some(1));
    }

    #[test]
    fn pick_macos_audio_system_only_errors_without_loopback() {
        let audio = vec![(0, "MacBook Pro Microphone".to_string())];
        let err = pick_macos_audio_device(&audio, RecordingOptions::SYSTEM_ONLY).unwrap_err();
        let Error::Recording(msg) = err else { panic!("wrong error variant") };
        assert!(msg.to_lowercase().contains("blackhole"),
                "actionable message should name a loopback option: {msg}");
    }

    #[test]
    fn pick_macos_audio_mic_and_sys_degrades_to_mic_without_loopback() {
        let audio = vec![(0, "MacBook Pro Microphone".to_string())];
        assert_eq!(pick_macos_audio_device(&audio, RecordingOptions::MIC_AND_SYS).unwrap(), Some(0));
    }
}
