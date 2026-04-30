//! Flat `Message` enum dispatched by `App::update`.

use iced::window;
use kashot_core::{annotation::Point2, Tool};

#[derive(Debug, Clone)]
pub enum Message {
    /// Periodic 30Hz tick — drains tray + hotkey event channels.
    Tick,

    /// Hotkey or tray "Capture" — kick off a screen capture.
    StartCapture,

    /// Settings menu picked.
    OpenSettings,

    /// About menu picked.
    OpenAbout,

    /// Exit menu picked.
    Exit,

    /// Capture finished; bitmap is ready and we should open the overlay window.
    CaptureReady(SharedCapture),

    /// Capture failed; show a tray balloon and stay resident.
    CaptureFailed(String),

    // ── Overlay events ──────────────────────────────────────────────────
    Overlay(window::Id, OverlayMessage),

    // ── Settings dialog events ──────────────────────────────────────────
    Settings(window::Id, SettingsMessage),

    // ── Pin window events ───────────────────────────────────────────────
    Pin(window::Id, PinMessage),

    // ── Window lifecycle ────────────────────────────────────────────────
    WindowClosed(window::Id),
}

#[derive(Debug, Clone)]
pub enum OverlayMessage {
    MouseDown { p: Point2, button: MouseButton, mods: KeyMods },
    MouseMove { p: Point2 },
    MouseUp   { p: Point2, button: MouseButton },
    KeyPress  { key: Key, mods: KeyMods },
    SelectTool(Tool),
    PickColor(kashot_core::Rgba),
    OpenColorPicker,
    CloseColorPicker,
    NextPalette,
    PrevPalette,
    CycleThickness,
    Undo,
    Redo,
    Save,
    Copy,
    Pin,
    Cancel,
    SaveResult(Result<std::path::PathBuf, String>),
    CopyResult(Result<(), String>),
    /// Internal — text input committed.
    TextCommitted(String),
    TextCancelled,
    TextChanged(String),
}

#[derive(Debug, Clone)]
pub enum SettingsMessage {
    HotkeyChanged { mods: u32, vk: u32 },
    SaveDirChanged(String),
    BrowseSaveDir,
    BrowseSaveDirResult(Option<std::path::PathBuf>),
    StartWithOsToggled(bool),
    WatermarkToggled(bool),
    WatermarkTextChanged(String),
    ThemeChanged(String),
    Apply,
    Cancel,
}

#[derive(Debug, Clone)]
pub enum PinMessage {
    DragStart { p: Point2 },
    DragMove  { p: Point2 },
    DragEnd,
    Copy,
    Save,
    SaveResult(Result<std::path::PathBuf, String>),
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyMods {
    pub ctrl:  bool,
    pub shift: bool,
    pub alt:   bool,
    pub logo:  bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Escape,
    Enter,
    Char(char),
}

/// `Arc<Captured>` so the bitmap can move between subscriptions and update
/// without a deep clone — the screenshot can be tens of megabytes.
pub type SharedCapture = std::sync::Arc<kashot_platform::capture::Captured>;
