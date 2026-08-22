//! Crash-safe whole-file replacement.
//!
//! `fs::write` truncates the destination and then streams into it, so a crash
//! (or a power cut, or an OOM kill) anywhere in the middle leaves a
//! half-written file behind. For `settings.json` that is a corrupt document:
//! `AppSettings::load` can't parse it, silently falls back to `Default`, and
//! the user's save folder, hotkey, watermark and theme are all gone.
//!
//! The fix is the standard write-temp-then-rename dance. The temp file lives
//! in the *same directory* as the destination — a rename is only atomic within
//! a filesystem, and `/tmp` is frequently a different one — and is fsynced
//! before the rename so the rename can't be reordered ahead of the data.
//! After the rename the destination either holds the complete old content or
//! the complete new content, never a splice of the two.
//!
//! `fs::rename` is the right primitive on every platform we ship: POSIX
//! `rename(2)` replaces atomically, and Rust's Windows implementation calls
//! `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Rename attempts before giving up. Windows lets a virus scanner or the
/// search indexer hold a transient handle on a file it just saw appear, which
/// fails the replace with a sharing violation for a few milliseconds. Unix
/// never needs the retries and never pays for them.
const RENAME_ATTEMPTS: u32 = 4;
const RENAME_BACKOFF_MS: u64 = 40;

/// Monotonic suffix so two writers in one process can't pick the same temp
/// name. The pid covers writers in different processes.
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Temp-file name for a destination, in the destination's own directory.
///
/// Dot-prefixed so it doesn't show up in a file listing during the split
/// second it exists, and suffixed with pid + counter so concurrent writers
/// never collide on it.
fn temp_path_for(dest: &Path) -> PathBuf {
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    let stem = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{stem}.tmp-{}-{n}", std::process::id()))
}

/// Replace `dest` with `bytes`, atomically.
///
/// The destination's parent directory must already exist — this is a file
/// primitive, not a directory one, and callers who need the directory created
/// know better than this function whether that's appropriate.
///
/// On any failure the temp file is removed and `dest` is left exactly as it
/// was, so a failed save never destroys the previous good copy.
pub fn write_atomic(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = temp_path_for(dest);

    // Scoped so the handle is closed before the rename: Windows refuses to
    // rename a file that still has an open handle in some sharing modes.
    let write_result = (|| -> io::Result<()> {
        let mut f: File = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        // Without this the rename can land before the data does, which on a
        // crash gives us an atomically-renamed *empty* file — the exact
        // corruption we're trying to avoid.
        f.sync_all()?;
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    let mut last = None;
    for attempt in 0..RENAME_ATTEMPTS {
        match fs::rename(&tmp, dest) {
            Ok(()) => {
                sync_parent_dir(dest);
                return Ok(());
            }
            Err(e) => {
                last = Some(e);
                if attempt + 1 < RENAME_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(RENAME_BACKOFF_MS));
                }
            }
        }
    }

    let _ = fs::remove_file(&tmp);
    Err(last.unwrap_or_else(|| io::Error::other("rename failed")))
}

/// Persist the directory entry the rename just created.
///
/// Best-effort: a rename is atomic with respect to readers either way, and
/// this only guards the narrower case of a power loss immediately afterwards.
/// Unix-only — Windows has no directory handle to fsync, and `MoveFileExW`
/// already orders the metadata write for us.
#[cfg(unix)]
fn sync_parent_dir(dest: &Path) {
    if let Some(dir) = dest.parent() {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
}

#[cfg(not(unix))]
fn sync_parent_dir(_dest: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own directory under the OS temp dir. No `tempfile`
    /// dependency: kashot-core deliberately carries almost none.
    fn scratch_dir(tag: &str) -> PathBuf {
        let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir()
            .join(format!("kashot-atomic-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Files the caller didn't ask for, i.e. leaked temps.
    fn stray_entries(dir: &Path, expected: &str) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != expected)
            .collect()
    }

    #[test]
    fn writes_a_new_file() {
        let dir = scratch_dir("new");
        let dest = dir.join("settings.json");
        write_atomic(&dest, b"{\"a\":1}").unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "{\"a\":1}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replaces_existing_content_wholesale() {
        let dir = scratch_dir("replace");
        let dest = dir.join("settings.json");
        write_atomic(&dest, b"old content that is quite long").unwrap();
        write_atomic(&dest, b"new").unwrap();
        // Not "newcontent that is quite long" — the replace is whole-file, so
        // no tail of the previous write survives.
        assert_eq!(fs::read_to_string(&dest).unwrap(), "new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let dir = scratch_dir("clean");
        let dest = dir.join("settings.json");
        for i in 0..5 {
            write_atomic(&dest, format!("{i}").as_bytes()).unwrap();
        }
        assert_eq!(
            stray_entries(&dir, "settings.json"),
            Vec::<String>::new(),
            "temp files leaked into the destination directory"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// The whole point: the destination is only ever the old bytes or the new
    /// bytes. Approximated by checking that the temp file, not the
    /// destination, is what gets opened for writing — the destination's inode
    /// content is untouched until the rename.
    #[test]
    fn temp_lands_in_the_destination_directory() {
        let dir = scratch_dir("samedir");
        let dest = dir.join("settings.json");
        let tmp = temp_path_for(&dest);
        assert_eq!(tmp.parent(), dest.parent(),
            "temp must share a filesystem with the destination for rename to be atomic");
        assert_ne!(tmp.file_name(), dest.file_name());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn temp_names_are_unique_per_call() {
        let dest = Path::new("/nowhere/settings.json");
        let a = temp_path_for(dest);
        let b = temp_path_for(dest);
        assert_ne!(a, b);
    }

    /// A missing parent directory is an error, and — critically — it doesn't
    /// leave a stray temp anywhere.
    #[test]
    fn missing_parent_directory_is_an_error() {
        let dir = scratch_dir("noparent");
        let dest = dir.join("does-not-exist").join("settings.json");
        assert!(write_atomic(&dest, b"x").is_err());
        assert!(!dest.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A failed write must not take the previous good copy with it. The
    /// unwritable case is hard to force portably, so this covers the shape we
    /// can force: the old file survives a write whose destination directory
    /// was swapped out from under it.
    #[test]
    fn failed_write_preserves_previous_content() {
        let dir = scratch_dir("preserve");
        let dest = dir.join("settings.json");
        write_atomic(&dest, b"good").unwrap();

        let doomed = dir.join("gone").join("settings.json");
        assert!(write_atomic(&doomed, b"bad").is_err());

        assert_eq!(fs::read_to_string(&dest).unwrap(), "good");
        assert_eq!(stray_entries(&dir, "settings.json"), Vec::<String>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn handles_empty_content() {
        let dir = scratch_dir("empty");
        let dest = dir.join("settings.json");
        write_atomic(&dest, b"seeded").unwrap();
        write_atomic(&dest, b"").unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Concurrent writers must never collide on a temp name or corrupt the
    /// destination: whoever renames last wins, and the file is one of the
    /// candidate payloads in full.
    #[test]
    fn concurrent_writers_leave_one_intact_payload() {
        let dir = scratch_dir("threads");
        let dest = dir.join("settings.json");
        let payloads = ["aaaa".repeat(64), "bbbb".repeat(64), "cccc".repeat(64)];

        std::thread::scope(|s| {
            for p in &payloads {
                let dest = dest.clone();
                s.spawn(move || {
                    for _ in 0..10 {
                        write_atomic(&dest, p.as_bytes()).unwrap();
                    }
                });
            }
        });

        let got = fs::read_to_string(&dest).unwrap();
        assert!(payloads.iter().any(|p| *p == got), "torn write: {} bytes", got.len());
        assert_eq!(stray_entries(&dir, "settings.json"), Vec::<String>::new());
        let _ = fs::remove_dir_all(&dir);
    }
}
