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
//! * macOS            : built-in `screencapture -v`. Audio control is
//!                      limited and still ignored. TODO(v0.3): drive
//!                      AVFoundation directly so we can pick mic + system.
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
#[cfg(any(target_os = "linux", target_os = "windows"))]
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

#[cfg(target_os = "macos")]
fn spawn_recorder(output: &Path, options: RecordingOptions) -> Result<Child> {
    let _ = options; // screencapture has limited audio control; ignore for now.
    // TODO(v0.3): drive AVFoundation directly so mic + system audio can be
    // selected on macOS.
    let path = output.to_str().ok_or_else(||
        Error::Recording("non-UTF-8 output path".into()))?;

    Command::new("screencapture")
        .args(["-v", path])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::Recording(format!("failed to spawn screencapture: {e}")))
}

// ── Windows: ffmpeg -f gdigrab (+ -f dshow for mic) ─────────────────────────

#[cfg(target_os = "windows")]
fn spawn_recorder(output: &Path, options: RecordingOptions) -> Result<Child> {
    let path = output.to_str().ok_or_else(||
        Error::Recording("non-UTF-8 output path".into()))?;
    let ffmpeg = locate_ffmpeg().unwrap_or_else(|| PathBuf::from("ffmpeg.exe"));

    // Discover the default DirectShow microphone if the user asked for mic
    // (or system audio — see the TODO on WASAPI loopback at the top of the
    // file; until that lands, "system audio" silently degrades to "mic only"
    // when there's a mic available, and to "video only" when there isn't).
    let mic_device: Option<String> = if options.mic || options.system_audio {
        list_dshow_audio_devices(&ffmpeg)
            .into_iter()
            .next()
    } else { None };

    let args = build_windows_ffmpeg_args(path, options, mic_device.as_deref());
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
    // screencapture stops cleanly on SIGINT. We don't depend on libc, so
    // shell out to /bin/kill — it's part of the base macOS install.
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
}
