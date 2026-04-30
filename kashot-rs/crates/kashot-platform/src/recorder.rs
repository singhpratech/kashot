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
    pub fn start(&mut self, output: PathBuf) -> Result<()> {
        if self.is_recording() {
            return Err(Error::Recording("a recording is already in progress".into()));
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let child = spawn_recorder(&output)?;
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
fn spawn_recorder(output: &Path) -> Result<Child> {
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into());
    let path = output.to_str().ok_or_else(||
        Error::Recording("non-UTF-8 output path".into()))?;

    let res = Command::new("ffmpeg")
        .args([
            "-y",                   // overwrite existing output
            "-f", "x11grab",
            "-framerate", "30",
            "-i", &display,
            "-c:v", "libx264",
            "-preset", "ultrafast",
            "-pix_fmt", "yuv420p",
            path,
        ])
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
fn spawn_recorder(output: &Path) -> Result<Child> {
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
fn spawn_recorder(_output: &Path) -> Result<Child> {
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
