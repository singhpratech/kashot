//! Wayland screen recording: ScreenCast portal -> PipeWire -> ffmpeg stdin.
//!
//! `ffmpeg` has no PipeWire input device, and Wayland has no X11 screen for
//! `-f x11grab` to read (XWayland answers the grab and hands back a black
//! frame, which is why [`crate::session::is_wayland`] exists at all). So this
//! module becomes the capture device: it asks `xdg-desktop-portal` for a
//! ScreenCast stream, reads the PipeWire buffers itself, and feeds ffmpeg raw
//! frames on stdin as `-f rawvideo`.
//!
//! stdin is free by design. Linux audio still arrives the way it always has —
//! `-f pulse` inputs, which work unchanged under PipeWire's Pulse shim — and
//! the platforms that stream PCM over a loopback TCP socket (Windows WASAPI,
//! macOS ScreenCaptureKit) are untouched. Nothing else wanted the pipe.
//!
//! ## Threads
//!
//! Three, all owned by one [`CastSession`]:
//!
//! * **portal** — runs the `CreateSession` / `SelectSources` / `Start` /
//!   `OpenPipeWireRemote` dance, then holds the session open until stop. The
//!   session must outlive the stream: closing it tears the PipeWire node down.
//! * **pipewire** — owns the `MainLoop` and the stream. Its `process` callback
//!   copies each frame into a single-slot buffer, dropping whatever was there.
//!   Never writes to a pipe, so a slow encoder can't stall the compositor.
//! * **writer** — wakes on a fixed cadence, takes whatever is in the slot, and
//!   writes it to ffmpeg. Repeats the previous frame when nothing new arrived.
//!
//! ## Why the writer paces itself
//!
//! `-f rawvideo` carries no timestamps: ffmpeg times the stream purely by
//! counting frames at the declared `-framerate`. A compositor only sends a
//! buffer when something on screen changed, so writing frames as they arrive
//! would produce a file where thirty seconds of a still screen plays back in
//! one. Re-sending the last frame on a fixed cadence is what makes wall-clock
//! duration and playback duration agree.
//!
//! ## Failure is loud
//!
//! [`CastSession::start`] does not return until a real frame has been copied
//! out of a real PipeWire buffer, so the format handed to ffmpeg is measured,
//! not assumed — and a session that negotiates but never delivers (an
//! unmappable DMA-BUF, a compositor that stalls) fails here with a message,
//! before any file is created. The recorder never announces a recording that
//! isn't running.

use crate::{Error, Result};

use std::os::fd::OwnedFd;
use std::process::ChildStdin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;

/// Frames per second we ask the compositor for and pace the writer at.
/// Matches the `-framerate 30` every other platform's capture uses, so the
/// output file is the same shape everywhere.
pub const TARGET_FPS: u32 = 30;

/// How long to wait for the portal to produce a stream.
///
/// This window contains a human: most desktops put up a "share your screen?"
/// picker, and the user has to choose a monitor and confirm. Generous enough
/// not to cancel a hesitating user out from under themselves, bounded so a
/// portal that never answers doesn't wedge the caller forever.
const PORTAL_TIMEOUT: Duration = Duration::from_secs(90);

/// How long to wait, after the portal hands over a node, for the first frame
/// to actually land. No user is involved here — this is pure negotiation plus
/// one compositor repaint — so a stall means something is wrong.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// Restore token from an earlier ScreenCast in this process, so a second
/// recording doesn't put the picker in front of the user again.
///
/// Deliberately process-global and not persisted: [`PersistMode::Application`]
/// scopes the grant to this run of Kashot, which is the most a screen recorder
/// should quietly hold on to.
fn restore_token_slot() -> &'static Mutex<Option<String>> {
    static SLOT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// The pixel layout the compositor settled on, in the terms ffmpeg wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CastFormat {
    pub width:  u32,
    pub height: u32,
    /// An ffmpeg `-pix_fmt` name. Always a 4-bytes-per-pixel packed layout —
    /// see [`ffmpeg_pix_fmt`].
    pub pix_fmt: &'static str,
}

impl CastFormat {
    /// Bytes in one tightly-packed frame.
    fn frame_len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

/// Translate a SPA video format into ffmpeg's name for the same byte order.
///
/// Both projects name packed formats by the order the components sit in
/// memory, so the mapping is one-for-one: SPA `BGRx` and ffmpeg `bgr0` are
/// both "blue, green, red, ignored". Only the 32-bit packed layouts are
/// listed because those are the only ones [`enum_format_pod`] offers, which
/// keeps the frame copy a plain row-wise memcpy with no unpacking.
fn ffmpeg_pix_fmt(format: pipewire::spa::param::video::VideoFormat) -> Option<&'static str> {
    use pipewire::spa::param::video::VideoFormat as F;
    Some(match format {
        F::BGRx => "bgr0",
        F::RGBx => "rgb0",
        F::BGRA => "bgra",
        F::RGBA => "rgba",
        F::xRGB => "0rgb",
        F::xBGR => "0bgr",
        F::ARGB => "argb",
        F::ABGR => "abgr",
        _ => return None,
    })
}

// ── the latest-frame slot ───────────────────────────────────────────────────

/// One frame, handed from the PipeWire thread to the writer thread.
///
/// Single-slot on purpose: if the writer is behind, the *newest* frame is the
/// one worth keeping, and an unbounded queue would just grow until the machine
/// noticed. `seq` lets the writer tell "nothing new" (repeat the last frame)
/// from "a fresh frame" without comparing pixels.
struct Slot {
    bytes: Vec<u8>,
    seq:   u64,
}

struct FrameSlot {
    slot:  Mutex<Slot>,
    ready: Condvar,
}

impl FrameSlot {
    fn new() -> Self {
        Self {
            slot:  Mutex::new(Slot { bytes: Vec::new(), seq: 0 }),
            ready: Condvar::new(),
        }
    }

    /// Called from the PipeWire thread. `scratch` is swapped in so the copy
    /// reuses an allocation instead of making one per frame.
    fn publish(&self, scratch: &mut Vec<u8>) {
        if let Ok(mut guard) = self.slot.lock() {
            std::mem::swap(&mut guard.bytes, scratch);
            guard.seq = guard.seq.saturating_add(1);
            self.ready.notify_all();
        }
    }

    /// Block until at least one frame has ever been published, or `timeout`
    /// elapses. Returns whether a frame arrived.
    fn wait_for_first(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let Ok(mut guard) = self.slot.lock() else { return false };
        while guard.seq == 0 {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else { return false };
            let Ok((next, wait)) = self.ready.wait_timeout(guard, left) else { return false };
            if wait.timed_out() && next.seq == 0 { return false; }
            guard = next;
        }
        true
    }

    /// Called from the writer thread. If a frame newer than `last_seq` is
    /// waiting, swap it into `scratch` and update `last_seq`. Returns whether
    /// `scratch` changed; when it didn't, the caller re-sends what it has.
    fn take_newer(&self, scratch: &mut Vec<u8>, last_seq: &mut u64) -> bool {
        let Ok(mut guard) = self.slot.lock() else { return false };
        if guard.seq == *last_seq { return false; }
        std::mem::swap(&mut guard.bytes, scratch);
        *last_seq = guard.seq;
        true
    }
}

/// Whether the cast is still alive, shared with the PipeWire thread.
///
/// A recording can lose its source without ffmpeg noticing: the user hits
/// "Stop sharing" in their desktop's indicator, or the compositor drops the
/// session. Frames simply stop arriving — and since the writer re-sends the
/// last frame to keep the timeline honest, the encoder happily writes a frozen
/// picture for as long as the user leaves it running. "No frames lately" is
/// not the signal to watch (an idle screen produces no damage and so no
/// frames, quite legitimately); the stream leaving `Streaming` is.
struct CastHealth {
    failed:  AtomicBool,
    message: Mutex<String>,
}

impl CastHealth {
    fn new() -> Self {
        Self { failed: AtomicBool::new(false), message: Mutex::new(String::new()) }
    }

    fn fail(&self, message: String) {
        if let Ok(mut m) = self.message.lock() { *m = message; }
        self.failed.store(true, Ordering::Relaxed);
    }

    fn failure(&self) -> Option<String> {
        if !self.failed.load(Ordering::Relaxed) { return None; }
        Some(self.message.lock().map(|m| m.clone()).unwrap_or_default())
    }
}

// ── the session ─────────────────────────────────────────────────────────────

/// Message to the PipeWire thread. One variant, but a typed channel is what
/// `pipewire::channel` wants and it documents itself.
struct Terminate;

/// A live ScreenCast: portal session, PipeWire stream, and (once
/// [`attach`](CastSession::attach) has run) the thread writing to ffmpeg.
///
/// Everything is torn down by [`stop`](CastSession::stop) or by `Drop`, in the
/// order the pipeline runs: the writer stops first so ffmpeg sees a clean EOF
/// on stdin and finalizes its container, then PipeWire, then the portal
/// session.
pub struct CastSession {
    format: CastFormat,
    frames: Arc<FrameSlot>,
    health: Arc<CastHealth>,
    /// Tells the writer thread to finish. Also read as "stopping" by the
    /// first-frame wait so a cancelled start doesn't sit out its timeout.
    stopping: Arc<AtomicBool>,
    pw_stop:  Option<pipewire::channel::Sender<Terminate>>,
    /// Closing this ends the portal thread, which closes the portal session.
    portal_stop: Option<Sender<()>>,
    pw_thread:     Option<std::thread::JoinHandle<()>>,
    writer_thread: Option<std::thread::JoinHandle<()>>,
    portal_thread: Option<std::thread::JoinHandle<()>>,
}

impl CastSession {
    /// Negotiate a screen cast and start pulling frames.
    ///
    /// Returns only once a frame has been copied out of a real buffer, so
    /// [`format`](CastSession::format) describes measured pixels. Frames
    /// captured between here and [`attach`](CastSession::attach) are dropped —
    /// there is nowhere to put them yet, and the newest one is the one that
    /// matters.
    pub fn start() -> Result<Self> {
        let (node_id, fd, portal_stop, portal_thread) = start_portal()?;

        let frames   = Arc::new(FrameSlot::new());
        let health   = Arc::new(CastHealth::new());
        let stopping = Arc::new(AtomicBool::new(false));
        let (fmt_tx, fmt_rx) = std::sync::mpsc::channel::<std::result::Result<CastFormat, String>>();
        let (pw_stop, pw_rx) = pipewire::channel::channel::<Terminate>();

        let pw_thread = {
            let frames = Arc::clone(&frames);
            let health = Arc::clone(&health);
            std::thread::Builder::new()
                .name("kashot-pipewire".into())
                .spawn(move || pipewire_thread(fd, node_id, frames, health, fmt_tx, pw_rx))
                .map_err(|e| Error::Recording(format!("couldn't start the capture thread: {e}")))?
        };

        // Partially-built session, so every early return below tears down the
        // threads that are already running rather than leaking them.
        let mut session = CastSession {
            // Replaced below; a zero format never escapes this function.
            format: CastFormat { width: 0, height: 0, pix_fmt: "bgr0" },
            frames,
            health,
            stopping,
            pw_stop:  Some(pw_stop),
            portal_stop: Some(portal_stop),
            pw_thread:     Some(pw_thread),
            writer_thread: None,
            portal_thread: Some(portal_thread),
        };

        let format = match fmt_rx.recv_timeout(FIRST_FRAME_TIMEOUT) {
            Ok(Ok(f))    => f,
            Ok(Err(msg)) => return Err(Error::Recording(msg)),
            Err(_) => return Err(Error::Recording(format!(
                "your desktop set up a screen cast but sent no usable frame within {} \
                 seconds. This usually means the compositor is handing out GPU buffers \
                 Kashot can't read. Recording from an X11 / Xorg session works today; \
                 please report your desktop and version so this can be fixed.",
                FIRST_FRAME_TIMEOUT.as_secs()
            ))),
        };

        // The format arrives with the first frame, so this is already true —
        // it's re-checked because everything downstream sizes buffers from it.
        if !session.frames.wait_for_first(FIRST_FRAME_TIMEOUT) {
            return Err(Error::Recording(
                "your desktop's screen cast produced no frames".into()));
        }
        session.format = format;
        Ok(session)
    }

    /// `Some(reason)` once the cast has stopped producing on its own — the
    /// user revoked sharing, or the compositor tore the session down. Cheap
    /// enough to call from the event loop every tick, which is what
    /// `Recorder::poll_health` does.
    pub fn failure(&self) -> Option<String> {
        // Our own teardown disconnects the stream, which reports itself as
        // `Unconnected` like any other end-of-session. Once we've asked to
        // stop, nothing the stream says is news.
        if self.stopping.load(Ordering::Relaxed) { return None; }
        self.health.failure()
    }

    /// The negotiated pixel layout. Feed it to ffmpeg as
    /// `-f rawvideo -pix_fmt <pix_fmt> -video_size <width>x<height>`.
    pub fn format(&self) -> CastFormat { self.format }

    /// Start writing frames into `stdin` at [`TARGET_FPS`].
    ///
    /// Takes ffmpeg's stdin by value: the writer owns it for the rest of the
    /// recording, and dropping it at the end is what gives ffmpeg the EOF it
    /// needs to finalize the file.
    pub fn attach(&mut self, stdin: ChildStdin) -> Result<()> {
        if self.writer_thread.is_some() {
            return Err(Error::Recording("this screen cast is already recording".into()));
        }
        let frames   = Arc::clone(&self.frames);
        let stopping = Arc::clone(&self.stopping);
        let format   = self.format;
        let handle = std::thread::Builder::new()
            .name("kashot-cast-writer".into())
            .spawn(move || writer_thread(stdin, frames, stopping, format))
            .map_err(|e| Error::Recording(format!("couldn't start the frame writer: {e}")))?;
        self.writer_thread = Some(handle);
        Ok(())
    }

    /// Ask the writer to stop, without waiting for it.
    ///
    /// Mirrors the Windows audio pumps' `signal_stop`: the recorder signals
    /// everything first, then waits on ffmpeg, then joins. Dropping stdin here
    /// (which the writer does as it exits) is what starts ffmpeg's finalize.
    pub fn signal_stop(&self) {
        self.stopping.store(true, Ordering::Relaxed);
    }

    /// Stop everything and wait for it. Safe to call more than once.
    pub fn stop(&mut self) {
        self.signal_stop();
        if let Some(h) = self.writer_thread.take() { let _ = h.join(); }
        if let Some(s) = self.pw_stop.take()       { let _ = s.send(Terminate); }
        if let Some(h) = self.pw_thread.take()     { let _ = h.join(); }
        // Dropping the sender ends the portal thread's recv, which closes the
        // session. Done last: closing it earlier kills the PipeWire node out
        // from under a stream we're still shutting down.
        drop(self.portal_stop.take());
        if let Some(h) = self.portal_thread.take() { let _ = h.join(); }
    }
}

impl Drop for CastSession {
    fn drop(&mut self) { self.stop(); }
}

// ── portal ──────────────────────────────────────────────────────────────────

/// Run the ScreenCast portal handshake and keep the session open.
///
/// Returns the PipeWire node id, the remote fd, a sender whose drop ends the
/// session, and the thread's handle.
fn start_portal() -> Result<(u32, OwnedFd, Sender<()>, std::thread::JoinHandle<()>)> {
    let (ready_tx, ready_rx) =
        std::sync::mpsc::channel::<std::result::Result<(u32, OwnedFd), String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    let thread = std::thread::Builder::new()
        .name("kashot-screencast".into())
        .spawn(move || portal_thread(ready_tx, stop_rx))
        .map_err(|e| Error::Recording(format!("couldn't start the portal thread: {e}")))?;

    match ready_rx.recv_timeout(PORTAL_TIMEOUT) {
        Ok(Ok((node_id, fd))) => Ok((node_id, fd, stop_tx, thread)),
        Ok(Err(msg))          => Err(Error::Recording(msg)),
        Err(_) => Err(Error::Recording(format!(
            "your desktop didn't set up a screen cast within {} seconds — the sharing \
             prompt may still be waiting, or the portal may not be running. Recording \
             was not started.",
            PORTAL_TIMEOUT.as_secs()
        ))),
    }
}

fn portal_thread(
    ready: Sender<std::result::Result<(u32, OwnedFd), String>>,
    stop:  Receiver<()>,
) {
    pollster::block_on(async move {
        let proxy = match Screencast::new().await {
            Ok(p)  => p,
            Err(e) => { let _ = ready.send(Err(portal_missing(&e))); return; }
        };
        let session = match proxy.create_session().await {
            Ok(s)  => s,
            Err(e) => { let _ = ready.send(Err(portal_missing(&e))); return; }
        };

        // Reuse this run's grant if the user already picked a screen once.
        let token = restore_token_slot().lock().ok().and_then(|t| t.clone());
        let select = proxy.select_sources(
            &session,
            // Embedded: the pointer belongs in a screen recording, and it's
            // the only mode every backend implements.
            CursorMode::Embedded,
            SourceType::Monitor.into(),
            false,
            token.as_deref(),
            PersistMode::Application,
        ).await;
        if let Err(e) = select {
            let _ = session.close().await;
            let _ = ready.send(Err(format!(
                "your desktop refused to set up screen sharing ({e}).")));
            return;
        }

        let streams = match proxy.start(&session, None).await {
            Ok(request) => match request.response() {
                Ok(s)  => s,
                Err(e) => {
                    let _ = session.close().await;
                    let _ = ready.send(Err(start_refused(&e)));
                    return;
                }
            },
            Err(e) => {
                let _ = session.close().await;
                let _ = ready.send(Err(start_refused(&e)));
                return;
            }
        };

        if let Some(t) = streams.restore_token() {
            if let Ok(mut slot) = restore_token_slot().lock() {
                *slot = Some(t.to_string());
            }
        }

        let Some(stream) = streams.streams().first() else {
            let _ = session.close().await;
            let _ = ready.send(Err(
                "your desktop started a screen cast but shared no screen with it. \
                 Pick a monitor when the sharing prompt appears.".to_string()));
            return;
        };
        let node_id = stream.pipe_wire_node_id();

        let fd = match proxy.open_pipe_wire_remote(&session).await {
            Ok(fd) => fd,
            Err(e) => {
                let _ = session.close().await;
                let _ = ready.send(Err(format!(
                    "your desktop wouldn't open the screen-cast stream ({e}).")));
                return;
            }
        };

        if ready.send(Ok((node_id, fd))).is_err() {
            let _ = session.close().await;
            return;
        }

        // Hold the session open for the recording. `recv` returns on stop or
        // when the sender is dropped; either way the session closes here and
        // nowhere else.
        let _ = stop.recv();
        let _ = session.close().await;
    });
}

fn portal_missing(e: &ashpd::Error) -> String {
    format!(
        "this is a Wayland session, where screen recording goes through \
         xdg-desktop-portal — and the portal didn't answer ({e}). Install and start \
         xdg-desktop-portal plus the backend for your desktop (for example \
         xdg-desktop-portal-gnome, -kde or -wlr), then try again."
    )
}

fn start_refused(e: &ashpd::Error) -> String {
    format!("screen sharing wasn't granted, so no recording was started ({e}).")
}

// ── pipewire ────────────────────────────────────────────────────────────────

/// What the stream callbacks accumulate between them.
struct StreamState {
    /// Set by `param_changed` once the compositor fixates a format.
    format: Option<CastFormat>,
    /// Cleared after the first frame is published, so the recorder is told the
    /// format exactly once.
    announce: Option<Sender<std::result::Result<CastFormat, String>>>,
    /// Reused across frames so the copy doesn't allocate 8 MB thirty times a
    /// second.
    scratch: Vec<u8>,
    /// Frames we had to skip because the buffer wasn't host-readable. Reported
    /// once so an all-DMA-BUF compositor produces a diagnosis rather than a
    /// silent stall.
    unmappable: u64,
}

fn pipewire_thread(
    fd:      OwnedFd,
    node_id: u32,
    frames:  Arc<FrameSlot>,
    health:  Arc<CastHealth>,
    announce: Sender<std::result::Result<CastFormat, String>>,
    stop_rx: pipewire::channel::Receiver<Terminate>,
) {
    use pipewire as pw;

    // `pw_init` is process-global and not re-entrant; xcap calls it too.
    static PW_INIT: std::sync::Once = std::sync::Once::new();
    PW_INIT.call_once(pw::init);

    let report = |announce: &Sender<std::result::Result<CastFormat, String>>, msg: String| {
        let _ = announce.send(Err(msg));
    };

    let mainloop = match pw::main_loop::MainLoopRc::new(None) {
        Ok(m)  => m,
        Err(e) => return report(&announce, format!("couldn't start PipeWire: {e}")),
    };
    let context = match pw::context::ContextRc::new(&mainloop, None) {
        Ok(c)  => c,
        Err(e) => return report(&announce, format!("couldn't create a PipeWire context: {e}")),
    };
    let core = match context.connect_fd_rc(fd, None) {
        Ok(c)  => c,
        Err(e) => return report(&announce, format!(
            "couldn't connect to the PipeWire stream your desktop opened: {e}")),
    };

    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE     => "Video",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE     => "Screen",
    };
    let stream = match pw::stream::StreamRc::new(core, "kashot-screen", props) {
        Ok(s)  => s,
        Err(e) => return report(&announce, format!("couldn't create the capture stream: {e}")),
    };

    let state = StreamState {
        format: None,
        announce: Some(announce.clone()),
        scratch: Vec::new(),
        unmappable: 0,
    };

    let listener = stream
        .add_local_listener_with_user_data(state)
        .state_changed({
            let health = Arc::clone(&health);
            move |_stream, _state, _old, new| {
                match new {
                    pw::stream::StreamState::Error(ref why) => health.fail(format!(
                        "the screen cast failed: {why}")),
                    // Reached when the session ends from the other side — the
                    // user pressing "Stop sharing", or the compositor closing
                    // the session. Not reached during a normal stop: we quit
                    // the loop first and the listener is dropped with it.
                    pw::stream::StreamState::Unconnected => health.fail(
                        "screen sharing was stopped from your desktop, so the recording \
                         lost its picture.".to_string()),
                    _ => {}
                }
            }
        })
        .param_changed(|_stream, state, id, param| {
            let Some(param) = param else { return };
            if id != pw::spa::param::ParamType::Format.as_raw() { return; }

            let Ok((media_type, media_subtype)) =
                pw::spa::param::format_utils::parse_format(param) else { return };
            if media_type != pw::spa::param::format::MediaType::Video
                || media_subtype != pw::spa::param::format::MediaSubtype::Raw
            {
                return;
            }

            let mut info = pw::spa::param::video::VideoInfoRaw::default();
            if info.parse(param).is_err() { return; }

            let size = info.size();
            match ffmpeg_pix_fmt(info.format()) {
                Some(pix_fmt) if size.width > 0 && size.height > 0 => {
                    state.format = Some(CastFormat {
                        width:  size.width,
                        height: size.height,
                        pix_fmt,
                    });
                }
                // We only ever offered layouts `ffmpeg_pix_fmt` knows, so this
                // is a compositor answering outside the enumeration. Leave the
                // format unset: `process` then has nothing to copy into and the
                // first-frame timeout reports it.
                _ => state.format = None,
            }
        })
        .process({
            let frames = Arc::clone(&frames);
            move |stream, state| {
                let Some(format) = state.format else { return };
                let Some(mut buffer) = stream.dequeue_buffer() else { return };
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else { return };

                let chunk_size = data.chunk().size() as usize;
                let offset     = data.chunk().offset() as usize;
                let raw_stride = data.chunk().stride();
                // A zero-sized chunk is how PipeWire says "nothing changed";
                // the writer will re-send the previous frame on its own.
                if chunk_size == 0 { return; }

                let row = format.width as usize * 4;
                // A negative or absent stride means "tightly packed".
                let stride = if raw_stride > 0 { raw_stride as usize } else { row };

                let height = format.height as usize;
                let Some(bytes) = data.data() else {
                    // DMA-BUF or otherwise unmapped: there are no host-visible
                    // pixels here to copy.
                    state.unmappable = state.unmappable.saturating_add(1);
                    return;
                };

                state.scratch.clear();
                state.scratch.reserve(format.frame_len());
                for y in 0..height {
                    let start = offset + y * stride;
                    let end   = start + row;
                    if end > bytes.len() {
                        // Truncated buffer — publishing a half frame would put
                        // a torn image in the file. Skip and wait for the next.
                        return;
                    }
                    state.scratch.extend_from_slice(&bytes[start..end]);
                }

                frames.publish(&mut state.scratch);
                // Told once, and only after a frame that really copied.
                if let Some(tx) = state.announce.take() {
                    let _ = tx.send(Ok(format));
                }
            }
        })
        .register();

    let listener = match listener {
        Ok(l)  => l,
        Err(e) => return report(&announce, format!("couldn't listen to the capture stream: {e}")),
    };

    let values = enum_format_pod();
    let Some(pod) = pw::spa::pod::Pod::from_bytes(&values) else {
        return report(&announce, "couldn't describe the video formats to PipeWire".into());
    };
    let mut params = [pod];

    if let Err(e) = stream.connect(
        pw::spa::utils::Direction::Input,
        Some(node_id),
        // No RT_PROCESS: `process` does a memcpy under a mutex, which has no
        // business on a real-time thread. MAP_BUFFERS is what makes
        // `data.data()` return host-visible pixels at all.
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    ) {
        return report(&announce, format!("couldn't start the capture stream: {e}"));
    }

    let _stop = stop_rx.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |_| mainloop.quit()
    });

    mainloop.run();

    // Ordered teardown: stop the flow, then drop the callbacks that reference
    // the shared slot.
    let _ = stream.disconnect();
    drop(listener);
}

/// The `SPA_PARAM_EnumFormat` pod: every packed 32-bit RGB layout we can copy
/// straight through, at any size and any frame rate up to a sane ceiling.
///
/// Deliberately *not* offering a DMA-BUF modifier: without one the compositor
/// negotiates host-visible (memfd) buffers, which is what makes the frame copy
/// a memcpy instead of a GBM import and an EGL context.
fn enum_format_pod() -> Vec<u8> {
    use pipewire::spa;
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::param::video::VideoFormat;
    use spa::utils::{Fraction, Rectangle};

    let object = spa::pod::object! {
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice, Enum, Id,
            // Default first, then every alternative (the default repeats).
            VideoFormat::BGRx,
            VideoFormat::BGRx, VideoFormat::RGBx,
            VideoFormat::BGRA, VideoFormat::RGBA,
            VideoFormat::xRGB, VideoFormat::xBGR,
            VideoFormat::ARGB, VideoFormat::ABGR,
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Choice, Range, Rectangle,
            Rectangle { width: 1920, height: 1080 },
            Rectangle { width: 1, height: 1 },
            Rectangle { width: 16384, height: 16384 }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice, Range, Fraction,
            Fraction { num: TARGET_FPS, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction { num: 240, denom: 1 }
        ),
    };

    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .expect("serializing a fixed, self-describing pod cannot fail")
    .0
    .into_inner()
}

// ── writer ──────────────────────────────────────────────────────────────────

/// Write frames to ffmpeg at a fixed cadence until told to stop.
///
/// Exits by returning, which drops `stdin` and gives ffmpeg the EOF it
/// finalizes on. A write error also ends the loop: the encoder is gone, and
/// the recorder's own health poll is what reports that to the user.
fn writer_thread(
    mut stdin: ChildStdin,
    frames:    Arc<FrameSlot>,
    stopping:  Arc<AtomicBool>,
    format:    CastFormat,
) {
    use std::io::Write;

    let interval = Duration::from_nanos(1_000_000_000 / TARGET_FPS as u64);
    let expected = format.frame_len();
    // Scratch space handed to the frame slot, so taking a frame is a pointer
    // swap rather than an 8 MB copy.
    let mut scratch: Vec<u8> = Vec::with_capacity(expected);
    // The frame currently being sent. Held separately from `scratch` so a
    // frame that doesn't match the declared size — a mid-recording resolution
    // change is the only way to get one — is ignored instead of blanking the
    // picture. ffmpeg's frame count is fixed by the command line, so sending
    // a differently-sized frame would shear every frame after it.
    let mut sending: Vec<u8> = Vec::with_capacity(expected);
    let mut last_seq = 0u64;
    let mut next = Instant::now();

    while !stopping.load(Ordering::Relaxed) {
        if frames.take_newer(&mut scratch, &mut last_seq) && scratch.len() == expected {
            std::mem::swap(&mut sending, &mut scratch);
        }

        // Before the first frame there is nothing to send; ffmpeg is happy to
        // wait on a quiet pipe.
        if sending.len() == expected && stdin.write_all(&sending).is_err() {
            return;
        }

        next += interval;
        let now = Instant::now();
        if now < next {
            std::thread::sleep(next - now);
        } else if now - next > interval {
            // More than a frame behind (a stalled encoder, a suspended
            // laptop). Re-base instead of writing a catch-up burst that would
            // play back as a speed-up.
            next = now;
        }
    }

    let _ = stdin.flush();
}
