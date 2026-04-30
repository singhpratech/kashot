//! Kashot editor — iced-based tray-resident screenshot tool.
//!
//! Module layout:
//!   * `app`              — `iced::daemon` root, owns settings + tray + hotkey
//!   * `message`          — flat `Message` enum the whole app dispatches
//!   * `overlay`          — overlay window state + iced view
//!   * `canvas`           — `iced::widget::canvas::Program` for the capture surface
//!   * `render`           — annotation drawing into a `canvas::Frame`
//!   * `toolbar`          — floating tool panel + action panel + color picker
//!   * `icons`            — procedurally drawn tool icons (no asset files)
//!   * `save`             — final-image rendering and save/copy/pin actions
//!   * `settings_dialog`  — settings window
//!   * `about_dialog`     — about window
//!   * `pin_window`       — pinned screenshot window

mod about_dialog;
mod app;
mod canvas;
mod icons;
mod message;
mod overlay;
mod pin_window;
mod render;
mod save;
mod settings_dialog;
mod toolbar;

pub use app::run;
