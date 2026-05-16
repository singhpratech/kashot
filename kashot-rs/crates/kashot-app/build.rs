//! Build-time ffmpeg bundling.
//!
//! Lookup order:
//!   1. `KASHOT_FFMPEG` env var — explicit path the user / CI gives us.
//!      Use this in release builds to point at a known-good static binary
//!      (the shipped one needs to run on machines without libavcodec
//!      installed, so a static build is the right thing to ship).
//!   2. `ffmpeg` (or `ffmpeg.exe`) on the build host's `PATH` — convenient
//!      for local development where the dev's own ffmpeg is fine.
//!
//! When found, the binary is copied to the same directory as the kashot
//! executable (`target/<profile>/`). At runtime `locate_ffmpeg()` in
//! convert_video_form.rs checks that path first, so no extra wiring is
//! needed in the application code.
//!
//! When not found, we just warn — the runtime still works on any host
//! that has ffmpeg on PATH, which is most desktop Linux machines and any
//! Mac with Homebrew. Windows users without ffmpeg get an actionable error
//! from the Convert-video dialog.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    // Re-run on env override change so a CI value flip rebuilds cleanly.
    println!("cargo:rerun-if-env-changed=KASHOT_FFMPEG");
    println!("cargo:rerun-if-changed=build.rs");

    let src = locate_ffmpeg_source();
    let Some(src) = src else {
        println!("cargo:warning=ffmpeg not found at build time. Convert-video will fall back to runtime PATH lookup. Set KASHOT_FFMPEG=/path/to/ffmpeg (preferably static) to bundle.");
        return;
    };
    if !src.is_file() {
        println!("cargo:warning=KASHOT_FFMPEG points at a non-file: {}", src.display());
        return;
    }

    let Some(target_dir) = find_target_profile_dir() else {
        println!("cargo:warning=Could not locate cargo target/<profile>/ to bundle ffmpeg into. Skipping bundle.");
        return;
    };

    let bin_name = if cfg!(target_os = "windows") { "ffmpeg.exe" } else { "ffmpeg" };
    let dst = target_dir.join(bin_name);

    // Skip the copy if the dst already exists and is newer-or-equal to the
    // source. Saves churn on incremental rebuilds.
    let stale = match (mtime(&src), mtime(&dst)) {
        (Some(sm), Some(dm)) => sm > dm,
        _                    => true,
    };
    if !stale && dst.is_file() {
        return;
    }

    if let Err(e) = std::fs::copy(&src, &dst) {
        println!("cargo:warning=Failed to copy ffmpeg {} → {}: {e}",
                 src.display(), dst.display());
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755));
    }
    println!("cargo:warning=Bundled ffmpeg: {} -> {}",
             src.display(), dst.display());
}

fn locate_ffmpeg_source() -> Option<PathBuf> {
    if let Ok(p) = env::var("KASHOT_FFMPEG") {
        return Some(PathBuf::from(p));
    }
    let name = if cfg!(target_os = "windows") { "ffmpeg.exe" } else { "ffmpeg" };
    let path_var = env::var("PATH").ok()?;
    let sep = if cfg!(target_os = "windows") { ';' } else { ':' };
    for part in path_var.split(sep) {
        let p = Path::new(part).join(name);
        if p.is_file() { return Some(p); }
    }
    None
}

/// Cargo invokes build scripts with `OUT_DIR` set to
/// `<workspace>/target/<profile>/build/<pkg-name-hash>/out`. We ascend
/// three levels (out → <pkg-name-hash> → build → <profile>) to reach
/// the directory where the final binary lands.
fn find_target_profile_dir() -> Option<PathBuf> {
    let out_dir = env::var("OUT_DIR").ok()?;
    let p = Path::new(&out_dir);
    let profile_dir = p.parent()?.parent()?.parent()?.to_path_buf();
    if profile_dir.is_dir() { Some(profile_dir) } else { None }
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}
