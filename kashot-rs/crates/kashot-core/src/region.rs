//! Screen-region geometry for recording.
//!
//! Pure math, shared by the region-selection overlay and by every recorder
//! backend, so a rectangle the user drags means exactly the same thing under
//! X11 `x11grab`, Windows `gdigrab` and macOS `screencapture` — and so the
//! encoder never sees a size it can't accept.
//!
//! **Coordinates are virtual-desktop pixels**: the space the capture stitches
//! its monitors into. `DesktopBounds` names the origin of that space, which is
//! not necessarily `(0, 0)` — a monitor placed left of the primary one gives it
//! a negative `x` on Windows and macOS.
//!
//! The backends disagree about where they measure a capture rectangle from, so
//! `CaptureRect` offers both readings of the same rectangle:
//!
//! * **absolute** (`x` / `y`) — macOS `screencapture -R`, whose origin is the
//!   main display's top-left corner;
//! * **desktop-relative** (`offset_x()` / `offset_y()`) — X11 `x11grab`, whose
//!   `:0.0+X,Y` is measured from the root window's corner, and Windows
//!   `gdigrab`, whose `-offset_x` is added to `SM_XVIRTUALSCREEN`.
//!
//! **Even dimensions are not optional.** Every backend encodes H.264 with
//! `yuv420p`, whose chroma planes are half-resolution: an odd width or height
//! either fails outright or silently costs a column of pixels. `even_dimension`
//! is the one place that rounding happens, and `record_rect_from_selection`
//! applies it after clamping so a rectangle can never leave here odd.

/// Smallest side, in pixels, we'll hand an encoder. Below this a "region" is a
/// mis-click rather than an intent, and libx264 output at 8×8 is useless to
/// whoever ends up watching it.
pub const MIN_RECORD_SIDE: u32 = 16;

/// Bounding box of every monitor, in virtual-desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DesktopBounds {
    pub x:      i32,
    pub y:      i32,
    pub width:  u32,
    pub height: u32,
    /// Device pixels per logical point for the desktop these coordinates
    /// were measured on — `1.0` unless the capture was stitched at a higher
    /// density. Backends that address the screen in points rather than
    /// pixels (macOS `screencapture -R`) divide by this.
    pub scale:  f32,
}

impl DesktopBounds {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self { x, y, width, height, scale: 1.0 }
    }

    /// Same bounds, measured on a desktop with `scale` device pixels per
    /// point. Non-finite or non-positive scales are treated as `1.0`.
    pub fn with_scale(mut self, scale: f32) -> Self {
        self.scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        self
    }

    /// One past the right-most pixel column.
    pub fn right(&self) -> i32 { self.x.saturating_add(self.width as i32) }

    /// One past the bottom-most pixel row.
    pub fn bottom(&self) -> i32 { self.y.saturating_add(self.height as i32) }
}

/// A rectangle to record, already clamped to the desktop and already sized to
/// even dimensions. Construct it through [`record_rect_from_selection`] rather
/// than by hand — the invariants above are the whole point of the type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureRect {
    /// Left edge in absolute virtual-desktop coordinates.
    pub x: i32,
    /// Top edge in absolute virtual-desktop coordinates.
    pub y: i32,
    /// Always even, always ≥ [`MIN_RECORD_SIDE`].
    pub width: u32,
    /// Always even, always ≥ [`MIN_RECORD_SIDE`].
    pub height: u32,
    /// Top-left corner of the virtual desktop, carried along so a backend that
    /// addresses its capture region relative to that corner can translate
    /// without querying the monitor layout a second time (and getting a
    /// different answer if the user re-arranged displays in between).
    pub desktop_origin: (i32, i32),
    /// Device pixels per point on the desktop this rectangle was picked on
    /// (see [`DesktopBounds::scale`]).
    pub scale: f32,
}

impl CaptureRect {
    /// The same rectangle in logical points: `(x, y, width, height)` divided
    /// by `scale` and rounded to the nearest whole point. At 1x this is the
    /// pixel rectangle unchanged. For a backend whose capture API speaks
    /// points (macOS `screencapture -R`); the pixel fields stay authoritative
    /// everywhere else.
    pub fn points(&self) -> (i32, i32, u32, u32) {
        let s = self.scale;
        (
            (self.x as f32 / s).round() as i32,
            (self.y as f32 / s).round() as i32,
            (self.width as f32 / s).round().max(1.0) as u32,
            (self.height as f32 / s).round().max(1.0) as u32,
        )
    }

    /// One past the right-most pixel column, absolute.
    pub fn right(&self) -> i32 { self.x.saturating_add(self.width as i32) }

    /// One past the bottom-most pixel row, absolute.
    pub fn bottom(&self) -> i32 { self.y.saturating_add(self.height as i32) }

    /// Left edge measured from the desktop's top-left corner — what
    /// `x11grab`'s `+X,Y` and `gdigrab`'s `-offset_x` expect.
    pub fn offset_x(&self) -> i32 { self.x.saturating_sub(self.desktop_origin.0) }

    /// Top edge measured from the desktop's top-left corner.
    pub fn offset_y(&self) -> i32 { self.y.saturating_sub(self.desktop_origin.1) }

    /// `"WxH"` — the spelling ffmpeg's `-video_size` wants.
    pub fn video_size(&self) -> String { format!("{}x{}", self.width, self.height) }

    /// Does this rectangle share any pixel with `(x, y, w, h)`?
    pub fn overlaps(&self, x: i32, y: i32, w: u32, h: u32) -> bool {
        let ox1 = x.saturating_add(w as i32);
        let oy1 = y.saturating_add(h as i32);
        x < self.right() && ox1 > self.x && y < self.bottom() && oy1 > self.y
    }
}

/// Round a pixel dimension **down** to the nearest even number.
///
/// Down rather than up: growing the rectangle would either push it past the
/// edge of the desktop or start capturing a pixel column the user didn't
/// select, and one lost column is the cheaper mistake.
pub fn even_dimension(v: u32) -> u32 { v & !1 }

/// Turn a raw overlay selection into something every backend can record.
///
/// `selection` is `(x, y, w, h)` in virtual-desktop coordinates and may carry
/// negative width/height (a right-to-left drag). The result is clamped to
/// `bounds`, rounded to even dimensions, and rejected outright — `None` — when
/// what's left is smaller than [`MIN_RECORD_SIDE`] on either axis or falls
/// entirely off the desktop.
pub fn record_rect_from_selection(
    selection: (i32, i32, i32, i32),
    bounds:    DesktopBounds,
) -> Option<CaptureRect> {
    let (sx, sy, sw, sh) = selection;
    let (mut x0, mut x1) = if sw >= 0 {
        (sx, sx.saturating_add(sw))
    } else {
        (sx.saturating_add(sw), sx)
    };
    let (mut y0, mut y1) = if sh >= 0 {
        (sy, sy.saturating_add(sh))
    } else {
        (sy.saturating_add(sh), sy)
    };

    x0 = x0.max(bounds.x);
    y0 = y0.max(bounds.y);
    x1 = x1.min(bounds.right());
    y1 = y1.min(bounds.bottom());
    if x1 <= x0 || y1 <= y0 { return None; }

    let width  = even_dimension((x1 - x0) as u32);
    let height = even_dimension((y1 - y0) as u32);
    if width < MIN_RECORD_SIDE || height < MIN_RECORD_SIDE { return None; }

    Some(CaptureRect {
        x: x0,
        y: y0,
        width,
        height,
        desktop_origin: (bounds.x, bounds.y),
        scale: bounds.scale,
    })
}

/// Where to park the floating REC indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndicatorPlacement {
    pub x: i32,
    pub y: i32,
    /// True when the panel lands entirely outside the recorded region. On a
    /// backend with no capture-exclusion API (X11) this is the difference
    /// between showing the panel and burning it into the user's video, so the
    /// caller must check it rather than assume.
    pub clear_of_region: bool,
}

/// Pick a spot for the REC indicator that keeps it out of the recorded region.
///
/// Tries, in order: below the region, above it, to its right, to its left, then
/// the desktop's top-right corner — taking the first that fits on the desktop
/// *and* misses the region. A full-desktop recording has no such spot, so the
/// top-right corner is returned with `clear_of_region: false` and the caller
/// decides whether a visible panel or an unobstructed video matters more.
///
/// `margin` is the gap left between the panel and whatever it is dodging.
pub fn indicator_placement(
    bounds: DesktopBounds,
    region: Option<CaptureRect>,
    size:   (u32, u32),
    margin: i32,
) -> IndicatorPlacement {
    let (w, h)   = size;
    let (iw, ih) = (w as i32, h as i32);

    // Desktop top-right, one margin in from both edges — where the panel has
    // always lived, and the fallback when nothing better fits.
    let corner = (
        (bounds.right() - iw - margin).max(bounds.x + margin),
        bounds.y + margin,
    );

    let Some(r) = region else {
        return IndicatorPlacement { x: corner.0, y: corner.1, clear_of_region: true };
    };

    let candidates = [
        (r.right() - iw,      r.bottom() + margin),   // below, right-aligned
        (r.right() - iw,      r.y - margin - ih),     // above, right-aligned
        (r.right() + margin,  r.y),                   // to the right
        (r.x - margin - iw,   r.y),                   // to the left
        corner,
    ];

    for (cx, cy) in candidates {
        let fits = cx >= bounds.x
            && cy >= bounds.y
            && cx.saturating_add(iw) <= bounds.right()
            && cy.saturating_add(ih) <= bounds.bottom();
        if fits && !r.overlaps(cx, cy, w, h) {
            return IndicatorPlacement { x: cx, y: cy, clear_of_region: true };
        }
    }

    IndicatorPlacement { x: corner.0, y: corner.1, clear_of_region: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop() -> DesktopBounds { DesktopBounds::new(0, 0, 1920, 1080) }

    // ── even dimensions ────────────────────────────────────────────────────

    #[test]
    fn points_divide_by_scale() {
        let bounds = DesktopBounds::new(0, 0, 2880, 1800).with_scale(2.0);
        let r = record_rect_from_selection((200, 100, 800, 600), bounds).unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (200, 100, 800, 600));
        assert_eq!(r.points(), (100, 50, 400, 300));
    }

    #[test]
    fn points_are_pixels_at_one_x() {
        let r = record_rect_from_selection((200, 100, 800, 600), desktop()).unwrap();
        assert_eq!(r.scale, 1.0);
        assert_eq!(r.points(), (200, 100, 800, 600));
    }

    #[test]
    fn with_scale_rejects_nonsense() {
        assert_eq!(DesktopBounds::new(0, 0, 10, 10).with_scale(0.0).scale, 1.0);
        assert_eq!(DesktopBounds::new(0, 0, 10, 10).with_scale(f32::NAN).scale, 1.0);
        assert_eq!(DesktopBounds::new(0, 0, 10, 10).with_scale(1.5).scale, 1.5);
    }

    #[test]
    fn even_dimension_rounds_down_and_leaves_even_alone() {
        assert_eq!(even_dimension(0), 0);
        assert_eq!(even_dimension(1), 0);
        assert_eq!(even_dimension(2), 2);
        assert_eq!(even_dimension(1919), 1918);
        assert_eq!(even_dimension(1920), 1920);
    }

    #[test]
    fn selection_dimensions_are_always_even() {
        // 401×303 is the shape of a real drag; libx264 + yuv420p can't take it.
        let r = record_rect_from_selection((10, 20, 401, 303), desktop()).unwrap();
        assert_eq!((r.width, r.height), (400, 302));
        assert_eq!((r.x, r.y), (10, 20));
        assert_eq!(r.width % 2, 0);
        assert_eq!(r.height % 2, 0);
    }

    // ── clamping ───────────────────────────────────────────────────────────

    #[test]
    fn selection_is_clamped_to_the_desktop() {
        // Dragged off the right/bottom edges: the rect stops at the edge
        // rather than asking the encoder for pixels that don't exist.
        let r = record_rect_from_selection((1800, 1000, 500, 500), desktop()).unwrap();
        assert_eq!(r.x, 1800);
        assert_eq!(r.y, 1000);
        assert_eq!(r.right(), 1920);
        assert_eq!(r.bottom(), 1080);
    }

    #[test]
    fn selection_starting_off_screen_is_pulled_back_in() {
        let r = record_rect_from_selection((-200, -100, 600, 400), desktop()).unwrap();
        assert_eq!((r.x, r.y), (0, 0));
        assert_eq!((r.width, r.height), (400, 300));
    }

    #[test]
    fn negative_drag_is_normalized() {
        // Dragged up-and-left from (500, 400).
        let r = record_rect_from_selection((500, 400, -300, -200), desktop()).unwrap();
        assert_eq!((r.x, r.y), (200, 200));
        assert_eq!((r.width, r.height), (300, 200));
    }

    #[test]
    fn selection_entirely_off_desktop_is_rejected() {
        assert!(record_rect_from_selection((3000, 3000, 100, 100), desktop()).is_none());
    }

    #[test]
    fn tiny_selection_is_rejected() {
        // A click with a twitch, not a region.
        assert!(record_rect_from_selection((10, 10, 6, 6), desktop()).is_none());
        // Just under the limit on one axis only still fails — an encoder
        // handed a 200×14 strip produces something nobody can watch.
        assert!(record_rect_from_selection((10, 10, 200, 14), desktop()).is_none());
    }

    #[test]
    fn odd_rounding_cannot_sneak_a_rect_under_the_minimum() {
        // 17 rounds down to 16, which is exactly the floor and stays.
        let r = record_rect_from_selection((0, 0, 17, 17), desktop()).unwrap();
        assert_eq!((r.width, r.height), (16, 16));
        // 15 has nowhere to go.
        assert!(record_rect_from_selection((0, 0, 15, 200), desktop()).is_none());
    }

    // ── coordinate conventions ─────────────────────────────────────────────

    #[test]
    fn offsets_are_relative_to_a_negative_desktop_origin() {
        // A second monitor to the left of the primary one puts the virtual
        // desktop's corner at a negative x — the case that separates
        // gdigrab's -offset_x from screencapture's -R.
        let bounds = DesktopBounds::new(-1920, 0, 3840, 1080);
        let r = record_rect_from_selection((-1000, 100, 640, 480), bounds).unwrap();
        assert_eq!((r.x, r.y), (-1000, 100), "absolute coords stay absolute");
        assert_eq!(r.offset_x(), 920, "offset is measured from the desktop corner");
        assert_eq!(r.offset_y(), 100);
        assert_eq!(r.video_size(), "640x480");
    }

    #[test]
    fn offsets_equal_absolute_coords_when_the_desktop_starts_at_origin() {
        let r = record_rect_from_selection((320, 240, 800, 600), desktop()).unwrap();
        assert_eq!(r.offset_x(), r.x);
        assert_eq!(r.offset_y(), r.y);
    }

    // ── overlap ────────────────────────────────────────────────────────────

    #[test]
    fn overlap_is_exclusive_at_the_edges() {
        let r = record_rect_from_selection((100, 100, 200, 200), desktop()).unwrap();
        assert!(r.overlaps(150, 150, 10, 10), "inside");
        assert!(r.overlaps(90, 90, 20, 20), "corner bite");
        assert!(!r.overlaps(300, 100, 50, 50), "flush against the right edge");
        assert!(!r.overlaps(100, 300, 50, 50), "flush against the bottom edge");
        assert!(!r.overlaps(0, 0, 100, 100), "flush against the top-left corner");
    }

    // ── indicator placement ────────────────────────────────────────────────

    const PANEL: (u32, u32) = (220, 56);

    #[test]
    fn indicator_without_a_region_sits_in_the_desktop_corner() {
        let p = indicator_placement(desktop(), None, PANEL, 24);
        assert_eq!((p.x, p.y), (1920 - 220 - 24, 24));
        assert!(p.clear_of_region);
    }

    #[test]
    fn indicator_drops_below_a_region_with_room_under_it() {
        let r = record_rect_from_selection((200, 100, 800, 600), desktop()).unwrap();
        let p = indicator_placement(desktop(), Some(r), PANEL, 24);
        assert!(p.clear_of_region);
        assert!(!r.overlaps(p.x, p.y, PANEL.0, PANEL.1));
        assert_eq!(p.y, r.bottom() + 24);
    }

    #[test]
    fn indicator_moves_above_a_region_that_reaches_the_bottom_edge() {
        let r = record_rect_from_selection((200, 300, 800, 780), desktop()).unwrap();
        assert_eq!(r.bottom(), 1080);
        let p = indicator_placement(desktop(), Some(r), PANEL, 24);
        assert!(p.clear_of_region);
        assert!(!r.overlaps(p.x, p.y, PANEL.0, PANEL.1));
        assert!(p.y + PANEL.1 as i32 <= r.y);
    }

    #[test]
    fn indicator_goes_beside_a_full_height_region() {
        // Nothing above or below; there's room to the right.
        let r = record_rect_from_selection((0, 0, 1000, 1080), desktop()).unwrap();
        let p = indicator_placement(desktop(), Some(r), PANEL, 24);
        assert!(p.clear_of_region);
        assert!(p.x >= r.right());
    }

    #[test]
    fn indicator_reports_no_clear_spot_for_a_full_desktop_region() {
        let r = record_rect_from_selection((0, 0, 1920, 1080), desktop()).unwrap();
        let p = indicator_placement(desktop(), Some(r), PANEL, 24);
        assert!(!p.clear_of_region, "nowhere on this desktop is outside the region");
        // Still a usable on-screen position — the caller decides whether to
        // show the panel at all.
        assert!(p.x >= 0 && p.y >= 0);
    }

    #[test]
    fn indicator_stays_on_a_desktop_whose_origin_is_negative() {
        let bounds = DesktopBounds::new(-1920, -200, 3840, 1280);
        let r = record_rect_from_selection((-1800, -100, 600, 400), bounds).unwrap();
        let p = indicator_placement(bounds, Some(r), PANEL, 24);
        assert!(p.x >= bounds.x && p.y >= bounds.y);
        assert!(p.x + PANEL.0 as i32 <= bounds.right());
        assert!(p.y + PANEL.1 as i32 <= bounds.bottom());
    }
}
