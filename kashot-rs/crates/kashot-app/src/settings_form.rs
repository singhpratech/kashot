//! Real Kashot — Settings window.
//!
//! Borderless winit + softbuffer + the in-tree bitmap font. Same lifecycle
//! pattern as `PinView` / `Overlay`: TrayApp owns an `Option<SettingsView>`,
//! dispatches `WindowEvent`s by `WindowId`, and polls `outcome` after each
//! event to learn when the user saved or cancelled.
//!
//! Layout — three grouped sections, all keyboard- and mouse-driven:
//!
//!   PATHS         Screenshots folder (path display + Browse…)
//!                 Recordings folder  (path display + Browse…)
//!   WATERMARK     Enabled (toggle pill)
//!                 Text    (editable, focus on click, types live)
//!                 Position (cycles TopLeft → TopRight → BottomRight → BottomLeft)
//!                 Opacity  (cycles 25 / 50 / 75 / 100 %)
//!   APPEARANCE    Theme   (cycles Light / Dark)
//!                 Start with system (toggle pill)
//!
//! Hotkey rebinding is the one option still routed through the JSON file —
//! it needs a keystroke-capture widget that's out of scope here. The
//! "Edit as JSON" button in the action bar opens settings.json in the user's
//! default editor for anything this dialog doesn't expose.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use kashot_core::AppSettings;
use kashot_core::color::Rgba as KashotRgba;
use kashot_core::settings::WatermarkAnchor;

use crate::bitmap_font;
use crate::painter;

// ── colors (laser-green / void-black brand) ─────────────────────────────────
const BG_TOP:        u32 = 0x0008_0c0a;
const BG_BODY:       u32 = 0x000a_0e0c;
const HEADER_RULE:   u32 = 0x0014_2a1f;
const PANEL_BORDER:  u32 = 0x0014_2a1f;
const FIELD_BG:      u32 = 0x0006_0a08;
const FIELD_BORDER:  u32 = 0x001c_2e25;
const FIELD_FOCUS:   u32 = 0x0000_ff95;
const TEXT_BRIGHT:   u32 = 0x00e8_ffe8;
const TEXT_MUTED:    u32 = 0x009c_b0a4;
const TEXT_DIM:      u32 = 0x0068_7a70;
const SECTION_TINT:  u32 = 0x0066_ffb6;
const LASER:         u32 = 0x0000_ff95;
const LASER_DIM:     u32 = 0x0000_8050;
const HOVER_FILL:    u32 = 0x0010_2018;
const TOGGLE_OFF:    u32 = 0x0014_1c18;
const TOGGLE_ON:     u32 = 0x0000_5a36;
const TOGGLE_KNOB:   u32 = 0x0000_ff95;
const TOGGLE_OFF_K:  u32 = 0x004a_5a52;

// ── geometry ────────────────────────────────────────────────────────────────
const WIN_W: u32 = 640;
const WIN_H: u32 = 600;
const PAD:   i32 = 22;
const ROW_H: i32 = 34;
const LABEL_W: i32 = 200;
const BTN_H: i32 = 30;
// Header band carries the brand strip on top and the action buttons (Save /
// Cancel / Edit-as-JSON) anchored to its right side. Bigger than the body
// padding so the buttons have generous click targets.
const HEADER_H: i32 = 84;
// No footer rule anymore — the action buttons moved into the header.
const FOOTER_H: i32 = 28;

// Caret blink period (ms).
const CARET_BLINK_MS: u128 = 530;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WidgetKind {
    SaveFolder,
    RecordingsFolder,
    WatermarkToggle,
    WatermarkText,
    WatermarkPos,
    WatermarkOpacity,
    ThemeCycle,
    StartWithOs,
    OpenJson,
    SaveBtn,
    CancelBtn,
}

struct Row {
    kind:  WidgetKind,
    label: &'static str,
    rect:  (i32, i32, i32, i32),
}

pub enum SettingsOutcome {
    /// User clicked [Save]; tray loop applies + persists.
    Saved(AppSettings),
    /// User clicked [Cancel] or hit Esc.
    Cancelled,
    /// User clicked [Edit as JSON]; tray loop should shell-open the config
    /// path. The view stays open so subsequent edits land in the draft when
    /// the user re-opens it.
    OpenJson,
}

pub struct SettingsView {
    window:  Rc<Window>,
    _ctx:    Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    draft:   AppSettings,
    rows:    Vec<Row>,
    cursor:  (i32, i32),
    hover:   Option<usize>,
    /// Index in `rows` of the watermark-text field while the user is editing.
    /// `None` if no input has focus.
    focus:   Option<usize>,
    /// Wall-clock at which the caret was last reset (used for blink phase).
    caret_t: Instant,
    /// True while the user is left-dragging the opacity slider knob. We
    /// capture the drag once mouse-down lands on the track and keep
    /// updating the opacity value as the cursor moves until release.
    dragging_opacity: bool,
    pub outcome: Option<SettingsOutcome>,
}

impl SettingsView {
    pub fn new(loop_target: &ActiveEventLoop, current: AppSettings) -> Result<Self> {
        let (cx, cy) = centered_origin(loop_target, WIN_W, WIN_H);
        let attrs = WindowAttributes::default()
            .with_title("Kashot — Settings")
            .with_decorations(true)
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(WIN_W, WIN_H))
            .with_position(PhysicalPosition::new(cx, cy))
            .with_window_icon(crate::brand_icon::shared());

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window (settings): {e}"))?;

        window.set_cursor(CursorIcon::Default);
        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new (settings): {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new (settings): {e}"))?;

        let mut me = SettingsView {
            window, _ctx: ctx, surface,
            draft: current,
            rows: Vec::new(),
            cursor: (0, 0),
            hover: None,
            focus: None,
            caret_t: Instant::now(),
            dragging_opacity: false,
            outcome: None,
        };
        me.rows = me.build_rows();
        me.redraw();
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.outcome = Some(SettingsOutcome::Cancelled);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as i32, position.y as i32);
                // Continue any active opacity drag — the slider tracks the
                // cursor's X coord even when the cursor leaves the row.
                if self.dragging_opacity {
                    self.set_opacity_from_cursor_x();
                }
                let new_hover = self.hit_test(self.cursor.0, self.cursor.1);
                let want_cursor = match new_hover.and_then(|i| Some(self.rows[i].kind)) {
                    Some(WidgetKind::WatermarkText) => CursorIcon::Text,
                    Some(_)                          => CursorIcon::Pointer,
                    None                             => CursorIcon::Default,
                };
                self.window.set_cursor(want_cursor);
                if new_hover != self.hover {
                    self.hover = new_hover;
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left, ..
            } => {
                if self.dragging_opacity {
                    self.dragging_opacity = false;
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left, ..
            } => {
                let hit = self.hit_test(self.cursor.0, self.cursor.1);
                // Focus management: clicking the text field focuses it; clicking
                // anything else (or empty space) defocuses.
                match hit.and_then(|i| Some(self.rows[i].kind)) {
                    Some(WidgetKind::WatermarkText) => {
                        self.focus = hit;
                        self.caret_t = Instant::now();
                        self.window.request_redraw();
                    }
                    _ => {
                        if self.focus.is_some() {
                            self.focus = None;
                            self.window.request_redraw();
                        }
                    }
                }
                if let Some(i) = hit {
                    self.activate(i);
                }
            }
            WindowEvent::KeyboardInput { event: key_ev, .. } => {
                self.on_key(key_ev);
            }
            WindowEvent::Resized(_) | WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn on_key(&mut self, ev: winit::event::KeyEvent) {
        if ev.state != ElementState::Pressed { return; }
        // Esc always cancels.
        if matches!(ev.logical_key, Key::Named(NamedKey::Escape)) {
            if self.focus.is_some() {
                self.focus = None;
                self.window.request_redraw();
            } else {
                self.outcome = Some(SettingsOutcome::Cancelled);
            }
            return;
        }
        // Routing while a text field has focus.
        let Some(focus_idx) = self.focus else {
            // No focus — Enter saves, Tab moves nothing yet.
            if matches!(ev.logical_key, Key::Named(NamedKey::Enter)) {
                self.outcome = Some(SettingsOutcome::Saved(self.draft.clone()));
            }
            return;
        };
        let kind = self.rows[focus_idx].kind;
        if kind != WidgetKind::WatermarkText { return; }
        match ev.logical_key {
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab) => {
                self.focus = None;
                self.window.request_redraw();
            }
            Key::Named(NamedKey::Backspace) => {
                self.draft.watermark_text.pop();
                self.caret_t = Instant::now();
                self.window.request_redraw();
            }
            Key::Named(NamedKey::Space) => {
                if self.draft.watermark_text.len() < 64 {
                    self.draft.watermark_text.push(' ');
                    self.caret_t = Instant::now();
                    self.window.request_redraw();
                }
            }
            Key::Character(ref s) => {
                for ch in s.chars() {
                    if !ch.is_control() && self.draft.watermark_text.len() < 64 {
                        self.draft.watermark_text.push(ch);
                    }
                }
                self.caret_t = Instant::now();
                self.window.request_redraw();
            }
            _ => {}
        }
    }

    /// Geometry of the opacity slider's track. Returns (x, y, width, height)
    /// of the rectangle the knob can travel in. Source of truth for both
    /// rendering and drag-tracking so the two stay in sync.
    fn opacity_track(&self) -> Option<(i32, i32, i32, i32)> {
        let row = self.rows.iter().find(|r| r.kind == WidgetKind::WatermarkOpacity)?;
        let (rx, ry, rw, rh) = row.rect;
        let vx = rx + LABEL_W;
        let vw = rw - LABEL_W - 4;
        let vy = ry + 4;
        let vh = rh - 8;
        Some((vx, vy, vw, vh))
    }

    /// Snap the watermark opacity to the cursor's X position inside the
    /// slider track. Step is 1 % so the value reads as a clean percentage.
    fn set_opacity_from_cursor_x(&mut self) {
        let Some((tx, _ty, tw, _th)) = self.opacity_track() else { return; };
        if tw <= 1 { return; }
        let cx = self.cursor.0;
        let mut t = (cx - tx) as f32 / (tw - 1) as f32;
        if !t.is_finite() { t = 0.0; }
        t = t.clamp(0.0, 1.0);
        // Quantize to 1 % so the display reads cleanly.
        let q = (t * 100.0).round() / 100.0;
        self.draft.watermark_opacity = q;
        self.window.request_redraw();
    }

    fn hit_test(&self, x: i32, y: i32) -> Option<usize> {
        // Reverse so action-bar buttons on top of any overlapping layout win.
        self.rows.iter().enumerate().rev().find_map(|(i, r)| {
            let (rx, ry, rw, rh) = r.rect;
            if x >= rx && x < rx + rw && y >= ry && y < ry + rh { Some(i) } else { None }
        })
    }

    fn activate(&mut self, idx: usize) {
        let kind = self.rows[idx].kind;
        match kind {
            WidgetKind::SaveFolder => {
                let starting = if self.draft.save_directory.is_empty() {
                    directories::UserDirs::new()
                        .and_then(|u| u.picture_dir().map(|p| p.to_path_buf()))
                        .unwrap_or_else(std::env::temp_dir)
                } else {
                    PathBuf::from(&self.draft.save_directory)
                };
                if let Some(p) = rfd::FileDialog::new()
                    .set_title("Kashot — Screenshots folder")
                    .set_directory(&starting)
                    .pick_folder()
                {
                    self.draft.save_directory = p.to_string_lossy().to_string();
                }
            }
            WidgetKind::RecordingsFolder => {
                let starting = if self.draft.recordings_directory.is_empty() {
                    directories::UserDirs::new()
                        .and_then(|u| u.video_dir().map(|p| p.to_path_buf()))
                        .unwrap_or_else(std::env::temp_dir)
                } else {
                    PathBuf::from(&self.draft.recordings_directory)
                };
                if let Some(p) = rfd::FileDialog::new()
                    .set_title("Kashot — Recordings folder")
                    .set_directory(&starting)
                    .pick_folder()
                {
                    self.draft.recordings_directory = p.to_string_lossy().to_string();
                }
            }
            WidgetKind::ThemeCycle => {
                self.draft.theme = match self.draft.theme.as_str() {
                    "Dark" => "Light".to_owned(),
                    _      => "Dark".to_owned(),
                };
            }
            WidgetKind::WatermarkToggle => {
                self.draft.watermark_enabled = !self.draft.watermark_enabled;
            }
            WidgetKind::WatermarkText => {
                // Click handled in MouseInput branch — focusing the field.
                // Nothing extra to do here.
            }
            WidgetKind::WatermarkPos => {
                let next = match WatermarkAnchor::parse(&self.draft.watermark_position) {
                    WatermarkAnchor::TopLeft     => "TopRight",
                    WatermarkAnchor::TopRight    => "BottomRight",
                    WatermarkAnchor::BottomRight => "BottomLeft",
                    WatermarkAnchor::BottomLeft  => "TopLeft",
                };
                self.draft.watermark_position = next.to_owned();
            }
            WidgetKind::WatermarkOpacity => {
                // Click anywhere on the slider track → jump to that value;
                // hold-and-drag → continue tracking the cursor until mouse
                // release. The drag flag is cleared on `MouseInput::Released`.
                self.dragging_opacity = true;
                self.set_opacity_from_cursor_x();
            }
            WidgetKind::StartWithOs => {
                self.draft.start_with_windows = !self.draft.start_with_windows;
            }
            WidgetKind::OpenJson => {
                self.outcome = Some(SettingsOutcome::OpenJson);
                return;
            }
            WidgetKind::SaveBtn => {
                self.outcome = Some(SettingsOutcome::Saved(self.draft.clone()));
                return;
            }
            WidgetKind::CancelBtn => {
                self.outcome = Some(SettingsOutcome::Cancelled);
                return;
            }
        }
        self.window.request_redraw();
    }

    /// Lay out section headers + every row. Sections are spaced apart with
    /// a single section-rule line between them. Row rect spans the full
    /// content width; the value half is computed inside `render_row`.
    ///
    /// Action buttons (Save / Cancel / Edit as JSON) are pinned to the
    /// right side of the header band at the very top — clicking them is
    /// the user's commit/abort action so they want to be reachable
    /// without scrolling, like a real app's toolbar.
    fn build_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let x = PAD;
        let row_w = WIN_W as i32 - PAD * 2;

        // Header action bar — Edit-as-JSON sits leftmost on the right
        // side, then Cancel, then the primary Save button.
        let header_btn_y = (HEADER_H - BTN_H) / 2 + 4;
        let save_w   = 110;
        let cancel_w = 110;
        let edit_w   = 140;
        let save_x   = WIN_W as i32 - PAD - save_w;
        let cancel_x = save_x   - 10 - cancel_w;
        let edit_x   = cancel_x - 10 - edit_w;
        rows.push(Row { kind: WidgetKind::OpenJson,  label: "Edit as JSON", rect: (edit_x,   header_btn_y, edit_w,   BTN_H) });
        rows.push(Row { kind: WidgetKind::CancelBtn, label: "Cancel",       rect: (cancel_x, header_btn_y, cancel_w, BTN_H) });
        rows.push(Row { kind: WidgetKind::SaveBtn,   label: "Save",         rect: (save_x,   header_btn_y, save_w,   BTN_H) });

        // Content rows start below the header band + first section title.
        let mut y = HEADER_H + 14 + 18;

        // PATHS
        rows.push(Row { kind: WidgetKind::SaveFolder,       label: "Screenshots folder", rect: (x, y, row_w, ROW_H) }); y += ROW_H + 8;
        rows.push(Row { kind: WidgetKind::RecordingsFolder, label: "Recordings folder",  rect: (x, y, row_w, ROW_H) }); y += ROW_H + 22;

        // WATERMARK header takes 18px above its first row.
        y += 18;
        rows.push(Row { kind: WidgetKind::WatermarkToggle,  label: "Enabled",   rect: (x, y, row_w, ROW_H) }); y += ROW_H + 8;
        rows.push(Row { kind: WidgetKind::WatermarkText,    label: "Text",      rect: (x, y, row_w, ROW_H) }); y += ROW_H + 8;
        rows.push(Row { kind: WidgetKind::WatermarkPos,     label: "Position",  rect: (x, y, row_w, ROW_H) }); y += ROW_H + 8;
        rows.push(Row { kind: WidgetKind::WatermarkOpacity, label: "Opacity",   rect: (x, y, row_w, ROW_H) }); y += ROW_H + 22;

        // APPEARANCE
        y += 18;
        rows.push(Row { kind: WidgetKind::ThemeCycle,  label: "Theme",             rect: (x, y, row_w, ROW_H) }); y += ROW_H + 8;
        rows.push(Row { kind: WidgetKind::StartWithOs, label: "Start with system", rect: (x, y, row_w, ROW_H) }); y += ROW_H;

        let _ = y;
        rows
    }

    fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height))
            else { return; };
        if let Err(e) = self.surface.resize(w, h) {
            eprintln!("settings: surface.resize: {e}"); return;
        }
        let mut buf = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("settings: buffer_mut: {e}"); return; }
        };
        let win_w = w.get() as usize;
        let win_h = h.get() as usize;

        // Background — slim header band slightly lighter; body solid.
        for y in 0..win_h {
            let band = if (y as i32) < HEADER_H { BG_TOP } else { BG_BODY };
            for x in 0..win_w { buf[y * win_w + x] = band; }
        }
        // Header rule below the brand + action bar.
        h_line(&mut buf, win_w, win_h, 0, win_w as i32, HEADER_H, HEADER_RULE);
        let _ = FOOTER_H;

        let mut surf = BufferSurface { buf: &mut buf, w: win_w as i32, h: win_h as i32 };

        // Title strip.
        draw_text(&mut surf, PAD, 18, 2, "KASHOT // SETTINGS",  argb_to_kashot(LASER));
        draw_text(&mut surf, PAD, 44, 1, "Capture output, watermark and appearance.",
                  argb_to_kashot(TEXT_MUTED));

        // Section headers — anchored relative to their first row.
        if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::SaveFolder) {
            section_header(&mut surf, "PATHS", r.rect.1 - 22);
        }
        if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::WatermarkToggle) {
            section_header(&mut surf, "WATERMARK", r.rect.1 - 22);
        }
        if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::ThemeCycle) {
            section_header(&mut surf, "APPEARANCE", r.rect.1 - 22);
        }

        let caret_visible = ((self.caret_t.elapsed().as_millis() / CARET_BLINK_MS) % 2) == 0;
        for (i, row) in self.rows.iter().enumerate() {
            let hovered = self.hover == Some(i);
            let focused = self.focus == Some(i);
            render_row(&mut surf, row, hovered, focused, caret_visible, &self.draft);
        }

        if let Err(e) = buf.present() {
            eprintln!("settings: buf.present: {e}");
        }

        // When the watermark text field is focused, keep the caret blink
        // animation going by requesting another redraw after the next blink
        // frame would land.
        if self.focus.is_some() {
            self.window.request_redraw();
        }
    }

}

fn section_header<S: painter::Surface>(surf: &mut S, text: &str, y: i32) {
    draw_text(surf, PAD, y, 1, text, argb_to_kashot(SECTION_TINT));
    let tw = bitmap_font::measure(text, 1);
    // Right-side rule from after the section title to the content edge.
    let rule_x0 = PAD + tw + 10;
    let rule_x1 = WIN_W as i32 - PAD;
    let rule_y  = y + bitmap_font::GLYPH_H / 2;
    for x in rule_x0..rule_x1 {
        surf.write(x, rule_y, [
            ((HEADER_RULE >> 16) & 0xFF) as u8,
            ((HEADER_RULE >>  8) & 0xFF) as u8,
            ( HEADER_RULE        & 0xFF) as u8,
            0xFF,
        ]);
    }
}

fn render_row<S: painter::Surface>(
    surf: &mut S, row: &Row,
    hovered: bool, focused: bool, caret_visible: bool,
    draft: &AppSettings,
) {
    let (rx, ry, rw, rh) = row.rect;

    // ── Action-bar buttons ─────────────────────────────────────────────────
    if matches!(row.kind, WidgetKind::SaveBtn | WidgetKind::CancelBtn | WidgetKind::OpenJson) {
        let is_primary = row.kind == WidgetKind::SaveBtn;
        let border = if is_primary { LASER } else if hovered { LASER_DIM } else { PANEL_BORDER };
        let fill   = if is_primary && hovered { 0x0000_2818 } else if hovered { HOVER_FILL } else { 0x0000_0000 };
        if fill != 0 {
            fill_rect(surf, rx, ry, rw, rh, argb_to_kashot(fill));
        }
        stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_kashot(border));
        let tw = bitmap_font::measure(row.label, 1);
        let tx = rx + (rw - tw) / 2;
        let ty = ry + (rh - bitmap_font::GLYPH_H) / 2;
        let color = if is_primary { LASER } else { TEXT_BRIGHT };
        draw_text(surf, tx, ty, 1, row.label, argb_to_kashot(color));
        return;
    }

    // ── Setting rows ───────────────────────────────────────────────────────
    if hovered { fill_rect(surf, rx, ry, rw, rh, argb_to_kashot(HOVER_FILL)); }

    let label_y = ry + (rh - bitmap_font::GLYPH_H) / 2;
    draw_text(surf, rx + 6, label_y, 1, row.label, argb_to_kashot(TEXT_BRIGHT));

    // Geometry of the value half: starts right of the label column, ends at
    // the row edge. Folder rows reserve a "Browse…" button on the right.
    let val_x = rx + LABEL_W;
    let val_w = rw - LABEL_W - 4;
    let val_y = ry + 4;
    let val_h = rh - 8;

    match row.kind {
        WidgetKind::SaveFolder | WidgetKind::RecordingsFolder => {
            let browse_w = 80;
            let path_w   = val_w - browse_w - 8;
            // path field
            fill_rect(surf, val_x, val_y, path_w, val_h, argb_to_kashot(FIELD_BG));
            stroke_rect_argb(surf, val_x, val_y, path_w, val_h, argb_to_kashot(FIELD_BORDER));
            let val = match row.kind {
                // Pre-populate both folder fields with the actual resolved
                // path when no explicit value is set, so users see exactly
                // where output is going (matches what `save_directory` /
                // `recordings_directory_for` in tray_loop.rs pick at
                // runtime).
                WidgetKind::SaveFolder => folder_or_default_picture(&draft.save_directory),
                _                      => folder_or_default_video(&draft.recordings_directory),
            };
            let truncated = truncate_for(&val, path_w - 16);
            let ty = val_y + (val_h - bitmap_font::GLYPH_H) / 2;
            // Dim the path when it's the resolved fallback (no explicit value
            // in settings) so the user can tell at a glance "this hasn't been
            // customized yet".
            let user_set = match row.kind {
                WidgetKind::SaveFolder       => !draft.save_directory.is_empty(),
                WidgetKind::RecordingsFolder => !draft.recordings_directory.is_empty(),
                _                            => true,
            };
            let color = if user_set { TEXT_BRIGHT } else { TEXT_MUTED };
            draw_text(surf, val_x + 8, ty, 1, &truncated, argb_to_kashot(color));
            // Browse… button
            let bx = val_x + path_w + 8;
            let by = val_y;
            let bw = browse_w;
            let bh = val_h;
            let b_border = if hovered { LASER_DIM } else { FIELD_BORDER };
            stroke_rect_argb(surf, bx, by, bw, bh, argb_to_kashot(b_border));
            let label = "Browse…";
            let tw = bitmap_font::measure(label, 1);
            let tx = bx + (bw - tw) / 2;
            let ty = by + (bh - bitmap_font::GLYPH_H) / 2;
            draw_text(surf, tx, ty, 1, label, argb_to_kashot(TEXT_BRIGHT));
        }
        WidgetKind::WatermarkToggle | WidgetKind::StartWithOs => {
            let on = match row.kind {
                WidgetKind::WatermarkToggle => draft.watermark_enabled,
                _                            => draft.start_with_windows,
            };
            draw_toggle(surf, val_x, val_y, val_h, on);
        }
        WidgetKind::WatermarkText => {
            let border = if focused { FIELD_FOCUS } else { FIELD_BORDER };
            fill_rect(surf, val_x, val_y, val_w, val_h, argb_to_kashot(FIELD_BG));
            stroke_rect_argb(surf, val_x, val_y, val_w, val_h, argb_to_kashot(border));
            let text = &draft.watermark_text;
            let truncated = truncate_for(text, val_w - 16);
            let ty = val_y + (val_h - bitmap_font::GLYPH_H) / 2;
            let placeholder = text.is_empty();
            let shown = if placeholder { "(empty)" } else { truncated.as_str() };
            let color = if placeholder { TEXT_DIM } else { TEXT_BRIGHT };
            draw_text(surf, val_x + 8, ty, 1, shown, argb_to_kashot(color));
            if focused && caret_visible {
                let caret_x = val_x + 8 + bitmap_font::measure(&truncated, 1) + 1;
                let cy0 = val_y + 5;
                let cy1 = val_y + val_h - 5;
                for yy in cy0..cy1 {
                    surf.write(caret_x, yy, [
                        ((LASER >> 16) & 0xFF) as u8,
                        ((LASER >>  8) & 0xFF) as u8,
                        ( LASER        & 0xFF) as u8,
                        0xFF,
                    ]);
                }
            }
        }
        WidgetKind::WatermarkPos | WidgetKind::ThemeCycle => {
            // Cycle "pill": framed field + value + ↻ glyph on the right.
            fill_rect(surf, val_x, val_y, val_w, val_h, argb_to_kashot(FIELD_BG));
            stroke_rect_argb(surf, val_x, val_y, val_w, val_h, argb_to_kashot(FIELD_BORDER));
            let val = value_string(row.kind, draft);
            let ty = val_y + (val_h - bitmap_font::GLYPH_H) / 2;
            draw_text(surf, val_x + 8, ty, 1, &val, argb_to_kashot(TEXT_BRIGHT));
            // Cycle hint on the right edge — small text "↻" rendered as "[>]".
            let hint = "[>]";
            let tw = bitmap_font::measure(hint, 1);
            draw_text(surf, val_x + val_w - tw - 8, ty, 1, hint, argb_to_kashot(LASER));
        }
        WidgetKind::WatermarkOpacity => {
            // Draggable slider. Track is a thin dark groove with a brighter
            // "filled" segment from left up to the current value. The knob
            // is a 14×14 laser square that snaps to the current %.
            let val_label = format!("{}%", (draft.watermark_opacity * 100.0).round() as i32);
            let label_w   = bitmap_font::measure(&val_label, 1);
            // Reserve room on the right for the % readout.
            let track_pad_r = label_w + 14;
            let track_x = val_x + 4;
            let track_w = val_w - 8 - track_pad_r;
            let track_h = 6;
            let track_y = val_y + (val_h - track_h) / 2;
            fill_rect(surf, track_x, track_y, track_w, track_h, argb_to_kashot(FIELD_BG));
            stroke_rect_argb(surf, track_x, track_y, track_w, track_h, argb_to_kashot(FIELD_BORDER));
            let t = draft.watermark_opacity.clamp(0.0, 1.0);
            let fill_w = ((track_w as f32) * t).round() as i32;
            if fill_w > 0 {
                fill_rect(surf, track_x, track_y, fill_w, track_h, argb_to_kashot(LASER_DIM));
            }
            // Knob.
            let knob_w = 14;
            let knob_h = 14;
            let kx = track_x + ((track_w - 1) as f32 * t).round() as i32 - knob_w / 2;
            let kx = kx.clamp(track_x - 1, track_x + track_w - knob_w + 1);
            let ky = track_y + (track_h - knob_h) / 2;
            fill_rect(surf, kx, ky, knob_w, knob_h, argb_to_kashot(LASER));
            stroke_rect_argb(surf, kx, ky, knob_w, knob_h, argb_to_kashot(TEXT_BRIGHT));
            // Right-side %.
            let lx = track_x + track_w + 10;
            let ly = val_y + (val_h - bitmap_font::GLYPH_H) / 2;
            draw_text(surf, lx, ly, 1, &val_label, argb_to_kashot(TEXT_BRIGHT));
        }
        _ => {}
    }
}

/// Draw an ON/OFF toggle pill anchored at (x, y) with the given height; width
/// is fixed at ~64 px. Knob slides to the right when on.
fn draw_toggle<S: painter::Surface>(s: &mut S, x: i32, y: i32, h: i32, on: bool) {
    let w = 64;
    let bg = if on { TOGGLE_ON } else { TOGGLE_OFF };
    fill_rect(s, x, y, w, h, argb_to_kashot(bg));
    let border = if on { LASER_DIM } else { FIELD_BORDER };
    stroke_rect_argb(s, x, y, w, h, argb_to_kashot(border));
    // Knob — small filled square that sits on left when off, right when on.
    let knob_w = h - 8;
    let knob_x = if on { x + w - knob_w - 4 } else { x + 4 };
    let knob_y = y + 4;
    let knob_color = if on { TOGGLE_KNOB } else { TOGGLE_OFF_K };
    fill_rect(s, knob_x, knob_y, knob_w, knob_w, argb_to_kashot(knob_color));
    // Label inside, opposite side of the knob.
    let label = if on { "ON" } else { "OFF" };
    let tw = bitmap_font::measure(label, 1);
    let ty = y + (h - bitmap_font::GLYPH_H) / 2;
    let tx = if on { x + 8 } else { x + w - tw - 8 };
    let color = if on { TEXT_BRIGHT } else { TEXT_MUTED };
    draw_text(s, tx, ty, 1, label, argb_to_kashot(color));
}

fn value_string(kind: WidgetKind, draft: &AppSettings) -> String {
    match kind {
        WidgetKind::WatermarkPos     => human_position(&draft.watermark_position),
        WidgetKind::WatermarkOpacity => format!("{}%", (draft.watermark_opacity * 100.0).round() as i32),
        WidgetKind::ThemeCycle       => draft.theme.clone(),
        _                            => String::new(),
    }
}

fn human_position(raw: &str) -> String {
    match WatermarkAnchor::parse(raw) {
        WatermarkAnchor::TopLeft     => "Top left",
        WatermarkAnchor::TopRight    => "Top right",
        WatermarkAnchor::BottomLeft  => "Bottom left",
        WatermarkAnchor::BottomRight => "Bottom right",
    }.to_owned()
}

/// Resolve the screenshots-folder display value: explicit `save_directory`
/// when set, otherwise the actual XDG Pictures dir on disk (with a generic
/// `~/Pictures` fallback if no UserDirs are available).
fn folder_or_default_picture(v: &str) -> String {
    if !v.is_empty() { return v.to_owned(); }
    directories::UserDirs::new()
        .and_then(|u| u.picture_dir().map(|p| p.to_string_lossy().to_string()))
        .unwrap_or_else(|| "~/Pictures".to_owned())
}

/// Resolve the recordings-folder display value: explicit
/// `recordings_directory` when set, otherwise the actual XDG Videos dir.
/// Mirrors `recordings_directory_for` in tray_loop.rs so what the user sees
/// here is what the recorder will actually use.
fn folder_or_default_video(v: &str) -> String {
    if !v.is_empty() { return v.to_owned(); }
    directories::UserDirs::new()
        .and_then(|u| u.video_dir().map(|p| p.to_string_lossy().to_string()))
        .unwrap_or_else(|| "~/Videos".to_owned())
}

// ── tiny rendering helpers, scoped to this file ─────────────────────────────

struct BufferSurface<'a, 'b> {
    buf: &'a mut softbuffer::Buffer<'b, Rc<Window>, Rc<Window>>,
    w:   i32,
    h:   i32,
}

impl<'a, 'b> painter::Surface for BufferSurface<'a, 'b> {
    fn width(&self)  -> i32 { self.w }
    fn height(&self) -> i32 { self.h }
    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.w || y >= self.h { return [0, 0, 0, 0xFF]; }
        let p = self.buf[(y as usize) * (self.w as usize) + (x as usize)];
        [((p >> 16) & 0xFF) as u8, ((p >> 8) & 0xFF) as u8, (p & 0xFF) as u8, 0xFF]
    }
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h { return; }
        let dst = (y as usize) * (self.w as usize) + (x as usize);
        self.buf[dst] = ((rgba[0] as u32) << 16) | ((rgba[1] as u32) << 8) | rgba[2] as u32;
    }
}

fn argb_to_kashot(argb: u32) -> KashotRgba {
    KashotRgba {
        r: ((argb >> 16) & 0xFF) as u8,
        g: ((argb >>  8) & 0xFF) as u8,
        b: ( argb        & 0xFF) as u8,
        a: 255,
    }
}

fn h_line(
    buf: &mut softbuffer::Buffer<'_, Rc<Window>, Rc<Window>>,
    win_w: usize, win_h: usize,
    x0: i32, x1: i32, y: i32, color: u32,
) {
    if y < 0 || y as usize >= win_h { return; }
    let a = x0.max(0) as usize;
    let b = (x1 - 1).min(win_w as i32 - 1).max(0) as usize;
    for x in a..=b {
        buf[y as usize * win_w + x] = color;
    }
}

fn fill_rect<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, color: KashotRgba) {
    let rgba = [color.r, color.g, color.b, color.a];
    for yy in y..y + h {
        for xx in x..x + w {
            s.write(xx, yy, rgba);
        }
    }
}

fn stroke_rect_argb<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, color: KashotRgba) {
    let rgba = [color.r, color.g, color.b, color.a];
    for xx in x..x + w { s.write(xx, y, rgba); s.write(xx, y + h - 1, rgba); }
    for yy in y..y + h { s.write(x, yy, rgba); s.write(x + w - 1, yy, rgba); }
}

fn draw_text<S: painter::Surface>(s: &mut S, x: i32, y: i32, scale: i32, text: &str, color: KashotRgba) {
    painter::draw_text(s, x, y, scale, text, color);
}

fn centered_origin(loop_target: &winit::event_loop::ActiveEventLoop, w: u32, h: u32) -> (i32, i32) {
    let primary = loop_target.primary_monitor()
        .or_else(|| loop_target.available_monitors().next());
    let (mon_x, mon_y, mon_w, mon_h) = match primary {
        Some(m) => {
            let pos  = m.position();
            let size = m.size();
            (pos.x as i32, pos.y as i32, size.width as i32, size.height as i32)
        }
        None => (0, 0, 1920, 1080),
    };
    let x = mon_x + (mon_w - w as i32) / 2;
    let y = mon_y + (mon_h - h as i32) / 2;
    (x.max(mon_x), y.max(mon_y))
}

fn truncate_for(s: &str, max_px: i32) -> String {
    if bitmap_font::measure(s, 1) <= max_px { return s.to_owned(); }
    let ellipsis = "..";
    let ell_w = bitmap_font::measure(ellipsis, 1);
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = bitmap_font::measure(&ch.to_string(), 1);
        if w + cw + ell_w > max_px { break; }
        out.push(ch);
        w += cw;
    }
    out.push_str(ellipsis);
    out
}
