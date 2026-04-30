//! Daemon-mode iced application — owns the tray, hotkey, settings, and the
//! map of currently-open windows (overlay / settings / about / pins).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use iced::{window, Element, Length, Subscription, Task, Theme};
use kashot_core::AppSettings;
use kashot_platform::{
    capture::capture_all_screens,
    hotkey::HotkeyManager,
    tray::{Tray, TrayEvent},
};

use super::message::{Message, OverlayMessage, PinMessage, SettingsMessage, SharedCapture};
use super::overlay::OverlayState;
use super::pin_window::PinState;
use super::settings_dialog::SettingsState;

/// Variants of windows the app can have open.
pub enum AppWindow {
    Overlay(OverlayState),
    Settings(SettingsState),
    About,
    Pin(PinState),
}

pub struct App {
    pub settings: AppSettings,
    pub windows:  HashMap<window::Id, AppWindow>,
    pub tray:     Option<Tray>,
    pub hotkeys:  Option<HotkeyManager>,
    pub capturing: bool,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let settings = AppSettings::load();

        // Tray + hotkey init are best-effort. If either fails (no DBus / no
        // desktop env / hotkey already taken) the app stays running so the
        // user can fix the issue and try again from the menu later.
        let tray = match Tray::new(tray_tooltip(&settings)) {
            Ok(t)  => Some(t),
            Err(e) => { eprintln!("tray init: {e}"); None }
        };

        let hotkeys = match HotkeyManager::new() {
            Ok(mut hk) => {
                if let Err(e) = hk.register(settings.hotkey()) {
                    eprintln!("hotkey register: {e}");
                }
                Some(hk)
            }
            Err(e) => { eprintln!("hotkey init: {e}"); None }
        };

        (
            App { settings, windows: HashMap::new(), tray, hotkeys, capturing: false },
            Task::none(),
        )
    }

    fn title(&self, id: window::Id) -> String {
        match self.windows.get(&id) {
            Some(AppWindow::Overlay(_))  => "Kashot — Capture".into(),
            Some(AppWindow::Settings(_)) => "Kashot Settings".into(),
            Some(AppWindow::About)       => "About Kashot".into(),
            Some(AppWindow::Pin(_))      => "Kashot — Pinned".into(),
            None                         => "Kashot".into(),
        }
    }

    fn theme(&self, _id: window::Id) -> Theme {
        match self.settings.theme().as_str() {
            "Dark" => Theme::Dark,
            _      => Theme::Light,
        }
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::Tick => self.on_tick(),

            Message::StartCapture => self.start_capture(),

            Message::CaptureReady(shared) => self.open_overlay(shared),

            Message::CaptureFailed(err) => {
                eprintln!("capture failed: {err}");
                self.capturing = false;
                Task::none()
            }

            Message::OpenSettings => self.open_settings(),
            Message::OpenAbout    => self.open_about(),
            Message::Exit         => iced::exit(),

            Message::Overlay(id, m)  => self.update_overlay(id, m),
            Message::Settings(id, m) => self.update_settings(id, m),
            Message::Pin(id, m)      => self.update_pin(id, m),

            Message::WindowClosed(id) => {
                self.windows.remove(&id);
                Task::none()
            }
        }
    }

    fn on_tick(&mut self) -> Task<Message> {
        // Drain tray events
        if let Some(tray) = &self.tray {
            match tray.try_recv() {
                TrayEvent::None      => {}
                TrayEvent::Capture   => return Task::done(Message::StartCapture),
                TrayEvent::Settings  => return Task::done(Message::OpenSettings),
                TrayEvent::About     => return Task::done(Message::OpenAbout),
                TrayEvent::Exit      => return Task::done(Message::Exit),
            }
        }
        // Drain hotkey events
        if let Some(hk) = &self.hotkeys {
            if hk.drain_pressed() {
                return Task::done(Message::StartCapture);
            }
        }
        Task::none()
    }

    fn start_capture(&mut self) -> Task<Message> {
        if self.capturing { return Task::none(); }
        self.capturing = true;
        Task::perform(
            async {
                // Brief delay so the tray menu / flyout can dismiss before we shoot.
                tokio::time::sleep(Duration::from_millis(200)).await;
                tokio::task::spawn_blocking(|| capture_all_screens())
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()))
            },
            |result| match result {
                Ok(captured) => Message::CaptureReady(Arc::new(captured)),
                Err(e)       => Message::CaptureFailed(e),
            },
        )
    }

    fn open_overlay(&mut self, shared: SharedCapture) -> Task<Message> {
        self.capturing = false;

        let state = OverlayState::new(shared, &self.settings);
        let (id, open) = window::open(window::Settings {
            position: window::Position::Specific(iced::Point::new(
                state.virtual_origin.0 as f32,
                state.virtual_origin.1 as f32,
            )),
            size: iced::Size::new(
                state.virtual_size.0 as f32,
                state.virtual_size.1 as f32,
            ),
            level:        window::Level::AlwaysOnTop,
            decorations:  false,
            transparent:  true,
            resizable:    false,
            visible:      true,
            ..Default::default()
        });

        self.windows.insert(id, AppWindow::Overlay(state));
        open.discard()
    }

    fn open_settings(&mut self) -> Task<Message> {
        let state = SettingsState::from(&self.settings);
        let (id, open) = window::open(window::Settings {
            size: iced::Size::new(560.0, 460.0),
            position: window::Position::Centered,
            resizable: false,
            ..Default::default()
        });
        self.windows.insert(id, AppWindow::Settings(state));
        open.discard()
    }

    fn open_about(&mut self) -> Task<Message> {
        let (id, open) = window::open(window::Settings {
            size: iced::Size::new(440.0, 380.0),
            position: window::Position::Centered,
            resizable: false,
            ..Default::default()
        });
        self.windows.insert(id, AppWindow::About);
        open.discard()
    }

    fn update_overlay(&mut self, id: window::Id, m: OverlayMessage) -> Task<Message> {
        let Some(AppWindow::Overlay(state)) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        super::overlay::update(state, &mut self.settings, id, m)
    }

    fn update_settings(&mut self, id: window::Id, m: SettingsMessage) -> Task<Message> {
        let Some(AppWindow::Settings(state)) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        super::settings_dialog::update(state, &mut self.settings, id, m, self.hotkeys.as_mut())
    }

    fn update_pin(&mut self, id: window::Id, m: PinMessage) -> Task<Message> {
        let Some(AppWindow::Pin(state)) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        super::pin_window::update(state, id, m)
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        match self.windows.get(&id) {
            Some(AppWindow::Overlay(state))  =>
                super::overlay::view(state, id),
            Some(AppWindow::Settings(state)) =>
                super::settings_dialog::view(state, &self.settings, id),
            Some(AppWindow::About)           =>
                super::about_dialog::view(),
            Some(AppWindow::Pin(state))      =>
                super::pin_window::view(state, id),
            None => iced::widget::container(iced::widget::text(""))
                .width(Length::Fill).height(Length::Fill).into(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch(vec![
            iced::time::every(Duration::from_millis(33)).map(|_| Message::Tick),
            window::close_events().map(Message::WindowClosed),
        ])
    }
}

pub fn run() -> iced::Result {
    iced::daemon(App::title, App::update, App::view)
        .subscription(App::subscription)
        .theme(App::theme)
        .run_with(App::new)
}

fn tray_tooltip(s: &AppSettings) -> String {
    let combo = describe_hotkey(s);
    format!("Kashot — press {combo} to capture")
}

fn describe_hotkey(s: &AppSettings) -> String {
    let vk = s.hotkey_virtual_key;
    let mut parts = Vec::new();
    if s.hotkey_modifiers & 0x0002 != 0 { parts.push("Ctrl"); }
    if s.hotkey_modifiers & 0x0004 != 0 { parts.push("Shift"); }
    if s.hotkey_modifiers & 0x0001 != 0 { parts.push("Alt"); }
    if s.hotkey_modifiers & 0x0008 != 0 { parts.push("Win"); }
    let key = vk_name(vk);
    if parts.is_empty() { key.into() } else { format!("{} + {}", parts.join(" + "), key) }
}

fn vk_name(vk: u32) -> &'static str {
    match vk {
        0x2C => "PrintScreen",
        0x70 => "F1",  0x71 => "F2",  0x72 => "F3",
        0x73 => "F4",  0x74 => "F5",  0x75 => "F6",
        0x76 => "F7",  0x77 => "F8",  0x78 => "F9",
        0x79 => "F10", 0x7A => "F11", 0x7B => "F12",
        _    => "(custom)",
    }
}
