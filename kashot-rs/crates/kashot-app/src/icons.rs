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
            // Pen body — diagonal stroke
            let mut pb = PathBuilder::new();
            pb.move_to(ix0 + 4.0, iy1);
            pb.line_to(ix1, iy0 + 2.0);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
            // Pen tip — filled triangle at the bottom-left
            let mut pb2 = PathBuilder::new();
            pb2.move_to(ix0,       iy1);
            pb2.line_to(ix0 + 6.0, iy1);
            pb2.line_to(ix0 + 4.0, iy1 - 6.0);
            pb2.close();
            if let Some(p) = pb2.finish() {
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
            // Filled disc — same shape as an actual numbered step.
            let r = (ix1 - ix0).min(iy1 - iy0) / 2.0;
            let mut pb = PathBuilder::new();
            pb.push_circle(cx, cy, r);
            if let Some(p) = pb.finish() {
                pixmap.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }
        IconKind::Tool(Tool::Pixelate) => {
            // 4×4 mosaic — alternating filled cells.
            let cw_step = (ix1 - ix0) / 4.0;
            let ch_step = (iy1 - iy0) / 4.0;
            for gy in 0..4 {
                for gx in 0..4 {
                    if (gx + gy) & 1 == 0 {
                        if let Some(r) = Rect::from_xywh(
                            ix0 + gx as f32 * cw_step,
                            iy0 + gy as f32 * ch_step,
                            cw_step, ch_step,
                        ) {
                            let mut pb = PathBuilder::new();
                            pb.push_rect(r);
                            if let Some(p) = pb.finish() {
                                pixmap.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
                            }
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
            // Curved arrow pointing left.
            let mut pb = PathBuilder::new();
            pb.move_to(ix1 - 1.0, iy0 + 4.0);
            pb.cubic_to(ix1 - 1.0, iy0 + 12.0, ix0 + 4.0, iy0 + 14.0, ix0 + 2.0, iy1 - 3.0);
            // Arrowhead
            pb.move_to(ix0 + 2.0,  iy1 - 3.0);
            pb.line_to(ix0 + 6.0,  iy1 - 8.0);
            pb.move_to(ix0 + 2.0,  iy1 - 3.0);
            pb.line_to(ix0 - 1.0,  iy1 - 8.0);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
        }
        IconKind::Redo => {
            // Mirrored — curved arrow pointing right.
            let mut pb = PathBuilder::new();
            pb.move_to(ix0 + 1.0, iy0 + 4.0);
            pb.cubic_to(ix0 + 1.0, iy0 + 12.0, ix1 - 4.0, iy0 + 14.0, ix1 - 2.0, iy1 - 3.0);
            pb.move_to(ix1 - 2.0,  iy1 - 3.0);
            pb.line_to(ix1 - 6.0,  iy1 - 8.0);
            pb.move_to(ix1 - 2.0,  iy1 - 3.0);
            pb.line_to(ix1 + 1.0,  iy1 - 8.0);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
        }
        IconKind::Pin => {
            // Thumbtack: top bar + filled triangle + shaft.
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 7.0, cy - 7.0);
            pb.line_to(cx + 7.0, cy - 7.0);
            pb.move_to(cx, cy + 3.0);
            pb.line_to(cx, iy1);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
            }
            let mut pb2 = PathBuilder::new();
            pb2.move_to(cx - 5.0, cy - 6.0);
            pb2.line_to(cx + 5.0, cy - 6.0);
            pb2.line_to(cx,       cy + 3.0);
            pb2.close();
            if let Some(p) = pb2.finish() {
                pixmap.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }
        IconKind::Copy => {
            // Two stacked rounded rects (back + front sheets of a copy stack).
            if let (Some(r1), Some(r2)) = (
                Rect::from_xywh(ix0,        iy0,        ix1 - ix0 - 5.0, iy1 - iy0 - 5.0),
                Rect::from_xywh(ix0 + 5.0,  iy0 + 5.0,  ix1 - ix0 - 5.0, iy1 - iy0 - 5.0),
            ) {
                let mut pb = PathBuilder::new();
                pb.push_rect(r1);
                if let Some(p) = pb.finish() {
                    pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
                }
                let mut pb2 = PathBuilder::new();
                pb2.push_rect(r2);
                if let Some(p) = pb2.finish() {
                    pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
                }
            }
        }
        IconKind::Save => {
            // Modern save — down arrow into a tray.
            let mut pb = PathBuilder::new();
            pb.move_to(cx, iy0);
            pb.line_to(cx, iy1 - 4.0);
            pb.move_to(cx - 5.0, iy1 - 9.0);
            pb.line_to(cx,       iy1 - 4.0);
            pb.line_to(cx + 5.0, iy1 - 9.0);
            pb.move_to(ix0,  iy1);
            pb.line_to(ix1,  iy1);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
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
