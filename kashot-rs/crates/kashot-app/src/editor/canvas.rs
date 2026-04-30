//! `iced::widget::canvas::Program` implementation for the overlay surface.
//!
//! Draws (in order):
//!   1. screenshot covering the whole virtual desktop
//!   2. semi-transparent dim
//!   3. "punch" the selection by redrawing the screenshot inside it
//!   4. dashed selection border
//!   5. annotations clipped to the selection
//!   6. resize handles (8 of them) when the selection is editable
//!   7. dimension label below the selection
//!   8. crosshair + 4× magnifier (idle / selecting only)
//!
//! Mouse + keyboard events flow back to the overlay's `update` via
//! `OverlayMessage`s.

use iced::mouse::{Cursor, Interaction};
use iced::widget::canvas::{event::Status, Cache, Event, Frame, Geometry, Path, Program, Stroke, Text};
use iced::{keyboard, Color, Pixels, Point as IPoint, Rectangle, Renderer, Size, Theme, Vector};
use kashot_core::annotation::Point2;
use kashot_core::state::{hit_test_edge, Edge};

use super::message::{Key, KeyMods, Message, MouseButton, OverlayMessage};
use super::overlay::{OverlayState, State};
use super::render;

pub struct OverlayCanvas<'a> {
    pub state: &'a OverlayState,
}

impl<'a, Theme_> Program<Message, Theme_> for OverlayCanvas<'a> {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme_,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());

        // 1. screenshot background
        render::screenshot_background(&mut frame, self.state, bounds);

        // 2. dim everything
        let dim = Path::rectangle(IPoint::ORIGIN, bounds.size());
        frame.fill(&dim, Color::from_rgba(0.0, 0.0, 0.0, 0.40));

        let sel = self.state.current_selection();
        if sel.w > 0.0 && sel.h > 0.0 {
            // 3. cut the dim out (redraw the screenshot inside selection)
            render::screenshot_inside_selection(&mut frame, self.state, sel);

            // 4. dashed border
            let border_path = Path::rectangle(
                IPoint::new(sel.x, sel.y),
                Size::new(sel.w, sel.h),
            );
            frame.stroke(&border_path, Stroke {
                style: iced::widget::canvas::stroke::Style::Solid(Color::from_rgb8(100, 149, 237)),
                width: 1.5,
                line_dash: iced::widget::canvas::stroke::LineDash { segments: &[4.0, 4.0], offset: 0 },
                ..Default::default()
            });

            // 5. annotations clipped to selection
            render::annotations(&mut frame, self.state, sel);

            // 6. resize handles
            if matches!(self.state.state, State::Selected | State::Resizing | State::Moving) {
                render::resize_handles(&mut frame, sel);
            }

            // 7. dimension label
            render::dimension_label(&mut frame, sel, bounds);
        }

        // 8. crosshair + magnifier (idle / selecting only)
        if matches!(self.state.state, State::Idle | State::Selecting) {
            render::crosshair(&mut frame, self.state.cursor, bounds);
            render::magnifier(&mut frame, self.state, bounds);
        }

        vec![frame.into_geometry()]
    }

    fn update(
        &self,
        _state: &mut Self::State,
        event: Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> (Status, Option<Message>) {
        let id_placeholder = iced::window::Id::unique(); // not really used — we route via window::Id externally
        let p = match cursor.position_in(bounds) {
            Some(pt) => Point2::new(pt.x, pt.y),
            None     => return (Status::Ignored, None),
        };

        match event {
            Event::Mouse(iced::mouse::Event::ButtonPressed(b)) => {
                let button = translate_button(b);
                let mods = KeyMods::default();
                (
                    Status::Captured,
                    Some(Message::Overlay(id_placeholder, OverlayMessage::MouseDown { p, button, mods })),
                )
            }
            Event::Mouse(iced::mouse::Event::CursorMoved { .. }) => (
                Status::Captured,
                Some(Message::Overlay(id_placeholder, OverlayMessage::MouseMove { p })),
            ),
            Event::Mouse(iced::mouse::Event::ButtonReleased(b)) => {
                let button = translate_button(b);
                (
                    Status::Captured,
                    Some(Message::Overlay(id_placeholder, OverlayMessage::MouseUp { p, button })),
                )
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                let mods = KeyMods {
                    ctrl: modifiers.control(),
                    shift: modifiers.shift(),
                    alt: modifiers.alt(),
                    logo: modifiers.logo(),
                };
                let k = match key {
                    keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Key::Escape),
                    keyboard::Key::Named(keyboard::key::Named::Enter)  => Some(Key::Enter),
                    keyboard::Key::Character(c) => c.chars().next().map(Key::Char),
                    _ => None,
                };
                if let Some(k) = k {
                    (
                        Status::Captured,
                        Some(Message::Overlay(id_placeholder, OverlayMessage::KeyPress { key: k, mods })),
                    )
                } else {
                    (Status::Ignored, None)
                }
            }
            _ => (Status::Ignored, None),
        }
    }

    fn mouse_interaction(&self, _state: &Self::State, bounds: Rectangle, cursor: Cursor) -> Interaction {
        let Some(p) = cursor.position_in(bounds) else { return Interaction::default(); };
        match self.state.state {
            State::Idle | State::Selecting => Interaction::Crosshair,
            State::Selected => {
                let edge = hit_test_edge(
                    (self.state.selection.x, self.state.selection.y, self.state.selection.w, self.state.selection.h),
                    (p.x, p.y),
                );
                match edge {
                    Edge::Left | Edge::Right => Interaction::ResizingHorizontally,
                    Edge::Top  | Edge::Bottom => Interaction::ResizingVertically,
                    Edge::TopLeft | Edge::BottomRight | Edge::TopRight | Edge::BottomLeft => Interaction::Grab,
                    Edge::None => {
                        let inside = p.x >= self.state.selection.x
                            && p.x <= self.state.selection.x + self.state.selection.w
                            && p.y >= self.state.selection.y
                            && p.y <= self.state.selection.y + self.state.selection.h;
                        if inside { Interaction::Crosshair } else { Interaction::default() }
                    }
                }
            }
            _ => Interaction::default(),
        }
    }
}

fn translate_button(b: iced::mouse::Button) -> MouseButton {
    use iced::mouse::Button;
    match b {
        Button::Left   => MouseButton::Left,
        Button::Right  => MouseButton::Right,
        Button::Middle => MouseButton::Middle,
        _              => MouseButton::Left,
    }
}
