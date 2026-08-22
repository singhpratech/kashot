//! Floating recording control — small always-on-top window that appears
//! whenever a screen recording is live, so the user has a one-click way to
//! stop the capture without hunting through the tray menu.
//!
//! Layout (180 × 56, borderless):
//!
//!   ┌──────────────────────────────┐
//!   │  ● REC  00:12        [STOP]  │
//!   └──────────────────────────────┘
//!
//! The red dot flashes once per second (square wave, 60 % on). The "00:12"
//! is a wall-clock timer counting from when the recording started.
//! Left-drag the chrome to move; click [STOP] to end the recording.
//!
//! Lifecycle, same shape as `PinView`: `TrayApp` owns an
//! `Option<RecordingIndicator>`, dispatches `WindowEvent`s by `WindowId`,
//! and reads `stop_requested` after each event to learn when the STOP
//! button was clicked.
//!
//! The panel must NEVER appear inside the recorded video, so `new()`
//! excludes the window from screen capture before returning:
//!   - Windows: `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` — DWM
//!     omits the window from BitBlt/DXGI screen reads (what gdigrab sees).
//!   - macOS: `NSWindow.sharingType = NSWindowSharingNone` — `screencapture`
//!     skips windows that opt out of sharing.
//!   - Linux/X11: no such API exists (x11grab reads the composited root
//!     window), so the tray loop never creates the panel while recording —
//!     see `start_recording` in tray_loop.rs.
//! If exclusion fails (e.g. Windows 10 before 2004), `new()` returns Err and
//! the caller records without a panel rather than burning it into the video.
//!
//! A **region** recording changes that calculus on X11: the panel is only a
//! problem where the recorder is looking, so `new_at` lets the caller park it
//! at a spot `kashot_core::region::indicator_placement` has certified as
//! outside the recorded rectangle. `SIZE` is what that placement measures.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId, WindowLevel};

use crate::bitmap_font;
use crate::painter;

const WIN_W: u32 = 220;
const WIN_H: u32 = 56;

/// Outer size of the panel, for callers positioning it around a recorded
/// region. Same numbers `new()` builds the window with.
pub const SIZE: (u32, u32) = (WIN_W, WIN_H);

// Colors (matches the void-black / laser-green / record-red palette).
const BG:          u32 = 0x0010_1614;
const BG_BORDER:   u32 = 0x0028_3a30;
const TEXT_BRIGHT: u32 = 0x00e8_ffe8;
const TEXT_DIM:    u32 = 0x008a_9c92;
const REC_ON:      u32 = 0x00ff_3a3a;
const REC_OFF:     u32 = 0x0044_1414;
const STOP_FILL:   u32 = 0x00ff_3a3a;
const STOP_BORDER: u32 = 0x00ff_6a6a;
const STOP_TEXT:   u32 = 0x000a_0606;

pub struct RecordingIndicator {
    window:      Rc<Window>,
    _ctx:        Context<Rc<Window>>,
    surface:     Surface<Rc<Window>, Rc<Window>>,
    started:     Instant,
    cursor:      (i32, i32),
    hover_stop:  bool,
    /// Set to `true` when the user clicks the STOP button. The tray loop
    /// polls this after each event and drops the indicator + calls
    /// `stop_recording` when it flips.
    pub stop_requested: bool,
}

impl RecordingIndicator {
    /// Open the panel at a caller-chosen screen position.
    ///
    /// The position is not cosmetic: the panel has to land outside the recorded
    /// rectangle, which is the difference between a visible STOP button and a
    /// STOP button burned into the user's video on X11. `tray_loop` picks it
    /// with `kashot_core::region::indicator_placement`.
    pub fn new_at(loop_target: &ActiveEventLoop, origin: (i32, i32)) -> Result<Self> {
        let (x, y) = origin;

        let attrs = WindowAttributes::default()
            .with_title("KAShot — recording")
            .with_decorations(false)
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(WIN_W, WIN_H))
            .with_position(PhysicalPosition::new(x, y))
            .with_window_icon(crate::brand_icon::shared())
            .with_window_level(WindowLevel::AlwaysOnTop);

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window (recording indicator): {e}"))?;

        // Keep the panel out of the recording itself. Hard-fail on error:
        // no panel beats a panel burned into the user's video.
        exclude_from_capture(&window)?;

        window.set_cursor(CursorIcon::Default);

        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new (recording indicator): {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new (recording indicator): {e}"))?;

        let mut me = RecordingIndicator {
            window, _ctx: ctx, surface,
            started: Instant::now(),
            cursor: (0, 0),
            hover_stop: false,
            stop_requested: false,
        };
        me.redraw();
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    /// Drive the flashing dot — called every poll tick from the tray loop
    /// so the dot animates whether or not the user moves the mouse over us.
    pub fn tick(&self) {
        self.window.request_redraw();
    }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.stop_requested = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as i32, position.y as i32);
                let new_hover = self.hit_stop(self.cursor.0, self.cursor.1);
                let new_cursor = if new_hover { CursorIcon::Pointer } else { CursorIcon::Move };
                self.window.set_cursor(new_cursor);
                if new_hover != self.hover_stop {
                    self.hover_stop = new_hover;
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left, ..
            } => {
                if self.hit_stop(self.cursor.0, self.cursor.1) {
                    self.stop_requested = true;
                } else {
                    // Anywhere else inside the chrome → start a window drag
                    // (delegates to the WM via `_NET_WM_MOVERESIZE` on X11,
                    // the equivalent on Wayland / Win32 / Cocoa). Same trick
                    // PinView uses.
                    let _ = self.window.drag_window();
                }
            }
            WindowEvent::Resized(_) | WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn hit_stop(&self, x: i32, y: i32) -> bool {
        let (sx, sy, sw, sh) = stop_rect();
        x >= sx && x < sx + sw && y >= sy && y < sy + sh
    }

    fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height))
            else { return; };
        if let Err(e) = self.surface.resize(w, h) {
            eprintln!("recording indicator: surface.resize: {e}"); return;
        }
        let mut buf = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("recording indicator: buffer_mut: {e}"); return; }
        };
        let win_w = w.get() as usize;
        let win_h = h.get() as usize;

        // Background + border.
        for i in 0..(win_w * win_h) { buf[i] = BG; }
        for x in 0..win_w {
            buf[x] = BG_BORDER;
            buf[(win_h - 1) * win_w + x] = BG_BORDER;
        }
        for y in 0..win_h {
            buf[y * win_w] = BG_BORDER;
            buf[y * win_w + (win_w - 1)] = BG_BORDER;
        }

        let mut surf = BufferSurface { buf: &mut buf, w: win_w as i32, h: win_h as i32 };

        // Red dot — flashes once per second (square wave, 60 % on).
        let elapsed = self.started.elapsed().as_secs_f32();
        let cycle = (elapsed * 1.0).fract();
        let dot_on = cycle < 0.6;
        let dot_color = if dot_on { REC_ON } else { REC_OFF };
        let dx = 14;
        let dy = WIN_H as i32 / 2;
        fill_disc(&mut surf, dx, dy, 6, argb_to_rgba(dot_color));
        if dot_on {
            // Soft outer halo when bright.
            fill_disc_alpha(&mut surf, dx, dy, 10, 0xff, 0x3a, 0x3a, 70);
            fill_disc_alpha(&mut surf, dx, dy, 14, 0xff, 0x3a, 0x3a, 30);
        }

        // "REC" label.
        let rec_text = "REC";
        let rec_x = dx + 12;
        let rec_y = dy - bitmap_font::GLYPH_H / 2;
        draw_text(&mut surf, rec_x, rec_y, 1, rec_text, argb_to_rgba(TEXT_BRIGHT));

        // Timer "MM:SS".
        let total = self.started.elapsed().as_secs();
        let mins = total / 60;
        let secs = total % 60;
        let timer = format!("{:02}:{:02}", mins, secs);
        let timer_x = rec_x + bitmap_font::measure(rec_text, 1) + 12;
        let timer_y = rec_y;
        draw_text(&mut surf, timer_x, timer_y, 1, &timer, argb_to_rgba(TEXT_DIM));

        // STOP button.
        let (sx, sy, sw, sh) = stop_rect();
        let fill = if self.hover_stop { 0x00ff_6a6a } else { STOP_FILL };
        fill_rect(&mut surf, sx, sy, sw, sh, argb_to_rgba(fill));
        stroke_rect(&mut surf, sx, sy, sw, sh, argb_to_rgba(STOP_BORDER));
        let label = "STOP";
        let lw = bitmap_font::measure(label, 1);
        let lx = sx + (sw - lw) / 2;
        let ly = sy + (sh - bitmap_font::GLYPH_H) / 2;
        draw_text(&mut surf, lx, ly, 1, label, argb_to_rgba(STOP_TEXT));

        if let Err(e) = buf.present() {
            eprintln!("recording indicator: buf.present: {e}");
        }
    }
}

/// Exclude the indicator window from screen capture so it never shows up
/// inside the recorded video. Windows 10 2004+ (build 19041): the DWM
/// drops the window from BitBlt/DXGI screen reads — exactly what ffmpeg's
/// gdigrab performs (same trick OBS and KeePassXC ship).
///
/// MUST be version-gated, not just error-checked: on older builds the call
/// *succeeds* but degrades to WDA_MONITOR semantics — the panel would show
/// as a solid black box in the recording, strictly worse than showing it.
/// On gate/API failure we bubble the Err up; the tray loop then records
/// without a floating panel (tray menu still stops).
#[cfg(windows)]
fn exclude_from_capture(window: &Rc<Window>) -> Result<()> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // First Windows build where WDA_EXCLUDEFROMCAPTURE means "exclude"
    // rather than "black box" (Windows 10 2004).
    const MIN_BUILD: u32 = 19041;
    let build = windows_build_number();
    if build < MIN_BUILD {
        return Err(anyhow!(
            "Windows build {build} < {MIN_BUILD}: capture exclusion unavailable \
             (would record a black box) — recording without the floating panel."
        ));
    }

    let handle = window
        .window_handle()
        .map_err(|e| anyhow!("window_handle (recording indicator): {e}"))?;
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return Err(anyhow!("recording indicator: not a Win32 window handle"));
    };
    let hwnd = HWND(h.hwnd.get() as *mut core::ffi::c_void);
    unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) }
        .map_err(|e| anyhow!("SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE): {e}"))
}

/// Build number from the registry (`CurrentBuildNumber`). `GetVersionEx`
/// lies without a compatibility manifest; the registry value doesn't.
/// Returns 0 when unreadable — callers treat that as "too old".
#[cfg(windows)]
fn windows_build_number() -> u32 {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };

    let mut buf = [0u16; 32];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return 0;
    }
    let len = (size as usize / 2).saturating_sub(1).min(buf.len());
    String::from_utf16_lossy(&buf[..len]).trim().parse().unwrap_or(0)
}

/// macOS: `NSWindow.sharingType = NSWindowSharingNone` — the window keeps
/// rendering on the monitor but opts out of being read by capture APIs,
/// including the `screencapture -v` process the recorder spawns.
#[cfg(target_os = "macos")]
fn exclude_from_capture(window: &Rc<Window>) -> Result<()> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = window
        .window_handle()
        .map_err(|e| anyhow!("window_handle (recording indicator): {e}"))?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return Err(anyhow!("recording indicator: not an AppKit window handle"));
    };
    unsafe {
        let view = h.ns_view.as_ptr() as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![&*view, window];
        if ns_window.is_null() {
            return Err(anyhow!("recording indicator: NSView has no NSWindow"));
        }
        // 0 = NSWindowSharingNone (NSUInteger).
        let _: () = msg_send![&*ns_window, setSharingType: 0usize];
    }
    Ok(())
}

/// Linux/X11: x11grab records the composited root window — there is no
/// per-window capture-exclusion API, so the tray loop never creates the
/// panel while recording (see `start_recording`). This stub keeps the
/// call site uniform if it's ever reached.
#[cfg(not(any(windows, target_os = "macos")))]
fn exclude_from_capture(_window: &Rc<Window>) -> Result<()> {
    Ok(())
}

fn stop_rect() -> (i32, i32, i32, i32) {
    let w = 60;
    let h = 24;
    let x = WIN_W as i32 - w - 14;
    let y = (WIN_H as i32 - h) / 2;
    (x, y, w, h)
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

fn argb_to_rgba(argb: u32) -> kashot_core::color::Rgba {
    kashot_core::color::Rgba {
        r: ((argb >> 16) & 0xFF) as u8,
        g: ((argb >>  8) & 0xFF) as u8,
        b: ( argb        & 0xFF) as u8,
        a: 255,
    }
}

fn fill_rect<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, color: kashot_core::color::Rgba) {
    let rgba = [color.r, color.g, color.b, color.a];
    for yy in y..y + h { for xx in x..x + w { s.write(xx, yy, rgba); } }
}

fn stroke_rect<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, color: kashot_core::color::Rgba) {
    let rgba = [color.r, color.g, color.b, color.a];
    for xx in x..x + w { s.write(xx, y, rgba); s.write(xx, y + h - 1, rgba); }
    for yy in y..y + h { s.write(x, yy, rgba); s.write(x + w - 1, yy, rgba); }
}

fn fill_disc<S: painter::Surface>(s: &mut S, cx: i32, cy: i32, r: i32, color: kashot_core::color::Rgba) {
    let r2 = r * r;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r2 {
                s.write(cx + dx, cy + dy, [color.r, color.g, color.b, 255]);
            }
        }
    }
}

fn fill_disc_alpha<S: painter::Surface>(s: &mut S, cx: i32, cy: i32, r: i32, rr: u8, gg: u8, bb: u8, a: u8) {
    let r2 = r * r;
    let inv = 255 - a as u32;
    let ra = a as u32;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r2 {
                let cur = s.read(cx + dx, cy + dy);
                let nr = ((rr as u32 * ra + cur[0] as u32 * inv + 127) / 255).min(255) as u8;
                let ng = ((gg as u32 * ra + cur[1] as u32 * inv + 127) / 255).min(255) as u8;
                let nb = ((bb as u32 * ra + cur[2] as u32 * inv + 127) / 255).min(255) as u8;
                s.write(cx + dx, cy + dy, [nr, ng, nb, 255]);
            }
        }
    }
}

fn draw_text<S: painter::Surface>(s: &mut S, x: i32, y: i32, scale: i32, text: &str, color: kashot_core::color::Rgba) {
    painter::draw_text(s, x, y, scale, text, color);
}
