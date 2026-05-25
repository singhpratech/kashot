//! macOS system-audio capture via ScreenCaptureKit.
//!
//! avfoundation can only fold a *mic* into a recording — there's no driverless
//! way to grab the system-audio mix, which is why the old path errored asking
//! the user to install BlackHole. ScreenCaptureKit (macOS 13+) captures the
//! system mix natively under the same Screen-Recording permission Kashot
//! already holds, with no virtual-audio driver.
//!
//! We use SCK for **system audio only**: an `SCStream` with `capturesAudio`
//! delivers Float32 PCM sample buffers to a delegate on a background queue. We
//! pull the PCM out and write it to a 127.0.0.1 socket that ffmpeg reads as an
//! extra raw-PCM input — the same transport the Windows WASAPI path uses, and
//! the same role the Linux `<sink>.monitor` source plays. Video and mic stay
//! on the existing avfoundation path; ffmpeg muxes/`amix`es everything.
//!
//! SCK audio is delivered **non-interleaved** (one buffer per channel), so we
//! interleave to plain `f32le` before handing it to ffmpeg. Unlike WASAPI
//! loopback, SCK keeps delivering buffers (silence included) while the system
//! is quiet, so no silence-fill clock is needed here.

#![allow(non_snake_case)]

use crate::{Error, Result};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use objc2_core_foundation::CFRetained;
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType, SCWindow,
};

/// System audio is always negotiated at this format (we set it on the config),
/// so the ffmpeg input can be built without a runtime handshake.
const SAMPLE_RATE: u32 = 48_000;
const CHANNELS:    u16 = 2;
pub(crate) const FFMPEG_FMT: &str = "f32le";

/// A live SCK system-audio capture. Held in the `Process` backend alongside the
/// ffmpeg child so it can be stopped when the recording stops.
pub(crate) struct SckSession {
    stream:   Retained<SCStream>,
    _delegate: Retained<AudioDelegate>,
    stop:     Arc<AtomicBool>,
    socket:   Arc<Mutex<Option<TcpStream>>>,
    acceptor: Option<JoinHandle<()>>,
    pub port: u16,
}

impl SckSession {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Stop the stream and wait for the completion handler so no more audio
        // callbacks fire after we tear the socket down.
        let (tx, rx) = mpsc::channel::<()>();
        let handler = RcBlock::new(move |_err: *mut NSError| {
            let _ = tx.send(());
        });
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&handler)); }
        let _ = rx.recv_timeout(Duration::from_secs(5));
        // Drop the socket so ffmpeg sees EOF on this input.
        if let Ok(mut g) = self.socket.lock() {
            *g = None;
        }
        if let Some(h) = self.acceptor.take() {
            let _ = h.join();
        }
    }
}

/// Start capturing the system-audio mix. Returns the session (to stop later)
/// and the loopback port ffmpeg should read from. The format is fixed
/// (`f32le`, 48 kHz, stereo) by the configuration we set.
pub(crate) fn start_system_audio() -> Result<SckSession> {
    // Listen first so the port exists before ffmpeg is spawned.
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| Error::Recording(format!("could not open a loopback socket for system audio: {e}")))?;
    let port = listener.local_addr()
        .map_err(|e| Error::Recording(format!("could not read loopback port: {e}")))?
        .port();

    // First available display — SCContentFilter needs one even though we only
    // consume audio. The display enumeration is async; block on it.
    let display = first_display()?;

    // Audio-only-oriented config: capture audio, tiny video (we never read the
    // screen buffers), exclude our own output so Kashot's UI sounds aren't fed
    // back into the recording.
    let config = unsafe {
        let c = SCStreamConfiguration::new();
        c.setCapturesAudio(true);
        c.setSampleRate(SAMPLE_RATE as isize);
        c.setChannelCount(CHANNELS as isize);
        c.setExcludesCurrentProcessAudio(true);
        c.setWidth(2);
        c.setHeight(2);
        c
    };

    let empty_windows: Retained<NSArray<SCWindow>> = NSArray::new();
    let filter = unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &display,
            &empty_windows,
        )
    };

    let stop = Arc::new(AtomicBool::new(false));
    let socket: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
    let delegate = AudioDelegate::new(Arc::clone(&socket), Arc::clone(&stop));

    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None)
    };
    let proto: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*delegate);
    // None queue → SCK uses its own internal serial queue for our callback.
    unsafe {
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(proto, SCStreamOutputType::Audio, None)
            .map_err(|e| Error::Recording(format!("could not attach the system-audio output: {e}")))?;
    }

    // Start, blocking on the async completion handler.
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let start_handler = RcBlock::new(move |err: *mut NSError| {
        let msg = unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string());
        let _ = tx.send(msg);
    });
    unsafe { stream.startCaptureWithCompletionHandler(Some(&start_handler)); }
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(None) => {}
        Ok(Some(msg)) => {
            return Err(Error::Recording(format!(
                "macOS could not start system-audio capture: {msg}. Grant Kashot \
                 Screen Recording permission in System Settings → Privacy & \
                 Security → Screen Recording, then retry."
            )));
        }
        Err(_) => {
            return Err(Error::Recording(
                "macOS system-audio capture didn't start within 10s.".into()));
        }
    }

    // Accept ffmpeg's connection on a small thread and stash the stream where
    // the audio callback can find it.
    let acc_socket = Arc::clone(&socket);
    let acc_stop = Arc::clone(&stop);
    let acceptor = thread::Builder::new()
        .name("kashot-sck-accept".into())
        .spawn(move || {
            let _ = listener.set_nonblocking(true);
            for _ in 0..1000 {
                if acc_stop.load(Ordering::Relaxed) { return; }
                match listener.accept() {
                    Ok((s, _)) => {
                        let _ = s.set_nodelay(true);
                        if let Ok(mut g) = acc_socket.lock() { *g = Some(s); }
                        return;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            }
        })
        .map_err(|e| Error::Recording(format!("could not start system-audio acceptor: {e}")))?;

    Ok(SckSession {
        stream,
        _delegate: delegate,
        stop,
        socket,
        acceptor: Some(acceptor),
        port,
    })
}

/// Block on the async `getShareableContentWithCompletionHandler` and return the
/// first display. The objc objects are moved across the completion-handler
/// thread boundary as raw pointers (their refcount is atomic), which keeps the
/// `Send` checker happy without unsafe assertions on `Retained`.
fn first_display() -> Result<Retained<objc2_screen_capture_kit::SCDisplay>> {
    let (tx, rx) = mpsc::channel::<std::result::Result<usize, String>>();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        if let Some(e) = unsafe { err.as_ref() } {
            let _ = tx.send(Err(e.localizedDescription().to_string()));
            return;
        }
        let Some(content) = (unsafe { content.as_ref() }) else {
            let _ = tx.send(Err("no shareable content returned".into()));
            return;
        };
        let displays = unsafe { content.displays() };
        match displays.firstObject() {
            Some(d) => { let _ = tx.send(Ok(Retained::into_raw(d) as usize)); }
            None    => { let _ = tx.send(Err("no displays available to capture".into())); }
        }
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler); }

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(raw)) => {
            let ptr = raw as *mut objc2_screen_capture_kit::SCDisplay;
            unsafe { Retained::from_raw(ptr) }
                .ok_or_else(|| Error::Recording("display pointer was null".into()))
        }
        Ok(Err(msg)) => Err(Error::Recording(format!(
            "macOS could not enumerate displays for system-audio capture: {msg}"))),
        Err(_) => Err(Error::Recording(
            "macOS display enumeration timed out.".into())),
    }
}

struct AudioDelegateIvars {
    socket: Arc<Mutex<Option<TcpStream>>>,
    stop:   Arc<AtomicBool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "KashotSCKAudioDelegate"]
    #[ivars = AudioDelegateIvars]
    struct AudioDelegate;

    unsafe impl NSObjectProtocol for AudioDelegate {}

    unsafe impl SCStreamOutput for AudioDelegate {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_didOutputSampleBuffer_ofType(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            ty: SCStreamOutputType,
        ) {
            if ty != SCStreamOutputType::Audio { return; }
            if self.ivars().stop.load(Ordering::Relaxed) { return; }
            self.forward_audio(sample_buffer);
        }
    }
);

impl AudioDelegate {
    fn new(socket: Arc<Mutex<Option<TcpStream>>>, stop: Arc<AtomicBool>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(AudioDelegateIvars { socket, stop });
        unsafe { msg_send![super(this), init] }
    }

    /// Pull the PCM out of one audio sample buffer, interleave the per-channel
    /// planes, and write `f32le` to the socket (once ffmpeg has connected).
    fn forward_audio(&self, sbuf: &CMSampleBuffer) {
        // Storage for an AudioBufferList with up to 8 channel buffers.
        const MAX_BUFFERS: usize = 8;
        let mut storage = vec![
            0u8;
            std::mem::size_of::<AudioBufferList>()
                + (MAX_BUFFERS - 1) * std::mem::size_of::<AudioBuffer>()
        ];
        let abl = storage.as_mut_ptr() as *mut AudioBufferList;
        let mut size_needed: usize = 0;
        let mut block_buffer: *mut objc2_core_media::CMBlockBuffer = ptr::null_mut();

        // flag 1 = kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment
        let status = unsafe {
            sbuf.audio_buffer_list_with_retained_block_buffer(
                &mut size_needed,
                abl,
                storage.len(),
                None,
                None,
                1,
                &mut block_buffer,
            )
        };
        if status != 0 { return; }
        // Own the returned (+1) block buffer so it's released when we're done.
        let _owned_block = NonNull::new(block_buffer).map(|nn| unsafe { CFRetained::from_raw(nn) });

        let interleaved = unsafe { interleave_pcm(abl) };
        if interleaved.is_empty() { return; }

        if let Ok(mut guard) = self.ivars().socket.lock() {
            if let Some(stream) = guard.as_mut() {
                if stream.write_all(&interleaved).is_err() {
                    // ffmpeg closed the socket → recording is stopping.
                    *guard = None;
                }
            }
            // If ffmpeg hasn't connected yet (guard is None) we drop this buffer;
            // that's at most a few ms of leading audio.
        }
    }
}

/// Interleave the (possibly multi-plane) Float32 audio in `abl` into one
/// little-endian interleaved `f32` byte buffer. A single buffer is already
/// interleaved (or mono) and is copied as-is.
unsafe fn interleave_pcm(abl: *mut AudioBufferList) -> Vec<u8> {
    let nbuf = unsafe { (*abl).mNumberBuffers } as usize;
    if nbuf == 0 { return Vec::new(); }
    let first = unsafe { (*abl).mBuffers.as_ptr() };

    if nbuf == 1 {
        let b = unsafe { &*first };
        if b.mData.is_null() { return Vec::new(); }
        let bytes = unsafe { std::slice::from_raw_parts(b.mData as *const u8, b.mDataByteSize as usize) };
        return bytes.to_vec();
    }

    // Planar: one f32 plane per channel. Interleave frame-by-frame.
    let planes: Vec<&[f32]> = (0..nbuf)
        .map(|i| {
            let b = unsafe { &*first.add(i) };
            if b.mData.is_null() {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(b.mData as *const f32, (b.mDataByteSize as usize) / 4) }
            }
        })
        .collect();
    let frames = planes.iter().map(|p| p.len()).min().unwrap_or(0);
    let mut out = Vec::with_capacity(frames * nbuf * 4);
    for f in 0..frames {
        for plane in &planes {
            out.extend_from_slice(&plane[f].to_le_bytes());
        }
    }
    out
}
