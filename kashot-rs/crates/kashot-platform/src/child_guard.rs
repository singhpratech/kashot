//! Make the recorder child process die with KAShot, on every platform.
//!
//! A screen recording is a long-lived `ffmpeg` (or `screencapture`) child.
//! Nothing tied its lifetime to ours: if KAShot crashed, was `kill -9`d, or
//! was killed by the session on logout, the encoder kept running — forever,
//! holding the camera or the display grab, filling the disk with a clip
//! nobody would ever stop, and with no UI left to stop it from. The user's
//! only recourse was Task Manager / `pkill`.
//!
//! Two layers, because no single mechanism covers all three OSes:
//!
//! 1. **Die-with-parent, at spawn time.** Kernel-enforced where the kernel
//!    offers it:
//!    * Linux   — `prctl(PR_SET_PDEATHSIG, SIGTERM)` in `pre_exec`, so the
//!                kernel signals the child the moment we die. ffmpeg handles
//!                SIGTERM by finalizing the container, so the clip is usually
//!                still playable.
//!    * Windows — a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
//!                Our handle to the job is the only one; the kernel closes it
//!                when our process ends, however it ends, and terminates
//!                every process in the job.
//!    * macOS   — has no pdeathsig and no job objects, so a detached
//!                `/bin/sh` watchdog polls both pids and interrupts the
//!                encoder once we're gone.
//!
//! 2. **Reap-on-next-launch.** The live recorder's pid is written to a file
//!    next to the settings and removed when the recording ends. If a record
//!    is still there at startup, the previous run didn't get to clean up:
//!    verify the pid still belongs to a recorder we know (never blind-kill a
//!    recycled pid), stop it, and drop the record. This is the backstop for
//!    the cases layer 1 can't reach — a watchdog that was itself killed, a
//!    machine that lost power mid-recording — and it is what makes the macOS
//!    story equivalent to the other two rather than merely similar.

use std::path::PathBuf;
use std::process::{Child, Command};

use kashot_core::AppSettings;

/// Name of the live-recorder record inside the per-app config directory.
const PIDFILE: &str = "recorder.pid";

/// Recorder programs we are willing to kill during a reap. A pid whose image
/// isn't one of these has been recycled by the OS and belongs to someone
/// else — killing it would be far worse than leaking an encoder.
const RECORDER_IMAGES: [&str; 3] = ["ffmpeg", "ffmpeg.exe", "screencapture"];

fn pidfile_path() -> Option<PathBuf> {
    AppSettings::config_dir().map(|d| d.join(PIDFILE))
}

/// Spawn a recorder child that won't outlive us.
///
/// Takes the `Command` already fully configured by the caller (argv, stdio):
/// this only adds the die-with-parent plumbing, so the recorder module keeps
/// owning every decision about *what* to run.
pub(crate) fn spawn_guarded(cmd: &mut Command) -> std::io::Result<Child> {
    let image = program_basename(cmd);
    prepare(cmd);
    let child = cmd.spawn()?;
    adopt(&child);
    record_live(child.id(), &image);
    Ok(child)
}

/// Forget the live-recorder record. Called from every recorder teardown path,
/// so a record left behind can only mean the process died without one.
pub(crate) fn clear_live() {
    if let Some(p) = pidfile_path() {
        let _ = std::fs::remove_file(p);
    }
}

fn record_live(pid: u32, image: &str) {
    let Some(path) = pidfile_path() else { return };
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    let _ = kashot_core::atomic_file::write_atomic(&path, format_record(pid, image).as_bytes());
}

/// Basename of the program a `Command` will run, lowercased so the Windows
/// image-name comparison isn't case-sensitive.
fn program_basename(cmd: &Command) -> String {
    std::path::Path::new(cmd.get_program())
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

// ── pidfile format (pure, so it can be tested anywhere) ─────────────────────

/// `<pid>\n<image basename>\n`. Deliberately not JSON: kashot-platform has no
/// serde dependency, and two lines need none.
fn format_record(pid: u32, image: &str) -> String {
    format!("{pid}\n{image}\n")
}

/// Parse a record written by [`format_record`]. `None` for anything we don't
/// fully understand — a partially-written or hand-edited file must not send
/// us hunting for a pid to kill.
fn parse_record(text: &str) -> Option<(u32, String)> {
    let mut lines = text.lines();
    let pid: u32 = lines.next()?.trim().parse().ok()?;
    if pid == 0 { return None; }
    let image = lines.next()?.trim().to_ascii_lowercase();
    if image.is_empty() { return None; }
    Some((pid, image))
}

// ── reap ────────────────────────────────────────────────────────────────────

/// Stop a recorder left running by a previous KAShot, if there is one.
///
/// Returns the pid it stopped, for logging. Safe to call unconditionally at
/// startup; the common case is one failed `read` of a file that isn't there.
///
/// Only ever called by the instance that owns the single-instance lock, so it
/// can't race a *live* sibling's recording.
pub fn reap_orphaned_recorder() -> Option<u32> {
    let path = pidfile_path()?;
    let text = std::fs::read_to_string(&path).ok();
    // Drop the record either way: it describes a session that is over, and a
    // record we couldn't act on must not be retried on every future launch.
    let _ = std::fs::remove_file(&path);

    let (pid, image) = parse_record(&text?)?;
    if !RECORDER_IMAGES.contains(&image.as_str()) { return None; }
    if terminate_if_matching(pid, &image) { Some(pid) } else { None }
}

/// Stop `pid` if it is still alive *and* still running `image`. The image
/// check is what makes this safe against pid reuse.
#[cfg(unix)]
fn terminate_if_matching(pid: u32, image: &str) -> bool {
    if running_image(pid).as_deref() != Some(image) { return false; }

    let raw = pid as libc::pid_t;
    // SIGINT rather than SIGKILL: both ffmpeg and screencapture treat it as
    // "stop and finalize", so an orphan reaped here usually leaves a playable
    // clip behind instead of a truncated one.
    unsafe { libc::kill(raw, libc::SIGINT) };
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if unsafe { libc::kill(raw, 0) } != 0 { return true; }
    }
    // Wouldn't go quietly. It has had a second; the point of this function is
    // that nothing survives it.
    unsafe { libc::kill(raw, libc::SIGKILL) };
    true
}

/// Image basename of a running process, lowercased, or `None` if the pid is
/// dead or unreadable.
#[cfg(target_os = "linux")]
fn running_image(pid: u32) -> Option<String> {
    // `comm` is the kernel's own record and needs no subprocess. It is capped
    // at 15 characters, which comfortably fits every name in RECORDER_IMAGES.
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim().to_ascii_lowercase())
}

#[cfg(target_os = "macos")]
fn running_image(pid: u32) -> Option<String> {
    // No /proc on macOS. `ps -o comm=` prints the executable path with no
    // header; an exited pid prints nothing and exits non-zero.
    let out = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() { return None; }
    let path = String::from_utf8(out.stdout).ok()?;
    let path = path.trim();
    if path.is_empty() { return None; }
    Some(std::path::Path::new(path).file_name()?.to_string_lossy().to_ascii_lowercase())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn running_image(_pid: u32) -> Option<String> { None }

#[cfg(windows)]
fn terminate_if_matching(pid: u32, image: &str) -> bool {
    // One command does both halves: the two filters mean taskkill only acts
    // when that pid is still running that image, so pid reuse can't turn this
    // into a kill of an unrelated process. There is no graceful stop for a
    // console-less child on Windows, so an orphan reaped here leaves an
    // unfinalized file — the job object above is what normally prevents the
    // orphan in the first place.
    use std::os::windows::process::CommandExt;
    let status = Command::new("taskkill")
        .args([
            "/F",
            "/FI", &format!("PID eq {pid}"),
            "/FI", &format!("IMAGENAME eq {image}"),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // CREATE_NO_WINDOW — taskkill is a console app and would otherwise
        // flash a black window on every launch that reaps something.
        .creation_flags(0x0800_0000)
        .status();
    matches!(status, Ok(s) if s.success())
}

#[cfg(not(any(unix, windows)))]
fn terminate_if_matching(_pid: u32, _image: &str) -> bool { false }

// ── layer 1: die with the parent ────────────────────────────────────────────

/// Linux: ask the kernel to signal the child when we die.
#[cfg(target_os = "linux")]
fn prepare(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: the closure runs between fork and exec, where only
    // async-signal-safe calls are allowed. prctl, getppid and _exit all are.
    unsafe {
        cmd.pre_exec(move || {
            // The second prctl argument is read as an unsigned long, so the
            // signal number has to be widened explicitly — passing a bare
            // c_int through the variadic leaves the top half undefined.
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong);
            // Close the fork/prctl race: if the parent died in that window the
            // death signal has already been and gone, and would never fire.
            if libc::getppid() != parent {
                libc::_exit(0);
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn prepare(_cmd: &mut Command) {}

/// Windows: put the child in a job that dies with our process.
#[cfg(windows)]
fn adopt(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::AssignProcessToJobObject;

    let Some(job) = kill_on_close_job() else { return };
    // SAFETY: both handles are live — ours is the leaked job handle, the
    // child's is owned by `child` for the duration of this call.
    unsafe {
        let _ = AssignProcessToJobObject(
            HANDLE(job as *mut core::ffi::c_void),
            HANDLE(child.as_raw_handle() as *mut core::ffi::c_void),
        );
    }
}

/// The process-wide job, created on first use and then deliberately leaked:
/// the handle staying open for our whole lifetime is the mechanism. When the
/// process ends — cleanly, by crash, or by Task Manager — the kernel closes
/// it and terminates everything in the job.
#[cfg(windows)]
fn kill_on_close_job() -> Option<usize> {
    use std::sync::OnceLock;
    use windows::core::PCWSTR;
    use windows::Win32::System::JobObjects::{
        CreateJobObjectW, SetInformationJobObject, JobObjectExtendedLimitInformation,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    static JOB: OnceLock<Option<usize>> = OnceLock::new();
    *JOB.get_or_init(|| unsafe {
        // Unnamed: a named job would be shared with any other process that
        // guessed the name, and we want exactly our own children in it.
        let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            core::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .ok()?;
        Some(job.0 as usize)
    })
}

/// macOS: no pdeathsig, no job objects. Leave a tiny detached shell watching
/// both pids; it interrupts the encoder as soon as we're gone.
#[cfg(target_os = "macos")]
fn adopt(child: &Child) {
    let script = watchdog_script(std::process::id(), child.id());
    // The outer shell backgrounds the loop and exits immediately, so `wait`
    // returns at once and reaps it — no zombie per recording — while the
    // subshell is reparented to launchd and keeps watching.
    let _ = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut c| c.wait());
}

#[cfg(not(any(windows, target_os = "macos")))]
fn adopt(_child: &Child) {}

/// Body of the macOS watchdog. Pure, so its shape can be asserted on any host.
///
/// Polls once a second until either process is gone, then — only if it was the
/// *parent* that went — interrupts the encoder so it finalizes the container,
/// and hard-kills it if it hasn't exited a few seconds later. When the encoder
/// exits first (the normal case) the loop simply ends and the watchdog does
/// nothing.
#[cfg(any(target_os = "macos", test))]
fn watchdog_script(parent: u32, child: u32) -> String {
    format!(
        "( while /bin/kill -0 {parent} 2>/dev/null && /bin/kill -0 {child} 2>/dev/null; \
         do sleep 1; done; \
         /bin/kill -0 {parent} 2>/dev/null || \
         {{ /bin/kill -INT {child} 2>/dev/null; sleep 5; /bin/kill -9 {child} 2>/dev/null; }} \
         ) >/dev/null 2>&1 &"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trips() {
        let text = format_record(4321, "ffmpeg");
        assert_eq!(parse_record(&text), Some((4321, "ffmpeg".to_owned())));
    }

    #[test]
    fn record_is_case_insensitive_on_the_image() {
        let text = format_record(7, "FFmpeg.EXE");
        assert_eq!(parse_record(&text), Some((7, "ffmpeg.exe".to_owned())));
    }

    /// A truncated or garbled record must parse to nothing rather than to a
    /// pid we would then go and signal.
    #[test]
    fn malformed_records_are_rejected() {
        for text in ["", "\n", "notapid\nffmpeg\n", "1234", "1234\n", "1234\n\n", "0\nffmpeg\n"] {
            assert_eq!(parse_record(text), None, "should reject {text:?}");
        }
    }

    /// Only recorder images may be killed — pid reuse would otherwise let a
    /// stale record point at an unrelated process.
    #[test]
    fn only_known_recorder_images_are_reapable() {
        for ok in ["ffmpeg", "ffmpeg.exe", "screencapture"] {
            assert!(RECORDER_IMAGES.contains(&ok));
        }
        for bad in ["bash", "explorer.exe", "kashot", ""] {
            assert!(!RECORDER_IMAGES.contains(&bad), "{bad} must not be reapable");
        }
    }

    /// Every image a recorder is actually spawned with has to be in the
    /// reapable set, or the pidfile records a name the reaper will refuse.
    /// Paths are written in the host's own syntax — `Path::file_name` only
    /// splits on backslashes when it is compiled for Windows.
    #[test]
    fn spawned_program_names_are_reapable() {
        #[cfg(windows)]
        let progs = ["ffmpeg.exe", r"C:\Program Files\Kashot\ffmpeg.exe"];
        #[cfg(not(windows))]
        let progs = ["ffmpeg", "/usr/bin/ffmpeg", "screencapture",
                     "/Applications/Kashot.app/Contents/Resources/ffmpeg"];
        for prog in progs {
            let name = program_basename(&Command::new(prog));
            assert!(RECORDER_IMAGES.contains(&name.as_str()),
                "{prog} -> {name:?} is not in RECORDER_IMAGES");
        }
    }

    #[test]
    fn watchdog_script_names_both_pids_and_backgrounds_itself() {
        let s = watchdog_script(111, 222);
        assert!(s.contains("111") && s.contains("222"), "{s}");
        assert!(s.trim_end().ends_with('&'), "watchdog must detach: {s}");
        // Graceful first, hard kill only as a fallback.
        assert!(s.find("-INT").unwrap() < s.find("-9").unwrap(), "{s}");
    }

    /// The watchdog must be valid shell. `sh -n` parses without executing.
    #[cfg(unix)]
    #[test]
    fn watchdog_script_is_syntactically_valid_shell() {
        use std::io::Write;
        let mut c = Command::new("/bin/sh")
            .args(["-n"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(watchdog_script(1, 2).as_bytes()).unwrap();
        assert!(c.wait().unwrap().success(), "watchdog script is not valid shell");
    }
}
