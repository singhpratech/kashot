//! Single-instance guard, and the one message a rejected instance can send.
//!
//! KAShot is tray-resident, so a second launch — a double-click on the
//! desktop icon, a second autostart entry, a `kashot` typed into a terminal —
//! used to add a *second* tray icon, a second global-hotkey registration
//! (which the OS refuses, so the newcomer's hotkey silently does nothing) and
//! a second recorder that fights the first over the same output filenames.
//!
//! The guard is an advisory lock held on a file in the per-app config
//! directory for as long as the process lives. Both mechanisms below are
//! kernel-owned, so the lock is released by process exit *including a crash
//! or a SIGKILL* — no stale-PID heuristics, and no lock left behind by an
//! unclean shutdown:
//!
//! * Unix    — `flock(LOCK_EX | LOCK_NB)` on an open fd.
//! * Windows — `CreateFileW` with a share mode of 0, i.e. an exclusive open.
//!             A second opener gets a sharing violation.
//!
//! Losing the race is not an error. The second instance leaves a capture
//! request next to the lock file and exits quietly; the running instance
//! picks it up on its next event-loop tick and opens the capture overlay. So
//! "launch KAShot again" does the useful thing instead of nothing.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use kashot_core::AppSettings;

/// Name of the lock file inside the per-app config directory.
const LOCK_FILE: &str = "instance.lock";

/// Name of the "the user launched me again — please capture" flag file.
const CAPTURE_REQUEST_FILE: &str = "capture.request";

/// Held for the lifetime of the primary instance. Dropping it releases the
/// lock, so it must be kept alive in `main` for as long as the app runs.
pub struct InstanceLock {
    /// The lock lives on this open handle. Never read or written — closing it
    /// is the entire contract.
    _file: File,
}

/// Outcome of trying to become the one running KAShot.
pub enum Instance {
    /// We hold the lock. Keep the guard alive for the whole process.
    Primary(InstanceLock),
    /// Another KAShot already holds it. The caller should exit.
    AlreadyRunning,
    /// There is no per-app directory to lock in (no HOME, read-only config
    /// root, …). We can't tell, so we let the launch through — refusing to
    /// start is much worse than a duplicate tray icon.
    Unsupported,
}

fn lock_path() -> Option<PathBuf> {
    AppSettings::config_dir().map(|d| d.join(LOCK_FILE))
}

fn capture_request_path() -> Option<PathBuf> {
    AppSettings::config_dir().map(|d| d.join(CAPTURE_REQUEST_FILE))
}

/// Try to become the primary instance, answering immediately.
///
/// On success any capture request left over from a previous run is cleared:
/// it belongs to a conversation with an instance that is no longer around,
/// and consuming it would open an overlay the user never asked for.
pub fn acquire() -> Instance {
    acquire_waiting(std::time::Duration::ZERO)
}

/// Try to become the primary instance, retrying until `grace` elapses.
///
/// A zero grace is the plain "is anyone else running?" question. A non-zero
/// one is for the single case where an overlap is expected and legitimate:
/// the self-updater spawns the freshly-installed binary and *then* exits, so
/// for a moment two KAShots are alive by design. Without the wait the new
/// process would mistake its own predecessor for a duplicate launch and quit,
/// leaving the user with no KAShot at all after an update.
pub fn acquire_waiting(grace: std::time::Duration) -> Instance {
    let deadline = std::time::Instant::now() + grace;
    loop {
        match acquire_once() {
            Instance::AlreadyRunning if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

fn acquire_once() -> Instance {
    let Some(path) = lock_path() else { return Instance::Unsupported };
    if let Some(dir) = path.parent() {
        if fs::create_dir_all(dir).is_err() {
            return Instance::Unsupported;
        }
    }

    let file = match open_lock_file(&path) {
        Ok(f) => f,
        // Windows reports the *contended* case here too, because the
        // exclusive open below is the open itself. Everything else (no
        // permission, read-only filesystem) is "we can't tell".
        Err(e) => return classify_open_error(&e),
    };

    match try_lock(&file) {
        LockOutcome::Held => {
            if let Some(req) = capture_request_path() { let _ = fs::remove_file(req); }
            Instance::Primary(InstanceLock { _file: file })
        }
        LockOutcome::Contended => Instance::AlreadyRunning,
        LockOutcome::Unsupported => Instance::Unsupported,
    }
}

/// Ask the running instance to open the capture overlay.
///
/// Best-effort and fire-and-forget: this is the last thing a losing instance
/// does before exiting, and a failure here just means the extra launch was a
/// no-op — which is the behaviour we had before the guard existed anyway.
pub fn request_capture() {
    let Some(path) = capture_request_path() else { return };
    if let Some(dir) = path.parent() { let _ = fs::create_dir_all(dir); }
    let _ = fs::write(&path, b"capture\n");
}

/// Consume a pending capture request, if one is waiting.
///
/// Cheap enough to call from the event loop every tick: on the overwhelmingly
/// common path it is a single `unlink` that fails with `ENOENT`. Removing the
/// file *is* the claim, so two ticks can never both act on one request.
pub fn take_capture_request() -> bool {
    let Some(path) = capture_request_path() else { return false };
    fs::remove_file(path).is_ok()
}

/// Result of the platform's "claim this file" step.
///
/// Only the Unix path produces all three: on Windows the exclusive open is
/// itself the lock, so contention has already been decided by the time
/// `try_lock` runs and `Held` is the only value it can return.
#[cfg_attr(not(unix), allow(dead_code))]
enum LockOutcome {
    Held,
    Contended,
    Unsupported,
}

// ── Unix: flock on the open fd ──────────────────────────────────────────────

#[cfg(unix)]
fn try_lock(file: &File) -> LockOutcome {
    use std::os::unix::io::AsRawFd;
    // LOCK_NB so we get an immediate answer instead of blocking behind the
    // running instance forever.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 { return LockOutcome::Held; }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        // EWOULDBLOCK (== EAGAIN on every platform we ship) is the one
        // outcome that actually means "someone else has it".
        Some(e) if e == libc::EWOULDBLOCK => LockOutcome::Contended,
        // flock is unimplemented on some network filesystems. Don't turn that
        // into a refusal to start.
        _ => LockOutcome::Unsupported,
    }
}

#[cfg(unix)]
fn classify_open_error(_e: &std::io::Error) -> Instance {
    // On Unix the open always succeeds when the path is usable; contention is
    // decided by flock afterwards. Anything failing here is environmental.
    Instance::Unsupported
}

// ── Windows: the exclusive open is the lock ─────────────────────────────────

#[cfg(windows)]
fn try_lock(_file: &File) -> LockOutcome {
    // Nothing more to do: `open_exclusive` below already succeeded, and it
    // could only do that if no other process had the file open.
    LockOutcome::Held
}

#[cfg(windows)]
fn classify_open_error(e: &std::io::Error) -> Instance {
    // ERROR_SHARING_VIOLATION (32) — another process holds the file with a
    // share mode that excludes us, which for this file means another KAShot.
    // ERROR_ACCESS_DENIED (5) can surface for the same reason on some
    // filesystems, so treat it the same way rather than starting a duplicate.
    match e.raw_os_error() {
        Some(32) | Some(5) => Instance::AlreadyRunning,
        _ => Instance::Unsupported,
    }
}

// ── neither: no guard, but never a refusal to start ─────────────────────────

#[cfg(not(any(unix, windows)))]
fn try_lock(_file: &File) -> LockOutcome { LockOutcome::Unsupported }

#[cfg(not(any(unix, windows)))]
fn classify_open_error(_e: &std::io::Error) -> Instance { Instance::Unsupported }

/// Open the lock file with the platform's exclusive sharing mode.
///
/// Split out from `acquire` so the Windows share-mode flag has somewhere to
/// live: on Windows the sharing mode *is* the lock, so it has to be part of
/// the open call itself.
#[cfg(windows)]
fn open_lock_file(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        // dwShareMode = 0: no other process may open this file at all until
        // our handle closes, which the kernel does for us even on a crash.
        .share_mode(0)
        .open(path)
}

#[cfg(not(windows))]
fn open_lock_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("kashot-instance-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// The lock is exclusive against a second handle on the same file, and
    /// released when the first handle is dropped. Exercised directly rather
    /// than through `acquire`, which is tied to the real config directory.
    #[cfg(unix)]
    #[test]
    fn second_holder_is_refused_until_the_first_drops() {
        let dir = scratch("exclusive");
        let path = dir.join(LOCK_FILE);

        let first = open_lock_file(&path).unwrap();
        assert!(matches!(try_lock(&first), LockOutcome::Held));

        let second = open_lock_file(&path).unwrap();
        assert!(matches!(try_lock(&second), LockOutcome::Contended),
            "a second holder must not get the lock");
        drop(second);

        drop(first);
        let third = open_lock_file(&path).unwrap();
        assert!(matches!(try_lock(&third), LockOutcome::Held),
            "closing the holder must release the lock");
        drop(third);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Opening the lock file must never truncate it — the file is a lock
    /// token, and a truncating open would be a write to a file another
    /// process is mid-flight on.
    #[test]
    fn opening_the_lock_file_preserves_it() {
        let dir = scratch("notruncate");
        let path = dir.join(LOCK_FILE);
        fs::write(&path, b"marker").unwrap();
        let f = open_lock_file(&path).unwrap();
        drop(f);
        assert_eq!(fs::read(&path).unwrap(), b"marker");
        let _ = fs::remove_dir_all(&dir);
    }

    /// `take_capture_request` claims by removing, so exactly one caller can
    /// win a given request. Modelled on the real files, in a scratch dir.
    #[test]
    fn a_capture_request_is_claimed_exactly_once() {
        let dir = scratch("request");
        let path = dir.join(CAPTURE_REQUEST_FILE);
        fs::write(&path, b"capture\n").unwrap();
        assert!(fs::remove_file(&path).is_ok(), "first claim wins");
        assert!(fs::remove_file(&path).is_err(), "second claim finds nothing");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The two files must be distinct: consuming a capture request must never
    /// unlink the lock file out from under the running instance.
    #[test]
    fn lock_and_request_are_different_files() {
        assert_ne!(LOCK_FILE, CAPTURE_REQUEST_FILE);
    }
}
