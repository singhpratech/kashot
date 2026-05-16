//! Themed About dialog. Same laser-green / void-black skin as the Settings
//! window — replaces the prior `rfd::MessageDialog` info popup.
//!
//! Lifecycle pattern mirrors `SettingsView` / `PinView`: the tray app owns
//! an `Option<AboutView>`, routes `WindowEvent`s by `WindowId`, and polls
//! `outcome` after each event to know when the user dismissed the dialog.

use std::num::NonZeroU32;
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
use crate::painter::Surface as _;

// Colors — exact match to settings_form.rs so the windows feel like one app.
const BG_TOP:        u32 = 0x0008_0c0a;
const BG_BODY:       u32 = 0x000a_0e0c;
const HEADER_RULE:   u32 = 0x0014_2a1f;
const PANEL_BORDER:  u32 = 0x0014_2a1f;
const TEXT_BRIGHT:   u32 = 0x00e8_ffe8;
const TEXT_MUTED:    u32 = 0x009c_b0a4;
const TEXT_DIM:      u32 = 0x0068_7a70;
const SECTION_TINT:  u32 = 0x0066_ffb6;
const LASER:         u32 = 0x0000_ff95;
const LASER_DIM:     u32 = 0x0000_8050;
const HOVER_FILL:    u32 = 0x0010_2018;

const WIN_W: u32 = 480;
const WIN_H: u32 = 360;
const PAD:   i32 = 22;
const BTN_H: i32 = 30;
const HEADER_H: i32 = 84;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BtnKind { Project, Updates, Author, Close }

struct Btn {
    kind:  BtnKind,
    label: &'static str,
    rect:  (i32, i32, i32, i32),
}

pub enum AboutOutcome {
    /// User clicked Close / hit Esc / closed the window.
    Closed,
    /// User clicked "Project" — tray loop should shell-open the project URL
    /// (kashot.org) and keep the view open until Close is hit explicitly.
    OpenProject,
    /// User clicked "Check for updates" — tray loop should open the
    /// themed updates dialog (or the releases URL as a fallback).
    OpenUpdates,
    /// User clicked the author byline → open Prateek's personal page.
    OpenAuthor,
}

pub struct AboutView {
    window:  Rc<Window>,
    _ctx:    Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    btns:    Vec<Btn>,
    cursor:  (i32, i32),
    hover:   Option<usize>,
    pub outcome: Option<AboutOutcome>,
}

impl AboutView {
    pub fn new(loop_target: &ActiveEventLoop) -> Result<Self> {
        let (cx, cy) = centered_origin(loop_target, WIN_W, WIN_H);
        let attrs = WindowAttributes::default()
            .with_title("About KAShot")
            .with_decorations(true)
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(WIN_W, WIN_H))
            .with_position(PhysicalPosition::new(cx, cy))
            .with_window_icon(crate::brand_icon::shared());

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window (about): {e}"))?;

        window.set_cursor(CursorIcon::Default);
        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new (about): {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new (about): {e}"))?;

        let mut me = AboutView {
            window, _ctx: ctx, surface,
            btns: Vec::new(),
            cursor: (0, 0),
            hover: None,
            outcome: None,
        };
        me.btns = me.build_btns();
        me.redraw();
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.outcome = Some(AboutOutcome::Closed),
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter),
                    state: ElementState::Pressed, ..
                }, ..
            } => self.outcome = Some(AboutOutcome::Closed),
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
                        BtnKind::Project => AboutOutcome::OpenProject,
                        BtnKind::Updates => AboutOutcome::OpenUpdates,
                        BtnKind::Author  => AboutOutcome::OpenAuthor,
                        BtnKind::Close   => AboutOutcome::Closed,
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
        // Header action bar (top-right) — primary "Close" lives here so the
        // user's commit action is in the same spot as Settings.
        let header_btn_y = (HEADER_H - BTN_H) / 2 + 4;
        let close_w = 110;
        let close_x = WIN_W as i32 - PAD - close_w;
        btns.push(Btn { kind: BtnKind::Close, label: "Close", rect: (close_x, header_btn_y, close_w, BTN_H) });

        // Author byline hotspot. Geometry mirrors what `redraw` paints for
        // the "With love from PrateekSingh." line so the click hit-rect
        // hugs the underlined name only (the lead-in stays non-clickable).
        let lead = "With love from ";
        let name = "PrateekSingh.";
        let lead_w = bitmap_font::measure(lead, 1);
        let name_w = bitmap_font::measure(name, 1);
        let author_y = HEADER_H + 28;
        let author_x = PAD + lead_w;
        // A bit of vertical padding so it's easy to hit.
        btns.push(Btn {
            kind:  BtnKind::Author,
            label: "Author",
            rect:  (author_x, author_y - 3, name_w, bitmap_font::GLYPH_H + 8),
        });

        // Body links — Project + Check for updates buttons centered above
        // the bottom edge, side-by-side.
        let link_w = 200;
        let link_h = 36;
        let y      = WIN_H as i32 - PAD - link_h;
        let total  = link_w * 2 + 12;
        let lx     = (WIN_W as i32 - total) / 2;
        btns.push(Btn { kind: BtnKind::Project, label: "Project page",     rect: (lx,             y, link_w, link_h) });
        btns.push(Btn { kind: BtnKind::Updates, label: "Check for updates", rect: (lx + link_w + 12, y, link_w, link_h) });
        btns
    }

    fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height))
            else { return; };
        if let Err(e) = self.surface.resize(w, h) { eprintln!("about: surface.resize: {e}"); return; }
        let mut buf = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("about: buffer_mut: {e}"); return; }
        };
        let win_w = w.get() as usize;
        let win_h = h.get() as usize;
        for y in 0..win_h {
            let band = if (y as i32) < HEADER_H { BG_TOP } else { BG_BODY };
            for x in 0..win_w { buf[y * win_w + x] = band; }
        }
        h_line(&mut buf, win_w, win_h, 0, win_w as i32, HEADER_H, HEADER_RULE);
        let _ = PANEL_BORDER;
        let _ = TEXT_DIM;

        let mut surf = BufferSurface { buf: &mut buf, w: win_w as i32, h: win_h as i32 };

        // Title strip.
        draw_text(&mut surf, PAD, 22, 2, "KASHOT // ABOUT", argb_to_kashot(LASER));
        let version = format!("v{}  ·  the lightweight screenshot tool", env!("CARGO_PKG_VERSION"));
        draw_text(&mut surf, PAD, 50, 1, &version, argb_to_kashot(TEXT_MUTED));

        // Body copy. The "PrateekSingh" word is rendered separately so it
        // can be a clickable link to the author's personal page (hit-rect
        // built in `build_btns` as a BtnKind::Author item).
        let mut y = HEADER_H + 28;
        let lead = "With love from ";
        let name = "PrateekSingh.";
        draw_text(&mut surf, PAD, y, 1, lead, argb_to_kashot(TEXT_BRIGHT));
        let lead_w = bitmap_font::measure(lead, 1);
        let name_x = PAD + lead_w;
        let name_w = bitmap_font::measure(name, 1);
        let author_hovered = self.btns.iter().enumerate()
            .any(|(i, b)| b.kind == BtnKind::Author && self.hover == Some(i));
        let name_color = if author_hovered { argb_to_kashot(LASER) }
                         else              { argb_to_kashot(SECTION_TINT) };
        draw_text(&mut surf, name_x, y, 1, name, name_color);
        // Underline so it reads as a link.
        let underline_y = y + bitmap_font::GLYPH_H;
        for x in name_x..(name_x + name_w) {
            surf.write(x, underline_y, [name_color.r, name_color.g, name_color.b, 255]);
        }
        y += 22;
        let year = chrono::Local::now().format("%Y").to_string();
        let copy = format!("© {} PrateekSingh. All rights reserved.", year);
        draw_text(&mut surf, PAD, y, 1, &copy, argb_to_kashot(TEXT_MUTED));
        y += 30;
        draw_text(&mut surf, PAD, y, 1, "WEB",     argb_to_kashot(SECTION_TINT));
        draw_text(&mut surf, PAD + 60, y, 1, "kashot.org",                           argb_to_kashot(TEXT_BRIGHT));
        y += 18;
        draw_text(&mut surf, PAD, y, 1, "SOURCE",  argb_to_kashot(SECTION_TINT));
        draw_text(&mut surf, PAD + 60, y, 1, "github.com/singhpratech/kashot",       argb_to_kashot(TEXT_BRIGHT));
        y += 18;
        draw_text(&mut surf, PAD, y, 1, "LICENSE", argb_to_kashot(SECTION_TINT));
        draw_text(&mut surf, PAD + 60, y, 1, "MIT",                                  argb_to_kashot(TEXT_BRIGHT));

        // Buttons.
        for (i, b) in self.btns.iter().enumerate() {
            let hovered = self.hover == Some(i);
            render_btn(&mut surf, b, hovered);
        }

        if let Err(e) = buf.present() { eprintln!("about: buf.present: {e}"); }
    }
}

fn render_btn<S: painter::Surface>(surf: &mut S, b: &Btn, hovered: bool) {
    // The Author hit-rect is painted as part of the body copy in `redraw`,
    // not as a framed button — skip the box+label drawing entirely.
    if b.kind == BtnKind::Author { return; }
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
