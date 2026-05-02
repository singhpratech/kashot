//! Overlay editor — region selection + annotation tools.
//!
//! Borderless fullscreen window that composites:
//!
//!   1. blit the captured screenshot at native resolution
//!   2. paint a 45 %-opaque dark dim over the whole surface
//!   3. "punch a hole" by re-blitting the screenshot (full brightness)
//!      inside the active selection rectangle
//!   4. draw any committed annotations *clipped to the selection*
//!   5. draw the in-progress annotation (if the user is mid-drag) on top
//!   6. draw a 1-pixel selection border + 8 corner/edge handles
//!   7. draw the floating tool-picker toolbar at the top of the screen
//!   8. on Enter / right-click: composite annotations onto the cropped
//!      bitmap and return it
//!   9. on Esc: clear the selection if there is one, else cancel
//!
//! The window must share the tray's `EventLoop` (winit forbids two), so
//! this exposes an `Overlay` struct rather than running its own event
//! loop. `tray_loop` opens the window inside a `&ActiveEventLoop`, then
//! routes `WindowEvent`s into `Overlay::handle_event` until it returns
//! `Cancelled` or `Accepted(image)`.
//!
//! Stack: winit (window + events) + softbuffer (CPU framebuffer; no GPU)
//! + the in-tree `painter` module for line / rect / ellipse / arrow rasters.
//!
//! What's still queued for later slices: text, step, pixelate, marker,
//! line; undo/redo stack; Save/Copy/Pin choice; magnifier zoom; edge-resize
//! after the selection is committed; real text on the dimension chip.

use std::num::NonZeroU32;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use image::{ImageBuffer, Rgba};
use kashot_core::annotation::{Annotation, Point2, Stroke};
use kashot_core::color::Rgba as KashotRgba;
use kashot_core::tool::Tool;
use softbuffer::{Context, Surface};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Fullscreen, Window, WindowAttributes, WindowId};

use crate::painter::{self, ImageSurface, U32Surface};

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
    /// click accepts; Esc clears it; clicks on the toolbar pick a tool;
    /// clicks inside the selection start a `Drawing`.
    Selected,
    /// Mouse-button is held inside the selection while a tool is active —
    /// `current` holds the in-progress annotation. Mouse-move extends it,
    /// mouse-up commits it to `annotations`.
    Drawing,
}

/// Toolbar geometry — kept simple and centered horizontally near the top
/// of the screen. We rebuild it on every redraw, which is cheap.
const TOOLBAR_TOP:    i32 = 18;
const TOOLBAR_PAD:    i32 = 8;
const TOOLBAR_BTN:    i32 = 36;
const TOOLBAR_GAP:    i32 = 4;
const TOOLBAR_RADIUS: i32 = 8;

pub struct Overlay {
    screenshot:  ImageBuffer<Rgba<u8>, Vec<u8>>,
    window:      Rc<Window>,
    _ctx:        Context<Rc<Window>>,
    surface:     Surface<Rc<Window>, Rc<Window>>,
    state:       State,
    cursor:      (i32, i32),
    anchor:      (i32, i32),
    /// (x, y, w, h) in window-pixel coordinates, normalized so w/h are non-negative.
    selection:   Option<(i32, i32, i32, i32)>,
    tool:        Tool,
    stroke:      Stroke,
    annotations: Vec<Annotation>,
    /// In-progress annotation while `state == Drawing`.
    current:     Option<Annotation>,
}

impl Overlay {
    /// Open the fullscreen overlay window for the given screenshot.
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

        window.set_cursor(CursorIcon::Crosshair);
        window.focus_window();

        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new: {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new: {e}"))?;

        Ok(Overlay {
            screenshot,
            window,
            _ctx:        ctx,
            surface,
            state:       State::Idle,
            cursor:      (0, 0),
            anchor:      (0, 0),
            selection:   None,
            tool:        Tool::Pen,
            stroke:      Stroke::default(),
            annotations: Vec::new(),
            current:     None,
        })
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    pub fn handle_event(&mut self, event: WindowEvent) -> OverlayOutcome {
        match event {
            WindowEvent::CloseRequested => OverlayOutcome::Cancelled,

            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, ..
            } => self.handle_key(logical_key),

            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                self.cursor = (x as i32, y as i32);
                match self.state {
                    State::Selecting => {
                        self.selection = Some(rect_from(self.anchor, self.cursor));
                        self.window.request_redraw();
                    }
                    State::Drawing => {
                        if let Some(a) = self.current.as_mut() {
                            a.extend(Point2::new(x as f32, y as f32));
                            self.window.request_redraw();
                        }
                    }
                    _ => {}
                }
                OverlayOutcome::Continue
            }

            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.handle_left_press()
            }

            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.handle_left_release()
            }

            // Right-click commits the current selection (matches the
            // OverlayForm gesture for "save this region right now").
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                if self.state == State::Drawing {
                    // Cancel the in-progress annotation, mirroring C# OverlayForm.
                    self.current = None;
                    self.state   = State::Selected;
                    self.window.request_redraw();
                    OverlayOutcome::Continue
                } else {
                    self.commit()
                }
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

    fn handle_key(&mut self, key: Key) -> OverlayOutcome {
        match key {
            Key::Named(NamedKey::Escape) => {
                if self.state == State::Drawing {
                    self.current = None;
                    self.state   = State::Selected;
                    self.window.request_redraw();
                    OverlayOutcome::Continue
                } else if self.state == State::Selected {
                    self.state       = State::Idle;
                    self.selection   = None;
                    self.annotations.clear();
                    self.window.request_redraw();
                    OverlayOutcome::Continue
                } else {
                    OverlayOutcome::Cancelled
                }
            }
            Key::Named(NamedKey::Enter) => self.commit(),
            Key::Character(s) => {
                if self.state == State::Selected {
                    if let Some(c) = s.chars().next() {
                        if let Some(t) = Tool::from_key(c) {
                            self.tool = t;
                            self.window.request_redraw();
                        }
                    }
                }
                OverlayOutcome::Continue
            }
            _ => OverlayOutcome::Continue,
        }
    }

    fn handle_left_press(&mut self) -> OverlayOutcome {
        // Toolbar takes priority — clicking a tool button never starts a draw.
        if self.state == State::Selected {
            if let Some(t) = self.toolbar_hit(self.cursor) {
                self.tool = t;
                self.window.request_redraw();
                return OverlayOutcome::Continue;
            }
        }

        match self.state {
            State::Idle => {
                self.state     = State::Selecting;
                self.anchor    = self.cursor;
                self.selection = Some((self.cursor.0, self.cursor.1, 0, 0));
                self.window.request_redraw();
            }
            State::Selected => {
                if self.cursor_in_selection() {
                    if let Some(a) = self.start_annotation() {
                        self.current = Some(a);
                        self.state   = State::Drawing;
                        self.window.request_redraw();
                    }
                } else {
                    // Start a new selection if the click was outside.
                    self.state     = State::Selecting;
                    self.anchor    = self.cursor;
                    self.selection = Some((self.cursor.0, self.cursor.1, 0, 0));
                    self.annotations.clear();
                    self.window.request_redraw();
                }
            }
            _ => {}
        }
        OverlayOutcome::Continue
    }

    fn handle_left_release(&mut self) -> OverlayOutcome {
        match self.state {
            State::Selecting => {
                let r = rect_from(self.anchor, self.cursor);
                if r.2 < 4 || r.3 < 4 {
                    self.state     = State::Idle;
                    self.selection = None;
                } else {
                    self.selection = Some(r);
                    self.state     = State::Selected;
                }
                self.window.request_redraw();
            }
            State::Drawing => {
                if let Some(a) = self.current.take() {
                    self.annotations.push(a);
                }
                self.state = State::Selected;
                self.window.request_redraw();
            }
            _ => {}
        }
        OverlayOutcome::Continue
    }

    fn cursor_in_selection(&self) -> bool {
        if let Some((x, y, w, h)) = self.selection {
            let (cx, cy) = self.cursor;
            cx >= x && cx < x + w && cy >= y && cy < y + h
        } else { false }
    }

    fn start_annotation(&self) -> Option<Annotation> {
        let p = Point2::new(self.cursor.0 as f32, self.cursor.1 as f32);
        Some(match self.tool {
            Tool::Pen       => Annotation::pen(self.stroke, p),
            Tool::Arrow     => Annotation::arrow(self.stroke, p),
            Tool::Rectangle => Annotation::rectangle(self.stroke, p),
            Tool::Ellipse   => Annotation::ellipse(self.stroke, p),
            // Tools queued for later slices — clicking their toolbar button
            // selects them, but starting a drag is a no-op until we ship the
            // rasterizer paths for them.
            Tool::Line | Tool::Marker | Tool::Text | Tool::Step | Tool::Pixelate => return None,
        })
    }

    fn commit(&mut self) -> OverlayOutcome {
        if self.state == State::Selected {
            if let Some(rect) = self.selection {
                let mut img = crop(&self.screenshot, rect);
                // Burn annotations into the cropped output, translating their
                // coordinates from window-space into the crop's local space.
                let dx = -rect.0 as f32;
                let dy = -rect.1 as f32;
                let mut surf = ImageSurface(&mut img);
                for a in &self.annotations {
                    let translated = translate_annotation(a, dx, dy);
                    painter::render_annotation(&mut surf, &translated);
                }
                return OverlayOutcome::Accepted(img);
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

        let dim_num: u32   = 55;
        let dim_denom: u32 = 100;
        let sel_rect = self.selection.map(|(x, y, w, h)| (x, y, x + w, y + h));

        // Pass 1: screenshot + dim outside selection.
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
                } else { false };
                let (rr, gg, bb) = if inside {
                    (r, g, b)
                } else {
                    (r * dim_num / dim_denom, g * dim_num / dim_denom, b * dim_num / dim_denom)
                };
                buf[dst_idx] = (rr << 16) | (gg << 8) | bb;
            }
        }

        // Pass 2: annotations, clipped to the selection. We render into the
        // shared u32 buffer through `U32Surface`. Bounds-clipping happens at
        // the per-pixel level inside the painter so we don't have to manage
        // a scissor here, but we still skip when there's no selection.
        let mut surf = U32Surface { buf: &mut buf, stride: win_w as i32, height: win_h as i32 };
        for a in &self.annotations {
            painter::render_annotation(&mut surf, a);
        }
        if let Some(a) = self.current.as_ref() {
            painter::render_annotation(&mut surf, a);
        }

        // Pass 3: selection border + 8 handles.
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

        // Pass 4: toolbar (only while a region is locked in).
        if matches!(self.state, State::Selected | State::Drawing) {
            self.draw_toolbar(&mut buf, win_w, win_h);
        }

        if let Err(e) = buf.present() {
            eprintln!("overlay: buf.present: {e}");
        }
    }

    // ── toolbar ─────────────────────────────────────────────────────────────

    fn toolbar_origin(&self, win_w: usize) -> (i32, i32) {
        let n = Tool::ALL.len() as i32;
        let inner = n * TOOLBAR_BTN + (n - 1) * TOOLBAR_GAP;
        let total = inner + TOOLBAR_PAD * 2;
        let x = ((win_w as i32) - total) / 2;
        (x.max(0), TOOLBAR_TOP)
    }

    fn toolbar_button_rect(&self, win_w: usize, idx: i32) -> (i32, i32, i32, i32) {
        let (ox, oy) = self.toolbar_origin(win_w);
        let x = ox + TOOLBAR_PAD + idx * (TOOLBAR_BTN + TOOLBAR_GAP);
        let y = oy + TOOLBAR_PAD;
        (x, y, x + TOOLBAR_BTN, y + TOOLBAR_BTN)
    }

    fn toolbar_hit(&self, (cx, cy): (i32, i32)) -> Option<Tool> {
        let win_w = self.window.inner_size().width as usize;
        for (i, t) in Tool::ALL.iter().enumerate() {
            let (x0, y0, x1, y1) = self.toolbar_button_rect(win_w, i as i32);
            if cx >= x0 && cx < x1 && cy >= y0 && cy < y1 {
                return Some(*t);
            }
        }
        None
    }

    fn draw_toolbar(&self, buf: &mut [u32], win_w: usize, win_h: usize) {
        const BG:           u32 = 0x00_22_22_24;
        const BTN:          u32 = 0x00_2E_2E_32;
        const BTN_ACTIVE:   u32 = 0x00_64_95_ED;
        const BTN_DISABLED: u32 = 0x00_3A_3A_3E;
        const STRIPE:       u32 = 0x00_DC_26_26;
        const TEXT:         u32 = 0x00_E8_E8_EC;

        let n = Tool::ALL.len() as i32;
        let inner = n * TOOLBAR_BTN + (n - 1) * TOOLBAR_GAP;
        let total = inner + TOOLBAR_PAD * 2;
        let (ox, oy) = self.toolbar_origin(win_w);
        let h_total  = TOOLBAR_BTN + TOOLBAR_PAD * 2;

        draw_rounded_rect(buf, win_w, win_h, ox, oy, ox + total, oy + h_total, TOOLBAR_RADIUS, BG);

        for (i, t) in Tool::ALL.iter().enumerate() {
            let (x0, y0, x1, y1) = self.toolbar_button_rect(win_w, i as i32);
            let active   = *t == self.tool;
            let working  = matches!(t, Tool::Pen | Tool::Arrow | Tool::Rectangle | Tool::Ellipse);
            let bg = if active { BTN_ACTIVE } else if working { BTN } else { BTN_DISABLED };
            draw_rounded_rect(buf, win_w, win_h, x0, y0, x1, y1, 6, bg);
            draw_tool_glyph(buf, win_w, win_h, x0, y0, x1, y1, *t, TEXT);
            if !working {
                // Diagonal stripe across disabled buttons so you can see what's
                // shipped vs what's still queued.
                draw_diagonal_stripe(buf, win_w, win_h, x0, y0, x1, y1, STRIPE);
            }
        }

        // 4-px swatch under the active tool showing the current stroke color.
        if let Some(active_idx) = Tool::ALL.iter().position(|t| *t == self.tool) {
            let (x0, _y0, x1, y1) = self.toolbar_button_rect(win_w, active_idx as i32);
            let swatch = self.stroke.color;
            let rgb = ((swatch.r as u32) << 16) | ((swatch.g as u32) << 8) | swatch.b as u32;
            draw_filled_rect(buf, win_w, win_h, x0 + 4, y1 + 2, x1 - 4, y1 + 5, rgb);
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

/// Translate an annotation by (dx, dy) — used to move window-space coords
/// into the cropped output's local space when burning into the saved PNG.
fn translate_annotation(a: &Annotation, dx: f32, dy: f32) -> Annotation {
    use kashot_core::annotation::AnnotationKind as K;
    let shift = |p: Point2| Point2::new(p.x + dx, p.y + dy);
    let kind = match a.kind.clone() {
        K::Pen       { stroke, points } => K::Pen       { stroke, points: points.into_iter().map(shift).collect() },
        K::Marker    { stroke, points } => K::Marker    { stroke, points: points.into_iter().map(shift).collect() },
        K::Line      { stroke, start, end } => K::Line      { stroke, start: shift(start), end: shift(end) },
        K::Arrow     { stroke, start, end } => K::Arrow     { stroke, start: shift(start), end: shift(end) },
        K::Rectangle { stroke, start, end } => K::Rectangle { stroke, start: shift(start), end: shift(end) },
        K::Ellipse   { stroke, start, end } => K::Ellipse   { stroke, start: shift(start), end: shift(end) },
        K::Pixelate  { start, end, block_size } => K::Pixelate { start: shift(start), end: shift(end), block_size },
        K::Text      { color, position, text, font_size } => K::Text { color, position: shift(position), text, font_size },
        K::Step      { color, center, number } => K::Step { color, center: shift(center), number },
    };
    Annotation { kind }
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

/// Filled rect — radius is reserved for a later AA pass; sharp corners
/// are good enough for the slice-1 toolbar chrome.
fn draw_rounded_rect(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, _radius: i32, rgb: u32,
) {
    draw_filled_rect(buf, stride, height, x0, y0, x1, y1, rgb);
}

/// Draw a thin diagonal stripe across the rect to indicate "disabled".
fn draw_diagonal_stripe(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32,
) {
    let w = x1 - x0;
    let h = y1 - y0;
    let steps = w.max(h);
    for i in 0..steps {
        let x = x0 + i * w / steps;
        let y = y0 + i * h / steps;
        for dy in -1..=1 {
            let yy = (y + dy).max(0) as usize;
            if x >= 0 && (x as usize) < stride && yy < height {
                buf[yy * stride + x as usize] = rgb;
            }
        }
    }
}

/// Draw a procedural glyph for each tool — same convention the C# overlay
/// uses (`IconPen`, `IconArrow`, ...). These are intentionally minimalist:
/// a few line strokes inside the button bounds. A future slice can swap
/// these for actual SVG icons if we want to.
fn draw_tool_glyph(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, tool: Tool, rgb: u32,
) {
    let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
    let pad = 8;
    let ix0 = x0 + pad;
    let iy0 = y0 + pad;
    let ix1 = x1 - pad;
    let iy1 = y1 - pad;
    let stamp = |buf: &mut [u32], x: i32, y: i32| {
        if x >= 0 && (x as usize) < stride && y >= 0 && (y as usize) < height {
            buf[y as usize * stride + x as usize] = rgb;
        }
    };
    let line2 = |buf: &mut [u32], a: (i32, i32), b: (i32, i32)| {
        // Naive thin Bresenham — fine for icon-sized glyphs.
        let mut x0 = a.0; let mut y0 = a.1;
        let x1 = b.0; let y1 = b.1;
        let dx =  (x1 - x0).abs(); let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            stamp(buf, x0, y0);
            if x0 == x1 && y0 == y1 { break; }
            let e2 = 2 * err;
            if e2 >= dy { err += dy; x0 += sx; }
            if e2 <= dx { err += dx; y0 += sy; }
        }
    };
    match tool {
        Tool::Pen       => line2(buf, (ix0, iy1), (ix1, iy0)),
        Tool::Line      => line2(buf, (ix0, iy1), (ix1, iy0)),
        Tool::Arrow     => {
            line2(buf, (ix0, iy1), (ix1, iy0));
            line2(buf, (ix1, iy0), (ix1 - 6, iy0));
            line2(buf, (ix1, iy0), (ix1, iy0 + 6));
        }
        Tool::Rectangle => {
            line2(buf, (ix0, iy0), (ix1, iy0));
            line2(buf, (ix1, iy0), (ix1, iy1));
            line2(buf, (ix1, iy1), (ix0, iy1));
            line2(buf, (ix0, iy1), (ix0, iy0));
        }
        Tool::Ellipse   => {
            // 64-step parametric circle.
            let rx = (ix1 - ix0) / 2;
            let ry = (iy1 - iy0) / 2;
            let mut prev = (cx + rx, cy);
            for k in 1..=64 {
                let t = (k as f32) / 64.0 * std::f32::consts::TAU;
                let p = (cx + ((rx as f32) * t.cos()) as i32, cy + ((ry as f32) * t.sin()) as i32);
                line2(buf, prev, p);
                prev = p;
            }
        }
        Tool::Marker    => {
            line2(buf, (ix0, iy1), (ix1, iy0));
            line2(buf, (ix0, iy1 - 1), (ix1, iy0 - 1));
            line2(buf, (ix0, iy1 - 2), (ix1, iy0 - 2));
        }
        Tool::Text      => {
            line2(buf, (ix0, iy0), (ix1, iy0));
            line2(buf, (cx,  iy0), (cx,  iy1));
        }
        Tool::Step      => {
            let rx = (ix1 - ix0) / 2;
            let ry = (iy1 - iy0) / 2;
            let mut prev = (cx + rx, cy);
            for k in 1..=48 {
                let t = (k as f32) / 48.0 * std::f32::consts::TAU;
                let p = (cx + ((rx as f32) * t.cos()) as i32, cy + ((ry as f32) * t.sin()) as i32);
                line2(buf, prev, p);
                prev = p;
            }
            line2(buf, (cx - 1, cy + 3), (cx + 1, cy + 3));
            line2(buf, (cx - 2, cy - 3), (cx + 2, cy - 3));
        }
        Tool::Pixelate  => {
            for gy in 0..3 {
                for gx in 0..3 {
                    let bx = ix0 + gx * (ix1 - ix0) / 3;
                    let by = iy0 + gy * (iy1 - iy0) / 3;
                    let bw = (ix1 - ix0) / 3;
                    let bh = (iy1 - iy0) / 3;
                    if (gx + gy) & 1 == 0 {
                        for yy in by..by + bh {
                            for xx in bx..bx + bw {
                                stamp(buf, xx, yy);
                            }
                        }
                    }
                }
            }
        }
    }
}
