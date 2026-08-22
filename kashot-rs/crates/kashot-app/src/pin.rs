//! Pinned-image window — borderless, always-on-top, draggable.
//!
//! Mirrors `Kashot/PinForm.cs` from the C# build: when the user picks "Pin"
//! on a captured selection, the cropped bitmap stays floating on screen as
//! its own little window. Click-and-drag moves it; Esc / right-click /
//! middle-click closes it.
//!
//! Like the editor overlay, this can't own its own event loop because the
//! tray app is the single owner — instead `TrayApp` keeps a `Vec<PinView>`
//! and dispatches `WindowEvent`s by `WindowId`.

use std::num::NonZeroU32;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use image::{ImageBuffer, Rgba};
use kashot_core::dpi::sample_index;
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId, WindowLevel};

pub struct PinView {
    image:   ImageBuffer<Rgba<u8>, Vec<u8>>,
    window:  Rc<Window>,
    _ctx:    Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
}

impl PinView {
    /// Pin the image at the given desktop position, in **device pixels**.
    ///
    /// The bitmap is in device pixels too (it was cut out of the capture,
    /// which the display map builds at device resolution), so a window whose
    /// *physical* inner size is the bitmap's size covers exactly the region
    /// the user selected on any display — a 2x panel gets a 2x bitmap in a
    /// window that is half as many logical points across, i.e. the same
    /// physical rectangle. `redraw` handles the case where the window manager
    /// hands back a different size anyway.
    pub fn new(
        loop_target: &ActiveEventLoop,
        image:       ImageBuffer<Rgba<u8>, Vec<u8>>,
        screen_pos:  (i32, i32),
    ) -> Result<Self> {
        let (w, h) = (image.width(), image.height());
        let attrs = WindowAttributes::default()
            .with_title("KAShot — pinned")
            .with_decorations(false)
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(w, h))
            .with_position(PhysicalPosition::new(screen_pos.0, screen_pos.1))
            .with_window_icon(crate::brand_icon::shared())
            .with_window_level(WindowLevel::AlwaysOnTop);

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window (pin): {e}"))?;

        // KAShot runs as a menu-bar/tray agent, so on macOS there is no Dock
        // icon to click if this opens behind the frontmost app. Ask for focus
        // explicitly; on Windows/Linux it is a harmless raise.
        window.focus_window();
        window.set_cursor(CursorIcon::Move);

        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new (pin): {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new (pin): {e}"))?;

        let mut me = PinView { image, window, _ctx: ctx, surface };
        me.redraw();
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    /// Returns `true` when the pin window should be torn down.
    pub fn handle_event(&mut self, event: WindowEvent) -> bool {
        match event {
            WindowEvent::CloseRequested => true,

            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), state: ElementState::Pressed, .. }, ..
            } => true,

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => true,

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } => true,

            // Left-click-and-drag delegates to the window manager via winit's
            // interactive-drag API. On X11 this fires _NET_WM_MOVERESIZE which
            // every modern WM (Cinnamon / Mutter / KWin / sway) honors.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let _ = self.window.drag_window();
                false
            }

            WindowEvent::Resized(_) => {
                self.redraw();
                false
            }

            WindowEvent::RedrawRequested => {
                self.redraw();
                false
            }

            _ => false,
        }
    }

    fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) else { return; };
        if let Err(e) = self.surface.resize(w, h) {
            eprintln!("pin: surface.resize: {e}"); return;
        }
        let mut buf = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("pin: buffer_mut: {e}"); return; }
        };

        let win_w = w.get() as usize;
        let win_h = h.get() as usize;
        let img_w = self.image.width();
        let img_h = self.image.height();
        let raw   = self.image.as_raw();

        if img_w == 0 || img_h == 0 {
            buf.fill(0x0010_1014);
            if let Err(e) = buf.present() {
                eprintln!("pin: buf.present: {e}");
            }
            return;
        }

        // Fit the bitmap to whatever size we actually got. It normally is the
        // bitmap's own size (see `new`), and then this is a straight copy —
        // but a window manager that clamps the window, or a move onto a
        // display with a different scale factor, would otherwise crop the
        // pinned image instead of showing all of it.
        for y in 0..win_h {
            let sy = sample_index(y as u32, win_h as u32, img_h) as usize;
            let src_row = sy * img_w as usize * 4;
            let dst_row = y * win_w;
            for x in 0..win_w {
                let sx = sample_index(x as u32, win_w as u32, img_w) as usize;
                let src = src_row + sx * 4;
                let r = raw[src]     as u32;
                let g = raw[src + 1] as u32;
                let b = raw[src + 2] as u32;
                buf[dst_row + x] = (r << 16) | (g << 8) | b;
            }
        }

        if let Err(e) = buf.present() {
            eprintln!("pin: buf.present: {e}");
        }
    }
}
