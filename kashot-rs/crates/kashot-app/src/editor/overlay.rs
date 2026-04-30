//! Overlay window state + iced view.
//!
//! Mirrors the C# `OverlayForm` state machine:
//!   `Idle → Selecting → Selected → { Drawing | TextInput | Resizing | Moving }`

use iced::widget::{column, container, row, stack, text_input};
use iced::{window, Color, Element, Length, Task};
use kashot_core::annotation::{Annotation, AnnotationKind, Point2, Rect, Stroke};
use kashot_core::{state::Edge, AppSettings, Rgba, Tool};

use super::canvas::OverlayCanvas;
use super::message::{Key, KeyMods, Message, MouseButton, OverlayMessage, SharedCapture};
use super::toolbar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Selecting,
    Selected,
    Drawing,
    TextInput,
    Resizing,
    Moving,
}

pub struct OverlayState {
    pub captured:        SharedCapture,
    pub virtual_origin:  (i32, i32),
    pub virtual_size:    (u32, u32),

    pub state:           State,
    pub tool:            Tool,
    pub color:           Rgba,
    pub thickness:       f32,
    pub palette_index:   usize,

    pub selection_start:   Point2,
    pub selection_current: Point2,
    pub selection:         Rect,
    pub start_rect:        Rect,
    pub resize_edge:       Edge,

    pub annotations: Vec<Annotation>,
    pub redo_stack:  Vec<Annotation>,
    pub active:      Option<Annotation>,

    pub cursor:           Point2,
    pub color_picker_open: bool,
    pub text_buffer:       String,
    pub text_position:     Point2,
}

impl OverlayState {
    pub fn new(captured: SharedCapture, settings: &AppSettings) -> Self {
        let virtual_origin = captured.virtual_origin;
        let virtual_size   = (captured.bitmap.width(), captured.bitmap.height());
        OverlayState {
            captured,
            virtual_origin,
            virtual_size,
            state:             State::Idle,
            tool:              parse_tool(&settings.last_tool),
            color:             Rgba::from_argb(settings.last_color_argb),
            thickness:         settings.last_thickness,
            palette_index:     settings.palette_index.max(0).min(3) as usize,
            selection_start:   Point2::new(0.0, 0.0),
            selection_current: Point2::new(0.0, 0.0),
            selection:         Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
            start_rect:        Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
            resize_edge:       Edge::None,
            annotations:       Vec::new(),
            redo_stack:        Vec::new(),
            active:            None,
            cursor:            Point2::new(0.0, 0.0),
            color_picker_open: false,
            text_buffer:       String::new(),
            text_position:     Point2::new(0.0, 0.0),
        }
    }

    pub fn current_selection(&self) -> Rect {
        match self.state {
            State::Idle      => Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
            State::Selecting => Rect::from_corners(self.selection_start, self.selection_current),
            _                => self.selection,
        }
    }

    pub fn add_annotation(&mut self, a: Annotation) {
        self.annotations.push(a);
        self.redo_stack.clear();
    }

    pub fn undo(&mut self) {
        if let Some(a) = self.annotations.pop() {
            self.redo_stack.push(a);
        }
    }

    pub fn redo(&mut self) {
        if let Some(a) = self.redo_stack.pop() {
            self.annotations.push(a);
        }
    }

    fn current_stroke(&self) -> Stroke {
        Stroke { color: self.color, thickness: self.thickness }
    }

    fn next_step_number(&self) -> u32 {
        let n = self.annotations.iter().filter(|a| matches!(a.kind, AnnotationKind::Step { .. })).count();
        (n + 1) as u32
    }
}

fn parse_tool(s: &str) -> Tool {
    match s {
        "Line"      => Tool::Line,
        "Arrow"     => Tool::Arrow,
        "Rectangle" => Tool::Rectangle,
        "Ellipse"   => Tool::Ellipse,
        "Marker"    => Tool::Marker,
        "Text"      => Tool::Text,
        "Step"      => Tool::Step,
        "Pixelate"  => Tool::Pixelate,
        _           => Tool::Pen,
    }
}

// ── update ─────────────────────────────────────────────────────────────────

pub fn update(
    s: &mut OverlayState,
    settings: &mut AppSettings,
    id: window::Id,
    m: OverlayMessage,
) -> Task<Message> {
    match m {
        OverlayMessage::MouseDown { p, button, mods } => {
            handle_mouse_down(s, p, button, mods);
            Task::none()
        }
        OverlayMessage::MouseMove { p } => {
            s.cursor = p;
            handle_mouse_move(s, p);
            Task::none()
        }
        OverlayMessage::MouseUp { p, button } => {
            handle_mouse_up(s, p, button);
            Task::none()
        }
        OverlayMessage::KeyPress { key, mods } => {
            handle_key(s, settings, id, key, mods)
        }
        OverlayMessage::SelectTool(t) => {
            s.tool = t;
            s.color_picker_open = false;
            Task::none()
        }
        OverlayMessage::PickColor(c) => {
            s.color = c;
            s.color_picker_open = false;
            Task::none()
        }
        OverlayMessage::OpenColorPicker  => { s.color_picker_open = true;  Task::none() }
        OverlayMessage::CloseColorPicker => { s.color_picker_open = false; Task::none() }
        OverlayMessage::NextPalette      => { s.palette_index = (s.palette_index + 1) % 4; Task::none() }
        OverlayMessage::PrevPalette      => { s.palette_index = (s.palette_index + 3) % 4; Task::none() }
        OverlayMessage::CycleThickness   => {
            const SIZES: [f32; 5] = [1.0, 2.0, 3.0, 5.0, 8.0];
            let i = SIZES.iter().position(|t| (*t - s.thickness).abs() < 0.01).unwrap_or(2);
            s.thickness = SIZES[(i + 1) % SIZES.len()];
            Task::none()
        }
        OverlayMessage::Undo => { s.undo(); Task::none() }
        OverlayMessage::Redo => { s.redo(); Task::none() }

        OverlayMessage::Save => super::save::save(s, settings, id),
        OverlayMessage::Copy => super::save::copy(s, id),
        OverlayMessage::Pin  => super::save::pin(s, id),

        OverlayMessage::Cancel => {
            persist_settings_from(s, settings);
            window::close::<Message>(id)
        }

        OverlayMessage::SaveResult(Ok(_path)) => {
            persist_settings_from(s, settings);
            window::close::<Message>(id)
        }
        OverlayMessage::SaveResult(Err(err)) => {
            eprintln!("save failed: {err}");
            Task::none()
        }
        OverlayMessage::CopyResult(Ok(())) => {
            persist_settings_from(s, settings);
            window::close::<Message>(id)
        }
        OverlayMessage::CopyResult(Err(err)) => {
            eprintln!("copy failed: {err}");
            Task::none()
        }

        OverlayMessage::TextChanged(t) => { s.text_buffer = t; Task::none() }
        OverlayMessage::TextCommitted(t) => {
            if !t.trim().is_empty() {
                s.add_annotation(Annotation::text(s.color, s.text_position, t));
            }
            s.text_buffer.clear();
            s.state = State::Selected;
            Task::none()
        }
        OverlayMessage::TextCancelled => {
            s.text_buffer.clear();
            s.state = State::Selected;
            Task::none()
        }
    }
}

fn persist_settings_from(s: &OverlayState, settings: &mut AppSettings) {
    settings.last_tool        = format!("{:?}", s.tool);
    settings.last_color_argb  = s.color.to_argb();
    settings.last_thickness   = s.thickness;
    settings.palette_index    = s.palette_index as i32;
    let _ = settings.save();
}

fn handle_mouse_down(s: &mut OverlayState, p: Point2, button: MouseButton, mods: KeyMods) {
    if button == MouseButton::Right {
        match s.state {
            State::Drawing => { s.active = None; s.state = State::Selected; }
            State::TextInput => { s.text_buffer.clear(); s.state = State::Selected; }
            _ => { /* close requested via Cancel */ }
        }
        return;
    }
    if button != MouseButton::Left { return; }
    s.color_picker_open = false;

    match s.state {
        State::Idle => {
            s.selection_start = p;
            s.selection_current = p;
            s.state = State::Selecting;
        }
        State::Selected => {
            let edge = kashot_core::state::hit_test_edge(
                (s.selection.x, s.selection.y, s.selection.w, s.selection.h), (p.x, p.y));
            if edge != Edge::None {
                s.resize_edge = edge;
                s.selection_start = p;
                s.start_rect = s.selection;
                s.state = State::Resizing;
            } else if mods.alt && s.selection.contains(p) {
                s.selection_start = p;
                s.start_rect = s.selection;
                s.state = State::Moving;
            } else if s.selection.contains(p) {
                if s.tool == Tool::Text {
                    s.text_position = p;
                    s.text_buffer.clear();
                    s.state = State::TextInput;
                } else {
                    start_drawing(s, p);
                }
            } else {
                // Started a new selection — wipe the editor
                s.annotations.clear();
                s.redo_stack.clear();
                s.selection_start = p;
                s.selection_current = p;
                s.state = State::Selecting;
            }
        }
        _ => {}
    }
}

fn handle_mouse_move(s: &mut OverlayState, p: Point2) {
    match s.state {
        State::Selecting => {
            s.selection_current = p;
        }
        State::Drawing => {
            if let Some(a) = &mut s.active { a.extend(p); }
        }
        State::Resizing => {
            update_resize(s, p);
        }
        State::Moving => {
            update_move(s, p);
        }
        _ => {}
    }
}

fn handle_mouse_up(s: &mut OverlayState, _p: Point2, button: MouseButton) {
    if button != MouseButton::Left { return; }
    match s.state {
        State::Selecting => finalize_selection(s),
        State::Drawing   => finalize_drawing(s),
        State::Resizing | State::Moving => {
            s.resize_edge = Edge::None;
            s.state = State::Selected;
        }
        _ => {}
    }
}

fn finalize_selection(s: &mut OverlayState) {
    let r = Rect::from_corners(s.selection_start, s.selection_current);
    if r.w < 5.0 || r.h < 5.0 {
        s.state = State::Idle;
        return;
    }
    s.selection = r;
    s.state = State::Selected;
}

fn start_drawing(s: &mut OverlayState, p: Point2) {
    let stroke = s.current_stroke();
    let active = match s.tool {
        Tool::Pen       => Some(Annotation::pen(stroke, p)),
        Tool::Line      => Some(Annotation::line(stroke, p)),
        Tool::Arrow     => Some(Annotation::arrow(stroke, p)),
        Tool::Rectangle => Some(Annotation::rectangle(stroke, p)),
        Tool::Ellipse   => Some(Annotation::ellipse(stroke, p)),
        Tool::Marker    => Some(Annotation::marker(stroke, p)),
        Tool::Pixelate  => Some(Annotation::pixelate(p)),
        Tool::Step      => {
            // Click-to-place — finalize immediately, no Drawing state.
            let n = s.next_step_number();
            s.add_annotation(Annotation::step(s.color, p, n));
            return;
        }
        Tool::Text => {
            s.text_position = p;
            s.text_buffer.clear();
            s.state = State::TextInput;
            return;
        }
    };

    if let Some(a) = active {
        s.active = Some(a);
        s.state  = State::Drawing;
    }
}

fn finalize_drawing(s: &mut OverlayState) {
    if let Some(a) = s.active.take() { s.add_annotation(a); }
    s.state = State::Selected;
}

fn update_resize(s: &mut OverlayState, p: Point2) {
    let r0 = s.start_rect;
    let mut left = r0.x;
    let mut top  = r0.y;
    let mut right = r0.x + r0.w;
    let mut bottom = r0.y + r0.h;
    let dx = p.x - s.selection_start.x;
    let dy = p.y - s.selection_start.y;
    match s.resize_edge {
        Edge::Left        => left   += dx,
        Edge::Right       => right  += dx,
        Edge::Top         => top    += dy,
        Edge::Bottom      => bottom += dy,
        Edge::TopLeft     => { left  += dx; top    += dy; }
        Edge::TopRight    => { right += dx; top    += dy; }
        Edge::BottomLeft  => { left  += dx; bottom += dy; }
        Edge::BottomRight => { right += dx; bottom += dy; }
        Edge::None        => return,
    }
    if right < left { std::mem::swap(&mut left, &mut right); }
    if bottom < top { std::mem::swap(&mut top, &mut bottom); }
    s.selection = Rect { x: left, y: top, w: right - left, h: bottom - top };
}

fn update_move(s: &mut OverlayState, p: Point2) {
    let dx = p.x - s.selection_start.x;
    let dy = p.y - s.selection_start.y;
    s.selection = Rect {
        x: s.start_rect.x + dx,
        y: s.start_rect.y + dy,
        w: s.start_rect.w,
        h: s.start_rect.h,
    };
}

fn handle_key(
    s: &mut OverlayState,
    settings: &mut AppSettings,
    id: window::Id,
    key: Key,
    mods: KeyMods,
) -> Task<Message> {
    if key == Key::Escape {
        match s.state {
            State::TextInput => { s.text_buffer.clear(); s.state = State::Selected; return Task::none(); }
            State::Drawing   => { s.active = None; s.state = State::Selected; return Task::none(); }
            _ => {
                persist_settings_from(s, settings);
                return window::close::<Message>(id);
            }
        }
    }

    if mods.ctrl && !mods.shift && !mods.alt {
        match key {
            Key::Char('z') if !mods.shift => { s.undo(); return Task::none(); }
            Key::Char('y') => { s.redo(); return Task::none(); }
            Key::Char('c') if s.state == State::Selected => return super::save::copy(s, id),
            Key::Char('s') if s.state == State::Selected => return super::save::save(s, settings, id),
            _ => {}
        }
    }

    if mods.ctrl && mods.shift && key == Key::Char('z') {
        s.redo();
        return Task::none();
    }

    if !mods.ctrl && !mods.alt && !mods.shift && s.state == State::Selected {
        if let Key::Char(c) = key {
            if let Some(t) = Tool::from_key(c) {
                s.tool = t;
            }
        }
    }
    Task::none()
}

// ── view ──────────────────────────────────────────────────────────────────

pub fn view(s: &OverlayState, _id: window::Id) -> Element<'_, Message> {
    let canvas: Element<'_, Message> = iced::widget::canvas(OverlayCanvas { state: s })
        .width(Length::Fill)
        .height(Length::Fill)
        .into();

    let mut layers = vec![canvas];

    if s.state == State::TextInput {
        let editor = column![
            text_input("Type and press Enter…", &s.text_buffer)
                .on_input(|t| Message::Overlay(_id, OverlayMessage::TextChanged(t)))
                .on_submit(Message::Overlay(_id, OverlayMessage::TextCommitted(s.text_buffer.clone())))
                .padding(8)
                .size(16),
        ]
        .padding([s.text_position.y as u16, 0, 0, s.text_position.x as u16]);
        layers.push(container(editor).width(Length::Fill).height(Length::Fill).into());
    }

    if matches!(s.state, State::Selected | State::Drawing | State::TextInput | State::Resizing | State::Moving) {
        let toolbars = toolbar::view(s, _id);
        layers.push(toolbars);
    }

    container(stack(layers))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| iced::widget::container::Style {
            background: Some(Color::TRANSPARENT.into()),
            ..Default::default()
        })
        .into()
}

pub fn rect_to_iced(r: Rect) -> iced::Rectangle {
    iced::Rectangle { x: r.x, y: r.y, width: r.w, height: r.h }
}
