//! In-process self-updater for Kashot.
//!
//! Notepatra-style transactional upgrade. The Updates dialog asks us to
//! download a release asset, verify its SHA-256, swap the running binary,
//! and relaunch. Everything happens in a background thread so the winit
//! event loop stays live — the caller polls the returned `JoinHandle`.
//!
//! Platform notes
//! --------------
//! - **Linux / macOS**: POSIX lets us `rename(new, current_exe)` even while
//!   the old binary is executing — the kernel keeps the running file
//!   reachable through its inode until our process exits. So we just
//!   overwrite-and-relaunch.
//!
//! - **Windows**: the running .exe is locked against deletion *and*
//!   overwriting, but Windows *does* let us rename it. The trick is:
//!     1. Move the running `kashot.exe` aside to `kashot.exe.old`.
//!     2. Move the freshly-downloaded binary into `kashot.exe`.
//!     3. Spawn the new binary and exit. The kernel releases the .old
//!        file as soon as our process dies.
//!     4. On the next launch we try to delete the `.old` leftover.
//!
//! Asset shapes the updater understands
//! ------------------------------------
//! - `.tar.gz` (Linux) — extracted via the system `tar` binary into a
//!   scratch dir; we walk it and find the first file named `kashot`.
//! - `.zip` (Windows) — extracted via PowerShell's `Expand-Archive`; we
//!   walk it and find `kashot.exe`.
//! - anything else (macOS naked binary) — treated as the binary itself.
//!
//! Verification
//! ------------
//! SHA-256 is computed over the *extracted* binary, not the archive,
//! because the CI publishes per-platform hashes of the binary file the
//! user will actually run. `expected_sha256 = None` is a graceful
//! degradation path that exists so this PR can ship before the
//! `ci/installer-artifacts` PR lands its `SHA256SUMS` artifact — once
//! both are in we'll always pass a hash from the caller.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::JoinHandle;

use sha2::{Digest, Sha256};

/// Kick off an asynchronous download + install + relaunch.
///
/// Returns immediately with a [`JoinHandle`] so the caller's event loop
/// can poll for completion via [`JoinHandle::is_finished`] / `try_join`
/// (well — `join` once `is_finished` returns true).
///
/// On success the thread does **not** return — the process is replaced
/// by the new binary via `exit(0)` after the relaunch is spawned, so the
/// `Ok(())` arm is really "we got far enough to spawn the new binary;
/// the old one is about to die".
pub fn download_and_install(
    asset_url: String,
    expected_sha256: Option<String>,
    on_progress: impl Fn(u64, Option<u64>) + Send + 'static,
) -> JoinHandle<Result<(), String>> {
    std::thread::spawn(move || run(asset_url, expected_sha256, on_progress))
}

fn run(
    asset_url: String,
    expected_sha256: Option<String>,
    on_progress: impl Fn(u64, Option<u64>) + Send + 'static,
) -> Result<(), String> {
    on_progress(0, None);

    let download_path = temp_path_for_url(&asset_url);
    download(&asset_url, &download_path, &on_progress)?;

    // Hand the archive (or raw binary) to the extractor; it returns a
    // path to a file that *should* be the new kashot executable.
    let extracted = extract_binary(&download_path)?;

    if let Some(want) = expected_sha256.as_deref() {
        verify_sha256(&extracted, want)?;
    } else {
        eprintln!(
            "self-updater: WARNING — installing {} without SHA-256 verification \
             (caller passed expected_sha256 = None)",
            extracted.display()
        );
    }

    make_executable(&extracted)?;

    let current_exe = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))?;

    swap_running_binary(&extracted, &current_exe)?;
    spawn_and_exit(&current_exe);
}

// ── download ────────────────────────────────────────────────────────────────

/// A deterministic temp filename so a half-completed download from an
/// earlier session is overwritten cleanly when the user retries.
fn temp_path_for_url(url: &str) -> PathBuf {
    let mut h = Sha256::new();
    h.update(url.as_bytes());
    let hex = hex_encode(&h.finalize());
    // 16 hex chars is plenty — collision space is still 64 bits and we
    // never compare two URLs by it.
    let short = &hex[..16];
    let suffix = guess_suffix_from_url(url);
    std::env::temp_dir().join(format!("kashot-update-{short}{suffix}"))
}

fn guess_suffix_from_url(url: &str) -> &'static str {
    // The filename — i.e. the last path segment, stripped of any query
    // string — is what we want to sniff. `kashot-linux-x86_64.tar.gz`
    // → `.tar.gz`, etc.
    let last = url.rsplit('/').next().unwrap_or("");
    let last = last.split('?').next().unwrap_or(last);
    if last.ends_with(".tar.gz") { ".tar.gz" }
    else if last.ends_with(".tgz") { ".tgz" }
    else if last.ends_with(".zip") { ".zip" }
    else { ".bin" }
}

/// Shell out to `curl` like the rest of the app does. `-f` makes curl
/// fail loudly on HTTP errors, `-L` follows redirects (GitHub release
/// downloads bounce through a CDN), `-#` would print a progress bar but
/// we capture stderr instead so the dialog can show its own meter.
fn download(url: &str, out_path: &Path, on_progress: &impl Fn(u64, Option<u64>)) -> Result<(), String> {
    // Best-effort progress: we have no streaming hook into curl from
    // here, so the dialog gets a "downloading" pulse — the real byte
    // counts come from the file's size after curl exits. Acceptable
    // tradeoff vs. pulling a full HTTP client into the workspace.
    on_progress(0, None);

    // Make sure we don't trip on a stale partial download from a prior
    // failed attempt. Ignore errors — the file may not exist yet.
    let _ = std::fs::remove_file(out_path);

    let status = Command::new("curl")
        .args([
            "-fL",
            "-A", "kashot-self-updater",
            "--retry", "2",
            "--retry-delay", "1",
            "-o",
        ])
        .arg(out_path)
        .arg(url)
        .status()
        .map_err(|e| format!("curl spawn: {e}"))?;

    if !status.success() {
        return Err(format!("curl exited with {status} downloading {url}"));
    }
    let meta = std::fs::metadata(out_path)
        .map_err(|e| format!("stat downloaded file: {e}"))?;
    let bytes = meta.len();
    on_progress(bytes, Some(bytes));
    Ok(())
}

// ── extraction ──────────────────────────────────────────────────────────────

/// Returns the path to a file we believe is the new kashot binary.
/// For `.tar.gz` / `.zip` we extract into a sibling scratch dir and
/// walk to find `kashot` / `kashot.exe`. For unknown shapes we treat
/// the downloaded blob as the binary itself.
fn extract_binary(archive: &Path) -> Result<PathBuf, String> {
    let name = archive.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        extract_tar_gz(archive)
    } else if name.ends_with(".zip") {
        extract_zip(archive)
    } else {
        // Raw binary (current macOS shape): nothing to do.
        Ok(archive.to_path_buf())
    }
}

fn scratch_dir_for(archive: &Path) -> PathBuf {
    // Sibling of the archive so cleanup is colocated.
    let name = archive.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("kashot-update");
    archive.with_file_name(format!("{name}.extracted"))
}

fn extract_tar_gz(archive: &Path) -> Result<PathBuf, String> {
    let dir = scratch_dir_for(archive);
    // Wipe any leftover from a prior attempt so the find below picks up
    // the freshly-extracted binary and not a stale one.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create scratch dir {}: {e}", dir.display()))?;

    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(&dir)
        .status()
        .map_err(|e| format!("tar spawn: {e} — is `tar` on PATH?"))?;
    if !status.success() {
        return Err(format!("tar exited with {status} extracting {}", archive.display()));
    }

    find_named(&dir, "kashot")
        .ok_or_else(|| format!("`kashot` binary not found inside {}", archive.display()))
}

fn extract_zip(archive: &Path) -> Result<PathBuf, String> {
    let dir = scratch_dir_for(archive);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create scratch dir {}: {e}", dir.display()))?;

    // On Windows we have PowerShell's Expand-Archive guaranteed; on
    // Linux / macOS the system `unzip` is the path of least resistance.
    // We pick at compile-time so we never spawn a missing tool.
    #[cfg(target_os = "windows")]
    {
        let cmd = format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            archive.display().to_string().replace('\'', "''"),
            dir.display().to_string().replace('\'', "''"),
        );
        let status = Command::new("powershell")
            .args(["-NoProfile", "-Command", &cmd])
            .status()
            .map_err(|e| format!("powershell spawn: {e}"))?;
        if !status.success() {
            return Err(format!("Expand-Archive exited with {status} extracting {}", archive.display()));
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let status = Command::new("unzip")
            .arg("-o")
            .arg(archive)
            .arg("-d")
            .arg(&dir)
            .status()
            .map_err(|e| format!("unzip spawn: {e} — is `unzip` on PATH?"))?;
        if !status.success() {
            return Err(format!("unzip exited with {status} extracting {}", archive.display()));
        }
    }

    let want = if cfg!(target_os = "windows") { "kashot.exe" } else { "kashot" };
    find_named(&dir, want)
        .ok_or_else(|| format!("`{want}` not found inside {}", archive.display()))
}

/// Depth-first walk; returns the first file whose name matches `want`.
/// Archives we ship are small (one binary + maybe a README) so this is
/// fine without a hand-rolled iterator.
fn find_named(root: &Path, want: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if path.file_name().and_then(|s| s.to_str()) == Some(want) {
                return Some(path);
            }
        } else if path.is_dir() {
            if let Some(hit) = find_named(&path, want) {
                return Some(hit);
            }
        }
    }
    None
}

// ── verification ────────────────────────────────────────────────────────────

fn verify_sha256(path: &Path, expected_hex: &str) -> Result<(), String> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| format!("open {} for hashing: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)
            .map_err(|e| format!("read {} during hashing: {e}", path.display()))?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    let got = hex_encode(&hasher.finalize());
    let want = expected_hex.trim().to_ascii_lowercase();
    if got != want {
        return Err(format!(
            "SHA-256 mismatch for {}: expected {want}, got {got}",
            path.display()
        ));
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(TABLE[(b >> 4) as usize] as char);
        s.push(TABLE[(b & 0x0F) as usize] as char);
    }
    s
}

// ── filesystem helpers ──────────────────────────────────────────────────────

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .map_err(|e| format!("chmod +x {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    // Windows derives "is this executable" from the file extension and
    // the PE header, not from a permission bit. Nothing to do.
    Ok(())
}

// ── the swap ────────────────────────────────────────────────────────────────

#[cfg(not(target_os = "windows"))]
fn swap_running_binary(new_bin: &Path, current_exe: &Path) -> Result<(), String> {
    // POSIX guarantee: rename(2) of an executable file is safe while
    // the kernel still has it open for execution; the kernel keeps the
    // old inode alive until our process exits, so the in-flight code
    // pages stay valid.
    std::fs::rename(new_bin, current_exe).map_err(|e| {
        format!(
            "rename {} -> {}: {e}",
            new_bin.display(),
            current_exe.display()
        )
    })
}

#[cfg(target_os = "windows")]
fn swap_running_binary(new_bin: &Path, current_exe: &Path) -> Result<(), String> {
    // Windows refuses to delete or overwrite the running .exe, but it
    // happily *renames* it — the file handle stays valid against the
    // new path. So:
    //   1. Rename the running kashot.exe to kashot.exe.old (atomic).
    //   2. Move the new binary into kashot.exe.
    //   3. Let main()'s startup-cleanup delete the .old on the next
    //      launch, once our PID has exited and the lock is released.
    let old_path = old_path_for(current_exe);
    move_file_replacing(current_exe, &old_path)?;
    if let Err(e) = move_file_replacing(new_bin, current_exe) {
        // Try to roll back so we don't leave the user without a kashot.
        // Best-effort; if this fails too there's not much more we can do.
        let _ = move_file_replacing(&old_path, current_exe);
        return Err(e);
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn old_path_for(current_exe: &Path) -> PathBuf {
    // Sibling of the running .exe, so it lands on the same volume —
    // MoveFileExW with MOVEFILE_REPLACE_EXISTING is atomic only when
    // the source and dest are on the same volume.
    let mut s = current_exe.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

#[cfg(target_os = "windows")]
fn move_file_replacing(src: &Path, dst: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING};

    let src_w: Vec<u16> = src.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let dst_w: Vec<u16> = dst.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

    // SAFETY: both buffers are NUL-terminated UTF-16 sequences and live
    // for the duration of the call. MoveFileExW takes raw pointers.
    let res = unsafe {
        MoveFileExW(
            PCWSTR(src_w.as_ptr()),
            PCWSTR(dst_w.as_ptr()),
            MOVEFILE_REPLACE_EXISTING,
        )
    };
    res.map_err(|e| {
        format!(
            "MoveFileExW {} -> {}: {e}",
            src.display(),
            dst.display()
        )
    })
}

// ── relaunch ────────────────────────────────────────────────────────────────

/// Spawn the freshly-installed binary detached from us, then exit so the
/// new process owns the tray slot. This function does not return.
fn spawn_and_exit(current_exe: &Path) -> ! {
    // `Command::spawn` on Unix already detaches enough — the child gets
    // its own pgid as soon as we exec, and Rust doesn't insert any
    // wait-for-child plumbing on drop. On Windows, the child inherits
    // our console (when one exists) but tray apps run windowless so it
    // doesn't matter; in dev builds the child opens its own console.
    let spawn_res = Command::new(current_exe).spawn();
    if let Err(e) = spawn_res {
        // We've already swapped the binary so the next manual launch
        // will get the new build. Log + die so the user can re-open.
        eprintln!("self-updater: relaunch failed: {e} — please reopen Kashot");
    }
    std::process::exit(0);
}

// ── SHA256SUMS parse helper ─────────────────────────────────────────────────

/// Find the hash for `want_filename` in a SHA256SUMS document of the form
/// produced by `sha256sum` / `shasum -a 256`:
///
/// ```text
/// <64-hex>  kashot-linux-x86_64.tar.gz
/// <64-hex>  kashot-windows-x86_64.zip
/// ```
///
/// Returns the hash in lowercase hex if a matching line exists. Tolerates
/// extra whitespace, CRLF line endings, and BSD-style `SHA256 (file) = hash`
/// formatting in case a CI step ever switches.
///
/// `#[allow(dead_code)]` because the parallel
/// `feat/updates-dialog-release-notes` UI PR is the first caller — until
/// it merges this is plumbing without a consumer inside the binary.
#[allow(dead_code)]
pub fn parse_sha256sums(text: &str, want_filename: &str) -> Option<String> {
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') { continue; }

        // GNU coreutils form: "<hash>  <name>" or "<hash> *<name>".
        if let Some((hash, rest)) = line.split_once(char::is_whitespace) {
            // The filename can have a leading "*" (binary mode marker)
            // or extra spaces.
            let name = rest.trim_start().trim_start_matches('*').trim();
            if name == want_filename && is_hex_sha256(hash) {
                return Some(hash.to_ascii_lowercase());
            }
        }

        // BSD form: "SHA256 (<name>) = <hash>"
        if let Some(rest) = line.strip_prefix("SHA256 (") {
            if let Some(end_paren) = rest.find(')') {
                let name = &rest[..end_paren];
                if name == want_filename {
                    if let Some(eq) = rest[end_paren..].find('=') {
                        let hash = rest[end_paren + eq + 1..].trim();
                        if is_hex_sha256(hash) {
                            return Some(hash.to_ascii_lowercase());
                        }
                    }
                }
            }
        }
    }
    None
}

#[allow(dead_code)] // only used by parse_sha256sums above.
fn is_hex_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

// ── startup cleanup of Windows leftover ─────────────────────────────────────

/// Called from `main()` at startup. On Windows, after a self-update the
/// previous .exe was renamed to `<current_exe>.old`; once the original
/// PID dies the lock releases and we can finally delete it. On Linux /
/// macOS there's nothing to do.
pub fn cleanup_stale_old_binary() {
    #[cfg(target_os = "windows")]
    {
        let Ok(current_exe) = std::env::current_exe() else { return; };
        let old = {
            let mut s = current_exe.as_os_str().to_owned();
            s.push(".old");
            std::path::PathBuf::from(s)
        };
        if old.exists() {
            // If this races with a still-pending lock release we just
            // leave the file alone; the next launch will catch it.
            let _ = std::fs::remove_file(&old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sha256sums_gnu_format() {
        let doc = "\
abc123def4567890abc123def4567890abc123def4567890abc123def4567890  kashot-linux-x86_64.tar.gz
1111111111111111111111111111111111111111111111111111111111111111  kashot-windows-x86_64.zip
2222222222222222222222222222222222222222222222222222222222222222  Kashot-macos-arm64
";
        assert_eq!(
            parse_sha256sums(doc, "kashot-linux-x86_64.tar.gz"),
            Some("abc123def4567890abc123def4567890abc123def4567890abc123def4567890".to_owned())
        );
        assert_eq!(
            parse_sha256sums(doc, "kashot-windows-x86_64.zip"),
            Some("1111111111111111111111111111111111111111111111111111111111111111".to_owned())
        );
        assert_eq!(parse_sha256sums(doc, "kashot-linux-arm64.tar.gz"), None);
    }

    #[test]
    fn parse_sha256sums_tolerates_crlf_and_binary_marker() {
        let doc = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef *kashot.zip\r\n";
        assert_eq!(
            parse_sha256sums(doc, "kashot.zip"),
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_owned())
        );
    }

    #[test]
    fn parse_sha256sums_bsd_format() {
        let doc = "SHA256 (kashot-windows-x86_64.zip) = CAFEBABECAFEBABECAFEBABECAFEBABECAFEBABECAFEBABECAFEBABECAFEBABE\n";
        assert_eq!(
            parse_sha256sums(doc, "kashot-windows-x86_64.zip"),
            Some("cafebabecafebabecafebabecafebabecafebabecafebabecafebabecafebabe".to_owned())
        );
    }

    #[test]
    fn parse_sha256sums_skips_comments_and_blanks() {
        let doc = "\
# generated by ci

0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  kashot
";
        assert_eq!(
            parse_sha256sums(doc, "kashot"),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned())
        );
    }

    #[test]
    fn parse_sha256sums_rejects_short_hash() {
        let doc = "deadbeef  kashot\n";
        assert_eq!(parse_sha256sums(doc, "kashot"), None);
    }

    #[test]
    fn hex_encode_round_trip() {
        assert_eq!(hex_encode(&[0xDE, 0xAD, 0xBE, 0xEF]), "deadbeef");
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0, 0, 0]), "000000");
    }

    #[test]
    fn guess_suffix_picks_archive_extension() {
        assert_eq!(
            guess_suffix_from_url("https://example.com/kashot-linux-x86_64.tar.gz"),
            ".tar.gz"
        );
        assert_eq!(
            guess_suffix_from_url("https://example.com/kashot-windows-x86_64.zip?token=abc"),
            ".zip"
        );
        assert_eq!(
            guess_suffix_from_url("https://example.com/Kashot-macos-arm64"),
            ".bin"
        );
    }
}
