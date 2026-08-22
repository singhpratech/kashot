//! Reads the running install's channel out of the real environment.
//!
//! All the judgement lives in `kashot_core::install_channel`; this module is
//! just the I/O edge — environment variables, `current_exe()`, and two
//! marker-file probes — so the classification itself stays pure and testable.
//! The result is cached: nothing it looks at can change while we run, and the
//! Updates dialog asks for it on every open.

use std::path::PathBuf;
use std::sync::OnceLock;

use kashot_core::install_channel::{self, HostOs, InstallChannel, InstallProbe, UpdateAction};

static DETECTED: OnceLock<(InstallChannel, UpdateAction)> = OnceLock::new();

/// The channel this binary was installed from, and what the updater is
/// allowed to do about a newer release.
pub fn detected() -> &'static (InstallChannel, UpdateAction) {
    DETECTED.get_or_init(|| {
        let probe = probe();
        let resolved = install_channel::detect_action(&probe);
        eprintln!(
            "updates: install channel = {} ({:?})",
            resolved.0.label(),
            resolved.1
        );
        resolved
    })
}

/// The channel alone.
pub fn channel() -> InstallChannel {
    detected().0
}

/// `$APPIMAGE` — the `.AppImage` file the running process was launched from.
/// This is the file an AppImage update replaces; `current_exe()` points into
/// the read-only FUSE mount instead.
pub fn appimage_path() -> Option<PathBuf> {
    non_empty_env("APPIMAGE").map(PathBuf::from)
}

fn probe() -> InstallProbe {
    let exe_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    InstallProbe {
        os: HostOs::host(),
        exe_path,
        snap_dir: non_empty_env("SNAP"),
        flatpak_id: non_empty_env("FLATPAK_ID"),
        flatpak_info_exists: std::path::Path::new("/.flatpak-info").exists(),
        appimage_path: non_empty_env("APPIMAGE"),
        homebrew_cask_marker: homebrew_cask_marker(),
        os_release: read_os_release(),
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// A Homebrew cask leaves its bookkeeping in `<prefix>/Caskroom/kashot` while
/// the app bundle itself lands in `/Applications` looking hand-installed, so
/// this directory is the only way to tell the two apart. Checked on macOS
/// only — the cask is a macOS-only artifact.
fn homebrew_cask_marker() -> bool {
    if HostOs::host() != HostOs::MacOs {
        return false;
    }
    let mut prefixes: Vec<PathBuf> = vec![
        // Apple silicon and Intel default prefixes.
        PathBuf::from("/opt/homebrew"),
        PathBuf::from("/usr/local"),
    ];
    if let Some(custom) = non_empty_env("HOMEBREW_PREFIX") {
        prefixes.insert(0, PathBuf::from(custom));
    }
    prefixes
        .iter()
        .any(|p| p.join("Caskroom").join("kashot").exists())
}

/// `/etc/os-release` names the distro family so we can print `apt` / `dnf` /
/// `pacman`-flavoured advice instead of a shrug. Linux-only; absent or
/// unreadable degrades to a generic hint.
fn read_os_release() -> Option<String> {
    if HostOs::host() != HostOs::Linux {
        return None;
    }
    std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .ok()
}
