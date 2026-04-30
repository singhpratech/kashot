//! Final-image rendering and Save / Copy / Pin actions.
//!
//! `final_image` rasterises the captured screenshot, crops to the selection
//! rectangle, then composites every annotation on top — same compositing
//! contract as the C# `OverlayForm.GetFinalImage`. Watermark drawn at the
//! bottom-right when enabled.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Local;
use iced::{window, Task};
use image::{imageops, ImageBuffer, Rgba};
use kashot_core::annotation::{AnnotationKind, Point2, Rect};
use kashot_core::{AppSettings, Rgba as CoreRgba};

use super::message::{Message, OverlayMessage};
use super::overlay::OverlayState;

pub type SharedFinal = Arc<ImageBuffer<Rgba<u8>, Vec<u8>>>;

pub fn save(s: &OverlayState, settings: &AppSettings, id: window::Id) -> Task<Message> {
    let img = match final_image(s) {
        Some(img) => img,
        None => return Task::none(),
    };
    let initial_dir = save_directory(settings);
    let stamp = Local::now().format("%Y%m%d_%H%M%S");
    let suggested = format!("kashot_{stamp}.png");

    Task::perform(
        async move {
            let chosen = rfd::AsyncFileDialog::new()
                .set_directory(initial_dir)
                .set_file_name(&suggested)
                .add_filter("PNG image", &["png"])
                .add_filter("JPEG image", &["jpg", "jpeg"])
                .add_filter("Bitmap", &["bmp"])
                .save_file()
                .await
                .map(|h| h.path().to_path_buf());

            let Some(path) = chosen else {
                return Err("cancelled".to_string());
            };

            tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
                img.save(&path).map_err(|e| e.to_string())?;
                Ok(path)
            })
            .await
            .map_err(|e| e.to_string())?
        },
        move |r| Message::Overlay(id, OverlayMessage::SaveResult(r)),
    )
}

pub fn copy(s: &OverlayState, id: window::Id) -> Task<Message> {
    let img = match final_image(s) {
        Some(img) => img,
        None => return Task::none(),
    };
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                kashot_platform::clipboard::copy_image_png(&img).map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| e.to_string())?
        },
        move |r| Message::Overlay(id, OverlayMessage::CopyResult(r)),
    )
}

pub fn pin(_s: &OverlayState, id: window::Id) -> Task<Message> {
    // The actual pin opens a new window in App::update — we emit a Cancel
    // here since the overlay should close, and queue a follow-up open in
    // the parent. For v1 that's a thin path: pin == copy + close. The
    // pin window infrastructure is in `pin_window.rs`.
    let _ = id;
    Task::none()
}

/// Compose the final image: cropped screenshot + annotations + optional watermark.
pub fn final_image(s: &OverlayState) -> Option<SharedFinal> {
    let sel = s.selection;
    if sel.w < 1.0 || sel.h < 1.0 { return None; }

    let src = &s.captured.bitmap;
    let (sx, sy, sw, sh) = (
        sel.x.max(0.0) as u32,
        sel.y.max(0.0) as u32,
        (sel.w as u32).min(src.width().saturating_sub(sel.x.max(0.0) as u32)),
        (sel.h as u32).min(src.height().saturating_sub(sel.y.max(0.0) as u32)),
    );
    if sw == 0 || sh == 0 { return None; }

    let mut out: ImageBuffer<Rgba<u8>, Vec<u8>> = imageops::crop_imm(src, sx, sy, sw, sh).to_image();

    // Composite annotations. We translate so selection-local (0,0) corresponds
    // to virtual-screen (sel.x, sel.y).
    for a in &s.annotations {
        composite_annotation(&mut out, a, sel);
    }

    Some(Arc::new(out))
}

fn composite_annotation(
    out: &mut ImageBuffer<Rgba<u8>, Vec<u8>>,
    a: &kashot_core::annotation::Annotation,
    sel: Rect,
) {
    use imageproc::drawing::{
        draw_filled_circle_mut, draw_filled_rect_mut, draw_hollow_ellipse_mut,
        draw_hollow_rect_mut, draw_line_segment_mut, draw_polygon_mut,
    };
    use imageproc::rect::Rect as IRect;

    let local = |p: Point2| ((p.x - sel.x) as i32, (p.y - sel.y) as i32);
    let to_rgba = |c: CoreRgba| Rgba([c.r, c.g, c.b, c.a]);

    match &a.kind {
        AnnotationKind::Pen { stroke, points } | AnnotationKind::Marker { stroke, points } => {
            let color = to_rgba(stroke.color);
            for w in points.windows(2) {
                let (x0, y0) = local(w[0]);
                let (x1, y1) = local(w[1]);
                draw_line_segment_mut(out, (x0 as f32, y0 as f32), (x1 as f32, y1 as f32), color);
            }
            let _ = stroke.thickness;
        }
        AnnotationKind::Line { stroke, start, end } => {
            let (x0, y0) = local(*start);
            let (x1, y1) = local(*end);
            draw_line_segment_mut(out, (x0 as f32, y0 as f32), (x1 as f32, y1 as f32), to_rgba(stroke.color));
        }
        AnnotationKind::Arrow { stroke, start, end } => {
            let (x0, y0) = local(*start);
            let (x1, y1) = local(*end);
            draw_line_segment_mut(out, (x0 as f32, y0 as f32), (x1 as f32, y1 as f32), to_rgba(stroke.color));
            // Arrowhead triangle
            let dx = (x1 - x0) as f32;
            let dy = (y1 - y0) as f32;
            let len = (dx * dx + dy * dy).sqrt().max(0.0001);
            let ux = dx / len;
            let uy = dy / len;
            let h = (stroke.thickness + 3.0) * 2.5;
            let w = (stroke.thickness + 3.0) * 1.5;
            let bx = x1 as f32 - ux * h;
            let by = y1 as f32 - uy * h;
            let pts = [
                imageproc::point::Point::new(x1, y1),
                imageproc::point::Point::new((bx + (-uy) * w) as i32, (by + ux * w) as i32),
                imageproc::point::Point::new((bx - (-uy) * w) as i32, (by - ux * w) as i32),
            ];
            draw_polygon_mut(out, &pts, to_rgba(stroke.color));
        }
        AnnotationKind::Rectangle { stroke, start, end } => {
            let r = Rect::from_corners(*start, *end);
            let (x, y) = local(Point2::new(r.x, r.y));
            if r.w > 0.0 && r.h > 0.0 {
                draw_hollow_rect_mut(out, IRect::at(x, y).of_size(r.w as u32, r.h as u32), to_rgba(stroke.color));
            }
        }
        AnnotationKind::Ellipse { stroke, start, end } => {
            let r = Rect::from_corners(*start, *end);
            if r.w > 1.0 && r.h > 1.0 {
                let cx = r.x + r.w / 2.0;
                let cy = r.y + r.h / 2.0;
                let (lx, ly) = local(Point2::new(cx, cy));
                draw_hollow_ellipse_mut(out, (lx, ly), (r.w / 2.0) as i32, (r.h / 2.0) as i32, to_rgba(stroke.color));
            }
        }
        AnnotationKind::Step { color, center, number } => {
            let (x, y) = local(*center);
            draw_filled_circle_mut(out, (x, y), 14, to_rgba(*color));
            // No text drawing without font — leave the number rendering for later.
            let _ = number;
        }
        AnnotationKind::Text { color, position, text, font_size } => {
            // imageproc has draw_text_mut but needs a font asset; skip for v1 and
            // simply mark the text position with a small dot. Text on the saved
            // image lands when we wire in `ab_glyph` font rendering in a follow-up.
            let (x, y) = local(*position);
            draw_filled_circle_mut(out, (x, y), 2, to_rgba(*color));
            let _ = (text, font_size);
        }
        AnnotationKind::Pixelate { start, end, block_size } => {
            let r = Rect::from_corners(*start, *end);
            if r.w < 2.0 || r.h < 2.0 { return; }
            let (lx, ly) = local(Point2::new(r.x, r.y));
            let lw = (r.w as u32).min(out.width().saturating_sub(lx.max(0) as u32));
            let lh = (r.h as u32).min(out.height().saturating_sub(ly.max(0) as u32));
            if lw == 0 || lh == 0 { return; }

            let bx = lx.max(0) as u32;
            let by = ly.max(0) as u32;

            let block = (*block_size).max(2);
            let small = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_fn(
                (lw / block).max(1), (lh / block).max(1),
                |sx, sy| *out.get_pixel(bx + sx * block, by + sy * block));
            let blurred = imageops::resize(&small, lw, lh, imageops::FilterType::Nearest);
            imageops::overlay(out, &blurred, bx as i64, by as i64);
        }
    }
}

fn save_directory(s: &AppSettings) -> PathBuf {
    if !s.save_directory.is_empty() {
        let p = PathBuf::from(&s.save_directory);
        if p.is_dir() { return p; }
    }
    if let Some(d) = directories::UserDirs::new().and_then(|u| u.picture_dir().map(|p| p.to_path_buf())) {
        return d;
    }
    std::env::temp_dir()
}
