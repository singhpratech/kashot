//! kashot-platform
//!
//! Cross-platform shims for the things every screenshot tool needs but that
//! no language can make uniform: full-screen capture, global hotkey
//! registration, tray icon, and OS clipboard. Each module hides its OS-specific
//! crate behind a thin trait-shaped API.

pub mod capture;
/// Portal Screenshot fallback for Wayland sessions where the X11 monitor
/// enumeration `xcap` needs isn't available.
#[cfg(target_os = "linux")]
pub mod capture_portal;
pub mod clipboard;
pub mod hotkey;
/// GlobalShortcuts-portal hotkey backend, selected by `hotkey::HotkeyManager`
/// on Wayland sessions.
#[cfg(target_os = "linux")]
pub mod hotkey_portal;
pub mod recorder;
pub mod session;
pub mod tray;

pub use capture::{capture_all_screens, Captured};
pub use clipboard::copy_image_png;
pub use hotkey::{HotkeyBackend, HotkeyHandle, HotkeyManager};
pub use recorder::Recorder;
pub use session::is_wayland;
pub use tray::{Tray, TrayEvent};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("capture failed: {0}")]
    Capture(String),

    #[error("hotkey error: {0}")]
    Hotkey(String),

    #[error("clipboard error: {0}")]
    Clipboard(String),

    #[error("tray error: {0}")]
    Tray(String),

    #[error("recording error: {0}")]
    Recording(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
