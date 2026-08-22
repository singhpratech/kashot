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
pub mod dpi;
pub mod edit;
pub mod history;
pub mod region;
pub mod settings;
pub mod shortcut;
pub mod state;
pub mod text;
pub mod theme;
pub mod tool;
pub mod virtual_desktop;

pub use annotation::{Annotation, AnnotationKind, ColorPalette, Palettes};
pub use color::Rgba;
pub use install_channel::{detect_action, HostOs, InstallChannel, InstallProbe, UpdateAction};
pub use hotkeys::HotkeyAction;
pub use dpi::{DisplayMap, GrabRegion, LogicalRect, MonitorGeometry, MonitorPlacement, PhysicalRect};
pub use history::{EditOp, History};
pub use region::{CaptureRect, DesktopBounds};
pub use settings::{AppSettings, Hotkey, Modifiers};
pub use shortcut::portal_trigger;
pub use state::{Edge, State};
pub use text::{PlacedGlyph, TextBlock, TextRect};
pub use theme::{ThemeColors, ThemeName};
pub use tool::Tool;
pub use virtual_desktop::{DesktopGeometry, MonitorRect};
