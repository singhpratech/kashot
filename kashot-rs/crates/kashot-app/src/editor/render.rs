//! Drawing helpers for the overlay canvas. Annotations, dim cutout, handles,
//! crosshair, magnifier, dimension label.
//!
//! All inputs are in window-local pixel coordinates, which match virtual-screen
//! coordinates 1:1 because the overlay window is positioned at the virtual
//! origin and sized to the virtual bounds.

use iced::widget::canvas::path::Builder as PathBuilder;
use iced::widget::canvas::{Fill, Frame, Path, Stroke, Text};
use iced::{Color, Pixels, Point as IPoint, Rectangle, Size, Vector};
use kashot_core::annotation::{Annotation, AnnotationKind, Point2, Rect};
use kashot_core::Rgba;

use super::overlay::OverlayState;

// ── colour helpers ─────────────────────────────────────────────────────────

fn iced_color(c: Rgba) -> Color {
    Color::from_rgba(
        c.r as f32 / 255.0,
        c.g as f32 / 255.0,
        c.b as f32 / 255.0,
        c.a as f32 / 255.0,
    )
}

// ── screenshot background ──────────────────────────────────────────────────
//
// In our layout the screenshot is drawn by an `iced::widget::image` below the
// canvas in a Stack — the canvas only handles overlays. These functions are
// kept as no-ops so the calling code reads top-to-bottom like the C# version.

pub fn screenshot_background(_frame: &mut Frame, _state: &OverlayState, _bounds: Rectangle) {
    // image widget below us renders the screenshot — nothing to do here.
}

pub fn screenshot_inside_selection(_frame: &mut Frame, _state: &OverlayState, _sel: Rect) {
    // The dim is drawn with EvenOdd fill (see `dim_with_cutout`) so the
    // selection naturally shows the underlying screenshot. Nothing extra here.
}

// ── dim with selection cutout ──────────────────────────────────────────────

/// Replace the plain dim with a compound path that excludes the selection,
/// so the screenshot under the selection is fully visible.
pub fn dim_with_cutout(frame: &mut Frame, sel: Rect, bounds: Rectangle) {
    let mut b = PathBuilder::new();
    b.move_to(IPoint::new(0.0, 0.0));
    b.line_to(IPoint::new(bounds.width, 0.0));
    b.line_to(IPoint::new(bounds.width, bounds.height));
    b.line_to(IPoint::new(0.0, bounds.height));
    b.close();

    if sel.w > 0.0 && sel.h > 0.0 {
        b.move_to(IPoint::new(sel.x, sel.y));
        b.line_to(IPoint::new(sel.x + sel.w, sel.y));
        b.line_to(IPoint::new(sel.x + sel.w, sel.y + sel.h));
        b.line_to(IPoint::new(sel.x, sel.y + sel.h));
        b.close();
    }

    let path = b.build();
    frame.fill(&path, Fill {
        style: Color::from_rgba(0.0, 0.0, 0.0, 0.40).into(),
        rule:  iced::widget::canvas::fill::Rule::EvenOdd,
    });
}

// ── annotations clipped to selection ───────────────────────────────────────

pub fn annotations(frame: &mut Frame, state: &OverlayState, sel: Rect) {
    let clip = Rectangle { x: sel.x, y: sel.y, width: sel.w, height: sel.h };

    // iced::Frame doesn't have native clip; nest within `with_save` and
    // hand-clip every shape — for v1 we just draw and accept slight overdraw
    // outside the selection, which is masked anyway by the dim layer.
    for a in &state.annotations { draw_annotation(frame, a); }
    if let Some(a) = &state.active { draw_annotation(frame, a); }
    let _ = clip;
}

fn draw_annotation(frame: &mut Frame, a: &Annotation) {
    match &a.kind {
        AnnotationKind::Pen { stroke, points } => draw_polyline(frame, stroke.color, stroke.thickness, points),
        AnnotationKind::Marker { stroke, points } => {
            let c = stroke.color.with_alpha(80);
            draw_polyline(frame, c, stroke.thickness, points)
        }
        AnnotationKind::Line { stroke, start, end } => {
            let p = Path::line(IPoint::new(start.x, start.y), IPoint::new(end.x, end.y));
            frame.stroke(&p, Stroke { width: stroke.thickness, ..stroke_default(stroke.color) });
        }
        AnnotationKind::Arrow { stroke, start, end } => draw_arrow(frame, stroke.color, stroke.thickness, *start, *end),
        AnnotationKind::Rectangle { stroke, start, end } => {
            let r = Rect::from_corners(*start, *end);
            if r.is_empty() { return; }
            let p = Path::rectangle(IPoint::new(r.x, r.y), Size::new(r.w, r.h));
            frame.stroke(&p, Stroke { width: stroke.thickness, ..stroke_default(stroke.color) });
        }
        AnnotationKind::Ellipse { stroke, start, end } => {
            let r = Rect::from_corners(*start, *end);
            if r.is_empty() { return; }
            let cx = r.x + r.w / 2.0;
            let cy = r.y + r.h / 2.0;
            let mut b = PathBuilder::new();
            ellipse_path(&mut b, cx, cy, r.w / 2.0, r.h / 2.0);
            let p = b.build();
            frame.stroke(&p, Stroke { width: stroke.thickness, ..stroke_default(stroke.color) });
        }
        AnnotationKind::Text { color, position, text, font_size } => {
            let shadow = Color::from_rgba(0.0, 0.0, 0.0, 0.24);
            frame.fill_text(Text {
                content:   text.clone(),
                position:  IPoint::new(position.x + 1.0, position.y + 1.0),
                color:     shadow,
                size:      Pixels(*font_size),
                font:      iced::Font::DEFAULT,
                ..Default::default()
            });
            frame.fill_text(Text {
                content:   text.clone(),
                position:  IPoint::new(position.x, position.y),
                color:     iced_color(*color),
                size:      Pixels(*font_size),
                font:      iced::Font::DEFAULT,
                ..Default::default()
            });
        }
        AnnotationKind::Step { color, center, number } => {
            let radius = 14.0;
            let p = Path::circle(IPoint::new(center.x, center.y), radius);
            frame.fill(&p, iced_color(*color));
            frame.stroke(&p, Stroke { width: 2.0, ..stroke_default(Rgba::WHITE) });
            frame.fill_text(Text {
                content: number.to_string(),
                position: IPoint::new(center.x - 5.0, center.y - 9.0),
                color:    Color::WHITE,
                size:     Pixels(14.0),
                font:     iced::Font::DEFAULT,
                ..Default::default()
            });
        }
        AnnotationKind::Pixelate { start, end, .. } => {
            // Without per-pixel access in canvas::Frame we draw a translucent
            // mosaic-effect rectangle as a stand-in. For pixel-perfect blur the
            // final image is composited by `save::final_image`, which uses the
            // raw bitmap.
            let r = Rect::from_corners(*start, *end);
            if r.is_empty() { return; }
            let p = Path::rectangle(IPoint::new(r.x, r.y), Size::new(r.w, r.h));
            frame.fill(&p, Color::from_rgba(0.5, 0.5, 0.5, 0.55));
            // Procedural mosaic dots so it reads as "redacted" on screen
            let block = 8.0_f32;
            let cols = (r.w / block).floor() as i32;
            let rows = (r.h / block).floor() as i32;
            for row in 0..rows {
                for col in 0..cols {
                    let v = ((row * 31 + col * 17) % 5) as f32 / 5.0;
                    let cell = Path::rectangle(
                        IPoint::new(r.x + col as f32 * block, r.y + row as f32 * block),
                        Size::new(block - 1.0, block - 1.0),
                    );
                    frame.fill(&cell, Color::from_rgba(v, v, v, 0.6));
                }
            }
        }
    }
}

fn stroke_default(color: Rgba) -> Stroke<'static> {
    Stroke {
        style: iced::widget::canvas::stroke::Style::Solid(iced_color(color)),
        line_cap: iced::widget::canvas::LineCap::Round,
        line_join: iced::widget::canvas::LineJoin::Round,
        ..Default::default()
    }
}

fn draw_polyline(frame: &mut Frame, color: Rgba, width: f32, points: &[Point2]) {
    if points.len() < 2 { return; }
    let mut b = PathBuilder::new();
    b.move_to(IPoint::new(points[0].x, points[0].y));
    for p in &points[1..] { b.line_to(IPoint::new(p.x, p.y)); }
    let p = b.build();
    frame.stroke(&p, Stroke { width, ..stroke_default(color) });
}

fn draw_arrow(frame: &mut Frame, color: Rgba, width: f32, start: Point2, end: Point2) {
    // Shaft
    let shaft = Path::line(IPoint::new(start.x, start.y), IPoint::new(end.x, end.y));
    frame.stroke(&shaft, Stroke { width, ..stroke_default(color) });

    // Arrowhead — equilateral-ish triangle of side ~3*width pointing toward `end`
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = (dx * dx + dy * dy).sqrt().max(0.0001);
    let ux = dx / len;
    let uy = dy / len;
    let head_len = (width + 3.0) * 2.5;
    let head_w   = (width + 3.0) * 1.5;
    let bx = end.x - ux * head_len;
    let by = end.y - uy * head_len;
    let px = -uy;
    let py =  ux;
    let p1 = IPoint::new(end.x, end.y);
    let p2 = IPoint::new(bx + px * head_w, by + py * head_w);
    let p3 = IPoint::new(bx - px * head_w, by - py * head_w);
    let mut b = PathBuilder::new();
    b.move_to(p1);
    b.line_to(p2);
    b.line_to(p3);
    b.close();
    let head = b.build();
    frame.fill(&head, iced_color(color));
}

fn ellipse_path(b: &mut PathBuilder, cx: f32, cy: f32, rx: f32, ry: f32) {
    // Approximate an ellipse with a 36-segment polyline — good enough for
    // overlay use, the saved bitmap is composited from the real annotation
    // data via `image` crate paths if needed.
    let n = 36;
    for i in 0..=n {
        let t = (i as f32) / (n as f32) * std::f32::consts::TAU;
        let x = cx + rx * t.cos();
        let y = cy + ry * t.sin();
        if i == 0 { b.move_to(IPoint::new(x, y)); }
        else      { b.line_to(IPoint::new(x, y)); }
    }
}

// ── resize handles ─────────────────────────────────────────────────────────

pub fn resize_handles(frame: &mut Frame, sel: Rect) {
    let s = 6.0_f32;
    let pts = [
        (sel.x,           sel.y),
        (sel.x + sel.w,   sel.y),
        (sel.x,           sel.y + sel.h),
        (sel.x + sel.w,   sel.y + sel.h),
        (sel.x + sel.w/2.0, sel.y),
        (sel.x + sel.w/2.0, sel.y + sel.h),
        (sel.x,           sel.y + sel.h/2.0),
        (sel.x + sel.w,   sel.y + sel.h/2.0),
    ];
    for (x, y) in pts {
        let r = Path::rectangle(IPoint::new(x - s/2.0, y - s/2.0), Size::new(s, s));
        frame.fill(&r, Color::WHITE);
        frame.stroke(&r, Stroke {
            width: 1.0,
            style: iced::widget::canvas::stroke::Style::Solid(Color::from_rgb8(100, 149, 237)),
            ..Default::default()
        });
    }
}

// ── dimension label ────────────────────────────────────────────────────────

pub fn dimension_label(frame: &mut Frame, sel: Rect, bounds: Rectangle) {
    let txt = format!("{} × {}", sel.w as i32, sel.h as i32);
    let mut y = sel.y + sel.h + 6.0;
    if y + 18.0 > bounds.height { y = sel.y - 22.0; }
    let bg = Path::rectangle(IPoint::new(sel.x, y), Size::new(80.0, 18.0));
    frame.fill(&bg, Color::from_rgba(0.118, 0.118, 0.118, 0.78));
    frame.fill_text(Text {
        content: txt,
        position: IPoint::new(sel.x + 6.0, y + 2.0),
        color:    Color::WHITE,
        size:     Pixels(11.0),
        font:     iced::Font::DEFAULT,
        ..Default::default()
    });
}

// ── crosshair ──────────────────────────────────────────────────────────────

pub fn crosshair(frame: &mut Frame, p: Point2, bounds: Rectangle) {
    let pen = Stroke {
        style: iced::widget::canvas::stroke::Style::Solid(Color::from_rgba(0.39, 0.58, 0.93, 0.6)),
        width: 1.0,
        line_dash: iced::widget::canvas::stroke::LineDash { segments: &[2.0, 2.0], offset: 0 },
        ..Default::default()
    };
    let h = Path::line(IPoint::new(0.0, p.y), IPoint::new(bounds.width, p.y));
    let v = Path::line(IPoint::new(p.x, 0.0), IPoint::new(p.x, bounds.height));
    frame.stroke(&h, pen.clone());
    frame.stroke(&v, pen);
}

// ── magnifier ──────────────────────────────────────────────────────────────

pub fn magnifier(frame: &mut Frame, state: &OverlayState, bounds: Rectangle) {
    // 30×30 source area scaled 4× → 120×120 magnified view, plus a label band.
    const SRC: f32 = 30.0;
    const ZOOM: f32 = 4.0;
    let mag = SRC * ZOOM;

    let p = state.cursor;
    let mut mx = p.x + 25.0;
    let mut my = p.y + 25.0;
    if mx + mag + 2.0 > bounds.width  { mx = p.x - mag - 25.0; }
    if my + mag + 30.0 > bounds.height { my = p.y - mag - 45.0; }

    let bg = Path::rectangle(IPoint::new(mx - 1.0, my - 1.0), Size::new(mag + 2.0, mag + 22.0));
    frame.fill(&bg, Color::BLACK);

    // The actual zoomed image content is rendered by an inset iced::widget::image
    // when the user is hovering — this canvas pass draws the chrome.
    let border = Path::rectangle(IPoint::new(mx, my), Size::new(mag, mag));
    frame.stroke(&border, Stroke { width: 2.0, ..stroke_default(Rgba::new_opaque(100, 149, 237)) });

    // Crosshair inside the magnifier
    let cx = Path::line(IPoint::new(mx, my + mag / 2.0), IPoint::new(mx + mag, my + mag / 2.0));
    let cy = Path::line(IPoint::new(mx + mag / 2.0, my), IPoint::new(mx + mag / 2.0, my + mag));
    let cp = Stroke { width: 1.0, ..stroke_default(Rgba::new(255, 255, 255, 180)) };
    frame.stroke(&cx, cp.clone());
    frame.stroke(&cy, cp);

    // Coordinate readout
    let virt_x = p.x as i32 + state.virtual_origin.0;
    let virt_y = p.y as i32 + state.virtual_origin.1;
    frame.fill_text(Text {
        content: format!("X:{virt_x} Y:{virt_y}"),
        position: IPoint::new(mx + 4.0, my + mag + 4.0),
        color:    Color::WHITE,
        size:     Pixels(11.0),
        font:     iced::Font::MONOSPACE,
        ..Default::default()
    });

    let _ = Vector::new(0.0, 0.0); // suppress unused-import lint when this builds standalone
}
