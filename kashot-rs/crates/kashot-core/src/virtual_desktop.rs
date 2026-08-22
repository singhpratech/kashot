//! Virtual-desktop geometry: the union of every monitor, and the coordinate
//! mappings between the overlay's framebuffer and virtual-screen space.
//!
//! Three coordinate spaces are in play once a second monitor exists, and
//! mixing them up is what makes a region selected on a non-primary monitor
//! crop the wrong pixels:
//!
//! | Space | Origin | Used by |
//! |---|---|---|
//! | virtual-screen | wherever the OS puts it — `(0, 0)` is the *primary* monitor's top-left, so a monitor placed left of / above it has **negative** coordinates | monitor rects, window positions (winit), the pin window |
//! | bitmap | top-left of the stitched capture = the union's top-left | `capture_all_screens`'s bitmap, the crop rect |
//! | frame | top-left of the overlay window's client area | mouse events, annotations, every painter call |
//!
//! When the overlay covers the whole virtual desktop, frame == bitmap and
//! both differ from virtual-screen space by the union origin. The overlay
//! re-derives its frame origin from the window itself each redraw, so a
//! window manager that refuses the requested geometry shifts the mapping
//! instead of silently capturing the wrong region.
//!
//! Everything here is unit-agnostic: feed it logical or physical pixels
//! consistently and it stays correct. Scale-factor conversion is a separate
//! concern and composes on top of these functions rather than inside them.

/// One monitor's rectangle in virtual-screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl MonitorRect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    /// Exclusive right edge.
    pub const fn right(&self) -> i32 { self.x + self.width as i32 }

    /// Exclusive bottom edge.
    pub const fn bottom(&self) -> i32 { self.y + self.height as i32 }

    pub const fn contains(&self, (px, py): (i32, i32)) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
}

/// Bounding box of every monitor, in virtual-screen coordinates, plus the
/// monitors themselves. `None` when the list is empty — a machine with no
/// monitors has no desktop to describe.
pub fn union_bounds(monitors: &[MonitorRect]) -> Option<((i32, i32), (u32, u32))> {
    let first = monitors.first()?;
    let mut min_x = first.x;
    let mut min_y = first.y;
    let mut max_x = first.right();
    let mut max_y = first.bottom();
    for m in &monitors[1..] {
        min_x = min_x.min(m.x);
        min_y = min_y.min(m.y);
        max_x = max_x.max(m.right());
        max_y = max_y.max(m.bottom());
    }
    Some((
        (min_x, min_y),
        ((max_x - min_x).max(0) as u32, (max_y - min_y).max(0) as u32),
    ))
}

/// The virtual desktop: where the stitched capture starts in virtual-screen
/// space, how big it is, and which monitors make it up.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DesktopGeometry {
    origin: (i32, i32),
    size: (u32, u32),
    monitors: Vec<MonitorRect>,
}

impl DesktopGeometry {
    /// Build from a monitor list. `None` when the list is empty.
    pub fn from_monitors(monitors: Vec<MonitorRect>) -> Option<Self> {
        let (origin, size) = union_bounds(&monitors)?;
        Some(Self { origin, size, monitors })
    }

    /// A bitmap that is not a screen capture (a video frame, say): rooted at
    /// `(0, 0)` with no monitor list, so chrome lays out against the whole
    /// framebuffer instead of against a monitor.
    pub fn bitmap(width: u32, height: u32) -> Self {
        Self { origin: (0, 0), size: (width, height), monitors: Vec::new() }
    }

    /// Virtual-screen coordinates of the bitmap's top-left pixel.
    pub fn origin(&self) -> (i32, i32) { self.origin }

    pub fn size(&self) -> (u32, u32) { self.size }

    pub fn monitors(&self) -> &[MonitorRect] { &self.monitors }

    /// True when a window that merely fills the primary monitor would miss
    /// part of the desktop — more than one monitor, or a union that starts
    /// somewhere other than the origin. That is exactly the case where the
    /// overlay has to be placed by hand instead of asking for fullscreen.
    pub fn spans_multiple_monitors(&self) -> bool {
        self.monitors.len() > 1 || self.origin != (0, 0)
    }
}

/// Framebuffer point -> virtual-screen point.
pub fn frame_to_virtual(frame_origin: (i32, i32), (x, y): (i32, i32)) -> (i32, i32) {
    (frame_origin.0 + x, frame_origin.1 + y)
}

/// Virtual-screen point -> framebuffer point.
pub fn virtual_to_frame(frame_origin: (i32, i32), (x, y): (i32, i32)) -> (i32, i32) {
    (x - frame_origin.0, y - frame_origin.1)
}

/// Framebuffer rect -> virtual-screen rect. Size is unchanged; only the
/// origin moves.
pub fn frame_rect_to_virtual(
    frame_origin: (i32, i32),
    (x, y, w, h): (i32, i32, i32, i32),
) -> (i32, i32, i32, i32) {
    let (vx, vy) = frame_to_virtual(frame_origin, (x, y));
    (vx, vy, w, h)
}

/// Framebuffer point -> pixel coordinates in the stitched capture.
///
/// `bitmap_origin` is the capture's virtual-screen origin (the union's
/// top-left, negative whenever a monitor sits left of / above the primary
/// one); `frame_origin` is the overlay window's own virtual-screen origin.
/// They are equal when the overlay covers the whole desktop as asked, and
/// the difference is exactly the correction a partly-placed overlay needs.
pub fn frame_to_bitmap(
    bitmap_origin: (i32, i32),
    frame_origin: (i32, i32),
    p: (i32, i32),
) -> (i32, i32) {
    virtual_to_frame(bitmap_origin, frame_to_virtual(frame_origin, p))
}

/// Framebuffer rect -> rect in the stitched capture. This is the mapping
/// the crop goes through: a selection dragged on a monitor left of the
/// primary one has negative virtual coordinates but lands at a positive
/// bitmap offset.
pub fn frame_rect_to_bitmap(
    bitmap_origin: (i32, i32),
    frame_origin: (i32, i32),
    (x, y, w, h): (i32, i32, i32, i32),
) -> (i32, i32, i32, i32) {
    let (bx, by) = frame_to_bitmap(bitmap_origin, frame_origin, (x, y));
    (bx, by, w, h)
}

/// Offset, in bitmap pixels, of the framebuffer's top-left corner inside
/// the capture. Sampling helper for blits that walk the framebuffer.
pub fn bitmap_offset(bitmap_origin: (i32, i32), frame_origin: (i32, i32)) -> (i32, i32) {
    frame_to_bitmap(bitmap_origin, frame_origin, (0, 0))
}

/// Intersection of two rects given as `(x, y, w, h)`. `None` when they
/// don't overlap or either is empty.
pub fn intersect_rect(
    a: (i32, i32, i32, i32),
    b: (i32, i32, i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    let x1 = (a.0 + a.2).min(b.0 + b.2);
    let y1 = (a.1 + a.3).min(b.1 + b.3);
    if x1 > x0 && y1 > y0 { Some((x0, y0, x1 - x0, y1 - y0)) } else { None }
}

/// Framebuffer-space rect the floating chrome (tool panel, action panel,
/// magnifier, hint chip) should lay itself out inside: the monitor holding
/// `point`, clipped to the framebuffer.
///
/// Falls back to the whole framebuffer when no monitor claims the point —
/// which is also what an empty monitor list gives, so a non-capture bitmap
/// keeps the single-surface behaviour.
pub fn monitor_bounds_in_frame(
    monitors: &[MonitorRect],
    frame_origin: (i32, i32),
    frame_size: (u32, u32),
    point: (i32, i32),
) -> (i32, i32, i32, i32) {
    let whole = (0, 0, frame_size.0 as i32, frame_size.1 as i32);
    let virt = frame_to_virtual(frame_origin, point);
    let Some(m) = monitors.iter().find(|m| m.contains(virt)) else { return whole };
    let (mx, my) = virtual_to_frame(frame_origin, (m.x, m.y));
    intersect_rect((mx, my, m.width as i32, m.height as i32), whole).unwrap_or(whole)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Second monitor to the LEFT of the primary one: the union starts at a
    /// negative x, which is the layout that made region capture pick the
    /// wrong pixels.
    fn left_of_primary() -> DesktopGeometry {
        DesktopGeometry::from_monitors(vec![
            MonitorRect::new(0, 0, 1920, 1080),
            MonitorRect::new(-1600, -200, 1600, 900),
        ])
        .unwrap()
    }

    #[test]
    fn union_of_one_monitor_is_that_monitor() {
        let g = DesktopGeometry::from_monitors(vec![MonitorRect::new(0, 0, 1920, 1080)]).unwrap();
        assert_eq!(g.origin(), (0, 0));
        assert_eq!(g.size(), (1920, 1080));
        assert!(!g.spans_multiple_monitors());
    }

    #[test]
    fn union_spans_negative_origins() {
        let g = left_of_primary();
        assert_eq!(g.origin(), (-1600, -200));
        assert_eq!(g.size(), (3520, 1280));
        assert!(g.spans_multiple_monitors());
    }

    #[test]
    fn union_of_no_monitors_is_none() {
        assert!(DesktopGeometry::from_monitors(Vec::new()).is_none());
        assert!(union_bounds(&[]).is_none());
    }

    #[test]
    fn bitmap_geometry_has_no_monitors() {
        let g = DesktopGeometry::bitmap(1280, 720);
        assert_eq!(g.origin(), (0, 0));
        assert_eq!(g.size(), (1280, 720));
        assert!(g.monitors().is_empty());
        assert!(!g.spans_multiple_monitors());
    }

    #[test]
    fn frame_and_virtual_round_trip() {
        let origin = (-1600, -200);
        for p in [(0, 0), (10, 20), (3519, 1279)] {
            assert_eq!(virtual_to_frame(origin, frame_to_virtual(origin, p)), p);
        }
        assert_eq!(frame_to_virtual(origin, (0, 0)), (-1600, -200));
        assert_eq!(frame_to_virtual(origin, (1600, 200)), (0, 0));
    }

    /// The whole point of the fix: a selection on the left-hand monitor maps
    /// to a positive offset in the stitched bitmap, and to a negative
    /// virtual-screen position for the pin window.
    #[test]
    fn selection_on_left_monitor_maps_into_the_bitmap() {
        let g = left_of_primary();
        let origin = g.origin();
        // Overlay covers the whole desktop, so frame origin == bitmap origin.
        let sel = (100, 50, 400, 300);
        assert_eq!(frame_rect_to_bitmap(origin, origin, sel), (100, 50, 400, 300));
        assert_eq!(frame_rect_to_virtual(origin, sel), (-1500, -150, 400, 300));
    }

    /// Same selection, but the window manager refused the placement and put
    /// the overlay at (0, 0). The crop has to shift by the difference or it
    /// grabs pixels from the wrong monitor.
    #[test]
    fn misplaced_overlay_shifts_the_crop() {
        let bitmap_origin = (-1600, -200);
        let frame_origin = (0, 0);
        assert_eq!(bitmap_offset(bitmap_origin, frame_origin), (1600, 200));
        assert_eq!(
            frame_rect_to_bitmap(bitmap_origin, frame_origin, (100, 50, 400, 300)),
            (1700, 250, 400, 300)
        );
        assert_eq!(frame_rect_to_virtual(frame_origin, (100, 50, 400, 300)), (100, 50, 400, 300));
    }

    #[test]
    fn aligned_overlay_has_no_bitmap_offset() {
        assert_eq!(bitmap_offset((-1600, -200), (-1600, -200)), (0, 0));
        assert_eq!(bitmap_offset((0, 0), (0, 0)), (0, 0));
    }

    #[test]
    fn intersect_rect_clips_and_rejects() {
        assert_eq!(intersect_rect((0, 0, 100, 100), (50, 50, 100, 100)), Some((50, 50, 50, 50)));
        assert_eq!(intersect_rect((0, 0, 100, 100), (100, 0, 10, 10)), None);
        assert_eq!(intersect_rect((0, 0, 0, 100), (0, 0, 10, 10)), None);
        assert_eq!(intersect_rect((-40, -40, 80, 80), (0, 0, 10, 10)), Some((0, 0, 10, 10)));
    }

    /// Chrome anchored to a selection on the left-hand monitor must lay out
    /// inside that monitor's slice of the framebuffer, not the whole
    /// 3520-px-wide desktop — otherwise the tool panel lands on the monitor
    /// next door.
    #[test]
    fn chrome_bounds_follow_the_monitor_under_the_point() {
        let g = left_of_primary();
        let origin = g.origin();
        let size = g.size();
        // Point on the secondary (left/top) monitor.
        assert_eq!(
            monitor_bounds_in_frame(g.monitors(), origin, size, (10, 10)),
            (0, 0, 1600, 900)
        );
        // Point on the primary monitor: its frame-space origin is the
        // negated union origin.
        assert_eq!(
            monitor_bounds_in_frame(g.monitors(), origin, size, (1700, 300)),
            (1600, 200, 1920, 1080)
        );
    }

    #[test]
    fn chrome_bounds_fall_back_to_the_frame() {
        let g = left_of_primary();
        // Dead zone: the union covers it but no monitor does.
        let dead = (10, 1200);
        assert_eq!(
            monitor_bounds_in_frame(g.monitors(), g.origin(), g.size(), dead),
            (0, 0, 3520, 1280)
        );
        // No monitor list at all (a video frame).
        assert_eq!(
            monitor_bounds_in_frame(&[], (0, 0), (1280, 720), (5, 5)),
            (0, 0, 1280, 720)
        );
    }

    /// A monitor that hangs off the framebuffer (window smaller than the
    /// desktop) is clipped to what is actually drawable.
    #[test]
    fn chrome_bounds_clip_to_the_framebuffer() {
        let monitors = [MonitorRect::new(0, 0, 1920, 1080)];
        assert_eq!(
            monitor_bounds_in_frame(&monitors, (0, 0), (1280, 720), (5, 5)),
            (0, 0, 1280, 720)
        );
    }
}
