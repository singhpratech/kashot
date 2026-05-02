//! Polymorphic annotation hierarchy. Each variant draws into a graphics target
//! supplied by the renderer — `kashot-app` provides the iced::Canvas backend.
//!
//! Mirrors the C# `Annotation` hierarchy in `Kashot/Annotations.cs`. When you
//! add a new annotation type, also update:
//!  * `tool::Tool` enum
//!  * `tool::Tool::ALL` array
//!  * the renderer (`kashot-app/src/overlay/render.rs`)

use serde::{Deserialize, Serialize};

use crate::color::Rgba;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point2 {
    pub x: f32,
    pub y: f32,
}

impl Point2 {
    pub const fn new(x: f32, y: f32) -> Self { Self { x, y } }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn from_corners(a: Point2, b: Point2) -> Self {
        let x = a.x.min(b.x);
        let y = a.y.min(b.y);
        Rect { x, y, w: (b.x - a.x).abs(), h: (b.y - a.y).abs() }
    }

    pub fn contains(self, p: Point2) -> bool {
        p.x >= self.x && p.x <= self.x + self.w && p.y >= self.y && p.y <= self.y + self.h
    }

    pub fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// Common style fields shared across most annotations.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub color:     Rgba,
    pub thickness: f32,
}

impl Default for Stroke {
    fn default() -> Self {
        Stroke { color: Rgba::RED, thickness: 3.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AnnotationKind {
    Pen        { stroke: Stroke, points: Vec<Point2> },
    Line       { stroke: Stroke, start: Point2, end: Point2 },
    Arrow      { stroke: Stroke, start: Point2, end: Point2 },
    Rectangle  { stroke: Stroke, start: Point2, end: Point2 },
    Ellipse    { stroke: Stroke, start: Point2, end: Point2 },
    Marker     { stroke: Stroke, points: Vec<Point2> },
    Text       { color: Rgba, position: Point2, text: String, font_size: f32 },
    Step       { color: Rgba, center: Point2, number: u32 },
    Pixelate   { start: Point2, end: Point2, block_size: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub kind: AnnotationKind,
}

impl Annotation {
    pub fn pen(stroke: Stroke, p: Point2) -> Annotation {
        Annotation { kind: AnnotationKind::Pen { stroke, points: vec![p] } }
    }
    pub fn line(stroke: Stroke, a: Point2) -> Annotation {
        Annotation { kind: AnnotationKind::Line { stroke, start: a, end: a } }
    }
    pub fn arrow(stroke: Stroke, a: Point2) -> Annotation {
        Annotation { kind: AnnotationKind::Arrow { stroke, start: a, end: a } }
    }
    pub fn rectangle(stroke: Stroke, a: Point2) -> Annotation {
        Annotation { kind: AnnotationKind::Rectangle { stroke, start: a, end: a } }
    }
    pub fn ellipse(stroke: Stroke, a: Point2) -> Annotation {
        Annotation { kind: AnnotationKind::Ellipse { stroke, start: a, end: a } }
    }
    pub fn marker(stroke: Stroke, p: Point2) -> Annotation {
        // Marker thickness is 6× the configured thickness, with alpha pinned
        // to 0xC8 so a vivid color reads as a highlighter regardless of which
        // palette the user has active. Mirrors `Kashot/Annotations.cs::MarkerAnnotation`.
        let stroke = Stroke {
            thickness: stroke.thickness * 6.0,
            color:     stroke.color.with_alpha(0xC8),
        };
        Annotation { kind: AnnotationKind::Marker { stroke, points: vec![p] } }
    }
    pub fn text(color: Rgba, p: Point2, text: impl Into<String>) -> Annotation {
        Annotation { kind: AnnotationKind::Text { color, position: p, text: text.into(), font_size: 14.0 } }
    }
    pub fn step(color: Rgba, center: Point2, number: u32) -> Annotation {
        Annotation { kind: AnnotationKind::Step { color, center, number } }
    }
    pub fn pixelate(a: Point2) -> Annotation {
        Annotation { kind: AnnotationKind::Pixelate { start: a, end: a, block_size: 10 } }
    }

    /// Update the in-progress annotation with the latest mouse position.
    pub fn extend(&mut self, p: Point2) {
        match &mut self.kind {
            AnnotationKind::Pen { points, .. }      => points.push(p),
            AnnotationKind::Marker { points, .. }   => points.push(p),
            AnnotationKind::Line { end, .. }        => *end = p,
            AnnotationKind::Arrow { end, .. }       => *end = p,
            AnnotationKind::Rectangle { end, .. }   => *end = p,
            AnnotationKind::Ellipse { end, .. }     => *end = p,
            AnnotationKind::Pixelate { end, .. }    => *end = p,
            AnnotationKind::Text { .. }             => {}
            AnnotationKind::Step { .. }             => {}
        }
    }
}

// ── Color palettes ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ColorPalette {
    pub name:   &'static str,
    pub colors: [Rgba; 16],
}

pub struct Palettes;

impl Palettes {
    pub const ALL: [ColorPalette; 4] = [
        ColorPalette { name: "Vivid", colors: [
            Rgba::new_opaque(220,  38,  38),
            Rgba::new_opaque(255, 100,   0),
            Rgba::new_opaque(255, 180,   0),
            Rgba::new_opaque(255, 230,   0),
            Rgba::new_opaque(130, 220,   0),
            Rgba::new_opaque(  0, 180,  80),
            Rgba::new_opaque(  0, 200, 200),
            Rgba::new_opaque(  0, 180, 240),
            Rgba::new_opaque(  0, 100, 255),
            Rgba::new_opaque( 80,  80, 220),
            Rgba::new_opaque(160,  60, 240),
            Rgba::new_opaque(240,  60, 220),
            Rgba::new_opaque(255, 100, 200),
            Rgba::new_opaque(255,  80,  80),
            Rgba::new_opaque(255, 255, 255),
            Rgba::new_opaque(  0,   0,   0),
        ] },
        ColorPalette { name: "Highlighter", colors: [
            Rgba::new(255, 235,   0, 170),
            Rgba::new(100, 255, 100, 170),
            Rgba::new(255, 100, 200, 170),
            Rgba::new(100, 230, 255, 170),
            Rgba::new(255, 150,   0, 170),
            Rgba::new(200, 100, 255, 170),
            Rgba::new( 50, 220,  50, 170),
            Rgba::new(100, 150, 255, 170),
            Rgba::new(255,  80,  80, 170),
            Rgba::new(240,  60, 220, 170),
            Rgba::new(  0, 200, 200, 170),
            Rgba::new(255, 180, 100, 170),
            Rgba::new(180, 255, 100, 170),
            Rgba::new(100, 100, 255, 170),
            Rgba::new(230, 230, 230, 170),
            Rgba::new( 80,  80,  80, 170),
        ] },
        ColorPalette { name: "Pastel", colors: [
            Rgba::new_opaque(255, 200, 200),
            Rgba::new_opaque(255, 220, 180),
            Rgba::new_opaque(255, 235, 180),
            Rgba::new_opaque(255, 245, 200),
            Rgba::new_opaque(230, 255, 200),
            Rgba::new_opaque(200, 255, 220),
            Rgba::new_opaque(200, 245, 245),
            Rgba::new_opaque(200, 230, 255),
            Rgba::new_opaque(200, 215, 255),
            Rgba::new_opaque(220, 200, 255),
            Rgba::new_opaque(240, 200, 255),
            Rgba::new_opaque(255, 200, 235),
            Rgba::new_opaque(255, 215, 215),
            Rgba::new_opaque(240, 240, 240),
            Rgba::new_opaque(200, 200, 210),
            Rgba::new_opaque(100, 100, 110),
        ] },
        ColorPalette { name: "Pro", colors: [
            Rgba::new_opaque(220,  38,  38),
            Rgba::new_opaque( 30, 100, 220),
            Rgba::new_opaque( 30, 160,  60),
            Rgba::new_opaque(240, 120,   0),
            Rgba::new_opaque(138,  43, 226),
            Rgba::new_opaque(220, 180,   0),
            Rgba::new_opaque(  0, 160, 200),
            Rgba::new_opaque(200,  60, 130),
            Rgba::new_opaque(100,  30,  30),
            Rgba::new_opaque( 30,  30, 100),
            Rgba::new_opaque( 30,  80,  30),
            Rgba::new_opaque(100,  60,   0),
            Rgba::new_opaque(  0,   0,   0),
            Rgba::new_opaque( 80,  80,  80),
            Rgba::new_opaque(200, 200, 200),
            Rgba::new_opaque(255, 255, 255),
        ] },
    ];

    pub fn get(index: usize) -> &'static ColorPalette {
        let i = index.min(Self::ALL.len() - 1);
        &Self::ALL[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_from_corners_normalizes() {
        let r = Rect::from_corners(Point2::new(50.0, 80.0), Point2::new(10.0, 20.0));
        assert_eq!(r, Rect { x: 10.0, y: 20.0, w: 40.0, h: 60.0 });
    }

    #[test]
    fn pen_extension_appends_points() {
        let mut a = Annotation::pen(Stroke::default(), Point2::new(1.0, 2.0));
        a.extend(Point2::new(3.0, 4.0));
        a.extend(Point2::new(5.0, 6.0));
        match a.kind {
            AnnotationKind::Pen { points, .. } => assert_eq!(points.len(), 3),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn palettes_have_16_colors_each() {
        for p in Palettes::ALL.iter() { assert_eq!(p.colors.len(), 16); }
    }
}
