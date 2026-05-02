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
use kashot_core::state::{hit_test_edge, Edge};
use kashot_core::tool::Tool;
use softbuffer::{Context, Surface};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Fullscreen, Window, WindowAttributes, WindowId, WindowLevel};

use crate::painter::{self, ImageSurface, U32Surface};

/// What the overlay window should do next after handling an event.
pub enum OverlayOutcome {
    /// Keep the overlay alive — more events expected.
    Continue,
    /// User cancelled (Esc, window close). Caller should drop the Overlay.
    Cancelled,
    /// User accepted a region (Enter / right-click / Ctrl+S). Caller saves
    /// the cropped bitmap to the configured output folder.
    Accepted(ImageBuffer<Rgba<u8>, Vec<u8>>),
    /// User pressed Ctrl+C — caller writes the cropped bitmap to the
    /// system clipboard via arboard instead of saving to disk.
    Copied(ImageBuffer<Rgba<u8>, Vec<u8>>),
    /// User pressed Ctrl+P — caller floats the bitmap as a pinned, always-
    /// on-top window at the selection's screen position. Carries the (x, y)
    /// of the selection so the pin window opens right where the user
    /// captured. Mirrors `Kashot/PinForm.cs`.
    Pinned(ImageBuffer<Rgba<u8>, Vec<u8>>, (i32, i32)),
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
    /// User grabbed an edge or corner of the locked-in selection — mouse-
    /// move adjusts the corresponding side of the rect, mouse-up returns
    /// to `Selected`. The grabbed edge lives in `resize_edge`.
    Resizing,
    /// User clicked with `Tool::Text` active and is now typing into a
    /// pending Text annotation. Typed characters extend the buffer in
    /// `current`; Backspace deletes; Enter commits; Esc cancels.
    TextInput,
}

/// Tool / action panel geometry. Mirrors `Kashot/OverlayForm.cs::PositionToolbars`:
/// the tool panel is a vertical column adjacent to the right edge of the
/// selection; the action panel is a horizontal row beneath the selection,
/// right-aligned. Both fall back to the opposite side if they'd clip the
/// screen edge. Free-floating, never covering the whole screen.
const PANEL_BTN:    i32 = 36;
const PANEL_GAP:    i32 = 4;
const PANEL_PAD:    i32 = 5;
const PANEL_RADIUS: i32 = 8;
/// Wide gap between visually distinct groups inside the tool panel.
const PANEL_GROUP_GAP: i32 = 8;
/// Stroke widths the thickness button cycles through. Default is index 1
/// (4 px) — matches `Stroke::default().thickness` in kashot-core.
const THICKNESSES: [f32; 3] = [2.0, 4.0, 8.0];

/// Tool-panel button identities. The first 9 mirror `Tool::ALL`; the last 4
/// (`Color`, `Thickness`, `Undo`, `Redo`) are buttons that don't pick a tool
/// — they trigger a popup or an action. Mirrors C# CreateToolPanel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolPanelButton { Tool(Tool), Color, Thickness, Undo, Redo }

const TOOL_PANEL_BUTTONS: [ToolPanelButton; 13] = [
    ToolPanelButton::Tool(Tool::Pen),
    ToolPanelButton::Tool(Tool::Line),
    ToolPanelButton::Tool(Tool::Arrow),
    ToolPanelButton::Tool(Tool::Rectangle),
    ToolPanelButton::Tool(Tool::Ellipse),
    ToolPanelButton::Tool(Tool::Marker),
    ToolPanelButton::Tool(Tool::Text),
    ToolPanelButton::Tool(Tool::Step),
    ToolPanelButton::Tool(Tool::Pixelate),
    // Visual divider sits between index 8 and 9 — see `tool_panel_dims`.
    ToolPanelButton::Color,
    ToolPanelButton::Thickness,
    ToolPanelButton::Undo,
    ToolPanelButton::Redo,
];

/// Action-panel buttons (horizontal row under the selection). Returning
/// outcomes routed through `tray_loop`. `Close` mirrors C# OverlayForm
/// "Close (Esc)" — closes the overlay without saving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionButton { Pin, Copy, Save, Close }

const ACTION_BUTTONS: [ActionButton; 4] = [
    ActionButton::Pin, ActionButton::Copy, ActionButton::Save, ActionButton::Close,
];

/// Magnifier — small zoomed lens shown near the cursor in Idle / Selecting,
/// so the user can position the selection edge by individual pixels.
const MAG_ZOOM:    i32 = 7;
const MAG_RADIUS:  i32 = 8;          // sample ±8 source pixels around cursor
const MAG_PIXELS:  i32 = MAG_RADIUS * 2 + 1;
const MAG_SIZE:    i32 = MAG_PIXELS * MAG_ZOOM;
const MAG_OFFSET:  i32 = 24;         // pixel offset from cursor to chip corner

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
    /// Stack of annotations that have been undone with Ctrl+Z but can still
    /// be redone with Ctrl+Y / Ctrl+Shift+Z. Adding any new annotation
    /// clears this — same convention as `Kashot/OverlayForm.cs`.
    redo_stack:  Vec<Annotation>,
    /// In-progress annotation while `state == Drawing`.
    current:     Option<Annotation>,
    /// Next number assigned by `Tool::Step`. Resets to 1 whenever the user
    /// clears the selection (Esc on `Selected` or starts a fresh drag).
    step_count:  u32,
    /// Live modifier state — winit 0.30 doesn't put modifiers on KeyEvent so
    /// we track them via `WindowEvent::ModifiersChanged` and consult them in
    /// the keyboard handler for Ctrl+Z / Ctrl+Y / Ctrl+S / Ctrl+C.
    mods:        ModifiersState,
    /// Which edge / corner is being dragged while `state == Resizing`.
    resize_edge: Edge,
    /// True while the color palette popup is showing. Toggled by clicking
    /// the Color button in the tool panel; closes when the user picks a
    /// swatch or clicks anywhere outside the popup.
    palette_open:  bool,
    /// Active palette in the popup (0..=3 → Vivid / Highlighter / Pastel /
    /// Pro). Stored on Overlay rather than in `Stroke` so swapping between
    /// palettes doesn't change the live stroke color until the user picks
    /// a new swatch. Mirrors C# `_paletteIndex`.
    palette_index: usize,
    /// Hovered tool/action/utility button → tooltip label + anchor pixel.
    /// Recomputed on every CursorMoved while in `Selected`. Mirrors the
    /// `tip` arg in C# OverlayForm `MakeButton(tip, …)`.
    hover_tip:     Option<(&'static str, i32, i32)>,
}

impl Overlay {
    /// Open the fullscreen overlay window for the given screenshot.
    pub fn new(
        loop_target: &ActiveEventLoop,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
    ) -> Result<Self> {
        // Plain borderless fullscreen — let the WM manage focus + stacking
        // normally. We tried `override_redirect=true` on X11 to layer above
        // DOCK panels, but it blocked KeyPress delivery to the Text tool on
        // Cinnamon because keyboard focus doesn't propagate to override-
        // redirect windows the same way. Trade-off: dock panels may still
        // be visible at the screen edges, but every annotation tool works
        // including Text. Mirrors C# OverlayForm:
        //   `FormBorderStyle = None; WindowState = Maximized;`
        // `Fullscreen::Borderless(None)` opens at a default size on Cinnamon
        // (~800×600). Setting an explicit inner_size to the primary monitor's
        // physical size + position (0,0) + AlwaysOnTop makes the WM open us
        // at full screen even when fullscreen state isn't honored.
        let monitor_size = loop_target
            .primary_monitor()
            .or_else(|| loop_target.available_monitors().next())
            .map(|m| m.size())
            .unwrap_or(winit::dpi::PhysicalSize::new(
                screenshot.width(),
                screenshot.height(),
            ));
        let primary = loop_target.primary_monitor()
            .or_else(|| loop_target.available_monitors().next());
        let attrs = WindowAttributes::default()
            .with_title("Kashot")
            .with_decorations(false)
            .with_resizable(false)
            .with_inner_size(monitor_size)
            .with_position(PhysicalPosition::new(0i32, 0i32))
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_fullscreen(Some(Fullscreen::Borderless(primary)));

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window: {e}"))?;

        window.set_cursor(CursorIcon::Crosshair);
        window.focus_window();

        // No manual X11 focus grab — with the regular WM-managed fullscreen
        // window, Cinnamon / Plasma / GNOME Shell hand the keyboard to us
        // when we map. `Window::focus_window()` above sends the
        // _NET_ACTIVE_WINDOW client message which is the spec-correct way
        // to ask for focus.

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
            redo_stack:  Vec::new(),
            current:     None,
            step_count:  1,
            mods:        ModifiersState::empty(),
            resize_edge: Edge::None,
            palette_open: false,
            palette_index: 0,
            hover_tip:     None,
        })
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    pub fn handle_event(&mut self, event: WindowEvent) -> OverlayOutcome {
        match event {
            WindowEvent::CloseRequested => OverlayOutcome::Cancelled,

            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                OverlayOutcome::Continue
            }

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
                    State::Resizing => {
                        self.apply_resize();
                        self.window.request_redraw();
                    }
                    State::Selected => {
                        // Update the cursor icon based on which edge we're
                        // hovering, matching the C# OverlayForm convention.
                        self.update_resize_cursor();
                        // And recompute the hover tooltip so the user can
                        // tell Pen / Line / Marker apart instantly. Mirrors
                        // C# MakeButton(tip, …) tooltip text.
                        let prev = self.hover_tip;
                        self.hover_tip = self.compute_hover_tip();
                        if prev != self.hover_tip { self.window.request_redraw(); }
                    }
                    State::Idle => {
                        // Magnifier follows the cursor in Idle so the user
                        // can place the first selection edge by pixel.
                        self.window.request_redraw();
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
        eprintln!("kashot: key={:?} state={:?} mods={:?}", key, self.state, self.mods);
        // Text-input state owns the keyboard while it's active — typed
        // characters extend the pending annotation; Enter commits, Esc
        // cancels, Backspace pops the last char.
        if self.state == State::TextInput {
            return self.handle_text_key(key);
        }
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
                    self.redo_stack.clear();
                    self.step_count  = 1;
                    self.window.request_redraw();
                    OverlayOutcome::Continue
                } else {
                    OverlayOutcome::Cancelled
                }
            }
            Key::Named(NamedKey::Enter) => self.commit(),
            Key::Character(s) => {
                if self.state != State::Selected {
                    return OverlayOutcome::Continue;
                }
                let c    = match s.chars().next() { Some(c) => c, None => return OverlayOutcome::Continue };
                let ctrl = self.mods.control_key();
                let shift = self.mods.shift_key();
                let lc    = c.to_ascii_lowercase();
                if ctrl {
                    match lc {
                        // Ctrl+Z → undo, Ctrl+Shift+Z → redo
                        'z' => { if shift { self.redo(); } else { self.undo(); } }
                        // Ctrl+Y → redo (Windows convention)
                        'y' => self.redo(),
                        // Ctrl+S → commit-and-save (same as Enter)
                        's' => return self.commit(),
                        // Ctrl+C → commit-and-copy
                        'c' => return self.commit_as_copy(),
                        // Ctrl+P → commit-and-pin (float bitmap on screen)
                        'p' => return self.commit_as_pin(),
                        _ => {}
                    }
                } else if let Some(t) = Tool::from_key(c) {
                    self.tool = t;
                    self.window.request_redraw();
                }
                OverlayOutcome::Continue
            }
            _ => OverlayOutcome::Continue,
        }
    }

    fn handle_text_key(&mut self, key: Key) -> OverlayOutcome {
        use kashot_core::annotation::AnnotationKind;
        eprintln!("kashot: text-input key={:?}", key);
        match key {
            Key::Named(NamedKey::Escape) => {
                self.current = None;
                self.state   = State::Selected;
                self.window.request_redraw();
            }
            Key::Named(NamedKey::Enter) => {
                // Commit only if the user actually typed something.
                if let Some(a) = self.current.take() {
                    if let AnnotationKind::Text { ref text, .. } = a.kind {
                        if !text.is_empty() {
                            self.add_annotation(a);
                        }
                    }
                }
                self.state = State::Selected;
                self.window.request_redraw();
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(a) = self.current.as_mut() {
                    if let AnnotationKind::Text { ref mut text, .. } = a.kind {
                        text.pop();
                        self.window.request_redraw();
                    }
                }
            }
            Key::Named(NamedKey::Space) => {
                if let Some(a) = self.current.as_mut() {
                    if let AnnotationKind::Text { ref mut text, .. } = a.kind {
                        text.push(' ');
                        self.window.request_redraw();
                    }
                }
            }
            Key::Character(s) => {
                // Skip Ctrl-modified characters so Ctrl+Z / Ctrl+S etc. don't
                // get swallowed as plain text input.
                if self.mods.control_key() { return OverlayOutcome::Continue; }
                if let Some(a) = self.current.as_mut() {
                    if let AnnotationKind::Text { ref mut text, .. } = a.kind {
                        for c in s.chars() {
                            if !c.is_control() {
                                text.push(c);
                            }
                        }
                        self.window.request_redraw();
                    }
                }
            }
            _ => {}
        }
        OverlayOutcome::Continue
    }

    fn undo(&mut self) {
        if let Some(a) = self.annotations.pop() {
            // Step counter follows the visible numbers — popping a Step
            // brings us back to where we were.
            if let kashot_core::annotation::AnnotationKind::Step { number, .. } = a.kind {
                self.step_count = number;
            }
            self.redo_stack.push(a);
            self.window.request_redraw();
        }
    }

    fn redo(&mut self) {
        if let Some(a) = self.redo_stack.pop() {
            if let kashot_core::annotation::AnnotationKind::Step { number, .. } = a.kind {
                self.step_count = number.saturating_add(1);
            }
            self.annotations.push(a);
            self.window.request_redraw();
        }
    }

    fn handle_left_press(&mut self) -> OverlayOutcome {
        // Click anywhere while typing → commit the pending text and keep
        // going. Mirrors the C# TextBox-loses-focus behaviour.
        if self.state == State::TextInput {
            use kashot_core::annotation::AnnotationKind;
            if let Some(a) = self.current.take() {
                if let AnnotationKind::Text { ref text, .. } = a.kind {
                    if !text.is_empty() {
                        self.add_annotation(a);
                    }
                }
            }
            self.state = State::Selected;
            self.window.request_redraw();
            // Fall through so the click can still pick a swatch / start a
            // new text input / drag a region etc.
        }
        // Tool/action panel + popup hit-testing happens BEFORE the
        // edge-resize / draw-start path, so a click on a button always
        // wins over a draw inside the selection. Mirrors the order in
        // C# OverlayForm.OnMouseDown.
        if self.state == State::Selected {
            if let Some(sel) = self.selection {
                let win_w = self.window.inner_size().width  as usize;
                let win_h = self.window.inner_size().height as usize;
                let tp_origin = tool_panel_origin(win_w, win_h, sel);

                // Color popup — must be tested before the tool panel itself
                // so a click that falls on the popup doesn't get eaten by
                // the panel underneath.
                if self.palette_open {
                    let pp_origin = palette_popup_origin(win_w, tp_origin);
                    // Header arrows — prev/next palette.
                    if let Some(prev) = palette_header_hit(pp_origin, self.cursor) {
                        if prev {
                            self.palette_index = (self.palette_index + PALETTE_COUNT - 1) % PALETTE_COUNT;
                        } else {
                            self.palette_index = (self.palette_index + 1) % PALETTE_COUNT;
                        }
                        self.window.request_redraw();
                        return OverlayOutcome::Continue;
                    }
                    if let Some(idx) = palette_popup_hit(pp_origin, self.cursor) {
                        let pal = kashot_core::annotation::Palettes::get(self.palette_index);
                        self.stroke.color = pal.colors[idx];
                        self.palette_open = false;
                        self.window.request_redraw();
                        return OverlayOutcome::Continue;
                    }
                    if !palette_popup_in(pp_origin, self.cursor) {
                        // Click outside the popup → close it; let the click
                        // continue dispatching (so e.g. clicking another
                        // button still works).
                        self.palette_open = false;
                    } else {
                        return OverlayOutcome::Continue;
                    }
                }

                // Tool panel.
                if let Some((_, btn)) = tool_panel_hit(tp_origin, self.cursor) {
                    match btn {
                        ToolPanelButton::Tool(t) => { self.tool = t; }
                        ToolPanelButton::Color   => { self.palette_open = !self.palette_open; }
                        ToolPanelButton::Thickness => {
                            // Cycle through the configured stroke widths,
                            // matching C# OverlayForm.CycleThickness.
                            let cur = THICKNESSES.iter().position(|t| (t - self.stroke.thickness).abs() < 0.01).unwrap_or(1);
                            self.stroke.thickness = THICKNESSES[(cur + 1) % THICKNESSES.len()];
                        }
                        ToolPanelButton::Undo => self.undo(),
                        ToolPanelButton::Redo => self.redo(),
                    }
                    self.window.request_redraw();
                    return OverlayOutcome::Continue;
                }

                // Action panel.
                let ap_origin = action_panel_origin(win_w, win_h, sel);
                if let Some(action) = action_panel_hit(ap_origin, self.cursor) {
                    return match action {
                        ActionButton::Pin   => self.commit_as_pin(),
                        ActionButton::Copy  => self.commit_as_copy(),
                        ActionButton::Save  => self.commit(),
                        ActionButton::Close => OverlayOutcome::Cancelled,
                    };
                }
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
                // Edge-resize takes priority over starting a draw — if the
                // cursor is sitting on an edge or corner of the selection,
                // clicking there grabs that edge for resizing.
                if let Some(sel) = self.selection {
                    let hit = hit_test_edge(
                        (sel.0 as f32, sel.1 as f32, sel.2 as f32, sel.3 as f32),
                        (self.cursor.0 as f32, self.cursor.1 as f32),
                    );
                    if hit.is_some() {
                        self.state       = State::Resizing;
                        self.resize_edge = hit;
                        self.window.request_redraw();
                        return OverlayOutcome::Continue;
                    }
                }
                if self.cursor_in_selection() {
                    // Step is click-to-place — never enters `Drawing`. Drop a
                    // numbered marker right where the user clicked and bump
                    // the counter for the next click.
                    if self.tool == Tool::Step {
                        let p = Point2::new(self.cursor.0 as f32, self.cursor.1 as f32);
                        self.add_annotation(Annotation::step(self.stroke.color, p, self.step_count));
                        self.step_count = self.step_count.saturating_add(1);
                        self.window.request_redraw();
                    } else if self.tool == Tool::Text {
                        // Click-to-place a text caret. Typed characters
                        // extend the annotation; Enter commits, Esc cancels.
                        let p = Point2::new(self.cursor.0 as f32, self.cursor.1 as f32);
                        self.current = Some(Annotation::text(self.stroke.color, p, ""));
                        self.state   = State::TextInput;
                        eprintln!("kashot: entered TextInput at ({}, {})", p.x, p.y);
                        self.window.request_redraw();
                    } else if let Some(a) = self.start_annotation() {
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
                    self.redo_stack.clear();
                    self.step_count = 1;
                    self.window.request_redraw();
                }
            }
            _ => {}
        }
        OverlayOutcome::Continue
    }

    fn add_annotation(&mut self, a: Annotation) {
        self.annotations.push(a);
        self.redo_stack.clear();
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
                    self.add_annotation(a);
                }
                self.state = State::Selected;
                self.window.request_redraw();
            }
            State::Resizing => {
                self.state       = State::Selected;
                self.resize_edge = Edge::None;
                self.window.set_cursor(CursorIcon::Crosshair);
                self.window.request_redraw();
            }
            _ => {}
        }
        OverlayOutcome::Continue
    }

    /// Mutate the selection rect according to the edge being dragged.
    fn apply_resize(&mut self) {
        let Some((mut x, mut y, mut w, mut h)) = self.selection else { return; };
        let (cx, cy) = self.cursor;
        match self.resize_edge {
            Edge::Left       => { let dx = cx - x; w -= dx; x  = cx; }
            Edge::Right      => { w = cx - x; }
            Edge::Top        => { let dy = cy - y; h -= dy; y  = cy; }
            Edge::Bottom     => { h = cy - y; }
            Edge::TopLeft    => { let dx = cx - x; let dy = cy - y; w -= dx; h -= dy; x = cx; y = cy; }
            Edge::TopRight   => { let dy = cy - y; w = cx - x; h -= dy; y = cy; }
            Edge::BottomLeft => { let dx = cx - x; w -= dx; h = cy - y; x = cx; }
            Edge::BottomRight=> { w = cx - x; h = cy - y; }
            Edge::None       => {}
        }
        // Clamp to non-negative width/height — flip the rect if the user
        // dragged past the opposite edge.
        if w < 0 { x += w; w = -w; }
        if h < 0 { y += h; h = -h; }
        if w < 4 { w = 4; }
        if h < 4 { h = 4; }
        self.selection = Some((x, y, w, h));
    }

    fn compute_hover_tip(&self) -> Option<(&'static str, i32, i32)> {
        let sel = self.selection?;
        let win_w = self.window.inner_size().width  as usize;
        let win_h = self.window.inner_size().height as usize;
        // Tool panel.
        let tp = tool_panel_origin(win_w, win_h, sel);
        if let Some((idx, btn)) = tool_panel_hit(tp, self.cursor) {
            let (_, _, x1, y1) = tool_panel_button_rect(tp, idx as i32);
            let label = match btn {
                ToolPanelButton::Tool(Tool::Pen)        => "Pen (P)",
                ToolPanelButton::Tool(Tool::Line)       => "Line (L)",
                ToolPanelButton::Tool(Tool::Arrow)      => "Arrow (A)",
                ToolPanelButton::Tool(Tool::Rectangle)  => "Rectangle (R)",
                ToolPanelButton::Tool(Tool::Ellipse)    => "Ellipse (E)",
                ToolPanelButton::Tool(Tool::Marker)     => "Marker (M)",
                ToolPanelButton::Tool(Tool::Text)       => "Text (T)",
                ToolPanelButton::Tool(Tool::Step)       => "Step (N)",
                ToolPanelButton::Tool(Tool::Pixelate)   => "Pixelate (B)",
                ToolPanelButton::Color                  => "Color",
                ToolPanelButton::Thickness              => "Thickness",
                ToolPanelButton::Undo                   => "Undo (Ctrl+Z)",
                ToolPanelButton::Redo                   => "Redo (Ctrl+Y)",
            };
            return Some((label, x1 + 6, y1 - 14));
        }
        // Action panel.
        let ap = action_panel_origin(win_w, win_h, sel);
        if let Some(btn) = action_panel_hit(ap, self.cursor) {
            let label = match btn {
                ActionButton::Pin   => "Pin to screen",
                ActionButton::Copy  => "Copy (Ctrl+C)",
                ActionButton::Save  => "Save (Ctrl+S)",
                ActionButton::Close => "Close (Esc)",
            };
            return Some((label, self.cursor.0 + 14, self.cursor.1 + 14));
        }
        None
    }

    fn update_resize_cursor(&self) {
        let Some(sel) = self.selection else { return; };
        let hit = hit_test_edge(
            (sel.0 as f32, sel.1 as f32, sel.2 as f32, sel.3 as f32),
            (self.cursor.0 as f32, self.cursor.1 as f32),
        );
        let icon = match hit {
            Edge::Left | Edge::Right                  => CursorIcon::EwResize,
            Edge::Top  | Edge::Bottom                 => CursorIcon::NsResize,
            Edge::TopLeft | Edge::BottomRight         => CursorIcon::NwseResize,
            Edge::TopRight | Edge::BottomLeft         => CursorIcon::NeswResize,
            Edge::None                                => CursorIcon::Crosshair,
        };
        self.window.set_cursor(icon);
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
            Tool::Line      => Annotation::line(self.stroke, p),
            Tool::Marker    => Annotation::marker(self.stroke, p),
            Tool::Pixelate  => Annotation::pixelate(p),
            // Step is handled inline at click site (no `Drawing` state).
            Tool::Step      => return None,
            // Text enters its own `TextInput` state instead of `Drawing` —
            // also handled inline.
            Tool::Text      => return None,
        })
    }

    fn commit(&mut self) -> OverlayOutcome {
        match self.compose_final() {
            Some(img) => OverlayOutcome::Accepted(img),
            None      => OverlayOutcome::Continue,
        }
    }

    fn commit_as_copy(&mut self) -> OverlayOutcome {
        match self.compose_final() {
            Some(img) => OverlayOutcome::Copied(img),
            None      => OverlayOutcome::Continue,
        }
    }

    fn commit_as_pin(&mut self) -> OverlayOutcome {
        let pos = match self.selection {
            Some((x, y, _, _)) => (x, y),
            None               => return OverlayOutcome::Continue,
        };
        match self.compose_final() {
            Some(img) => OverlayOutcome::Pinned(img, pos),
            None      => OverlayOutcome::Continue,
        }
    }

    /// Crop + composite annotations into the output bitmap. Shared between
    /// the save and copy commit paths so they're guaranteed to produce the
    /// same pixels — only what the caller does with the bitmap differs.
    fn compose_final(&self) -> Option<ImageBuffer<Rgba<u8>, Vec<u8>>> {
        if self.state != State::Selected { return None; }
        let rect = self.selection?;
        let mut img = crop(&self.screenshot, rect);
        // Snapshot the un-annotated crop FIRST so pixelate's source-sampling
        // stays idempotent under draw-order: pixelate must always sample the
        // original screenshot, never something we already painted on.
        // Mirrors C# `PixelateAnnotation`.
        let pristine = img.clone();
        let dx = -rect.0 as f32;
        let dy = -rect.1 as f32;
        let mut surf = ImageSurface(&mut img);
        for a in &self.annotations {
            let translated = translate_annotation(a, dx, dy);
            painter::render_annotation(&mut surf, &translated, Some(&pristine));
        }
        Some(img)
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
            painter::render_annotation(&mut surf, a, Some(&self.screenshot));
        }
        if let Some(a) = self.current.as_ref() {
            painter::render_annotation(&mut surf, a, Some(&self.screenshot));
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

        // Pass 4: dimension chip — small dark pill at bottom-right of the
        // selection showing the locked-in width × height. Visible whenever
        // there's a selection (including mid-drag), matches C# OverlayForm.
        if let Some((x, y, w, h)) = self.selection {
            if w > 8 && h > 8 {
                draw_dimension_chip(&mut buf, win_w, win_h, x + w, y + h, w as u32, h as u32);
            }
        }

        // Pass 5: tool panel + action panel + (optional) color popup —
        // floating around the selection, never spanning the screen.
        if matches!(self.state, State::Selected | State::Drawing | State::Resizing | State::TextInput) {
            if let Some(sel) = self.selection {
                draw_tool_panel(&mut buf, win_w, win_h, sel,
                                self.tool, self.stroke.color, self.stroke.thickness);
                draw_action_panel(&mut buf, win_w, win_h, sel);
                if self.palette_open {
                    let tp_origin = tool_panel_origin(win_w, win_h, sel);
                    draw_palette_popup(&mut buf, win_w, win_h, tp_origin, self.stroke.color, self.palette_index);
                }
            }
        }

        // Pass 6: magnifier — only useful when the user is positioning the
        // selection edge by individual pixels. Once a region is locked in,
        // the toolbar+palette take over and the lens just gets in the way.
        if matches!(self.state, State::Idle | State::Selecting) {
            draw_magnifier(&mut buf, win_w, win_h, &self.screenshot, self.cursor);
        }

        // Pass 7: tooltip chip — only when the user is hovering a button
        // in `Selected`. Mirrors C# `MakeButton(tip, ...)` behaviour.
        if let Some((label, x, y)) = self.hover_tip {
            if matches!(self.state, State::Selected) {
                draw_tooltip(&mut buf, win_w, win_h, label, x, y);
            }
        }

        if let Err(e) = buf.present() {
            eprintln!("overlay: buf.present: {e}");
        }
    }

}

// ── tool/action panel layout (mirrors C# OverlayForm.PositionToolbars) ─────

/// Outer width × height of the vertical tool panel (13 buttons + 1 divider).
fn tool_panel_dims() -> (i32, i32) {
    let n   = TOOL_PANEL_BUTTONS.len() as i32;
    let h_buttons = n * PANEL_BTN + (n - 1) * PANEL_GAP;
    // A 1-px divider sits between the 9 tool buttons and the 4 utility
    // buttons (color / thickness / undo / redo). Two extra group-gaps add
    // breathing room above and below the divider line.
    let extra = PANEL_GROUP_GAP * 2 + 1;
    let w = PANEL_BTN + PANEL_PAD * 2;
    let h = h_buttons + extra + PANEL_PAD * 2;
    (w, h)
}

fn action_panel_dims() -> (i32, i32) {
    let n = ACTION_BUTTONS.len() as i32;
    let w = n * PANEL_BTN + (n - 1) * PANEL_GAP + PANEL_PAD * 2;
    let h = PANEL_BTN + PANEL_PAD * 2;
    (w, h)
}

/// Screen-space origin of the tool panel. Right of selection by default;
/// flips to the left if the right edge would clip; rounds inward to keep
/// the panel fully on screen. Returns `None` if no selection is locked in.
fn tool_panel_origin(win_w: usize, win_h: usize, sel: (i32, i32, i32, i32)) -> (i32, i32) {
    let (sx, sy, sw, sh) = sel;
    let (pw, ph) = tool_panel_dims();
    let mut tx = sx + sw + 5;
    let mut ty = sy;
    if tx + pw > win_w as i32 { tx = sx - pw - 5; }
    if ty + ph > win_h as i32 { ty = (win_h as i32) - ph; }
    let _ = sh;
    (tx.max(0), ty.max(0))
}

fn action_panel_origin(win_w: usize, win_h: usize, sel: (i32, i32, i32, i32)) -> (i32, i32) {
    let (sx, sy, sw, sh) = sel;
    let (pw, ph) = action_panel_dims();
    let mut ax = sx + sw - pw;
    let mut ay = sy + sh + 5;
    if ay + ph > win_h as i32 { ay = sy - ph - 5; }
    if ax < 0 { ax = sx; }
    let _ = win_w;
    (ax.max(0), ay.max(0))
}

/// Rectangle for the i-th tool-panel button. Index above the divider
/// position skips one slot to leave room for the line.
fn tool_panel_button_rect(panel_origin: (i32, i32), idx: i32) -> (i32, i32, i32, i32) {
    let (ox, oy) = panel_origin;
    let x = ox + PANEL_PAD;
    // Indices 0..9 are the 9 tools. Index 9..13 are utility buttons that
    // sit *below* the divider (extra group gap + 1px line + group gap).
    let above_divider = idx < 9;
    let extra = if above_divider { 0 } else { PANEL_GROUP_GAP * 2 + 1 };
    let y = oy + PANEL_PAD + idx * (PANEL_BTN + PANEL_GAP) + extra;
    (x, y, x + PANEL_BTN, y + PANEL_BTN)
}

fn action_panel_button_rect(panel_origin: (i32, i32), idx: i32) -> (i32, i32, i32, i32) {
    let (ox, oy) = panel_origin;
    let x = ox + PANEL_PAD + idx * (PANEL_BTN + PANEL_GAP);
    let y = oy + PANEL_PAD;
    (x, y, x + PANEL_BTN, y + PANEL_BTN)
}

fn tool_panel_hit(panel_origin: (i32, i32), (cx, cy): (i32, i32)) -> Option<(usize, ToolPanelButton)> {
    for (i, b) in TOOL_PANEL_BUTTONS.iter().enumerate() {
        let (x0, y0, x1, y1) = tool_panel_button_rect(panel_origin, i as i32);
        if cx >= x0 && cx < x1 && cy >= y0 && cy < y1 {
            return Some((i, *b));
        }
    }
    None
}

fn action_panel_hit(panel_origin: (i32, i32), (cx, cy): (i32, i32)) -> Option<ActionButton> {
    for (i, b) in ACTION_BUTTONS.iter().enumerate() {
        let (x0, y0, x1, y1) = action_panel_button_rect(panel_origin, i as i32);
        if cx >= x0 && cx < x1 && cy >= y0 && cy < y1 {
            return Some(*b);
        }
    }
    None
}

// ── color popup (header + 4×4 grid of 16 swatches) ────────────────────────

const PALETTE_SWATCH: i32 = 40;
const PALETTE_COLS:   i32 = 4;
const PALETTE_ROWS:   i32 = 4;
const PALETTE_PAD:    i32 = 6;
/// Header row with prev / palette-name / next.
const PALETTE_HEADER: i32 = 32;
/// Gap between header and swatch grid.
const PALETTE_HEADER_GAP: i32 = 8;
/// Total palette count from kashot-core (Vivid / Highlighter / Pastel / Pro).
const PALETTE_COUNT: usize = 4;

fn palette_popup_dims() -> (i32, i32) {
    let grid_w = PALETTE_COLS * PALETTE_SWATCH + (PALETTE_COLS - 1) * PANEL_GAP;
    let grid_h = PALETTE_ROWS * PALETTE_SWATCH + (PALETTE_ROWS - 1) * PANEL_GAP;
    let w = grid_w + PALETTE_PAD * 2;
    let h = PALETTE_HEADER + PALETTE_HEADER_GAP + grid_h + PALETTE_PAD * 2;
    (w, h)
}

fn palette_header_button_rect(origin: (i32, i32), prev: bool) -> (i32, i32, i32, i32) {
    let (pw, _ph) = palette_popup_dims();
    let y0 = origin.1 + PALETTE_PAD;
    let y1 = y0 + PALETTE_HEADER;
    if prev {
        let x0 = origin.0 + PALETTE_PAD;
        (x0, y0, x0 + PALETTE_HEADER, y1)
    } else {
        let x1 = origin.0 + pw - PALETTE_PAD;
        (x1 - PALETTE_HEADER, y0, x1, y1)
    }
}

/// Where the color popup opens — to the LEFT of the tool panel, top-aligned
/// with the Color button, falling back to the right side if the left clips.
fn palette_popup_origin(win_w: usize, panel_origin: (i32, i32)) -> (i32, i32) {
    let (pw, _ph) = palette_popup_dims();
    let mut x = panel_origin.0 - pw - 5;
    let y     = panel_origin.1;
    if x < 0 {
        let (tw, _) = tool_panel_dims();
        x = panel_origin.0 + tw + 5;
        if x + pw > win_w as i32 { x = (win_w as i32) - pw - 5; }
    }
    (x.max(0), y.max(0))
}

fn palette_popup_swatch_rect(origin: (i32, i32), idx: i32) -> (i32, i32, i32, i32) {
    let row = idx / PALETTE_COLS;
    let col = idx % PALETTE_COLS;
    let grid_y0 = origin.1 + PALETTE_PAD + PALETTE_HEADER + PALETTE_HEADER_GAP;
    let x = origin.0 + PALETTE_PAD + col * (PALETTE_SWATCH + PANEL_GAP);
    let y = grid_y0 + row * (PALETTE_SWATCH + PANEL_GAP);
    (x, y, x + PALETTE_SWATCH, y + PALETTE_SWATCH)
}

fn palette_popup_hit(origin: (i32, i32), (cx, cy): (i32, i32)) -> Option<usize> {
    for i in 0..16 {
        let (x0, y0, x1, y1) = palette_popup_swatch_rect(origin, i as i32);
        if cx >= x0 && cx < x1 && cy >= y0 && cy < y1 { return Some(i); }
    }
    None
}

fn palette_popup_in(origin: (i32, i32), (cx, cy): (i32, i32)) -> bool {
    let (pw, ph) = palette_popup_dims();
    cx >= origin.0 && cx < origin.0 + pw && cy >= origin.1 && cy < origin.1 + ph
}

// ── drawing ─────────────────────────────────────────────────────────────────

fn draw_tool_panel(
    buf:        &mut [u32],
    win_w:      usize,
    win_h:      usize,
    sel:        (i32, i32, i32, i32),
    active:     Tool,
    swatch:     kashot_core::color::Rgba,
    thickness:  f32,
) {
    const BG:         u32 = 0x00_22_22_24;
    const BTN:        u32 = 0x00_2E_2E_32;
    const BTN_ACTIVE: u32 = 0x00_64_95_ED;
    const TEXT:       u32 = 0x00_E8_E8_EC;
    const DIVIDER:    u32 = 0x00_44_44_48;

    let (ox, oy) = tool_panel_origin(win_w, win_h, sel);
    let (pw, ph) = tool_panel_dims();
    draw_rounded_rect(buf, win_w, win_h, ox, oy, ox + pw, oy + ph, PANEL_RADIUS, BG);

    // Divider sits between buttons 8 (Pixelate) and 9 (Color).
    let div_y = oy + PANEL_PAD + 9 * (PANEL_BTN + PANEL_GAP) + PANEL_GROUP_GAP;
    draw_filled_rect(buf, win_w, win_h, ox + 6, div_y, ox + pw - 6, div_y + 1, DIVIDER);

    for (i, b) in TOOL_PANEL_BUTTONS.iter().enumerate() {
        let (x0, y0, x1, y1) = tool_panel_button_rect((ox, oy), i as i32);
        let highlight = match b {
            ToolPanelButton::Tool(t) => *t == active,
            _ => false,
        };
        let bg = if highlight { BTN_ACTIVE } else { BTN };
        draw_rounded_rect(buf, win_w, win_h, x0, y0, x1, y1, 6, bg);
        match b {
            ToolPanelButton::Tool(t)    => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Tool(*t), [232,232,236,255], None, thickness),
            ToolPanelButton::Color      => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Color, [232,232,236,255], Some([swatch.r, swatch.g, swatch.b, 255]), thickness),
            ToolPanelButton::Thickness  => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Thickness, [232,232,236,255], None, thickness),
            ToolPanelButton::Undo       => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Undo, [232,232,236,255], None, thickness),
            ToolPanelButton::Redo       => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Redo, [232,232,236,255], None, thickness),
        }
    }
}

fn draw_action_panel(
    buf:   &mut [u32],
    win_w: usize,
    win_h: usize,
    sel:   (i32, i32, i32, i32),
) {
    const BG:   u32 = 0x00_22_22_24;
    const BTN:  u32 = 0x00_2E_2E_32;
    const TEXT: u32 = 0x00_E8_E8_EC;

    let origin = action_panel_origin(win_w, win_h, sel);
    let (pw, ph) = action_panel_dims();
    draw_rounded_rect(buf, win_w, win_h, origin.0, origin.1, origin.0 + pw, origin.1 + ph, PANEL_RADIUS, BG);

    for (i, b) in ACTION_BUTTONS.iter().enumerate() {
        let (x0, y0, x1, y1) = action_panel_button_rect(origin, i as i32);
        draw_rounded_rect(buf, win_w, win_h, x0, y0, x1, y1, 6, BTN);
        match b {
            ActionButton::Pin   => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Pin,   [232,232,236,255], None, 4.0),
            ActionButton::Copy  => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Copy,  [232,232,236,255], None, 4.0),
            ActionButton::Save  => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Save,  [232,232,236,255], None, 4.0),
            ActionButton::Close => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Close, [232,232,236,255], None, 4.0),
        }
    }
}

fn draw_palette_popup(
    buf:           &mut [u32],
    win_w:         usize,
    win_h:         usize,
    panel_origin:  (i32, i32),
    active_color:  kashot_core::color::Rgba,
    palette_index: usize,
) {
    const BG:        u32 = 0x00_22_22_24;
    const HEADER_BG: u32 = 0x00_2E_2E_32;

    let origin = palette_popup_origin(win_w, panel_origin);
    let (pw, ph) = palette_popup_dims();
    draw_rounded_rect(buf, win_w, win_h, origin.0, origin.1, origin.0 + pw, origin.1 + ph, PANEL_RADIUS, BG);

    // Header — prev arrow + palette name + next arrow.
    let prev = palette_header_button_rect(origin, true);
    let next = palette_header_button_rect(origin, false);
    draw_rounded_rect(buf, win_w, win_h, prev.0, prev.1, prev.2, prev.3, 4, HEADER_BG);
    draw_rounded_rect(buf, win_w, win_h, next.0, next.1, next.2, next.3, 4, HEADER_BG);
    {
        // Center label between the two buttons, same height.
        let lx0 = prev.2 + 4;
        let lx1 = next.0 - 4;
        let ly0 = prev.1;
        let ly1 = prev.3;
        draw_rounded_rect(buf, win_w, win_h, lx0, ly0, lx1, ly1, 4, HEADER_BG);
        let name = palette_name(palette_index);
        let scale = 2;
        let text_w = crate::bitmap_font::measure(name, scale);
        let text_x = (lx0 + lx1) / 2 - text_w / 2;
        let text_y = (ly0 + ly1) / 2 - (crate::bitmap_font::GLYPH_H * scale) / 2;
        let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
        crate::painter::draw_text(&mut surf, text_x, text_y, scale, name, kashot_core::color::Rgba::WHITE);
        // Arrow glyphs.
        draw_chevron(buf, win_w, win_h, prev.0, prev.1, prev.2, prev.3, true);
        draw_chevron(buf, win_w, win_h, next.0, next.1, next.2, next.3, false);
    }

    // Swatches.
    let pal = kashot_core::annotation::Palettes::get(palette_index);
    for i in 0..16usize {
        let c = pal.colors[i];
        let (x0, y0, x1, y1) = palette_popup_swatch_rect(origin, i as i32);
        let rgb = ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32;
        draw_filled_rect(buf, win_w, win_h, x0, y0, x1, y1, rgb);
        let selected = c.r == active_color.r && c.g == active_color.g && c.b == active_color.b;
        let bw = if selected { 0x00_FF_FF_FF } else { 0x00_50_50_54 };
        draw_rect_border(buf, win_w, win_h, x0, y0, x1, y1, bw);
        if selected {
            // Double border to make selection unmistakable.
            draw_rect_border(buf, win_w, win_h, x0 + 1, y0 + 1, x1 - 1, y1 - 1, 0x00_FF_FF_FF);
        }
    }
}

fn palette_name(idx: usize) -> &'static str {
    match idx % PALETTE_COUNT {
        0 => "Vivid",
        1 => "Highlighter",
        2 => "Pastel",
        _ => "Pro",
    }
}

fn draw_chevron(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, left: bool,
) {
    let cx = (x0 + x1) / 2;
    let cy = (y0 + y1) / 2;
    let line = |buf: &mut [u32], mut sx0: i32, mut sy0: i32, ex: i32, ey: i32| {
        let dx =  (ex - sx0).abs();
        let dy = -(ey - sy0).abs();
        let stepx = if sx0 < ex { 1 } else { -1 };
        let stepy = if sy0 < ey { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            if sx0 >= 0 && (sx0 as usize) < stride && sy0 >= 0 && (sy0 as usize) < height {
                buf[sy0 as usize * stride + sx0 as usize] = 0x00_E8_E8_EC;
            }
            if sx0 == ex && sy0 == ey { break; }
            let e2 = 2 * err;
            if e2 >= dy { err += dy; sx0 += stepx; }
            if e2 <= dx { err += dx; sy0 += stepy; }
        }
    };
    if left {
        line(buf, cx + 4, cy - 6, cx - 4, cy);
        line(buf, cx - 4, cy,     cx + 4, cy + 6);
    } else {
        line(buf, cx - 4, cy - 6, cx + 4, cy);
        line(buf, cx + 4, cy,     cx - 4, cy + 6);
    }
}

/// Returns Some(true) if the prev-arrow button was hit, Some(false) if next.
fn palette_header_hit(origin: (i32, i32), (cx, cy): (i32, i32)) -> Option<bool> {
    let (px0, py0, px1, py1) = palette_header_button_rect(origin, true);
    if cx >= px0 && cx < px1 && cy >= py0 && cy < py1 { return Some(true); }
    let (nx0, ny0, nx1, ny1) = palette_header_button_rect(origin, false);
    if cx >= nx0 && cx < nx1 && cy >= ny0 && cy < ny1 { return Some(false); }
    None
}

/// Small dark pill carrying a button label, drawn near where the user's
/// cursor is hovering. Uses the in-tree 5×7 bitmap font at scale 2 so it
/// stays consistent with the dimension chip and palette header.
fn draw_tooltip(
    buf:   &mut [u32],
    win_w: usize,
    win_h: usize,
    label: &str,
    x:     i32,
    y:     i32,
) {
    let scale = 2;
    let text_w = crate::bitmap_font::measure(label, scale);
    let text_h = crate::bitmap_font::GLYPH_H * scale;
    let pad_x  = 6;
    let pad_y  = 4;
    let chip_w = text_w + pad_x * 2;
    let chip_h = text_h + pad_y * 2;
    // Auto-flip so the chip stays on screen.
    let mut x0 = x;
    let mut y0 = y;
    if x0 + chip_w > win_w as i32 { x0 = (win_w as i32) - chip_w - 4; }
    if y0 + chip_h > win_h as i32 { y0 = (win_h as i32) - chip_h - 4; }
    if x0 < 0 { x0 = 4; }
    if y0 < 0 { y0 = 4; }
    let x1 = x0 + chip_w;
    let y1 = y0 + chip_h;
    draw_filled_rect(buf, win_w, win_h, x0, y0, x1, y1, 0x00_10_10_14);
    draw_rect_border(buf, win_w, win_h, x0, y0, x1, y1, 0x00_4A_4A_50);
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    crate::painter::draw_text(&mut surf, x0 + pad_x, y0 + pad_y, scale, label, kashot_core::color::Rgba::WHITE);
}

/// Magnifier lens. Samples the original screenshot in a (2·R+1)² window
/// around the cursor and draws each source pixel as a `MAG_ZOOM`-sized
/// square. Adds a 1-px border + crosshair through the center pixel.
/// Auto-flips position so it never falls off the screen edge.
fn draw_magnifier(
    buf:    &mut [u32],
    win_w:  usize,
    win_h:  usize,
    shot:   &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    cursor: (i32, i32),
) {
    let chip = MAG_SIZE + 4;          // includes border
    let mut x0 = cursor.0 + MAG_OFFSET;
    let mut y0 = cursor.1 + MAG_OFFSET;
    if x0 + chip > win_w as i32 { x0 = cursor.0 - MAG_OFFSET - chip; }
    if y0 + chip > win_h as i32 { y0 = cursor.1 - MAG_OFFSET - chip; }
    if x0 < 0 || y0 < 0 { return; }   // not enough room either way

    let shot_w = shot.width()  as i32;
    let shot_h = shot.height() as i32;

    // Background fill (kept opaque so the lens stays readable on dark
    // shots) + 1-px white border.
    draw_filled_rect(buf, win_w, win_h, x0, y0, x0 + chip, y0 + chip, 0x00_10_10_14);
    draw_rect_border(buf, win_w, win_h, x0, y0, x0 + chip, y0 + chip, 0x00_FF_FF_FF);

    let inner_x = x0 + 2;
    let inner_y = y0 + 2;
    for sy in 0..MAG_PIXELS {
        for sx in 0..MAG_PIXELS {
            let src_x = cursor.0 + sx - MAG_RADIUS;
            let src_y = cursor.1 + sy - MAG_RADIUS;
            let px = if src_x >= 0 && src_x < shot_w && src_y >= 0 && src_y < shot_h {
                let p = shot.get_pixel(src_x as u32, src_y as u32).0;
                ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | p[2] as u32
            } else { 0x00_00_00_00 };
            let dx = inner_x + sx * MAG_ZOOM;
            let dy = inner_y + sy * MAG_ZOOM;
            draw_filled_rect(buf, win_w, win_h, dx, dy, dx + MAG_ZOOM, dy + MAG_ZOOM, px);
        }
    }

    // Crosshair through the center pixel — a 1-px red plus inside the lens
    // makes the exact source pixel obvious.
    let cx = inner_x + MAG_RADIUS * MAG_ZOOM;
    let cy = inner_y + MAG_RADIUS * MAG_ZOOM;
    let red = 0x00_DC_26_26;
    draw_filled_rect(buf, win_w, win_h, inner_x, cy, inner_x + MAG_SIZE, cy + 1, red);
    draw_filled_rect(buf, win_w, win_h, cx, inner_y, cx + 1, inner_y + MAG_SIZE, red);
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

/// Width × height chip rendered just outside the bottom-right corner of the
/// selection. Uses the existing 5×7 digit font (via `painter::draw_number`)
/// and a tiny hand-drawn `×` glyph between the two numbers. Background is a
/// 75 %-opaque dark fill so it stays legible on light or dark screenshots.
fn draw_dimension_chip(
    buf: &mut [u32], stride: usize, height: usize,
    anchor_x: i32, anchor_y: i32, w: u32, h: u32,
) {
    const SCALE:  i32 = 2;
    const PAD_X:  i32 = 6;
    const PAD_Y:  i32 = 4;
    const X_GLYPH_W: i32 = 5 * SCALE;
    const GAP:    i32 = SCALE * 2;

    let glyph_h    = 7 * SCALE;
    let digits_w_w = digit_count(w) as i32 * 5 * SCALE + (digit_count(w) as i32 - 1).max(0) * SCALE;
    let digits_h_w = digit_count(h) as i32 * 5 * SCALE + (digit_count(h) as i32 - 1).max(0) * SCALE;
    let inner_w    = digits_w_w + GAP + X_GLYPH_W + GAP + digits_h_w;
    let chip_w     = inner_w + PAD_X * 2;
    let chip_h     = glyph_h + PAD_Y * 2;

    // Place chip just inside the selection's bottom-right corner. Flip
    // outward if it would clip the screen edge.
    let mut x0 = anchor_x - chip_w - 4;
    let mut y0 = anchor_y - chip_h - 4;
    if x0 < 0 { x0 = anchor_x + 4; }
    if y0 < 0 { y0 = anchor_y + 4; }
    let x1 = x0 + chip_w;
    let y1 = y0 + chip_h;

    // 75 %-opaque dark fill — no real alpha blend on the u32 buffer, so just
    // mix toward the existing pixel by 1/4. This also keeps the chip from
    // wiping out the screenshot underneath.
    let xa = x0.max(0) as usize;
    let xb = (x1.min(stride as i32) as usize).max(xa);
    let ya = y0.max(0) as usize;
    let yb = (y1.min(height as i32) as usize).max(ya);
    for y in ya..yb.min(height) {
        for x in xa..xb.min(stride) {
            let dst = buf[y * stride + x];
            let dr = (dst >> 16) & 0xFF;
            let dg = (dst >> 8)  & 0xFF;
            let db =  dst        & 0xFF;
            // src = 0x16191D, weight 192/256.
            let r = (dr * 64 + 0x16 * 192) / 256;
            let g = (dg * 64 + 0x19 * 192) / 256;
            let b = (db * 64 + 0x1D * 192) / 256;
            buf[y * stride + x] = (r << 16) | (g << 8) | b;
        }
    }

    // Render the digits + 'x' separator using the painter via a tiny inline
    // U32Surface (the painter alpha-blends, which leaves the chip background
    // visible underneath the strokes — that's intentional).
    let mut surf = crate::painter::U32Surface { buf, stride: stride as i32, height: height as i32 };
    let text_y = y0 + PAD_Y;
    let mut cx = x0 + PAD_X;
    crate::painter::draw_number(&mut surf, cx, text_y, SCALE, w, kashot_core::color::Rgba::WHITE);
    cx += digits_w_w + GAP;
    draw_x_glyph(&mut surf, cx, text_y, SCALE);
    cx += X_GLYPH_W + GAP;
    crate::painter::draw_number(&mut surf, cx, text_y, SCALE, h, kashot_core::color::Rgba::WHITE);
}

fn digit_count(mut n: u32) -> u32 {
    if n == 0 { return 1; }
    let mut c = 0u32; while n > 0 { c += 1; n /= 10; } c
}

/// Tiny `×` drawn as two diagonal lines through a 5-wide × 7-tall cell. Same
/// scale convention as `draw_number`.
fn draw_x_glyph(surf: &mut crate::painter::U32Surface, x: i32, y: i32, scale: i32) {
    use kashot_core::color::Rgba;
    let w = 5 * scale;
    let h = 7 * scale;
    crate::painter::line(surf, x, y + scale, x + w - 1, y + h - scale - 1, scale as f32, Rgba::WHITE);
    crate::painter::line(surf, x + w - 1, y + scale, x, y + h - scale - 1, scale as f32, Rgba::WHITE);
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

