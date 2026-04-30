//! Overlay editor — region selection (PLAN.md § R7).
//!
//! Borderless fullscreen window that composites:
//!
//!   1. blit the captured screenshot at native resolution
//!   2. paint a 45 %-opaque dark dim over the whole surface
//!   3. "punch a hole" by re-blitting the screenshot (full brightness)
//!      inside the active selection rectangle
//!   4. draw a 1-pixel selection border + 8 corner/edge handles
//!   5. on Enter / right-click: crop and return the region
//!   6. on Esc: clear the selection if there is one, else cancel
//!
//! The window must share the tray's `EventLoop` (winit forbids two), so
//! this exposes an `Overlay` struct rather than running its own event
//! loop. `tray_loop` opens the window inside a `&ActiveEventLoop`, then
//! routes `WindowEvent`s into `Overlay::handle_event` until it returns
//! `Cancelled` or `Accepted(image)`.
//!
//! Stack: winit (window + events) + softbuffer (CPU framebuffer; no GPU).
//!
//! Not here yet — annotation tools (pen / arrow / text / pixelate / step
//! / line / rect / ellipse / marker), undo / redo, save / copy / pin
//! choice, magnifier zoom, edge-resize after selection. Those layer on
//! top of this module.

use std::num::NonZeroU32;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use image::{ImageBuffer, Rgba};
use softbuffer::{Context, Surface};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Fullscreen, Window, WindowAttributes, WindowId};

/// What the overlay window should do next after handling an event.
pub enum OverlayOutcome {
    /// Keep the overlay alive — more events expected.
    Continue,
    /// User cancelled (Esc, window close). Caller should drop the Overlay.
    Cancelled,
    /// User accepted a region. Caller should drop the Overlay and persist
    /// the cropped bitmap.
    Accepted(ImageBuffer<Rgba<u8>, Vec<u8>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Cursor visible, no selection in progress.
    Idle,
    /// Mouse-button down; user is dragging out the rectangle.
    Selecting,
    /// Selection committed (mouse released after a drag). Enter / right-
    /// click accepts; Esc clears it; a new mouse-down starts a fresh drag.
    Selected,
}

pub struct Overlay {
    screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
    window:     Rc<Window>,
    _ctx:       Context<Rc<Window>>,
    surface:    Surface<Rc<Window>, Rc<Window>>,
    state:      State,
    cursor:     (i32, i32),
    anchor:     (i32, i32),
    /// (x, y, w, h) in window-pixel coordinates, normalized so w/h are non-negative.
    selection:  Option<(i32, i32, i32, i32)>,
}

impl Overlay {
    /// Open the fullscreen overlay window for the given screenshot. The
    /// returned `Overlay` borrows nothing from `loop_target`; the caller
    /// retains it and feeds in window events until the outcome resolves.
    pub fn new(
        loop_target: &ActiveEventLoop,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
    ) -> Result<Self> {
        let attrs = WindowAttributes::default()
            .with_title("Kashot")
            .with_decorations(false)
            .with_resizable(false)
            .with_fullscreen(Some(Fullscreen::Borderless(None)));

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window: {e}"))?;

        // Crosshair while no region is locked in — same convention the
        // C# OverlayForm uses.
        window.set_cursor(CursorIcon::Crosshair);
        window.focus_window();

        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new: {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new: {e}"))?;

        Ok(Overlay {
            screenshot,
            window,
            _ctx:    ctx,
            surface,
            state:   State::Idle,
            cursor:  (0, 0),
            anchor:  (0, 0),
            selection: None,
        })
    }

    /// The winit `WindowId` of the overlay window. Used by the tray loop to
    /// route `WindowEvent`s — only events for this id should be dispatched
    /// into `handle_event`.
    pub fn window_id(&self) -> WindowId { self.window.id() }

    /// Process a single winit `WindowEvent` for the overlay window. Returns
    /// `Continue` while the user is still interacting. `Cancelled` when
    /// they press Esc on an empty selection or close the window. `Accepted`
    /// with the cropped bitmap when they press Enter / right-click on a
    /// committed selection.
    pub fn handle_event(&mut self, event: WindowEvent) -> OverlayOutcome {
        match event {
            WindowEvent::CloseRequested => OverlayOutcome::Cancelled,

            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, ..
            } => match logical_key {
                Key::Named(NamedKey::Escape) => {
                    if self.state == State::Selected {
                        // Clear the selection; second Esc closes the overlay.
                        self.state = State::Idle;
                        self.selection = None;
                        self.window.request_redraw();
                        OverlayOutcome::Continue
                    } else {
                        OverlayOutcome::Cancelled
                    }
                }
                Key::Named(NamedKey::Enter) => self.commit(),
                _ => OverlayOutcome::Continue,
            },

            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                self.cursor = (x as i32, y as i32);
                if self.state == State::Selecting {
                    self.selection = Some(rect_from(self.anchor, self.cursor));
                    self.window.request_redraw();
                }
                OverlayOutcome::Continue
            }

            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.state     = State::Selecting;
                self.anchor    = self.cursor;
                self.selection = Some((self.cursor.0, self.cursor.1, 0, 0));
                self.window.request_redraw();
                OverlayOutcome::Continue
            }

            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.state == State::Selecting {
                    let r = rect_from(self.anchor, self.cursor);
                    if r.2 < 4 || r.3 < 4 {
                        // Treat tiny drags as "no selection".
                        self.state     = State::Idle;
                        self.selection = None;
                    } else {
                        self.selection = Some(r);
                        self.state     = State::Selected;
                    }
                    self.window.request_redraw();
                }
                OverlayOutcome::Continue
            }

            // Right-click commits the current selection (matches the
            // OverlayForm gesture for "save this region right now").
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                self.commit()
            }

            WindowEvent::Resized(_) => {
                self.window.request_redraw();
                OverlayOutcome::Continue
            }

            WindowEvent::RedrawRequested => {
                self.redraw();
                OverlayOutcome::Continue
            }

            _ => OverlayOutcome::Continue,
        }
    }

    fn commit(&mut self) -> OverlayOutcome {
        if self.state == State::Selected {
            if let Some(rect) = self.selection {
                return OverlayOutcome::Accepted(crop(&self.screenshot, rect));
            }
        }
        OverlayOutcome::Continue
    }

    fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) else { return; };
        if let Err(e) = self.surface.resize(w, h) {
            eprintln!("overlay: surface.resize: {e}"); return;
        }
        let mut buf = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("overlay: buffer_mut: {e}"); return; }
        };

        let win_w  = w.get() as usize;
        let win_h  = h.get() as usize;
        let shot_w = self.screenshot.width()  as usize;
        let shot_h = self.screenshot.height() as usize;
        let shot   = self.screenshot.as_raw();

        // 45 % darkening factor for the dim outside the selection.
        let dim_num: u32   = 55;
        let dim_denom: u32 = 100;

        // Selection rect in (x0, y0, x1, y1) inclusive-exclusive form.
        let sel_rect = self.selection.map(|(x, y, w, h)| (x, y, x + w, y + h));

        for y in 0..win_h {
            for x in 0..win_w {
                let dst_idx = y * win_w + x;
                let (r, g, b) = if x < shot_w && y < shot_h {
                    let src = (y * shot_w + x) * 4;
                    (shot[src] as u32, shot[src + 1] as u32, shot[src + 2] as u32)
                } else {
                    (0, 0, 0)
                };

                let inside = if let Some((x0, y0, x1, y1)) = sel_rect {
                    (x as i32) >= x0 && (x as i32) < x1 && (y as i32) >= y0 && (y as i32) < y1
                } else {
                    false
                };

                let (rr, gg, bb) = if inside {
                    (r, g, b)
                } else {
                    (r * dim_num / dim_denom, g * dim_num / dim_denom, b * dim_num / dim_denom)
                };
                buf[dst_idx] = (rr << 16) | (gg << 8) | bb;
            }
        }

        // 1-px selection border + 8 handles (cornflower blue, matches the
        // C# overlay's rendering).
        if let Some((x0, y0, x1, y1)) = sel_rect {
            const BLUE:  u32 = 0x00_64_95_ED;
            const WHITE: u32 = 0x00_FF_FF_FF;
            draw_rect_border(&mut buf, win_w, win_h, x0, y0, x1, y1, BLUE);

            let xm = (x0 + x1) / 2;
            let ym = (y0 + y1) / 2;
            for &(hx, hy) in &[
                (x0, y0), (xm, y0), (x1.saturating_sub(1), y0),
                (x0, ym),                     (x1.saturating_sub(1), ym),
                (x0, y1.saturating_sub(1)), (xm, y1.saturating_sub(1)), (x1.saturating_sub(1), y1.saturating_sub(1)),
            ] {
                draw_filled_rect(&mut buf, win_w, win_h, hx - 3, hy - 3, hx + 3, hy + 3, WHITE);
                draw_rect_border(&mut buf, win_w, win_h, hx - 3, hy - 3, hx + 3, hy + 3, BLUE);
            }
        }

        if let Err(e) = buf.present() {
            eprintln!("overlay: buf.present: {e}");
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────

fn rect_from(a: (i32, i32), b: (i32, i32)) -> (i32, i32, i32, i32) {
    let x = a.0.min(b.0);
    let y = a.1.min(b.1);
    let w = (a.0 - b.0).abs();
    let h = (a.1 - b.1).abs();
    (x, y, w, h)
}

fn crop(
    src: &ImageBuffer<Rgba<u8>, Vec<u8>>,
    (x, y, w, h): (i32, i32, i32, i32),
) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let img_w = src.width()  as i32;
    let img_h = src.height() as i32;
    let x0 = x.max(0).min(img_w);
    let y0 = y.max(0).min(img_h);
    let x1 = (x + w).max(0).min(img_w);
    let y1 = (y + h).max(0).min(img_h);
    let cw = (x1 - x0).max(1) as u32;
    let ch = (y1 - y0).max(1) as u32;
    let mut out = ImageBuffer::<Rgba<u8>, Vec<u8>>::new(cw, ch);
    for j in 0..ch {
        for i in 0..cw {
            out.put_pixel(i, j, *src.get_pixel(x0 as u32 + i, y0 as u32 + j));
        }
    }
    out
}

fn draw_rect_border(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32,
) {
    let xa = x0.max(0) as usize;
    let xb = (x1.min(stride as i32) as usize).max(xa);
    let ya = y0.max(0) as usize;
    let yb = (y1.min(height as i32) as usize).max(ya);
    if xa >= stride || ya >= height || xa == xb || ya == yb { return; }
    for x in xa..xb.min(stride) {
        buf[ya * stride + x] = rgb;
        let by = (yb - 1).min(height - 1);
        buf[by * stride + x] = rgb;
    }
    for y in ya..yb.min(height) {
        buf[y * stride + xa] = rgb;
        let bx = (xb - 1).min(stride - 1);
        buf[y * stride + bx] = rgb;
    }
}

fn draw_filled_rect(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32,
) {
    let xa = x0.max(0) as usize;
    let xb = (x1.min(stride as i32) as usize).max(xa);
    let ya = y0.max(0) as usize;
    let yb = (y1.min(height as i32) as usize).max(ya);
    for y in ya..yb.min(height) {
        for x in xa..xb.min(stride) {
            buf[y * stride + x] = rgb;
        }
    }
}
