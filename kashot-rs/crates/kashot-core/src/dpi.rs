//! Logical ↔ physical coordinate mapping for high-DPI (Retina / fractional
//! scaling) displays.
//!
//! # The two coordinate spaces
//!
//! * **Logical** — the space the OS reports monitor layout in, and the space
//!   the desktop is laid out in. macOS calls these points; Windows and X11
//!   call them desktop pixels. A monitor's logical rect is what the platform
//!   capture layer reports as `(x, y, width, height)`.
//! * **Physical** — real device pixels: what a captured bitmap actually
//!   contains, and what a framebuffer window is measured in. One logical unit
//!   is `scale` physical pixels, and `scale` is per monitor.
//!
//! On a plain 1× desktop the two spaces are identical, which is why nothing
//! here changes behaviour for the un-scaled case: every conversion collapses
//! to the identity.
//!
//! # Why the scale is *measured*, not trusted
//!
//! Each platform reports the pair differently — macOS reports monitor bounds
//! in points and hands back a bitmap in device pixels (2× on Retina), Windows
//! reports both in device pixels (so the ratio is 1 even when the DPI factor
//! is 1.5), X11 divides its rects by `Xft.dpi/96` while the captured image
//! stays at framebuffer resolution. Dividing the captured bitmap size by the
//! reported monitor size gives the ratio that actually relates the two spaces
//! on every platform, with no per-OS branching — see [`effective_scale`]. The
//! OS-reported factor is kept only as a fallback for degenerate input.
//!
//! # The stitched bitmap
//!
//! [`DisplayMap::from_monitors`] lays every monitor out inside one bitmap that
//! covers the whole virtual desktop. The bitmap is built at the *highest*
//! per-monitor scale so the sharpest display keeps every pixel it captured;
//! a lower-DPI monitor in a mixed layout is stretched into its slot instead of
//! the high-DPI one being thrown away. That keeps the logical→physical map a
//! single affine transform (`physical = (logical - origin) · scale`), which is
//! invertible, cheap, and the same for the overlay selection, the final crop,
//! pin placement and a recording region.

/// A rectangle in logical desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl LogicalRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
    /// Half-open containment: the right/bottom edges belong to the next rect,
    /// so tiled monitors never both claim a point on their shared seam.
    pub fn contains(&self, p: (f32, f32)) -> bool {
        p.0 >= self.x && p.0 < self.right() && p.1 >= self.y && p.1 < self.bottom()
    }
}

/// A rectangle in physical (device-pixel) coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl PhysicalRect {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
    pub const ZERO: PhysicalRect = PhysicalRect { x: 0, y: 0, w: 0, h: 0 };
    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    pub fn contains(&self, p: (i32, i32)) -> bool {
        p.0 >= self.x && p.0 < self.right() && p.1 >= self.y && p.1 < self.bottom()
    }
}

/// One monitor as the platform reports it: rect in logical coordinates plus
/// the scale that turns those into device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

impl MonitorGeometry {
    pub fn new(x: i32, y: i32, width: u32, height: u32, scale: f32) -> Self {
        Self { x, y, width, height, scale }
    }
    pub fn logical_rect(&self) -> LogicalRect {
        LogicalRect::new(self.x as f32, self.y as f32, self.width as f32, self.height as f32)
    }
}

/// A monitor placed inside the stitched bitmap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorPlacement {
    /// The monitor as the platform reports it (logical space).
    pub geometry: MonitorGeometry,
    /// Where this monitor's pixels live inside the stitched bitmap.
    pub physical: PhysicalRect,
}

/// Lowest / highest scale we will believe from an OS or a size ratio. A value
/// outside this range means something went wrong upstream; 1× is the safe
/// answer because it is what an un-scaled desktop uses.
const MIN_SCALE: f32 = 0.1;
const MAX_SCALE: f32 = 16.0;

/// Clamp a scale into a believable range, snapping near-integer-eighth values
/// (1.25, 1.5, 1.75, 2, …) to the exact fraction. Nonsense becomes 1.0.
pub fn sanitize_scale(scale: f32) -> f32 {
    if !scale.is_finite() || scale < MIN_SCALE || scale > MAX_SCALE {
        return 1.0;
    }
    snap_scale(scale)
}

fn snap_scale(scale: f32) -> f32 {
    let eighth = (scale * 8.0).round() / 8.0;
    if eighth >= MIN_SCALE && (scale - eighth).abs() <= 0.02 {
        eighth
    } else {
        scale
    }
}

/// Derive the scale relating a monitor's logical rect to the bitmap actually
/// captured from it.
///
/// `reported` is the OS-supplied factor, used only when the measurement can't
/// be trusted: a zero-sized input, or width and height ratios that disagree
/// (a rotated monitor, or a backend that hands back a transposed rect).
pub fn effective_scale(logical: (u32, u32), physical: (u32, u32), reported: f32) -> f32 {
    let fallback = sanitize_scale(reported);
    if logical.0 == 0 || logical.1 == 0 || physical.0 == 0 || physical.1 == 0 {
        return fallback;
    }
    let sx = physical.0 as f32 / logical.0 as f32;
    let sy = physical.1 as f32 / logical.1 as f32;
    if (sx - sy).abs() > 0.02 * sx.max(sy) {
        return fallback;
    }
    sanitize_scale((sx + sy) * 0.5)
}

/// Map a destination pixel index onto a source pixel index when a `src_len`
/// long run of pixels is stretched (or squeezed) into `dst_len`.
///
/// Nearest-neighbour on purpose: the runs being resampled here are
/// screenshots — hard edges and text — and interpolating them reads as blur
/// where blockiness reads as "this monitor is lower resolution". Shared by the
/// capture stitcher and the pinned-image window so both scale identically.
pub fn sample_index(dst: u32, dst_len: u32, src_len: u32) -> u32 {
    if dst_len == 0 || src_len == 0 {
        return 0;
    }
    if dst_len == src_len {
        return dst.min(src_len - 1);
    }
    let idx = (dst as u64 * src_len as u64 / dst_len as u64) as u32;
    idx.min(src_len - 1)
}

/// A capture region handed to a screen-recording backend, in both of the
/// conventions the three platforms use.
///
/// `x11grab` (Linux) and `gdigrab` (Windows) take absolute **device pixels**;
/// macOS `screencapture -R` takes **points**. Both come out of the same
/// [`DisplayMap`] so a region selected in the overlay means the same rectangle
/// whichever backend records it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrabRegion {
    /// Absolute device-pixel rect on the virtual desktop. Width and height are
    /// even: H.264 chroma subsampling can't encode an odd dimension, and a
    /// grabber that is asked for one fails outright.
    pub device: PhysicalRect,
    /// The same rectangle in logical units (points).
    pub points: LogicalRect,
}

/// The single logical↔physical authority: virtual-desktop origin, the scale
/// the stitched bitmap was built at, and where each monitor landed in it.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayMap {
    origin: (i32, i32),
    scale: f32,
    size: (u32, u32),
    monitors: Vec<MonitorPlacement>,
}

impl DisplayMap {
    /// A 1× map over a bitmap of the given size, anchored at the origin.
    /// Every conversion through it is the identity — the fallback whenever
    /// real monitor geometry isn't available.
    pub fn identity(width: u32, height: u32) -> Self {
        DisplayMap {
            origin: (0, 0),
            scale: 1.0,
            size: (width, height),
            monitors: vec![MonitorPlacement {
                geometry: MonitorGeometry::new(0, 0, width, height, 1.0),
                physical: PhysicalRect::new(0, 0, width, height),
            }],
        }
    }

    /// Lay every monitor out into one stitched bitmap.
    ///
    /// The bitmap covers the bounding box of the whole virtual desktop and is
    /// built at the highest per-monitor scale. Placements are derived by
    /// rounding each monitor's logical *edges*, so two monitors sharing a
    /// logical seam always share the same physical seam — no gap, no overlap,
    /// at any scale.
    pub fn from_monitors(monitors: &[MonitorGeometry]) -> Self {
        if monitors.is_empty() {
            return DisplayMap::identity(0, 0);
        }

        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        let mut scale = 1.0f32;
        for m in monitors {
            min_x = min_x.min(m.x);
            min_y = min_y.min(m.y);
            max_x = max_x.max(m.x.saturating_add(m.width as i32));
            max_y = max_y.max(m.y.saturating_add(m.height as i32));
            scale = scale.max(sanitize_scale(m.scale));
        }

        let origin = (min_x, min_y);
        let span_w = (max_x - min_x).max(0) as f32;
        let span_h = (max_y - min_y).max(0) as f32;
        let size = (
            (span_w * scale).round().max(0.0) as u32,
            (span_h * scale).round().max(0.0) as u32,
        );

        let mut map = DisplayMap { origin, scale, size, monitors: Vec::new() };
        map.monitors = monitors
            .iter()
            .map(|g| MonitorPlacement {
                geometry: *g,
                physical: map.logical_rect_to_physical(g.logical_rect()),
            })
            .collect();
        map
    }

    /// Virtual-desktop origin (top-left of the bounding box) in logical units.
    pub fn origin(&self) -> (i32, i32) {
        self.origin
    }

    /// Physical pixels per logical unit for the stitched bitmap as a whole.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Size of the stitched bitmap, in physical pixels.
    pub fn physical_size(&self) -> (u32, u32) {
        self.size
    }

    /// Size of the virtual desktop, in logical units.
    pub fn logical_size(&self) -> (f32, f32) {
        (self.size.0 as f32 / self.scale, self.size.1 as f32 / self.scale)
    }

    pub fn monitors(&self) -> &[MonitorPlacement] {
        &self.monitors
    }

    /// Top-left of the stitched bitmap in absolute device pixels — where a
    /// window covering the whole capture would have to be positioned.
    pub fn physical_origin(&self) -> (i32, i32) {
        (
            (self.origin.0 as f32 * self.scale).round() as i32,
            (self.origin.1 as f32 * self.scale).round() as i32,
        )
    }

    pub fn logical_to_physical(&self, p: (f32, f32)) -> (f32, f32) {
        (
            (p.0 - self.origin.0 as f32) * self.scale,
            (p.1 - self.origin.1 as f32) * self.scale,
        )
    }

    pub fn physical_to_logical(&self, p: (f32, f32)) -> (f32, f32) {
        (
            p.0 / self.scale + self.origin.0 as f32,
            p.1 / self.scale + self.origin.1 as f32,
        )
    }

    /// Map a logical rect into bitmap pixels, clamped to the bitmap. This is
    /// the conversion the final crop goes through.
    pub fn logical_rect_to_physical(&self, r: LogicalRect) -> PhysicalRect {
        self.map_rect(r, true)
    }

    /// Map a logical rect into *absolute* device pixels on the virtual desktop
    /// (i.e. not relative to the stitched bitmap, and not clamped to it).
    /// This is the form a screen grabber wants: `x11grab` / `gdigrab` offsets
    /// and window positions are absolute device pixels.
    pub fn logical_rect_to_device(&self, r: LogicalRect) -> PhysicalRect {
        let mut rect = self.map_rect(r, false);
        let (ox, oy) = self.physical_origin();
        rect.x += ox;
        rect.y += oy;
        rect
    }

    /// Bitmap pixels → absolute device pixels.
    pub fn physical_rect_to_device(&self, r: PhysicalRect) -> PhysicalRect {
        let (ox, oy) = self.physical_origin();
        PhysicalRect::new(r.x + ox, r.y + oy, r.w, r.h)
    }

    /// Bitmap pixels → logical desktop units. Used to place a window (a pin,
    /// say) that has to line up with the region it was cropped from, and to
    /// hand a region to a backend that speaks logical units — `screencapture
    /// -R` on macOS takes points, not pixels.
    pub fn physical_rect_to_logical(&self, r: PhysicalRect) -> LogicalRect {
        let (x, y) = self.physical_to_logical((r.x as f32, r.y as f32));
        LogicalRect::new(x, y, r.w as f32 / self.scale, r.h as f32 / self.scale)
    }

    /// The monitor containing a logical point, if any.
    pub fn monitor_at_logical(&self, p: (f32, f32)) -> Option<&MonitorPlacement> {
        self.monitors.iter().find(|m| m.geometry.logical_rect().contains(p))
    }

    /// The monitor containing a bitmap pixel, if any.
    pub fn monitor_at_physical(&self, p: (i32, i32)) -> Option<&MonitorPlacement> {
        self.monitors.iter().find(|m| m.physical.contains(p))
    }

    /// Per-monitor scale at a logical point, falling back to the bitmap scale
    /// outside every monitor (the dead corners of an L-shaped layout).
    pub fn scale_at_logical(&self, p: (f32, f32)) -> f32 {
        self.monitor_at_logical(p)
            .map(|m| sanitize_scale(m.geometry.scale))
            .unwrap_or(self.scale)
    }

    /// Per-monitor scale at a bitmap pixel.
    pub fn scale_at_physical(&self, p: (i32, i32)) -> f32 {
        self.monitor_at_physical(p)
            .map(|m| sanitize_scale(m.geometry.scale))
            .unwrap_or(self.scale)
    }

    /// Turn a selection in bitmap pixels into a recording region.
    ///
    /// Nothing passes a region to the recorder yet — it records the whole
    /// desktop — but when one does, this is the conversion it goes through, so
    /// the recorded rectangle and the screenshot crop can't drift apart.
    pub fn grab_region(&self, selection: PhysicalRect) -> GrabRegion {
        let mut device = self.physical_rect_to_device(selection);
        device.w &= !1;
        device.h &= !1;
        GrabRegion {
            device,
            points: self.physical_rect_to_logical(PhysicalRect::new(
                selection.x, selection.y, device.w, device.h,
            )),
        }
    }

    fn map_rect(&self, r: LogicalRect, clamp: bool) -> PhysicalRect {
        if r.is_empty() {
            return PhysicalRect::ZERO;
        }
        let (lx, ly) = self.logical_to_physical((r.x, r.y));
        let (rx, ry) = self.logical_to_physical((r.right(), r.bottom()));
        let mut x0 = lx.round() as i32;
        let mut y0 = ly.round() as i32;
        let mut x1 = rx.round() as i32;
        let mut y1 = ry.round() as i32;
        if clamp {
            x0 = x0.clamp(0, self.size.0 as i32);
            y0 = y0.clamp(0, self.size.1 as i32);
            x1 = x1.clamp(0, self.size.0 as i32);
            y1 = y1.clamp(0, self.size.1 as i32);
        }
        // A sub-pixel selection still has to produce a pixel to crop.
        if x1 <= x0 {
            x1 = x0 + 1;
            if clamp && x1 > self.size.0 as i32 {
                x0 = (self.size.0 as i32 - 1).max(0);
                x1 = self.size.0 as i32;
            }
        }
        if y1 <= y0 {
            y1 = y0 + 1;
            if clamp && y1 > self.size.1 as i32 {
                y0 = (self.size.1 as i32 - 1).max(0);
                y1 = self.size.1 as i32;
            }
        }
        PhysicalRect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32)
    }
}

impl Default for DisplayMap {
    fn default() -> Self {
        DisplayMap::identity(0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "{a} != {b}");
    }

    // ── scale 1.0: nothing may move ──────────────────────────────────────

    #[test]
    fn scale_one_is_the_identity() {
        let map = DisplayMap::from_monitors(&[MonitorGeometry::new(0, 0, 1920, 1080, 1.0)]);
        assert_eq!(map.scale(), 1.0);
        assert_eq!(map.physical_size(), (1920, 1080));
        assert_eq!(map.origin(), (0, 0));
        assert_eq!(map.logical_to_physical((640.0, 480.0)), (640.0, 480.0));
        assert_eq!(map.physical_to_logical((640.0, 480.0)), (640.0, 480.0));
        assert_eq!(
            map.logical_rect_to_physical(LogicalRect::new(10.0, 20.0, 300.0, 400.0)),
            PhysicalRect::new(10, 20, 300, 400)
        );
        assert_eq!(
            map.monitors()[0].physical,
            PhysicalRect::new(0, 0, 1920, 1080)
        );
    }

    #[test]
    fn identity_map_matches_bitmap() {
        let map = DisplayMap::identity(1280, 720);
        assert_eq!(map.scale(), 1.0);
        assert_eq!(map.physical_size(), (1280, 720));
        assert_eq!(map.scale_at_logical((10.0, 10.0)), 1.0);
        assert_eq!(
            map.logical_rect_to_device(LogicalRect::new(0.0, 0.0, 100.0, 50.0)),
            PhysicalRect::new(0, 0, 100, 50)
        );
    }

    // ── scale 2.0 (Retina) ───────────────────────────────────────────────

    #[test]
    fn retina_doubles_the_bitmap() {
        let map = DisplayMap::from_monitors(&[MonitorGeometry::new(0, 0, 1440, 900, 2.0)]);
        assert_eq!(map.scale(), 2.0);
        assert_eq!(map.physical_size(), (2880, 1800));
        assert_eq!(map.logical_to_physical((100.0, 50.0)), (200.0, 100.0));
        assert_eq!(map.physical_to_logical((200.0, 100.0)), (100.0, 50.0));
        assert_eq!(
            map.logical_rect_to_physical(LogicalRect::new(100.0, 50.0, 400.0, 300.0)),
            PhysicalRect::new(200, 100, 800, 600)
        );
    }

    #[test]
    fn retina_crop_round_trips_back_to_logical() {
        let map = DisplayMap::from_monitors(&[MonitorGeometry::new(0, 0, 1440, 900, 2.0)]);
        let rect = PhysicalRect::new(200, 100, 800, 600);
        let back = map.physical_rect_to_logical(rect);
        assert_eq!(back, LogicalRect::new(100.0, 50.0, 400.0, 300.0));
    }

    // ── scale 1.5 (fractional) ───────────────────────────────────────────

    #[test]
    fn fractional_scale_rounds_edges() {
        let map = DisplayMap::from_monitors(&[MonitorGeometry::new(0, 0, 1920, 1080, 1.5)]);
        assert_eq!(map.scale(), 1.5);
        assert_eq!(map.physical_size(), (2880, 1620));
        // Edges, not extents: 10 → 15 and 43 → 64.5 → 65, so the crop is 50
        // wide. Rounding the width on its own (33 · 1.5 = 49.5 → 50) happens
        // to agree here, but it drifts as soon as the origin is odd.
        assert_eq!(
            map.logical_rect_to_physical(LogicalRect::new(10.0, 10.0, 33.0, 33.0)),
            PhysicalRect::new(15, 15, 50, 50)
        );
        // Odd origin: 11 → 16.5 → 17 and 44 → 66, so the same 33-wide
        // selection is 49 physical pixels here.
        assert_eq!(
            map.logical_rect_to_physical(LogicalRect::new(11.0, 11.0, 33.0, 33.0)),
            PhysicalRect::new(17, 17, 49, 49)
        );
    }

    #[test]
    fn fractional_scale_leaves_no_seam() {
        // Two 1.5× monitors side by side: the right edge of the left one has
        // to be the exact left edge of the right one, or the stitched bitmap
        // shows a black hairline.
        let map = DisplayMap::from_monitors(&[
            MonitorGeometry::new(0, 0, 1707, 960, 1.5),
            MonitorGeometry::new(1707, 0, 1707, 960, 1.5),
        ]);
        let a = map.monitors()[0].physical;
        let b = map.monitors()[1].physical;
        assert_eq!(a.right(), b.x);
        assert_eq!(a.x, 0);
        assert_eq!(b.right() as u32, map.physical_size().0);
    }

    // ── mixed-scale multi-monitor ────────────────────────────────────────

    #[test]
    fn mixed_scale_layout_uses_the_sharpest_monitor() {
        // Retina laptop left of a 1× external display.
        let map = DisplayMap::from_monitors(&[
            MonitorGeometry::new(0, 0, 1440, 900, 2.0),
            MonitorGeometry::new(1440, 0, 1920, 1080, 1.0),
        ]);
        assert_eq!(map.scale(), 2.0);
        assert_eq!(map.physical_size(), (6720, 2160));
        assert_eq!(map.monitors()[0].physical, PhysicalRect::new(0, 0, 2880, 1800));
        assert_eq!(map.monitors()[1].physical, PhysicalRect::new(2880, 0, 3840, 2160));
        // Per-monitor scale still reports what each display really is.
        assert_eq!(map.scale_at_logical((100.0, 100.0)), 2.0);
        assert_eq!(map.scale_at_logical((2000.0, 100.0)), 1.0);
        assert_eq!(map.scale_at_physical((100, 100)), 2.0);
        assert_eq!(map.scale_at_physical((4000, 100)), 1.0);
        // A point in the dead space below the laptop belongs to no monitor.
        assert!(map.monitor_at_logical((100.0, 1500.0)).is_none());
        assert_eq!(map.scale_at_logical((100.0, 1500.0)), 2.0);
    }

    #[test]
    fn mixed_scale_selection_spanning_both_monitors_maps_whole_rect() {
        let map = DisplayMap::from_monitors(&[
            MonitorGeometry::new(0, 0, 1440, 900, 2.0),
            MonitorGeometry::new(1440, 0, 1920, 1080, 1.0),
        ]);
        let sel = LogicalRect::new(1400.0, 100.0, 200.0, 100.0);
        assert_eq!(
            map.logical_rect_to_physical(sel),
            PhysicalRect::new(2800, 200, 400, 200)
        );
    }

    #[test]
    fn negative_origin_layout_anchors_at_zero() {
        // Secondary monitor to the left of / above the primary.
        let map = DisplayMap::from_monitors(&[
            MonitorGeometry::new(0, 0, 1920, 1080, 2.0),
            MonitorGeometry::new(-1280, -200, 1280, 1024, 2.0),
        ]);
        assert_eq!(map.origin(), (-1280, -200));
        assert_eq!(map.physical_size(), (6400, 2560));
        assert_eq!(map.monitors()[1].physical, PhysicalRect::new(0, 0, 2560, 2048));
        assert_eq!(map.monitors()[0].physical, PhysicalRect::new(2560, 400, 3840, 2160));
        assert_eq!(map.logical_to_physical((-1280.0, -200.0)), (0.0, 0.0));
        assert_eq!(map.physical_origin(), (-2560, -400));
        // A grabber wants absolute device pixels, not bitmap-relative ones.
        assert_eq!(
            map.logical_rect_to_device(LogicalRect::new(-1280.0, -200.0, 100.0, 50.0)),
            PhysicalRect::new(-2560, -400, 200, 100)
        );
    }

    // ── clamping / degenerate input ──────────────────────────────────────

    #[test]
    fn crop_is_clamped_to_the_bitmap() {
        let map = DisplayMap::from_monitors(&[MonitorGeometry::new(0, 0, 800, 600, 2.0)]);
        let huge = map.logical_rect_to_physical(LogicalRect::new(-100.0, -100.0, 2000.0, 2000.0));
        assert_eq!(huge, PhysicalRect::new(0, 0, 1600, 1200));
        let past_edge = map.logical_rect_to_physical(LogicalRect::new(790.0, 590.0, 100.0, 100.0));
        assert_eq!(past_edge, PhysicalRect::new(1580, 1180, 20, 20));
    }

    #[test]
    fn sub_pixel_selection_still_yields_a_pixel() {
        let map = DisplayMap::from_monitors(&[MonitorGeometry::new(0, 0, 800, 600, 1.0)]);
        let tiny = map.logical_rect_to_physical(LogicalRect::new(10.0, 10.0, 0.2, 0.2));
        assert_eq!(tiny, PhysicalRect::new(10, 10, 1, 1));
        let at_edge = map.logical_rect_to_physical(LogicalRect::new(800.0, 600.0, 0.2, 0.2));
        assert_eq!(at_edge, PhysicalRect::new(799, 599, 1, 1));
        assert!(map
            .logical_rect_to_physical(LogicalRect::new(10.0, 10.0, 0.0, 50.0))
            .is_empty());
    }

    #[test]
    fn empty_monitor_list_falls_back_to_identity() {
        let map = DisplayMap::from_monitors(&[]);
        assert_eq!(map.scale(), 1.0);
        assert_eq!(map.physical_size(), (0, 0));
    }

    // ── effective_scale ──────────────────────────────────────────────────

    #[test]
    fn effective_scale_measures_retina() {
        // macOS: bounds in points, bitmap in device pixels.
        assert_eq!(effective_scale((1440, 900), (2880, 1800), 2.0), 2.0);
    }

    #[test]
    fn effective_scale_is_one_when_both_sides_are_device_pixels() {
        // Windows at 150% DPI still reports the rect in device pixels, so the
        // bitmap and the rect agree and nothing must be rescaled.
        assert_eq!(effective_scale((2560, 1440), (2560, 1440), 1.5), 1.0);
    }

    #[test]
    fn effective_scale_snaps_truncated_fractional_rects() {
        // X11 at Xft.dpi 144 reports floor(2560/1.5) = 1706 and captures 2560.
        assert_eq!(effective_scale((1706, 960), (2560, 1440), 1.5), 1.5);
    }

    #[test]
    fn effective_scale_falls_back_when_axes_disagree() {
        // Transposed / rotated rect: trust the OS instead of the ratio.
        assert_eq!(effective_scale((1080, 1920), (3840, 2160), 2.0), 2.0);
    }

    #[test]
    fn effective_scale_survives_degenerate_input() {
        assert_eq!(effective_scale((0, 0), (2880, 1800), 2.0), 2.0);
        assert_eq!(effective_scale((1440, 900), (0, 0), f32::NAN), 1.0);
        assert_eq!(effective_scale((1440, 900), (0, 0), 0.0), 1.0);
        assert_eq!(effective_scale((1440, 900), (0, 0), 1e9), 1.0);
    }

    // ── recording region ─────────────────────────────────────────────────

    #[test]
    fn grab_region_is_device_pixels_and_points() {
        let map = DisplayMap::from_monitors(&[MonitorGeometry::new(0, 0, 1440, 900, 2.0)]);
        let region = map.grab_region(PhysicalRect::new(200, 100, 800, 600));
        assert_eq!(region.device, PhysicalRect::new(200, 100, 800, 600));
        assert_eq!(region.points, LogicalRect::new(100.0, 50.0, 400.0, 300.0));
    }

    #[test]
    fn grab_region_rounds_odd_dimensions_down() {
        let map = DisplayMap::identity(1920, 1080);
        let region = map.grab_region(PhysicalRect::new(11, 13, 101, 67));
        assert_eq!(region.device, PhysicalRect::new(11, 13, 100, 66));
        assert_eq!(region.points, LogicalRect::new(11.0, 13.0, 100.0, 66.0));
    }

    #[test]
    fn grab_region_offsets_by_the_virtual_origin() {
        let map = DisplayMap::from_monitors(&[
            MonitorGeometry::new(0, 0, 1920, 1080, 1.0),
            MonitorGeometry::new(-1920, 0, 1920, 1080, 1.0),
        ]);
        // Bitmap pixel (1920, 0) is the primary monitor's top-left.
        let region = map.grab_region(PhysicalRect::new(1920, 0, 640, 480));
        assert_eq!(region.device, PhysicalRect::new(0, 0, 640, 480));
        assert_eq!(region.points, LogicalRect::new(0.0, 0.0, 640.0, 480.0));
    }

    #[test]
    fn sample_index_is_identity_at_the_same_length() {
        for i in 0..4 {
            assert_eq!(sample_index(i, 4, 4), i);
        }
    }

    #[test]
    fn sample_index_stretches_and_squeezes() {
        // 2 source pixels across 8 destination pixels: 4 each.
        assert_eq!(sample_index(0, 8, 2), 0);
        assert_eq!(sample_index(3, 8, 2), 0);
        assert_eq!(sample_index(4, 8, 2), 1);
        assert_eq!(sample_index(7, 8, 2), 1);
        // 8 source pixels down to 2.
        assert_eq!(sample_index(0, 2, 8), 0);
        assert_eq!(sample_index(1, 2, 8), 4);
    }

    #[test]
    fn sample_index_never_runs_off_the_source() {
        assert_eq!(sample_index(99, 8, 2), 1);
        assert_eq!(sample_index(0, 0, 4), 0);
        assert_eq!(sample_index(0, 4, 0), 0);
    }

    #[test]
    fn sanitize_scale_snaps_and_clamps() {
        approx(sanitize_scale(1.4999), 1.5);
        approx(sanitize_scale(2.0001), 2.0);
        approx(sanitize_scale(1.3333), 1.3333);
        approx(sanitize_scale(-1.0), 1.0);
        approx(sanitize_scale(f32::INFINITY), 1.0);
    }

    #[test]
    fn logical_and_physical_round_trip_at_every_scale() {
        for scale in [1.0f32, 1.5, 2.0, 3.0] {
            let map = DisplayMap::from_monitors(&[MonitorGeometry::new(-100, 40, 1000, 800, scale)]);
            for p in [(-100.0f32, 40.0), (0.0, 100.0), (899.0, 839.0)] {
                let there = map.logical_to_physical(p);
                let back = map.physical_to_logical(there);
                approx(back.0, p.0);
                approx(back.1, p.1);
            }
        }
    }
}
