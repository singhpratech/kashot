//! Screen recording via system tools.
//!
//! Cross-platform recording isn't a one-binary affair — every OS has its own
//! capture stack and the high-quality choices live outside the Rust ecosystem:
//!
//! * Linux  (X11)     : `ffmpeg -f x11grab` — needs `ffmpeg` installed.
//! * macOS            : built-in `screencapture -v`.
//! * Windows          : the C# build uses ScreenRecorderLib (see
//!                      `Kashot/ScreenRecorder.cs`) which is the canonical
//!                      Windows path. The Rust shim here returns "not
//!                      supported" on Windows so we don't ship a half-working
//!                      duplicate.
//!
//! Wayland: not supported here yet — proper screen capture on Wayland goes
//! through `xdg-desktop-portal` (PipeWire), which is a substantial integration
//! and queued separately.
//!
//! Stop is graceful per platform: write `q` to `ffmpeg`'s stdin (Linux) or
//! send SIGINT to `screencapture` (macOS) so the MP4 moov atom is finalized.
//! `Drop` falls back to `child.kill()` — the file may be unplayable in that
//! case but the process won't leak.

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
            // Best-effort graceful first; SIGKILL if the process doesn't
            // exit promptly. We can't sit forever in Drop so the wait is
            // whatever the OS does with kill().
            graceful_signal(&mut c);
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

// ── platform spawn / signal ─────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn spawn_recorder(output: &Path, options: RecordingOptions) -> Result<Child> {
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
    let monitor_source = if opt.system_audio {
        Command::new("pactl")
            .arg("get-default-sink")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| format!("{}.monitor", s.trim()))
    } else { None };

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
       .args(["-f", "x11grab", "-framerate", "30", "-i", &display]);
    // Optional mic input as the second stream (index 1).
    if opt.mic {
        cmd.args(["-f", "pulse", "-i", "default"]);
    }
    // Optional system-audio input. If mic is also set, this becomes
    // stream index 2; otherwise index 1.
    if let Some(monitor) = monitor_source.as_deref() {
        cmd.args(["-f", "pulse", "-i", monitor]);
    }
    cmd.args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
       // x264 + yuv420p require even dimensions; common RDP / odd laptop
       // panels otherwise hit "height not divisible by 2" and abort.
       .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"]);
    match (opt.mic, monitor_source.as_deref()) {
        (true, Some(_)) => {
            // Mix mic + system into one stereo track. Stream 1 = mic, 2 = sys.
            cmd.args(["-filter_complex", "[1:a][2:a]amix=inputs=2:duration=longest:dropout_transition=0[aout]"])
               .args(["-map", "0:v", "-map", "[aout]"])
               .args(["-c:a", "aac", "-b:a", "160k"]);
        }
        (true, None) | (false, Some(_)) => {
            cmd.args(["-c:a", "aac", "-b:a", "160k"]);
        }
        (false, None) => {
            // Pure video.
        }
    }
    cmd.arg(path);

    let res = cmd
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

#[cfg(target_os = "macos")]
fn spawn_recorder(output: &Path, options: RecordingOptions) -> Result<Child> {
    let _ = options; // screencapture has limited audio control; ignore for now
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn spawn_recorder(_output: &Path, _options: RecordingOptions) -> Result<Child> {
    Err(Error::Recording(
        "recording is not supported on this platform — \
         the Windows MSI uses the C# ScreenRecorderLib build".into()))
}

/// Send the platform-appropriate "please finish gracefully" signal so the
/// container is finalized before the process exits.
#[cfg(target_os = "linux")]
fn graceful_signal(child: &mut Child) {
    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        // ffmpeg interprets 'q' on stdin as "stop and finalize".
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn graceful_signal(_child: &mut Child) {}
