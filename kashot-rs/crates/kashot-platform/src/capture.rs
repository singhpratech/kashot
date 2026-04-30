//! Cross-platform screen capture via the `xcap` crate.
//!
//! `xcap` is built on `windows-capture` (Win), `xcb` / `wayland-rs` (Linux),
//! and `core-graphics` (macOS) under the hood. It already handles the
//! "different OS API per platform" reality so we don't have to maintain
//! three implementations of this file.
//!
//! Returned `Captured` carries raw RGBA8 bytes laid out row-major, plus the
//! virtual-screen offset of each monitor — enough to stitch them into a
//! single bitmap if we want to (matching the C# `SystemInformation.VirtualScreen`
//! behavior).

use crate::{Error, Result};
use image::{ImageBuffer, Rgba};

#[derive(Debug, Clone)]
pub struct Captured {
    /// Stitched bitmap covering the bounding box of all monitors.
    pub bitmap: ImageBuffer<Rgba<u8>, Vec<u8>>,
    /// Bounding-box origin in virtual-screen coordinates (top-left).
    pub virtual_origin: (i32, i32),
    /// Per-monitor frames, in screen order. Already drawn into `bitmap`,
    /// kept around so callers can do per-monitor logic if they need to.
    pub monitors: Vec<MonitorFrame>,
}

#[derive(Debug, Clone)]
pub struct MonitorFrame {
    pub x: i32,
    pub y: i32,
    pub width:  u32,
    pub height: u32,
    pub name:   String,
    pub scale_factor: f32,
}

/// Capture every monitor and stitch into one bitmap.
///
/// Coordinates of pixel `(px, py)` in `bitmap` correspond to virtual-screen
/// coordinates `(virtual_origin.0 + px as i32, virtual_origin.1 + py as i32)`.
pub fn capture_all_screens() -> Result<Captured> {
    let monitors = xcap::Monitor::all()
        .map_err(|e| Error::Capture(format!("Monitor::all: {e}")))?;
    if monitors.is_empty() {
        return Err(Error::Capture("no monitors found".into()));
    }

    // Bounding box of every monitor in virtual-screen space.
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for m in &monitors {
        let x = m.x().map_err(|e| Error::Capture(format!("monitor x: {e}")))?;
        let y = m.y().map_err(|e| Error::Capture(format!("monitor y: {e}")))?;
        let w = m.width().map_err(|e| Error::Capture(format!("monitor w: {e}")))? as i32;
        let h = m.height().map_err(|e| Error::Capture(format!("monitor h: {e}")))? as i32;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w);
        max_y = max_y.max(y + h);
    }

    let total_w = (max_x - min_x) as u32;
    let total_h = (max_y - min_y) as u32;

    let mut canvas: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(total_w, total_h, Rgba([0, 0, 0, 255]));

    let mut frames = Vec::with_capacity(monitors.len());
    for m in monitors {
        let x = m.x().map_err(|e| Error::Capture(format!("monitor x: {e}")))?;
        let y = m.y().map_err(|e| Error::Capture(format!("monitor y: {e}")))?;
        let w = m.width().map_err(|e| Error::Capture(format!("monitor w: {e}")))?;
        let h = m.height().map_err(|e| Error::Capture(format!("monitor h: {e}")))?;
        let name = m.name().map_err(|e| Error::Capture(format!("monitor name: {e}")))?;
        let scale = m.scale_factor().map_err(|e| Error::Capture(format!("scale: {e}")))?;

        let img = m.capture_image()
            .map_err(|e| Error::Capture(format!("capture {name}: {e}")))?;

        // Blit into canvas at offset (x - min_x, y - min_y).
        let ox = (x - min_x) as i64;
        let oy = (y - min_y) as i64;
        for (px, py, pixel) in img.enumerate_pixels() {
            let cx = ox + px as i64;
            let cy = oy + py as i64;
            if cx >= 0 && cy >= 0 && (cx as u32) < total_w && (cy as u32) < total_h {
                canvas.put_pixel(cx as u32, cy as u32, *pixel);
            }
        }

        frames.push(MonitorFrame {
            x, y, width: w, height: h, name,
            scale_factor: scale,
        });
    }

    Ok(Captured {
        bitmap: canvas,
        virtual_origin: (min_x, min_y),
        monitors: frames,
    })
}
