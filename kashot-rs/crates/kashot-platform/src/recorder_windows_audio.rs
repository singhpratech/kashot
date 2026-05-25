//! Windows system-audio + microphone capture via WASAPI.
//!
//! DirectShow has no general "record what the speakers are playing" device —
//! "Stereo Mix" is disabled by default on modern Windows and absent on many
//! chipsets, and a virtual cable (VB-Audio / VoiceMeeter) means asking the user
//! to install a kernel driver. WASAPI **loopback** solves this with zero setup:
//! opening the default *render* endpoint in capture mode hands us exactly the
//! mix that's going to the speakers. The default *capture* endpoint gives the
//! microphone the same way.
//!
//! Transport: `Recorder` stops ffmpeg by writing `q` to its **stdin**, so the
//! captured PCM can't share that pipe. Instead each source owns a loopback
//! `TcpListener`; ffmpeg connects back as a client
//! (`-f f32le -ar R -ac C -i tcp://127.0.0.1:<port>`) and does the resample +
//! `amix` itself, mirroring the Linux `pulse` + monitor path. We never touch
//! the samples beyond forwarding the device-native PCM and declaring its format
//! to ffmpeg.
//!
//! WASAPI loopback emits **no packets at all** while the render endpoint is
//! idle (nothing playing), which would make the audio track shorter than the
//! video and drift out of sync. The capture loop fills those idle gaps with
//! silence keyed off a monotonic clock so the stream always advances in real
//! time.

use crate::{Error, Result};
use std::collections::VecDeque;
use std::io::Write;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use wasapi::{Direction, SampleType, ShareMode};

/// Which endpoint to capture from.
#[derive(Clone, Copy)]
pub(crate) enum SourceKind {
    /// Default render endpoint in loopback mode → what the speakers play.
    SystemLoopback,
    /// Default capture endpoint → the microphone.
    Microphone,
}

impl SourceKind {
    fn label(self) -> &'static str {
        match self {
            SourceKind::SystemLoopback => "system audio",
            SourceKind::Microphone     => "microphone",
        }
    }
}

/// A started WASAPI source: the negotiated PCM format (so the caller can build
/// the matching ffmpeg `-i`) plus the pump owning the capture thread.
pub(crate) struct StartedSource {
    pub spec: super::WasapiAudioSpec,
    pub pump: super::AudioPump,
}

/// Format handshake the capture thread sends back once the stream is live.
struct FmtInfo {
    sample_rate: u32,
    channels:    u16,
    ffmpeg_fmt:  &'static str,
}

/// Open `kind`, negotiate its format, and start streaming PCM over a fresh
/// loopback TCP listener. Returns once the capture thread has initialized the
/// stream (so the format is known) — or an actionable error if the device
/// can't be opened (the common case being Windows microphone-privacy denial).
pub(crate) fn start_source(kind: SourceKind) -> Result<StartedSource> {
    // Bind on this thread so the port is known before ffmpeg is spawned and the
    // listening socket is already queued to accept ffmpeg's connection.
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| Error::Recording(format!("could not open a loopback socket for {}: {e}", kind.label())))?;
    let port = listener.local_addr()
        .map_err(|e| Error::Recording(format!("could not read loopback port: {e}")))?
        .port();
    listener.set_nonblocking(true)
        .map_err(|e| Error::Recording(format!("could not configure loopback socket: {e}")))?;

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Result<FmtInfo>>();

    let stop_thread = Arc::clone(&stop);
    let handle = thread::Builder::new()
        .name(format!("kashot-wasapi-{}", kind.label().replace(' ', "-")))
        .spawn(move || run_capture(kind, listener, stop_thread, tx))
        .map_err(|e| Error::Recording(format!("could not start {} capture thread: {e}", kind.label())))?;

    // Wait for the thread to report the negotiated format (or a startup error).
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(fmt)) => Ok(StartedSource {
            spec: super::WasapiAudioSpec {
                port,
                sample_rate: fmt.sample_rate,
                channels:    fmt.channels,
                ffmpeg_fmt:  fmt.ffmpeg_fmt,
            },
            pump: super::AudioPump { stop, handle: Some(handle) },
        }),
        Ok(Err(e)) => {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
            Err(e)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
            Err(Error::Recording(format!(
                "{} capture didn't start within 5s — the audio device may be \
                 held in exclusive mode by another app.", kind.label()
            )))
        }
    }
}

/// Capture-thread entry point. All WASAPI/COM objects live here (COM is
/// apartment-bound). On any error before the format handshake, the error is
/// reported back to `start_source`; errors after capture has begun just stop
/// this one source (its socket closes and ffmpeg sees EOF).
fn run_capture(
    kind:     SourceKind,
    listener: TcpListener,
    stop:     Arc<AtomicBool>,
    tx:       mpsc::Sender<Result<FmtInfo>>,
) {
    if let Err(e) = init_and_pump(kind, listener, &stop, &tx) {
        // If the handshake hasn't been sent yet this surfaces to the user;
        // if it has, the receiver is gone and this send is a harmless no-op.
        let _ = tx.send(Err(e));
    }
}

fn init_and_pump(
    kind:     SourceKind,
    listener: TcpListener,
    stop:     &AtomicBool,
    tx:       &mpsc::Sender<Result<FmtInfo>>,
) -> Result<()> {
    let map = |ctx: &str| {
        let ctx = ctx.to_string();
        move |e: Box<dyn std::error::Error>| classify_wasapi_error(kind, &ctx, &e.to_string())
    };

    // COM init for this thread (MTA — we own no window/message loop here).
    // `initialize_mta` returns an HRESULT; `.ok()` turns it into a Result.
    wasapi::initialize_mta().ok()
        .map_err(|e| Error::Recording(format!("could not initialize Windows audio (COM): {e}")))?;

    // Loopback = render endpoint opened for capture; mic = capture endpoint.
    let device_dir = match kind {
        SourceKind::SystemLoopback => Direction::Render,
        SourceKind::Microphone     => Direction::Capture,
    };
    let device = wasapi::get_default_device(&device_dir)
        .map_err(map("no default audio device"))?;
    let mut audio_client = device.get_iaudioclient()
        .map_err(map("could not open the audio client"))?;

    let format = audio_client.get_mixformat()
        .map_err(map("could not read the device audio format"))?;
    let sample_rate = format.get_samplespersec();
    let channels    = format.get_nchannels();
    let bits        = format.get_bitspersample();
    let ffmpeg_fmt  = pcm_ffmpeg_format(&format, bits)?;
    let blockalign  = format.get_blockalign() as usize;

    let (_default_period, min_period) = audio_client.get_periods()
        .map_err(map("could not read the device timing"))?;

    // Always initialize the *stream* in capture mode; the loopback flag is
    // implied by having opened the render device above.
    audio_client.initialize_client(
        &format,
        min_period,
        &Direction::Capture,
        &ShareMode::Shared,
        false, // no autoconvert — we hand ffmpeg the device-native mix format
    ).map_err(map("could not start the audio stream"))?;

    let capture_client = audio_client.get_audiocaptureclient()
        .map_err(map("could not open the capture client"))?;
    // `initialize_client` always sets the event-callback flag, so an event
    // handle must be registered before `start_stream` or it fails. We wait on
    // this handle for "buffer ready" rather than busy-polling.
    let h_event = audio_client.set_get_eventhandle()
        .map_err(map("could not set up the audio event"))?;
    audio_client.start_stream()
        .map_err(map("could not start audio capture"))?;

    // Stream is live: report the negotiated format so ffmpeg can be spawned.
    let _ = tx.send(Ok(FmtInfo { sample_rate, channels, ffmpeg_fmt }));

    // Accept ffmpeg's connection (non-blocking poll so a failed start can't
    // hang the thread).
    let mut socket = accept_with_timeout(&listener, stop, Duration::from_secs(10))?;

    let bytes_per_frame = blockalign.max(1);
    let mut queue: VecDeque<u8> = VecDeque::new();
    let silence = vec![0u8; bytes_per_frame * 1024];

    // Monotonic idle clock: WASAPI loopback emits NO buffers while the render
    // endpoint is idle, so the event wait simply times out. On a timeout we
    // synthesize silence for the elapsed wall-clock gap, keeping the audio
    // track the same length as the video instead of ending up short.
    let mut last_real = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        match h_event.wait_for_event(100) {
            Ok(()) => {
                // Drain every packet WASAPI has ready (one GetBuffer per call).
                queue.clear();
                loop {
                    let before = queue.len();
                    if capture_client.read_from_device_to_deque(&mut queue).is_err() {
                        break;
                    }
                    if queue.len() == before { break; } // no more frames waiting
                }
                if !queue.is_empty() {
                    let buf: Vec<u8> = queue.drain(..).collect();
                    if socket.write_all(&buf).is_err() {
                        break; // ffmpeg closed the socket → recording is stopping
                    }
                    last_real = Instant::now();
                }
            }
            Err(_) => {
                // Timed out → endpoint idle. Pad silence for the elapsed gap.
                let gap = last_real.elapsed();
                let frames = (gap.as_secs_f64() * sample_rate as f64) as usize;
                if frames > 0 {
                    let mut bytes = frames * bytes_per_frame;
                    while bytes > 0 {
                        let chunk = bytes.min(silence.len());
                        if socket.write_all(&silence[..chunk]).is_err() {
                            return finish(&mut audio_client, &map);
                        }
                        bytes -= chunk;
                    }
                    // Advance the clock by exactly the silence we emitted.
                    last_real += Duration::from_secs_f64(frames as f64 / sample_rate as f64);
                }
            }
        }
    }

    finish(&mut audio_client, &map)
}

/// Stop the WASAPI stream cleanly. Errors here are non-fatal to the recording
/// (the file is already being finalized by ffmpeg) but we surface them for logs.
fn finish<F, G>(audio_client: &mut wasapi::AudioClient, map: &F) -> Result<()>
where
    F: Fn(&str) -> G,
    G: FnOnce(Box<dyn std::error::Error>) -> Error,
{
    audio_client.stop_stream().map_err(map("could not stop audio capture"))?;
    Ok(())
}

/// Poll-accept a single connection, respecting the stop flag and a deadline.
fn accept_with_timeout(
    listener: &TcpListener,
    stop:     &AtomicBool,
    timeout:  Duration,
) -> Result<std::net::TcpStream> {
    let deadline = Instant::now() + timeout;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Recording("audio capture cancelled before ffmpeg connected".into()));
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(Error::Recording(
                        "ffmpeg never connected to the audio socket".into()));
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(Error::Recording(format!("audio socket accept failed: {e}"))),
        }
    }
}

/// Map a WASAPI mix format to the raw-PCM format string ffmpeg expects on its
/// `-f <fmt>` input. Shared-mode mix formats are essentially always 32-bit
/// float; 16/32-bit integer are handled defensively.
fn pcm_ffmpeg_format(format: &wasapi::WaveFormat, bits: u16) -> Result<&'static str> {
    let sample_type = format.get_subformat()
        .map_err(|e| Error::Recording(format!("could not read the device sample type: {e}")))?;
    match (sample_type, bits) {
        (SampleType::Float, 32) => Ok("f32le"),
        (SampleType::Int,   16) => Ok("s16le"),
        (SampleType::Int,   32) => Ok("s32le"),
        (other_type, other_bits) => Err(Error::Recording(format!(
            "unsupported audio sample format from the device \
             ({other_type:?}, {other_bits}-bit). Please report this with your \
             audio hardware so we can add it."
        ))),
    }
}

/// Turn a raw WASAPI error string into an actionable message. The dominant
/// real-world failure is Windows microphone privacy blocking the capture
/// endpoint (E_ACCESSDENIED / 0x80070005).
fn classify_wasapi_error(kind: SourceKind, ctx: &str, raw: &str) -> Error {
    let low = raw.to_lowercase();
    if matches!(kind, SourceKind::Microphone) {
        if low.contains("denied") || low.contains("0x80070005") {
            return Error::Recording(
                "Windows blocked microphone access for Kashot.\n\n\
                 Open Settings → Privacy & Security → Microphone, turn on \
                 \"Microphone access\" AND \"Let desktop apps access your \
                 microphone\", then retry.".into()
            );
        }
        // 0x80070490 = ELEMENT_NOT_FOUND — there's no *default* recording
        // device. Usually no mic is plugged in / none is set as default, but
        // it can also be the privacy gate hiding it from desktop apps.
        if low.contains("element not found") || low.contains("0x80070490")
            || low.contains("not found") {
            return Error::Recording(
                "No microphone found.\n\n\
                 Plug in a microphone and set it as the default recording \
                 device in Windows Sound settings, then retry. If you do have \
                 one, also check Settings → Privacy & Security → Microphone → \
                 \"Microphone access\" and \"Let desktop apps access your \
                 microphone\" are ON.\n\n\
                 To record without a mic, choose system audio only.".into()
            );
        }
    }
    if low.contains("0x88890008") || low.contains("unsupported") {
        return Error::Recording(format!(
            "{} capture failed: the device format isn't supported ({raw}).",
            kind.label()
        ));
    }
    Error::Recording(format!("{ctx} for {} ({raw}).", kind.label()))
}
