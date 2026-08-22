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
//! Recording is full-screen by default and region-limited when `start_region`
//! is handed a `CaptureRect`. The rectangle is part of the **public start API**
//! rather than an x11grab detail, because every backend can honour it and a
//! future PipeWire / xdg-desktop-portal backend will need the same value:
//!
//! * Linux  (X11)     : `-video_size WxH -i <display>+X,Y`
//! * Windows          : `-offset_x X -offset_y Y -video_size WxH -i desktop`
//! * macOS            : `screencapture -v -R x,y,w,h`, or an ffmpeg
//!                      `crop=W:H:X:Y` filter when audio routes capture
//!                      through avfoundation.
//!
//! X11 and gdigrab measure from the virtual desktop's corner
//! (`CaptureRect::offset_x`); `screencapture` measures from the main display's
//! corner (`CaptureRect::x`). `kashot_core::region` owns that distinction along
//! with the even-dimension rounding H.264 requires — nothing here re-derives it.
//!
//! Audio is untouched by any of this: the same sources, the same best-effort
//! degradation, the same `amix`. Only the video input differs.
//!
//! Stop is graceful per platform: write `q` to `ffmpeg`'s stdin (Linux,
//! Windows) or send SIGINT to `screencapture` (macOS) so the MP4 moov atom
//! is finalized. Every wait after that signal is bounded — ~15 s for an
//! explicit `stop()`, ~2 s in `Drop` — and escalates to a kill, so a normal
//! teardown always yields a playable file while a wedged encoder can never
//! freeze the UI thread. ffmpeg treats `q` on stdin the same on Windows as it
//! does on Linux, so the `recording_indicator` STOP button needs no
//! platform-specific tweaks.
//!
//! Between start and stop the encoder is watched rather than trusted:
//! `Recorder::poll_health()` reaps a child that died on its own, and the last
//! lines of its stderr are kept in a ring buffer so the failure can be
//! reported with its actual cause instead of a bare exit code.

use crate::{Error, Result};
use kashot_core::region::CaptureRect;
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
        /// Rolling tail of the child's stderr, kept by the drain thread.
        stderr: StderrTail,
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

/// How long `Recorder::stop()` waits for the encoder to finalize the container
/// before escalating to a kill, in 100 ms polls. Generous: a long 4K recording
/// can spend several seconds writing the moov atom, and killing early corrupts
/// exactly the file the user asked us to save. Still bounded, because this runs
/// on the UI thread and a wedged encoder must not freeze the app forever.
const STOP_POLL_STEPS: u32 = 150;

/// Same escalation ladder for `Drop`, but ~2 s: nobody is waiting on the file
/// at that point and the process is on its way out.
const DROP_POLL_STEPS: u32 = 20;

impl Backend {
    /// Graceful stop for `Recorder::stop()`: signal, wait up to
    /// `STOP_POLL_STEPS` for the child to finalize the container, then kill.
    /// `Ok` means the encoder exited on its own and the file is whole; `Err`
    /// means we escalated and whatever is on disk is very likely truncated.
    ///
    /// `output` rides along only so the failure message can name the file: a
    /// killed encoder usually still leaves a playable clip behind, and an
    /// error that doesn't say where it is sends the user hunting for it.
    /// Pass an empty path when there is none to name.
    fn stop_blocking(self, output: &Path) -> Result<()> {
        match self {
            Backend::Process {
                mut child,
                stderr,
                #[cfg(target_os = "windows")] mut pumps,
                #[cfg(target_os = "macos")] sck,
            } => {
                graceful_signal(&mut child);
                #[cfg(target_os = "windows")]
                for p in &pumps { p.signal_stop(); }
                let exited = wait_bounded(&mut child, STOP_POLL_STEPS);
                if !exited { let _ = child.kill(); }
                let _ = child.wait();
                #[cfg(target_os = "windows")]
                for p in &mut pumps { p.join(); }
                #[cfg(target_os = "macos")]
                if let Some(s) = sck { s.stop(); }
                if exited {
                    Ok(())
                } else {
                    let where_ = if output.as_os_str().is_empty() {
                        String::new()
                    } else {
                        format!(" Whatever was written so far is at {}.", output.display())
                    };
                    Err(Error::Recording(format!(
                        "the encoder didn't finish within {} seconds of being asked to \
                         stop, so it was killed. The saved file may be truncated or \
                         unplayable.{}{}",
                        STOP_POLL_STEPS / 10,
                        where_,
                        stderr.as_error_suffix()
                    )))
                }
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
                stderr: _,
                #[cfg(target_os = "windows")] mut pumps,
                #[cfg(target_os = "macos")] sck,
            } => {
                graceful_signal(&mut child);
                #[cfg(target_os = "windows")]
                for p in &pumps { p.signal_stop(); }
                if !wait_bounded(&mut child, DROP_POLL_STEPS) { let _ = child.kill(); }
                let _ = child.wait();
                #[cfg(target_os = "windows")]
                for p in &mut pumps { p.join(); }
                #[cfg(target_os = "macos")]
                if let Some(s) = sck { s.stop(); }
            }
        }
    }

    /// Has the child already exited on its own? `None` while it's still running
    /// (the healthy case) or if the status can't be read.
    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        match self {
            Backend::Process { child, .. } => child.try_wait().ok().flatten(),
        }
    }

    /// Tear down a backend whose child is already known to be dead: reap it and
    /// release the audio capture threads. No graceful signal — there's nothing
    /// left to signal.
    fn discard_dead(self) {
        match self {
            Backend::Process {
                mut child,
                stderr: _,
                #[cfg(target_os = "windows")] mut pumps,
                #[cfg(target_os = "macos")] sck,
            } => {
                let _ = child.wait();
                #[cfg(target_os = "windows")]
                for p in &mut pumps { p.signal_stop(); p.join(); }
                #[cfg(target_os = "macos")]
                if let Some(s) = sck { s.stop(); }
            }
        }
    }

    fn stderr_tail(&self) -> StderrTail {
        match self {
            Backend::Process { stderr, .. } => stderr.clone(),
        }
    }
}

/// Poll `child` for up to `steps` × 100 ms. Returns whether it exited. A
/// `try_wait` error is reported as "did not exit" so the caller escalates
/// rather than assuming a clean finalize it can't actually confirm.
fn wait_bounded(child: &mut Child, steps: u32) -> bool {
    for _ in 0..steps {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None)    => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(_)      => return false,
        }
    }
    false
}

// ── stderr ring buffer ──────────────────────────────────────────────────────

/// How many stderr lines we keep. The encoder's actionable message ("No space
/// left on device", "Device or resource busy") lands within the last handful of
/// lines before it dies; sixteen is enough context without holding a log.
const STDERR_TAIL_LINES: usize = 16;

/// The tail of a running recorder's stderr.
///
/// Something has to read that pipe for the whole recording — an unread pipe
/// fills its kernel buffer and stalls the encoder mid-write — so the drain
/// thread is not optional. Keeping the last few lines instead of discarding
/// them costs one small ring buffer and is the difference between telling the
/// user "recording stopped unexpectedly" and telling them the disk is full.
#[derive(Clone)]
struct StderrTail {
    lines: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
}

impl StderrTail {
    fn new() -> Self {
        Self {
            lines: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::with_capacity(STDERR_TAIL_LINES))),
        }
    }

    /// Record one stderr line, dropping the oldest once the ring is full.
    /// Blank lines and ffmpeg's per-second progress counters are skipped —
    /// they'd otherwise push every real diagnostic out of a 16-line window.
    fn push_line(&self, raw: &[u8]) {
        let line = sanitize_ascii(&String::from_utf8_lossy(raw));
        let line = line.trim();
        if line.is_empty() { return; }
        if line.starts_with("frame=") || line.starts_with("size=") { return; }
        if let Ok(mut q) = self.lines.lock() {
            if q.len() == STDERR_TAIL_LINES { q.pop_front(); }
            q.push_back(line.to_string());
        }
    }

    fn text(&self) -> String {
        match self.lines.lock() {
            Ok(q)  => q.iter().cloned().collect::<Vec<_>>().join("\n"),
            Err(_) => String::new(),
        }
    }

    /// The tail formatted for appending to an error message, or nothing at all
    /// when the encoder said nothing worth repeating.
    fn as_error_suffix(&self) -> String {
        let t = self.text();
        if t.is_empty() { String::new() } else { format!("\n\n{t}") }
    }
}

/// Fold arbitrary encoder output down to printable ASCII. Kashot renders text
/// with a 5x7 ASCII bitmap font, so anything else would reach the user as '?'
/// anyway — and control bytes would corrupt the layout. Doing it here means
/// every consumer of the tail gets renderable text.
fn sanitize_ascii(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '?' })
        .collect()
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

    /// Begin recording the whole desktop to `output`.
    ///
    /// Shorthand for [`Recorder::start_region`] with no rectangle.
    pub fn start(&mut self, output: PathBuf, options: RecordingOptions)
        -> Result<RecordingOptions>
    {
        self.start_region(output, options, None)
    }

    /// Begin recording to `output`, optionally limited to `region`.
    ///
    /// `region` is `None` for the whole desktop, or a `CaptureRect` in
    /// virtual-desktop coordinates — already clamped to the desktop and already
    /// even-sized by `kashot_core::region::record_rect_from_selection`. Each
    /// backend translates it into its own coordinate convention; see the module
    /// docs. Audio behaves identically either way.
    ///
    /// Returns the **effective** options — what's actually being recorded, not
    /// what was asked for. Audio is best-effort by design: a box with no
    /// PulseAudio server, or no default sink to monitor, still records video
    /// rather than failing outright. Callers must render their "recording
    /// started" toast from this value, otherwise they promise the user a
    /// microphone track that isn't in the file.
    ///
    /// Errors if a recording is already in progress, if the parent directory
    /// can't be created, or if the platform's recording tool isn't available.
    pub fn start_region(&mut self,
                        output:  PathBuf,
                        options: RecordingOptions,
                        region:  Option<CaptureRect>)
        -> Result<RecordingOptions>
    {
        if self.is_recording() {
            return Err(Error::Recording("a recording is already in progress".into()));
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let (backend, effective) = spawn_recorder(&output, options, region)?;
        self.backend = Some(backend);
        self.output  = Some(output);
        Ok(effective)
    }

    /// Check whether the recording is still alive, cheaply enough to call from
    /// the event loop every tick.
    ///
    /// Everything between `start()` and `stop()` used to be unobserved: if the
    /// encoder died at minute three (disk filled, capture device yanked, OOM
    /// kill) the app kept flashing its REC dot and `stop()` reported success
    /// over a truncated file. On a dead child this tears the backend down —
    /// `is_recording()` is false afterwards, so the caller can drop straight
    /// back to idle — and returns the reason, encoder stderr included.
    ///
    /// `Ok(())` while the recording is healthy, and when there's no recording
    /// at all: polling an idle recorder is not an error.
    pub fn poll_health(&mut self) -> Result<()> {
        let Some(backend) = self.backend.as_mut() else { return Ok(()) };
        let Some(status) = backend.exited() else { return Ok(()) };

        let tail = backend.stderr_tail();
        // `exited()` borrowed self.backend mutably; it's finished with it now.
        if let Some(b) = self.backend.take() { b.discard_dead(); }
        let path = self.output.take();

        let where_ = match &path {
            Some(p) => format!(" Whatever was written so far is at {}.", p.display()),
            None    => String::new(),
        };
        Err(Error::Recording(format!(
            "recording stopped unexpectedly: the encoder exited on its own ({status}).{}{}",
            where_, tail.as_error_suffix()
        )))
    }

    /// Stop the active recording. Returns the output file path on success.
    /// The OS recorder needs a moment to flush the trailing frames + finalize
    /// the container — we block on it (bounded; see `STOP_POLL_STEPS`) so the
    /// file is playable when this returns.
    pub fn stop(&mut self) -> Result<PathBuf> {
        let backend = self.backend.take()
            .ok_or_else(|| Error::Recording("not currently recording".into()))?;
        let path = self.output.take()
            .unwrap_or_else(PathBuf::new);

        let finalized = backend.stop_blocking(&path);

        // Late-failure catch: the encoder can clear the startup window yet still
        // fail to produce a file (it died mid-recording, or a downstream mux
        // error left nothing on disk). Don't hand back a path to nothing.
        // Checked before the finalize verdict because "no file at all" is the
        // more actionable of the two messages.
        if !path.as_os_str().is_empty() {
            match std::fs::metadata(&path) {
                Ok(m) if m.len() > 0 => {}
                Ok(_) => return Err(Error::Recording(format!(
                    "recording stopped but the saved file is empty: {} — the \
                     encoder likely failed mid-recording.", path.display()))),
                Err(_) => return Err(Error::Recording(format!(
                    "recording stopped but no file was written to {} — the \
                     encoder likely failed to start.", path.display()))),
            }
        }
        finalized?;
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
fn spawn_recorder(output: &Path, options: RecordingOptions, region: Option<CaptureRect>)
    -> Result<(Backend, RecordingOptions)>
{
    // Reject Wayland up-front — `-f x11grab` against XWayland silently
    // captures only XWayland clients (typically a black frame), and on
    // Wayland-only sessions DISPLAY may be unset entirely.
    if crate::session::is_wayland() {
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
    let mut opt = if pulse_ok { options } else { RecordingOptions::NONE };

    // System-audio source is the default sink's monitor (`<sink>.monitor`).
    // If we can't name one, drop system audio and keep the rest: video and a
    // requested mic are still perfectly recordable without it.
    let monitor_source: Option<String> = if opt.system_audio {
        match default_pulse_monitor_source() {
            Some(m) => Some(m),
            None    => { opt.system_audio = false; None }
        }
    } else { None };

    let args = build_linux_ffmpeg_args(&display, path, opt, monitor_source.as_deref(), region);
    let ffmpeg = locate_ffmpeg().unwrap_or_else(|| PathBuf::from("ffmpeg"));
    let res = Command::new(&ffmpeg)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();

    match res {
        Ok(c) => {
            let (child, stderr) = watch_recorder_startup(c)?;
            Ok((Backend::Process { child, stderr }, opt))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::Recording(
            "ffmpeg not found in PATH — install with: sudo apt install ffmpeg".into()
        )),
        Err(e) => Err(Error::Recording(format!("failed to spawn ffmpeg: {e}"))),
    }
}

/// Name the PulseAudio monitor source for the current default sink, i.e. the
/// pseudo-input that carries what the speakers are playing.
///
/// `pactl get-default-sink` can exit non-zero, or exit zero and print nothing,
/// when there's no server or no default sink. Both used to be folded into a
/// literal `".monitor"` source name, which ffmpeg can't open — and since a
/// failed input aborts the whole command, a missing sink took the video down
/// with it. `None` here means "record without system audio".
#[cfg(target_os = "linux")]
fn default_pulse_monitor_source() -> Option<String> {
    let out = Command::new("pactl")
        .arg("get-default-sink")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() { return None; }
    let sink = String::from_utf8(out.stdout).ok()?;
    let sink = sink.trim();
    if sink.is_empty() { return None; }
    Some(format!("{sink}.monitor"))
}

/// Build the ffmpeg argv for Linux X11 capture. Pure function so the test
/// suite can assert exact argv composition without spawning a process.
///
/// `region` limits the grab: x11grab takes the size as `-video_size` *before*
/// the input and the top-left corner as a `+X,Y` suffix on the display name
/// (`:0.0+120,80`). Those coordinates are measured from the X root window's
/// corner, which is the virtual desktop's corner — hence `offset_x/offset_y`
/// rather than the absolute fields.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn build_linux_ffmpeg_args(
    display:        &str,
    output_path:    &str,
    options:        RecordingOptions,
    monitor_source: Option<&str>,
    region:         Option<CaptureRect>,
) -> Vec<String> {
    let mut a: Vec<String> = Vec::with_capacity(36);
    let push = |a: &mut Vec<String>, s: &str| a.push(s.to_string());
    push(&mut a, "-y");
    push(&mut a, "-f"); push(&mut a, "x11grab");
    push(&mut a, "-framerate"); push(&mut a, "30");
    match region {
        Some(r) => {
            push(&mut a, "-video_size"); a.push(r.video_size());
            push(&mut a, "-i");
            a.push(format!("{display}+{},{}", r.offset_x(), r.offset_y()));
        }
        None => { push(&mut a, "-i"); push(&mut a, display); }
    }
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
fn spawn_recorder(output: &Path, options: RecordingOptions, region: Option<CaptureRect>)
    -> Result<(Backend, RecordingOptions)>
{
    let path = output.to_str().ok_or_else(||
        Error::Recording("non-UTF-8 output path".into()))?;

    // Video-only: keep the dependency-free built-in. stdin stays null, which
    // is how `graceful_signal` tells the two backends apart.
    if !options.has_audio() {
        let child = Command::new("screencapture")
            .args(build_macos_screencapture_args(path, region))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Recording(format!("failed to spawn screencapture: {e}")))?;
        let (child, stderr) = watch_recorder_startup(child)?;
        return Ok((Backend::Process { child, stderr, sck: None }, RecordingOptions::NONE));
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

    // What we're actually about to record: a requested mic that avfoundation
    // never listed isn't in the file, whatever the caller asked for.
    let effective = RecordingOptions {
        mic:          mic_idx.is_some(),
        system_audio: sck.is_some(),
    };

    let args = build_macos_ffmpeg_args(screen_idx, mic_idx, sck_port, path, region);
    let res = Command::new(&ffmpeg)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();

    match res {
        Ok(child) => match watch_recorder_startup(child) {
            Ok((child, stderr)) => Ok((Backend::Process { child, stderr, sck }, effective)),
            Err(e)              => { if let Some(s) = sck { s.stop(); } Err(e) }
        },
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

/// Build the `screencapture` argv for the dependency-free video-only path.
///
/// `-R x,y,w,h` limits the capture to a rectangle in macOS' global display
/// space, whose origin is the **main display's** top-left corner — the same
/// space `CaptureRect`'s absolute fields live in, so no translation is needed.
/// Pure function so the argv can be asserted without a Mac.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn build_macos_screencapture_args(
    output_path: &str,
    region:      Option<CaptureRect>,
) -> Vec<String> {
    let mut a: Vec<String> = Vec::with_capacity(4);
    a.push("-v".to_string());
    if let Some(r) = region {
        // `screencapture` parses its options with getopt, so the rectangle can
        // ride in its own argv slot rather than being glued to the flag.
        a.push("-R".to_string());
        a.push(format!("{},{},{},{}", r.x, r.y, r.width, r.height));
    }
    a.push(output_path.to_string());
    a
}

/// Build the ffmpeg argv for macOS: avfoundation video (+ optional fused mic)
/// as input 0, plus an optional ScreenCaptureKit system-audio TCP input. When
/// both mic and system audio are present they're `amix`ed to one stereo AAC
/// track. Pure function so the suite can assert argv shape without a Mac.
///
/// avfoundation has no capture-rectangle option, so a `region` becomes a
/// `crop` filter ahead of the usual even-dimension `pad`. Crop coordinates are
/// relative to the frame the screen device delivers — the main display — which
/// is also where macOS' global coordinate origin sits, so the absolute fields
/// go in unchanged. A region living entirely on a *secondary* display is
/// outside that frame; the video-only `screencapture` path has no such limit.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn build_macos_ffmpeg_args(
    screen_idx: usize,
    mic_idx:    Option<usize>,
    sck_port:   Option<u16>,
    output_path: &str,
    region:     Option<CaptureRect>,
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
    push(&mut a, "-vf"); a.push(video_filter_chain(region));
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

/// The `-vf` chain for the macOS avfoundation path: an optional `crop` for a
/// region, then the even-dimension `pad` every platform applies. `pad` stays
/// even when cropping — a `CaptureRect` is already even, so it is a no-op there
/// and a safety net for the full-screen case (an odd-sized display).
#[cfg(any(target_os = "macos", test))]
fn video_filter_chain(region: Option<CaptureRect>) -> String {
    const PAD: &str = "pad=ceil(iw/2)*2:ceil(ih/2)*2";
    match region {
        Some(r) => format!("crop={}:{}:{}:{},{PAD}", r.width, r.height, r.x, r.y),
        None    => PAD.to_string(),
    }
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
fn spawn_recorder(output: &Path, options: RecordingOptions, region: Option<CaptureRect>)
    -> Result<(Backend, RecordingOptions)>
{
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
    let args = build_windows_ffmpeg_args(path, &specs, region);

    // CREATE_NO_WINDOW (0x08000000): ffmpeg.exe is a console app, so without
    // this flag Windows allocates a visible black console window that stays up
    // for the whole recording. Suppress it.
    use std::os::windows::process::CommandExt;
    let res = Command::new(&ffmpeg)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000)
        .spawn();

    match res {
        Ok(child) => match watch_recorder_startup(child) {
            Ok((child, stderr)) => {
                let pumps = started.into_iter().map(|s| s.pump).collect();
                // Every requested source started or we'd have bailed above, so
                // the effective options are the requested ones.
                Ok((Backend::Process { child, stderr, pumps }, options))
            }
            Err(e) => {
                for mut s in started { s.pump.signal_stop(); s.pump.join(); }
                Err(e)
            }
        },
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
///
/// A `region` becomes `-offset_x` / `-offset_y` / `-video_size`, all of which
/// gdigrab reads *before* its `-i desktop`. gdigrab adds the offsets to
/// `SM_XVIRTUALSCREEN` / `SM_YVIRTUALSCREEN`, i.e. it measures from the virtual
/// desktop's corner — which is why this uses `offset_x()` and not `x`. On a
/// layout with a monitor left of the primary one those differ by 1920.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn build_windows_ffmpeg_args(
    output_path: &str,
    audio:       &[WasapiAudioSpec],
    region:      Option<CaptureRect>,
) -> Vec<String> {
    let mut a: Vec<String> = Vec::with_capacity(52);
    let push = |a: &mut Vec<String>, s: &str| a.push(s.to_string());
    push(&mut a, "-y");
    // Video: GDI grab of the desktop at 30 fps. `desktop` is gdigrab's
    // pseudo-device name for the full virtual screen; the offset/size options
    // below narrow it to the user's rectangle.
    push(&mut a, "-f"); push(&mut a, "gdigrab");
    push(&mut a, "-framerate"); push(&mut a, "30");
    if let Some(r) = region {
        push(&mut a, "-offset_x"); a.push(r.offset_x().to_string());
        push(&mut a, "-offset_y"); a.push(r.offset_y().to_string());
        push(&mut a, "-video_size"); a.push(r.video_size());
    }
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
fn spawn_recorder(_output: &Path,
                  _options: RecordingOptions,
                  _region:  Option<CaptureRect>)
    -> Result<(Backend, RecordingOptions)>
{
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

// ── startup-failure detection ────────────────────────────────────────────────

/// Watch a freshly-spawned recorder child for an *immediate* failure.
///
/// ffmpeg (and macOS `screencapture`) validate their inputs at startup: a
/// missing pulse demuxer, an unopenable capture device, a Wayland display, or a
/// malformed argv make the process exit within a few hundred ms — before a
/// single frame is written. We used to route stderr to `/dev/null` and treat a
/// successful *spawn* as a successful *recording*, so those failures produced a
/// cheery "recording started" toast and silently left no file behind.
///
/// Instead: poll for ~400 ms. If the child already died, hand back the tail of
/// its stderr as an actionable error. If it survived the window, keep draining
/// stderr to EOF on a detached thread — an unread pipe eventually fills and
/// stalls a long recording — but into a `StderrTail` ring rather than a sink,
/// so a *later* death (`Recorder::poll_health`, `Recorder::stop`) can still name
/// its cause. Caller must spawn with `stderr(Stdio::piped())` for either
/// diagnostic to work.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn watch_recorder_startup(mut child: Child) -> Result<(Child, StderrTail)> {
    use std::io::Read;
    let mut stderr = child.stderr.take();

    for _ in 0..8 {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut buf = String::new();
                if let Some(mut e) = stderr.take() { let _ = e.read_to_string(&mut buf); }
                return Err(Error::Recording(format!(
                    "recording failed to start — the encoder exited immediately ({status}).\n\n{}",
                    ffmpeg_error_tail(&buf)
                )));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_)   => break,
        }
    }

    let tail = StderrTail::new();
    if let Some(e) = stderr.take() {
        let sink = tail.clone();
        std::thread::spawn(move || drain_stderr_into(e, sink));
    }
    Ok((child, tail))
}

/// Read a recorder child's stderr to EOF, keeping the last few lines in `tail`.
///
/// Split on both `\n` and `\r`: ffmpeg overwrites its progress counter with a
/// carriage return, so a newline-only reader would treat an entire recording's
/// stats as one unbounded line and never see the error printed after it.
/// Over-long lines are clipped rather than grown without limit — nothing
/// actionable lives past a few hundred characters.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn drain_stderr_into(mut src: std::process::ChildStderr, tail: StderrTail) {
    use std::io::Read;
    const MAX_LINE: usize = 512;
    let mut buf  = [0u8; 8192];
    let mut line = Vec::with_capacity(MAX_LINE);
    loop {
        let n = match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                tail.push_line(&line);
                line.clear();
            } else if line.len() < MAX_LINE {
                line.push(b);
            }
        }
    }
    // Whatever the encoder printed without a trailing newline before dying is
    // usually the most interesting line of all.
    tail.push_line(&line);
}

/// Pull the most useful lines out of an encoder's startup stderr. The actionable
/// message ("Unknown input format: 'pulse'", "Error opening input files",
/// "Permission denied") is always near the end, after the banner + build config.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn ffmpeg_error_tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return "The encoder produced no diagnostics. Check that recording isn't \
                running on Wayland (X11 only for now), that the bundled ffmpeg \
                supports your audio backend, and that the capture device exists."
            .to_string();
    }
    let start = lines.len().saturating_sub(6);
    lines[start..].join("\n")
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use kashot_core::region::{record_rect_from_selection, DesktopBounds};

    /// A 1920x1080 desktop rooted at the origin — the single-monitor case.
    fn desktop() -> DesktopBounds { DesktopBounds::new(0, 0, 1920, 1080) }

    /// The rectangle a user would drag for "record this window", already
    /// clamped + even-sized the way the overlay hands it to the recorder.
    fn region() -> CaptureRect {
        record_rect_from_selection((320, 180, 640, 480), desktop()).unwrap()
    }

    // Linux argv-builder: assert the shape of the command we hand to ffmpeg
    // for every audio combination, without spawning anything.

    #[test]
    fn linux_argv_video_only() {
        let a = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", RecordingOptions::NONE, None, None);
        assert!(a.windows(2).any(|w| w == ["-f", "x11grab"]),
                "missing -f x11grab in: {:?}", a);
        assert!(a.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(!a.iter().any(|s| s == "pulse"), "pulse should be absent when audio off");
        assert!(!a.iter().any(|s| s == "-c:a"), "audio codec should be absent when audio off");
        assert_eq!(a.last().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn linux_argv_mic_only() {
        let a = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", RecordingOptions::MIC_ONLY, None, None);
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
            RecordingOptions::MIC_AND_SYS, Some("alsa_output.0.monitor"), None);
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
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4", &[], None);
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
            &[spec(54123, 48000, 2, "f32le")], None);
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
            &[spec(40001, 48000, 2, "f32le"), spec(40002, 44100, 1, "s16le")], None);
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
            &[spec(50000, 48000, 2, "f32le")], None);
        assert!(a.windows(2).any(|w| w == ["-thread_queue_size", "1024"]),
                "audio input should set -thread_queue_size: {:?}", a);
    }

    // macOS avfoundation: argv-builder, device-table parser, and picker.
    // Same `#[cfg(any(target_os = "macos", test))]` strategy so a Linux CI
    // agent catches regressions in the command we hand to ffmpeg on a Mac.

    #[test]
    fn macos_argv_mic_only_uses_avfoundation_fused_input() {
        // Mic, no system audio → just the fused avfoundation input, no TCP.
        let a = build_macos_ffmpeg_args(1, Some(0), None, "/tmp/out.mp4", None);
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
        let a = build_macos_ffmpeg_args(2, None, Some(50321), "/tmp/out.mp4", None);
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
        let a = build_macos_ffmpeg_args(1, Some(0), Some(40044), "/tmp/out.mp4", None);
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
        let a = build_macos_ffmpeg_args(2, None, None, "/tmp/out.mp4", None);
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

    // Region recording: the same rectangle has to reach three very different
    // capture stacks with the right coordinate convention on each, and the
    // audio wiring must not shift by a single argument.

    #[test]
    fn linux_argv_region_sets_video_size_and_display_offset() {
        let a = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", RecordingOptions::NONE,
                                        None, Some(region()));
        assert!(a.windows(2).any(|w| w == ["-video_size", "640x480"]),
                "region size must be declared before the input: {a:?}");
        // x11grab carries the corner as a +X,Y suffix on the display name.
        assert!(a.windows(2).any(|w| w == ["-i", ":0+320,180"]),
                "expected display+offset input: {a:?}");
        // -video_size only means anything ahead of the -i it applies to.
        let vs = a.iter().position(|s| s == "-video_size").unwrap();
        let i  = a.iter().position(|s| s == "-i").unwrap();
        assert!(vs < i, "-video_size must precede -i: {a:?}");
        assert_eq!(a.last().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn linux_argv_full_screen_has_no_region_flags() {
        let a = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", RecordingOptions::NONE, None, None);
        assert!(!a.iter().any(|s| s == "-video_size"), "no size for a full grab: {a:?}");
        assert!(a.windows(2).any(|w| w == ["-i", ":0"]), "bare display input: {a:?}");
    }

    #[test]
    fn linux_argv_region_leaves_audio_wiring_alone() {
        // The whole point of threading the rect through `start` rather than
        // patching the x11grab branch: audio must be byte-identical.
        let full = build_linux_ffmpeg_args(":0", "/tmp/o.mp4", RecordingOptions::MIC_AND_SYS,
                                           Some("alsa_output.0.monitor"), None);
        let reg  = build_linux_ffmpeg_args(":0", "/tmp/o.mp4", RecordingOptions::MIC_AND_SYS,
                                           Some("alsa_output.0.monitor"), Some(region()));
        let audio_of = |a: &[String]| -> Vec<String> {
            let start = a.iter().position(|s| s == "-c:v").unwrap();
            a[start..].to_vec()
        };
        assert_eq!(audio_of(&full), audio_of(&reg),
                   "everything from the video codec onward must match");
        assert!(reg.windows(2).any(|w| w == ["-i", "alsa_output.0.monitor"]));
    }

    #[test]
    fn linux_argv_region_offsets_from_the_desktop_corner() {
        // X11's root window starts at the desktop corner, so a desktop whose
        // origin is not (0,0) must not be addressed with absolute coordinates.
        let bounds = DesktopBounds::new(-1920, 0, 3840, 1080);
        let r = record_rect_from_selection((-1600, 200, 800, 600), bounds).unwrap();
        let a = build_linux_ffmpeg_args(":0.0", "/tmp/out.mp4", RecordingOptions::NONE,
                                        None, Some(r));
        assert!(a.windows(2).any(|w| w == ["-i", ":0.0+320,200"]),
                "offset must be relative to the desktop corner: {a:?}");
    }

    #[test]
    fn windows_argv_region_uses_gdigrab_offsets_before_the_input() {
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4", &[], Some(region()));
        assert!(a.windows(2).any(|w| w == ["-offset_x", "320"]), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["-offset_y", "180"]), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["-video_size", "640x480"]), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["-i", "desktop"]));
        let ox = a.iter().position(|s| s == "-offset_x").unwrap();
        let i  = a.iter().position(|s| s == "-i").unwrap();
        assert!(ox < i, "gdigrab reads its offsets before -i: {a:?}");
    }

    #[test]
    fn windows_argv_region_offsets_from_the_virtual_screen_corner() {
        // gdigrab adds -offset_x to SM_XVIRTUALSCREEN, which is negative when a
        // monitor sits left of the primary. Absolute coordinates would land the
        // capture 1920 px to the right of what the user selected.
        let bounds = DesktopBounds::new(-1920, -120, 3840, 1200);
        let r = record_rect_from_selection((-1000, 0, 500, 400), bounds).unwrap();
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4", &[], Some(r));
        assert!(a.windows(2).any(|w| w == ["-offset_x", "920"]), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["-offset_y", "120"]), "{a:?}");
    }

    #[test]
    fn windows_argv_region_leaves_audio_wiring_alone() {
        let specs = [spec(40001, 48000, 2, "f32le"), spec(40002, 44100, 1, "s16le")];
        let full = build_windows_ffmpeg_args("C:/o.mp4", &specs, None);
        let reg  = build_windows_ffmpeg_args("C:/o.mp4", &specs, Some(region()));
        let tail = |a: &[String]| -> Vec<String> {
            let start = a.iter().position(|s| s == "-c:v").unwrap();
            a[start..].to_vec()
        };
        assert_eq!(tail(&full), tail(&reg));
        // Audio inputs keep their indices — the region adds no new input.
        assert!(reg.iter().any(|s| s == "tcp://127.0.0.1:40001"));
        assert!(reg.iter().any(|s| s == "tcp://127.0.0.1:40002"));
    }

    #[test]
    fn windows_argv_full_screen_has_no_offsets() {
        let a = build_windows_ffmpeg_args("C:/tmp/out.mp4", &[], None);
        assert!(!a.iter().any(|s| s == "-offset_x"), "{a:?}");
        assert!(!a.iter().any(|s| s == "-video_size"), "{a:?}");
    }

    #[test]
    fn macos_screencapture_argv_region_passes_absolute_rect() {
        // screencapture measures from the main display's corner, which is the
        // origin of the absolute coordinates — no translation.
        let a = build_macos_screencapture_args("/tmp/out.mp4", Some(region()));
        assert_eq!(a, vec!["-v", "-R", "320,180,640,480", "/tmp/out.mp4"]);
    }

    #[test]
    fn macos_screencapture_argv_full_screen_is_unchanged() {
        let a = build_macos_screencapture_args("/tmp/out.mp4", None);
        assert_eq!(a, vec!["-v", "/tmp/out.mp4"]);
    }

    #[test]
    fn macos_argv_region_crops_before_padding() {
        // avfoundation has no capture rectangle, so the region is a filter.
        let a = build_macos_ffmpeg_args(1, Some(0), None, "/tmp/out.mp4", Some(region()));
        let vf = a.iter().position(|s| s == "-vf").expect("missing -vf");
        assert_eq!(a[vf + 1], "crop=640:480:320:180,pad=ceil(iw/2)*2:ceil(ih/2)*2");
        // Audio mapping is untouched by the crop.
        assert!(a.windows(2).any(|w| w == ["-map", "0:a"]));
    }

    #[test]
    fn macos_argv_full_screen_keeps_the_bare_pad_filter() {
        let a = build_macos_ffmpeg_args(1, None, None, "/tmp/out.mp4", None);
        let vf = a.iter().position(|s| s == "-vf").expect("missing -vf");
        assert_eq!(a[vf + 1], "pad=ceil(iw/2)*2:ceil(ih/2)*2");
    }

    #[test]
    fn every_backend_receives_even_dimensions() {
        // The encoder-facing invariant, asserted where it actually reaches an
        // encoder: an odd drag must never produce an odd -video_size / crop.
        let r = record_rect_from_selection((11, 23, 401, 303), desktop()).unwrap();
        assert_eq!(r.video_size(), "400x302");
        let lin = build_linux_ffmpeg_args(":0", "/o.mp4", RecordingOptions::NONE, None, Some(r));
        assert!(lin.windows(2).any(|w| w == ["-video_size", "400x302"]));
        let win = build_windows_ffmpeg_args("C:/o.mp4", &[], Some(r));
        assert!(win.windows(2).any(|w| w == ["-video_size", "400x302"]));
        let mac = build_macos_screencapture_args("/o.mp4", Some(r));
        assert!(mac.iter().any(|s| s == "11,23,400,302"), "{mac:?}");
        let vf = video_filter_chain(Some(r));
        assert!(vf.starts_with("crop=400:302:"), "{vf}");
    }

    // stderr ring buffer: what turns "recording stopped unexpectedly" into a
    // message that names the cause.

    #[test]
    fn stderr_tail_keeps_only_the_last_lines() {
        let t = StderrTail::new();
        for i in 0..(STDERR_TAIL_LINES + 20) {
            t.push_line(format!("line {i}").as_bytes());
        }
        let text = t.text();
        assert_eq!(text.lines().count(), STDERR_TAIL_LINES);
        assert!(text.contains(&format!("line {}", STDERR_TAIL_LINES + 19)),
                "newest line must survive: {text}");
        assert!(!text.contains("line 0\n"), "oldest line must be evicted: {text}");
    }

    #[test]
    fn stderr_tail_skips_progress_and_blank_lines() {
        // ffmpeg emits one of these per second; sixteen of them would push the
        // real diagnostic out of the window before anyone reads it.
        let t = StderrTail::new();
        t.push_line(b"No space left on device");
        for _ in 0..40 {
            t.push_line(b"frame= 1234 fps= 30 q=28.0 size=   65536kB");
            t.push_line(b"   ");
        }
        assert_eq!(t.text(), "No space left on device");
    }

    #[test]
    fn stderr_tail_reduces_output_to_printable_ascii() {
        // The bitmap font can't render anything else, and control bytes would
        // wreck the toast layout.
        let t = StderrTail::new();
        t.push_line("caf\u{e9}.mp4: \u{7} Invalid\targument".as_bytes());
        assert_eq!(t.text(), "caf?.mp4: ? Invalid?argument");
        assert!(t.text().is_ascii());
    }

    #[test]
    fn stderr_tail_error_suffix_is_empty_without_diagnostics() {
        let t = StderrTail::new();
        assert_eq!(t.as_error_suffix(), "");
        t.push_line(b"Device or resource busy");
        assert!(t.as_error_suffix().starts_with("\n\n"));
        assert!(t.as_error_suffix().contains("Device or resource busy"));
    }

    // Effective-options accounting: a caller that toasts what it asked for
    // instead of what it got promises audio tracks that aren't in the file.

    #[test]
    fn recording_options_none_when_no_audio_requested() {
        assert!(!RecordingOptions::NONE.has_audio());
        assert!(RecordingOptions::MIC_ONLY.has_audio());
        assert!(RecordingOptions::SYSTEM_ONLY.has_audio());
        assert!(RecordingOptions::MIC_AND_SYS.has_audio());
    }

    #[test]
    fn dropping_system_audio_keeps_the_mic() {
        // Mirrors the Linux path when `pactl get-default-sink` names nothing:
        // system audio goes, the mic and video stay.
        let mut opt = RecordingOptions::MIC_AND_SYS;
        opt.system_audio = false;
        assert_eq!(opt, RecordingOptions::MIC_ONLY);
        let a = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", opt, None, None);
        assert!(a.windows(2).any(|w| w == ["-i", "default"]), "mic must survive: {a:?}");
        assert!(!a.iter().any(|s| s.ends_with(".monitor")),
                "no monitor source should be passed: {a:?}");
        assert!(!a.iter().any(|s| s == "-filter_complex"),
                "single source needs no amix: {a:?}");
    }
}
