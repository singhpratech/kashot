//! Themed "Check for updates" dialog — same skin as the Settings + About
//! windows. Shows the installed version and the latest GitHub release tag,
//! with a one-click button to open the release page in the user's browser.
//!
//! Network fetch is fire-and-forget on a background thread. While it's in
//! flight the dialog shows "checking…"; on success it shows the result;
//! on failure it shows a polite error and keeps the manual "Open releases
//! page" button working so the user has an out.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

const BG_TOP:        u32 = 0x0008_0c0a;
const BG_BODY:       u32 = 0x000a_0e0c;
const HEADER_RULE:   u32 = 0x0014_2a1f;
const PANEL_BORDER:  u32 = 0x0014_2a1f;
const TEXT_BRIGHT:   u32 = 0x00e8_ffe8;
const TEXT_MUTED:    u32 = 0x009c_b0a4;
const SECTION_TINT:  u32 = 0x0066_ffb6;
const LASER:         u32 = 0x0000_ff95;
const LASER_DIM:     u32 = 0x0000_8050;
const HOVER_FILL:    u32 = 0x0010_2018;
const DANGER:        u32 = 0x00ff_7a6f;

const WIN_W: u32 = 480;
const WIN_H: u32 = 320;
const PAD:   i32 = 22;
const BTN_H: i32 = 30;
const HEADER_H: i32 = 84;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BtnKind { OpenReleases, Close }

struct Btn {
    kind:  BtnKind,
    label: &'static str,
    rect:  (i32, i32, i32, i32),
}

enum FetchState {
    Pending,
    Found { tag: String, has_update: bool },
    Error(String),
}

pub enum UpdatesOutcome {
    Closed,
    OpenReleases,
}

pub struct UpdatesView {
    window:  Rc<Window>,
    _ctx:    Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    btns:    Vec<Btn>,
    cursor:  (i32, i32),
    hover:   Option<usize>,
    started: Instant,
    state:   FetchState,
    rx:      Option<mpsc::Receiver<Result<String, String>>>,
    pub outcome: Option<UpdatesOutcome>,
}

impl UpdatesView {
    pub fn new(loop_target: &ActiveEventLoop) -> Result<Self> {
        let (cx, cy) = centered_origin(loop_target, WIN_W, WIN_H);
        let attrs = WindowAttributes::default()
            .with_title("Kashot — Updates")
            .with_decorations(true)
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(WIN_W, WIN_H))
            .with_position(PhysicalPosition::new(cx, cy))
            .with_window_icon(crate::brand_icon::shared());

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window (updates): {e}"))?;

        window.set_cursor(CursorIcon::Default);
        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new (updates): {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new (updates): {e}"))?;

        // Kick off the background fetch immediately so by the time the user
        // looks at the dialog there's usually already a result. `ureq`
        // isn't a workspace dep — we shell out to `curl` instead, which is
        // available on every desktop OS we ship for (curl is preinstalled
        // on macOS 10.15+, Windows 10 build 17063+, every modern Linux).
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let res = fetch_latest_tag();
            let _ = tx.send(res);
        });

        let mut me = UpdatesView {
            window, _ctx: ctx, surface,
            btns: Vec::new(),
            cursor: (0, 0),
            hover: None,
            started: Instant::now(),
            state: FetchState::Pending,
            rx: Some(rx),
            outcome: None,
        };
        me.btns = me.build_btns();
        me.redraw();
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    /// Called from the tray-loop poll tick so we can advance the
    /// "checking…" animation and pick up the fetch result when it
    /// arrives.
    pub fn tick(&mut self) {
        if let Some(rx) = &self.rx {
            if let Ok(res) = rx.try_recv() {
                self.state = match res {
                    Ok(tag) => {
                        let has_update = !same_version(&tag, env!("CARGO_PKG_VERSION"));
                        FetchState::Found { tag, has_update }
                    }
                    Err(e) => FetchState::Error(e),
                };
                self.rx = None;
                self.window.request_redraw();
            }
        }
        // Keep the dot-dot-dot animation moving while we're waiting.
        if matches!(self.state, FetchState::Pending) {
            self.window.request_redraw();
        }
    }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.outcome = Some(UpdatesOutcome::Closed),
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter),
                    state: ElementState::Pressed, ..
                }, ..
            } => self.outcome = Some(UpdatesOutcome::Closed),
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
                    self.outcome = Some(match self.btns[i].kind {
                        BtnKind::OpenReleases => UpdatesOutcome::OpenReleases,
                        BtnKind::Close        => UpdatesOutcome::Closed,
                    });
                }
            }
            WindowEvent::Resized(_) | WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn hit_test(&self, x: i32, y: i32) -> Option<usize> {
        self.btns.iter().position(|b| {
            let (bx, by, bw, bh) = b.rect;
            x >= bx && x < bx + bw && y >= by && y < by + bh
        })
    }

    fn build_btns(&self) -> Vec<Btn> {
        let mut btns = Vec::new();
        let header_btn_y = (HEADER_H - BTN_H) / 2 + 4;
        let close_w = 110;
        let close_x = WIN_W as i32 - PAD - close_w;
        btns.push(Btn { kind: BtnKind::Close, label: "Close", rect: (close_x, header_btn_y, close_w, BTN_H) });

        let bw = 220;
        let bh = 36;
        let bx = (WIN_W as i32 - bw) / 2;
        let by = WIN_H as i32 - PAD - bh;
        btns.push(Btn { kind: BtnKind::OpenReleases, label: "Open releases page", rect: (bx, by, bw, bh) });
        btns
    }

    fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height))
            else { return; };
        if let Err(e) = self.surface.resize(w, h) { eprintln!("updates: surface.resize: {e}"); return; }
        let mut buf = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("updates: buffer_mut: {e}"); return; }
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

        // Title strip.
        draw_text(&mut surf, PAD, 22, 2, "KASHOT // UPDATES",   argb_to_kashot(LASER));
        draw_text(&mut surf, PAD, 50, 1, "Check for new releases on GitHub.",
                  argb_to_kashot(TEXT_MUTED));

        // Body — current + latest.
        let mut y = HEADER_H + 28;
        draw_text(&mut surf, PAD, y, 1, "INSTALLED",  argb_to_kashot(SECTION_TINT));
        let installed = format!("v{}", env!("CARGO_PKG_VERSION"));
        draw_text(&mut surf, PAD + 120, y, 1, &installed, argb_to_kashot(TEXT_BRIGHT));
        y += 24;
        draw_text(&mut surf, PAD, y, 1, "LATEST",     argb_to_kashot(SECTION_TINT));

        match &self.state {
            FetchState::Pending => {
                let dots = (self.started.elapsed().as_millis() / 400) % 4;
                let dots_s: String = std::iter::repeat('.').take(dots as usize).collect();
                let s = format!("checking{}", dots_s);
                draw_text(&mut surf, PAD + 120, y, 1, &s, argb_to_kashot(TEXT_MUTED));
            }
            FetchState::Found { tag, has_update } => {
                draw_text(&mut surf, PAD + 120, y, 1, tag, argb_to_kashot(TEXT_BRIGHT));
                y += 28;
                let (label, tint) = if *has_update {
                    ("A newer build is available — open the releases page to download.", LASER)
                } else {
                    ("You're on the latest build. Nothing to do.", TEXT_MUTED)
                };
                draw_text(&mut surf, PAD, y, 1, label, argb_to_kashot(tint));
            }
            FetchState::Error(e) => {
                draw_text(&mut surf, PAD + 120, y, 1, "unavailable", argb_to_kashot(DANGER));
                y += 28;
                let msg = format!("Couldn't reach GitHub: {}", e);
                draw_text(&mut surf, PAD, y, 1, &msg, argb_to_kashot(TEXT_MUTED));
            }
        }

        for (i, b) in self.btns.iter().enumerate() {
            let hovered = self.hover == Some(i);
            render_btn(&mut surf, b, hovered);
        }

        if let Err(e) = buf.present() { eprintln!("updates: buf.present: {e}"); }
    }
}

/// Shell out to `curl` (always-present on Linux / macOS / Windows 10+).
/// Returns the `tag_name` field from the latest-release JSON, e.g. `v0.1`.
fn fetch_latest_tag() -> Result<String, String> {
    let url = "https://api.github.com/repos/singhpratech/kashot/releases/latest";
    let out = std::process::Command::new("curl")
        .args([
            "-sS", "-A", "kashot-updater",
            "--max-time", "8",
            "-H", "Accept: application/vnd.github+json",
            url,
        ])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl exit {}", out.status));
    }
    let body = String::from_utf8_lossy(&out.stdout);
    // Tiny "find the tag_name field" — avoids pulling serde_json in for one
    // string. Looks for `"tag_name":"..."` and lifts the value.
    let needle = "\"tag_name\":";
    let i = body.find(needle).ok_or_else(|| "tag_name missing from response".to_owned())?;
    let after = &body[i + needle.len()..];
    let start = after.find('"').ok_or_else(|| "tag_name has no opening quote".to_owned())?;
    let rest  = &after[start + 1..];
    let end   = rest.find('"').ok_or_else(|| "tag_name has no closing quote".to_owned())?;
    Ok(rest[..end].to_owned())
}

/// `tag_name` from GitHub may be "v0.1" or "0.1" or "v0.1.0"; the embedded
/// CARGO_PKG_VERSION is always plain "0.1.0". Strip "v" prefixes and trailing
/// ".0" tails before comparing so the obvious shapes match.
fn same_version(tag: &str, pkg: &str) -> bool {
    fn norm(s: &str) -> String {
        let s = s.trim().trim_start_matches('v').trim_start_matches('V');
        let mut parts: Vec<&str> = s.split('.').collect();
        while parts.last().map(|p| *p == "0").unwrap_or(false) && parts.len() > 1 {
            parts.pop();
        }
        parts.join(".")
    }
    norm(tag) == norm(pkg)
}

fn render_btn<S: painter::Surface>(surf: &mut S, b: &Btn, hovered: bool) {
    let (x, y, w, h) = b.rect;
    let is_primary = b.kind == BtnKind::Close;
    let border = if is_primary { LASER } else if hovered { LASER_DIM } else { PANEL_BORDER };
    let fill   = if is_primary && hovered { 0x0000_2818 } else if hovered { HOVER_FILL } else { 0x0000_0000 };
    if fill != 0 { fill_rect(surf, x, y, w, h, argb_to_kashot(fill)); }
    stroke_rect_argb(surf, x, y, w, h, argb_to_kashot(border));
    let tw = bitmap_font::measure(b.label, 1);
    let tx = x + (w - tw) / 2;
    let ty = y + (h - bitmap_font::GLYPH_H) / 2;
    let color = if is_primary { LASER } else { TEXT_BRIGHT };
    draw_text(surf, tx, ty, 1, b.label, argb_to_kashot(color));
}

// ── tiny rendering helpers (same shape as settings_form) ────────────────────

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

// Quiet unused-imports warnings for items kept around for parity with the
// other dialog modules.
fn _quiet() { let _ = Duration::from_secs(0); }
