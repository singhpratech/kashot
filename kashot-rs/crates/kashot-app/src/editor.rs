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
//!   9. on Esc: never destroy work on the first press. With ink on the
//!      canvas the first Esc arms a confirmation bar; only a second Esc
//!      (or Enter) goes through with it, and even then the annotations
//!      are pushed onto the undo stack as one `EditOp::Clear`, so a
//!      single Ctrl+Z brings the whole session back — selection included.
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
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use image::{ImageBuffer, Rgba};
use kashot_core::annotation::{Annotation, Point2, Stroke};
use kashot_core::dpi::DisplayMap;
use kashot_core::edit;
use kashot_core::history::{EditOp, History};
use kashot_core::settings::AppSettings;
use kashot_core::state::{hit_test_edge_scaled, Edge};
use kashot_core::tool::Tool;
use kashot_core::virtual_desktop::{self as vdesk, DesktopGeometry};
use softbuffer::{Context, Surface};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey, SmolStr};
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
    /// Video-annotate commit: the raw annotation list plus the committed
    /// background frame. Composition into per-window overlays is deferred
    /// to the burn worker thread (`compose_overlay_groups`) because each
    /// distinct window start costs a synchronous ffmpeg seek for its
    /// pristine background — that must not stall the event loop. Replaces
    /// `Accepted` for video sessions so the screenshot save path keeps
    /// its single-bitmap payload.
    AcceptedVideo(VideoCommit),
    /// User picked a region to **record** rather than to capture: either the
    /// Record button in the action row of a normal capture session, or Enter /
    /// Record in a dedicated record-mode overlay (`new_for_region_record`).
    /// Carries `(x, y, w, h)` in window pixels — the caller clamps it to the
    /// desktop and hands it to the recorder. No bitmap: nothing is saved.
    RecordRegion((i32, i32, i32, i32)),
}

/// One video-burn group: an annotations-only transparent overlay plus its
/// visibility window in clip seconds. `end == None` = until the clip ends.
pub type OverlayGroup = (ImageBuffer<Rgba<u8>, Vec<u8>>, f32, Option<f32>);

/// Deferred video-commit payload carried by `AcceptedVideo`: everything
/// `compose_overlay_groups` needs to build the per-window overlays on the
/// burn worker thread instead of inside `commit`.
pub struct VideoCommit {
    /// Annotations in draw order, each carrying its visibility window.
    pub annotations: Vec<Annotation>,
    /// The background frame displayed at commit — reused as the pristine
    /// frame for groups whose window starts at the committed scrub spot,
    /// and as the fallback when a group's own extraction fails.
    pub frame: ImageBuffer<Rgba<u8>, Vec<u8>>,
    /// Timestamp `frame` was extracted at (the editor's `scrub_frame_t`).
    pub frame_t: f32,
    /// Parsed clip length, for nudging per-group extraction times inside
    /// the clip. `None` when the Duration banner didn't parse.
    pub duration: Option<f32>,
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
    /// Select mode: the user grabbed an existing annotation and is
    /// dragging it. Mouse-move shifts the annotation under the cursor,
    /// mouse-up records one undoable `EditOp::Move`.
    MovingAnnotation,
}

/// What a pending Esc confirmation would throw away if the user goes
/// through with it. Set by the first Esc, cleared by any key that isn't
/// Esc / Enter — see `handle_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingDiscard {
    /// Screenshot mode with a live selection: clearing drops the region
    /// and the ink in it (recoverable with Ctrl+Z).
    ClearSelection,
    /// Closing the overlay outright. Nothing survives this one.
    CloseOverlay,
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
/// Stroke widths the thickness button cycles through. The editor seeds
/// `stroke.thickness` from index 1 (4 px) in `Overlay::build` rather than
/// taking `Stroke::default().thickness`, which is off-list — starting off
/// the list makes the first click on the Thickness button skip to the
/// nearest entry above instead of advancing one step.
const THICKNESSES: [f32; 3] = [2.0, 4.0, 8.0];

/// Frame interval for the overlay's self-driven animations (the tool-panel
/// glow and the action-panel attention pulse). Nothing in the event stream
/// drives those, so `redraw` asks for its own next frame — paced to the
/// same ~30 Hz the tray loop polls at, because re-arming unconditionally
/// makes winit hand back the next redraw immediately and spins a core.
const ANIM_FRAME: std::time::Duration = std::time::Duration::from_millis(33);

// ── marker-opacity slider geometry ──────────────────────────────────────────
// Tucked underneath the tool panel and only painted when `Tool::Marker` is
// the active tool. Sized to comfortably fit a 140-px-wide track plus a "XX%"
// readout without exceeding the available screen width on either side of the
// selection. Anchored to the same X column as the tool panel so it feels
// like a satellite of the Marker button rather than a free-floating widget.
const MARKER_SLIDER_W:        i32 = 168;
const MARKER_SLIDER_H:        i32 = 38;
const MARKER_SLIDER_GAP:      i32 = 6;
const MARKER_SLIDER_PAD:      i32 = 8;
const MARKER_SLIDER_TRACK_H:  i32 = 16;
const MARKER_SLIDER_KNOB_W:   i32 = 14;
const MARKER_SLIDER_LABEL_W:  i32 = 34;

// ── video timeline bar geometry ─────────────────────────────────────────────
// Bottom-center scrub strip, video mode only. Holds (left→right) a
// "current / total" clock, the seek track with playhead + per-annotation
// start ticks, and the annotation-duration chip. Sized so a 720-px bar
// fits comfortably under a full-frame selection without covering the
// action panel, and shrinks with the window on small screens.
const TIMELINE_MAX_W:   i32 = 720;
const TIMELINE_H:       i32 = 40;
const TIMELINE_MARGIN:  i32 = 16;   // min gap to the screen edges
const TIMELINE_PAD:     i32 = 10;
const TIMELINE_TRACK_H: i32 = 10;
const TIMELINE_KNOB_W:  i32 = 8;
const TIMELINE_CHIP_W:  i32 = 44;
const TIMELINE_CHIP_H:  i32 = 24;

/// Annotation-duration presets the chip cycles through. `None` = visible
/// until the clip ends. The 5×7 bitmap font is ASCII-only (non-ASCII
/// falls back to '?'), so the "infinity" preset is spelled "End".
const DURATION_CHOICES: [Option<f32>; 4] = [None, Some(3.0), Some(5.0), Some(10.0)];

fn duration_chip_label(idx: usize) -> &'static str {
    match idx % DURATION_CHOICES.len() { 0 => "End", 1 => "3s", 2 => "5s", _ => "10s" }
}

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
enum ActionButton { Pin, Copy, Save, Record, Close }

/// Screenshot session: the region the user just dragged can be saved, copied,
/// pinned — or handed straight to the recorder, which is the same rectangle
/// they already picked and saves them dragging it twice.
const ACTION_BUTTONS: [ActionButton; 5] = [
    ActionButton::Pin, ActionButton::Copy, ActionButton::Save,
    ActionButton::Record, ActionButton::Close,
];

/// Video-annotate session: recording a region of a frame that is itself a
/// recording is meaningless, so Record is left out rather than left dead.
const VIDEO_ACTION_BUTTONS: [ActionButton; 4] = [
    ActionButton::Pin, ActionButton::Copy, ActionButton::Save, ActionButton::Close,
];

/// Record-mode session: selection only. No annotation tools, and nothing to
/// save — the rectangle is the entire output of the session.
const RECORD_ACTION_BUTTONS: [ActionButton; 2] = [
    ActionButton::Record, ActionButton::Close,
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
    /// Logical <-> physical mapping for `screenshot`. The overlay works
    /// entirely in captured-bitmap pixels, which are device pixels — the same
    /// units winit reports the cursor and the window size in — so nothing in
    /// here needs converting. The map is what turns a bitmap rect back into a
    /// desktop coordinate for anything outside the overlay (pin placement, a
    /// recording region), and what tells the editor how big a device pixel is
    /// so hit targets stay physically the same size on a high-DPI panel.
    /// Defaults to a 1x identity map, which is the un-scaled desktop.
    display:     DisplayMap,
    /// Virtual-desktop geometry of `screenshot`: where the stitched capture
    /// sits in virtual-screen space and which monitors it was built from.
    /// Video sessions carry a monitor-less `DesktopGeometry::bitmap`, which
    /// keeps every mapping below an identity.
    desktop:     DesktopGeometry,
    /// Virtual-screen coordinates of framebuffer pixel (0, 0). Equal to
    /// `desktop.origin()` when the window landed where we asked; re-derived
    /// from the window itself on every redraw by `sync_frame_origin`.
    frame_origin: (i32, i32),
    /// Set once we've reported a window manager placing the overlay
    /// somewhere other than the virtual-desktop origin — the mapping copes,
    /// but it's worth exactly one line in the log and not one per frame.
    placement_logged: bool,
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
    /// Undo/redo log over `annotations`. Holds `EditOp`s rather than bare
    /// annotations so moving and deleting existing ink is reversible too;
    /// adding a new annotation clears the redo side, same convention as
    /// `Kashot/OverlayForm.cs`.
    history:     History,
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
    /// On X11, set true once we've called XSetInputFocus + XGrabKeyboard
    /// against the overlay's window. Done lazily on the first redraw so
    /// the window is already mapped by the time we make the X requests.
    /// Without this Cinnamon never delivers KeyPress to the overlay and
    /// the Text tool sees no characters.
    focus_pushed:  bool,
    /// How many times we've tried to push X11 focus. Capped — see
    /// `push_x11_focus`. Without a cap, WMs that refuse focus entirely
    /// (GNOME focus-stealing prevention; some tiling WMs) make us spin
    /// requesting redraws forever and pin a CPU core.
    focus_attempts: u32,
    /// When the action panel was first shown (transition into `Selected`).
    /// Drives the laser-green attention pulse that fades over 3 s so users
    /// find the panel after it auto-positions for any screen size.
    panel_pulse_started: Option<std::time::Instant>,
    /// Was the previous redraw in `Selected` state? Used to detect the
    /// transition that triggers `panel_pulse_started`.
    last_was_selected: bool,
    /// In-flight IME composition for the pending Text annotation. Shown after
    /// the caret while the user composes but never written into the
    /// annotation — only `Ime::Commit` does that.
    ime_preedit: String,
    /// Wall-clock when the overlay window opened. Drives the slow
    /// sequential orange-neon glow that cycles through every tool-panel
    /// button top-to-bottom (mirrors the website's tool-palette demo).
    opened_at: std::time::Instant,
    /// Live `AppSettings` snapshot for the editor session. Most fields are
    /// read-only from here; `marker_opacity` is mutated by the per-tool
    /// slider below the panel and persisted on mouseup (and surfaced back
    /// to `TrayApp` via `marker_opacity()` when the overlay closes so the
    /// next capture session paints at the same alpha).
    settings: AppSettings,
    /// True while the user is left-dragging the Marker opacity knob. Same
    /// pattern as `SettingsView::dragging_opacity`: we follow CursorMoved
    /// until `MouseInput::Released` flips it off, at which point the new
    /// value is flushed to `settings.json`.
    dragging_marker_opacity: bool,
    /// True when annotating a video frame (`new_for_video`): the selection
    /// is locked to the full frame, Esc cancels outright, and `commit`
    /// returns the deferred `VideoCommit` payload instead of a cropped
    /// composite.
    video_mode: bool,
    /// True when the overlay was opened purely to pick a rectangle to record
    /// (`new_for_region_record`): the tool panel, the annotation tools and the
    /// Save / Copy / Pin actions are all absent, Enter or the Record button
    /// confirms, and Esc backs out. Everything else — the drag, the edge
    /// handles, the magnifier, the dimension chip — is the ordinary selector.
    record_mode: bool,
    /// Video-annotate context: the clip path, so the editor itself can
    /// re-extract frames when the user scrubs. `None` in screenshot mode.
    video_path: Option<PathBuf>,
    /// Clip length in seconds from the ffmpeg banner. `None` when parsing
    /// failed (broken file) — the timeline hides entirely and the session
    /// degrades to the pre-timeline static whole-clip editor.
    duration: Option<f32>,
    /// Current scrub position in clip seconds. New annotations stamp it
    /// as their window start; the background frame tracks it.
    scrub_pos: f32,
    /// Timestamp `screenshot` was actually extracted at — skips redundant
    /// ffmpeg spawns when a seek lands on the already-displayed frame.
    scrub_frame_t: f32,
    /// True while the user is left-dragging the timeline playhead. Same
    /// pattern as `dragging_marker_opacity`.
    dragging_playhead: bool,
    /// Last mid-drag frame swap. Each swap is a synchronous ffmpeg spawn,
    /// so mid-drag extraction is rate-limited to one per 200 ms; press
    /// and release always extract so the resting frame is exact.
    last_scrub_extract: Option<std::time::Instant>,
    /// Index into `DURATION_CHOICES` for the chip (default 0 = "End").
    duration_choice: usize,
    /// Cached pass-1 composite: the capture with everything outside the
    /// selection dimmed. Depends only on the window size, the selection
    /// rect and the captured frame, so it survives across frames and gets
    /// copied into the softbuffer instead of being re-shaded pixel by
    /// pixel — the animated chrome on top would otherwise re-dim the whole
    /// screen ~30 times a second.
    dim_cache: Vec<u32>,
    /// Key the cache above was built for:
    /// `(win_w, win_h, sel_rect, bitmap_offset)`.
    /// `None` forces a rebuild — that's how `swap_scrub_frame` invalidates
    /// the cache when it swaps the background to a new video frame.
    dim_cache_key: Option<(usize, usize, Option<(i32, i32, i32, i32)>, (i32, i32))>,
    /// When the last self-driven animation frame was presented. Paces the
    /// redraw re-arm to `ANIM_FRAME`.
    last_anim_frame: Option<std::time::Instant>,
    /// Select mode (S): clicks pick an existing annotation instead of
    /// starting a new one. Any tool key or tool button leaves it.
    select_mode: bool,
    /// Index into `annotations` of the annotation the user selected in
    /// select mode. Cleared by every undo/redo — indices shift under them.
    selected_idx: Option<usize>,
    /// Cursor position the running move drag was last sampled at.
    move_last: (i32, i32),
    /// Total offset the running move drag has accumulated, recorded as one
    /// `EditOp::Move` on mouse-up so undo reverses the whole drag.
    move_total: (f32, f32),
    /// Armed Esc confirmation, if any. Non-`None` means the confirmation
    /// bar is on screen and the next Esc / Enter goes through with it.
    pending_discard: Option<PendingDiscard>,
    /// Selection rect at the moment the canvas was last cleared. Undoing
    /// the clear restores it, so a mis-hit Esc costs nothing at all.
    last_selection: Option<(i32, i32, i32, i32)>,
}

impl Drop for Overlay {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        release_x11_focus();
    }
}

impl Overlay {
    /// Open the fullscreen overlay window for the given screenshot.
    ///
    /// `settings` is taken by value so the editor can mutate its own copy
    /// without racing the tray's `AppSettings`. Per-tool widgets (currently
    /// just the Marker opacity slider) update the local copy live and call
    /// `settings.save()` on commit; the tray reads `marker_opacity()` back
    /// when the overlay closes to keep its in-memory copy in sync for the
    /// next capture.
    ///
    /// `desktop` is the geometry the screenshot was stitched from
    /// (`Captured::geometry`): it places the overlay across the whole
    /// virtual desktop and maps every selection back to the right pixels.
    pub fn new(
        loop_target: &ActiveEventLoop,
        desktop: DesktopGeometry,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
        settings: AppSettings,
    ) -> Result<Self> {
        Self::build(loop_target, screenshot, desktop, settings, false, None, None)
    }

    /// Open the editor to annotate a video frame instead of a screenshot.
    /// The selection is pre-locked to the full frame (no cropping, no
    /// re-selecting, no edge-resize) and `commit` returns the annotation
    /// list + committed frame (see `compose_overlay_groups`) that the
    /// caller turns into annotations-only transparent overlays and burns
    /// over the clip with ffmpeg. A bottom-center timeline
    /// bar lets the user scrub the clip and stamp each annotation with a
    /// visibility window (`duration_secs` feeds it; `None` hides the bar
    /// and keeps the static whole-clip behavior). Copy/Pin still work —
    /// they produce an annotated *still* of the frame, which is useful on
    /// its own and keeps the action panel free of dead buttons.
    pub fn new_for_video(
        loop_target: &ActiveEventLoop,
        frame: ImageBuffer<Rgba<u8>, Vec<u8>>,
        settings: AppSettings,
        video: PathBuf,
        duration_secs: Option<f32>,
    ) -> Result<Self> {
        // A video frame is not a screen capture: it is drawn at framebuffer
        // (0, 0) whatever monitor the window ends up on, so it gets a
        // monitor-less geometry and every coordinate mapping stays identity.
        let desktop = DesktopGeometry::bitmap(frame.width(), frame.height());
        Self::build(loop_target, frame, desktop, settings, true, Some(video), duration_secs)
    }

    /// Open the overlay as a **recording** region selector.
    ///
    /// Same window, same drag, same edge handles as a capture session — the
    /// user shouldn't have to learn a second selector — but with the drawing
    /// half switched off, because nothing drawn here could ever end up in the
    /// video. Confirming returns `OverlayOutcome::RecordRegion`.
    pub fn new_for_region_record(
        loop_target: &ActiveEventLoop,
        desktop: DesktopGeometry,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
        settings: AppSettings,
    ) -> Result<Self> {
        let mut me = Self::build(loop_target, screenshot, desktop, settings, false, None, None)?;
        me.record_mode = true;
        Ok(me)
    }

    fn build(
        loop_target: &ActiveEventLoop,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
        desktop: DesktopGeometry,
        settings: AppSettings,
        video_mode: bool,
        video_path: Option<PathBuf>,
        duration_secs: Option<f32>,
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
        let desktop_origin = desktop.origin();
        let desktop_size   = desktop.size();
        let attrs = WindowAttributes::default()
            .with_title(if video_mode { "KAShot — annotate recording" } else { "KAShot" })
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_window_icon(crate::brand_icon::shared());

        // Multi-monitor: the overlay has to cover the *union* of every
        // screen, because a region dragged on a monitor left of / above the
        // primary one starts at a negative virtual-screen coordinate and a
        // primary-monitor-sized window can't even show it.
        //
        // One window spanning the union is the shape winit supports the same
        // way on all three platforms: there is no "fullscreen across all
        // monitors" mode (`Fullscreen::Borderless` takes a single monitor
        // handle), and a window per monitor would mean one softbuffer
        // surface and one event stream each, with a selection drag that
        // starts in one window and ends in another — the cursor leaves the
        // first window, so the drag would need pointer grabs that behave
        // differently on X11, Windows and macOS. A borderless always-on-top
        // window positioned at the union origin is placed natively by
        // Windows and by X11 WMs; macOS honours it too unless "Displays have
        // separate Spaces" is on, in which case the window is clamped to one
        // display — `sync_frame_origin` re-derives the mapping from where
        // the window actually landed, so the crop still matches what the
        // user selected instead of silently sliding to another monitor.
        let attrs = if desktop.spans_multiple_monitors() {
            attrs
                .with_inner_size(winit::dpi::PhysicalSize::new(
                    desktop_size.0.max(1), desktop_size.1.max(1)))
                .with_position(PhysicalPosition::new(desktop_origin.0, desktop_origin.1))
        } else {
            // Single monitor rooted at (0, 0): unchanged from before —
            // `Fullscreen::Borderless(None)` opens at a default size on
            // Cinnamon (~800×600), so the explicit size + position + fullscreen
            // request together make the WM open us at full screen.
            attrs
                .with_inner_size(monitor_size)
                .with_position(PhysicalPosition::new(0i32, 0i32))
                .with_fullscreen(Some(Fullscreen::Borderless(primary)))
        };

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window: {e}"))?;

        // Several WMs apply size/position only once the window is mapped and
        // ignore what the create request asked for. Re-assert both; the
        // request is a no-op when the window already covers the desktop.
        if desktop.spans_multiple_monitors() {
            window.set_outer_position(PhysicalPosition::new(desktop_origin.0, desktop_origin.1));
            let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(
                desktop_size.0.max(1), desktop_size.1.max(1)));
        }

        window.set_cursor(CursorIcon::Crosshair);
        window.focus_window();

        // `focus_window()` above is the spec-correct ask via _NET_ACTIVE_WINDOW.
        // Cinnamon ignores it fast enough that the overlay never gets KeyPress,
        // so we follow up with a manual XSetInputFocus + XGrabKeyboard from
        // `push_x11_focus` once the window is viewable.

        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new: {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new: {e}"))?;

        // Video mode opens with the whole frame locked in as the selection
        // so every tool is immediately usable and nothing can be cropped.
        let (full_w, full_h) = (screenshot.width() as i32, screenshot.height() as i32);
        Ok(Overlay {
            display: DisplayMap::identity(full_w.max(0) as u32, full_h.max(0) as u32),
            screenshot,
            desktop,
            frame_origin: desktop_origin,
            placement_logged: false,
            window,
            _ctx:        ctx,
            surface,
            state:       if video_mode { State::Selected } else { State::Idle },
            cursor:      (0, 0),
            anchor:      (0, 0),
            selection:   if video_mode { Some((0, 0, full_w, full_h)) } else { None },
            tool:        Tool::Pen,
            // Seeded from the cycle list so the Thickness button steps
            // 4 -> 8 -> 2 from a known position (see `THICKNESSES`).
            stroke:      Stroke { thickness: THICKNESSES[1], ..Stroke::default() },
            annotations: Vec::new(),
            history:     History::new(),
            current:     None,
            step_count:  1,
            mods:        ModifiersState::empty(),
            resize_edge: Edge::None,
            palette_open: false,
            palette_index: 0,
            hover_tip:     None,
            focus_pushed:  false,
            focus_attempts: 0,
            panel_pulse_started: None,
            last_was_selected:   false,
            ime_preedit:         String::new(),
            opened_at:           std::time::Instant::now(),
            settings,
            dragging_marker_opacity: false,
            video_mode,
            record_mode: false,
            video_path,
            duration: duration_secs,
            scrub_pos: 0.0,
            scrub_frame_t: 0.0,
            dragging_playhead: false,
            last_scrub_extract: None,
            duration_choice: 0,
            dim_cache: Vec::new(),
            dim_cache_key: None,
            last_anim_frame: None,
            select_mode: false,
            selected_idx: None,
            move_last: (0, 0),
            move_total: (0.0, 0.0),
            pending_discard: None,
            last_selection: None,
        })
    }

    /// Hand the overlay the capture's logical/physical mapping. Separate from
    /// `new` so the constructor's signature stays put; called right after the
    /// window opens. A map whose physical size doesn't match the bitmap we
    /// were handed is ignored — the 1x identity the constructor installed is
    /// always consistent with the frame, and a mismatched map would move the
    /// crop instead of correcting it.
    pub fn set_display_map(&mut self, map: DisplayMap) {
        if map.physical_size() == (self.screenshot.width(), self.screenshot.height()) {
            self.display = map;
        }
    }

    /// Device pixels per logical desktop unit for the captured frame.
    fn display_scale(&self) -> f32 { self.display.scale() }

    /// Read-only view of the editor's live settings — used by `tray_loop`
    /// after the overlay closes to pull back any per-tool slider values
    /// (currently just `marker_opacity`) the user changed mid-session.
    pub fn settings(&self) -> &AppSettings { &self.settings }

    /// Snap `settings.marker_opacity` to the cursor's X position inside the
    /// slider track. Cursor X is clamped to the track so dragging past the
    /// end pegs the value at 0 / 255 instead of wrapping. Step is 1 unit
    /// out of 256 so every pixel of the track represents a distinct value.
    fn set_marker_opacity_from_cursor(&mut self) {
        let Some(sel) = self.selection else { return; };
        let Some(panel) = marker_slider_rect(self.panel_bounds(sel), sel) else { return; };
        let (tx, _ty, tw, _th) = marker_slider_track(panel);
        if tw <= 1 { return; }
        let cx = self.cursor.0;
        let mut t = (cx - tx) as f32 / (tw - 1) as f32;
        if !t.is_finite() { t = 0.0; }
        t = t.clamp(0.0, 1.0);
        let v = (t * 255.0).round() as i32;
        self.settings.marker_opacity = v.clamp(0, 255) as u8;
        self.window.request_redraw();
    }

    /// Snap `scrub_pos` to the cursor's X inside the timeline track,
    /// clamped to [0, duration]. `force_extract` bypasses the mid-drag
    /// throttle so press and release always land the background on the
    /// exact frame; mid-drag swaps are rate-limited because each one is
    /// a synchronous ffmpeg spawn.
    fn set_scrub_from_cursor(&mut self, force_extract: bool) {
        let Some(dur) = self.duration else { return; };
        let bar = timeline_bar_rect(self.chrome_bounds(self.cursor));
        let (tx, _ty, tw, _th) = timeline_track(bar, dur);
        if tw <= 1 { return; }
        let mut t = (self.cursor.0 - tx) as f32 / (tw - 1) as f32;
        if !t.is_finite() { t = 0.0; }
        self.scrub_pos = t.clamp(0.0, 1.0) * dur;
        let throttle_ok = self.last_scrub_extract
            .map_or(true, |i| i.elapsed() >= std::time::Duration::from_millis(200));
        if force_extract || throttle_ok {
            self.swap_scrub_frame();
        }
        self.window.request_redraw();
    }

    /// Replace the background with the frame at the current scrub
    /// position. Failures keep the previous frame — a stale background
    /// beats killing the session mid-drag.
    fn swap_scrub_frame(&mut self) {
        let (Some(dur), Some(video)) = (self.duration, self.video_path.as_deref()) else { return; };
        self.last_scrub_extract = Some(std::time::Instant::now());
        let t = clamp_extract_t(self.scrub_pos, dur);
        if t == self.scrub_frame_t { return; }
        match crate::tray_loop::extract_frame_at(video, t) {
            Ok(frame) => {
                self.screenshot    = frame;
                self.scrub_frame_t = t;
                // The dimmed composite is built from the old frame.
                self.dim_cache_key = None;
            }
            Err(e) => eprintln!("kashot: scrub frame extract failed: {e}"),
        }
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    // ── virtual-desktop coordinate mapping ─────────────────────────────
    //
    // Mouse events, annotations and every painter call are in *frame*
    // (framebuffer) space. The capture is in *bitmap* space, and the pin
    // window wants *virtual-screen* space. `kashot_core::virtual_desktop`
    // owns the arithmetic; these four helpers feed it the overlay's state.

    /// Re-derive the framebuffer's virtual-screen origin from the window.
    ///
    /// Called at the top of every redraw. When the window manager honours
    /// the requested placement this is a no-op — but when it doesn't (macOS
    /// with separate Spaces per display, a WM that refuses negative
    /// positions), it is what keeps the crop, the pin position and the
    /// magnifier pointed at the pixels the user is actually looking at.
    /// Skipped for non-capture bitmaps (video frames), which are drawn at
    /// framebuffer (0, 0) no matter which monitor the window opens on.
    fn sync_frame_origin(&mut self) {
        if self.desktop.monitors().is_empty() { return; }
        // Wayland refuses to report window position at all; the fallback
        // there is the origin we asked for, same as before.
        let pos = self.window.inner_position().or_else(|_| self.window.outer_position());
        let Ok(p) = pos else { return; };
        if (p.x, p.y) != self.frame_origin {
            self.frame_origin = (p.x, p.y);
            // Pass 1 samples the capture through this offset.
            self.dim_cache_key = None;
        }
        if self.frame_origin != self.desktop.origin() && !self.placement_logged {
            self.placement_logged = true;
            let (ox, oy) = self.desktop.origin();
            eprintln!(
                "kashot: overlay placed at {},{} but the desktop starts at {ox},{oy} — \
                 mapping selections through the difference",
                self.frame_origin.0, self.frame_origin.1
            );
        }
    }

    /// Offset of framebuffer pixel (0, 0) inside the captured bitmap.
    fn bitmap_offset(&self) -> (i32, i32) {
        vdesk::bitmap_offset(self.desktop.origin(), self.frame_origin)
    }

    /// The selection as a crop rect in the captured bitmap.
    fn selection_in_bitmap(&self) -> Option<(i32, i32, i32, i32)> {
        Some(vdesk::frame_rect_to_bitmap(
            self.desktop.origin(), self.frame_origin, self.selection?))
    }

    /// Framebuffer-space rect the floating chrome lays itself out inside:
    /// the monitor under `point`, clipped to the framebuffer. Keeps the
    /// tool / action panels, the magnifier and the hint chip on the monitor
    /// the user is working on instead of letting them flip onto the one
    /// next door, which is where clamping against the whole virtual desktop
    /// would put them.
    fn chrome_bounds(&self, point: (i32, i32)) -> (i32, i32, i32, i32) {
        let size = self.window.inner_size();
        vdesk::monitor_bounds_in_frame(
            self.desktop.monitors(), self.frame_origin, (size.width, size.height), point)
    }

    /// Chrome bounds for the widgets anchored to the selection: the monitor
    /// holding the selection's center.
    fn panel_bounds(&self, sel: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
        self.chrome_bounds((sel.0 + sel.2 / 2, sel.1 + sel.3 / 2))
    }

    /// The action row for this session. Three shapes, one panel — see the
    /// button-list constants for why each session drops what it drops.
    fn action_buttons(&self) -> &'static [ActionButton] {
        if self.record_mode      { &RECORD_ACTION_BUTTONS }
        else if self.video_mode  { &VIDEO_ACTION_BUTTONS  }
        else                     { &ACTION_BUTTONS        }
    }

    pub fn handle_event(&mut self, event: WindowEvent) -> OverlayOutcome {
        match event {
            WindowEvent::CloseRequested => self.request_close(),

            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                OverlayOutcome::Continue
            }

            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, text, state: ElementState::Pressed, .. }, ..
            } => self.handle_key_with_text(logical_key, text),

            // IME: dead keys, compose sequences, and full input methods all
            // arrive here rather than as `KeyEvent::text`. Only meaningful
            // while typing — `set_text_input_active` turns the IME on with
            // the Text tool and off again, so tool shortcuts stay single
            // keypresses the rest of the time.
            WindowEvent::Ime(ime) => self.handle_ime(ime),

            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                self.cursor = (x as i32, y as i32);
                // Marker opacity drag takes priority over per-state cursor
                // updates so the slider tracks the cursor smoothly even
                // when the user drags well outside the slider's own rect.
                if self.dragging_marker_opacity {
                    self.set_marker_opacity_from_cursor();
                    return OverlayOutcome::Continue;
                }
                if self.dragging_playhead {
                    self.set_scrub_from_cursor(false);
                    return OverlayOutcome::Continue;
                }
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
                    State::MovingAnnotation => {
                        if let Some(idx) = self.selected_idx {
                            let dx = (self.cursor.0 - self.move_last.0) as f32;
                            let dy = (self.cursor.1 - self.move_last.1) as f32;
                            if let Some(a) = self.annotations.get_mut(idx) {
                                edit::translate(a, dx, dy);
                            }
                            self.move_total.0 += dx;
                            self.move_total.1 += dy;
                        }
                        self.move_last = self.cursor;
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

    /// Route a keypress, preferring the platform's composed `text` over the
    /// logical key while the Text tool is capturing input.
    ///
    /// `KeyEvent::text` is what the OS decided this keystroke produces after
    /// layout, dead keys and modifiers — "é" from `'` + `e`, "ł" from AltGr+L,
    /// "Ω" from a Greek layout. `logical_key` is only the key's own identity,
    /// which is why the old ASCII path could never see those. Control
    /// characters (Enter, Tab, Backspace, Esc all arrive with a `text` of
    /// their own) are filtered out and fall through to `handle_key`, which
    /// keeps owning every command key.
    fn handle_key_with_text(&mut self, key: Key, text: Option<SmolStr>) -> OverlayOutcome {
        if self.state == State::TextInput && !self.mods.control_key() {
            if let Some(raw) = text.as_deref() {
                let typed = kashot_core::text::sanitize_input(raw);
                // Newlines only ever come from Enter, which `handle_key`
                // treats as "commit"; leave that decision there.
                if !typed.is_empty() && !typed.contains('\n') {
                    self.insert_text(&typed);
                    return OverlayOutcome::Continue;
                }
            }
        }
        self.handle_key(key)
    }

    /// Append user-typed text to the pending Text annotation.
    fn insert_text(&mut self, typed: &str) {
        use kashot_core::annotation::AnnotationKind;
        if typed.is_empty() { return; }
        if let Some(a) = self.current.as_mut() {
            if let AnnotationKind::Text { ref mut text, .. } = a.kind {
                text.push_str(typed);
                self.window.request_redraw();
            }
        }
    }

    /// Turn the window's IME on/off around a text-input session and clear any
    /// half-composed preedit. Enabling it unconditionally would let a CJK IME
    /// swallow the single-letter tool shortcuts, so it is scoped to typing.
    fn set_text_input_active(&mut self, active: bool) {
        self.ime_preedit.clear();
        self.window.set_ime_allowed(active);
    }

    fn handle_ime(&mut self, ime: Ime) -> OverlayOutcome {
        if self.state != State::TextInput {
            return OverlayOutcome::Continue;
        }
        match ime {
            // Committed text is final — append it exactly as the IME built it.
            Ime::Commit(s) => {
                self.ime_preedit.clear();
                let typed = kashot_core::text::sanitize_input(&s);
                self.insert_text(&typed);
            }
            // In-flight composition: shown after the caret, never stored in
            // the annotation, discarded if the user cancels.
            Ime::Preedit(s, _) => {
                self.ime_preedit = kashot_core::text::sanitize_input(&s);
                self.window.request_redraw();
            }
            Ime::Enabled | Ime::Disabled => {
                self.ime_preedit.clear();
                self.window.request_redraw();
            }
        }
        OverlayOutcome::Continue
    }

    fn handle_key(&mut self, key: Key) -> OverlayOutcome {
        eprintln!("kashot: key={:?} state={:?} mods={:?}", key, self.state, self.mods);
        // Text-input state owns the keyboard while it's active — typed
        // characters extend the pending annotation; Enter commits, Esc
        // cancels, Backspace pops the last char.
        if self.state == State::TextInput {
            return self.handle_text_key(key);
        }
        // An armed Esc confirmation owns the keyboard: Esc / Enter go
        // through with the discard, anything else calls it off. Bare
        // modifier presses are ignored so reaching for Ctrl doesn't count
        // as an answer.
        if self.pending_discard.is_some() && !is_modifier_key(&key) {
            return match key {
                Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter) => self.confirm_discard(),
                _ => {
                    self.pending_discard = None;
                    self.window.request_redraw();
                    OverlayOutcome::Continue
                }
            };
        }
        match key {
            Key::Named(NamedKey::Escape) => self.handle_escape(),
            Key::Named(NamedKey::Enter) => self.commit(),
            // Select mode: Delete / Backspace remove the picked annotation.
            // Both are undoable; outside select mode neither key does
            // anything, exactly as before.
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                self.delete_selected_annotation();
                OverlayOutcome::Continue
            }
            Key::Character(s) => {
                // Record mode has no tools to switch to and nothing to save,
                // copy or pin, so every character shortcut would be a lie.
                if self.record_mode {
                    return OverlayOutcome::Continue;
                }
                let c    = match s.chars().next() { Some(c) => c, None => return OverlayOutcome::Continue };
                let ctrl = self.mods.control_key();
                let shift = self.mods.shift_key();
                let lc    = c.to_ascii_lowercase();
                // Undo / redo stay live even with no selection on screen —
                // that is precisely the state a confirmed Esc leaves behind,
                // and Ctrl+Z is the way back from it.
                if ctrl && (lc == 'z' || lc == 'y') {
                    // Ctrl+Z → undo, Ctrl+Y / Ctrl+Shift+Z → redo.
                    if lc == 'y' || shift { self.redo(); } else { self.undo(); }
                    return OverlayOutcome::Continue;
                }
                if self.state != State::Selected {
                    return OverlayOutcome::Continue;
                }
                if ctrl {
                    match lc {
                        // Ctrl+S → commit-and-save (same as Enter)
                        's' => return self.commit(),
                        // Ctrl+C → commit-and-copy
                        'c' => return self.commit_as_copy(),
                        // Ctrl+P → commit-and-pin (float bitmap on screen)
                        'p' => return self.commit_as_pin(),
                        _ => {}
                    }
                } else if lc == 's' {
                    // S → select mode. No tool letter uses it (see
                    // `Tool::shortcut`), so nothing is shadowed.
                    self.select_mode = !self.select_mode;
                    if !self.select_mode { self.selected_idx = None; }
                    self.window.request_redraw();
                } else if let Some(t) = Tool::from_key(c) {
                    self.tool = t;
                    // Picking a drawing tool leaves select mode — otherwise
                    // the next click would move ink instead of drawing.
                    self.leave_select_mode();
                    self.window.request_redraw();
                }
                OverlayOutcome::Continue
            }
            _ => OverlayOutcome::Continue,
        }
    }

    /// Esc, outside text input. Ordered cheapest-cancel first: an
    /// in-progress stroke, then the select-mode highlight, then the mode
    /// itself, and only after all of those anything that would cost the
    /// user work — which never happens on this press, it only arms the
    /// confirmation bar.
    fn handle_escape(&mut self) -> OverlayOutcome {
        if self.state == State::Drawing {
            self.current = None;
            self.state   = State::Selected;
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        if self.state == State::MovingAnnotation {
            // Abort the drag by putting the annotation back where it was.
            // Nothing was recorded yet, so there is nothing to undo either.
            if let Some(idx) = self.selected_idx {
                if let Some(a) = self.annotations.get_mut(idx) {
                    edit::translate(a, -self.move_total.0, -self.move_total.1);
                }
            }
            self.move_total = (0.0, 0.0);
            self.state      = State::Selected;
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        if self.selected_idx.is_some() {
            self.selected_idx = None;
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        if self.select_mode {
            self.select_mode = false;
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        // Screenshot mode with a live selection: Esc clears the region and
        // the ink drawn in it. Video mode has no "no selection" state to
        // fall back to — the frame IS the selection — so there Esc closes.
        if self.state == State::Selected && !self.video_mode {
            if self.annotations.is_empty() {
                // Nothing to lose — clear straight away, as before.
                return self.confirm_discard_kind(PendingDiscard::ClearSelection);
            }
            self.pending_discard = Some(PendingDiscard::ClearSelection);
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        if self.has_unsaved_work() {
            self.pending_discard = Some(PendingDiscard::CloseOverlay);
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        OverlayOutcome::Cancelled
    }

    /// Go through with whatever the confirmation bar is asking about.
    fn confirm_discard(&mut self) -> OverlayOutcome {
        match self.pending_discard.take() {
            Some(kind) => self.confirm_discard_kind(kind),
            None       => OverlayOutcome::Continue,
        }
    }

    fn confirm_discard_kind(&mut self, kind: PendingDiscard) -> OverlayOutcome {
        self.pending_discard = None;
        match kind {
            PendingDiscard::ClearSelection => {
                self.discard_annotations_recoverably();
                self.state        = State::Idle;
                self.selection    = None;
                self.step_count   = 1;
                self.selected_idx = None;
                self.window.request_redraw();
                OverlayOutcome::Continue
            }
            PendingDiscard::CloseOverlay => OverlayOutcome::Cancelled,
        }
    }

    /// Is there annotation work in this session that closing would destroy?
    /// Counts the canvas, an in-progress annotation, and anything sitting
    /// in the undo/redo log — a cleared canvas is still one Ctrl+Z away.
    fn has_unsaved_work(&self) -> bool {
        !self.annotations.is_empty()
            || self.current.is_some()
            || self.history.holds_recoverable_work()
    }

    /// The Close button and the WM's close request both land here: they
    /// ask, they never destroy. With nothing to lose the overlay closes
    /// immediately, exactly as before.
    fn request_close(&mut self) -> OverlayOutcome {
        if self.has_unsaved_work() {
            self.pending_discard = Some(PendingDiscard::CloseOverlay);
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        OverlayOutcome::Cancelled
    }

    /// Leave select mode and drop any annotation highlight.
    fn leave_select_mode(&mut self) {
        self.select_mode  = false;
        self.selected_idx = None;
    }

    /// Index of the topmost annotation under the cursor. Video mode only
    /// considers ink that is actually visible at the scrub position — the
    /// user can't click what they can't see.
    fn hit_annotation_at_cursor(&self) -> Option<usize> {
        let p = Point2::new(self.cursor.0 as f32, self.cursor.1 as f32);
        if self.video_mode {
            self.annotations.iter().enumerate().rev()
                .find(|(_, a)| annotation_visible_at(a, self.scrub_pos) && edit::hit_test(a, p))
                .map(|(i, _)| i)
        } else {
            edit::hit_test_topmost(&self.annotations, p)
        }
    }

    /// Remove the annotation picked in select mode, recording it so both
    /// Ctrl+Z and Ctrl+Y replay the removal.
    fn delete_selected_annotation(&mut self) {
        let Some(idx) = self.selected_idx else { return; };
        if idx >= self.annotations.len() { self.selected_idx = None; return; }
        let removed = self.annotations.remove(idx);
        self.history.record(EditOp::Delete { index: idx, annotation: removed });
        self.selected_idx = None;
        self.step_count   = edit::next_step_number(&self.annotations);
        self.window.request_redraw();
    }

    fn handle_text_key(&mut self, key: Key) -> OverlayOutcome {
        use kashot_core::annotation::AnnotationKind;
        eprintln!("kashot: text-input key={:?}", key);
        match key {
            Key::Named(NamedKey::Escape) => {
                self.current = None;
                self.state   = State::Selected;
                self.set_text_input_active(false);
                self.window.request_redraw();
            }
            // Shift+Enter opens a second line; plain Enter commits.
            Key::Named(NamedKey::Enter) if self.mods.shift_key() => {
                self.insert_text("\n");
            }
            Key::Named(NamedKey::Enter) => {
                // Commit only if the user actually typed something.
                if let Some(a) = self.current.take() {
                    if let AnnotationKind::Text { ref text, .. } = a.kind {
                        if !text.trim().is_empty() {
                            self.add_annotation(a);
                        }
                    }
                }
                self.state = State::Selected;
                self.set_text_input_active(false);
                self.window.request_redraw();
            }
            Key::Named(NamedKey::Backspace) => {
                // A half-composed IME preedit is cancelled by the IME itself;
                // here Backspace removes one *visible* character, which on
                // combining or ZWJ sequences is several `char`s at once.
                if let Some(a) = self.current.as_mut() {
                    if let AnnotationKind::Text { ref mut text, .. } = a.kind {
                        kashot_core::text::pop_grapheme(text);
                        self.window.request_redraw();
                    }
                }
            }
            // Fallbacks for platforms/layouts where `KeyEvent::text` is
            // absent — `handle_key_with_text` handles the normal case.
            Key::Named(NamedKey::Space) => self.insert_text(" "),
            Key::Character(s) => {
                // Skip Ctrl-modified characters so Ctrl+Z / Ctrl+S etc. don't
                // get swallowed as plain text input.
                if self.mods.control_key() { return OverlayOutcome::Continue; }
                let typed = kashot_core::text::sanitize_input(&s);
                self.insert_text(&typed);
            }
            _ => {}
        }
        OverlayOutcome::Continue
    }

    fn undo(&mut self) {
        let Some(op) = self.history.undo(&mut self.annotations) else { return; };
        self.after_history_op();
        // Undoing the clear that Esc (or a click outside the region) did
        // brings the selection back with the ink, so the user lands exactly
        // where they were instead of having to re-drag a region first.
        if matches!(op, EditOp::Clear { .. })
            && self.state == State::Idle
            && self.selection.is_none()
            && !self.annotations.is_empty()
        {
            if let Some(rect) = self.last_selection {
                self.selection = Some(rect);
                self.state     = State::Selected;
            }
        }
    }

    fn redo(&mut self) {
        if self.history.redo(&mut self.annotations).is_some() {
            self.after_history_op();
        }
    }

    /// Shared bookkeeping after any undo/redo: step numbering follows the
    /// markers actually on the canvas, and the select-mode highlight is
    /// dropped because indices shift under inserts and removals.
    fn after_history_op(&mut self) {
        self.step_count   = edit::next_step_number(&self.annotations);
        self.selected_idx = None;
        self.window.request_redraw();
    }

    /// Drop the committed annotations off the canvas without destroying
    /// them: both ways of clearing a selection (a confirmed Esc, or
    /// clicking outside the region to start a fresh drag) used to throw the
    /// whole vector away, so one stray keypress could wipe minutes of work
    /// with no way back. The vector moves onto the undo stack as a single
    /// `EditOp::Clear` instead, so one Ctrl+Z restores every annotation —
    /// and `undo` puts the selection rect back with them.
    fn discard_annotations_recoverably(&mut self) {
        if self.annotations.is_empty() { return; }
        if self.selection.is_some() { self.last_selection = self.selection; }
        let cleared = std::mem::take(&mut self.annotations);
        self.history.record(EditOp::Clear { annotations: cleared });
        self.selected_idx = None;
    }

    fn handle_left_press(&mut self) -> OverlayOutcome {
        // An armed Esc confirmation owns the click the same way it owns
        // the keyboard: clicking Close again goes through with it, a click
        // anywhere else calls it off and is swallowed so the answer can
        // never double as a drawing gesture.
        if self.pending_discard.is_some() {
            let on_close = self.selection.map_or(false, |sel| {
                let lb = self.panel_bounds(sel);
                let buttons = self.action_buttons();
                action_panel_hit(action_panel_origin(lb, sel, buttons), self.cursor, buttons)
                    == Some(ActionButton::Close)
            });
            if on_close { return self.confirm_discard(); }
            self.pending_discard = None;
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        // Click anywhere while typing → commit the pending text and keep
        // going. Mirrors the C# TextBox-loses-focus behaviour.
        if self.state == State::TextInput {
            use kashot_core::annotation::AnnotationKind;
            if let Some(a) = self.current.take() {
                if let AnnotationKind::Text { ref text, .. } = a.kind {
                    if !text.trim().is_empty() {
                        self.add_annotation(a);
                    }
                }
            }
            self.state = State::Selected;
            self.set_text_input_active(false);
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
                let lb = self.panel_bounds(sel);
                let tp_origin = tool_panel_origin(lb, sel);

                // Color popup — must be tested before the tool panel itself
                // so a click that falls on the popup doesn't get eaten by
                // the panel underneath.
                if self.palette_open {
                    let pp_origin = palette_popup_origin(lb, tp_origin);
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

                // Tool panel — absent in record mode, so it can't take clicks
                // there either. The palette, the marker slider and the timeline
                // are already unreachable in that mode (no Color button, no
                // tool letter keys, not a video session).
                if let Some((_, btn)) = tool_panel_hit(tp_origin, self.cursor)
                    .filter(|_| !self.record_mode)
                {
                    match btn {
                        ToolPanelButton::Tool(t) => { self.tool = t; self.leave_select_mode(); }
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
                let ap_origin = action_panel_origin(lb, sel, self.action_buttons());
                if let Some(action) = action_panel_hit(ap_origin, self.cursor, self.action_buttons()) {
                    return match action {
                        ActionButton::Pin    => self.commit_as_pin(),
                        ActionButton::Copy   => self.commit_as_copy(),
                        ActionButton::Save   => self.commit(),
                        ActionButton::Record => self.commit_as_record(),
                        ActionButton::Close  => self.request_close(),
                    };
                }

                // Marker opacity slider — only visible when Marker is the
                // active tool. A click anywhere inside the panel jumps the
                // value to the cursor X and arms drag-tracking until the
                // user releases the mouse button (mirrors the watermark
                // opacity slider in `settings_form.rs`).
                if self.tool == Tool::Marker {
                    if let Some(panel) = marker_slider_rect(lb, sel) {
                        if marker_slider_hit(panel, self.cursor) {
                            self.dragging_marker_opacity = true;
                            self.set_marker_opacity_from_cursor();
                            return OverlayOutcome::Continue;
                        }
                    }
                }

                // Video timeline + duration chip. Gated on `video_mode`
                // so the bar can never steal clicks in the screenshot
                // editor, and on a known duration so a broken clip keeps
                // the old static behavior.
                if self.video_mode && self.duration.is_some() {
                    let bar = timeline_bar_rect(lb);
                    if rect_contains(timeline_chip_rect(bar), self.cursor) {
                        self.duration_choice = (self.duration_choice + 1) % DURATION_CHOICES.len();
                        self.window.request_redraw();
                        return OverlayOutcome::Continue;
                    }
                    if rect_contains(bar, self.cursor) {
                        // Click-to-seek + arm drag-tracking until mouseup,
                        // mirroring the marker-opacity slider.
                        self.dragging_playhead = true;
                        self.set_scrub_from_cursor(true);
                        return OverlayOutcome::Continue;
                    }
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
                // clicking there grabs that edge for resizing. Video mode
                // locks the selection to the full frame: resizing it would
                // desync the annotation coordinates from the video, so the
                // edges are not grabbable there. Select mode gives the edge
                // band back to hit-testing so ink drawn against the border
                // is still reachable.
                if let Some(sel) = self.selection.filter(|_| !self.video_mode && !self.select_mode) {
                    let hit = hit_test_edge_scaled(
                        (sel.0 as f32, sel.1 as f32, sel.2 as f32, sel.3 as f32),
                        (self.cursor.0 as f32, self.cursor.1 as f32),
                        self.display_scale(),
                    );
                    if hit.is_some() {
                        self.state       = State::Resizing;
                        self.resize_edge = hit;
                        self.window.request_redraw();
                        return OverlayOutcome::Continue;
                    }
                }
                // Select mode intercepts the click before any drawing
                // starts: hit-test the ink under the cursor, pick the
                // topmost match and arm a move-drag. A click that lands on
                // nothing just drops the highlight — it never starts a new
                // region, so ink can't be lost by a stray click here.
                if self.select_mode {
                    self.selected_idx = self.hit_annotation_at_cursor();
                    if self.selected_idx.is_some() {
                        self.state      = State::MovingAnnotation;
                        self.move_last  = self.cursor;
                        self.move_total = (0.0, 0.0);
                    }
                    self.window.request_redraw();
                    return OverlayOutcome::Continue;
                }
                if self.cursor_in_selection() {
                    // Nothing to draw in record mode — a click inside the
                    // rectangle is a no-op rather than a stray annotation the
                    // video would never carry anyway.
                    if self.record_mode {
                        return OverlayOutcome::Continue;
                    }
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
                        let p  = Point2::new(self.cursor.0 as f32, self.cursor.1 as f32);
                        // Text size rides the thickness cycle, so the same
                        // button that fattens a pen stroke steps text through
                        // three sizes without a control of its own.
                        let px = kashot_core::text::font_size_for_thickness(self.stroke.thickness);
                        self.current = Some(Annotation::text_sized(self.stroke.color, p, "", px));
                        self.state   = State::TextInput;
                        self.set_text_input_active(true);
                        // Park the IME candidate window on the caret so a
                        // CJK/compose popup doesn't cover what's being typed.
                        self.window.set_ime_cursor_area(
                            PhysicalPosition::new(p.x as i32, p.y as i32),
                            winit::dpi::PhysicalSize::new(1u32, px.ceil() as u32),
                        );
                        eprintln!("kashot: entered TextInput at ({}, {})", p.x, p.y);
                        self.window.request_redraw();
                    } else if let Some(a) = self.start_annotation() {
                        self.current = Some(a);
                        self.state   = State::Drawing;
                        self.window.request_redraw();
                    }
                } else {
                    // Start a new selection if the click was outside. The
                    // old region's ink is stashed BEFORE the selection is
                    // replaced, so undoing the clear restores the rect the
                    // annotations were drawn in, not the fresh empty one.
                    self.discard_annotations_recoverably();
                    self.state     = State::Selecting;
                    self.anchor    = self.cursor;
                    self.selection = Some((self.cursor.0, self.cursor.1, 0, 0));
                    self.step_count = 1;
                    self.window.request_redraw();
                }
            }
            _ => {}
        }
        OverlayOutcome::Continue
    }

    fn add_annotation(&mut self, mut a: Annotation) {
        // Video mode stamps the visibility window at commit time: start =
        // the current scrub position, end = start + the chip preset. The
        // untouched default (scrub 0 + "End") stays `None`, keeping the
        // zero-interaction session structurally identical to the static
        // whole-clip behavior.
        if self.video_mode {
            a.time = self.annotation_window();
        }
        self.annotations.push(a.clone());
        self.history.record(EditOp::Add { index: self.annotations.len() - 1, annotation: a });
    }

    /// The window a new annotation gets at the current scrub position +
    /// chip preset (see `stamp_window`). `None` = whole clip.
    fn annotation_window(&self) -> Option<(f32, f32)> {
        let dur = self.duration?;
        stamp_window(self.scrub_pos, dur,
                     DURATION_CHOICES[self.duration_choice % DURATION_CHOICES.len()])
    }

    fn handle_left_release(&mut self) -> OverlayOutcome {
        // End any active marker-opacity drag and persist the new value to
        // disk so the next session paints at the same alpha. Best-effort:
        // a save failure here is logged but never aborts the editor (same
        // contract as `AppSettings::save` everywhere else).
        if self.dragging_marker_opacity {
            self.dragging_marker_opacity = false;
            if let Err(e) = self.settings.save() {
                eprintln!("kashot: marker opacity save failed: {e}");
            }
        }
        if self.dragging_playhead {
            self.dragging_playhead = false;
            // Final frame swap so the background matches exactly where
            // the drag ended (mid-drag swaps are throttled).
            self.set_scrub_from_cursor(true);
        }
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
            State::MovingAnnotation => {
                self.state = State::Selected;
                // One `EditOp::Move` per drag, not per mouse-move event, so
                // a single Ctrl+Z puts the annotation back where it started.
                if let Some(idx) = self.selected_idx {
                    if self.move_total != (0.0, 0.0) {
                        self.history.record(EditOp::Move {
                            index: idx,
                            dx:    self.move_total.0,
                            dy:    self.move_total.1,
                        });
                    }
                }
                self.move_total = (0.0, 0.0);
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
        let lb  = self.panel_bounds(sel);
        // Tool panel — never drawn in record mode, so never hoverable either.
        let tp = tool_panel_origin(lb, sel);
        if let Some((idx, btn)) = tool_panel_hit(tp, self.cursor).filter(|_| !self.record_mode) {
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
        let ap = action_panel_origin(lb, sel, self.action_buttons());
        if let Some(btn) = action_panel_hit(ap, self.cursor, self.action_buttons()) {
            let label = match btn {
                ActionButton::Pin    => "Pin to screen",
                ActionButton::Copy   => "Copy (Ctrl+C)",
                ActionButton::Save   => "Save (Ctrl+S)",
                ActionButton::Record => "Record region (Enter)",
                ActionButton::Close  => "Close (Esc)",
            };
            return Some((label, self.cursor.0 + 14, self.cursor.1 + 14));
        }
        // Marker-only opacity slider — only present when the Marker tool
        // is active. Tooltip cues the user that the widget is a slider
        // (some users may not recognise the chrome on first sight).
        if self.tool == Tool::Marker {
            if let Some(panel) = marker_slider_rect(lb, sel) {
                if marker_slider_hit(panel, self.cursor) {
                    return Some(("Marker opacity", self.cursor.0 + 14, self.cursor.1 + 14));
                }
            }
        }
        // Video timeline — same gates as the click handler so the tip can
        // only ever appear where a click would actually land.
        if self.video_mode && self.duration.is_some() {
            let bar = timeline_bar_rect(lb);
            if rect_contains(timeline_chip_rect(bar), self.cursor) {
                return Some(("Annotation duration", self.cursor.0 + 14, self.cursor.1 - 24));
            }
            if rect_contains(bar, self.cursor) {
                return Some(("Seek", self.cursor.0 + 14, self.cursor.1 - 24));
            }
        }
        None
    }

    fn update_resize_cursor(&self) {
        // Select mode has no edge-resize: the cursor reports whether there
        // is ink under it to grab instead.
        if self.select_mode {
            let icon = if self.hit_annotation_at_cursor().is_some() {
                CursorIcon::Move
            } else {
                CursorIcon::Default
            };
            self.window.set_cursor(icon);
            return;
        }
        let Some(sel) = self.selection else { return; };
        let hit = hit_test_edge_scaled(
            (sel.0 as f32, sel.1 as f32, sel.2 as f32, sel.3 as f32),
            (self.cursor.0 as f32, self.cursor.1 as f32),
            self.display_scale(),
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
            Tool::Marker    => Annotation::marker(self.stroke, p, self.settings.marker_opacity),
            Tool::Pixelate  => Annotation::pixelate(p),
            // Step is handled inline at click site (no `Drawing` state).
            Tool::Step      => return None,
            // Text enters its own `TextInput` state instead of `Drawing` —
            // also handled inline.
            Tool::Text      => return None,
        })
    }

    fn commit(&mut self) -> OverlayOutcome {
        // Record mode: Enter starts a recording of the rectangle instead of
        // saving a screenshot of it.
        if self.record_mode {
            return self.commit_as_record();
        }
        // Video mode: AcceptedVideo carries the raw annotation list plus
        // the committed background frame. The tray loop's burn worker
        // composes the per-window overlays from it (`compose_overlay_
        // groups`) — each distinct window start spawns ffmpeg for its
        // pristine frame, which would freeze the event loop here.
        if self.video_mode {
            if self.state != State::Selected { return OverlayOutcome::Continue; }
            return OverlayOutcome::AcceptedVideo(VideoCommit {
                annotations: self.annotations.clone(),
                frame:       self.screenshot.clone(),
                frame_t:     self.scrub_frame_t,
                duration:    self.duration,
            });
        }
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

    /// Hand the locked-in rectangle back as a recording region.
    ///
    /// Unlike every other commit path this composes no bitmap: the output of
    /// the session is the rectangle itself. Virtual-screen (device) pixels,
    /// lifted out of frame space the same way the pin position is — the
    /// caller clamps them to the desktop and rounds them for the encoder.
    fn commit_as_record(&mut self) -> OverlayOutcome {
        if self.state != State::Selected { return OverlayOutcome::Continue; }
        match self.selection {
            Some(rect) => OverlayOutcome::RecordRegion(
                vdesk::frame_rect_to_virtual(self.frame_origin, rect)),
            None       => OverlayOutcome::Continue,
        }
    }

    fn commit_as_pin(&mut self) -> OverlayOutcome {
        // `PinView` positions its window in absolute device pixels, which the
        // selection is only trivially equal to on a single 1x screen at the
        // virtual origin. `frame_origin` is the framebuffer's device-pixel
        // origin (the display map's physical origin, re-derived from the
        // window), so lifting the selection through it lands the pin over
        // the region it was cut from on a scaled or offset desktop — on a
        // monitor left of / above the primary one that is a negative position.
        // `PinView` positions its window in virtual-screen coordinates, so
        // the selection has to be lifted out of frame space — on a monitor
        // left of / above the primary one that is a negative position.
        let pos = match self.selection {
            Some((x, y, _, _)) => vdesk::frame_to_virtual(self.frame_origin, (x, y)),
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
        // The selection is in frame space; the capture is in bitmap space.
        // On a multi-monitor desktop those differ by the virtual-desktop
        // origin, which is what used to make a region picked on a monitor
        // left of / above the primary one crop the wrong pixels.
        let mut img = crop(&self.screenshot, self.selection_in_bitmap()?);
        // Snapshot the un-annotated crop FIRST so pixelate's source-sampling
        // stays idempotent under draw-order: pixelate must always sample the
        // original screenshot, never something we already painted on.
        // Mirrors C# `PixelateAnnotation`.
        let pristine = img.clone();
        let dx = -rect.0 as f32;
        let dy = -rect.1 as f32;
        let mut surf = ImageSurface(&mut img);
        for a in &self.annotations {
            // Video-mode Copy/Pin produce a still of exactly what is on
            // screen: only ink visible at the scrub position is burned.
            // Screenshot mode carries no windows so this never skips.
            if self.video_mode && !annotation_visible_at(a, self.scrub_pos) { continue; }
            let translated = translate_annotation(a, dx, dy);
            painter::render_annotation(&mut surf, &translated, Some(&pristine));
        }
        Some(img)
    }

    /// Force keyboard focus + grab to our window via raw X11 calls. Retries
    /// on every redraw until SetInputFocus stops returning BadMatch — the
    /// window may still be in `IsUnviewable` state on the first frame and
    /// X11 rejects focus changes against unviewable windows. Cinnamon
    /// usually maps the window within ~2-3 redraw cycles after which the
    /// focus push succeeds.
    #[cfg(target_os = "linux")]
    fn push_x11_focus(&mut self) {
        if self.focus_pushed { return; }
        const MAX_FOCUS_ATTEMPTS: u32 = 60;
        self.focus_attempts = self.focus_attempts.saturating_add(1);
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let xid = match self.window.window_handle() {
            Ok(h) => match h.as_raw() {
                RawWindowHandle::Xlib(x) => x.window as u32,
                _ => { self.focus_pushed = true; return; }
            }
            Err(_) => { self.focus_pushed = true; return; }
        };
        match force_x11_focus(xid) {
            Ok(()) => {
                eprintln!("kashot: x11 focus + grab pushed for window 0x{xid:x}");
                self.focus_pushed = true;
            }
            Err(e) => {
                if self.focus_attempts >= MAX_FOCUS_ATTEMPTS {
                    eprintln!(
                        "kashot: gave up pushing x11 focus after {} attempts ({e}); \
                         Text tool may not receive keypresses on this WM",
                        self.focus_attempts
                    );
                    self.focus_pushed = true;
                } else {
                    eprintln!("kashot: x11 focus retry pending ({e})");
                }
            }
        }
    }

    fn redraw(&mut self) {
        // Where the window actually landed decides every coordinate mapping
        // below, so re-read it before anything is drawn or sampled.
        self.sync_frame_origin();
        #[cfg(target_os = "linux")]
        {
            self.push_x11_focus();
            // Keep cycling redraws until focus is grabbed. After that it
            // settles and we redraw only on actual events.
            if !self.focus_pushed {
                self.window.request_redraw();
            }
        }
        // Bound before the framebuffer is borrowed: the list is `'static`, but
        // reading it off `self` mid-draw would alias the surface borrow.
        let actions = self.action_buttons();
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) else { return; };
        // Chrome bounds have to be resolved before the framebuffer is
        // borrowed — they read `self`, the buffer borrows `self.surface`.
        let cursor_bounds = self.chrome_bounds(self.cursor);
        let sel_bounds    = self.selection.map(|sel| self.panel_bounds(sel));
        // Offset of framebuffer (0, 0) inside the capture: zero whenever the
        // overlay covers the virtual desktop as asked.
        let shot_off      = self.bitmap_offset();
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

        // Pass 1: screenshot + dim outside selection. Cached — see
        // `dim_cache`: the shading depends only on the window size, the
        // selection rect and the captured frame, none of which move on an
        // animation frame, so a full-screen memcpy replaces a full-screen
        // per-pixel multiply on every frame but the first.
        let cache_key = (win_w, win_h, sel_rect, shot_off);
        if self.dim_cache_key != Some(cache_key) || self.dim_cache.len() != win_w * win_h {
            self.dim_cache.clear();
            self.dim_cache.resize(win_w * win_h, 0);
            let cache = &mut self.dim_cache;
            for y in 0..win_h {
                for x in 0..win_w {
                    let dst_idx = y * win_w + x;
                    // Frame pixel -> capture pixel. Off-capture pixels (a
                    // gap between differently-sized monitors, or a window
                    // the WM pushed past the capture) paint black.
                    let sx = x as i32 + shot_off.0;
                    let sy = y as i32 + shot_off.1;
                    let (r, g, b) = if sx >= 0 && sy >= 0
                        && (sx as usize) < shot_w && (sy as usize) < shot_h {
                        let src = (sy as usize * shot_w + sx as usize) * 4;
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
                    cache[dst_idx] = (rr << 16) | (gg << 8) | bb;
                }
            }
            self.dim_cache_key = Some(cache_key);
        }
        buf.copy_from_slice(&self.dim_cache);

        // Pass 2: annotations, clipped to the selection. We render into the
        // shared u32 buffer through `U32Surface`. Bounds-clipping happens at
        // the per-pixel level inside the painter so we don't have to manage
        // a scissor here, but we still skip when there's no selection.
        let mut surf = U32Surface { buf: &mut buf, stride: win_w as i32, height: win_h as i32 };
        for a in &self.annotations {
            // Timeline preview: only ink whose window contains the scrub
            // position is visible. Screenshot mode carries no windows.
            if self.video_mode && !annotation_visible_at(a, self.scrub_pos) { continue; }
            painter::render_annotation_offset(&mut surf, a, Some(&self.screenshot), shot_off);
        }
        if let Some(a) = self.current.as_ref() {
            painter::render_annotation_offset(&mut surf, a, Some(&self.screenshot), shot_off);
        }

        // While typing — show a subtle dashed rectangle around the text
        // bounds so the user can see where text will appear. Drawn only
        // in the live preview; `compose_final` never renders it, so the
        // saved/copied bitmap shows just the text without any frame.
        if self.state == State::TextInput {
            if let Some(a) = self.current.as_ref() {
                if let kashot_core::annotation::AnnotationKind::Text { color, position, text, font_size } = &a.kind {
                    // Frame + caret are measured with the same rasterizer that
                    // paints the glyphs, so they track accents, wide scripts
                    // and multi-line text exactly.
                    let px = *font_size;
                    // The preedit is drawn but not stored, so it has to be
                    // measured as if it were already appended.
                    let composed = if self.ime_preedit.is_empty() {
                        None
                    } else {
                        Some(format!("{text}{}", self.ime_preedit))
                    };
                    let shown: &str = composed.as_deref().unwrap_or(text);
                    let block = kashot_core::text::layout(shown, px);
                    if !self.ime_preedit.is_empty() {
                        // Composition is provisional — draw it dimmed after
                        // the committed text so the user can tell them apart.
                        let mut surf = U32Surface { buf: &mut buf, stride: win_w as i32, height: win_h as i32 };
                        let preedit_color = color.with_alpha(0x99);
                        painter::blit_text_block(&mut surf, position.x as i32, position.y as i32,
                                                 &block, preedit_color);
                    }
                    let (caret_x_f, caret_y_f) = block.caret();
                    let text_w = block.width().ceil() as i32;
                    let text_h = block.height().ceil() as i32;
                    let pad = 4;
                    let x0 = position.x as i32 - pad;
                    let y0 = position.y as i32 - pad;
                    let x1 = position.x as i32 + text_w.max(2) + pad;
                    let y1 = position.y as i32 + text_h + pad;
                    draw_dashed_border(&mut buf, win_w, win_h, x0, y0, x1, y1, 0x00_88_88_8C);
                    // Solid 1-px caret on the last line, at the trailing edge.
                    let caret_x  = position.x as i32 + caret_x_f.round() as i32 + 1;
                    let caret_y0 = position.y as i32 + caret_y_f.round() as i32;
                    let caret_y1 = caret_y0 + block.line_height.round() as i32;
                    for cy in caret_y0..caret_y1 {
                        if caret_x >= 0 && (caret_x as usize) < win_w && cy >= 0 && (cy as usize) < win_h {
                            buf[cy as usize * win_w + caret_x as usize] = 0x00_E8_E8_EC;
                        }
                    }
                }
            }
        }

        // Pass 2.5: select-mode highlight — a dashed laser-green box around
        // the picked annotation's painted bounds, with solid corner ticks so
        // it reads as "grabbed" rather than as another region rectangle.
        // Preview only: `compose_final` never draws it, so the saved bitmap
        // carries the ink alone.
        if let Some(idx) = self.selected_idx {
            if let Some(a) = self.annotations.get(idx) {
                const HILITE: u32 = 0x00_00_FF_95;      // laser green
                let b   = kashot_core::edit::bounds(a);
                let pad = 5;
                let x0  = b.x as i32 - pad;
                let y0  = b.y as i32 - pad;
                let x1  = (b.x + b.w) as i32 + pad;
                let y1  = (b.y + b.h) as i32 + pad;
                draw_dashed_border(&mut buf, win_w, win_h, x0, y0, x1, y1, HILITE);
                let tick = 4;
                for &(hx, hy) in &[(x0, y0), (x1 - tick, y0), (x0, y1 - tick), (x1 - tick, y1 - tick)] {
                    draw_filled_rect(&mut buf, win_w, win_h, hx, hy, hx + tick, hy + tick, HILITE);
                }
            }
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
                let lb = sel_bounds.unwrap_or((0, 0, win_w as i32, win_h as i32));
                draw_dimension_chip(&mut buf, win_w, win_h, lb, x + w, y + h, w as u32, h as u32);
            }
        }

        // Pass 5: tool panel + action panel + (optional) color popup —
        // floating around the selection, never spanning the screen.
        let is_selected_now = matches!(self.state, State::Selected);
        if is_selected_now && !self.last_was_selected {
            self.panel_pulse_started = Some(std::time::Instant::now());
        }
        self.last_was_selected = is_selected_now;

        if matches!(self.state,
                    State::Selected | State::Drawing | State::Resizing
                    | State::TextInput | State::MovingAnnotation) {
            if let Some(sel) = self.selection {
                let lb = sel_bounds.unwrap_or((0, 0, win_w as i32, win_h as i32));
                // Record mode is selection-only: no tool column, no glow, no
                // palette, no per-tool widgets. Just the rectangle and the
                // two-button action row.
                if !self.record_mode {
                    draw_tool_panel(&mut buf, win_w, win_h, lb, sel,
                                    self.tool, self.stroke.color, self.stroke.thickness);
                    // Sequential orange-neon halo cycles through every button
                    // (top→bottom, 1 per slot). Mirrors the website's tool-palette
                    // demo so the in-app palette feels alive even when the user
                    // is just hovering. Painted after the panel so the halo sits
                    // on top of the button background but under the icon glyph.
                    let elapsed = self.opened_at.elapsed().as_secs_f32();
                    draw_tool_panel_glow(&mut buf, win_w, win_h, lb, sel, elapsed);
                }
                draw_action_panel(&mut buf, win_w, win_h, lb, sel, actions);
                // Attention pulse on the action panel for the first 3 s after
                // it appears — the panel auto-positions based on selection +
                // screen, so first-time users can lose track of where Save /
                // Copy / Pin landed. Fades smoothly to nothing.
                if matches!(self.state, State::Selected) {
                    if let Some(start) = self.panel_pulse_started {
                        let elapsed = start.elapsed().as_secs_f32();
                        if elapsed < 3.0 {
                            draw_action_panel_pulse(&mut buf, win_w, win_h, lb, sel, elapsed, actions);
                        } else {
                            self.panel_pulse_started = None;
                        }
                    }
                }
                if self.palette_open && !self.record_mode {
                    let tp_origin = tool_panel_origin(lb, sel);
                    draw_palette_popup(&mut buf, win_w, win_h, lb, tp_origin,
                                       self.stroke.color, self.palette_index);
                }
                // Marker opacity slider — appears only while the Marker
                // tool is the active selection. The slider track shows the
                // marker color at the chosen opacity so the user previews
                // exactly what their next stroke will look like.
                if self.tool == Tool::Marker && !self.record_mode {
                    if let Some(panel) = marker_slider_rect(lb, sel) {
                        draw_marker_opacity_slider(
                            &mut buf, win_w, win_h, panel,
                            self.stroke.color, self.settings.marker_opacity,
                        );
                    }
                }
            }
        }

        // Pass 5.5: idle hint — before any selection exists the overlay is
        // just a dimmed desktop with a lens on it, which says nothing about
        // dragging a region or about the way back out. One dim line spells
        // both out; it disappears the moment the drag starts.
        if matches!(self.state, State::Idle) {
            draw_idle_hint(&mut buf, win_w, win_h, cursor_bounds,
                           if self.record_mode { RECORD_HINT } else { IDLE_HINT });
        }

        // Pass 6: magnifier — only useful when the user is positioning the
        // selection edge by individual pixels. Once a region is locked in,
        // the toolbar+palette take over and the lens just gets in the way.
        // Drawn after the hint so the lens is never covered by it.
        if matches!(self.state, State::Idle | State::Selecting) {
            draw_magnifier(&mut buf, win_w, win_h, cursor_bounds,
                           &self.screenshot, self.cursor, shot_off);
        }

        // Pass 6.5: video timeline — video mode only, and only when the
        // clip length is known (a broken Duration banner hides the bar
        // and the session behaves like the static whole-clip editor).
        if self.video_mode {
            if let Some(dur) = self.duration {
                let ticks: Vec<f32> = self.annotations.iter()
                    .filter_map(|a| a.time.map(|(s, _)| s))
                    .collect();
                draw_timeline_bar(&mut buf, win_w, win_h, cursor_bounds, self.scrub_pos, dur,
                                  duration_chip_label(self.duration_choice), &ticks);
            }
        }

        // Pass 7: tooltip chip — only when the user is hovering a button
        // in `Selected`. Mirrors C# `MakeButton(tip, ...)` behaviour.
        if let Some((label, x, y)) = self.hover_tip {
            if matches!(self.state, State::Selected) {
                draw_tooltip(&mut buf, win_w, win_h, cursor_bounds, label, x, y);
            }
        }

        // Pass 8: the notice bar — the Esc confirmation, or the select-mode
        // cue when nothing needs confirming. Painted last so it is never
        // covered by a panel, a popup or the timeline.
        if let Some(kind) = self.pending_discard {
            let headline = match kind {
                PendingDiscard::ClearSelection =>
                    format!("Discard {} annotation{}?", self.annotations.len(),
                            if self.annotations.len() == 1 { "" } else { "s" }),
                PendingDiscard::CloseOverlay => "Close without saving?".to_string(),
            };
            draw_notice_bar(
                &mut buf, win_w, win_h, cursor_bounds,
                &[&headline, "Esc or Enter to discard  -  any other key to keep editing"],
                0x00_FF_B0_20, 0x00_FF_D8_8A,
            );
        } else if self.select_mode {
            // Once something is picked the cue shrinks to a single line —
            // the user is working under it and doesn't need the recipe any
            // more.
            let lines: &[&str] = if self.selected_idx.is_some() {
                &["Select mode", "drag to move  -  Del removes"]
            } else {
                &["Select mode", "click ink to move it  -  Del removes  -  S or a tool key exits"]
            };
            draw_notice_bar(&mut buf, win_w, win_h, cursor_bounds, lines, 0x00_00_FF_95, 0x00_C8_FF_E4);
        }

        if let Err(e) = buf.present() {
            eprintln!("overlay: buf.present: {e}");
        }

        // The tool-panel glow and the action-panel pulse have no event
        // behind them, so this frame has to ask for the next one. Only the
        // resting `Selected` state self-drives: while the user is drawing,
        // resizing or typing, their input already produces frames, and
        // pacing those would just add latency. Sleeping out the remainder
        // of the interval is what keeps this from becoming a hot loop —
        // winit hands back a redraw requested here the instant we return,
        // so without the wait the overlay renders as fast as the CPU
        // allows for an animation that only steps 30 times a second.
        if is_selected_now && self.selection.is_some() {
            let since = self.last_anim_frame.map_or(ANIM_FRAME, |t| t.elapsed());
            if since < ANIM_FRAME {
                std::thread::sleep(ANIM_FRAME - since);
            }
            self.last_anim_frame = Some(std::time::Instant::now());
            self.window.request_redraw();
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

fn action_panel_dims(buttons: &[ActionButton]) -> (i32, i32) {
    let n = buttons.len() as i32;
    let w = n * PANEL_BTN + (n - 1) * PANEL_GAP + PANEL_PAD * 2;
    let h = PANEL_BTN + PANEL_PAD * 2;
    (w, h)
}

/// Frame-space origin of the tool panel. Right of selection by default;
/// flips to the left if the right edge would clip; rounds inward to keep
/// the panel fully inside `bounds`.
///
/// `bounds` is the monitor the selection sits on (see `Overlay::panel_bounds`),
/// not the whole framebuffer — on a multi-monitor overlay clamping against
/// the framebuffer would let the panel drift onto the monitor next door
/// instead of flipping to the other side of the selection.
fn tool_panel_origin(bounds: (i32, i32, i32, i32), sel: (i32, i32, i32, i32)) -> (i32, i32) {
    let (bx, by, bw, bh) = bounds;
    let (sx, sy, sw, _sh) = sel;
    let (pw, ph) = tool_panel_dims();
    let mut tx = sx + sw + 5;
    let mut ty = sy;
    if tx + pw > bx + bw { tx = sx - pw - 5; }
    if ty + ph > by + bh { ty = by + bh - ph; }
    (tx.max(bx), ty.max(by))
}

/// Origin + size of the Marker-only opacity slider panel. Sits directly
/// below the tool panel sharing its X column. If the tool panel is on the
/// RIGHT of the selection the slider hangs off its left edge so the wider
/// slider doesn't push past the screen edge; on the LEFT it hangs off the
/// right edge for the same reason. Returns `None` only if the slider would
/// have to overflow horizontally on a screen narrower than ~180 px — in
/// which case the caller silently skips drawing.
fn marker_slider_rect(
    bounds: (i32, i32, i32, i32),
    sel:    (i32, i32, i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    let (bx, by, bw, bh) = bounds;
    let (tp_x, tp_y) = tool_panel_origin(bounds, sel);
    let (tp_w, tp_h) = tool_panel_dims();
    let (sw, _sh) = (sel.2, sel.3);
    // The tool panel sits to the RIGHT of the selection when it fits, LEFT
    // otherwise. The slider extends in the direction with more room.
    let panel_on_right = tp_x >= sel.0 + sw;
    let sx = if panel_on_right {
        // Right-of-selection panel — extend slider leftward so its right
        // edge lines up with the panel's right edge.
        tp_x + tp_w - MARKER_SLIDER_W
    } else {
        // Left-of-selection panel — extend slider rightward so its left
        // edge lines up with the panel's left edge.
        tp_x
    };
    let sy = tp_y + tp_h + MARKER_SLIDER_GAP;
    // Clamp into screen bounds; if there's no room below the panel, hop
    // above instead.
    let sx = sx.max(bx).min((bx + bw - MARKER_SLIDER_W).max(bx));
    let sy = if sy + MARKER_SLIDER_H > by + bh {
        (tp_y - MARKER_SLIDER_GAP - MARKER_SLIDER_H).max(by)
    } else { sy };
    Some((sx, sy, MARKER_SLIDER_W, MARKER_SLIDER_H))
}

/// Inside the slider panel, where the draggable track lives. Same source
/// of truth for hit-testing and rendering so the knob always coincides
/// with where the mouse clicks.
fn marker_slider_track(panel: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let (px, py, pw, ph) = panel;
    let tx = px + MARKER_SLIDER_PAD;
    let tw = pw - MARKER_SLIDER_PAD * 2 - MARKER_SLIDER_LABEL_W - 6;
    let ty = py + (ph - MARKER_SLIDER_TRACK_H) / 2;
    (tx, ty, tw, MARKER_SLIDER_TRACK_H)
}

fn marker_slider_hit(panel: (i32, i32, i32, i32), (cx, cy): (i32, i32)) -> bool {
    let (px, py, pw, ph) = panel;
    cx >= px && cx < px + pw && cy >= py && cy < py + ph
}

// ── video timeline bar layout (video mode only) ────────────────────────────

/// Point-in-rect test shared by the timeline bar + duration chip.
fn rect_contains((x, y, w, h): (i32, i32, i32, i32), (cx, cy): (i32, i32)) -> bool {
    cx >= x && cx < x + w && cy >= y && cy < y + h
}

/// Bottom-center timeline bar, video mode only. Centered inside `bounds`
/// so it lands on one monitor rather than straddling the seam between two.
fn timeline_bar_rect(bounds: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let (bx, by, bw, bh) = bounds;
    let w = TIMELINE_MAX_W.min(bw - TIMELINE_MARGIN * 2).max(120);
    let x = bx + (bw - w) / 2;
    let y = (by + bh - TIMELINE_MARGIN - TIMELINE_H).max(by);
    (x, y, w, TIMELINE_H)
}

/// Duration chip — right end of the bar, inside it, so it can never clip
/// the screen edge regardless of window size.
fn timeline_chip_rect(bar: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let (bx, by, bw, bh) = bar;
    (bx + bw - TIMELINE_PAD - TIMELINE_CHIP_W,
     by + (bh - TIMELINE_CHIP_H) / 2,
     TIMELINE_CHIP_W, TIMELINE_CHIP_H)
}

/// Seek track between the clock readout and the chip. The readout slot is
/// measured from the worst-case "total / total" label so the track does
/// not jitter as the current-time digits change while scrubbing.
fn timeline_track(bar: (i32, i32, i32, i32), total_secs: f32) -> (i32, i32, i32, i32) {
    let (bx, by, _bw, bh) = bar;
    let label   = format!("{0} / {0}", format_timecode(total_secs));
    let label_w = crate::bitmap_font::measure(&label, 2);
    let tx   = bx + TIMELINE_PAD + label_w + 10;
    let chip = timeline_chip_rect(bar);
    let tw   = (chip.0 - 8 - tx).max(40);
    let ty   = by + (bh - TIMELINE_TRACK_H) / 2;
    (tx, ty, tw, TIMELINE_TRACK_H)
}

/// "M:SS" clock readout (minutes unpadded; hours roll into minutes —
/// recordings are short).
fn format_timecode(secs: f32) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// `[start, end)` containment for the timeline preview. `None` = whole
/// clip; an `f32::INFINITY` end means "until the clip ends".
fn annotation_visible_at(a: &Annotation, t: f32) -> bool {
    match a.time { None => true, Some((s, e)) => t >= s && t < e }
}

/// Seeking at (or past) the clip's end yields no frame from ffmpeg, so
/// extraction times are nudged just inside the clip.
fn clamp_extract_t(t: f32, dur: f32) -> f32 { t.min((dur - 0.05).max(0.0)) }

/// Window stamped on a new annotation: start = the scrub position, end =
/// start + the chip preset. `None` = whole clip. The start is nudged
/// inside the clip with `clamp_extract_t` because the seek track clamps
/// to [0, dur] — a stamp at exactly `dur` would gate the burn on
/// gte(t, dur), which no frame ever reaches (the last frame's timestamp
/// is one frame-duration short of the container length), silently
/// dropping ink the preview showed on the displayed (already-nudged)
/// final frame. A preset that runs past the clip's end means the same as
/// "End" — open-ended, so the burn emits gte() instead of trusting the
/// parsed duration to the centisecond.
fn stamp_window(scrub_pos: f32, dur: f32, preset: Option<f32>) -> Option<(f32, f32)> {
    let start = clamp_extract_t(scrub_pos, dur);
    let end = match preset {
        Some(d) if start + d < dur => start + d,
        _ => f32::INFINITY,
    };
    if start == 0.0 && end == f32::INFINITY { return None; }
    Some((start, end))
}

/// Group annotations into runs of identical visibility windows —
/// consecutive only, so the chained overlays composite in exactly the
/// editor's draw order. Merging across runs would re-order ink: with
/// A(whole-clip) B(timed) C(whole-clip), folding C into A's group puts
/// B's overlay above C in the burn while the preview painted C on top.
/// Interleaved sequences cost one extra overlay input per run; the
/// zero-interaction default is still a single run, so the byte-identical
/// pre-timeline filtergraph is preserved.
fn group_by_window(annotations: &[Annotation]) -> Vec<(Option<(f32, f32)>, Vec<&Annotation>)> {
    let mut groups: Vec<(Option<(f32, f32)>, Vec<&Annotation>)> = Vec::new();
    for a in annotations {
        match groups.last_mut() {
            Some((k, list)) if *k == a.time => list.push(a),
            _ => groups.push((a.time, vec![a])),
        }
    }
    groups
}

/// Video-mode counterpart of `compose_final`: render the annotations into
/// fully-transparent RGBA buffers the size of the frame, leaving untouched
/// pixels transparent — one buffer per run of identical (start, end)
/// visibility windows. ffmpeg then overlays each buffer on the clip, gated
/// to its window with `enable=`.
///
/// The painter's `blend` pre-composites semi-transparent strokes against
/// whatever `read()` returns, so we can't render straight onto
/// transparency — Marker would come out as an opaque dark band. Instead
/// `DiffSurface` reads from (and writes to) a live copy of the frame — so
/// blending and Pixelate sampling behave exactly like the screenshot
/// editor — while mirroring every touched pixel, opaque, into the
/// transparent out-buffer. WYSIWYG with one caveat: Marker/Pixelate
/// pixels carry frame content frozen from the annotated frame.
///
/// Runs on the burn worker thread (see `burn_annotations`) — each group
/// whose start differs from the committed scrub frame costs a synchronous
/// ffmpeg seek for its pristine background, which must not stall the
/// event loop. Extractions are deduplicated by start time so runs sharing
/// a position pay for one seek, not one per run.
pub(crate) fn compose_overlay_groups(commit: &VideoCommit, video: &std::path::Path) -> Vec<OverlayGroup> {
    let (w, h) = (commit.frame.width(), commit.frame.height());
    let mut groups = group_by_window(&commit.annotations);
    // No annotations → keep today's behavior: one fully-transparent
    // whole-clip overlay; the burn still produces the _annotated copy.
    if groups.is_empty() { groups.push((None, Vec::new())); }

    let mut extracted: Vec<(f32, ImageBuffer<Rgba<u8>, Vec<u8>>)> = Vec::new();
    let mut out_groups = Vec::with_capacity(groups.len());
    for (window, list) in groups {
        let start = window.map(|(s, _)| s).unwrap_or(0.0);
        // Pristine background = the frame at the group's START, so
        // Marker blending + Pixelate sampling are faithful to the
        // moment the ink first appears. The committed scrub frame is
        // reused when it already matches; extraction failure falls
        // back to it — losing blend fidelity for one group beats
        // failing the whole burn.
        let pristine = if start == commit.frame_t {
            commit.frame.clone()
        } else if let Some((_, f)) = extracted.iter().find(|(t, _)| *t == start) {
            f.clone()
        } else {
            let t = commit.duration.map(|d| clamp_extract_t(start, d)).unwrap_or(start);
            match crate::tray_loop::extract_frame_at(video, t) {
                Ok(frame) => {
                    extracted.push((start, frame.clone()));
                    frame
                }
                Err(e) => {
                    eprintln!("kashot: group frame extract failed: {e}");
                    commit.frame.clone()
                }
            }
        };
        let mut frame = pristine.clone();
        let mut out   = ImageBuffer::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        let mut surf  = DiffSurface { frame: &mut frame, out: &mut out };
        for a in list {
            painter::render_annotation(&mut surf, a, Some(&pristine));
        }
        let end = window.and_then(|(_, e)| e.is_finite().then_some(e));
        out_groups.push((out, start, end));
    }
    out_groups
}

fn action_panel_origin(
    bounds: (i32, i32, i32, i32),
    sel: (i32, i32, i32, i32),
    buttons: &[ActionButton],
) -> (i32, i32) {
    let (bx, by, _bw, bh) = bounds;
    let (sx, sy, sw, sh) = sel;
    let (pw, ph) = action_panel_dims(buttons);
    let mut ax = sx + sw - pw;
    let mut ay = sy + sh + 5;
    if ay + ph > by + bh { ay = sy - ph - 5; }
    if ax < bx { ax = sx; }
    (ax.max(bx), ay.max(by))
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

fn action_panel_hit(
    panel_origin: (i32, i32),
    (cx, cy): (i32, i32),
    buttons: &[ActionButton],
) -> Option<ActionButton> {
    for (i, b) in buttons.iter().enumerate() {
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
fn palette_popup_origin(bounds: (i32, i32, i32, i32), panel_origin: (i32, i32)) -> (i32, i32) {
    let (bx, by, bw, _bh) = bounds;
    let (pw, _ph) = palette_popup_dims();
    let mut x = panel_origin.0 - pw - 5;
    let y     = panel_origin.1;
    if x < bx {
        let (tw, _) = tool_panel_dims();
        x = panel_origin.0 + tw + 5;
        if x + pw > bx + bw { x = bx + bw - pw - 5; }
    }
    (x.max(bx), y.max(by))
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
    bounds:     (i32, i32, i32, i32),
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

    let (ox, oy) = tool_panel_origin(bounds, sel);
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

/// Sequential orange-neon halo around the tool-panel buttons, cycling
/// top-to-bottom one button per second. Each button gets a brief glow
/// window of ~0.85 s out of every (N × 1 s) cycle, where N is the number
/// of buttons in `TOOL_PANEL_BUTTONS`. Halo is a 4-ring outer glow that
/// fades with distance so the icon glyph stays readable through it.
///
/// Visual mirror of the website's `.tool-glow` CSS animation — same
/// orange palette, same ~1 s active window, same slow loop.
fn draw_tool_panel_glow(
    buf:     &mut [u32],
    win_w:   usize,
    win_h:   usize,
    bounds:  (i32, i32, i32, i32),
    sel:     (i32, i32, i32, i32),
    elapsed: f32,
) {
    let n = TOOL_PANEL_BUTTONS.len() as f32;       // 13
    let slot = 1.0_f32;                            // 1 second per button
    let cycle = n * slot;                          // 13 s total
    let t = elapsed.rem_euclid(cycle);
    let active_idx = (t / slot).floor() as i32;
    // Fraction-through-slot, 0..1.
    let phase = (t - active_idx as f32 * slot) / slot;
    // On for the first 85 % of the slot, off for the trailing 15 % so the
    // glow has a visible "step" between buttons.
    if phase > 0.85 { return; }
    // Triangular envelope inside the on-window: rises fast, peaks ~mid,
    // fades. Keeps the transition smooth at slot boundaries.
    let env = if phase < 0.15 { phase / 0.15 }
              else if phase < 0.55 { 1.0 }
              else { ((0.85 - phase) / 0.30).max(0.0) };

    let panel_origin = tool_panel_origin(bounds, sel);
    let (x0, y0, x1, y1) = tool_panel_button_rect(panel_origin, active_idx);

    // Orange-neon palette — matches the website's `.tool-glow` filter.
    // Outer rings are progressively fainter so the halo reads as a soft
    // glow rather than a hard frame around the button.
    let orange_r: u32 = 0xFF;
    let orange_g: u32 = 0x7A;
    let orange_b: u32 = 0x1A;
    let base_alpha = (210.0 * env).round().clamp(0.0, 255.0) as u32;
    for ring in 0..6 {
        let inset    = 1 + ring;
        let layer_a  = (base_alpha / (1 + ring as u32 / 2)).min(255);
        if layer_a == 0 { continue; }
        let rx0 = x0 - inset;
        let ry0 = y0 - inset;
        let rx1 = x1 + inset - 1;
        let ry1 = y1 + inset - 1;
        blend_rect_outline(buf, win_w, win_h, rx0, ry0, rx1, ry1,
                           orange_r, orange_g, orange_b, layer_a);
    }
}

/// Square-wave flicker of a laser-green outline around the action panel,
/// running for the first 3 s after the user finishes their selection. Square
/// wave is much harder to ignore than a sine pulse — the eye locks onto the
/// hard on/off transitions exactly the way a missed-call indicator does.
///
/// Layout: 3-pixel solid inner stroke + 5 progressively-fainter outer halo
/// rings for the laser-glow silhouette. The flicker frequency (~6.5 Hz) is
/// fast enough to read as a strobe but slow enough not to look like a render
/// glitch. The pulse stops abruptly at t=3 s rather than fading — the caller
/// clears `panel_pulse_started` the moment elapsed crosses the threshold so
/// the next frame draws no pulse at all.
fn draw_action_panel_pulse(
    buf:    &mut [u32],
    win_w:  usize,
    win_h:  usize,
    bounds: (i32, i32, i32, i32),
    sel:    (i32, i32, i32, i32),
    elapsed: f32,
    buttons: &[ActionButton],
) {
    if elapsed >= 3.0 { return; }
    let origin = action_panel_origin(bounds, sel, buttons);
    let (pw, ph) = action_panel_dims(buttons);

    // Cycle: ~150 ms total; 60% ON, 40% OFF. Solid on the bright phase, faint
    // halo on the dim phase so the panel is always findable even mid-blink.
    let cycle  = (elapsed * 6.5).fract();
    let bright = cycle < 0.6;
    let stroke_a: u32 = if bright { 240 } else { 40 };
    let halo_a:   u32 = if bright { 130 } else { 22 };

    let laser_r: u32 = 0x00;
    let laser_g: u32 = 0xff;
    let laser_b: u32 = 0x95;

    // 3-px solid stroke directly hugging the panel.
    for inset in 1..=3 {
        let x0 = origin.0 - inset;
        let y0 = origin.1 - inset;
        let x1 = origin.0 + pw + inset;
        let y1 = origin.1 + ph + inset;
        blend_rect_outline(buf, win_w, win_h, x0, y0, x1, y1,
                           laser_r, laser_g, laser_b, stroke_a);
    }
    // 5-ring outer halo — each ring is fainter than the last.
    for ring in 0..5 {
        let inset    = 4 + ring;
        let layer_a  = (halo_a / (1 + ring as u32)).min(255);
        let x0       = origin.0 - inset;
        let y0       = origin.1 - inset;
        let x1       = origin.0 + pw + inset;
        let y1       = origin.1 + ph + inset;
        blend_rect_outline(buf, win_w, win_h, x0, y0, x1, y1,
                           laser_r, laser_g, laser_b, layer_a);
    }
}

/// Source-over blend a 1-px rectangular outline of `(r,g,b,a)` onto the
/// XRGB softbuffer. Clipped at the buffer bounds.
fn blend_rect_outline(
    buf: &mut [u32], win_w: usize, win_h: usize,
    x0: i32, y0: i32, x1: i32, y1: i32,
    r: u32, g: u32, b: u32, a: u32,
) {
    if a == 0 { return; }
    let inv = 255 - a;
    let plot = |buf: &mut [u32], x: i32, y: i32| {
        if x < 0 || y < 0 { return; }
        let (xu, yu) = (x as usize, y as usize);
        if xu >= win_w || yu >= win_h { return; }
        let idx = yu * win_w + xu;
        let cur = buf[idx];
        let dr  = (cur >> 16) & 0xFF;
        let dg  = (cur >>  8) & 0xFF;
        let db  =  cur        & 0xFF;
        let nr  = ((r * a + dr * inv + 127) / 255).min(255);
        let ng  = ((g * a + dg * inv + 127) / 255).min(255);
        let nb  = ((b * a + db * inv + 127) / 255).min(255);
        buf[idx] = (nr << 16) | (ng << 8) | nb;
    };
    for x in x0..=x1 { plot(buf, x, y0); plot(buf, x, y1); }
    for y in y0..=y1 { plot(buf, x0, y); plot(buf, x1, y); }
}

fn draw_action_panel(
    buf:    &mut [u32],
    win_w:  usize,
    win_h:  usize,
    bounds: (i32, i32, i32, i32),
    sel:    (i32, i32, i32, i32),
    buttons: &[ActionButton],
) {
    const BG:   u32 = 0x00_22_22_24;
    const BTN:  u32 = 0x00_2E_2E_32;
    const TEXT: u32 = 0x00_E8_E8_EC;
    // Record red, the same hue as the REC dot in `recording_indicator`, so the
    // one destructive-ish action in the row is never confused for Save.
    const REC_DOT: [u8; 4] = [0xFF, 0x3A, 0x3A, 0xFF];

    let origin = action_panel_origin(bounds, sel, buttons);
    let (pw, ph) = action_panel_dims(buttons);
    draw_rounded_rect(buf, win_w, win_h, origin.0, origin.1, origin.0 + pw, origin.1 + ph, PANEL_RADIUS, BG);

    for (i, b) in buttons.iter().enumerate() {
        let (x0, y0, x1, y1) = action_panel_button_rect(origin, i as i32);
        draw_rounded_rect(buf, win_w, win_h, x0, y0, x1, y1, 6, BTN);
        match b {
            ActionButton::Pin    => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Pin,    [232,232,236,255], None, 4.0),
            ActionButton::Copy   => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Copy,   [232,232,236,255], None, 4.0),
            ActionButton::Save   => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Save,   [232,232,236,255], None, 4.0),
            ActionButton::Record => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Record, [232,232,236,255], Some(REC_DOT), 4.0),
            ActionButton::Close  => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Close,  [232,232,236,255], None, 4.0),
        }
    }
}

/// Marker-only opacity slider. Drawn beneath the tool panel when Marker is
/// the active tool. Track is a void-black groove framed in laser-green; the
/// "filled" portion of the track is painted with the user's current marker
/// color at the chosen opacity (alpha-blended over a checker so transparent
/// values read as transparent rather than solid). Knob is a 14-px square
/// rendered in the marker color (opaque) with a laser-green border so it
/// stays visible against any underlying screenshot. A "XX%" readout to the
/// right reports the alpha as a percentage of 255.
fn draw_marker_opacity_slider(
    buf:     &mut [u32],
    win_w:   usize,
    win_h:   usize,
    panel:   (i32, i32, i32, i32),
    color:   kashot_core::color::Rgba,
    alpha:   u8,
) {
    // Palette aligned with the editor's existing dark chrome + laser-green
    // accent. Kept local so they stay legible alongside the other panel
    // helpers without dragging in the settings_form constants.
    const BG:         u32 = 0x00_22_22_24;
    const TRACK_BG:   u32 = 0x00_0A_0A_0E;        // void-black groove
    const BORDER:     u32 = 0x00_00_FF_95;        // laser-green frame
    const TICK:       u32 = 0x00_44_44_4A;
    const TEXT_BR:    u32 = 0x00_E8_E8_EC;
    const CHECKER_A:  u32 = 0x00_2A_2A_30;
    const CHECKER_B:  u32 = 0x00_18_18_1C;

    let (px, py, pw, ph) = panel;
    // Panel background.
    draw_rounded_rect(buf, win_w, win_h, px, py, px + pw, py + ph, PANEL_RADIUS, BG);
    draw_rect_border(buf, win_w, win_h, px, py, px + pw, py + ph, BORDER);

    let (tx, ty, tw, th) = marker_slider_track(panel);

    // Checkerboard backdrop so the alpha-tinted fill reads honestly: at
    // alpha 0 the track looks like a transparent grid; at alpha 255 the
    // color hides the grid entirely.
    let cell = 4;
    let xa = tx.max(0) as usize;
    let xb = ((tx + tw).min(win_w as i32) as usize).max(xa);
    let ya = ty.max(0) as usize;
    let yb = ((ty + th).min(win_h as i32) as usize).max(ya);
    for y in ya..yb.min(win_h) {
        for x in xa..xb.min(win_w) {
            let cx = (x as i32 - tx) / cell;
            let cy = (y as i32 - ty) / cell;
            let on = (cx + cy) & 1 == 0;
            buf[y * win_w + x] = if on { CHECKER_A } else { CHECKER_B };
        }
    }

    // Color-at-current-alpha fill over the whole track. Source-over blend
    // onto the checker so the user can see the transparency of the chosen
    // value at a glance.
    let cr = color.r as u32;
    let cg = color.g as u32;
    let cb = color.b as u32;
    let a  = alpha as u32;
    let inv = 255 - a;
    for y in ya..yb.min(win_h) {
        for x in xa..xb.min(win_w) {
            let idx = y * win_w + x;
            let cur = buf[idx];
            let dr = (cur >> 16) & 0xFF;
            let dg = (cur >>  8) & 0xFF;
            let db =  cur        & 0xFF;
            let nr = (cr * a + dr * inv + 127) / 255;
            let ng = (cg * a + dg * inv + 127) / 255;
            let nb = (cb * a + db * inv + 127) / 255;
            buf[idx] = (nr << 16) | (ng << 8) | nb;
        }
    }

    // Track frame.
    draw_rect_border(buf, win_w, win_h, tx, ty, tx + tw, ty + th, BORDER);
    let _ = TRACK_BG; // reserved for a later AA pass; keeps the constant declared.

    // Tick marks at 0 / 25 / 50 / 75 / 100 % so the user has a visual
    // reference for the common values.
    for n in 1..4 {
        let xn = tx + ((tw - 1) as f32 * n as f32 / 4.0).round() as i32;
        for y in (ty + 1)..(ty + th - 1) {
            if xn >= 0 && (xn as usize) < win_w && y >= 0 && (y as usize) < win_h {
                buf[y as usize * win_w + xn as usize] = TICK;
            }
        }
    }

    // Knob — opaque square painted in the marker color so the user sees
    // the underlying hue even at low alpha. Bordered in laser-green to
    // match the track frame.
    let t = (alpha as f32) / 255.0;
    let kx = tx + ((tw - 1) as f32 * t).round() as i32 - MARKER_SLIDER_KNOB_W / 2;
    let kx = kx.clamp(tx - 1, tx + tw - MARKER_SLIDER_KNOB_W + 1);
    let knob_h = th + 6;
    let ky = ty + (th - knob_h) / 2;
    let knob_rgb = (cr << 16) | (cg << 8) | cb;
    draw_filled_rect(buf, win_w, win_h, kx, ky, kx + MARKER_SLIDER_KNOB_W, ky + knob_h, knob_rgb);
    draw_rect_border(buf, win_w, win_h, kx, ky, kx + MARKER_SLIDER_KNOB_W, ky + knob_h, BORDER);

    // "XX%" readout to the right of the track.
    let pct = ((alpha as i32) * 100 + 127) / 255;
    let label = format!("{pct}%");
    let lx = tx + tw + 6;
    let ly = py + (ph - crate::bitmap_font::GLYPH_H) / 2;
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    crate::painter::draw_text(
        &mut surf, lx, ly, 1, &label,
        kashot_core::color::Rgba::new(
            ((TEXT_BR >> 16) & 0xFF) as u8,
            ((TEXT_BR >>  8) & 0xFF) as u8,
            ( TEXT_BR        & 0xFF) as u8,
            0xFF,
        ),
    );
}

/// Video-mode timeline bar: "current / total" clock, seek track with
/// elapsed fill + playhead, laser-green ticks at each timed annotation's
/// start, and the annotation-duration chip. Chrome matches the marker
/// opacity slider so the two widgets read as one family.
fn draw_timeline_bar(
    buf:        &mut [u32],
    win_w:      usize,
    win_h:      usize,
    bounds:     (i32, i32, i32, i32),
    scrub:      f32,
    total:      f32,
    chip_label: &str,
    tick_times: &[f32],
) {
    const BG:       u32 = 0x00_22_22_24;
    const BTN:      u32 = 0x00_2E_2E_32;
    const TRACK_BG: u32 = 0x00_0A_0A_0E;     // void-black groove
    const BORDER:   u32 = 0x00_00_FF_95;     // laser-green frame
    const FILL:     u32 = 0x00_64_95_ED;     // elapsed portion, selection blue
    const KNOB:     u32 = 0x00_FF_FF_FF;
    const TICK:     u32 = 0x00_00_FF_95;     // annotation starts, laser-green
    const TEXT:     u32 = 0x00_E8_E8_EC;

    let bar = timeline_bar_rect(bounds);
    let (bx, by, bw, bh) = bar;
    draw_rounded_rect(buf, win_w, win_h, bx, by, bx + bw, by + bh, PANEL_RADIUS, BG);
    draw_rect_border(buf, win_w, win_h, bx, by, bx + bw, by + bh, BORDER);

    // Clock readout, left slot.
    let label = format!("{} / {}", format_timecode(scrub), format_timecode(total));
    let ly = by + (bh - crate::bitmap_font::GLYPH_H * 2) / 2;
    {
        let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
        crate::painter::draw_text(&mut surf, bx + TIMELINE_PAD, ly, 2, &label,
            kashot_core::color::Rgba::new(
                ((TEXT >> 16) & 0xFF) as u8,
                ((TEXT >>  8) & 0xFF) as u8,
                ( TEXT        & 0xFF) as u8,
                0xFF,
            ));
    }

    // Seek track: groove, elapsed fill, frame.
    let (tx, ty, tw, th) = timeline_track(bar, total);
    draw_filled_rect(buf, win_w, win_h, tx, ty, tx + tw, ty + th, TRACK_BG);
    let frac   = if total > 0.0 { (scrub / total).clamp(0.0, 1.0) } else { 0.0 };
    let head_x = tx + ((tw - 1) as f32 * frac).round() as i32;
    draw_filled_rect(buf, win_w, win_h, tx, ty, head_x, ty + th, FILL);
    draw_rect_border(buf, win_w, win_h, tx, ty, tx + tw, ty + th, BORDER);

    // Annotation start ticks — full-height lines through the groove so
    // the user can find their timed ink without scrubbing for it.
    for &t in tick_times {
        let frac = if total > 0.0 { (t / total).clamp(0.0, 1.0) } else { 0.0 };
        let xn = tx + ((tw - 1) as f32 * frac).round() as i32;
        for y in (ty + 1)..(ty + th - 1) {
            if xn >= 0 && (xn as usize) < win_w && y >= 0 && (y as usize) < win_h {
                buf[y as usize * win_w + xn as usize] = TICK;
            }
        }
    }

    // Playhead knob — taller than the groove like the marker knob.
    let kx = (head_x - TIMELINE_KNOB_W / 2).clamp(tx - 1, tx + tw - TIMELINE_KNOB_W + 1);
    let knob_h = th + 6;
    let ky = ty + (th - knob_h) / 2;
    draw_filled_rect(buf, win_w, win_h, kx, ky, kx + TIMELINE_KNOB_W, ky + knob_h, KNOB);
    draw_rect_border(buf, win_w, win_h, kx, ky, kx + TIMELINE_KNOB_W, ky + knob_h, BORDER);

    // Duration chip, right slot.
    let (cx, cy, cw, ch) = timeline_chip_rect(bar);
    draw_rounded_rect(buf, win_w, win_h, cx, cy, cx + cw, cy + ch, 6, BTN);
    draw_rect_border(buf, win_w, win_h, cx, cy, cx + cw, cy + ch, BORDER);
    let text_w = crate::bitmap_font::measure(chip_label, 2);
    let text_x = cx + (cw - text_w) / 2;
    let text_y = cy + (ch - crate::bitmap_font::GLYPH_H * 2) / 2;
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    crate::painter::draw_text(&mut surf, text_x, text_y, 2, chip_label, kashot_core::color::Rgba::WHITE);
}

fn draw_palette_popup(
    buf:           &mut [u32],
    win_w:         usize,
    win_h:         usize,
    bounds:        (i32, i32, i32, i32),
    panel_origin:  (i32, i32),
    active_color:  kashot_core::color::Rgba,
    palette_index: usize,
) {
    const BG:        u32 = 0x00_22_22_24;
    const HEADER_BG: u32 = 0x00_2E_2E_32;

    let origin = palette_popup_origin(bounds, panel_origin);
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

/// Force focus + keyboard grab onto the given X11 window XID. Without this
/// Cinnamon ignores winit's `_NET_ACTIVE_WINDOW` request and never routes
/// KeyPress events to us, so the Text tool sees nothing.
#[cfg(target_os = "linux")]
fn force_x11_focus(xid: u32) -> anyhow::Result<()> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, GrabMode, InputFocus};
    let (conn, _screen) = x11rb::connect(None)
        .map_err(|e| anyhow!("x11rb::connect: {e}"))?;
    conn.set_input_focus(InputFocus::PARENT, xid, x11rb::CURRENT_TIME)
        .map_err(|e| anyhow!("set_input_focus: {e}"))?
        .check()
        .map_err(|e| anyhow!("set_input_focus check: {e}"))?;
    conn.grab_keyboard(false, xid, x11rb::CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
        .map_err(|e| anyhow!("grab_keyboard: {e}"))?
        .reply()
        .map_err(|e| anyhow!("grab_keyboard reply: {e}"))?;
    conn.flush().map_err(|e| anyhow!("flush: {e}"))?;
    Ok(())
}

/// Drop the X11 keyboard grab when the overlay closes so the next focused
/// app gets typed input back.
///
/// No-op on Wayland: `push_x11_focus` never took a grab there (the window
/// handle isn't an Xlib one), so there is nothing to release and an X11
/// connection attempt per closed overlay would only be noise.
#[cfg(target_os = "linux")]
fn release_x11_focus() {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::ConnectionExt;
    if kashot_platform::session::is_wayland() { return; }
    let Ok((conn, _)) = x11rb::connect(None) else { return; };
    let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    let _ = conn.flush();
}

/// Small dark pill carrying a button label, drawn near where the user's
/// cursor is hovering. Uses the in-tree 5×7 bitmap font at scale 2 so it
/// stays consistent with the dimension chip and palette header.
fn draw_tooltip(
    buf:    &mut [u32],
    win_w:  usize,
    win_h:  usize,
    bounds: (i32, i32, i32, i32),
    label:  &str,
    x:      i32,
    y:      i32,
) {
    let scale = 2;
    let text_w = crate::bitmap_font::measure(label, scale);
    let text_h = crate::bitmap_font::GLYPH_H * scale;
    let pad_x  = 6;
    let pad_y  = 4;
    let chip_w = text_w + pad_x * 2;
    let chip_h = text_h + pad_y * 2;
    // Auto-flip so the chip stays on the monitor it belongs to.
    let (bx, by, bw, bh) = bounds;
    let mut x0 = x;
    let mut y0 = y;
    if x0 + chip_w > bx + bw { x0 = bx + bw - chip_w - 4; }
    if y0 + chip_h > by + bh { y0 = by + bh - chip_h - 4; }
    if x0 < bx { x0 = bx + 4; }
    if y0 < by { y0 = by + 4; }
    let x1 = x0 + chip_w;
    let y1 = y0 + chip_h;
    draw_filled_rect(buf, win_w, win_h, x0, y0, x1, y1, 0x00_10_10_14);
    draw_rect_border(buf, win_w, win_h, x0, y0, x1, y1, 0x00_4A_4A_50);
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    crate::painter::draw_text(&mut surf, x0 + pad_x, y0 + pad_y, scale, label, kashot_core::color::Rgba::WHITE);
}

/// The one-line hint shown while nothing is selected yet. Plain ASCII —
/// the 5x7 bitmap font substitutes '?' for anything outside 0x20..=0x7E,
/// so no dashes or arrows beyond the keyboard set.
const IDLE_HINT: &str = "drag to select a region  -  Esc to close";

/// Record-mode variant: names the confirm key, because there is no Save button
/// to fall back on and the rectangle alone doesn't say how to start.
const RECORD_HINT: &str = "drag the area to record  -  Enter to start  -  Esc to close";

/// Bottom-center hint chip, painted only in `State::Idle`. Same dark fill +
/// hairline border as the tooltip chip so it reads as overlay chrome, but
/// in a muted grey: it's a nudge for first-time users, not something that
/// should pull attention away from the region they're about to pick.
fn draw_idle_hint(
    buf:    &mut [u32],
    win_w:  usize,
    win_h:  usize,
    bounds: (i32, i32, i32, i32),
    hint:   &str,
) {
    const SCALE:  i32 = 2;
    const PAD_X:  i32 = 12;
    const PAD_Y:  i32 = 8;
    const MARGIN: i32 = 48;          // gap from the bottom screen edge

    let (bx, by, bw, bh) = bounds;
    let chip_w = crate::bitmap_font::measure(hint, SCALE) + PAD_X * 2;
    let chip_h = crate::bitmap_font::GLYPH_H * SCALE + PAD_Y * 2;
    // Centered on the monitor the cursor is on, not across the seam of a
    // multi-monitor overlay.
    let x0 = bx + (bw - chip_w) / 2;
    let y0 = by + bh - chip_h - MARGIN;
    // Screens too small to hold the chip get no hint rather than a clipped
    // one — it would land on top of the user's content either way.
    if x0 < bx || y0 < by { return; }
    let x1 = x0 + chip_w;
    let y1 = y0 + chip_h;
    draw_filled_rect(buf, win_w, win_h, x0, y0, x1, y1, 0x00_10_10_14);
    draw_rect_border(buf, win_w, win_h, x0, y0, x1, y1, 0x00_3A_3A_40);
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    crate::painter::draw_text(
        &mut surf, x0 + PAD_X, y0 + PAD_Y, SCALE, hint,
        kashot_core::color::Rgba::new_opaque(0x9A, 0x9A, 0xA2),
    );
}

/// Bare modifier presses — winit delivers these as ordinary key events, and
/// they must not count as "any other key" while a confirmation is armed:
/// reaching for Ctrl+Z would otherwise silently dismiss the prompt.
fn is_modifier_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::AltGraph
                | NamedKey::CapsLock
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Fn
                | NamedKey::FnLock
                | NamedKey::Symbol
                | NamedKey::SymbolLock
        )
    )
}

/// Two-line notice chip pinned to the top center of the overlay. Same dark
/// fill as the tooltip and idle-hint chrome, with a colored hairline so the
/// Esc confirmation (amber) and the select-mode cue (laser green) are
/// distinguishable at a glance. ASCII only — the 5x7 font substitutes '?'
/// for anything outside 0x20..=0x7E. `bounds` is the monitor the chip is
/// centred in (the one under the cursor), so on a multi-monitor desktop it
/// never straddles a seam.
fn draw_notice_bar(
    buf:    &mut [u32],
    win_w:  usize,
    win_h:  usize,
    bounds: (i32, i32, i32, i32),
    lines:  &[&str],
    accent: u32,
    text:   u32,
) {
    const SCALE:  i32 = 2;
    const PAD_X:  i32 = 14;
    const PAD_Y:  i32 = 10;
    const LINE_GAP: i32 = 6;
    const MARGIN: i32 = 36;          // gap from the top screen edge

    if lines.is_empty() { return; }
    let line_h  = crate::bitmap_font::GLYPH_H * SCALE;
    let text_w  = lines.iter()
        .map(|l| crate::bitmap_font::measure(l, SCALE))
        .max()
        .unwrap_or(0);
    let chip_w = text_w + PAD_X * 2;
    let chip_h = line_h * lines.len() as i32
        + LINE_GAP * (lines.len() as i32 - 1)
        + PAD_Y * 2;
    let (bx, by, bw, bh) = bounds;
    let x0 = bx + (bw - chip_w) / 2;
    let y0 = by + MARGIN;
    // A screen too small to hold the chip gets none rather than a clipped
    // one — same rule as the idle hint.
    if x0 < bx || y0 + chip_h > by + bh
        || x0 < 0 || y0 < 0 || x0 + chip_w > win_w as i32 || y0 + chip_h > win_h as i32 { return; }
    draw_filled_rect(buf, win_w, win_h, x0, y0, x0 + chip_w, y0 + chip_h, 0x00_10_10_14);
    draw_rect_border(buf, win_w, win_h, x0, y0, x0 + chip_w, y0 + chip_h, accent);
    let text_rgb = kashot_core::color::Rgba::new_opaque(
        ((text >> 16) & 0xFF) as u8, ((text >> 8) & 0xFF) as u8, (text & 0xFF) as u8,
    );
    let accent_rgb = kashot_core::color::Rgba::new_opaque(
        ((accent >> 16) & 0xFF) as u8, ((accent >> 8) & 0xFF) as u8, (accent & 0xFF) as u8,
    );
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    for (i, line) in lines.iter().enumerate() {
        let lw = crate::bitmap_font::measure(line, SCALE);
        let lx = x0 + (chip_w - lw) / 2;
        let ly = y0 + PAD_Y + i as i32 * (line_h + LINE_GAP);
        // First line is the headline and takes the accent color; the rest
        // is the quieter instruction text.
        let color = if i == 0 { accent_rgb } else { text_rgb };
        crate::painter::draw_text(&mut surf, lx, ly, SCALE, line, color);
    }
}

/// Magnifier lens. Samples the original capture in a (2·R+1)² window around
/// the cursor and draws each source pixel as a `MAG_ZOOM`-sized square. Adds
/// a 1-px border + crosshair through the center pixel. Auto-flips position
/// so it never falls off the edge of the monitor it is on.
///
/// `shot_off` maps the cursor from frame space into the capture: on a
/// multi-monitor desktop the two differ by the virtual-desktop origin, and
/// without it the lens shows pixels from somewhere else entirely.
fn draw_magnifier(
    buf:      &mut [u32],
    win_w:    usize,
    win_h:    usize,
    bounds:   (i32, i32, i32, i32),
    shot:     &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    cursor:   (i32, i32),
    shot_off: (i32, i32),
) {
    let (bx, by, bw, bh) = bounds;
    let chip = MAG_SIZE + 4;          // includes border
    let mut x0 = cursor.0 + MAG_OFFSET;
    let mut y0 = cursor.1 + MAG_OFFSET;
    if x0 + chip > bx + bw { x0 = cursor.0 - MAG_OFFSET - chip; }
    if y0 + chip > by + bh { y0 = cursor.1 - MAG_OFFSET - chip; }
    if x0 < bx || y0 < by { return; } // not enough room either way

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
            let src_x = cursor.0 + shot_off.0 + sx - MAG_RADIUS;
            let src_y = cursor.1 + shot_off.1 + sy - MAG_RADIUS;
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

/// Paint surface for `compose_video_overlay`. Reads (and writes) a live
/// copy of the video frame so alpha blending + Pixelate source-sampling
/// match the screenshot editor pixel-for-pixel, while mirroring every
/// touched pixel — opaque — into a transparent out-buffer that becomes the
/// ffmpeg overlay. Pixels no annotation touches stay (0,0,0,0).
struct DiffSurface<'a> {
    frame: &'a mut ImageBuffer<Rgba<u8>, Vec<u8>>,
    out:   &'a mut ImageBuffer<Rgba<u8>, Vec<u8>>,
}

impl painter::Surface for DiffSurface<'_> {
    fn width(&self)  -> i32 { self.frame.width()  as i32 }
    fn height(&self) -> i32 { self.frame.height() as i32 }
    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        self.frame.get_pixel(x as u32, y as u32).0
    }
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        self.frame.put_pixel(x as u32, y as u32, Rgba(rgba));
        self.out.put_pixel(x as u32, y as u32, Rgba([rgba[0], rgba[1], rgba[2], 255]));
    }
}

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
/// The transform itself lives in `kashot-core` so the Select tool's move
/// and this crop-space shift can never drift apart.
fn translate_annotation(a: &Annotation, dx: f32, dy: f32) -> Annotation {
    edit::translated(a, dx, dy)
}

/// Width × height chip rendered just outside the bottom-right corner of the
/// selection. Uses the existing 5×7 digit font (via `painter::draw_number`)
/// and a tiny hand-drawn `×` glyph between the two numbers. Background is a
/// 75 %-opaque dark fill so it stays legible on light or dark screenshots.
fn draw_dimension_chip(
    buf: &mut [u32], stride: usize, height: usize,
    bounds: (i32, i32, i32, i32),
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
    let (bx, by, _bw, _bh) = bounds;
    let mut x0 = anchor_x - chip_w - 4;
    let mut y0 = anchor_y - chip_h - 4;
    if x0 < bx { x0 = anchor_x + 4; }
    if y0 < by { y0 = anchor_y + 4; }
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

/// 1-px dashed rectangle border. Used as a subtle text-area indicator
/// while the Text tool is in TextInput state — vanishes the moment the
/// user commits because compose_final / save / copy / pin all snapshot
/// the bitmap without re-running this render pass.
fn draw_dashed_border(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32,
) {
    let xa = x0.max(0) as usize;
    let xb = (x1.min(stride as i32) as usize).max(xa);
    let ya = y0.max(0) as usize;
    let yb = (y1.min(height as i32) as usize).max(ya);
    if xa >= stride || ya >= height || xa == xb || ya == yb { return; }
    // 3-on-3-off dash pattern.
    let dash = |i: usize| (i / 3) & 1 == 0;
    for x in xa..xb.min(stride) {
        if dash(x) {
            buf[ya * stride + x] = rgb;
            let by = (yb - 1).min(height - 1);
            buf[by * stride + x] = rgb;
        }
    }
    for y in ya..yb.min(height) {
        if dash(y) {
            buf[y * stride + xa] = rgb;
            let bx = (xb - 1).min(stride - 1);
            buf[y * stride + bx] = rgb;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use kashot_core::annotation::{Annotation, Point2, Stroke};
    use kashot_core::color::Rgba as KRgba;

    fn frame(w: u32, h: u32, px: [u8; 4]) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
        ImageBuffer::from_pixel(w, h, Rgba(px))
    }

    /// Untouched pixels of the video-annotate overlay must stay fully
    /// transparent so ffmpeg shows the clip through them.
    #[test]
    fn diff_surface_untouched_pixels_stay_transparent() {
        let mut f   = frame(32, 32, [50, 60, 70, 255]);
        let mut out = frame(32, 32, [0, 0, 0, 0]);
        let mut surf = DiffSurface { frame: &mut f, out: &mut out };

        let mut pen = Annotation::pen(
            Stroke { color: KRgba { r: 255, g: 0, b: 0, a: 255 }, thickness: 1.0 },
            Point2::new(4.0, 4.0),
        );
        pen.extend(Point2::new(10.0, 4.0));
        painter::render_annotation(&mut surf, &pen, None);

        assert_eq!(out.get_pixel(4, 4).0[3], 255, "stroked pixel must be opaque");
        assert_eq!(out.get_pixel(4, 4).0[0], 255, "stroked pixel keeps stroke color");
        assert_eq!(out.get_pixel(20, 20).0, [0, 0, 0, 0], "untouched pixel stays transparent");
    }

    /// Semi-transparent strokes (Marker) must blend against the FRAME, not
    /// against transparency — the regression this surface exists to stop:
    /// blending against (0,0,0,0) renders an opaque near-black band.
    #[test]
    fn diff_surface_marker_blends_against_frame() {
        let mut f   = frame(32, 32, [200, 200, 200, 255]);
        let mut out = frame(32, 32, [0, 0, 0, 0]);
        let mut surf = DiffSurface { frame: &mut f, out: &mut out };

        // 50%-alpha yellow marker over light grey.
        let mut marker = Annotation::marker(
            Stroke { color: KRgba { r: 255, g: 255, b: 0, a: 255 }, thickness: 4.0 },
            Point2::new(8.0, 8.0),
            128,
        );
        marker.extend(Point2::new(16.0, 8.0));
        painter::render_annotation(&mut surf, &marker, None);

        let p = out.get_pixel(10, 8).0;
        assert_eq!(p[3], 255, "marker pixel is opaque in the overlay");
        // Yellow@50% over grey(200) ≈ (227, 227, 100) — must NOT be the
        // near-black (127, 127, 0) that blending against transparency gives.
        assert!(p[0] > 180 && p[1] > 180,
            "marker must blend against the frame, got {p:?}");
        assert!(p[2] < 160, "blue channel should drop under a yellow marker, got {p:?}");
    }

    #[test]
    fn time_window_filter_is_half_open() {
        let mut a = Annotation::pen(Stroke::default(), Point2::new(0.0, 0.0));
        assert!(annotation_visible_at(&a, 0.0), "None = whole clip");
        assert!(annotation_visible_at(&a, 99.0));
        a.time = Some((1.0, 4.0));
        assert!(!annotation_visible_at(&a, 0.999));
        assert!( annotation_visible_at(&a, 1.0), "start is inclusive");
        assert!( annotation_visible_at(&a, 3.999));
        assert!(!annotation_visible_at(&a, 4.0), "end is exclusive");
        a.time = Some((2.0, f32::INFINITY));
        assert!(annotation_visible_at(&a, 9999.0), "open end means until clip end");
    }

    #[test]
    fn group_by_window_groups_consecutive_runs_only() {
        let p = Point2::new(0.0, 0.0);
        let a  = Annotation::pen(Stroke::default(), p);               // None
        let a2 = Annotation::pen(Stroke::default(), p);               // None — same run
        let mut b = Annotation::pen(Stroke::default(), p);
        b.time = Some((1.0, 4.0));
        let c = Annotation::pen(Stroke::default(), p);                // None again — new run
        let all = [a, a2, b, c];
        let groups = group_by_window(&all);
        assert_eq!(groups.len(), 3,
            "the trailing whole-clip stroke must start a new run — folding \
             it into the first would burn the timed ink on top of it");
        assert_eq!(groups[0].0, None);
        assert_eq!(groups[0].1.len(), 2, "consecutive whole-clip strokes share one group");
        assert_eq!(groups[1].0, Some((1.0, 4.0)));
        assert_eq!(groups[2].0, None);
        assert_eq!(groups[2].1.len(), 1);
    }

    #[test]
    fn stamp_window_at_clip_end_stays_inside_the_clip() {
        // Dragging the playhead fully right puts scrub_pos exactly at the
        // duration. A raw stamp would gate the burn on gte(t, dur), which
        // no frame ever reaches — frame timestamps stop one frame short
        // of the container length — so the ink the preview showed would
        // silently vanish from the burned video.
        let w = stamp_window(10.0, 10.0, None).expect("end-of-clip stamp is a window");
        assert!(w.0 < 10.0, "start must sit strictly inside the clip, got {}", w.0);
        assert_eq!(w.0, 10.0 - 0.05);
        assert_eq!(w.1, f32::INFINITY);
        // Presets that run past the end collapse to open-ended too.
        assert_eq!(stamp_window(10.0, 10.0, Some(3.0)), Some((10.0 - 0.05, f32::INFINITY)));
    }

    #[test]
    fn stamp_window_default_collapses_to_whole_clip() {
        assert_eq!(stamp_window(0.0, 10.0, None), None, "scrub 0 + \"End\" = no window");
        assert_eq!(stamp_window(0.0, 10.0, Some(3.0)), Some((0.0, 3.0)));
        assert_eq!(stamp_window(2.0, 10.0, Some(3.0)), Some((2.0, 5.0)));
        assert_eq!(stamp_window(2.0, 10.0, None), Some((2.0, f32::INFINITY)));
    }

    #[test]
    fn modifier_keys_do_not_answer_a_confirmation() {
        // "any other key cancels" must not include the modifier half of a
        // Ctrl+Z the user is reaching for.
        assert!(is_modifier_key(&Key::Named(NamedKey::Shift)));
        assert!(is_modifier_key(&Key::Named(NamedKey::Control)));
        assert!(is_modifier_key(&Key::Named(NamedKey::Alt)));
        assert!(!is_modifier_key(&Key::Named(NamedKey::Escape)));
        assert!(!is_modifier_key(&Key::Named(NamedKey::Enter)));
        assert!(!is_modifier_key(&Key::Character("p".into())));
    }

    #[test]
    fn notice_bar_skips_screens_too_small_to_hold_it() {
        // Must not panic or write out of bounds on a tiny surface.
        let mut buf = vec![0u32; 40 * 20];
        draw_notice_bar(&mut buf, 40, 20, (0, 0, 40, 20), &["Discard 2 annotations?", "Esc or Enter"], 0x00_FF_B0_20, 0x00_FF_D8_8A);
        assert!(buf.iter().all(|&px| px == 0), "no room for the chip means no chip");
    }

    #[test]
    fn notice_bar_paints_inside_a_roomy_surface() {
        let (w, h) = (600usize, 300usize);
        let mut buf = vec![0u32; w * h];
        draw_notice_bar(&mut buf, w, h, (0, 0, w as i32, h as i32), &["Select mode", "click ink to move it"], 0x00_00_FF_95, 0x00_C8_FF_E4);
        assert!(buf.iter().any(|&px| px != 0), "the chip should have been drawn");
        // Bottom rows stay untouched — the bar is pinned to the top.
        assert!(buf[(h - 1) * w..].iter().all(|&px| px == 0));
    }

    #[test]
    fn timecode_formats_minutes_and_seconds() {
        assert_eq!(format_timecode(0.0),    "0:00");
        assert_eq!(format_timecode(12.34),  "0:12");
        assert_eq!(format_timecode(75.0),   "1:15");
        assert_eq!(format_timecode(3601.0), "60:01");
    }
}

