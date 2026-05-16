//! Anti-aliased icon glyphs for the overlay toolbar / action panel.
//!
//! Replaces the previous 2-px Bresenham approach which read as pixelated.
//! Renders each icon into a `tiny_skia::Pixmap` with round caps + round
//! joins at 2.2-px stroke width, then alpha-blends into the softbuffer
//! u32 framebuffer. tiny-skia is already a workspace dep.

use kashot_core::tool::Tool;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Tool(Tool),
    Color,
    Thickness,
    Undo,
    Redo,
    Pin,
    Copy,
    Save,
    Close,
}

/// Color override for the Color/Thickness/etc. icons. `accent_rgba` is used
/// for the Color disc fill and the Thickness stroke width hint.
pub fn render_icon(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32,
    icon: IconKind,
    fg_rgba:    [u8; 4],
    accent_rgba: Option<[u8; 4]>,
    thickness:   f32,
) {
    let w = (x1 - x0).max(1) as u32;
    let h = (y1 - y0).max(1) as u32;
    let Some(mut pixmap) = Pixmap::new(w, h) else { return; };

    let mut paint = Paint::default();
    paint.set_color_rgba8(fg_rgba[0], fg_rgba[1], fg_rgba[2], fg_rgba[3]);
    paint.anti_alias = true;

    let stroke = Stroke {
        width:     2.2,
        line_cap:  LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };

    let cw  = w as f32;
    let ch  = h as f32;
    let pad = 8.0;
    let ix0 = pad;
    let iy0 = pad;
    let ix1 = cw - pad;
    let iy1 = ch - pad;
    let cx  = cw / 2.0;
    let cy  = ch / 2.0;

    match icon {
        IconKind::Tool(Tool::Pen) => {
            // Outlined pen barrel + filled nib triangle at the lower-left.
            // Single stroke weight matches Rectangle / Ellipse / Copy so the
            // whole tool row reads as a family rather than mixed weights.
            let mut barrel = PathBuilder::new();
            barrel.move_to(ix0 + 3.0,  iy1 - 2.0);
            barrel.line_to(ix0 + 6.0,  iy1 + 1.0);
            barrel.line_to(ix1 + 1.0,  iy0 + 6.0);
            barrel.line_to(ix1 - 2.0,  iy0 + 3.0);
            barrel.close();
            if let Some(p) = barrel.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
            // Nib — small filled triangle at the lower-left tip of the barrel.
            let mut nib = PathBuilder::new();
            nib.move_to(ix0 - 1.0,  iy1 + 2.0);
            nib.line_to(ix0 + 3.0,  iy1 - 2.0);
            nib.line_to(ix0 + 6.0,  iy1 + 1.0);
            nib.close();
            if let Some(p) = nib.finish() {
                pixmap.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }
        IconKind::Tool(Tool::Line) => {
            let mut pb = PathBuilder::new();
            pb.move_to(ix0, iy1);
            pb.line_to(ix1, iy0);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
        }
        IconKind::Tool(Tool::Arrow) => {
            let mut pb = PathBuilder::new();
            pb.move_to(ix0, iy1);
            pb.line_to(ix1, iy0);
            // Arrowhead — two short strokes back from tip
            pb.move_to(ix1, iy0);
            pb.line_to(ix1 - 7.0, iy0);
            pb.move_to(ix1, iy0);
            pb.line_to(ix1, iy0 + 7.0);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
        }
        IconKind::Tool(Tool::Rectangle) => {
            if let Some(r) = Rect::from_xywh(ix0, iy0, ix1 - ix0, iy1 - iy0) {
                let mut pb = PathBuilder::new();
                pb.push_rect(r);
                if let Some(p) = pb.finish() {
                    pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
                }
            }
        }
        IconKind::Tool(Tool::Ellipse) => {
            if let Some(r) = Rect::from_xywh(ix0, iy0, ix1 - ix0, iy1 - iy0) {
                let mut pb = PathBuilder::new();
                pb.push_oval(r);
                if let Some(p) = pb.finish() {
                    pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
                }
            }
        }
        IconKind::Tool(Tool::Marker) => {
            // Highlighter body — filled rounded rect with a slanted tip on top.
            if let Some(body) = Rect::from_xywh(ix0 + 2.0, iy0 + 6.0, ix1 - ix0 - 4.0, iy1 - iy0 - 6.0) {
                let mut pb = PathBuilder::new();
                pb.push_rect(body);
                if let Some(p) = pb.finish() {
                    pixmap.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
            if let Some(tip) = Rect::from_xywh(ix0 + 4.0, iy0, ix1 - ix0 - 8.0, 6.0) {
                let mut pb = PathBuilder::new();
                pb.push_rect(tip);
                if let Some(p) = pb.finish() {
                    pixmap.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
        }
        IconKind::Tool(Tool::Text) => {
            // Bold "T" — top bar + vertical stem.
            let bold = Stroke { width: 3.0, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
            let mut pb = PathBuilder::new();
            pb.move_to(ix0, iy0 + 1.0);
            pb.line_to(ix1, iy0 + 1.0);
            pb.move_to(cx, iy0 + 1.0);
            pb.line_to(cx, iy1);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &bold, Transform::identity(), None);
            }
        }
        IconKind::Tool(Tool::Step) => {
            // Outlined circle (matches Ellipse / Rectangle) with a stroked
            // "1" inside, so it reads as a numbered step rather than a Color
            // swatch (which is a filled disc).
            let r = (ix1 - ix0).min(iy1 - iy0) / 2.0 - 1.0;
            let mut circle = PathBuilder::new();
            circle.push_circle(cx, cy, r);
            if let Some(p) = circle.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
            let mut one = PathBuilder::new();
            one.move_to(cx - 2.2, cy - r * 0.30);
            one.line_to(cx + 0.4, cy - r * 0.62);
            one.line_to(cx + 0.4, cy + r * 0.55);
            if let Some(p) = one.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
        }
        IconKind::Tool(Tool::Pixelate) => {
            // 3×3 mosaic with varying opacity — reads as "low-res / pixelated"
            // rather than a chessboard. Corner cells are dim, edges medium,
            // center solid.
            let cw_step = (ix1 - ix0) / 3.0;
            let ch_step = (iy1 - iy0) / 3.0;
            // (col, row) → alpha multiplier
            let alphas: [[u8; 3]; 3] = [
                [110, 170, 110],
                [170, 255, 170],
                [110, 170, 110],
            ];
            for gy in 0..3 {
                for gx in 0..3 {
                    let mut cell_paint = Paint::default();
                    let a = alphas[gy][gx];
                    cell_paint.set_color_rgba8(fg_rgba[0], fg_rgba[1], fg_rgba[2],
                                                ((fg_rgba[3] as u16 * a as u16) / 255) as u8);
                    cell_paint.anti_alias = true;
                    if let Some(r) = Rect::from_xywh(
                        ix0 + gx as f32 * cw_step + 0.5,
                        iy0 + gy as f32 * ch_step + 0.5,
                        cw_step - 1.0, ch_step - 1.0,
                    ) {
                        let mut pb = PathBuilder::new();
                        pb.push_rect(r);
                        if let Some(p) = pb.finish() {
                            pixmap.fill_path(&p, &cell_paint, FillRule::Winding, Transform::identity(), None);
                        }
                    }
                }
            }
        }
        IconKind::Color => {
            // Filled disc using the active stroke color.
            let c = accent_rgba.unwrap_or(fg_rgba);
            let mut p2 = Paint::default();
            p2.set_color_rgba8(c[0], c[1], c[2], c[3]);
            p2.anti_alias = true;
            let r = (ix1 - ix0).min(iy1 - iy0) / 2.0 - 1.0;
            let mut pb = PathBuilder::new();
            pb.push_circle(cx, cy, r);
            if let Some(p) = pb.finish() {
                pixmap.fill_path(&p, &p2, FillRule::Winding, Transform::identity(), None);
            }
        }
        IconKind::Thickness => {
            // Horizontal stroke at the active thickness.
            let s2 = Stroke { width: thickness.max(2.0).min(8.0), line_cap: LineCap::Round, ..Default::default() };
            let mut pb = PathBuilder::new();
            pb.move_to(ix0, cy);
            pb.line_to(ix1, cy);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &s2, Transform::identity(), None);
            }
        }
        IconKind::Undo => {
            // Rounded counter-clockwise arc through the top, ending at the
            // lower-left with an arrowhead pointing down — clearly "go back."
            use std::f32::consts::PI;
            let r = ((ix1 - ix0).min(iy1 - iy0) / 2.0 - 1.5).max(4.0);
            let start_a =  PI / 4.0;             // 4-5 o'clock
            let end_a   = -PI * 5.0 / 4.0;       // sweeps CCW through top to 8 o'clock
            let segments = 28;
            let mut pb = PathBuilder::new();
            let mut last = (0.0f32, 0.0f32);
            for i in 0..=segments {
                let t = i as f32 / segments as f32;
                let a = start_a + (end_a - start_a) * t;
                let x = cx + r * a.cos();
                let y = cy + r * a.sin();
                if i == 0 { pb.move_to(x, y); } else { pb.line_to(x, y); }
                last = (x, y);
            }
            // Arrowhead at the lower-left endpoint, pointing back along the
            // arc's tangent (down-and-into-arc).
            let tan_a  = end_a - PI / 2.0;
            let back_a = tan_a + PI;
            let ah = 4.5f32;
            pb.move_to(last.0, last.1);
            pb.line_to(last.0 + ah * (back_a + 0.55).cos(),
                       last.1 + ah * (back_a + 0.55).sin());
            pb.move_to(last.0, last.1);
            pb.line_to(last.0 + ah * (back_a - 0.55).cos(),
                       last.1 + ah * (back_a - 0.55).sin());
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
        }
        IconKind::Redo => {
            // Mirror of Undo — clockwise arc through the top, ending at the
            // lower-right with arrowhead pointing down ("go forward").
            use std::f32::consts::PI;
            let r = ((ix1 - ix0).min(iy1 - iy0) / 2.0 - 1.5).max(4.0);
            let start_a =  PI * 3.0 / 4.0;       // 7-8 o'clock
            let end_a   = -PI / 4.0 + 2.0 * PI;  // sweeps CW through top to 4 o'clock
            let segments = 28;
            let mut pb = PathBuilder::new();
            let mut last = (0.0f32, 0.0f32);
            for i in 0..=segments {
                let t = i as f32 / segments as f32;
                let a = start_a + (end_a - start_a) * t;
                let x = cx + r * a.cos();
                let y = cy + r * a.sin();
                if i == 0 { pb.move_to(x, y); } else { pb.line_to(x, y); }
                last = (x, y);
            }
            let tan_a  = end_a + PI / 2.0;
            let back_a = tan_a + PI;
            let ah = 4.5f32;
            pb.move_to(last.0, last.1);
            pb.line_to(last.0 + ah * (back_a + 0.55).cos(),
                       last.1 + ah * (back_a + 0.55).sin());
            pb.move_to(last.0, last.1);
            pb.line_to(last.0 + ah * (back_a - 0.55).cos(),
                       last.1 + ah * (back_a - 0.55).sin());
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
        }
        IconKind::Pin => {
            // Google-Maps-style location pin: teardrop body with a hole.
            // The body is a filled shape — a circle on top tapering to a point
            // at the bottom. The hole is a small stroked circle at the center
            // of the round top so the silhouette reads as a marker.
            let head_r  = 6.0;
            let head_cy = iy0 + 2.0 + head_r;
            let tip_y   = iy1;
            // Teardrop: arc from left-of-head down to tip, then up to right-of-head.
            let mut body = PathBuilder::new();
            body.move_to(cx - head_r, head_cy);
            body.cubic_to(
                cx - head_r,  head_cy + head_r * 0.6,
                cx - 2.5,     tip_y - 2.0,
                cx,           tip_y,
            );
            body.cubic_to(
                cx + 2.5,     tip_y - 2.0,
                cx + head_r,  head_cy + head_r * 0.6,
                cx + head_r,  head_cy,
            );
            // Close the top with a semicircle.
            body.cubic_to(
                cx + head_r,  head_cy - head_r * 1.1,
                cx - head_r,  head_cy - head_r * 1.1,
                cx - head_r,  head_cy,
            );
            body.close();
            if let Some(p) = body.finish() {
                pixmap.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
            }
            // Punch the marker hole out of the head with a clear-blend circle.
            let mut hole_paint = Paint::default();
            hole_paint.set_color_rgba8(0, 0, 0, 0);
            hole_paint.anti_alias = true;
            hole_paint.blend_mode = tiny_skia::BlendMode::Clear;
            let mut hole = PathBuilder::new();
            hole.push_circle(cx, head_cy, 2.4);
            if let Some(p) = hole.finish() {
                pixmap.fill_path(&p, &hole_paint, FillRule::Winding, Transform::identity(), None);
            }
        }
        IconKind::Copy => {
            // Page-with-shadow: two identical stroked rects, the back one
            // offset down-and-right so it reads as a copy stack rather than
            // two arbitrary rectangles.
            let off  = 4.0;
            let w    = ix1 - ix0 - off;
            let h    = iy1 - iy0 - off;
            if let Some(back) = Rect::from_xywh(ix0 + off, iy0 + off, w, h) {
                let mut pb = PathBuilder::new();
                pb.push_rect(back);
                if let Some(p) = pb.finish() {
                    pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
                }
            }
            if let Some(front) = Rect::from_xywh(ix0, iy0, w, h) {
                let mut pb = PathBuilder::new();
                pb.push_rect(front);
                if let Some(p) = pb.finish() {
                    pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
                }
            }
        }
        IconKind::Save => {
            // 3.5" floppy disk: square body with one chamfered top-right
            // corner, a metal shutter strip across the top, and a paper-label
            // rectangle filling the lower half.
            let chamfer = 4.0;
            let mut body = PathBuilder::new();
            body.move_to(ix0,           iy0);
            body.line_to(ix1 - chamfer, iy0);
            body.line_to(ix1,           iy0 + chamfer);
            body.line_to(ix1,           iy1);
            body.line_to(ix0,           iy1);
            body.close();
            if let Some(p) = body.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
            // Top shutter strip — filled, with a slot punched on the right.
            let strip_h = 3.5;
            let strip_pad = 2.0;
            if let Some(strip) = Rect::from_xywh(
                ix0 + strip_pad,
                iy0 + 1.0,
                ix1 - ix0 - strip_pad * 2.0 - chamfer * 0.5,
                strip_h,
            ) {
                let mut pb = PathBuilder::new();
                pb.push_rect(strip);
                if let Some(p) = pb.finish() {
                    pixmap.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
            // Paper label — stroked rectangle in the lower portion.
            if let Some(label) = Rect::from_xywh(
                ix0 + 2.5,
                cy + 1.0,
                ix1 - ix0 - 5.0,
                iy1 - cy - 3.0,
            ) {
                let mut pb = PathBuilder::new();
                pb.push_rect(label);
                if let Some(p) = pb.finish() {
                    pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
                }
            }
        }
        IconKind::Close => {
            let mut pb = PathBuilder::new();
            pb.move_to(ix0, iy0);
            pb.line_to(ix1, iy1);
            pb.move_to(ix1, iy0);
            pb.line_to(ix0, iy1);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
        }
    }

    blit_pixmap(buf, stride, height, x0, y0, &pixmap);
}

/// Source-over blit of a tiny-skia premultiplied pixmap into the softbuffer
/// u32 framebuffer. Premultiplied → straight blend formula:
///   `dst = src + dst * (1 - src.a)` with src already in `(R*A, G*A, B*A)`.
fn blit_pixmap(
    buf: &mut [u32], stride: usize, height: usize,
    x: i32, y: i32, pixmap: &Pixmap,
) {
    let pw = pixmap.width()  as i32;
    let ph = pixmap.height() as i32;
    let pixels = pixmap.pixels();
    for py in 0..ph {
        for px in 0..pw {
            let pix = pixels[(py * pw + px) as usize];
            let a = pix.alpha() as u32;
            if a == 0 { continue; }
            let dst_x = x + px;
            let dst_y = y + py;
            if dst_x < 0 || (dst_x as usize) >= stride { continue; }
            if dst_y < 0 || (dst_y as usize) >= height { continue; }
            let dst_idx = dst_y as usize * stride + dst_x as usize;
            let dst = buf[dst_idx];
            let dr = (dst >> 16) & 0xFF;
            let dg = (dst >> 8)  & 0xFF;
            let db =  dst        & 0xFF;
            let inv = 255 - a;
            // pix.red() etc. on PremultipliedColorU8 are already premultiplied.
            let r = (pix.red()   as u32) + (dr * inv + 127) / 255;
            let g = (pix.green() as u32) + (dg * inv + 127) / 255;
            let b = (pix.blue()  as u32) + (db * inv + 127) / 255;
            let r = r.min(255);
            let g = g.min(255);
            let b = b.min(255);
            buf[dst_idx] = (r << 16) | (g << 8) | b;
        }
    }
}
