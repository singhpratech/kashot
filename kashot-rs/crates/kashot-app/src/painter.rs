//! CPU rasterizer primitives for the overlay editor.
//!
//! Two surfaces, one API: every routine works against either
//!   - a softbuffer-style `&mut [u32]` (XRGB, used for live preview), or
//!   - an `&mut ImageBuffer<Rgba<u8>, Vec<u8>>` (used at commit time when we
//!     burn annotations into the cropped output PNG).
//!
//! We pick correctness + readability over speed — selections are typically
//! a few hundred pixels per side and we redraw per mouse-move, but a naive
//! Bresenham + circular brush easily holds 60 fps in that regime.

use image::{ImageBuffer, Rgba};
use kashot_core::annotation::{Annotation, AnnotationKind, Point2, Stroke};
use kashot_core::color::Rgba as KashotRgba;

/// Anything we can stamp pixels into. Implemented for the live softbuffer
/// (`&mut [u32]`) and for the output bitmap (`&mut ImageBuffer<Rgba<u8>>`).
pub trait Surface {
    fn width(&self)  -> i32;
    fn height(&self) -> i32;
    /// Read the destination pixel as RGBA (alpha is opaque for u32 buffers).
    fn read(&self, x: i32, y: i32) -> [u8; 4];
    /// Write a fully-opaque RGB pixel. Alpha is dropped on u32 buffers.
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]);
}

pub struct U32Surface<'a> {
    pub buf:    &'a mut [u32],
    pub stride: i32,
    pub height: i32,
}

impl<'a> Surface for U32Surface<'a> {
    fn width(&self)  -> i32 { self.stride }
    fn height(&self) -> i32 { self.height }
    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        let p = self.buf[(y * self.stride + x) as usize];
        [((p >> 16) & 0xFF) as u8, ((p >> 8) & 0xFF) as u8, (p & 0xFF) as u8, 0xFF]
    }
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        self.buf[(y * self.stride + x) as usize] =
            ((rgba[0] as u32) << 16) | ((rgba[1] as u32) << 8) | rgba[2] as u32;
    }
}

pub struct ImageSurface<'a>(pub &'a mut ImageBuffer<Rgba<u8>, Vec<u8>>);

impl<'a> Surface for ImageSurface<'a> {
    fn width(&self)  -> i32 { self.0.width()  as i32 }
    fn height(&self) -> i32 { self.0.height() as i32 }
    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        self.0.get_pixel(x as u32, y as u32).0
    }
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        self.0.put_pixel(x as u32, y as u32, Rgba(rgba));
    }
}

// ── pixel-level ops ─────────────────────────────────────────────────────────

fn in_bounds<S: Surface>(s: &S, x: i32, y: i32) -> bool {
    x >= 0 && x < s.width() && y >= 0 && y < s.height()
}

/// Source-over blend `src` (premultiplied later — we keep it straight here)
/// onto whatever's at (x, y).
fn blend<S: Surface>(s: &mut S, x: i32, y: i32, src: [u8; 4]) {
    if !in_bounds(s, x, y) { return; }
    if src[3] == 0 { return; }
    if src[3] == 255 {
        s.write(x, y, [src[0], src[1], src[2], 255]);
        return;
    }
    let dst = s.read(x, y);
    let a   = src[3] as u32;
    let inv = 255 - a;
    let mix = |sc: u8, dc: u8| (((sc as u32) * a + (dc as u32) * inv + 127) / 255) as u8;
    s.write(x, y, [mix(src[0], dst[0]), mix(src[1], dst[1]), mix(src[2], dst[2]), 255]);
}

fn rgba_arr(c: KashotRgba) -> [u8; 4] { [c.r, c.g, c.b, c.a] }

// ── primitives ──────────────────────────────────────────────────────────────

/// Filled disc of radius `r` centered at (cx, cy).
fn stamp_disc<S: Surface>(s: &mut S, cx: i32, cy: i32, r: i32, color: [u8; 4]) {
    if r <= 0 { blend(s, cx, cy, color); return; }
    let r2 = r * r;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r2 {
                blend(s, cx + dx, cy + dy, color);
            }
        }
    }
}

/// Bresenham line, stamping a disc of radius `(thickness/2).max(1)` at every
/// step so the line has body. Endpoints are inclusive on both sides.
pub fn line<S: Surface>(s: &mut S, x0: i32, y0: i32, x1: i32, y1: i32, thickness: f32, color: KashotRgba) {
    let r = ((thickness / 2.0).round() as i32).max(0);
    let c = rgba_arr(color);
    let dx =  (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut x = x0;
    let mut y = y0;
    loop {
        stamp_disc(s, x, y, r, c);
        if x == x1 && y == y1 { break; }
        let e2 = 2 * err;
        if e2 >= dy { err += dy; x += sx; }
        if e2 <= dx { err += dx; y += sy; }
    }
}

/// Polyline through `points`. Used by Pen and Marker.
pub fn polyline<S: Surface>(s: &mut S, points: &[Point2], thickness: f32, color: KashotRgba) {
    if points.is_empty() { return; }
    let mut prev = points[0];
    let r = ((thickness / 2.0).round() as i32).max(0);
    stamp_disc(s, prev.x as i32, prev.y as i32, r, rgba_arr(color));
    for p in &points[1..] {
        line(s, prev.x as i32, prev.y as i32, p.x as i32, p.y as i32, thickness, color);
        prev = *p;
    }
}

/// 4-edge rectangle stroke between the two corners (any orientation).
pub fn stroke_rect<S: Surface>(s: &mut S, a: Point2, b: Point2, thickness: f32, color: KashotRgba) {
    let x0 = a.x.min(b.x) as i32;
    let y0 = a.y.min(b.y) as i32;
    let x1 = a.x.max(b.x) as i32;
    let y1 = a.y.max(b.y) as i32;
    line(s, x0, y0, x1, y0, thickness, color);
    line(s, x1, y0, x1, y1, thickness, color);
    line(s, x1, y1, x0, y1, thickness, color);
    line(s, x0, y1, x0, y0, thickness, color);
}

/// Parametric ellipse outline inside the bounding box (a, b). 360 samples is
/// more than enough for any typical capture-region size.
pub fn stroke_ellipse<S: Surface>(s: &mut S, a: Point2, b: Point2, thickness: f32, color: KashotRgba) {
    let cx = (a.x + b.x) * 0.5;
    let cy = (a.y + b.y) * 0.5;
    let rx = (a.x - b.x).abs() * 0.5;
    let ry = (a.y - b.y).abs() * 0.5;
    if rx < 0.5 || ry < 0.5 { return; }
    // Step density scales with the perimeter so big ellipses don't miss pixels.
    let perim = std::f32::consts::PI * (rx + ry);
    let steps = (perim.ceil() as i32).clamp(64, 4096);
    let mut prev = (cx + rx, cy);
    for i in 1..=steps {
        let t   = (i as f32) / (steps as f32) * std::f32::consts::TAU;
        let p   = (cx + rx * t.cos(), cy + ry * t.sin());
        line(s, prev.0 as i32, prev.1 as i32, p.0 as i32, p.1 as i32, thickness, color);
        prev = p;
    }
}

/// Filled triangle — used for the arrow head. Standard scanline fill.
fn fill_triangle<S: Surface>(s: &mut S, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), color: KashotRgba) {
    let mut v = [p0, p1, p2];
    v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    let (a, b, c) = (v[0], v[1], v[2]);
    let c_color = rgba_arr(color);
    let edge = |y, p: (f32, f32), q: (f32, f32)| {
        if (q.1 - p.1).abs() < 0.5 { p.0 } else { p.0 + (q.0 - p.0) * (y - p.1) / (q.1 - p.1) }
    };
    let y0 = a.1.floor() as i32;
    let y1 = c.1.ceil()  as i32;
    for y in y0..=y1 {
        let yf = y as f32;
        let (xa, xb) = if yf < b.1 {
            (edge(yf, a, b), edge(yf, a, c))
        } else {
            (edge(yf, b, c), edge(yf, a, c))
        };
        let (lx, rx) = if xa < xb { (xa, xb) } else { (xb, xa) };
        for x in (lx.floor() as i32)..=(rx.ceil() as i32) {
            blend(s, x, y, c_color);
        }
    }
}

/// Arrow: a line from start to end with a filled triangular head at the end.
/// Head size scales with thickness so thin arrows stay readable and fat ones
/// look proportionate. Mirrors `Kashot/Annotations.cs::ArrowAnnotation`.
pub fn arrow<S: Surface>(s: &mut S, a: Point2, b: Point2, thickness: f32, color: KashotRgba) {
    line(s, a.x as i32, a.y as i32, b.x as i32, b.y as i32, thickness, color);
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let head_len  = (thickness * 4.0).max(10.0);
    let head_half = (thickness * 2.0).max(5.0);
    let ux = dx / len;
    let uy = dy / len;
    let base = (b.x - ux * head_len, b.y - uy * head_len);
    let perp = (-uy, ux);
    let p0 = (b.x, b.y);
    let p1 = (base.0 + perp.0 * head_half, base.1 + perp.1 * head_half);
    let p2 = (base.0 - perp.0 * head_half, base.1 - perp.1 * head_half);
    fill_triangle(s, p0, p1, p2, color);
}

// ── step number rendering (5×7 bitmap font, 0–9 only) ──────────────────────

/// Each row is 5 bits, bit 4 = leftmost. 7 rows.
const DIGITS: [[u8; 7]; 10] = [
    // 0
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    // 1
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    // 2
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    // 3
    [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
    // 4
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    // 5
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    // 6
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    // 7
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    // 8
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    // 9
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

/// Render `n` (0–999) at the given top-left position, scaled by `scale`.
pub fn draw_number<S: Surface>(s: &mut S, x: i32, y: i32, scale: i32, n: u32, color: KashotRgba) {
    let mut digits = Vec::new();
    let mut v = n.min(999);
    if v == 0 { digits.push(0); } else {
        while v > 0 { digits.push((v % 10) as usize); v /= 10; }
    }
    digits.reverse();
    let glyph_w = 5 * scale;
    let gap     = scale;
    let mut cx  = x;
    for d in digits {
        let glyph = &DIGITS[d];
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                let on = (bits >> (4 - col)) & 1 == 1;
                if on {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            blend(s, cx + col * scale + sx, y + row as i32 * scale + sy, rgba_arr(color));
                        }
                    }
                }
            }
        }
        cx += glyph_w + gap;
    }
}

/// Filled disc in alpha-blended source-over.
pub fn fill_disc<S: Surface>(s: &mut S, cx: i32, cy: i32, r: i32, color: KashotRgba) {
    if r <= 0 { return; }
    let r2 = r * r;
    let c = rgba_arr(color);
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r2 {
                blend(s, cx + dx, cy + dy, c);
            }
        }
    }
}

/// Render a string starting at (x, y) using the in-tree 5×7 bitmap font.
/// Same scale convention as `draw_number`: `scale=2` → 10×14 per glyph
/// with a 2px column gap.
pub fn draw_text<S: Surface>(s: &mut S, x: i32, y: i32, scale: i32, text: &str, color: KashotRgba) {
    let mut cx = x;
    let glyph_w = crate::bitmap_font::GLYPH_W * scale;
    let gap     = scale;
    for ch in text.chars() {
        let g = crate::bitmap_font::glyph(ch);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..crate::bitmap_font::GLYPH_W {
                let on = (bits >> (4 - col)) & 1 == 1;
                if on {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            blend(s, cx + col * scale + sx, y + row as i32 * scale + sy, rgba_arr(color));
                        }
                    }
                }
            }
        }
        cx += glyph_w + gap;
    }
}

/// Numbered step circle: filled disc of `color`, white digits centered inside.
pub fn step_marker<S: Surface>(s: &mut S, center: Point2, number: u32, color: KashotRgba) {
    const RADIUS: i32 = 14;
    fill_disc(s, center.x as i32, center.y as i32, RADIUS, color);
    // 5×7 font scaled 2× → 10×14 per digit, with 2px gap.
    let scale = 2;
    let n = number.min(999);
    let digits = if n == 0 { 1 } else { (n.ilog10() + 1) as i32 };
    let total_w = digits * 5 * scale + (digits - 1) * scale;
    let total_h = 7 * scale;
    let x = center.x as i32 - total_w / 2;
    let y = center.y as i32 - total_h / 2;
    draw_number(s, x, y, scale, number, KashotRgba::WHITE);
}

// ── pixelate ────────────────────────────────────────────────────────────────

/// Average-then-fill: replace every `block_size×block_size` block in the dest
/// rect with the average color of the corresponding block in `src`. The source
/// is expected to be the *original* screenshot (in the same coordinate space
/// as the destination surface) — never a downstream surface that may already
/// have annotations baked in. That keeps pixelate idempotent under draw-order
/// changes, matching `Kashot/Annotations.cs::PixelateAnnotation`.
pub fn pixelate_rect<S: Surface>(
    s:          &mut S,
    src:        &ImageBuffer<Rgba<u8>, Vec<u8>>,
    a:          Point2,
    b:          Point2,
    block_size: u32,
) {
    let block = block_size.max(2) as i32;
    let x0 = a.x.min(b.x) as i32;
    let y0 = a.y.min(b.y) as i32;
    let x1 = a.x.max(b.x) as i32;
    let y1 = a.y.max(b.y) as i32;
    let src_w = src.width()  as i32;
    let src_h = src.height() as i32;

    let mut by = y0;
    while by < y1 {
        let mut bx = x0;
        while bx < x1 {
            // Average source pixels inside this block.
            let bx2 = (bx + block).min(x1);
            let by2 = (by + block).min(y1);
            let (mut rs, mut gs, mut bs, mut count) = (0u32, 0u32, 0u32, 0u32);
            for sy in by..by2 {
                for sx in bx..bx2 {
                    if sx >= 0 && sx < src_w && sy >= 0 && sy < src_h {
                        let p = src.get_pixel(sx as u32, sy as u32).0;
                        rs += p[0] as u32; gs += p[1] as u32; bs += p[2] as u32; count += 1;
                    }
                }
            }
            if count > 0 {
                let avg = [
                    (rs / count) as u8, (gs / count) as u8, (bs / count) as u8, 255,
                ];
                for sy in by..by2 {
                    for sx in bx..bx2 {
                        if in_bounds(s, sx, sy) {
                            s.write(sx, sy, avg);
                        }
                    }
                }
            }
            bx += block;
        }
        by += block;
    }
}

// ── annotation dispatch ─────────────────────────────────────────────────────

pub fn render_annotation<S: Surface>(
    s:      &mut S,
    a:      &Annotation,
    source: Option<&ImageBuffer<Rgba<u8>, Vec<u8>>>,
) {
    match &a.kind {
        AnnotationKind::Pen      { stroke: Stroke { color, thickness }, points } |
        AnnotationKind::Marker   { stroke: Stroke { color, thickness }, points } => {
            polyline(s, points, *thickness, *color);
        }
        AnnotationKind::Line     { stroke: Stroke { color, thickness }, start, end } => {
            line(s, start.x as i32, start.y as i32, end.x as i32, end.y as i32, *thickness, *color);
        }
        AnnotationKind::Arrow    { stroke: Stroke { color, thickness }, start, end } => {
            arrow(s, *start, *end, *thickness, *color);
        }
        AnnotationKind::Rectangle{ stroke: Stroke { color, thickness }, start, end } => {
            stroke_rect(s, *start, *end, *thickness, *color);
        }
        AnnotationKind::Ellipse  { stroke: Stroke { color, thickness }, start, end } => {
            stroke_ellipse(s, *start, *end, *thickness, *color);
        }
        AnnotationKind::Step     { color, center, number } => {
            step_marker(s, *center, *number, *color);
        }
        AnnotationKind::Pixelate { start, end, block_size } => {
            if let Some(src) = source {
                pixelate_rect(s, src, *start, *end, *block_size);
            }
        }
        AnnotationKind::Text { color, position, text, font_size } => {
            // The 5×7 font scales by integer pixel multiples; map font_size
            // (in C# point-ish units) onto a sane scale: 14pt → 2×, 24pt → 3×,
            // 36pt → 4×. Default Stroke font_size is 14.0 → 2× → ~14px tall.
            let scale = ((*font_size / 7.0).round() as i32).max(1);
            draw_text(s, position.x as i32, position.y as i32, scale, text, *color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kashot_core::color::Rgba as K;

    fn make_buf(w: i32, h: i32) -> Vec<u32> { vec![0; (w * h) as usize] }

    #[test]
    fn line_stamps_at_least_one_pixel_per_endpoint() {
        let mut data = make_buf(20, 20);
        let mut s = U32Surface { buf: &mut data, stride: 20, height: 20 };
        line(&mut s, 2, 2, 17, 17, 1.0, K::WHITE);
        assert_ne!(data[2 * 20 + 2], 0, "start pixel was not written");
        assert_ne!(data[17 * 20 + 17], 0, "end pixel was not written");
    }

    #[test]
    fn stroke_rect_paints_all_four_edges() {
        let mut data = make_buf(20, 20);
        let mut s = U32Surface { buf: &mut data, stride: 20, height: 20 };
        stroke_rect(&mut s, Point2::new(3.0, 3.0), Point2::new(15.0, 15.0), 1.0, K::WHITE);
        assert_ne!(data[3 * 20 + 3],  0); // top-left
        assert_ne!(data[3 * 20 + 15], 0); // top-right
        assert_ne!(data[15 * 20 + 3], 0); // bottom-left
        assert_ne!(data[15 * 20 + 15],0); // bottom-right
    }

    #[test]
    fn arrow_writes_pixels_at_tip() {
        let mut data = make_buf(40, 40);
        let mut s = U32Surface { buf: &mut data, stride: 40, height: 40 };
        arrow(&mut s, Point2::new(5.0, 5.0), Point2::new(30.0, 30.0), 2.0, K::RED);
        assert_ne!(data[30 * 40 + 30], 0, "arrow tip should be painted");
    }
}
