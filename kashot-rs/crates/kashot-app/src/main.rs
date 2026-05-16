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
mod about_form;
mod brand_icon;
mod convert_image_form;
mod convert_video_form;
mod pin;
mod recording_indicator;
mod settings_form;
mod tray_loop;
mod updates_form;

use anyhow::Result;

fn main() -> Result<()> {
    tray_loop::run()
}
