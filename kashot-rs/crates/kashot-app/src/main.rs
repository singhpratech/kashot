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
mod painter;
mod pin;
mod tray_loop;

use anyhow::Result;

fn main() -> Result<()> {
    tray_loop::run()
}
