//! Settings window — hotkey rebind, save folder, watermark, theme,
//! start-with-OS toggle. Mirrors the C# `SettingsForm`.

use std::path::PathBuf;

use iced::widget::{button, checkbox, column, container, pick_list, row, text, text_input};
use iced::{window, Alignment, Color, Element, Length, Padding, Task};
use kashot_core::AppSettings;
use kashot_platform::hotkey::HotkeyManager;

use super::message::{Message, SettingsMessage};

pub struct SettingsState {
    pub hotkey_modifiers:  u32,
    pub hotkey_virtual_key: u32,
    pub save_directory:     String,
    pub start_with_os:      bool,
    pub watermark_enabled:  bool,
    pub watermark_text:     String,
    pub theme:              String,
}

impl SettingsState {
    pub fn from(s: &AppSettings) -> Self {
        SettingsState {
            hotkey_modifiers:   s.hotkey_modifiers,
            hotkey_virtual_key: s.hotkey_virtual_key,
            save_directory:     s.save_directory.clone(),
            start_with_os:      s.start_with_windows,
            watermark_enabled:  s.watermark_enabled,
            watermark_text:     s.watermark_text.clone(),
            theme:              s.theme.clone(),
        }
    }
}

pub fn update(
    state: &mut SettingsState,
    settings: &mut AppSettings,
    id: window::Id,
    m: SettingsMessage,
    hotkeys: Option<&mut HotkeyManager>,
) -> Task<Message> {
    match m {
        SettingsMessage::HotkeyChanged { mods, vk } => {
            state.hotkey_modifiers   = mods;
            state.hotkey_virtual_key = vk;
            Task::none()
        }
        SettingsMessage::SaveDirChanged(s) => { state.save_directory = s; Task::none() }
        SettingsMessage::BrowseSaveDir => {
            let initial = if !state.save_directory.is_empty() { PathBuf::from(&state.save_directory) }
                else if let Some(p) = directories::UserDirs::new().and_then(|u| u.picture_dir().map(|p| p.to_path_buf())) { p }
                else { std::env::temp_dir() };
            Task::perform(async move {
                rfd::AsyncFileDialog::new().set_directory(initial).pick_folder().await
                    .map(|h| h.path().to_path_buf())
            }, move |r| Message::Settings(id, SettingsMessage::BrowseSaveDirResult(r)))
        }
        SettingsMessage::BrowseSaveDirResult(p) => {
            if let Some(p) = p {
                state.save_directory = p.to_string_lossy().to_string();
            }
            Task::none()
        }
        SettingsMessage::StartWithOsToggled(b) => { state.start_with_os = b; Task::none() }
        SettingsMessage::WatermarkToggled(b)   => { state.watermark_enabled = b; Task::none() }
        SettingsMessage::WatermarkTextChanged(t) => { state.watermark_text = t; Task::none() }
        SettingsMessage::ThemeChanged(t) => { state.theme = t; Task::none() }

        SettingsMessage::Apply => {
            settings.hotkey_modifiers   = state.hotkey_modifiers;
            settings.hotkey_virtual_key = state.hotkey_virtual_key;
            settings.save_directory     = state.save_directory.trim().to_string();
            settings.start_with_windows = state.start_with_os;
            settings.watermark_enabled  = state.watermark_enabled;
            settings.watermark_text     = state.watermark_text.clone();
            settings.theme              = state.theme.clone();
            let _ = settings.save();
            if let Some(hk) = hotkeys {
                let _ = hk.register(settings.hotkey());
            }
            window::close::<Message>(id)
        }
        SettingsMessage::Cancel => window::close::<Message>(id),
    }
}

pub fn view<'a>(state: &'a SettingsState, _settings: &'a AppSettings, id: window::Id) -> Element<'a, Message> {
    let label = |s: &'static str| text(s).size(13);

    let hotkey_summary = describe_hotkey(state.hotkey_modifiers, state.hotkey_virtual_key);

    let body = column![
        label("Capture hotkey"),
        text_input(&hotkey_summary, &hotkey_summary).padding(8),
        text("Click and press the desired key combination. Backspace clears.")
            .size(11).color(Color::from_rgb8(110, 110, 120)),

        label("Default save folder"),
        row![
            text_input("Pictures folder", &state.save_directory)
                .on_input(move |s| Message::Settings(id, SettingsMessage::SaveDirChanged(s)))
                .padding(8)
                .width(Length::Fill),
            button(text("Browse…"))
                .on_press(Message::Settings(id, SettingsMessage::BrowseSaveDir))
                .padding(8),
        ].spacing(8),

        checkbox("Start with the operating system", state.start_with_os)
            .on_toggle(move |b| Message::Settings(id, SettingsMessage::StartWithOsToggled(b))),

        label("Theme"),
        pick_list(
            vec!["Light".to_string(), "Dark".to_string()],
            Some(state.theme.clone()),
            move |t| Message::Settings(id, SettingsMessage::ThemeChanged(t)),
        ),

        checkbox("Add watermark to images", state.watermark_enabled)
            .on_toggle(move |b| Message::Settings(id, SettingsMessage::WatermarkToggled(b))),

        label("Watermark text"),
        text_input("Watermark text", &state.watermark_text)
            .on_input(move |t| Message::Settings(id, SettingsMessage::WatermarkTextChanged(t)))
            .padding(8),

        row![
            iced::widget::Space::with_width(Length::Fill),
            button(text("Cancel")).on_press(Message::Settings(id, SettingsMessage::Cancel)).padding([8, 16]),
            button(text("Save")).on_press(Message::Settings(id, SettingsMessage::Apply)).padding([8, 16]),
        ].spacing(8),
    ]
    .spacing(12)
    .padding(20);

    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn describe_hotkey(mods: u32, vk: u32) -> String {
    if vk == 0 { return "(none)".into(); }
    let mut parts = Vec::new();
    if mods & 0x0002 != 0 { parts.push("Ctrl"); }
    if mods & 0x0004 != 0 { parts.push("Shift"); }
    if mods & 0x0001 != 0 { parts.push("Alt"); }
    if mods & 0x0008 != 0 { parts.push("Win"); }
    let key = match vk {
        0x2C => "PrintScreen".to_string(),
        v if (0x30..=0x39).contains(&v) => ((v - 0x30) as u8 + b'0').to_string(),
        v if (0x41..=0x5A).contains(&v) => ((v - 0x41) as u8 + b'A').to_string(),
        v if (0x70..=0x7B).contains(&v) => format!("F{}", v - 0x6F),
        _ => format!("VK 0x{vk:02X}"),
    };
    parts.push(&key);
    parts.join(" + ")
}
