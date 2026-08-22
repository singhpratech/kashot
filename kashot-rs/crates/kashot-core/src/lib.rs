//! kashot-core
//!
//! Platform-agnostic Kashot logic: tools, annotations, app settings,
//! theme palette, and the overlay state machine. Mirrors the C# types
//! one-for-one so the two implementations stay legible side-by-side.

pub mod annotation;
pub mod atomic_file;
pub mod color;
pub mod failure;
pub mod install_channel;
pub mod hotkeys;
pub mod settings;
pub mod state;
pub mod theme;
pub mod tool;

pub use annotation::{Annotation, AnnotationKind, ColorPalette, Palettes};
pub use color::Rgba;
pub use install_channel::{detect_action, HostOs, InstallChannel, InstallProbe, UpdateAction};
pub use hotkeys::HotkeyAction;
pub use settings::{AppSettings, Hotkey, Modifiers};
pub use state::{Edge, State};
pub use theme::{ThemeColors, ThemeName};
pub use tool::Tool;
