//! Annotation editing geometry: hit-testing, bounding boxes, and the
//! translate transform used by both the Select tool and the commit-time
//! crop.
//!
//! Everything here is pure math so it can be unit-tested without a window.
//! The constants mirror what `kashot-app`'s painter actually rasterizes —
//! if a glyph metric or the step-marker radius ever changes there, change
//! it here too or the click target drifts away from the visible ink.

use crate::annotation::{Annotation, AnnotationKind, Point2, Rect};

/// Extra slack, in pixels, around an annotation's painted body that still
/// counts as a hit. Thin 2-px ink is otherwise nearly impossible to click.
pub const HIT_SLOP: f32 = 5.0;

/// Disc radius of a numbered-step marker. Mirrors `painter::step_marker`.
pub const STEP_RADIUS: f32 = 14.0;

/// 5x7 bitmap-font cell, before the integer scale factor. Mirrors
/// `kashot-app::bitmap_font::{GLYPH_W, GLYPH_H}`.
pub const GLYPH_W: f32 = 5.0;
pub const GLYPH_H: f32 = 7.0;

/// Integer scale the painter picks for a given `font_size`. Mirrors the
/// mapping in `painter::render_annotation` (14.0 -> 2x).
pub fn text_scale(font_size: f32) -> f32 {
    (font_size / 7.0).round().max(1.0)
}

/// Painted width x height of a text annotation, matching
/// `bitmap_font::measure` (one scale-wide gap between glyph cells).
pub fn text_extent(text: &str, font_size: f32) -> (f32, f32) {
    let scale = text_scale(font_size);
    let n = text.chars().count() as f32;
    let w = if n == 0.0 { 0.0 } else { n * GLYPH_W * scale + (n - 1.0) * scale };
    (w, GLYPH_H * scale)
}

/// How far from the ink's centerline a click still lands on it.
fn stroke_tolerance(thickness: f32) -> f32 {
    thickness / 2.0 + HIT_SLOP
}

/// Distance from `p` to the segment `a`-`b`. Degenerate segments collapse
/// to the point distance.
fn segment_distance(p: Point2, a: Point2, b: Point2) -> f32 {
    let vx = b.x - a.x;
    let vy = b.y - a.y;
    let len2 = vx * vx + vy * vy;
    if len2 <= f32::EPSILON {
        return ((p.x - a.x).powi(2) + (p.y - a.y).powi(2)).sqrt();
    }
    let t = (((p.x - a.x) * vx + (p.y - a.y) * vy) / len2).clamp(0.0, 1.0);
    let px = a.x + vx * t;
    let py = a.y + vy * t;
    ((p.x - px).powi(2) + (p.y - py).powi(2)).sqrt()
}

fn polyline_hit(points: &[Point2], p: Point2, tol: f32) -> bool {
    match points {
        []     => false,
        [only] => segment_distance(p, *only, *only) <= tol,
        _      => points.windows(2).any(|w| segment_distance(p, w[0], w[1]) <= tol),
    }
}

/// Hit-test the four edges of the rect spanned by `a`-`b`. The interior is
/// deliberately *not* a hit: a Rectangle annotation is a hollow outline, so
/// clicking the middle of a big frame must reach whatever is underneath it.
fn rect_outline_hit(a: Point2, b: Point2, p: Point2, tol: f32) -> bool {
    let r = Rect::from_corners(a, b);
    let tl = Point2::new(r.x, r.y);
    let tr = Point2::new(r.x + r.w, r.y);
    let br = Point2::new(r.x + r.w, r.y + r.h);
    let bl = Point2::new(r.x, r.y + r.h);
    [(tl, tr), (tr, br), (br, bl), (bl, tl)]
        .iter()
        .any(|(s, e)| segment_distance(p, *s, *e) <= tol)
}

/// Hit-test the ellipse outline inscribed in the rect spanned by `a`-`b`.
/// Compares the click's distance from the center against the ellipse radius
/// along the same ray, which is exact on that ray and cheap.
fn ellipse_outline_hit(a: Point2, b: Point2, p: Point2, tol: f32) -> bool {
    let r = Rect::from_corners(a, b);
    let (rx, ry) = (r.w / 2.0, r.h / 2.0);
    // A collapsed axis draws as a line, not an ellipse — fall back to the
    // bounding outline so a flat "ellipse" is still selectable.
    if rx <= tol || ry <= tol {
        return rect_outline_hit(a, b, p, tol);
    }
    let cx = r.x + rx;
    let cy = r.y + ry;
    let dx = p.x - cx;
    let dy = p.y - cy;
    let len = (dx * dx + dy * dy).sqrt();
    if len <= f32::EPSILON {
        return rx.min(ry) <= tol;
    }
    let ux = dx / len;
    let uy = dy / len;
    let denom = ((ux / rx).powi(2) + (uy / ry).powi(2)).sqrt();
    if denom <= f32::EPSILON {
        return false;
    }
    (len - 1.0 / denom).abs() <= tol
}

/// Does `p` land on the painted body of `a`?
pub fn hit_test(a: &Annotation, p: Point2) -> bool {
    match &a.kind {
        AnnotationKind::Pen { stroke, points } | AnnotationKind::Marker { stroke, points } => {
            polyline_hit(points, p, stroke_tolerance(stroke.thickness))
        }
        AnnotationKind::Line { stroke, start, end } => {
            segment_distance(p, *start, *end) <= stroke_tolerance(stroke.thickness)
        }
        AnnotationKind::Arrow { stroke, start, end } => {
            // The head is wider than the shaft, so the tip gets the head's
            // half-width as its tolerance instead of the shaft's.
            let shaft = stroke_tolerance(stroke.thickness);
            let head  = (stroke.thickness * 2.0).max(5.0) + HIT_SLOP;
            segment_distance(p, *start, *end) <= shaft
                || segment_distance(p, *end, *end) <= head
        }
        AnnotationKind::Rectangle { stroke, start, end } => {
            rect_outline_hit(*start, *end, p, stroke_tolerance(stroke.thickness))
        }
        AnnotationKind::Ellipse { stroke, start, end } => {
            ellipse_outline_hit(*start, *end, p, stroke_tolerance(stroke.thickness))
        }
        AnnotationKind::Text { position, text, font_size, .. } => {
            let (w, h) = text_extent(text, *font_size);
            Rect {
                x: position.x - HIT_SLOP,
                y: position.y - HIT_SLOP,
                w: w + HIT_SLOP * 2.0,
                h: h + HIT_SLOP * 2.0,
            }
            .contains(p)
        }
        AnnotationKind::Step { center, .. } => {
            let dx = p.x - center.x;
            let dy = p.y - center.y;
            (dx * dx + dy * dy).sqrt() <= STEP_RADIUS + HIT_SLOP
        }
        AnnotationKind::Pixelate { start, end, .. } => {
            // Pixelate is a filled effect — its whole rect is the body.
            let r = Rect::from_corners(*start, *end);
            Rect { x: r.x - HIT_SLOP, y: r.y - HIT_SLOP, w: r.w + HIT_SLOP * 2.0, h: r.h + HIT_SLOP * 2.0 }
                .contains(p)
        }
    }
}

/// Index of the topmost (last-drawn) annotation under `p`.
pub fn hit_test_topmost(annotations: &[Annotation], p: Point2) -> Option<usize> {
    annotations.iter().rposition(|a| hit_test(a, p))
}

/// Axis-aligned box covering everything the annotation paints, including
/// stroke width and arrow heads. Used to draw the selection highlight.
pub fn bounds(a: &Annotation) -> Rect {
    fn span(points: &[Point2], pad: f32) -> Rect {
        let (mut x0, mut y0) = (f32::MAX, f32::MAX);
        let (mut x1, mut y1) = (f32::MIN, f32::MIN);
        for p in points {
            x0 = x0.min(p.x); y0 = y0.min(p.y);
            x1 = x1.max(p.x); y1 = y1.max(p.y);
        }
        if points.is_empty() {
            return Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
        }
        Rect { x: x0 - pad, y: y0 - pad, w: (x1 - x0) + pad * 2.0, h: (y1 - y0) + pad * 2.0 }
    }
    match &a.kind {
        AnnotationKind::Pen { stroke, points } | AnnotationKind::Marker { stroke, points } => {
            span(points, stroke.thickness / 2.0)
        }
        AnnotationKind::Line { stroke, start, end }
        | AnnotationKind::Rectangle { stroke, start, end }
        | AnnotationKind::Ellipse { stroke, start, end } => {
            span(&[*start, *end], stroke.thickness / 2.0)
        }
        AnnotationKind::Arrow { stroke, start, end } => {
            span(&[*start, *end], (stroke.thickness * 2.0).max(5.0))
        }
        AnnotationKind::Pixelate { start, end, .. } => span(&[*start, *end], 0.0),
        AnnotationKind::Text { position, text, font_size, .. } => {
            let (w, h) = text_extent(text, *font_size);
            Rect { x: position.x, y: position.y, w, h }
        }
        AnnotationKind::Step { center, .. } => Rect {
            x: center.x - STEP_RADIUS,
            y: center.y - STEP_RADIUS,
            w: STEP_RADIUS * 2.0,
            h: STEP_RADIUS * 2.0,
        },
    }
}

/// Shift every coordinate of `a` by (dx, dy), in place.
pub fn translate(a: &mut Annotation, dx: f32, dy: f32) {
    let shift = |p: &mut Point2| { p.x += dx; p.y += dy; };
    match &mut a.kind {
        AnnotationKind::Pen { points, .. } | AnnotationKind::Marker { points, .. } => {
            for p in points.iter_mut() { shift(p); }
        }
        AnnotationKind::Line { start, end, .. }
        | AnnotationKind::Arrow { start, end, .. }
        | AnnotationKind::Rectangle { start, end, .. }
        | AnnotationKind::Ellipse { start, end, .. }
        | AnnotationKind::Pixelate { start, end, .. } => { shift(start); shift(end); }
        AnnotationKind::Text { position, .. } => shift(position),
        AnnotationKind::Step { center, .. }   => shift(center),
    }
}

/// Non-mutating `translate`. The commit path uses this to move window-space
/// coordinates into the cropped output's local space.
pub fn translated(a: &Annotation, dx: f32, dy: f32) -> Annotation {
    let mut out = a.clone();
    translate(&mut out, dx, dy);
    out
}

/// The number the next `Tool::Step` click should get: one past the highest
/// step already on the canvas. Keeps the counter honest after an undo, a
/// redo, or a deleted step marker.
pub fn next_step_number(annotations: &[Annotation]) -> u32 {
    annotations
        .iter()
        .filter_map(|a| match a.kind {
            AnnotationKind::Step { number, .. } => Some(number),
            _ => None,
        })
        .max()
        .map_or(1, |n| n.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::Stroke;
    use crate::color::Rgba;

    fn stroke() -> Stroke { Stroke { color: Rgba::RED, thickness: 4.0 } }
    fn p(x: f32, y: f32) -> Point2 { Point2::new(x, y) }

    // ── hit-testing, one test per annotation kind ───────────────────────

    #[test]
    fn hit_test_pen_path() {
        let mut a = Annotation::pen(stroke(), p(10.0, 10.0));
        a.extend(p(50.0, 10.0));
        a.extend(p(50.0, 40.0));
        assert!(hit_test(&a, p(30.0, 11.0)), "on the first segment");
        assert!(hit_test(&a, p(50.0, 30.0)), "on the second segment");
        assert!(!hit_test(&a, p(30.0, 35.0)), "inside the elbow but off the ink");
        assert!(!hit_test(&a, p(200.0, 200.0)), "far away");
    }

    #[test]
    fn hit_test_single_point_pen_path() {
        let a = Annotation::pen(stroke(), p(10.0, 10.0));
        assert!(hit_test(&a, p(11.0, 11.0)));
        assert!(!hit_test(&a, p(40.0, 40.0)));
    }

    #[test]
    fn hit_test_line() {
        let mut a = Annotation::line(stroke(), p(0.0, 0.0));
        a.extend(p(100.0, 100.0));
        assert!(hit_test(&a, p(50.0, 50.0)));
        assert!(!hit_test(&a, p(50.0, 70.0)));
        assert!(!hit_test(&a, p(-40.0, -40.0)), "past the endpoint, not on the segment");
    }

    #[test]
    fn hit_test_arrow_shaft_and_head() {
        let mut a = Annotation::arrow(stroke(), p(0.0, 0.0));
        a.extend(p(100.0, 0.0));
        assert!(hit_test(&a, p(50.0, 1.0)), "shaft");
        assert!(hit_test(&a, p(100.0, 10.0)), "head is wider than the shaft");
        assert!(!hit_test(&a, p(50.0, 30.0)));
    }

    #[test]
    fn hit_test_rectangle_is_outline_only() {
        let mut a = Annotation::rectangle(stroke(), p(10.0, 10.0));
        a.extend(p(110.0, 60.0));
        assert!(hit_test(&a, p(60.0, 10.0)), "top edge");
        assert!(hit_test(&a, p(110.0, 35.0)), "right edge");
        assert!(!hit_test(&a, p(60.0, 35.0)), "hollow interior must not swallow clicks");
    }

    #[test]
    fn hit_test_ellipse_is_outline_only() {
        let mut a = Annotation::ellipse(stroke(), p(0.0, 0.0));
        a.extend(p(200.0, 100.0));
        assert!(hit_test(&a, p(100.0, 0.0)),   "top of the curve");
        assert!(hit_test(&a, p(200.0, 50.0)),  "right of the curve");
        assert!(!hit_test(&a, p(100.0, 50.0)), "center is hollow");
        assert!(!hit_test(&a, p(0.0, 0.0)),    "bbox corner is outside the curve");
    }

    #[test]
    fn hit_test_flat_ellipse_falls_back_to_its_box() {
        let mut a = Annotation::ellipse(stroke(), p(0.0, 50.0));
        a.extend(p(100.0, 50.0));
        assert!(hit_test(&a, p(50.0, 50.0)), "a collapsed ellipse draws as a line");
    }

    #[test]
    fn hit_test_marker_uses_its_fat_stroke() {
        // Marker thickness is 6x, so its click target is much wider.
        let mut a = Annotation::marker(stroke(), p(0.0, 0.0), 0xC8);
        a.extend(p(100.0, 0.0));
        assert!(hit_test(&a, p(50.0, 11.0)), "inside the 24-px band");
        assert!(!hit_test(&a, p(50.0, 40.0)));
    }

    #[test]
    fn hit_test_text_box() {
        let a = Annotation::text(Rgba::RED, p(20.0, 30.0), "hi");
        // font_size 14 -> scale 2 -> 2 glyphs = 2*10 + 2 = 22 wide, 14 tall.
        assert!(hit_test(&a, p(25.0, 35.0)));
        assert!(hit_test(&a, p(41.0, 43.0)), "trailing edge, within slop");
        assert!(!hit_test(&a, p(80.0, 35.0)));
        assert!(!hit_test(&a, p(25.0, 80.0)));
    }

    #[test]
    fn hit_test_step_disc() {
        let a = Annotation::step(Rgba::RED, p(100.0, 100.0), 3);
        assert!(hit_test(&a, p(100.0, 100.0)), "center");
        assert!(hit_test(&a, p(112.0, 100.0)), "inside the disc");
        assert!(!hit_test(&a, p(130.0, 100.0)), "outside disc + slop");
    }

    #[test]
    fn hit_test_pixelate_is_filled() {
        let mut a = Annotation::pixelate(p(10.0, 10.0));
        a.extend(p(60.0, 40.0));
        assert!(hit_test(&a, p(35.0, 25.0)), "blur is a filled effect");
        assert!(!hit_test(&a, p(90.0, 25.0)));
    }

    #[test]
    fn hit_test_topmost_prefers_the_last_drawn() {
        let mut under = Annotation::rectangle(stroke(), p(0.0, 0.0));
        under.extend(p(100.0, 100.0));
        let over = Annotation::step(Rgba::RED, p(0.0, 0.0), 1);
        let list = vec![under, over];
        assert_eq!(hit_test_topmost(&list, p(0.0, 0.0)), Some(1));
        assert_eq!(hit_test_topmost(&list, p(100.0, 50.0)), Some(0));
        assert_eq!(hit_test_topmost(&list, p(400.0, 400.0)), None);
    }

    #[test]
    fn hit_test_topmost_on_empty_canvas() {
        assert_eq!(hit_test_topmost(&[], p(1.0, 1.0)), None);
    }

    // ── translate, one assertion per annotation kind ────────────────────

    fn all_kinds() -> Vec<Annotation> {
        let mut pen = Annotation::pen(stroke(), p(1.0, 2.0));
        pen.extend(p(3.0, 4.0));
        let mut marker = Annotation::marker(stroke(), p(1.0, 2.0), 0xC8);
        marker.extend(p(3.0, 4.0));
        let mut line = Annotation::line(stroke(), p(1.0, 2.0));       line.extend(p(3.0, 4.0));
        let mut arrow = Annotation::arrow(stroke(), p(1.0, 2.0));     arrow.extend(p(3.0, 4.0));
        let mut rect = Annotation::rectangle(stroke(), p(1.0, 2.0));  rect.extend(p(3.0, 4.0));
        let mut ell = Annotation::ellipse(stroke(), p(1.0, 2.0));     ell.extend(p(3.0, 4.0));
        let mut blur = Annotation::pixelate(p(1.0, 2.0));             blur.extend(p(3.0, 4.0));
        vec![
            pen, marker, line, arrow, rect, ell, blur,
            Annotation::text(Rgba::RED, p(1.0, 2.0), "abc"),
            Annotation::step(Rgba::RED, p(1.0, 2.0), 7),
        ]
    }

    #[test]
    fn translate_moves_every_annotation_kind() {
        for a in all_kinds() {
            let before = bounds(&a);
            let moved  = translated(&a, 25.0, -13.0);
            let after  = bounds(&moved);
            assert!((after.x - (before.x + 25.0)).abs() < 0.001, "x for {:?}", a.kind);
            assert!((after.y - (before.y - 13.0)).abs() < 0.001, "y for {:?}", a.kind);
            assert!((after.w - before.w).abs() < 0.001, "width must not change for {:?}", a.kind);
            assert!((after.h - before.h).abs() < 0.001, "height must not change for {:?}", a.kind);
        }
    }

    #[test]
    fn translate_round_trips_to_the_original() {
        for a in all_kinds() {
            let mut moved = a.clone();
            translate(&mut moved, 40.0, 90.0);
            translate(&mut moved, -40.0, -90.0);
            assert_eq!(moved, a, "move then unmove must restore {:?}", a.kind);
        }
    }

    #[test]
    fn translate_keeps_hit_test_and_style_in_sync() {
        for a in all_kinds() {
            let b = bounds(&a);
            let probe = p(b.x + b.w / 2.0, b.y + b.h / 2.0);
            let moved = translated(&a, 500.0, 500.0);
            assert!(!hit_test(&moved, probe),
                    "moved ink must not answer to the old location: {:?}", a.kind);
            assert_eq!(std::mem::discriminant(&a.kind), std::mem::discriminant(&moved.kind));
            assert_eq!(a.time, moved.time, "time window survives a move");
        }
    }

    #[test]
    fn translate_preserves_the_time_window() {
        let mut a = Annotation::pen(stroke(), p(0.0, 0.0));
        a.time = Some((1.0, 4.0));
        assert_eq!(translated(&a, 5.0, 5.0).time, Some((1.0, 4.0)));
    }

    // ── bounds + step numbering ────────────────────────────────────────

    #[test]
    fn bounds_cover_the_painted_body() {
        let a = Annotation::step(Rgba::RED, p(100.0, 100.0), 1);
        let b = bounds(&a);
        assert_eq!((b.x, b.y, b.w, b.h), (86.0, 86.0, 28.0, 28.0));
    }

    #[test]
    fn bounds_of_an_empty_path_are_empty() {
        let a = Annotation { kind: AnnotationKind::Pen { stroke: stroke(), points: vec![] }, time: None };
        assert!(bounds(&a).is_empty());
    }

    #[test]
    fn next_step_number_follows_the_highest_marker() {
        assert_eq!(next_step_number(&[]), 1);
        let list = vec![
            Annotation::step(Rgba::RED, p(0.0, 0.0), 1),
            Annotation::pen(stroke(), p(0.0, 0.0)),
            Annotation::step(Rgba::RED, p(0.0, 0.0), 2),
        ];
        assert_eq!(next_step_number(&list), 3);
        assert_eq!(next_step_number(&list[..1]), 2);
    }
}
