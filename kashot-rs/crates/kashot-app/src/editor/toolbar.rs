//! Floating tool panel + action panel + color picker.
//!
//! Both panels are positioned relative to the current selection rectangle —
//! tool panel on the right edge (flips left if it'd fall off-screen), action
//! panel below the bottom edge (flips above if it'd fall off-screen).

use iced::widget::{
    button, column, container, row, text, Space,
};
use iced::{window, Alignment, Background, Border, Color, Element, Length, Padding};
use kashot_core::annotation::Palettes;
use kashot_core::{Rgba, Tool};

use super::message::{Message, OverlayMessage};
use super::overlay::OverlayState;

const PANEL_BG:    Color = Color::from_rgba(0.176, 0.176, 0.176, 1.0);
const BUTTON_BG:   Color = Color::from_rgba(0.216, 0.216, 0.216, 1.0);
const BUTTON_HOV:  Color = Color::from_rgba(0.294, 0.294, 0.294, 1.0);
const BUTTON_SEL:  Color = Color::from_rgba(0.314, 0.314, 0.314, 1.0);

pub fn view(s: &OverlayState, id: window::Id) -> Element<'_, Message> {
    let tools = tool_panel(s, id);
    let actions = action_panel(id);

    let layout = column![
        // Top row: tool panel anchored to right of selection
        row![
            Space::with_width(Length::Fixed(s.selection.x + s.selection.w + 8.0)),
            tools,
        ],
        // Bottom row: action panel anchored below selection
        row![
            Space::with_width(Length::Fixed(s.selection.x.max(0.0))),
            actions,
        ],
    ]
    .spacing(8)
    .padding(Padding::new(0.0));

    container(layout)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(Padding {
            top:    s.selection.y.max(0.0),
            bottom: 0.0,
            left:   0.0,
            right:  0.0,
        })
        .into()
}

fn tool_panel(s: &OverlayState, id: window::Id) -> Element<'_, Message> {
    let mut col = column![]
        .spacing(2)
        .padding(4)
        .align_x(Alignment::Center);

    for t in Tool::ALL.iter().copied() {
        let selected = t == s.tool;
        col = col.push(tool_button(id, t, selected));
    }

    col = col.push(divider());
    col = col.push(color_button(s, id));
    col = col.push(thickness_button(id));
    col = col.push(divider());
    col = col.push(action_btn(id, "↶", "Undo (Ctrl+Z)", OverlayMessage::Undo));
    col = col.push(action_btn(id, "↷", "Redo (Ctrl+Y)", OverlayMessage::Redo));

    container(col)
        .padding(2)
        .style(|_| container::Style {
            background: Some(Background::Color(PANEL_BG)),
            border: Border { color: Color::TRANSPARENT, radius: 4.0.into(), width: 0.0 },
            ..Default::default()
        })
        .into()
}

fn action_panel(id: window::Id) -> Element<'static, Message> {
    let r = row![
        action_btn(id, "📌", "Pin to screen",  OverlayMessage::Pin),
        action_btn(id, "📋", "Copy (Ctrl+C)",   OverlayMessage::Copy),
        action_btn(id, "💾", "Save (Ctrl+S)",   OverlayMessage::Save),
        action_btn(id, "✕",  "Close (Esc)",     OverlayMessage::Cancel),
    ]
    .spacing(2)
    .padding(4)
    .align_y(Alignment::Center);

    container(r)
        .padding(2)
        .style(|_| container::Style {
            background: Some(Background::Color(PANEL_BG)),
            border: Border { color: Color::TRANSPARENT, radius: 4.0.into(), width: 0.0 },
            ..Default::default()
        })
        .into()
}

fn tool_button(id: window::Id, t: Tool, selected: bool) -> Element<'static, Message> {
    let label = format!("{}", short_label(t));
    let bg = if selected { BUTTON_SEL } else { BUTTON_BG };
    button(text(label).size(12).color(Color::WHITE))
        .padding([6, 8])
        .style(move |_, status| iced::widget::button::Style {
            background: Some(Background::Color(match status {
                iced::widget::button::Status::Hovered => BUTTON_HOV,
                _                                     => bg,
            })),
            text_color: Color::WHITE,
            border: Border { radius: 3.0.into(), ..Default::default() },
            ..Default::default()
        })
        .on_press(Message::Overlay(id, OverlayMessage::SelectTool(t)))
        .into()
}

fn short_label(t: Tool) -> &'static str {
    match t {
        Tool::Pen       => "✎ Pen",
        Tool::Line      => "／ Line",
        Tool::Arrow     => "→ Arrow",
        Tool::Rectangle => "▭ Rect",
        Tool::Ellipse   => "○ Ellipse",
        Tool::Marker    => "▬ Marker",
        Tool::Text      => "A Text",
        Tool::Step      => "① Step",
        Tool::Pixelate  => "▓ Blur",
    }
}

fn color_button(s: &OverlayState, id: window::Id) -> Element<'_, Message> {
    let c = s.color;
    let preview = container(text(""))
        .width(Length::Fixed(20.0))
        .height(Length::Fixed(14.0))
        .style(move |_| container::Style {
            background: Some(Background::Color(Color::from_rgba(
                c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0, 1.0,
            ))),
            border: Border { color: Color::WHITE, radius: 2.0.into(), width: 1.0 },
            ..Default::default()
        });

    let inner = row![preview, text("Color").size(11).color(Color::WHITE)]
        .spacing(6)
        .align_y(Alignment::Center);

    button(inner)
        .padding([6, 8])
        .style(|_, status| iced::widget::button::Style {
            background: Some(Background::Color(match status {
                iced::widget::button::Status::Hovered => BUTTON_HOV,
                _                                     => BUTTON_BG,
            })),
            text_color: Color::WHITE,
            border: Border { radius: 3.0.into(), ..Default::default() },
            ..Default::default()
        })
        .on_press(if s.color_picker_open {
            Message::Overlay(id, OverlayMessage::CloseColorPicker)
        } else {
            Message::Overlay(id, OverlayMessage::OpenColorPicker)
        })
        .into()
}

fn thickness_button(id: window::Id) -> Element<'static, Message> {
    button(text("Size").size(11).color(Color::WHITE))
        .padding([6, 8])
        .style(|_, status| iced::widget::button::Style {
            background: Some(Background::Color(match status {
                iced::widget::button::Status::Hovered => BUTTON_HOV,
                _                                     => BUTTON_BG,
            })),
            text_color: Color::WHITE,
            border: Border { radius: 3.0.into(), ..Default::default() },
            ..Default::default()
        })
        .on_press(Message::Overlay(id, OverlayMessage::CycleThickness))
        .into()
}

fn action_btn(id: window::Id, glyph: &'static str, label: &'static str, msg: OverlayMessage) -> Element<'static, Message> {
    let inner = row![
        text(glyph).size(14).color(Color::WHITE),
        text(label).size(11).color(Color::WHITE),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    button(inner)
        .padding([6, 10])
        .style(|_, status| iced::widget::button::Style {
            background: Some(Background::Color(match status {
                iced::widget::button::Status::Hovered => BUTTON_HOV,
                _                                     => BUTTON_BG,
            })),
            text_color: Color::WHITE,
            border: Border { radius: 3.0.into(), ..Default::default() },
            ..Default::default()
        })
        .on_press(Message::Overlay(id, msg))
        .into()
}

fn divider() -> Element<'static, Message> {
    container(text(""))
        .width(Length::Fixed(28.0))
        .height(Length::Fixed(1.0))
        .style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.275, 0.275, 0.275, 1.0))),
            ..Default::default()
        })
        .into()
}

// ── Color picker popup view (used as a separate layer in stack) ────────────

pub fn color_picker_popup(s: &OverlayState, id: window::Id) -> Element<'_, Message> {
    let palette = Palettes::get(s.palette_index);

    let header = row![
        button(text("‹").size(14).color(Color::WHITE))
            .on_press(Message::Overlay(id, OverlayMessage::PrevPalette))
            .padding([4, 8]),
        text(palette.name).size(13).color(Color::WHITE).width(Length::Fixed(120.0)),
        button(text("›").size(14).color(Color::WHITE))
            .on_press(Message::Overlay(id, OverlayMessage::NextPalette))
            .padding([4, 8]),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    let mut grid = column![].spacing(4);
    for chunk in palette.colors.chunks(4) {
        let mut r = row![].spacing(4);
        for c in chunk {
            let rgba = *c;
            r = r.push(
                button(container(text("")).width(Length::Fixed(36.0)).height(Length::Fixed(28.0)))
                    .style(move |_, status| iced::widget::button::Style {
                        background: Some(Background::Color(Color::from_rgba(
                            rgba.r as f32 / 255.0,
                            rgba.g as f32 / 255.0,
                            rgba.b as f32 / 255.0,
                            1.0,
                        ))),
                        border: Border {
                            color: if matches!(status, iced::widget::button::Status::Hovered) {
                                Color::WHITE
                            } else {
                                Color::from_rgba(0.4, 0.4, 0.4, 1.0)
                            },
                            radius: 2.0.into(),
                            width: 1.5,
                        },
                        ..Default::default()
                    })
                    .on_press(Message::Overlay(id, OverlayMessage::PickColor(rgba))),
            );
        }
        grid = grid.push(r);
    }

    let body = column![header, grid].spacing(8).padding(8).align_x(Alignment::Center);

    container(body)
        .style(|_| container::Style {
            background: Some(Background::Color(PANEL_BG)),
            border: Border { color: Color::from_rgba(0.4, 0.4, 0.4, 1.0), radius: 4.0.into(), width: 1.0 },
            ..Default::default()
        })
        .into()
}
