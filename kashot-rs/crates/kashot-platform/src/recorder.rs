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
//! * Windows          : `ffmpeg -f gdigrab` for video; audio is captured
//!                      natively via WASAPI (see `recorder_windows_audio.rs`)
//!                      and streamed into ffmpeg over a loopback TCP socket.
//!                      The default render endpoint in loopback mode is the
//!                      system audio and the default capture endpoint is the
//!                      mic, so both work with **no** Stereo Mix / VB-Audio
//!                      driver. gdigrab stays for video — it's CPU-heavy and
//!                      doesn't pick up DWM-composited surfaces as cleanly as
//!                      `Windows.Graphics.Capture` would, but it's a small,
//!                      proven delta from the Linux pattern.
//!                      TODO: port video to `Windows.Graphics.Capture` +
//!                      MediaFoundation for per-window capture and
//!                      hardware-accelerated encoding.
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

// WASAPI loopback + mic capture lives in its own file to keep the COM-heavy
// code out of this module. Windows-only; everything it exposes is gated too.
#[cfg(target_os = "windows")]
#[path = "recorder_windows_audio.rs"]
mod windows_audio;

// ScreenCaptureKit system-audio capture (macOS), likewise out-of-line.
#[cfg(target_os = "macos")]
#[path = "recorder_macos_audio.rs"]
mod macos_audio;

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
    backend: Option<Backend>,
    output:  Option<PathBuf>,
}

/// What's actually running underneath a live recording. Most platforms drive a
/// single child process (`ffmpeg` on Linux/Windows, `ffmpeg` or `screencapture`
/// on macOS); Windows additionally runs WASAPI capture threads that stream PCM
/// into ffmpeg over a loopback socket, and macOS 15+ drives a native
/// ScreenCaptureKit session with no child at all.
enum Backend {
    Process {
        child: Child,
        /// Windows-only: WASAPI capture pumps feeding ffmpeg over loopback TCP.
        /// The field only exists on Windows so every other platform keeps a
        /// single-field `Process` backend with nothing to join.
        #[cfg(target_os = "windows")]
        pumps: Vec<AudioPump>,
        /// macOS-only: a ScreenCaptureKit system-audio session feeding ffmpeg
        /// over loopback TCP. `None` for video-only or mic-only recordings.
        #[cfg(target_os = "macos")]
        sck: Option<macos_audio::SckSession>,
    },
}

impl Backend {
    /// Graceful stop for `Recorder::stop()`: signal, then block until the child
    /// has finalized the container.
    fn stop_blocking(self) {
        match self {
            Backend::Process {
                mut child,
                #[cfg(target_os = "windows")] mut pumps,
                #[cfg(target_os = "macos")] sck,
            } => {
                graceful_signal(&mut child);
                #[cfg(target_os = "windows")]
                for p in &pumps { p.signal_stop(); }
                let _ = child.wait();
                #[cfg(target_os = "windows")]
                for p in &mut pumps { p.join(); }
                #[cfg(target_os = "macos")]
                if let Some(s) = sck { s.stop(); }
            }
        }
    }

    /// Stop for `Drop`: graceful signal, bounded ~2 s wait, then SIGKILL only if
    /// the child is still alive — so a normal teardown always yields a playable
    /// file but a wedged child can't hang the app.
    fn stop_with_timeout(self) {
        match self {
            Backend::Process {
                mut child,
                #[cfg(target_os = "windows")] mut pumps,
                #[cfg(target_os = "macos")] sck,
            } => {
                graceful_signal(&mut child);
                #[cfg(target_os = "windows")]
                for p in &pumps { p.signal_stop(); }
                let mut exited = false;
                for _ in 0..20 {
                    match child.try_wait() {
                        Ok(Some(_)) => { exited = true; break; }
                        Ok(None)    => std::thread::sleep(std::time::Duration::from_millis(100)),
                        Err(_)      => break,
                    }
                }
                if !exited { let _ = child.kill(); }
                let _ = child.wait();
                #[cfg(target_os = "windows")]
                for p in &mut pumps { p.join(); }
                #[cfg(target_os = "macos")]
                if let Some(s) = sck { s.stop(); }
            }
        }
    }
}

/// A background thread streaming captured PCM into ffmpeg over a loopback TCP
/// socket, plus the flag that tells it to stop. Created only by the Windows
/// WASAPI path; see `windows_audio`.
#[cfg(target_os = "windows")]
struct AudioPump {
    stop:   std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "windows")]
impl AudioPump {
    fn signal_stop(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    fn join(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// The PCM format + loopback port of one started WASAPI source, everything
/// `build_windows_ffmpeg_args` needs to wire an `-i tcp://…` input. Plain data
/// (no COM), so the argv builder stays unit-testable on any host.
#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WasapiAudioSpec {
    pub port:        u16,
    pub sample_rate: u32,
    pub channels:    u16,
    pub ffmpeg_fmt:  &'static str,
}

impl Recorder {
    pub fn new() -> Self {
        Self { backend: None, output: None }
    }

    pub fn is_recording(&self) -> bool { self.backend.is_some() }
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
        let backend = spawn_recorder(&output, options)?;
        self.backend = Some(backend);
        self.output  = Some(output);
        Ok(())
    }

    /// Stop the active recording. Returns the output file path on success.
    /// The OS recorder needs a moment to flush the trailing frames + finalize
    /// the container — we block on it so the file is playable when this returns.
    pub fn stop(&mut self) -> Result<PathBuf> {
        let backend = self.backend.take()
            .ok_or_else(|| Error::Recording("not currently recording".into()))?;
        let path = self.output.take()
            .unwrap_or_else(PathBuf::new);

        backend.stop_blocking();
        Ok(path)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(b) = self.backend.take() {
            b.stop_with_timeout();
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
fn spawn_recorder(output: &Path, options: RecordingOptions) -> Result<Backend> {
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
        Ok(c) => Ok(Backend::Process { child: c }),
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

// ── macOS: screencapture / avfoundation video + mic, ScreenCaptureKit audio ──
//
// `screencapture -v` (dependency-free) still handles the video-only common
// case. When audio is requested, video + optional mic come through ffmpeg's
// avfoundation input, and **system audio** comes from ScreenCaptureKit
// (recorder_macos_audio.rs) streamed into ffmpeg over a loopback socket — so it
// works with no BlackHole / Aggregate device. ffmpeg muxes (and `amix`es when
// both mic and system audio are present), exactly like the Linux path.
#[cfg(target_os = "macos")]
fn spawn_recorder(output: &Path, options: RecordingOptions) -> Result<Backend> {
    let path = output.to_str().ok_or_else(||
        Error::Recording("non-UTF-8 output path".into()))?;

    // Video-only: keep the dependency-free built-in. stdin stays null, which
    // is how `graceful_signal` tells the two backends apart.
    if !options.has_audio() {
        let child = Command::new("screencapture")
            .args(["-v", path])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Recording(format!("failed to spawn screencapture: {e}")))?;
        return Ok(Backend::Process { child, sck: None });
    }

    // Audio requested → ffmpeg avfoundation (video + optional mic).
    let ffmpeg = locate_ffmpeg().ok_or_else(|| Error::Recording(
        "recording audio on macOS needs ffmpeg, which wasn't found next to \
         Kashot or on your PATH. Install it with: brew install ffmpeg — then \
         retry. (Video-only recording works without ffmpeg.)".into()
    ))?;

    let listing = list_avfoundation_devices(&ffmpeg);
    let (video_devs, audio_devs) = parse_avfoundation_devices(&listing);
    let screen_idx = pick_macos_screen_index(&video_devs)?;
    let mic_idx = if options.mic { pick_macos_mic_device(&audio_devs) } else { None };

    // System audio via ScreenCaptureKit, started before we build the argv so we
    // know the loopback port. If ffmpeg later fails to spawn we tear this down.
    let sck = if options.system_audio {
        Some(macos_audio::start_system_audio()?)
    } else {
        None
    };
    let sck_port = sck.as_ref().map(|s| s.port);

    let args = build_macos_ffmpeg_args(screen_idx, mic_idx, sck_port, path);
    let res = Command::new(&ffmpeg)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match res {
        Ok(child) => Ok(Backend::Process { child, sck }),
        Err(e) => {
            if let Some(s) = sck { s.stop(); }
            if e.kind() == std::io::ErrorKind::NotFound {
                Err(Error::Recording("ffmpeg not found — install it with: brew install ffmpeg".into()))
            } else {
                Err(Error::Recording(format!("failed to spawn ffmpeg: {e}")))
            }
        }
    }
}

/// Build the ffmpeg argv for macOS: avfoundation video (+ optional fused mic)
/// as input 0, plus an optional ScreenCaptureKit system-audio TCP input. When
/// both mic and system audio are present they're `amix`ed to one stereo AAC
/// track. Pure function so the suite can assert argv shape without a Mac.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn build_macos_ffmpeg_args(
    screen_idx: usize,
    mic_idx:    Option<usize>,
    sck_port:   Option<u16>,
    output_path: &str,
) -> Vec<String> {
    let mut a: Vec<String> = Vec::with_capacity(32);
    let push = |a: &mut Vec<String>, s: &str| a.push(s.to_string());
    push(&mut a, "-y");
    push(&mut a, "-f"); push(&mut a, "avfoundation");
    push(&mut a, "-framerate"); push(&mut a, "30");
    // avfoundation fuses one video + one audio device into a single token
    // "<video>:<audio>"; an empty audio half means video-only.
    let input = match mic_idx {
        Some(ai) => format!("{screen_idx}:{ai}"),
        None      => format!("{screen_idx}:"),
    };
    push(&mut a, "-i"); a.push(input);
    // System audio (ScreenCaptureKit) is a raw-PCM TCP input — input index 1.
    if let Some(port) = sck_port {
        push(&mut a, "-thread_queue_size"); push(&mut a, "1024");
        push(&mut a, "-f"); push(&mut a, "f32le");
        push(&mut a, "-ar"); push(&mut a, "48000");
        push(&mut a, "-ac"); push(&mut a, "2");
        push(&mut a, "-i"); a.push(format!("tcp://127.0.0.1:{port}"));
    }
    push(&mut a, "-c:v"); push(&mut a, "libx264");
    push(&mut a, "-preset"); push(&mut a, "ultrafast");
    push(&mut a, "-pix_fmt"); push(&mut a, "yuv420p");
    push(&mut a, "-vf"); push(&mut a, "pad=ceil(iw/2)*2:ceil(ih/2)*2");
    match (mic_idx.is_some(), sck_port.is_some()) {
        (true, true) => {
            // Mic (avf input 0 audio) + system audio (input 1) → one track.
            push(&mut a, "-filter_complex");
            push(&mut a, "[0:a][1:a]amix=inputs=2:duration=longest:dropout_transition=0[aout]");
            push(&mut a, "-map"); push(&mut a, "0:v");
            push(&mut a, "-map"); push(&mut a, "[aout]");
            push(&mut a, "-c:a"); push(&mut a, "aac");
            push(&mut a, "-b:a"); push(&mut a, "160k");
            push(&mut a, "-ac"); push(&mut a, "2");
        }
        (true, false) => {
            push(&mut a, "-map"); push(&mut a, "0:v");
            push(&mut a, "-map"); push(&mut a, "0:a");
            push(&mut a, "-c:a"); push(&mut a, "aac");
            push(&mut a, "-b:a"); push(&mut a, "160k");
            push(&mut a, "-ac"); push(&mut a, "2");
        }
        (false, true) => {
            push(&mut a, "-map"); push(&mut a, "0:v");
            push(&mut a, "-map"); push(&mut a, "1:a");
            push(&mut a, "-c:a"); push(&mut a, "aac");
            push(&mut a, "-b:a"); push(&mut a, "160k");
            push(&mut a, "-ac"); push(&mut a, "2");
        }
        (false, false) => {}
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

/// Pick the avfoundation microphone device index. System audio no longer comes
/// through here — it's captured natively by ScreenCaptureKit — so this only
/// chooses a mic: prefer a microphone-looking name, else the first audio
/// device, else `None` (no mic available → video[+system] only). Pure so it's
/// unit-testable off a Mac.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn pick_macos_mic_device(audio: &[(usize, String)]) -> Option<usize> {
    audio.iter()
        .find(|(_, n)| {
            let l = n.to_ascii_lowercase();
            l.contains("microphone") || l.contains("mic") || l.contains("built-in")
                || l.contains("macbook") || l.contains("headset")
        })
        .or_else(|| audio.first())
        .map(|(i, _)| *i)
}

// ── Windows: ffmpeg -f gdigrab video + WASAPI audio over loopback TCP ────────
//
// Video stays on gdigrab (low-risk, already shipping). Audio is captured
// natively via WASAPI (see recorder_windows_audio.rs): the default render
// endpoint in loopback mode is the system audio, the default capture endpoint
// is the mic — no Stereo Mix, no VB-Audio driver. Each source streams raw PCM
// over a 127.0.0.1 socket that ffmpeg reads as an extra `-i`, and ffmpeg does
// the resample + amix, exactly like the Linux pulse + monitor path.

#[cfg(target_os = "windows")]
fn spawn_recorder(output: &Path, options: RecordingOptions) -> Result<Backend> {
    use windows_audio::SourceKind;

    let path = output.to_str().ok_or_else(||
        Error::Recording("non-UTF-8 output path".into()))?;
    let ffmpeg = locate_ffmpeg().unwrap_or_else(|| PathBuf::from("ffmpeg.exe"));

    // One WASAPI capture per requested source. Input-index order is irrelevant
    // (both feed amix), so mic-then-system is fine. If any source fails to
    // start, tear down the ones already running so we never leak a capture
    // thread, then surface the actionable error (mic-privacy is the usual one).
    let mut kinds: Vec<SourceKind> = Vec::new();
    if options.mic          { kinds.push(SourceKind::Microphone); }
    if options.system_audio { kinds.push(SourceKind::SystemLoopback); }

    let mut started: Vec<windows_audio::StartedSource> = Vec::new();
    for kind in kinds {
        match windows_audio::start_source(kind) {
            Ok(s) => started.push(s),
            Err(e) => {
                for mut s in started { s.pump.signal_stop(); s.pump.join(); }
                return Err(e);
            }
        }
    }

    let specs: Vec<WasapiAudioSpec> = started.iter().map(|s| s.spec.clone()).collect();
    let args = build_windows_ffmpeg_args(path, &specs);

    let res = Command::new(&ffmpeg)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match res {
        Ok(child) => {
            let pumps = started.into_iter().map(|s| s.pump).collect();
            Ok(Backend::Process { child, pumps })
        }
        Err(e) => {
            for mut s in started { s.pump.signal_stop(); s.pump.join(); }
            if e.kind() == std::io::ErrorKind::NotFound {
                Err(Error::Recording(
                    "ffmpeg.exe not found — the Kashot installer normally ships \
                     it next to kashot.exe. Reinstall, or drop ffmpeg.exe into \
                     the same folder as kashot.exe and retry.".into()))
            } else {
                Err(Error::Recording(format!("failed to spawn ffmpeg: {e}")))
            }
        }
    }
}

/// Build the ffmpeg argv for Windows: gdigrab video plus one raw-PCM TCP input
/// per WASAPI source, mixed down to a single stereo AAC track. Pure function so
/// the suite can assert exact argv composition without WASAPI or a real device.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn build_windows_ffmpeg_args(
    output_path: &str,
    audio:       &[WasapiAudioSpec],
) -> Vec<String> {
    let mut a: Vec<String> = Vec::with_capacity(48);
    let push = |a: &mut Vec<String>, s: &str| a.push(s.to_string());
    push(&mut a, "-y");
    // Video: GDI grab of the whole desktop at 30 fps. `desktop` is gdigrab's
    // pseudo-device name for the full virtual screen.
    push(&mut a, "-f"); push(&mut a, "gdigrab");
    push(&mut a, "-framerate"); push(&mut a, "30");
    push(&mut a, "-i"); push(&mut a, "desktop");
    // Audio: one raw-PCM input per WASAPI source. We're already listening on
    // the loopback port; ffmpeg connects back as the TCP client. The format /
    // rate / channels are exactly what the device handed us, so no conversion
    // happens before ffmpeg. `-thread_queue_size` keeps the demuxer from
    // dropping packets while the encoder is busy.
    for s in audio {
        push(&mut a, "-thread_queue_size"); push(&mut a, "1024");
        push(&mut a, "-f"); push(&mut a, s.ffmpeg_fmt);
        push(&mut a, "-ar"); a.push(s.sample_rate.to_string());
        push(&mut a, "-ac"); a.push(s.channels.to_string());
        push(&mut a, "-i"); a.push(format!("tcp://127.0.0.1:{}", s.port));
    }
    // Video encode: H.264 ultrafast preset, yuv420p so the result plays in
    // every consumer player. Same even-dimension `pad` as Linux because
    // gdigrab on odd-sized monitor layouts otherwise fails the same way.
    push(&mut a, "-c:v"); push(&mut a, "libx264");
    push(&mut a, "-preset"); push(&mut a, "ultrafast");
    push(&mut a, "-pix_fmt"); push(&mut a, "yuv420p");
    push(&mut a, "-vf"); push(&mut a, "pad=ceil(iw/2)*2:ceil(ih/2)*2");
    match audio.len() {
        0 => {}
        1 => {
            // Single source: video is input 0, audio is input 1.
            push(&mut a, "-map"); push(&mut a, "0:v");
            push(&mut a, "-map"); push(&mut a, "1:a");
            push(&mut a, "-c:a"); push(&mut a, "aac");
            push(&mut a, "-b:a"); push(&mut a, "160k");
            push(&mut a, "-ac"); push(&mut a, "2");
        }
        n => {
            // Mix every audio input (mic + system) into one stereo AAC track,
            // mirroring the Linux amix path.
            let inputs: String = (1..=n).map(|i| format!("[{i}:a]")).collect();
            push(&mut a, "-filter_complex");
            a.push(format!(
                "{inputs}amix=inputs={n}:duration=longest:dropout_transition=0[aout]"
            ));
            push(&mut a, "-map"); push(&mut a, "0:v");
            push(&mut a, "-map"); push(&mut a, "[aout]");
            push(&mut a, "-c:a"); push(&mut a, "aac");
            push(&mut a, "-b:a"); push(&mut a, "160k");
            push(&mut a, "-ac"); push(&mut a, "2");
        }
    }
    push(&mut a, output_path);
    a
}

// ── unreachable on the platforms above, kept so non-tier-1 OSes still build ──

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn spawn_recorder(_output: &Path, _options: RecordingOptions) -> Result<Backend> {
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

    // Windows argv-builder: gdigrab video plus one raw-PCM TCP input per
    // WASAPI source. Gated `#[cfg(any(target_os = "windows", test))]` so a
    // Linux CI agent catches regressions in the argv we hand to ffmpeg.

    fn spec(port: u16, rate: u32, ch: u16, fmt: &'static str) -> WasapiAudioSpec {
        WasapiAudioSpec { port, sample_rate: rate, channels: ch, ffmpeg_fmt: fmt }
    }

    #[test]
    fn windows_argv_video_only_no_audio_sources() {
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4", &[]);
        assert!(a.windows(2).any(|w| w == ["-f", "gdigrab"]),
                "missing -f gdigrab in: {:?}", a);
        assert!(a.windows(2).any(|w| w == ["-i", "desktop"]));
        assert!(a.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(!a.iter().any(|s| s.starts_with("tcp://")),
                "no audio sockets when video-only: {:?}", a);
        assert!(!a.iter().any(|s| s == "-c:a"), "audio codec should be absent when audio off");
        assert_eq!(a.last().unwrap(), "C:/tmp/out.mp4");
    }

    #[test]
    fn windows_argv_single_source_maps_input_one() {
        // One WASAPI source → input index 1 (gdigrab is 0). The declared
        // format/rate/channels are exactly what the device reported.
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4",
            &[spec(54123, 48000, 2, "f32le")]);
        assert!(a.windows(2).any(|w| w == ["-f", "f32le"]),
                "audio input format must match the device: {:?}", a);
        assert!(a.windows(2).any(|w| w == ["-ar", "48000"]));
        assert!(a.windows(2).any(|w| w == ["-ac", "2"]));
        assert!(a.iter().any(|s| s == "tcp://127.0.0.1:54123"),
                "missing loopback tcp input: {:?}", a);
        assert!(a.windows(2).any(|w| w == ["-map", "0:v"]));
        assert!(a.windows(2).any(|w| w == ["-map", "1:a"]));
        assert!(a.windows(2).any(|w| w == ["-c:a", "aac"]));
        assert_eq!(a.last().unwrap(), "C:/tmp/out.mp4");
    }

    #[test]
    fn windows_argv_two_sources_uses_amix() {
        // mic + system loopback → two inputs (1 and 2) mixed to one AAC track.
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4",
            &[spec(40001, 48000, 2, "f32le"), spec(40002, 44100, 1, "s16le")]);
        let fc = a.iter().position(|s| s == "-filter_complex")
            .expect("missing -filter_complex");
        assert!(a[fc + 1].contains("[1:a][2:a]amix=inputs=2"),
                "expected amix over both inputs, got {:?}", a[fc + 1]);
        assert!(a.windows(2).any(|w| w == ["-map", "[aout]"]));
        // Each source keeps its own declared format/rate.
        assert!(a.iter().any(|s| s == "tcp://127.0.0.1:40001"));
        assert!(a.iter().any(|s| s == "tcp://127.0.0.1:40002"));
        assert!(a.windows(2).any(|w| w == ["-f", "s16le"]),
                "second source's int16 format must be declared: {:?}", a);
    }

    #[test]
    fn windows_argv_uses_thread_queue_size_per_audio_input() {
        // Guards against demuxer packet drops while the encoder is busy.
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4",
            &[spec(50000, 48000, 2, "f32le")]);
        assert!(a.windows(2).any(|w| w == ["-thread_queue_size", "1024"]),
                "audio input should set -thread_queue_size: {:?}", a);
    }

    // macOS avfoundation: argv-builder, device-table parser, and picker.
    // Same `#[cfg(any(target_os = "macos", test))]` strategy so a Linux CI
    // agent catches regressions in the command we hand to ffmpeg on a Mac.

    #[test]
    fn macos_argv_mic_only_uses_avfoundation_fused_input() {
        // Mic, no system audio → just the fused avfoundation input, no TCP.
        let a = build_macos_ffmpeg_args(1, Some(0), None, "/tmp/out.mp4");
        assert!(a.windows(2).any(|w| w == ["-f", "avfoundation"]),
                "missing -f avfoundation in: {:?}", a);
        assert!(a.windows(2).any(|w| w == ["-i", "1:0"]),
                "expected fused -i 1:0 in: {:?}", a);
        assert!(!a.iter().any(|s| s.starts_with("tcp://")),
                "no system-audio socket when system audio is off: {:?}", a);
        assert!(a.windows(2).any(|w| w == ["-map", "0:a"]));
        assert!(a.windows(2).any(|w| w == ["-c:a", "aac"]));
        assert!(a.windows(2).any(|w| w == ["-ac", "2"]));
        assert_eq!(a.last().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn macos_argv_system_only_adds_sck_tcp_input() {
        // System audio, no mic → video-only avf input plus the SCK TCP input
        // mapped as the audio track.
        let a = build_macos_ffmpeg_args(2, None, Some(50321), "/tmp/out.mp4");
        assert!(a.windows(2).any(|w| w == ["-i", "2:"]),
                "expected video-only fused input '2:' in: {:?}", a);
        assert!(a.iter().any(|s| s == "tcp://127.0.0.1:50321"),
                "missing SCK loopback input: {:?}", a);
        assert!(a.windows(2).any(|w| w == ["-f", "f32le"]));
        assert!(a.windows(2).any(|w| w == ["-map", "1:a"]));
        assert!(a.windows(2).any(|w| w == ["-c:a", "aac"]));
    }

    #[test]
    fn macos_argv_mic_and_system_uses_amix() {
        // Mic (avf input 0 audio) + system audio (input 1) → amix to one track.
        let a = build_macos_ffmpeg_args(1, Some(0), Some(40044), "/tmp/out.mp4");
        let fc = a.iter().position(|s| s == "-filter_complex")
            .expect("missing -filter_complex");
        assert!(a[fc + 1].contains("[0:a][1:a]amix=inputs=2"),
                "expected amix over mic + system, got {:?}", a[fc + 1]);
        assert!(a.windows(2).any(|w| w == ["-map", "[aout]"]));
        assert!(a.iter().any(|s| s == "tcp://127.0.0.1:40044"));
    }

    #[test]
    fn macos_argv_video_only_omits_audio_codec() {
        // No mic, no system audio (shouldn't normally reach the builder, but be
        // defensive): video-only input and no audio codec.
        let a = build_macos_ffmpeg_args(2, None, None, "/tmp/out.mp4");
        assert!(a.windows(2).any(|w| w == ["-i", "2:"]));
        assert!(!a.iter().any(|s| s == "-c:a"),
                "audio codec should be absent with no audio: {:?}", a);
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
    fn pick_macos_mic_prefers_microphone_named_device() {
        // System audio no longer routes through avfoundation, so the picker
        // only chooses a mic — it should skip the Aggregate device and pick the
        // real microphone.
        let audio = vec![(0, "Aggregate Device".to_string()),
                         (1, "MacBook Pro Microphone".to_string())];
        assert_eq!(pick_macos_mic_device(&audio), Some(1));
    }

    #[test]
    fn pick_macos_mic_falls_back_to_first_when_no_mic_named() {
        let audio = vec![(0, "Line In".to_string()), (1, "Aux".to_string())];
        assert_eq!(pick_macos_mic_device(&audio), Some(0));
    }

    #[test]
    fn pick_macos_mic_none_when_no_audio_devices() {
        assert_eq!(pick_macos_mic_device(&[]), None);
    }
}
