//! User-facing wording for the ways a capture can fail to land anywhere.
//!
//! These paths used to print to stderr and nothing else, which for a
//! tray-resident app means "nowhere": the overlay has already closed, so a
//! failed save, a refused clipboard, or a pin window that never opened looked
//! exactly like success. The tray now toasts each of them, and every message
//! is built here so the wording stays consistent — and so it can be tested
//! without a desktop session.
//!
//! House rules for every string below:
//!   * name the thing that failed (the folder, the file, the surface),
//!   * quote the OS reason verbatim — it is usually the actionable half,
//!   * end with the one step the user can actually take.

/// The save folder itself couldn't be created or reached.
pub fn save_folder_failure(dir: &str, reason: &str) -> String {
    format!(
        "Couldn't use the save folder {dir}: {reason}. \
         The screenshot was not saved. Pick a different folder under \
         Settings > Save folder, then capture again."
    )
}

/// The folder was fine; writing the PNG into it wasn't.
pub fn save_write_failure(path: &str, reason: &str) -> String {
    format!(
        "Couldn't write {path}: {reason}. \
         The screenshot was not saved. Check free disk space and that the \
         folder is writable, then capture again."
    )
}

/// The clipboard refused the image, so nothing was copied *and* nothing was
/// written to disk — the copy path never touches the save folder.
pub fn clipboard_failure(reason: &str) -> String {
    format!(
        "Couldn't copy the screenshot to the clipboard: {reason}. \
         Nothing was saved to disk either — capture again and press Enter to \
         save the shot to your save folder instead."
    )
}

/// The pinned-image window couldn't be created.
pub fn pin_failure(reason: &str) -> String {
    format!(
        "Couldn't pin the screenshot to the screen: {reason}. \
         Nothing was saved to disk — capture again and press Enter to save it \
         to your save folder instead."
    )
}

/// Settings were edited but couldn't be persisted, so the change is live for
/// this session only and disappears on the next launch.
pub fn settings_write_failure(reason: &str) -> String {
    format!(
        "Couldn't save your settings: {reason}. \
         They apply for now but will be lost when KAShot restarts. Check that \
         the config folder exists and is writable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every message has to carry the two things a user needs to act on:
    /// what failed (a path, where there is one) and why (the OS reason).
    #[test]
    fn messages_quote_path_and_reason() {
        let m = save_folder_failure("/data/shots", "Permission denied");
        assert!(m.contains("/data/shots"), "{m}");
        assert!(m.contains("Permission denied"), "{m}");

        let m = save_write_failure("/data/shots/kashot_1.png", "No space left on device");
        assert!(m.contains("/data/shots/kashot_1.png"), "{m}");
        assert!(m.contains("No space left on device"), "{m}");

        let m = clipboard_failure("X11 selection owner vanished");
        assert!(m.contains("X11 selection owner vanished"), "{m}");

        let m = pin_failure("no display server");
        assert!(m.contains("no display server"), "{m}");

        let m = settings_write_failure("Read-only file system");
        assert!(m.contains("Read-only file system"), "{m}");
    }

    /// A toast body that runs on for a paragraph gets truncated by every
    /// notification daemon we target, so the actionable sentence has to fit.
    #[test]
    fn messages_stay_short_enough_to_toast() {
        for m in [
            save_folder_failure("/data/shots", "Permission denied"),
            save_write_failure("/data/shots/kashot_1.png", "No space left on device"),
            clipboard_failure("selection owner vanished"),
            pin_failure("no display server"),
            settings_write_failure("Read-only file system"),
        ] {
            assert!(m.len() < 240, "too long to toast ({} chars): {m}", m.len());
        }
    }

    /// The copy and pin paths discard the bitmap when they fail, so both have
    /// to tell the user the shot is gone rather than implying it was filed
    /// away somewhere.
    #[test]
    fn discarding_paths_say_nothing_was_saved() {
        assert!(clipboard_failure("x").contains("Nothing was saved"));
        assert!(pin_failure("x").contains("Nothing was saved"));
        assert!(save_folder_failure("/d", "x").contains("was not saved"));
        assert!(save_write_failure("/d/f.png", "x").contains("was not saved"));
    }
}
