//! Cross-platform screen capture via the `xcap` crate.
//!
//! `xcap` is built on `windows-capture` (Win), `xcb` / `wayland-rs` (Linux),
//! and `core-graphics` (macOS) under the hood. It already handles the
//! "different OS API per platform" reality so we don't have to maintain
//! three implementations of this file.
//!
//! # High-DPI
//!
//! A monitor's reported rect and the bitmap it hands back are *not* in the
//! same units on every platform: macOS reports points and captures device
//! pixels (2× on Retina), X11 divides its rects by `Xft.dpi/96`, Windows
//! reports both in device pixels. `kashot_core::dpi` owns that mapping —
//! [`DisplayMap`] measures the real ratio per monitor, lays every monitor out
//! into one bitmap built at the sharpest scale, and is the single helper the
//! overlay selection, the final crop, pin placement and any recording region
//! convert through. The 1× case maps through it unchanged.
//!
//! Returned `Captured` carries raw RGBA8 bytes laid out row-major, the
//! virtual-screen offset of the stitched bitmap, and the map that relates the
//! two coordinate spaces.

use crate::{Error, Result};
use image::{ImageBuffer, Rgba};
use kashot_core::dpi::{effective_scale, sample_index, DisplayMap, MonitorGeometry, PhysicalRect};
use kashot_core::region::DesktopBounds;
use kashot_core::virtual_desktop::{DesktopGeometry, MonitorRect};

#[derive(Debug, Clone)]
pub struct Captured {
    /// Stitched bitmap covering the bounding box of all monitors, in
    /// **physical** (device) pixels.
    pub bitmap: ImageBuffer<Rgba<u8>, Vec<u8>>,
    /// Bounding-box origin in virtual-screen coordinates (top-left), in
    /// **logical** units. `map.physical_origin()` is the same point in
    /// device pixels.
    pub virtual_origin: (i32, i32),
    /// Logical ↔ physical mapping for `bitmap`. Everything that turns a
    /// screen coordinate into a bitmap pixel (or back) goes through this.
    pub map: DisplayMap,
    /// Per-monitor frames, in screen order. Already drawn into `bitmap`,
    /// kept around so callers can do per-monitor logic if they need to.
    pub monitors: Vec<MonitorFrame>,
}

impl Captured {
    /// The virtual-desktop rectangle this capture covers.
    ///
    /// Region recording clamps the user's selection against exactly this — the
    /// bitmap the selection was dragged over — so the rectangle handed to the
    /// encoder can never name a pixel that wasn't on screen, and so both sides
    /// agree on where the desktop's corner is even if the monitor layout
    /// changes between the capture and the start of the recording.
    ///
    /// Device pixels, like `geometry()`: the origin is the display map's
    /// physical origin and the size is the stitched bitmap's, so a selection
    /// made on the overlay clamps against the very pixels it was drawn on.
    pub fn desktop_bounds(&self) -> DesktopBounds {
        let (ox, oy) = self.map.physical_origin();
        DesktopBounds::new(ox, oy, self.bitmap.width(), self.bitmap.height())
    }
}

#[derive(Debug, Clone)]
pub struct MonitorFrame {
    /// Monitor origin/size as the platform reports it — logical units.
    pub x: i32,
    pub y: i32,
    pub width:  u32,
    pub height: u32,
    pub name:   String,
    /// Scale factor as reported by the OS. Informational: it disagrees with
    /// reality on some platforms (see `effective_scale`).
    pub scale_factor: f32,
    /// Physical pixels per logical unit, measured from the bitmap this
    /// monitor actually produced.
    pub effective_scale: f32,
    /// Where this monitor's pixels live inside `Captured::bitmap`.
    pub physical: PhysicalRect,
}

impl Captured {
    /// Virtual-desktop geometry of this capture: where the stitched bitmap
    /// starts in virtual-screen space, how big it is, and the monitors it
    /// was built from. The overlay places itself with this and maps every
    /// selection back through it, so a region picked on a monitor left of
    /// (or above) the primary one crops the pixels the user actually saw.
    ///
    /// Everything here is in *device* pixels — the unit the bitmap, the
    /// overlay window and winit's cursor all share — so the monitor slots are
    /// the `physical` rects the stitch produced, offset by the desktop's
    /// device-pixel origin. At 1x that is exactly the OS-reported layout.
    pub fn geometry(&self) -> DesktopGeometry {
        let (ox, oy) = self.map.physical_origin();
        let rects: Vec<MonitorRect> = self.monitors.iter()
            .map(|m| MonitorRect::new(ox + m.physical.x, oy + m.physical.y,
                                      m.physical.w, m.physical.h))
            .collect();
        DesktopGeometry::from_monitors(rects)
            .filter(|g| g.size() == (self.bitmap.width(), self.bitmap.height()))
            .unwrap_or_else(|| DesktopGeometry::bitmap(self.bitmap.width(), self.bitmap.height()))
    }
}

/// Capture every monitor and stitch into one bitmap.
///
/// Pixel `(px, py)` of `bitmap` is the logical point
/// `map.physical_to_logical((px, py))` on the virtual screen; the inverse,
/// `map.logical_to_physical`, turns a screen coordinate into a bitmap pixel.
/// At 1× that is `(virtual_origin.0 + px, virtual_origin.1 + py)`, exactly as
/// before.
pub fn capture_all_screens() -> Result<Captured> {
    let monitors = xcap::Monitor::all()
        .map_err(|e| Error::Capture(format!("Monitor::all: {e}")))?;
    if monitors.is_empty() {
        return Err(Error::Capture("no monitors found".into()));
    }

    // Grab every monitor first: the scale that relates a monitor's reported
    // rect to its pixels can only be measured once we hold both.
    let mut frames: Vec<MonitorFrame> = Vec::with_capacity(monitors.len());
    let mut panels: Vec<(MonitorGeometry, ImageBuffer<Rgba<u8>, Vec<u8>>)> =
        Vec::with_capacity(monitors.len());
    for m in monitors {
        let x = m.x().map_err(|e| Error::Capture(format!("monitor x: {e}")))?;
        let y = m.y().map_err(|e| Error::Capture(format!("monitor y: {e}")))?;
        let w = m.width().map_err(|e| Error::Capture(format!("monitor w: {e}")))?;
        let h = m.height().map_err(|e| Error::Capture(format!("monitor h: {e}")))?;
        let name = m.name().map_err(|e| Error::Capture(format!("monitor name: {e}")))?;
        let scale = m.scale_factor().map_err(|e| Error::Capture(format!("scale: {e}")))?;

        let img = m.capture_image()
            .map_err(|e| Error::Capture(format!("capture {name}: {e}")))?;

        let effective = effective_scale((w, h), (img.width(), img.height()), scale);
        frames.push(MonitorFrame {
            x, y, width: w, height: h, name,
            scale_factor: scale,
            effective_scale: effective,
            physical: PhysicalRect::ZERO,
        });
        panels.push((MonitorGeometry::new(x, y, w, h, effective), img));
    }

    let (canvas, map) = stitch(&panels)?;
    drop(panels);

    for (frame, placement) in frames.iter_mut().zip(map.monitors()) {
        frame.physical = placement.physical;
    }

    Ok(Captured {
        bitmap: canvas,
        virtual_origin: map.origin(),
        map,
        monitors: frames,
    })
}

/// Lay every monitor's bitmap out into one virtual-desktop bitmap.
///
/// Split out from `capture_all_screens` so the geometry — which is all the
/// high-DPI risk lives in — is testable without a screen attached.
fn stitch(
    panels: &[(MonitorGeometry, ImageBuffer<Rgba<u8>, Vec<u8>>)],
) -> Result<(ImageBuffer<Rgba<u8>, Vec<u8>>, DisplayMap)> {
    let geometries: Vec<MonitorGeometry> = panels.iter().map(|(g, _)| *g).collect();
    let map = DisplayMap::from_monitors(&geometries);
    let (total_w, total_h) = map.physical_size();
    if total_w == 0 || total_h == 0 {
        return Err(Error::Capture("monitor layout has zero area".into()));
    }

    let mut canvas: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(total_w, total_h, Rgba([0, 0, 0, 255]));
    for (idx, (_, img)) in panels.iter().enumerate() {
        blit(&mut canvas, total_w, total_h, map.monitors()[idx].physical, img);
    }
    Ok((canvas, map))
}

/// Draw one monitor's capture into its slot in the stitched canvas.
///
/// The common case — every monitor at the same scale — lands here with the
/// source and the slot exactly the same size, so it is a row-wise memcpy and
/// no resampling happens. A mixed-scale layout stretches the lower-DPI
/// monitor into its slot (nearest-neighbour: a screenshot has hard edges and
/// text, and smoothing them looks worse than the blockiness). A one- or
/// two-pixel disagreement — the rounding slack a fractional scale leaves —
/// copies straight across instead of resampling the whole screen for it.
fn blit(
    canvas: &mut ImageBuffer<Rgba<u8>, Vec<u8>>,
    canvas_w: u32,
    canvas_h: u32,
    dest: PhysicalRect,
    src: &ImageBuffer<Rgba<u8>, Vec<u8>>,
) {
    if dest.is_empty() || src.width() == 0 || src.height() == 0 {
        return;
    }
    // Clip the slot to the canvas — by construction it already fits, but a
    // blit that walks off the end would be a panic rather than a glitch.
    let x0 = dest.x.max(0);
    let y0 = dest.y.max(0);
    let x1 = dest.right().min(canvas_w as i32);
    let y1 = dest.bottom().min(canvas_h as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let (sw, sh) = (src.width(), src.height());
    let one_to_one = (sw as i64 - dest.w as i64).abs() <= 2 && (sh as i64 - dest.h as i64).abs() <= 2;
    let stride = canvas_w as usize * 4;
    let src_stride = sw as usize * 4;
    let src_buf = src.as_raw();
    let dst_buf: &mut [u8] = &mut *canvas;

    for dy in y0..y1 {
        let row_in_dest = (dy - dest.y) as u32;
        let sy = if one_to_one { row_in_dest } else { sample_index(row_in_dest, dest.h, sh) };
        if sy >= sh {
            continue;
        }
        let src_row = sy as usize * src_stride;
        let dst_row = dy as usize * stride;

        if one_to_one {
            let cols = ((x1 - x0) as usize).min(sw as usize - (x0 - dest.x).max(0) as usize);
            if cols == 0 {
                continue;
            }
            let sx0 = (x0 - dest.x).max(0) as usize;
            let s = src_row + sx0 * 4;
            let d = dst_row + x0 as usize * 4;
            dst_buf[d..d + cols * 4].copy_from_slice(&src_buf[s..s + cols * 4]);
        } else {
            for dx in x0..x1 {
                let col_in_dest = (dx - dest.x) as u32;
                let sx = sample_index(col_in_dest, dest.w, sw);
                if sx >= sw {
                    continue;
                }
                let s = src_row + sx as usize * 4;
                let d = dst_row + dx as usize * 4;
                dst_buf[d..d + 4].copy_from_slice(&src_buf[s..s + 4]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
        ImageBuffer::from_pixel(w, h, Rgba(c))
    }

    /// A monitor's capture: `w`x`h` device pixels of `fill`, with a distinct
    /// pixel in each corner so a flipped or offset blit is visible.
    fn panel(g: MonitorGeometry, w: u32, h: u32, fill: [u8; 4]) -> (MonitorGeometry, ImageBuffer<Rgba<u8>, Vec<u8>>) {
        let mut img = solid(w, h, fill);
        img.put_pixel(0, 0, Rgba([1, 1, 1, 255]));
        img.put_pixel(w - 1, h - 1, Rgba([2, 2, 2, 255]));
        (g, img)
    }

    #[test]
    fn stitch_at_1x_is_unchanged() {
        let panels = vec![
            panel(MonitorGeometry::new(0, 0, 40, 30, 1.0), 40, 30, [10, 0, 0, 255]),
            panel(MonitorGeometry::new(40, 0, 20, 30, 1.0), 20, 30, [0, 10, 0, 255]),
        ];
        let (canvas, map) = stitch(&panels).unwrap();
        assert_eq!(map.scale(), 1.0);
        assert_eq!((canvas.width(), canvas.height()), (60, 30));
        assert_eq!(canvas.get_pixel(0, 0), &Rgba([1, 1, 1, 255]));
        assert_eq!(canvas.get_pixel(39, 29), &Rgba([2, 2, 2, 255]));
        assert_eq!(canvas.get_pixel(40, 0), &Rgba([1, 1, 1, 255]));
        assert_eq!(canvas.get_pixel(59, 29), &Rgba([2, 2, 2, 255]));
        assert_eq!(canvas.get_pixel(5, 5), &Rgba([10, 0, 0, 255]));
        assert_eq!(canvas.get_pixel(45, 5), &Rgba([0, 10, 0, 255]));
    }

    #[test]
    fn stitch_keeps_every_retina_pixel() {
        // Reported rect in points, bitmap in device pixels — the macOS case
        // that used to land a quarter of the screen in a quarter-sized canvas.
        let panels = vec![panel(MonitorGeometry::new(0, 0, 40, 30, 2.0), 80, 60, [7, 7, 7, 255])];
        let (canvas, map) = stitch(&panels).unwrap();
        assert_eq!(map.scale(), 2.0);
        assert_eq!((canvas.width(), canvas.height()), (80, 60));
        assert_eq!(canvas.get_pixel(0, 0), &Rgba([1, 1, 1, 255]));
        assert_eq!(canvas.get_pixel(79, 59), &Rgba([2, 2, 2, 255]));
        // A selection made in the overlay is in these same pixels.
        assert_eq!(
            map.logical_rect_to_physical(kashot_core::dpi::LogicalRect::new(10.0, 10.0, 20.0, 10.0)),
            PhysicalRect::new(20, 20, 40, 20)
        );
    }

    #[test]
    fn stitch_at_1_5x_covers_the_whole_canvas() {
        let panels = vec![panel(MonitorGeometry::new(0, 0, 40, 30, 1.5), 60, 45, [3, 3, 3, 255])];
        let (canvas, _) = stitch(&panels).unwrap();
        assert_eq!((canvas.width(), canvas.height()), (60, 45));
        assert_eq!(canvas.get_pixel(59, 44), &Rgba([2, 2, 2, 255]));
        // Nothing left at the fill colour of an empty canvas.
        assert!(canvas.pixels().all(|p| p.0 != [0, 0, 0, 255]));
    }

    #[test]
    fn stitch_mixed_scales_keeps_the_sharp_monitor_sharp() {
        // 2x laptop left of a 1x external display.
        let panels = vec![
            panel(MonitorGeometry::new(0, 0, 40, 30, 2.0), 80, 60, [5, 0, 0, 255]),
            panel(MonitorGeometry::new(40, 0, 40, 20, 1.0), 40, 20, [0, 5, 0, 255]),
        ];
        let (canvas, map) = stitch(&panels).unwrap();
        assert_eq!(map.scale(), 2.0);
        assert_eq!((canvas.width(), canvas.height()), (160, 60));
        // Laptop: untouched, pixel for pixel.
        assert_eq!(canvas.get_pixel(0, 0), &Rgba([1, 1, 1, 255]));
        assert_eq!(canvas.get_pixel(79, 59), &Rgba([2, 2, 2, 255]));
        // External: stretched into its 80x40 slot, top-left corner intact.
        assert_eq!(canvas.get_pixel(80, 0), &Rgba([1, 1, 1, 255]));
        assert_eq!(canvas.get_pixel(81, 1), &Rgba([1, 1, 1, 255]));
        assert_eq!(canvas.get_pixel(159, 39), &Rgba([2, 2, 2, 255]));
        assert_eq!(canvas.get_pixel(100, 10), &Rgba([0, 5, 0, 255]));
        // Below the shorter external display stays canvas black.
        assert_eq!(canvas.get_pixel(100, 50), &Rgba([0, 0, 0, 255]));
    }

    #[test]
    fn stitch_rejects_an_empty_layout() {
        assert!(stitch(&[]).is_err());
    }

    #[test]
    fn blit_copies_one_to_one_without_resampling() {
        let mut canvas = solid(8, 4, [0, 0, 0, 255]);
        let mut src = solid(4, 4, [10, 20, 30, 255]);
        src.put_pixel(0, 0, Rgba([1, 2, 3, 255]));
        blit(&mut canvas, 8, 4, PhysicalRect::new(4, 0, 4, 4), &src);
        assert_eq!(canvas.get_pixel(4, 0), &Rgba([1, 2, 3, 255]));
        assert_eq!(canvas.get_pixel(5, 0), &Rgba([10, 20, 30, 255]));
        assert_eq!(canvas.get_pixel(7, 3), &Rgba([10, 20, 30, 255]));
        // Untouched half stays as it was.
        assert_eq!(canvas.get_pixel(0, 0), &Rgba([0, 0, 0, 255]));
    }

    #[test]
    fn blit_stretches_a_lower_dpi_monitor_into_its_slot() {
        let mut canvas = solid(8, 8, [0, 0, 0, 255]);
        let mut src = solid(2, 2, [0, 0, 0, 255]);
        src.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        src.put_pixel(1, 1, Rgba([0, 0, 255, 255]));
        blit(&mut canvas, 8, 8, PhysicalRect::new(0, 0, 8, 8), &src);
        assert_eq!(canvas.get_pixel(0, 0), &Rgba([255, 0, 0, 255]));
        assert_eq!(canvas.get_pixel(3, 3), &Rgba([255, 0, 0, 255]));
        assert_eq!(canvas.get_pixel(4, 4), &Rgba([0, 0, 255, 255]));
        assert_eq!(canvas.get_pixel(7, 7), &Rgba([0, 0, 255, 255]));
    }

    #[test]
    fn blit_clips_a_slot_that_runs_past_the_canvas() {
        let mut canvas = solid(4, 4, [0, 0, 0, 255]);
        let src = solid(4, 4, [9, 9, 9, 255]);
        blit(&mut canvas, 4, 4, PhysicalRect::new(2, 2, 4, 4), &src);
        assert_eq!(canvas.get_pixel(3, 3), &Rgba([9, 9, 9, 255]));
        assert_eq!(canvas.get_pixel(1, 1), &Rgba([0, 0, 0, 255]));
    }
}
