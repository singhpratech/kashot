//! Themed "Convert video" dialog. Same skin as Settings / About / Updates /
//! Convert image.
//!
//! Flow:
//!   1. Pick a source video file.
//!   2. Pick a target format (MP4 · MOV · WEBM · MKV · GIF).
//!   3. Click Convert → spawns ffmpeg in the background and writes the
//!      result next to the source as `<stem>.kashot.<ext>`.
//!
//! ffmpeg lookup, in order:
//!   1. `<dir-of-kashot-binary>/ffmpeg[.exe]`  (the bundled binary the
//!      installer drops next to ours — Linux .tar.gz, Windows MSI, macOS
//!      .app all carry one)
//!   2. `<dir-of-kashot-binary>/../Resources/ffmpeg` (macOS .app layout
//!      with the binary in `Contents/MacOS/` and the helper in
//!      `Contents/Resources/`)
//!   3. system `PATH` (last resort — user's own install)
//!
//! While ffmpeg runs the dialog shows "encoding…" with a moving dot; the
//! result text updates on completion.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Instant;

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

// Shared dialog palette.
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

const WIN_W: u32 = 760;
const WIN_H: u32 = 720;
const PAD:   i32 = 24;
const ROW_H: i32 = 36;
const LABEL_W: i32 = 150;
const BTN_H: i32 = 34;
const HEADER_H: i32 = 88;
// Tall, label-only pill for the format picker. Sized to fit one row inside
// the content area (no LABEL_W column reservation — the FORMAT section
// header already names it) so we can give each pill plenty of real estate.
const FMT_PILL_H: i32 = 46;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VidFormat { Mp4, Mov, Webm, Mkv, Gif }

/// Three-stop encode preset. Maps to libx264 / libvpx-vp9 knobs that real
/// users actually care about: file size vs visual quality.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VidQuality { Low, Med, High }

impl VidQuality {
    /// libx264 CRF (lower = better). 28 / 23 / 20 lines up with what the
    /// FFmpeg docs recommend as "visually-lossy / sane default / nearly
    /// indistinguishable".
    fn x264_crf(&self) -> &'static str { match self { Self::Low=>"28", Self::Med=>"23", Self::High=>"20" } }
    /// libx264 preset — speed/compression tradeoff.
    fn x264_preset(&self) -> &'static str { match self { Self::Low=>"fast", Self::Med=>"medium", Self::High=>"slow" } }
    /// libvpx-vp9 CRF. Range is 0..63; 38 / 32 / 28 mirrors the x264 stops.
    fn vp9_crf(&self) -> &'static str { match self { Self::Low=>"38", Self::Med=>"32", Self::High=>"28" } }
}

/// Output cap. Downscale-only — never upscale a smaller source. The
/// `min(iw, target)` trick in the ffmpeg expression handles that for us so
/// a 720p source picking "1080p" still comes out at 720p, no blurring.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VidResize { Source, P1080, P720, P480 }

impl VidResize {
    /// ffmpeg `-vf` scale expression that downscales to the cap on the
    /// long edge (width-clamped here since most desktop captures are
    /// landscape). `-2` for the other axis preserves aspect ratio AND
    /// keeps the dimension even, which several encoders require.
    fn scale_expr(&self) -> Option<&'static str> {
        match self {
            Self::Source => None,
            Self::P1080  => Some("scale='min(1920,iw)':'-2'"),
            Self::P720   => Some("scale='min(1280,iw)':'-2'"),
            Self::P480   => Some("scale='min(854,iw)':'-2'"),
        }
    }
}

/// GIF only — file size scales linearly with framerate so this knob has
/// real teeth there. We hide the row for other formats (their fps stays
/// at source).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GifFps { Eight, Twelve, TwentyFour }

impl GifFps {
    fn fps(&self) -> u32 { match self { Self::Eight=>8, Self::Twelve=>12, Self::TwentyFour=>24 } }
}

impl VidFormat {
    fn label(&self) -> &'static str {
        match self {
            VidFormat::Mp4  => "MP4",
            VidFormat::Mov  => "MOV",
            VidFormat::Webm => "WEBM",
            VidFormat::Mkv  => "MKV",
            VidFormat::Gif  => "GIF",
        }
    }
    fn ext(&self) -> &'static str {
        match self {
            VidFormat::Mp4  => "mp4",
            VidFormat::Mov  => "mov",
            VidFormat::Webm => "webm",
            VidFormat::Mkv  => "mkv",
            VidFormat::Gif  => "gif",
        }
    }

    /// Build the ffmpeg argv (minus the binary name itself) for converting
    /// `src` → `dst`. Quality preset + downscale cap are applied per
    /// container; the MKV path stream-copies (no re-encode), so quality
    /// and resize are silently ignored there — the user got a container
    /// swap and a fast path, no quality cost.
    fn ffmpeg_args(&self,
                   src: &str, dst: &str,
                   quality: VidQuality,
                   resize:  VidResize,
                   gif_fps: GifFps) -> Vec<String> {
        match self {
            VidFormat::Mp4 | VidFormat::Mov => {
                let mut args: Vec<String> = vec!["-y".into(), "-i".into(), src.into()];
                if let Some(scale) = resize.scale_expr() {
                    args.push("-vf".into());
                    args.push(scale.into());
                }
                args.extend([
                    "-c:v".into(), "libx264".into(),
                    "-preset".into(), quality.x264_preset().into(),
                    "-crf".into(),    quality.x264_crf().into(),
                    "-c:a".into(), "aac".into(), "-b:a".into(), "160k".into(),
                    "-movflags".into(), "+faststart".into(),
                    dst.into(),
                ]);
                args
            }
            VidFormat::Webm => {
                let mut args: Vec<String> = vec!["-y".into(), "-i".into(), src.into()];
                if let Some(scale) = resize.scale_expr() {
                    args.push("-vf".into());
                    args.push(scale.into());
                }
                args.extend([
                    "-c:v".into(), "libvpx-vp9".into(),
                    "-crf".into(), quality.vp9_crf().into(), "-b:v".into(), "0".into(),
                    "-c:a".into(), "libopus".into(), "-b:a".into(), "128k".into(),
                    dst.into(),
                ]);
                args
            }
            VidFormat::Mkv => vec![
                // Container swap. Stream-copy keeps quality + speed; the
                // quality/resize knobs are intentionally ignored — picking
                // MKV means "wrap as-is".
                "-y".into(), "-i".into(), src.into(),
                "-c".into(), "copy".into(),
                dst.into(),
            ],
            VidFormat::Gif => {
                // Build the filter chain: fps cap, optional scale cap, then
                // the standard two-pass palette pipeline that keeps GIFs
                // from looking like 1998.
                let mut filter = format!("fps={}", gif_fps.fps());
                if let Some(scale) = resize.scale_expr() {
                    filter.push(',');
                    filter.push_str(scale);
                }
                filter.push_str(",split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=sierra2_4a");
                vec![
                    "-y".into(), "-i".into(), src.into(),
                    "-vf".into(), filter,
                    dst.into(),
                ]
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WidgetKind {
    Source,
    FormatMp4,
    FormatMov,
    FormatWebm,
    FormatMkv,
    FormatGif,
    QualityLow,
    QualityMed,
    QualityHigh,
    ResizeSource,
    Resize1080,
    Resize720,
    Resize480,
    GifFps8,
    GifFps12,
    GifFps24,
    Convert,
    Close,
}

struct Row {
    kind:  WidgetKind,
    label: &'static str,
    rect:  (i32, i32, i32, i32),
}

enum Status {
    Idle,
    Running { since: Instant },
    Ok(PathBuf),
    Err(String),
}

pub enum ConvertVideoOutcome {
    Closed,
}

pub struct ConvertVideoView {
    window:  Rc<Window>,
    _ctx:    Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    source:  Option<PathBuf>,
    format:  VidFormat,
    quality: VidQuality,
    resize:  VidResize,
    gif_fps: GifFps,
    rows:    Vec<Row>,
    cursor:  (i32, i32),
    hover:   Option<usize>,
    status:  Status,
    rx:      Option<mpsc::Receiver<Result<PathBuf, String>>>,
    pub outcome: Option<ConvertVideoOutcome>,
}

impl ConvertVideoView {
    pub fn new(loop_target: &ActiveEventLoop) -> Result<Self> {
        let (cx, cy) = centered_origin(loop_target, WIN_W, WIN_H);
        let attrs = WindowAttributes::default()
            .with_title("KAShot — Convert video")
            .with_decorations(true)
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(WIN_W, WIN_H))
            .with_position(PhysicalPosition::new(cx, cy))
            .with_window_icon(crate::brand_icon::shared());

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window (convert-video): {e}"))?;
        window.set_cursor(CursorIcon::Default);

        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new (convert-video): {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new (convert-video): {e}"))?;

        let mut me = ConvertVideoView {
            window, _ctx: ctx, surface,
            source: None,
            format:  VidFormat::Mp4,
            quality: VidQuality::Med,
            resize:  VidResize::Source,
            gif_fps: GifFps::Twelve,
            rows: Vec::new(),
            cursor: (0, 0),
            hover: None,
            status: Status::Idle,
            rx: None,
            outcome: None,
        };
        me.rows = me.build_rows();
        me.redraw();
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    /// Called every poll tick from the tray loop so the running-animation
    /// keeps moving and the conversion result lands when it arrives.
    pub fn tick(&mut self) {
        if let Some(rx) = &self.rx {
            if let Ok(res) = rx.try_recv() {
                self.status = match res {
                    Ok(p)  => Status::Ok(p),
                    Err(e) => Status::Err(e),
                };
                self.rx = None;
                self.window.request_redraw();
            }
        }
        if matches!(self.status, Status::Running { .. }) {
            self.window.request_redraw();
        }
    }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.outcome = Some(ConvertVideoOutcome::Closed),
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: ElementState::Pressed, ..
                }, ..
            } => self.outcome = Some(ConvertVideoOutcome::Closed),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as i32, position.y as i32);
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
            WindowEvent::Resized(_) | WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn hit_test(&self, x: i32, y: i32) -> Option<usize> {
        let fmt = self.format;
        self.rows.iter().position(|r| {
            if !row_visible_for(fmt, r.kind) { return false; }
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
                            .and_then(|u| u.video_dir().map(|p| p.to_path_buf()))
                            .unwrap_or_else(std::env::temp_dir)
                    });
                if let Some(p) = rfd::FileDialog::new()
                    .set_title("Pick a video")
                    .set_directory(&starting)
                    .add_filter("Videos", &["mp4", "mov", "webm", "mkv", "avi", "m4v", "gif"])
                    .pick_file()
                {
                    self.source = Some(p);
                    self.status = Status::Idle;
                }
            }
            WidgetKind::FormatMp4  => { self.format = VidFormat::Mp4;  self.status = Status::Idle; }
            WidgetKind::FormatMov  => { self.format = VidFormat::Mov;  self.status = Status::Idle; }
            WidgetKind::FormatWebm => { self.format = VidFormat::Webm; self.status = Status::Idle; }
            WidgetKind::FormatMkv  => { self.format = VidFormat::Mkv;  self.status = Status::Idle; }
            WidgetKind::FormatGif  => { self.format = VidFormat::Gif;  self.status = Status::Idle; }
            WidgetKind::QualityLow  => { self.quality = VidQuality::Low;  }
            WidgetKind::QualityMed  => { self.quality = VidQuality::Med;  }
            WidgetKind::QualityHigh => { self.quality = VidQuality::High; }
            WidgetKind::ResizeSource => { self.resize = VidResize::Source; }
            WidgetKind::Resize1080   => { self.resize = VidResize::P1080;  }
            WidgetKind::Resize720    => { self.resize = VidResize::P720;   }
            WidgetKind::Resize480    => { self.resize = VidResize::P480;   }
            WidgetKind::GifFps8      => { self.gif_fps = GifFps::Eight;      }
            WidgetKind::GifFps12     => { self.gif_fps = GifFps::Twelve;     }
            WidgetKind::GifFps24     => { self.gif_fps = GifFps::TwentyFour; }
            WidgetKind::Convert => {
                self.start_conversion();
            }
            WidgetKind::Close => {
                self.outcome = Some(ConvertVideoOutcome::Closed);
                return;
            }
        }
        self.window.request_redraw();
    }

    fn start_conversion(&mut self) {
        if matches!(self.status, Status::Running { .. }) {
            return;
        }
        let Some(src) = self.source.clone() else {
            self.status = Status::Err("Pick a source video first.".to_owned());
            return;
        };
        let ffmpeg = match locate_ffmpeg() {
            Some(p) => p,
            None => {
                self.status = Status::Err("ffmpeg not found — bundle it next to kashot or install it on PATH.".to_owned());
                return;
            }
        };
        let dst = {
            let stem = src.file_stem().map(|s| s.to_string_lossy().to_string())
                                       .unwrap_or_else(|| "kashot".to_owned());
            let parent = src.parent().map(|p| p.to_path_buf())
                                     .unwrap_or_else(std::env::temp_dir);
            parent.join(format!("{stem}.kashot.{}", self.format.ext()))
        };
        let src_s = src.to_string_lossy().to_string();
        let dst_s = dst.to_string_lossy().to_string();
        let args = self.format.ffmpeg_args(&src_s, &dst_s, self.quality, self.resize, self.gif_fps);

        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.status = Status::Running { since: Instant::now() };

        let dst_thread = dst.clone();
        std::thread::spawn(move || {
            let res = Command::new(&ffmpeg)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output();
            let outcome = match res {
                Ok(out) if out.status.success() => Ok(dst_thread),
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let last_line = stderr.lines().last().unwrap_or("ffmpeg failed").to_owned();
                    Err(format!("ffmpeg: {last_line}"))
                }
                Err(e) => Err(format!("ffmpeg spawn failed: {e}")),
            };
            let _ = tx.send(outcome);
        });
    }

    fn build_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let row_w = WIN_W as i32 - PAD * 2;

        let header_btn_y = (HEADER_H - BTN_H) / 2 + 4;
        let close_w   = 110;
        let convert_w = 140;
        let close_x   = WIN_W as i32 - PAD - close_w;
        let convert_x = close_x - 10 - convert_w;
        rows.push(Row { kind: WidgetKind::Close,   label: "Close",       rect: (close_x,   header_btn_y, close_w,   BTN_H) });
        rows.push(Row { kind: WidgetKind::Convert, label: "Convert now", rect: (convert_x, header_btn_y, convert_w, BTN_H) });

        let mut y = HEADER_H + 14 + 18;
        rows.push(Row { kind: WidgetKind::Source, label: "Source video", rect: (PAD, y, row_w, ROW_H) });
        y += ROW_H + 22;

        y += 18;
        // FORMAT pills span the full content width — no left-column
        // reservation — so each pill is generous and the row reads as a
        // single visual unit. Five pills, evenly spaced with a 14 px gap.
        let fmt_y = y;
        let fmt_gap = 14;
        let count = 5;
        let content_w = WIN_W as i32 - PAD * 2;
        let fmt_w = (content_w - fmt_gap * (count - 1)) / count;
        let fmt_x0 = PAD;
        rows.push(Row { kind: WidgetKind::FormatMp4,  label: "MP4",  rect: (fmt_x0 + 0 * (fmt_w + fmt_gap), fmt_y, fmt_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::FormatMov,  label: "MOV",  rect: (fmt_x0 + 1 * (fmt_w + fmt_gap), fmt_y, fmt_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::FormatWebm, label: "WEBM", rect: (fmt_x0 + 2 * (fmt_w + fmt_gap), fmt_y, fmt_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::FormatMkv,  label: "MKV",  rect: (fmt_x0 + 3 * (fmt_w + fmt_gap), fmt_y, fmt_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::FormatGif,  label: "GIF",  rect: (fmt_x0 + 4 * (fmt_w + fmt_gap), fmt_y, fmt_w, FMT_PILL_H) });
        y += FMT_PILL_H + 22;

        // QUALITY (3 pills) — applies to MP4/MOV/WEBM/GIF. Hidden for MKV
        // (gated in render + hit_test) since MKV is a stream-copy path.
        y += 18;
        let q_y = y;
        let q_count = 3;
        let q_w = (content_w - fmt_gap * (q_count - 1)) / q_count;
        let q_x0 = PAD;
        rows.push(Row { kind: WidgetKind::QualityLow,  label: "Low",    rect: (q_x0 + 0 * (q_w + fmt_gap), q_y, q_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::QualityMed,  label: "Medium", rect: (q_x0 + 1 * (q_w + fmt_gap), q_y, q_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::QualityHigh, label: "High",   rect: (q_x0 + 2 * (q_w + fmt_gap), q_y, q_w, FMT_PILL_H) });
        y += FMT_PILL_H + 22;

        // RESIZE (4 pills) — downscale-only output cap. Always available.
        y += 18;
        let r_y = y;
        let r_count = 4;
        let r_w = (content_w - fmt_gap * (r_count - 1)) / r_count;
        let r_x0 = PAD;
        rows.push(Row { kind: WidgetKind::ResizeSource, label: "Source", rect: (r_x0 + 0 * (r_w + fmt_gap), r_y, r_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::Resize1080,   label: "1080p",  rect: (r_x0 + 1 * (r_w + fmt_gap), r_y, r_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::Resize720,    label: "720p",   rect: (r_x0 + 2 * (r_w + fmt_gap), r_y, r_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::Resize480,    label: "480p",   rect: (r_x0 + 3 * (r_w + fmt_gap), r_y, r_w, FMT_PILL_H) });
        y += FMT_PILL_H + 22;

        // GIF FPS (3 pills) — only meaningful for GIF (gated in render +
        // hit_test). Layout reserved so the dialog doesn't grow/shrink on
        // format change.
        y += 18;
        let g_y = y;
        let g_count = 3;
        let g_w = (content_w - fmt_gap * (g_count - 1)) / g_count;
        let g_x0 = PAD;
        rows.push(Row { kind: WidgetKind::GifFps8,  label: "8 fps",  rect: (g_x0 + 0 * (g_w + fmt_gap), g_y, g_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::GifFps12, label: "12 fps", rect: (g_x0 + 1 * (g_w + fmt_gap), g_y, g_w, FMT_PILL_H) });
        rows.push(Row { kind: WidgetKind::GifFps24, label: "24 fps", rect: (g_x0 + 2 * (g_w + fmt_gap), g_y, g_w, FMT_PILL_H) });
        rows
    }

    fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height))
            else { return; };
        if let Err(e) = self.surface.resize(w, h) { eprintln!("convert-video: surface.resize: {e}"); return; }
        let mut buf = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("convert-video: buffer_mut: {e}"); return; }
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

        draw_text(&mut surf, PAD, 22, 2, "KASHOT // CONVERT VIDEO", argb_to_kashot(LASER));
        draw_text(&mut surf, PAD, 50, 1, "Re-encode MP4 / MOV / WEBM / MKV / GIF via bundled ffmpeg.",
                  argb_to_kashot(TEXT_MUTED));

        if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::Source) {
            section_header(&mut surf, "SOURCE", r.rect.1 - 22);
        }
        if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::FormatMp4) {
            section_header(&mut surf, "FORMAT", r.rect.1 - 22);
        }
        // QUALITY is meaningful for everything except MKV (stream-copy).
        if self.format != VidFormat::Mkv {
            if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::QualityLow) {
                section_header(&mut surf, "QUALITY", r.rect.1 - 22);
            }
        }
        // RESIZE applies to MP4 / MOV / WEBM / GIF (but not MKV, which
        // stream-copies). Keep the header visible there too — the slot is
        // reserved so layout doesn't jump on format change.
        if self.format != VidFormat::Mkv {
            if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::ResizeSource) {
                section_header(&mut surf, "RESIZE  (downscale only)", r.rect.1 - 22);
            }
        }
        // GIF FPS section header is only meaningful when the user actually
        // picked GIF — the other formats keep source framerate.
        if self.format == VidFormat::Gif {
            if let Some(r) = self.rows.iter().find(|r| r.kind == WidgetKind::GifFps8) {
                section_header(&mut surf, "GIF FRAME RATE", r.rect.1 - 22);
            }
        }

        // Cache format ahead of the borrow loop — `row_visible` takes &self
        // and we'd otherwise alias the in-flight `&mut buf` mutable borrow.
        let format  = self.format;
        let quality = self.quality;
        let resize  = self.resize;
        let gif_fps = self.gif_fps;
        for (i, row) in self.rows.iter().enumerate() {
            if !row_visible_for(format, row.kind) { continue; }
            let hovered = self.hover == Some(i);
            render_row(&mut surf, row, hovered, &self.source, format, quality, resize, gif_fps);
        }

        // Status footer.
        let footer_y = WIN_H as i32 - PAD - bitmap_font::GLYPH_H;
        match &self.status {
            Status::Idle => {}
            Status::Running { since } => {
                let dots = (since.elapsed().as_millis() / 400) % 4;
                let dots_s: String = std::iter::repeat('.').take(dots as usize).collect();
                let secs = since.elapsed().as_secs();
                let msg = format!("encoding{dots_s}  ({secs}s elapsed — kashot keeps running, this is a background job)");
                draw_text(&mut surf, PAD, footer_y, 1, &msg, argb_to_kashot(TEXT_MUTED));
            }
            Status::Ok(path) => {
                let msg = format!("Saved: {}", path.display());
                draw_text(&mut surf, PAD, footer_y, 1, &msg, argb_to_kashot(OK_TINT));
            }
            Status::Err(e) => {
                draw_text(&mut surf, PAD, footer_y, 1, e, argb_to_kashot(DANGER));
            }
        }

        if let Err(e) = buf.present() { eprintln!("convert-video: buf.present: {e}"); }
    }
}

/// Find a usable ffmpeg. Order: next to our binary (installer bundle),
/// macOS `.app/Contents/Resources/ffmpeg`, then `PATH`. Returns the full
/// path so we can pass it straight to `Command::new`.
fn locate_ffmpeg() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let bundle_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let next_to_us = dir.join(bundle_name);
    if next_to_us.is_file() { return Some(next_to_us); }

    // macOS .app layout — `Contents/MacOS/kashot` → `Contents/Resources/ffmpeg`.
    if cfg!(target_os = "macos") {
        if let Some(contents) = dir.parent() {
            let mac_resources = contents.join("Resources").join("ffmpeg");
            if mac_resources.is_file() { return Some(mac_resources); }
        }
    }

    // System PATH.
    if let Ok(path_var) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ";" } else { ":" };
        for p in path_var.split(sep) {
            let candidate = std::path::Path::new(p).join(bundle_name);
            if candidate.is_file() { return Some(candidate); }
        }
    }
    None
}

/// Whether a given widget is currently relevant for the chosen format.
/// Hidden rows neither paint nor catch clicks — keeps the dialog from
/// showing controls that wouldn't do anything (MKV doesn't re-encode, only
/// GIF cares about frame rate). Free function so we can use it from
/// `redraw` while a mutable borrow on the framebuffer is alive.
fn row_visible_for(format: VidFormat, kind: WidgetKind) -> bool {
    match kind {
        WidgetKind::QualityLow | WidgetKind::QualityMed | WidgetKind::QualityHigh
            => format != VidFormat::Mkv,
        WidgetKind::ResizeSource | WidgetKind::Resize1080
        | WidgetKind::Resize720  | WidgetKind::Resize480
            => format != VidFormat::Mkv,
        WidgetKind::GifFps8 | WidgetKind::GifFps12 | WidgetKind::GifFps24
            => format == VidFormat::Gif,
        _   => true,
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
    format: VidFormat,
    quality: VidQuality,
    resize:  VidResize,
    gif_fps: GifFps,
) {
    let (rx, ry, rw, rh) = row.rect;

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

    // Generic "is this the currently-selected pill" check for every pill
    // group. Keeps the render branch below simple.
    let selected = match row.kind {
        WidgetKind::FormatMp4  => format == VidFormat::Mp4,
        WidgetKind::FormatMov  => format == VidFormat::Mov,
        WidgetKind::FormatWebm => format == VidFormat::Webm,
        WidgetKind::FormatMkv  => format == VidFormat::Mkv,
        WidgetKind::FormatGif  => format == VidFormat::Gif,
        WidgetKind::QualityLow  => quality == VidQuality::Low,
        WidgetKind::QualityMed  => quality == VidQuality::Med,
        WidgetKind::QualityHigh => quality == VidQuality::High,
        WidgetKind::ResizeSource => resize == VidResize::Source,
        WidgetKind::Resize1080   => resize == VidResize::P1080,
        WidgetKind::Resize720    => resize == VidResize::P720,
        WidgetKind::Resize480    => resize == VidResize::P480,
        WidgetKind::GifFps8      => gif_fps == GifFps::Eight,
        WidgetKind::GifFps12     => gif_fps == GifFps::Twelve,
        WidgetKind::GifFps24     => gif_fps == GifFps::TwentyFour,
        _                         => false,
    };
    let is_pill = !matches!(row.kind, WidgetKind::Source);
    if is_pill {
        let border = if selected { LASER } else if hovered { LASER_DIM } else { FIELD_BORDER };
        let fill   = if selected { 0x000c_2820 } else if hovered { HOVER_FILL } else { FIELD_BG };
        fill_rect(surf, rx, ry, rw, rh, argb_to_kashot(fill));
        stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_kashot(border));
        // Big labels for format; slightly smaller for the secondary
        // quality/resize/fps rows so they don't compete visually with the
        // primary format choice.
        let label_scale = if matches!(row.kind,
            WidgetKind::FormatMp4 | WidgetKind::FormatMov
            | WidgetKind::FormatWebm | WidgetKind::FormatMkv | WidgetKind::FormatGif)
        { 2 } else { 1 };
        let tw = bitmap_font::measure(row.label, label_scale);
        let tx = rx + (rw - tw) / 2;
        let ty = ry + (rh - bitmap_font::GLYPH_H * label_scale) / 2;
        let color = if selected { LASER } else { TEXT_BRIGHT };
        draw_text(surf, tx, ty, label_scale, row.label, argb_to_kashot(color));
        return;
    }

    if hovered && row.kind == WidgetKind::Source {
        fill_rect(surf, rx, ry, rw, rh, argb_to_kashot(HOVER_FILL));
    }
    let label_y = ry + (rh - bitmap_font::GLYPH_H) / 2;
    draw_text(surf, rx + 6, label_y, 1, row.label, argb_to_kashot(TEXT_BRIGHT));

    let val_x = rx + LABEL_W;
    let val_w = rw - LABEL_W - 4;
    let val_y = ry + 4;
    let val_h = rh - 8;

    if row.kind == WidgetKind::Source {
        let browse_w = 90;
        let path_w   = val_w - browse_w - 8;
        fill_rect(surf, val_x, val_y, path_w, val_h, argb_to_kashot(FIELD_BG));
        stroke_rect_argb(surf, val_x, val_y, path_w, val_h, argb_to_kashot(FIELD_BORDER));
        let val = source.as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "(pick a video)".to_owned());
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
