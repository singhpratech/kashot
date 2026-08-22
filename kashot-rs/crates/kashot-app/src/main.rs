//! Kashot — native cross-platform screenshot tool.
//!
//! Tray-resident lifecycle:
//!   - tray icon stays visible
//!   - global hotkey + tray "Capture" both call `start_capture`
//!   - capture grabs every monitor and stitches into a single bitmap
//!   - bitmap saved to the user's `SaveDirectory` (or `~/Pictures` if unset)
//!
//! The full overlay editor (region selection, 9 annotation tools, undo/redo,
//! save/copy/pin) is the next slice of work — see PLAN.md § R7.
//!
//! On Windows we hide the console window so this runs as a true tray app.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod bitmap_font;
mod editor;
mod icons;
mod install_source;
mod painter;
mod about_form;
mod brand_icon;
mod convert_image_form;
mod convert_video_form;
mod firstrun_form;
mod pin;
mod recording_indicator;
mod self_updater;
mod settings_form;
mod tray_loop;
mod updates_form;

use std::time::Duration;

use anyhow::Result;
use kashot_platform::instance::{self, Instance};

/// How long a relaunch spawned by the self-updater waits for the outgoing
/// process to release the single-instance lock. The predecessor exits
/// immediately after spawning us, so this is generous by an order of
/// magnitude; the cap only exists so a wedged predecessor can't hang us
/// forever.
const RELAUNCH_HANDOVER_GRACE: Duration = Duration::from_secs(10);

fn main() -> Result<()> {
    // After a self-update on Windows the previous .exe was renamed to
    // `<current_exe>.old` and couldn't be deleted until our PID exited.
    // We're that new PID now — clean it up. No-op on Linux / macOS.
    self_updater::cleanup_stale_old_binary();

    // One KAShot per user session. A second launch used to add a second tray
    // icon, a second (silently refused) hotkey registration, and a second
    // recorder racing the first for the same output filenames.
    //
    // The one legitimate overlap is a self-update relaunch: our predecessor
    // spawns us and only then exits, so we wait for the handover instead of
    // quitting as a duplicate.
    let grace = if std::env::var_os(self_updater::RELAUNCH_ENV).is_some() {
        RELAUNCH_HANDOVER_GRACE
    } else {
        Duration::ZERO
    };
    // Held for the whole process: dropping it releases the lock.
    let _instance = match instance::acquire_waiting(grace) {
        Instance::Primary(guard) => Some(guard),
        Instance::AlreadyRunning => {
            // Make the extra launch do the useful thing rather than nothing:
            // the running instance polls for this and opens the overlay.
            instance::request_capture();
            eprintln!(
                "KAShot is already running — asked it to capture. \
                 Use its tray icon to reach the menu."
            );
            return Ok(());
        }
        // No usable per-app directory to lock in. Start anyway: refusing to
        // launch is far worse than a duplicate tray icon.
        Instance::Unsupported => None,
    };

    // A recording only survives our death if we died without cleaning up
    // (crash, SIGKILL, power loss). The pid was recorded when it started, so
    // stop it now rather than leaving an encoder running with no UI attached
    // to it. Safe here and nowhere else: we hold the instance lock, so there
    // is no live sibling whose recorder this could be.
    if let Some(pid) = kashot_platform::reap_orphaned_recorder() {
        eprintln!("Stopped a screen recording left over from a previous run (pid {pid}).");
    }

    tray_loop::run()
}
