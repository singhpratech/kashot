//! Procedural icon drawing for the toolbar. Used by the iced::canvas-based
//! tool buttons; each icon takes a `Frame` and a `Rectangle` to draw into.
//!
//! Mirrors the `IconXxx` static methods in `Kashot/OverlayForm.cs` — same
//! visual language so the two implementations look identical at a glance.

use iced::widget::canvas::path::Builder as PathBuilder;
use iced::widget::canvas::{Frame, Path, Stroke};
use iced::{Color, Point as IPoint, Rectangle, Size};

const WHITE: Color = Color::WHITE;

fn pen(c: Color) -> Stroke<'static> {
    Stroke {
        style: iced::widget::canvas::stroke::Style::Solid(c),
        width: 2.0,
        line_cap:  iced::widget::canvas::LineCap::Round,
        line_join: iced::widget::canvas::LineJoin::Round,
        ..Default::default()
    }
}

pub fn pen_icon(frame: &mut Frame, r: Rectangle) {
    let p = Path::line(
        IPoint::new(r.x + 5.0, r.y + r.height - 4.0),
        IPoint::new(r.x + r.width - 4.0, r.y + 4.0),
    );
    frame.stroke(&p, Stroke { width: 3.0, ..pen(WHITE) });
}

pub fn line_icon(frame: &mut Frame, r: Rectangle) {
    let p = Path::line(
        IPoint::new(r.x + 2.0, r.y + r.height - 2.0),
        IPoint::new(r.x + r.width - 2.0, r.y + 2.0),
    );
    frame.stroke(&p, pen(WHITE));
}

pub fn arrow_icon(frame: &mut Frame, r: Rectangle) {
    let p = Path::line(
        IPoint::new(r.x + 2.0, r.y + r.height - 2.0),
        IPoint::new(r.x + r.width - 2.0, r.y + 2.0),
    );
    frame.stroke(&p, pen(WHITE));
    // Arrowhead
    let mut b = PathBuilder::new();
    b.move_to(IPoint::new(r.x + r.width - 2.0, r.y + 2.0));
    b.line_to(IPoint::new(r.x + r.width - 6.0, r.y + 2.0));
    b.line_to(IPoint::new(r.x + r.width - 2.0, r.y + 6.0));
    b.close();
    let head = b.build();
    frame.fill(&head, WHITE);
}

pub fn rect_icon(frame: &mut Frame, r: Rectangle) {
    let p = Path::rectangle(
        IPoint::new(r.x + 2.0, r.y + 4.0),
        Size::new(r.width - 4.0, r.height - 8.0),
    );
    frame.stroke(&p, pen(WHITE));
}

pub fn ellipse_icon(frame: &mut Frame, r: Rectangle) {
    let cx = r.x + r.width / 2.0;
    let cy = r.y + r.height / 2.0;
    let mut b = PathBuilder::new();
    let n = 24;
    for i in 0..=n {
        let t = (i as f32) / (n as f32) * std::f32::consts::TAU;
        let x = cx + (r.width / 2.0 - 2.0) * t.cos();
        let y = cy + (r.height / 2.0 - 4.0) * t.sin();
        if i == 0 { b.move_to(IPoint::new(x, y)); }
        else      { b.line_to(IPoint::new(x, y)); }
    }
    let p = b.build();
    frame.stroke(&p, pen(WHITE));
}

pub fn marker_icon(frame: &mut Frame, r: Rectangle) {
    // Yellow highlighter stroke
    let stroke_yellow = Path::line(
        IPoint::new(r.x + 2.0, r.y + r.height / 2.0),
        IPoint::new(r.x + r.width - 2.0, r.y + r.height / 2.0),
    );
    frame.stroke(&stroke_yellow, Stroke {
        width: 6.0,
        style: iced::widget::canvas::stroke::Style::Solid(Color::from_rgba(1.0, 0.92, 0.0, 0.6)),
        line_cap:  iced::widget::canvas::LineCap::Round,
        ..Default::default()
    });
}

pub fn text_icon(frame: &mut Frame, r: Rectangle) {
    frame.fill_text(iced::widget::canvas::Text {
        content: "A".into(),
        position: IPoint::new(r.x + 4.0, r.y + 1.0),
        color: WHITE,
        size:  iced::Pixels(14.0),
        font:  iced::Font::DEFAULT,
        ..Default::default()
    });
}

pub fn step_icon(frame: &mut Frame, r: Rectangle) {
    let cx = r.x + r.width / 2.0;
    let cy = r.y + r.height / 2.0;
    let p = Path::circle(IPoint::new(cx, cy), (r.width / 2.0 - 3.0).max(1.0));
    frame.fill(&p, Color::from_rgba(1.0, 0.314, 0.314, 1.0));
    frame.stroke(&p, Stroke { width: 1.5, ..pen(WHITE) });
    frame.fill_text(iced::widget::canvas::Text {
        content: "1".into(),
        position: IPoint::new(cx - 3.0, cy - 7.0),
        color: WHITE,
        size:  iced::Pixels(11.0),
        font:  iced::Font::DEFAULT,
        ..Default::default()
    });
}

pub fn pixelate_icon(frame: &mut Frame, r: Rectangle) {
    let s = (r.width - 4.0) / 3.0;
    let shades = [0.9_f32, 0.35, 0.7, 0.45, 0.78, 0.25, 0.32, 0.85, 0.55];
    for (i, v) in shades.iter().enumerate() {
        let row = i / 3;
        let col = i % 3;
        let cell = Path::rectangle(
            IPoint::new(r.x + 2.0 + col as f32 * s, r.y + 2.0 + row as f32 * s),
            Size::new(s, s),
        );
        frame.fill(&cell, Color::from_rgba(*v, *v, *v, 1.0));
    }
}

pub fn pin_icon(frame: &mut Frame, r: Rectangle) {
    let cx = r.x + r.width / 2.0;
    // Head
    let head = Path::circle(IPoint::new(cx, r.y + 5.0), 4.0);
    frame.fill(&head, WHITE);
    // Stem
    let stem = Path::line(IPoint::new(cx, r.y + 9.0), IPoint::new(cx, r.y + r.height - 2.0));
    frame.stroke(&stem, Stroke { width: 1.5, ..pen(WHITE) });
}

pub fn copy_icon(frame: &mut Frame, r: Rectangle) {
    let p1 = Path::rectangle(IPoint::new(r.x + 2.0, r.y + 1.0), Size::new(10.0, 12.0));
    let p2 = Path::rectangle(IPoint::new(r.x + 6.0, r.y + 5.0), Size::new(10.0, 12.0));
    frame.stroke(&p1, Stroke { width: 1.5, ..pen(WHITE) });
    frame.stroke(&p2, Stroke { width: 1.5, ..pen(WHITE) });
}

pub fn save_icon(frame: &mut Frame, r: Rectangle) {
    let outer = Path::rectangle(IPoint::new(r.x + 2.0, r.y + 2.0), Size::new(r.width - 4.0, r.height - 4.0));
    frame.stroke(&outer, Stroke { width: 1.5, ..pen(WHITE) });
    let label = Path::rectangle(IPoint::new(r.x + 4.0, r.y + r.height - 7.0), Size::new(r.width - 8.0, 5.0));
    frame.fill(&label, WHITE);
}

pub fn close_icon(frame: &mut Frame, r: Rectangle) {
    let red = Color::from_rgba(1.0, 0.39, 0.39, 1.0);
    let a = Path::line(
        IPoint::new(r.x + 4.0, r.y + 4.0),
        IPoint::new(r.x + r.width - 4.0, r.y + r.height - 4.0),
    );
    let b = Path::line(
        IPoint::new(r.x + r.width - 4.0, r.y + 4.0),
        IPoint::new(r.x + 4.0, r.y + r.height - 4.0),
    );
    frame.stroke(&a, Stroke { width: 2.0, ..pen(red) });
    frame.stroke(&b, Stroke { width: 2.0, ..pen(red) });
}

pub fn undo_icon(frame: &mut Frame, r: Rectangle) {
    let cx = r.x + r.width / 2.0;
    let cy = r.y + r.height / 2.0 + 1.0;
    let mut b = PathBuilder::new();
    let radius = (r.width / 2.0) - 3.0;
    let start_angle = std::f32::consts::PI;
    let end_angle = start_angle + std::f32::consts::PI * 1.3;
    let n = 24;
    for i in 0..=n {
        let t = start_angle + (end_angle - start_angle) * (i as f32) / (n as f32);
        let x = cx + radius * t.cos();
        let y = cy + radius * t.sin();
        if i == 0 { b.move_to(IPoint::new(x, y)); }
        else      { b.line_to(IPoint::new(x, y)); }
    }
    frame.stroke(&b.build(), pen(WHITE));
    // Arrowhead
    let mut tip = PathBuilder::new();
    tip.move_to(IPoint::new(r.x + 4.0, r.y + 4.0));
    tip.line_to(IPoint::new(r.x + 2.0, r.y + 8.0));
    tip.line_to(IPoint::new(r.x + 8.0, r.y + 5.0));
    tip.close();
    frame.fill(&tip.build(), WHITE);
}

pub fn redo_icon(frame: &mut Frame, r: Rectangle) {
    let cx = r.x + r.width / 2.0;
    let cy = r.y + r.height / 2.0 + 1.0;
    let mut b = PathBuilder::new();
    let radius = (r.width / 2.0) - 3.0;
    let start_angle = std::f32::consts::PI * 2.0;
    let end_angle = start_angle - std::f32::consts::PI * 1.3;
    let n = 24;
    for i in 0..=n {
        let t = start_angle + (end_angle - start_angle) * (i as f32) / (n as f32);
        let x = cx + radius * t.cos();
        let y = cy + radius * t.sin();
        if i == 0 { b.move_to(IPoint::new(x, y)); }
        else      { b.line_to(IPoint::new(x, y)); }
    }
    frame.stroke(&b.build(), pen(WHITE));
    let mut tip = PathBuilder::new();
    tip.move_to(IPoint::new(r.x + r.width - 4.0, r.y + 4.0));
    tip.line_to(IPoint::new(r.x + r.width - 2.0, r.y + 8.0));
    tip.line_to(IPoint::new(r.x + r.width - 8.0, r.y + 5.0));
    tip.close();
    frame.fill(&tip.build(), WHITE);
}
