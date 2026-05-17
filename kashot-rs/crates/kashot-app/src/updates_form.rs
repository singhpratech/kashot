//! Themed "Check for updates" dialog — same skin as the Settings + About
//! windows. Shows the installed version, the latest GitHub release tag,
//! the release date, the release notes (markdown body, rendered as
//! word-wrapped plain text in a scrollable pane) and a platform-aware
//! one-click Download button for the matching binary asset. Falls back to
//! "Open releases page" when no asset matches.
//!
//! Network fetch is fire-and-forget on a background thread. While it's in
//! flight the dialog shows "checking…"; on success it shows the parsed
//! release; on failure it shows a polite error and keeps the manual
//! "Open releases page" button working so the user has an out.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
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
const PANEL_FILL:    u32 = 0x0006_0a08;
const TEXT_BRIGHT:   u32 = 0x00e8_ffe8;
const TEXT_MUTED:    u32 = 0x009c_b0a4;
const SECTION_TINT:  u32 = 0x0066_ffb6;
const LASER:         u32 = 0x0000_ff95;
const LASER_DIM:     u32 = 0x0000_8050;
const HOVER_FILL:    u32 = 0x0010_2018;
const DANGER:        u32 = 0x00ff_7a6f;
const SCROLLBAR_BG:  u32 = 0x000c_1612;
const SCROLLBAR_FG:  u32 = 0x0000_8050;

const WIN_W: u32 = 560;
const WIN_H: u32 = 460;
const PAD:   i32 = 22;
const BTN_H: i32 = 30;
const HEADER_H: i32 = 84;
const LINE_H:   i32 = bitmap_font::GLYPH_H + 5;
const NOTES_TOP_OFFSET: i32 = 124;
const NOTES_BOTTOM_GAP: i32 = 70;
const SCROLLBAR_W: i32 = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BtnKind { OpenReleases, Download, Close }

struct Btn {
    kind:  BtnKind,
    label: String,
    rect:  (i32, i32, i32, i32),
}

struct ReleaseInfo {
    tag:        String,
    date:       String,
    has_update: bool,
    notes:      Vec<String>,
    asset_url:  Option<String>,
}

enum FetchState {
    Pending,
    Found(ReleaseInfo),
    Error(String),
}

pub enum UpdatesOutcome {
    Closed,
    OpenReleases,
    OpenAsset(String),
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
    rx:      Option<mpsc::Receiver<Result<RawRelease, String>>>,
    scroll:  i32,
    notes_rect: (i32, i32, i32, i32),
    pub outcome: Option<UpdatesOutcome>,
}

impl UpdatesView {
    pub fn new(loop_target: &ActiveEventLoop) -> Result<Self> {
        let (cx, cy) = centered_origin(loop_target, WIN_W, WIN_H);
        let attrs = WindowAttributes::default()
            .with_title("KAShot — Updates")
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
            let res = fetch_latest_release();
            let _ = tx.send(res);
        });

        let notes_rect = (
            PAD,
            HEADER_H + NOTES_TOP_OFFSET,
            WIN_W as i32 - PAD * 2,
            WIN_H as i32 - HEADER_H - NOTES_TOP_OFFSET - NOTES_BOTTOM_GAP,
        );

        let mut me = UpdatesView {
            window, _ctx: ctx, surface,
            btns: Vec::new(),
            cursor: (0, 0),
            hover: None,
            started: Instant::now(),
            state: FetchState::Pending,
            rx: Some(rx),
            scroll: 0,
            notes_rect,
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
                    Ok(raw) => {
                        let has_update = !same_version(&raw.tag_name, env!("CARGO_PKG_VERSION"));
                        let date = parse_iso_date(&raw.published_at);
                        let notes = wrap_body(
                            &strip_markdown(&raw.body),
                            self.notes_rect.2 - SCROLLBAR_W - 8,
                        );
                        let asset_url = pick_asset_url(&raw.assets);
                        FetchState::Found(ReleaseInfo {
                            tag: raw.tag_name,
                            date,
                            has_update,
                            notes,
                            asset_url,
                        })
                    }
                    Err(e) => FetchState::Error(e),
                };
                self.rx = None;
                self.btns = self.build_btns();
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
                    logical_key, state: ElementState::Pressed, ..
                }, ..
            } => match logical_key {
                Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter) => {
                    self.outcome = Some(UpdatesOutcome::Closed);
                }
                Key::Named(NamedKey::PageDown) => self.scroll_by(LINE_H * 6),
                Key::Named(NamedKey::PageUp)   => self.scroll_by(-LINE_H * 6),
                Key::Named(NamedKey::ArrowDown) => self.scroll_by(LINE_H),
                Key::Named(NamedKey::ArrowUp)   => self.scroll_by(-LINE_H),
                Key::Named(NamedKey::Home) => { self.scroll = 0; self.window.request_redraw(); }
                Key::Named(NamedKey::End)  => { self.scroll = self.max_scroll(); self.window.request_redraw(); }
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y)   => (y * LINE_H as f32 * 3.0) as i32,
                    MouseScrollDelta::PixelDelta(p)     => p.y as i32,
                };
                // Wheel-up scrolls content up (shows later lines), match the rest of the OS.
                self.scroll_by(-dy);
            }
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
                    let kind = self.btns[i].kind;
                    self.outcome = Some(match kind {
                        BtnKind::OpenReleases => UpdatesOutcome::OpenReleases,
                        BtnKind::Download => {
                            if let FetchState::Found(info) = &self.state {
                                if let Some(url) = &info.asset_url {
                                    UpdatesOutcome::OpenAsset(url.clone())
                                } else { UpdatesOutcome::OpenReleases }
                            } else { UpdatesOutcome::OpenReleases }
                        }
                        BtnKind::Close => UpdatesOutcome::Closed,
                    });
                }
            }
            WindowEvent::Resized(_) | WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn scroll_by(&mut self, dy: i32) {
        let max = self.max_scroll();
        self.scroll = (self.scroll + dy).clamp(0, max);
        self.window.request_redraw();
    }

    fn max_scroll(&self) -> i32 {
        if let FetchState::Found(info) = &self.state {
            let total = info.notes.len() as i32 * LINE_H;
            (total - self.notes_rect.3 + 8).max(0)
        } else { 0 }
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
        btns.push(Btn { kind: BtnKind::Close, label: "Close".into(), rect: (close_x, header_btn_y, close_w, BTN_H) });

        let bh = 36;
        let by = WIN_H as i32 - PAD - bh;

        let has_asset = matches!(&self.state,
            FetchState::Found(info) if info.asset_url.is_some());

        if has_asset {
            let dl_w = 200;
            let rel_w = 180;
            let gap  = 12;
            let total = dl_w + gap + rel_w;
            let dl_x = (WIN_W as i32 - total) / 2;
            let rel_x = dl_x + dl_w + gap;
            btns.push(Btn { kind: BtnKind::Download,     label: "Download for your system".into(), rect: (dl_x, by, dl_w, bh) });
            btns.push(Btn { kind: BtnKind::OpenReleases, label: "Open releases page".into(),      rect: (rel_x, by, rel_w, bh) });
        } else {
            let bw = 220;
            let bx = (WIN_W as i32 - bw) / 2;
            btns.push(Btn { kind: BtnKind::OpenReleases, label: "Open releases page".into(), rect: (bx, by, bw, bh) });
        }
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

        // Body — current + latest + date.
        let mut y = HEADER_H + 24;
        draw_text(&mut surf, PAD, y, 1, "INSTALLED",  argb_to_kashot(SECTION_TINT));
        let installed = format!("v{}", env!("CARGO_PKG_VERSION"));
        draw_text(&mut surf, PAD + 120, y, 1, &installed, argb_to_kashot(TEXT_BRIGHT));
        y += 22;
        draw_text(&mut surf, PAD, y, 1, "LATEST",     argb_to_kashot(SECTION_TINT));

        match &self.state {
            FetchState::Pending => {
                let dots = (self.started.elapsed().as_millis() / 400) % 4;
                let dots_s: String = std::iter::repeat('.').take(dots as usize).collect();
                let s = format!("checking{}", dots_s);
                draw_text(&mut surf, PAD + 120, y, 1, &s, argb_to_kashot(TEXT_MUTED));
            }
            FetchState::Found(info) => {
                draw_text(&mut surf, PAD + 120, y, 1, &info.tag, argb_to_kashot(TEXT_BRIGHT));
                y += 22;
                if !info.date.is_empty() {
                    draw_text(&mut surf, PAD, y, 1, "RELEASED", argb_to_kashot(SECTION_TINT));
                    draw_text(&mut surf, PAD + 120, y, 1, &info.date, argb_to_kashot(TEXT_BRIGHT));
                    y += 22;
                }
                let label = if info.has_update {
                    "A newer build is available."
                } else {
                    "You're on the latest build."
                };
                let tint = if info.has_update { LASER } else { TEXT_MUTED };
                draw_text(&mut surf, PAD, y, 1, label, argb_to_kashot(tint));

                let total = info.notes.len() as i32 * LINE_H;
                let max_scroll = (total - self.notes_rect.3 + 8).max(0);
                draw_notes(&mut surf, self.notes_rect, self.scroll, max_scroll, &info.notes);
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

fn draw_notes<S: painter::Surface>(
    surf: &mut S,
    rect: (i32, i32, i32, i32),
    scroll: i32,
    max_scroll: i32,
    lines: &[String],
) {
    let (nx, ny, nw, nh) = rect;
    fill_rect(surf, nx, ny, nw, nh, argb_to_kashot(PANEL_FILL));
    stroke_rect_argb(surf, nx, ny, nw, nh, argb_to_kashot(PANEL_BORDER));

    let inner_x = nx + 8;
    let inner_y = ny + 6;
    let inner_h = nh - 12;
    let first_visible = (scroll / LINE_H).max(0);
    let last_visible  = ((scroll + inner_h) / LINE_H + 1).min(lines.len() as i32);
    for i in first_visible..last_visible {
        let line_y = inner_y + (i * LINE_H) - scroll;
        if line_y + bitmap_font::GLYPH_H < inner_y || line_y > inner_y + inner_h { continue; }
        let line = &lines[i as usize];
        if let Some(rest) = line.strip_prefix("# ") {
            draw_text(surf, inner_x, line_y, 1, rest, argb_to_kashot(SECTION_TINT));
        } else if line.is_empty() {
            continue;
        } else {
            draw_text(surf, inner_x, line_y, 1, line, argb_to_kashot(TEXT_BRIGHT));
        }
    }

    let bar_x = nx + nw - SCROLLBAR_W - 2;
    let bar_y = ny + 4;
    let bar_h = nh - 8;
    fill_rect(surf, bar_x, bar_y, SCROLLBAR_W, bar_h, argb_to_kashot(SCROLLBAR_BG));
    if max_scroll > 0 {
        let total = (lines.len() as i32 * LINE_H).max(1);
        let thumb_h = ((bar_h as f32) * (inner_h as f32 / total as f32)).max(18.0) as i32;
        let thumb_h = thumb_h.min(bar_h);
        let progress = scroll as f32 / max_scroll as f32;
        let thumb_y = bar_y + ((bar_h - thumb_h) as f32 * progress) as i32;
        fill_rect(surf, bar_x, thumb_y, SCROLLBAR_W, thumb_h, argb_to_kashot(SCROLLBAR_FG));
    }
}

// ── network + parsing ───────────────────────────────────────────────────────

struct RawAsset {
    name: String,
    browser_download_url: String,
    // Kept for a future "Download (12.3 MB)" hover affordance.
    #[allow(dead_code)]
    size: u64,
}

struct RawRelease {
    tag_name:     String,
    body:         String,
    published_at: String,
    // Kept for a future "View on GitHub" link in the dialog footer.
    #[allow(dead_code)]
    html_url:     String,
    assets:       Vec<RawAsset>,
}

/// Shell out to `curl` (always-present on Linux / macOS / Windows 10+).
/// Parses the full release JSON via `serde_json` so we can pull the tag,
/// body, date, and asset list without re-doing string scanning per field.
fn fetch_latest_release() -> Result<RawRelease, String> {
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
    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse JSON: {e}"))?;

    let tag_name = v.get("tag_name").and_then(|x| x.as_str())
        .ok_or_else(|| "tag_name missing".to_owned())?.to_owned();
    let body_md = v.get("body").and_then(|x| x.as_str()).unwrap_or("").to_owned();
    // GitHub returns `published_at` even for drafts so we use it as the canonical date.
    let published_at = v.get("published_at").and_then(|x| x.as_str()).unwrap_or("").to_owned();
    let html_url = v.get("html_url").and_then(|x| x.as_str()).unwrap_or("").to_owned();

    let mut assets = Vec::new();
    if let Some(arr) = v.get("assets").and_then(|x| x.as_array()) {
        for a in arr {
            let name = a.get("name").and_then(|x| x.as_str()).unwrap_or("").to_owned();
            let bdu  = a.get("browser_download_url").and_then(|x| x.as_str()).unwrap_or("").to_owned();
            let size = a.get("size").and_then(|x| x.as_u64()).unwrap_or(0);
            if !name.is_empty() && !bdu.is_empty() {
                assets.push(RawAsset { name, browser_download_url: bdu, size });
            }
        }
    }

    Ok(RawRelease { tag_name, body: body_md, published_at, html_url, assets })
}

/// First 10 chars of an ISO-8601 timestamp, e.g. "2026-05-17T18:22:11Z" -> "2026-05-17".
fn parse_iso_date(iso: &str) -> String {
    if iso.len() >= 10 { iso[..10].to_owned() } else { String::new() }
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

/// Pick the right release asset for the current OS+arch. Substring-matches
/// so suffix tweaks (e.g. `-v0.3.0`) don't break the lookup.
fn pick_asset_url(assets: &[RawAsset]) -> Option<String> {
    let needle: &str = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "kashot-linux-x86_64.tar.gz"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "kashot-linux-arm64.tar.gz"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "Kashot-macos-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "Kashot-macos-x64"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "kashot-windows-x86_64.zip"
    } else {
        return None;
    };
    assets.iter()
        .find(|a| a.name.contains(needle))
        .map(|a| a.browser_download_url.clone())
}

/// Strip the bare-minimum markdown markers we expect from GitHub release
/// bodies: `**bold**`, `*em*`, `_em_`, `` `code` ``, leading `#` headings,
/// leading `- ` / `* ` bullets, and `[text](url)` links. Keeps line breaks.
fn strip_markdown(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    for raw_line in md.lines() {
        let mut line = raw_line.trim_end_matches('\r').to_owned();
        // Heading.
        if let Some(rest) = line.strip_prefix("### ") { line = format!("# {rest}"); }
        else if let Some(rest) = line.strip_prefix("## ") { line = format!("# {rest}"); }
        // "# " stays as-is so renderer can tint it.
        // Bullet.
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            let indent = line.len() - trimmed.len();
            line = format!("{}• {}", " ".repeat(indent), rest);
        }
        // Inline replacements.
        line = strip_inline(&line);
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn strip_inline(s: &str) -> String {
    // Order matters: bold (**…**) before em (*…*).
    let s = replace_pair(s, "**", "");
    let s = replace_pair(&s, "__", "");
    let s = replace_pair(&s, "*",  "");
    let s = replace_pair(&s, "_",  "");
    let s = replace_pair(&s, "`",  "");
    // [text](url) -> "text (url)"
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(end_txt) = s[i + 1..].find(']') {
                let txt_start = i + 1;
                let txt_end   = i + 1 + end_txt;
                if txt_end + 1 < bytes.len() && bytes[txt_end + 1] == b'(' {
                    if let Some(end_url) = s[txt_end + 2..].find(')') {
                        let url_start = txt_end + 2;
                        let url_end   = txt_end + 2 + end_url;
                        out.push_str(&s[txt_start..txt_end]);
                        out.push_str(" (");
                        out.push_str(&s[url_start..url_end]);
                        out.push(')');
                        i = url_end + 1;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Strip every occurrence of `marker` in `s`, replacing with `replacement`.
fn replace_pair(s: &str, marker: &str, replacement: &str) -> String {
    s.replace(marker, replacement)
}

/// Word-wrap `text` to `max_px` using the bitmap-font's 5×7-per-glyph
/// width. Each `\n` forces a hard break and is preserved as an empty
/// entry so paragraphs stay separated in the rendered output.
fn wrap_body(text: &str, max_px: i32) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in text.lines() {
        if raw_line.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        // Preserve the heading marker so the renderer can re-detect it.
        let (prefix, rest) = if let Some(r) = raw_line.strip_prefix("# ") {
            ("# ", r)
        } else { ("", raw_line) };
        let words: Vec<&str> = rest.split(' ').collect();
        let mut cur = String::new();
        for w in words {
            if w.is_empty() {
                if !cur.is_empty() { cur.push(' '); }
                continue;
            }
            let probe = if cur.is_empty() {
                w.to_owned()
            } else {
                format!("{} {}", cur, w)
            };
            let measured = bitmap_font::measure(&format!("{prefix}{probe}"), 1);
            if measured <= max_px || cur.is_empty() {
                cur = probe;
            } else {
                out.push(format!("{prefix}{cur}"));
                cur = w.to_owned();
            }
        }
        if !cur.is_empty() { out.push(format!("{prefix}{cur}")); }
    }
    out
}

// ── button rendering ────────────────────────────────────────────────────────

fn render_btn<S: painter::Surface>(surf: &mut S, b: &Btn, hovered: bool) {
    let (x, y, w, h) = b.rect;
    let is_primary = matches!(b.kind, BtnKind::Close | BtnKind::Download);
    let border = if is_primary { LASER } else if hovered { LASER_DIM } else { PANEL_BORDER };
    let fill   = if is_primary && hovered { 0x0000_2818 } else if hovered { HOVER_FILL } else { 0x0000_0000 };
    if fill != 0 { fill_rect(surf, x, y, w, h, argb_to_kashot(fill)); }
    stroke_rect_argb(surf, x, y, w, h, argb_to_kashot(border));
    let tw = bitmap_font::measure(&b.label, 1);
    let tx = x + (w - tw) / 2;
    let ty = y + (h - bitmap_font::GLYPH_H) / 2;
    let color = if is_primary { LASER } else { TEXT_BRIGHT };
    draw_text(surf, tx, ty, 1, &b.label, argb_to_kashot(color));
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
