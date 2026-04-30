//! kashot-platform
//!
//! Cross-platform shims for the things every screenshot tool needs but that
//! no language can make uniform: full-screen capture, global hotkey
//! registration, tray icon, and OS clipboard. Each module hides its OS-specific
//! crate behind a thin trait-shaped API.

pub mod capture;
pub mod clipboard;
pub mod hotkey;
pub mod recorder;
pub mod tray;

pub use capture::{capture_all_screens, Captured};
pub use clipboard::copy_image_png;
pub use hotkey::{HotkeyHandle, HotkeyManager};
pub use recorder::Recorder;
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
