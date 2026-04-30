//! Kashot — native cross-platform screenshot tool.
//!
//! Architecture:
//!   - tray + hotkey live in the daemon-mode iced app
//!   - hitting the hotkey or "Capture" menu item triggers a screen capture
//!   - the capture's bitmap is handed to a freshly-opened overlay window
//!   - overlay lets the user select a region, annotate, then save / copy / pin
//!   - settings persist to %APPDATA%/Kashot/settings.json (cross-platform path
//!     handled by the `directories` crate)
//!
//! On Windows we hide the console window so this runs as a true tray app.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod editor;

fn main() -> iced::Result {
    editor::run()
}
