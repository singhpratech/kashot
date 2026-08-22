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
use kashot_core::virtual_desktop::{union_bounds, DesktopGeometry, MonitorRect};

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

impl Captured {
    /// Virtual-desktop geometry of this capture: where the stitched bitmap
    /// starts in virtual-screen space, how big it is, and the monitors it
    /// was built from. The overlay places itself with this and maps every
    /// selection back through it, so a region picked on a monitor left of
    /// (or above) the primary one crops the pixels the user actually saw.
    pub fn geometry(&self) -> DesktopGeometry {
        let rects: Vec<MonitorRect> = self.monitors.iter()
            .map(|m| MonitorRect::new(m.x, m.y, m.width, m.height))
            .collect();
        DesktopGeometry::from_monitors(rects)
            .unwrap_or_else(|| DesktopGeometry::bitmap(self.bitmap.width(), self.bitmap.height()))
    }
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

    // Bounding box of every monitor in virtual-screen space. The union is
    // computed by `kashot_core::virtual_desktop` so the overlay's placement
    // and this stitch can never drift apart.
    let mut rects = Vec::with_capacity(monitors.len());
    for m in &monitors {
        let x = m.x().map_err(|e| Error::Capture(format!("monitor x: {e}")))?;
        let y = m.y().map_err(|e| Error::Capture(format!("monitor y: {e}")))?;
        let w = m.width().map_err(|e| Error::Capture(format!("monitor w: {e}")))?;
        let h = m.height().map_err(|e| Error::Capture(format!("monitor h: {e}")))?;
        rects.push(MonitorRect::new(x, y, w, h));
    }
    let ((min_x, min_y), (total_w, total_h)) = union_bounds(&rects)
        .ok_or_else(|| Error::Capture("no monitors found".into()))?;

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
