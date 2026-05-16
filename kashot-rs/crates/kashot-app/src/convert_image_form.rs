//! Themed "Convert image" dialog. Same skin as Settings / About / Updates.
//!
//! Flow:
//!   1. Pick a source image (Browse… opens a native file dialog).
//!   2. Pick a target format (PNG · JPG · WEBP · BMP).
//!   3. (Optional) drag the quality slider for JPG / WEBP.
//!   4. Click Convert → writes the result next to the source as
//!      `<stem>.kashot.<ext>` and shows a success message.
//!
//! Encoding is done entirely via the `image` crate, which is already a
//! workspace dep. No bundled binary required.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use kashot_core::color::Rgba as KashotRgba;

use crate::bitmap_font;
use crate::painter;

// ── colors — shared with the other themed dialogs ───────────────────────────
const BG_TOP:        u32 = 0x0008_0c0a;
const BG_BODY:       u32 = 0x000a_0e0c;
const HEADER_RULE:   u32 = 0x0014_2a1f;
const PANEL_BORDER:  u32 = 0x0014_2a1f;
const FIELD_BG:      u32 = 0x0006_0a08;
const FIELD_BORDER:  u32 = 0x001c_2e25;
const TEXT_BRIGHT:   u32 = 0x00e8_ffe8;
const TEXT_MUTED:    u32 = 0x009c_b0a4;
const TEXT_DIM:      u32 = 0x0068_7a70;
const SECTION_TINT:  u32 = 0x0066_ffb6;
const LASER:         u32 = 0x0000_ff95;
const LASER_DIM:     u32 = 0x0000_8050;
const HOVER_FILL:    u32 = 0x0010_2018;
const DANGER:        u32 = 0x00ff_7a6f;
const OK_TINT:       u32 = 0x004d_ffb0;

const WIN_W: u32 = 720;
const WIN_H: u32 = 620;
const PAD:   i32 = 24;
const ROW_H: i32 = 36;
const LABEL_W: i32 = 150;
const BTN_H: i32 = 34;
const HEADER_H: i32 = 88;
/// Tall label-only format pill (no LABEL_W indent).
const FMT_PILL_H: i32 = 46;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImgFormat { Png, Jpg, Webp, Bmp }

/// Image downscale preset. Downscale-only — picking 50 % on a 320 px source
/// halves it; "Source" leaves dimensions untouched. We don't expose upscale
/// because resampling-up on a screenshot just adds blur.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImgResize { Source, P50, P25 }

impl ImgFormat {
    fn label(&self) -> &'static str {
        match self {
            ImgFormat::Png  => "PNG",
            ImgFormat::Jpg  => "JPG",
            ImgFormat::Webp => "WEBP",
            ImgFormat::Bmp  => "BMP",
        }
    }
    fn ext(&self) -> &'static str {
        match self {
            ImgFormat::Png  => "png",
            ImgFormat::Jpg  => "jpg",
            ImgFormat::Webp => "webp",
            ImgFormat::Bmp  => "bmp",
        }
    }
    fn supports_quality(&self) -> bool {
        matches!(self, ImgFormat::Jpg | ImgFormat::Webp)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WidgetKind {
    Source,             // path display row + Browse button
    FormatPng,
    FormatJpg,
    FormatWebp,
    FormatBmp,
    Quality,            // slider
    ResizeSource,
    Resize50,
    Resize25,
    Convert,
    Close,
}

struct Row {
    kind:  WidgetKind,
    label: &'static str,
    rect:  (i32, i32, i32, i32),
}

#[derive(Clone)]
enum Status {
    Idle,
    Ok(PathBuf),
    Err(String),
}

pub enum ConvertImageOutcome {
    Closed,
}

pub struct ConvertImageView {
    window:  Rc<Window>,
    _ctx:    Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    source:  Option<PathBuf>,
    format:  ImgFormat,
    quality: u8,            // 0..=100 (JPG / WEBP)
    resize:  ImgResize,
    rows:    Vec<Row>,
    cursor:  (i32, i32),
    hover:   Option<usize>,
    dragging_quality: bool,
    status:  Status,
    pub outcome: Option<ConvertImageOutcome>,
}

impl ConvertImageView {
    pub fn new(loop_target: &ActiveEventLoop) -> Result<Self> {
        let (cx, cy) = centered_origin(loop_target, WIN_W, WIN_H);
        let attrs = WindowAttributes::default()
            .with_title("Kashot — Convert image")
            .with_decorations(true)
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(WIN_W, WIN_H))
            .with_position(PhysicalPosition::new(cx, cy))
            .with_window_icon(crate::brand_icon::shared());

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window (convert-image): {e}"))?;
        window.set_cursor(CursorIcon::Default);

        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new (convert-image): {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new (convert-image): {e}"))?;

        let mut me = ConvertImageView {
            window, _ctx: ctx, surface,
            source: None,
            format: ImgFormat::Png,
            quality: 90,
            resize:  ImgResize::Source,
            rows: Vec::new(),
            cursor: (0, 0),
            hover: None,
            dragging_quality: false,
            status: Status::Idle,
            outcome: None,
        };
        me.rows = me.build_rows();
        me.redraw();
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.outcome = Some(ConvertImageOutcome::Closed),
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: ElementState::Pressed, ..
                }, ..
            } => self.outcome = Some(ConvertImageOutcome::Closed),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as i32, position.y as i32);
                if self.dragging_quality {
                    self.set_quality_from_cursor();
                }
                let new_hover = self.hit_test(self.cursor.0, self.cursor.1);
                self.window.set_cursor(if new_hover.is_some() { CursorIcon::Pointer } else { CursorIcon::Default });
                if new_hover != self.hover {
                    self.hover = new_hover;
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left, ..
            } => {
                if let Some(i) = self.hit_test(self.cursor.0, self.cursor.1) {
                    self.activate(i);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left, ..
            } => {
                if self.dragging_quality {
                    self.dragging_quality = false;
                    self.window.request_redraw();
                }
            }
            WindowEvent::Resized(_) | WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn hit_test(&self, x: i32, y: i32) -> Option<usize> {
        self.rows.iter().position(|r| {
            // The QUALITY row is rendered + active only when the chosen
            // format supports a quality knob; otherwise it's invisible
            // and shouldn't catch clicks.
            if r.kind == WidgetKind::Quality && !self.format.supports_quality() {
                return false;
            }
            let (rx, ry, rw, rh) = r.rect;
            x >= rx && x < rx + rw && y >= ry && y < ry + rh
        })
    }

    fn activate(&mut self, idx: usize) {
        let kind = self.rows[idx].kind;
        match kind {
            WidgetKind::Source => {
                let starting = self.source.clone()
                    .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                    .unwrap_or_else(|| {
                        directories::UserDirs::new()
                            .and_then(|u| u.picture_dir().map(|p| p.to_path_buf()))
                            .unwrap_or_else(std::env::temp_dir)
                    });
                if let Some(p) = rfd::FileDialog::new()
                    .set_title("Pick an image")
                    .set_directory(&starting)
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif", "tiff", "tif"])
                    .pick_file()
                {
                    self.source = Some(p);
                    self.status = Status::Idle;
                }
            }
            WidgetKind::FormatPng  => { self.format = ImgFormat::Png;  self.status = Status::Idle; }
            WidgetKind::FormatJpg  => { self.format = ImgFormat::Jpg;  self.status = Status::Idle; }
            WidgetKind::FormatWebp => { self.format = ImgFormat::Webp; self.status = Status::Idle; }
            WidgetKind::FormatBmp  => { self.format = ImgFormat::Bmp;  self.status = Status::Idle; }
            WidgetKind::Quality => {
                self.dragging_quality = true;
                self.set_quality_from_cursor();
            }
            WidgetKind::ResizeSource => { self.resize = ImgResize::Source; }
            WidgetKind::Resize50     => { self.resize = ImgResize::P50;    }
            WidgetKind::Resize25     => { self.resize = ImgResize::P25;    }
            WidgetKind::Convert => {
                self.run_conversion();
            }
            WidgetKind::Close => {
                self.outcome = Some(ConvertImageOutcome::Closed);
                return;
            }
        }
        self.window.request_redraw();
    }

    fn quality_track(&self) -> Option<(i32, i32, i32, i32)> {
        let row = self.rows.iter().find(|r| r.kind == WidgetKind::Quality)?;
        let (rx, ry, rw, rh) = row.rect;
        let vx = rx + LABEL_W;
        let vy = ry + 4;
        let vw = rw - LABEL_W - 4;
        let vh = rh - 8;
        Some((vx, vy, vw, vh))
    }

    fn set_quality_from_cursor(&mut self) {
        let Some((tx, _ty, tw, _th)) = self.quality_track() else { return; };
        if tw <= 1 { return; }
        let cx = self.cursor.0;
        let mut t = (cx - tx) as f32 / (tw - 1) as f32;
        if !t.is_finite() { t = 0.0; }
        t = t.clamp(0.0, 1.0);
        self.quality = (t * 100.0).round() as u8;
        self.window.request_redraw();
    }

    /// Run the conversion. Best-effort — populates `self.status` for the
    /// next paint pass so the user gets feedback either way. Output path
    /// is `<input-stem>.kashot.<ext>` next to the source, which keeps the
    /// original untouched and is obvious enough that the user can find it.
    fn run_conversion(&mut self) {
        let Some(src) = self.source.clone() else {
            self.status = Status::Err("Pick a source image first.".to_owned());
            return;
        };
        let dst = {
            let stem = src.file_stem().map(|s| s.to_string_lossy().to_string())
                                       .unwrap_or_else(|| "kashot".to_owned());
            let parent = src.parent().map(|p| p.to_path_buf())
                                     .unwrap_or_else(std::env::temp_dir);
            parent.join(format!("{stem}.kashot.{}", self.format.ext()))
        };

        match image::open(&src) {
            Ok(img) => {
                // Apply the user's resize choice before we encode. Lanczos3
                // is the highest-quality downsample image-rs offers — worth
                // it because users will sometimes use this for shrinking a
                // 4K screenshot down to embed in a doc.
                let img = match self.resize {
                    ImgResize::Source => img,
                    ImgResize::P50 => img.resize(
                        (img.width()  / 2).max(1),
                        (img.height() / 2).max(1),
                        image::imageops::FilterType::Lanczos3,
                    ),
                    ImgResize::P25 => img.resize(
                        (img.width()  / 4).max(1),
                        (img.height() / 4).max(1),
                        image::imageops::FilterType::Lanczos3,
                    ),
                };
                let res = match self.format {
                    // PNG / BMP — use the trait save which infers the format
                    // from the extension; both encoders are always-on in the
                    // `image` crate.
                    ImgFormat::Png | ImgFormat::Bmp => img.save(&dst).map_err(|e| e.to_string()),
                    ImgFormat::Jpg => {
                        // JpegEncoder lets us pick a quality factor (1..=100).
                        let mut buf: Vec<u8> = Vec::new();
                        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, self.quality);
                        let rgb = img.to_rgb8();
                        enc.encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
                            .map_err(|e| e.to_string())
                            .and_then(|_| std::fs::write(&dst, &buf).map_err(|e| e.to_string()))
                    }
                    ImgFormat::Webp => {
                        // The `image` crate ships a lossless WEBP encoder.
                        // It doesn't expose a quality knob today; the slider
                        // is shown for parity but ignored for now.
                        img.save_with_format(&dst, image::ImageFormat::WebP).map_err(|e| e.to_string())
                    }
                };
                self.status = match res {
                    Ok(_)  => Status::Ok(dst),
                    Err(e) => Status::Err(format!("Encode failed: {e}")),
                };
            }
            Err(e) => self.status = Status::Err(format!("Couldn't open image: {e}")),
        }
    }

    fn build_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let row_w = WIN_W as i32 - PAD * 2;

        // Header action bar — Close on the right; Convert primary just left
        // of it so the user's commit lives in the header alongside the
        // other themed dialogs.
        let header_btn_y = (HEADER_H - BTN_H) / 2 + 4;
        let close_w   = 110;
        let convert_w = 140;
        let close_x   = WIN_W as i32 - PAD - close_w;
        let convert_x = close_x - 10 - convert_w;
        rows.push(Row { kind: WidgetKind::Close,   label: "Close",       rect: (close_x,   header_btn_y, close_w,   BTN_H) });
        rows.push(Row { kind: WidgetKind::Convert, label: "Convert now", rect: (convert_x, header_btn_y, convert_w, BTN_H) });

        // Section: SOURCE
        let mut y = HEADER_H + 14 + 18;
        rows.push(Row { kind: WidgetKind::Source, label: "Source image", rect: (PAD, y, row_w, ROW_H) });
        y += ROW_H + 22;

        // Section: FORMAT — four big pills spanning the full content width
        // (FORMAT section header already names the section; no left-column
        // indent needed).
        y += 18;
        let fmt_y = y;
        let fmt_gap = 14;
        let count = 4;
        let content_w = WIN_W as i32 - PAD * 2;
        let _ = content_w; // used both here and by RESIZE row below
        let fmt_w = (content_w - fmt_gap * (count - 1)) / count;
        let fmt_x0 = PAD;
        rows.push(Row { kind: WidgetKind::FormatPng,  label: "PNG",  rect: (fmt_x0 + 0 * (fmt_w + fmt_gap), fmt_y, fmt_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::FormatJpg,  label: "JPG",  rect: (fmt_x0 + 1 * (fmt_w + fmt_gap), fmt_y, fmt_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::FormatWebp, label: "WEBP", rect: (fmt_x0 + 2 * (fmt_w + fmt_gap), fmt_y, fmt_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::FormatBmp,  label: "BMP",  rect: (fmt_x0 + 3 * (fmt_w + fmt_gap), fmt_y, fmt_w, FMT_PILL_H) });
        y += FMT_PILL_H + 22;

        // Section: RESIZE (always available, downscale only).
        y += 18;
        let r_y = y;
        let r_gap = 14;
        let r_count = 3;
        let r_content_w = WIN_W as i32 - PAD * 2;
        let r_w = (r_content_w - r_gap * (r_count - 1)) / r_count;
        let r_x0 = PAD;
        rows.push(Row { kind: WidgetKind::ResizeSource, label: "Source", rect: (r_x0 + 0 * (r_w + r_gap), r_y, r_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::Resize50,     label: "50%",    rect: (r_x0 + 1 * (r_w + r_gap), r_y, r_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::Resize25,     label: "25%",    rect: (r_x0 + 2 * (r_w + r_gap), r_y, r_w, FMT_PILL_H) });
        y += FMT_PILL_H + 22;

        // Section: QUALITY — visible only when format supports it
        // (rendering + hit-test gate this row below).
        y += 18;
        rows.push(Row { kind: WidgetKind::Quality, label: "Quality", rect: (PAD, y, row_w, ROW_H) });
        rows
    }

    fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height))
            else { return; };
        if let Err(e) = self.surface.resize(w, h) { eprintln!("convert-image: surface.resize: {e}"); return; }
        let mut buf = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("convert-image: buffer_mut: {e}"); return; }
        };
        let win_w = w.get() as usize;
        let win_h = h.get() as usize;
        for y in 0..win_h {
            let band = if (y as i32) < HEADER_H { BG_TOP } else { BG_BODY };
            for x in 0..win_w { buf[y * win_w + x] = band; }
        }
        h_line(&mut buf, win_w, win_h, 0, win_w as i32, HEADER_H, HEADER_RULE);
        let _ = PANEL_BORDER;

        let mut surf = BufferSurface { buf: &mut buf, w: win_w as i32, h: win_h as i32 };

        // Title.
        draw_text(&mut surf, PAD, 22, 2, "KASHOT // CONVERT IMAGE", argb_to_kashot(LASER));
        draw_text(&mut surf, PAD, 50, 1, "Re-encode PNG / JPG / WEBP / BMP without leaving the tray.",
                  argb_to_kashot(TEXT_MUTED));

        // Section headers.
        if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::Source) {
            section_header(&mut surf, "SOURCE", r.rect.1 - 22);
        }
        if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::FormatPng) {
            section_header(&mut surf, "FORMAT", r.rect.1 - 22);
        }
        if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::ResizeSource) {
            section_header(&mut surf, "RESIZE  (downscale only)", r.rect.1 - 22);
        }
        // QUALITY section is meaningful only for formats with a quality
        // knob (JPG / WEBP). For PNG / BMP we skip both the section
        // header and the slider row entirely so the dialog doesn't show
        // a permanently-disabled control with an "n/a" placeholder.
        if self.format.supports_quality() {
            if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::Quality) {
                section_header(&mut surf, "QUALITY", r.rect.1 - 22);
            }
        }

        for (i, row) in self.rows.iter().enumerate() {
            if row.kind == WidgetKind::Quality && !self.format.supports_quality() { continue; }
            let hovered = self.hover == Some(i);
            render_row(&mut surf, row, hovered, &self.source, self.format, self.quality, self.resize);
        }

        // Status footer at the bottom.
        let footer_y = WIN_H as i32 - PAD - bitmap_font::GLYPH_H;
        match &self.status {
            Status::Idle => {}
            Status::Ok(path) => {
                let msg = format!("Saved: {}", path.display());
                draw_text(&mut surf, PAD, footer_y, 1, &msg, argb_to_kashot(OK_TINT));
            }
            Status::Err(e) => {
                draw_text(&mut surf, PAD, footer_y, 1, e, argb_to_kashot(DANGER));
            }
        }

        if let Err(e) = buf.present() { eprintln!("convert-image: buf.present: {e}"); }
    }
}

fn section_header<S: painter::Surface>(surf: &mut S, text: &str, y: i32) {
    draw_text(surf, PAD, y, 1, text, argb_to_kashot(SECTION_TINT));
    let tw = bitmap_font::measure(text, 1);
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
    hovered: bool,
    source: &Option<PathBuf>,
    format: ImgFormat,
    quality: u8,
    resize:  ImgResize,
) {
    let (rx, ry, rw, rh) = row.rect;

    // Action-bar buttons.
    if matches!(row.kind, WidgetKind::Convert | WidgetKind::Close) {
        let is_primary = row.kind == WidgetKind::Convert;
        let border = if is_primary { LASER } else if hovered { LASER_DIM } else { PANEL_BORDER };
        let fill   = if is_primary && hovered { 0x0000_2818 } else if hovered { HOVER_FILL } else { 0x0000_0000 };
        if fill != 0 { fill_rect(surf, rx, ry, rw, rh, argb_to_kashot(fill)); }
        stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_kashot(border));
        let tw = bitmap_font::measure(row.label, 1);
        let tx = rx + (rw - tw) / 2;
        let ty = ry + (rh - bitmap_font::GLYPH_H) / 2;
        let color = if is_primary { LASER } else { TEXT_BRIGHT };
        draw_text(surf, tx, ty, 1, row.label, argb_to_kashot(color));
        return;
    }

    // Format + resize pills — common look, common picker.
    let format_pill = matches!(row.kind,
        WidgetKind::FormatPng | WidgetKind::FormatJpg
        | WidgetKind::FormatWebp | WidgetKind::FormatBmp);
    let resize_pill = matches!(row.kind,
        WidgetKind::ResizeSource | WidgetKind::Resize50 | WidgetKind::Resize25);
    if format_pill || resize_pill {
        let selected = match row.kind {
            WidgetKind::FormatPng  => format == ImgFormat::Png,
            WidgetKind::FormatJpg  => format == ImgFormat::Jpg,
            WidgetKind::FormatWebp => format == ImgFormat::Webp,
            WidgetKind::FormatBmp  => format == ImgFormat::Bmp,
            WidgetKind::ResizeSource => resize == ImgResize::Source,
            WidgetKind::Resize50     => resize == ImgResize::P50,
            WidgetKind::Resize25     => resize == ImgResize::P25,
            _                         => false,
        };
        let border = if selected { LASER } else if hovered { LASER_DIM } else { FIELD_BORDER };
        let fill   = if selected { 0x000c_2820 } else if hovered { HOVER_FILL } else { FIELD_BG };
        fill_rect(surf, rx, ry, rw, rh, argb_to_kashot(fill));
        stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_kashot(border));
        // Format keeps the big 2x label; resize uses scale=1 so the
        // secondary control is visually subordinate to the format choice.
        let label_scale = if format_pill { 2 } else { 1 };
        let tw = bitmap_font::measure(row.label, label_scale);
        let tx = rx + (rw - tw) / 2;
        let ty = ry + (rh - bitmap_font::GLYPH_H * label_scale) / 2;
        let color = if selected { LASER } else { TEXT_BRIGHT };
        draw_text(surf, tx, ty, label_scale, row.label, argb_to_kashot(color));
        return;
    }

    // Setting rows (Source, Quality) — label on the left.
    if hovered && row.kind == WidgetKind::Source {
        fill_rect(surf, rx, ry, rw, rh, argb_to_kashot(HOVER_FILL));
    }
    let label_y = ry + (rh - bitmap_font::GLYPH_H) / 2;
    draw_text(surf, rx + 6, label_y, 1, row.label, argb_to_kashot(TEXT_BRIGHT));

    let val_x = rx + LABEL_W;
    let val_w = rw - LABEL_W - 4;
    let val_y = ry + 4;
    let val_h = rh - 8;

    match row.kind {
        WidgetKind::Source => {
            let browse_w = 90;
            let path_w   = val_w - browse_w - 8;
            fill_rect(surf, val_x, val_y, path_w, val_h, argb_to_kashot(FIELD_BG));
            stroke_rect_argb(surf, val_x, val_y, path_w, val_h, argb_to_kashot(FIELD_BORDER));
            let val = source.as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "(pick an image)".to_owned());
            let truncated = truncate_for(&val, path_w - 14);
            let ty = val_y + (val_h - bitmap_font::GLYPH_H) / 2;
            let color = if source.is_some() { TEXT_BRIGHT } else { TEXT_DIM };
            draw_text(surf, val_x + 7, ty, 1, &truncated, argb_to_kashot(color));
            let bx = val_x + path_w + 8;
            let by = val_y;
            let bw = browse_w;
            let bh = val_h;
            stroke_rect_argb(surf, bx, by, bw, bh, argb_to_kashot(if hovered { LASER_DIM } else { FIELD_BORDER }));
            let label = "Browse…";
            let tw = bitmap_font::measure(label, 1);
            let tx = bx + (bw - tw) / 2;
            let ty = by + (bh - bitmap_font::GLYPH_H) / 2;
            draw_text(surf, tx, ty, 1, label, argb_to_kashot(TEXT_BRIGHT));
        }
        WidgetKind::Quality => {
            let enabled = format.supports_quality();
            let label_color = if enabled { TEXT_BRIGHT } else { TEXT_DIM };
            draw_text(surf, rx + 6, label_y, 1, row.label, argb_to_kashot(label_color));
            let val_label = if enabled { format!("{}%", quality) } else { "n/a".to_owned() };
            let label_w   = bitmap_font::measure(&val_label, 1);
            let track_pad_r = label_w + 14;
            let track_x = val_x + 4;
            let track_w = val_w - 8 - track_pad_r;
            let track_h = 6;
            let track_y = val_y + (val_h - track_h) / 2;
            fill_rect(surf, track_x, track_y, track_w, track_h, argb_to_kashot(FIELD_BG));
            stroke_rect_argb(surf, track_x, track_y, track_w, track_h, argb_to_kashot(FIELD_BORDER));
            if enabled {
                let t = (quality as f32 / 100.0).clamp(0.0, 1.0);
                let fill_w = (track_w as f32 * t).round() as i32;
                if fill_w > 0 { fill_rect(surf, track_x, track_y, fill_w, track_h, argb_to_kashot(LASER_DIM)); }
                let knob_w = 14;
                let knob_h = 14;
                let kx = track_x + ((track_w - 1) as f32 * t).round() as i32 - knob_w / 2;
                let kx = kx.clamp(track_x - 1, track_x + track_w - knob_w + 1);
                let ky = track_y + (track_h - knob_h) / 2;
                fill_rect(surf, kx, ky, knob_w, knob_h, argb_to_kashot(LASER));
                stroke_rect_argb(surf, kx, ky, knob_w, knob_h, argb_to_kashot(TEXT_BRIGHT));
            }
            let lx = track_x + track_w + 10;
            let ly = val_y + (val_h - bitmap_font::GLYPH_H) / 2;
            draw_text(surf, lx, ly, 1, &val_label, argb_to_kashot(label_color));
        }
        _ => {}
    }
}

// ── tiny rendering helpers ──────────────────────────────────────────────────

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
    for x in a..=b { buf[y as usize * win_w + x] = color; }
}

fn fill_rect<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, color: KashotRgba) {
    let rgba = [color.r, color.g, color.b, color.a];
    for yy in y..y + h { for xx in x..x + w { s.write(xx, yy, rgba); } }
}

fn stroke_rect_argb<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, color: KashotRgba) {
    let rgba = [color.r, color.g, color.b, color.a];
    for xx in x..x + w { s.write(xx, y, rgba); s.write(xx, y + h - 1, rgba); }
    for yy in y..y + h { s.write(x, yy, rgba); s.write(x + w - 1, yy, rgba); }
}

fn draw_text<S: painter::Surface>(s: &mut S, x: i32, y: i32, scale: i32, text: &str, color: KashotRgba) {
    painter::draw_text(s, x, y, scale, text, color);
}

/// Compute the top-left corner that centers a `(w, h)` window on the
/// primary monitor. Falls back to (140, 140) if no monitor info is
/// available (headless, very early in WM bring-up, etc).
fn centered_origin(loop_target: &ActiveEventLoop, w: u32, h: u32) -> (i32, i32) {
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
