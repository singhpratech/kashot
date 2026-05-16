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
    /// Pin the image at the given desktop coordinates. The window is the
    /// same size as the bitmap, borderless, and always-on-top.
    pub fn new(
        loop_target: &ActiveEventLoop,
        image:       ImageBuffer<Rgba<u8>, Vec<u8>>,
        screen_pos:  (i32, i32),
    ) -> Result<Self> {
        let (w, h) = (image.width(), image.height());
        let attrs = WindowAttributes::default()
            .with_title("Kashot — pinned")
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
        let img_w = self.image.width()  as usize;
        let img_h = self.image.height() as usize;
        let raw   = self.image.as_raw();

        for y in 0..win_h {
            for x in 0..win_w {
                let dst = y * win_w + x;
                if x < img_w && y < img_h {
                    let src = (y * img_w + x) * 4;
                    let r = raw[src]     as u32;
                    let g = raw[src + 1] as u32;
                    let b = raw[src + 2] as u32;
                    buf[dst] = (r << 16) | (g << 8) | b;
                } else {
                    buf[dst] = 0x0010_1014;
                }
            }
        }

        if let Err(e) = buf.present() {
            eprintln!("pin: buf.present: {e}");
        }
    }
}
