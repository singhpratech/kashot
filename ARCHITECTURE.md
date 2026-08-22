# Kashot Architecture

## What this is

**Kashot** — a tray-resident screenshot + screen-recording tool with an in-place annotation editor, format conversion, and watermarking. Ships the **same Rust binary** on Windows, Linux, and macOS as of v0.3.0.

The repo previously carried a parallel C# / .NET 8 / WinForms build (`Kashot/`, `Installer/`, `build-csharp.yml`) — that's been retired now the Rust port covers all three platforms. Git history retains it if you need to look back.

## Naming

Project, namespace, assembly, binary, and every user-visible string are `Kashot` (technical identifier) or `KAShot` (user-visible brand). See [[feedback-author-attribution-vs-brand]] for the rule on which spelling goes where. The outer parent folder on disk happens to still be named `LightCapture` for historical reasons (originally LightCapture → PratShot → Kashot) — that name doesn't appear in code or any user-facing surface and you can ignore it.

## Build / Run

Cargo workspace under `kashot-rs/` with three crates:

| Crate              | Role                                                                  |
|--------------------|-----------------------------------------------------------------------|
| `kashot-core`      | Pure logic: `Tool`, `Annotation`, `AppSettings`, theme, state machine, annotation hit-testing (`edit.rs`) + undo log (`history.rs`) |
| `kashot-platform`  | OS shims: capture (xcap), hotkey (global-hotkey), tray, clipboard, recorder |
| `kashot-app`       | Tray-resident binary; winit event loop, themed dialogs, editor       |

```sh
cd kashot-rs
cargo test  -p kashot-core            # pure-logic tests, no system deps
cargo test  --workspace --release     # full tests on Linux/macOS/Windows
cargo build --release --bin kashot    # ~7 MB stripped binary
```

Linux build deps (CI installs these — see `.github/workflows/build-rust.yml`):
`libwayland-dev libxkbcommon-dev libxcb*-dev libgtk-3-dev libdbus-1-dev libayatana-appindicator3-dev libpipewire-0.3-dev libgbm-dev libxdo-dev libssl-dev pkg-config`.

Windows + macOS need no extra system packages.

CI: tagged push to `v*` triggers `build-rust.yml` which produces and auto-attaches to the GitHub Release:
- `kashot-linux-x86_64.tar.gz`
- `kashot-linux-arm64.tar.gz`
- `kashot-x86_64.AppImage` (repackaged from the x86_64 tarball)
- `kashot-windows-x86_64.zip`
- `Kashot.msi`
- `Kashot-macos-arm64.dmg` / `Kashot-macos-x64.dmg` — the macOS artifacts users download
- `kashot-macos-arm64-update.tar.gz` / `kashot-macos-x64-update.tar.gz` — bare `kashot` binary, payload for the in-app updater only
- `SHA256SUMS`

macOS deliberately ships no bare Mach-O asset: a binary downloaded through a
browser is quarantined and can't be launched from Finder, so the `.dmg` is the
only human-facing macOS download. The `.app` bundle inside it is assembled by
`dist/macos/make-app.sh` (Info.plist template alongside it), which CI calls —
so a local run reproduces the shipped bundle exactly.

## Architecture

Tray-resident screenshot tool with an annotation editor. The `kashot-app` binary boots a `winit` event loop that owns a tray-icon, up to three global hotkeys, and a per-purpose framebuffer window for each surface (overlay editor, settings, about, updates, convert-image, convert-video, pinned image, recording indicator).

### File map (`kashot-rs/crates/kashot-app/src/`)

| File | Role |
|---|---|
| `main.rs` | Entry. Boots `TrayLoop`, registers the global hotkeys, runs winit event loop. |
| `tray_loop.rs` | Owns tray menu state, hotkey routing, lifetime of every window/dialog. The orchestrator. |
| `editor.rs` | Capture surface + annotation editor. State machine: Idle / Selecting / Selected / Drawing / TextInput / Resizing / MovingAnnotation. |
| `painter.rs` | tiny-skia + softbuffer wrapper. The shared rendering layer every dialog uses. |
| `settings_form.rs` | Themed Settings dialog (paths, watermark, appearance, marker opacity). A live REBIND widget per global hotkey, plus an Edit-as-JSON button as an escape hatch. |
| `about_form.rs` | Themed About dialog. |
| `updates_form.rs` | Themed Update-check dialog. Background `curl` to `api.github.com/repos/singhpratech/kashot/releases/latest`. |
| `convert_image_form.rs` | PNG ↔ JPG / BMP / WEBP (the `image` crate must have `webp` feature for the last one). |
| `convert_video_form.rs` | MP4 → MOV / WEBM / MKV / GIF. Spawns bundled ffmpeg. |
| `recording_indicator.rs` | 220×56 floating window with flashing REC dot, MM:SS timer, STOP button. |
| `pin.rs` | Pinned-to-screen image window (drag-to-move). |
| `brand_icon.rs` | Shared brand-PNG decoded once into a `winit::Icon`. |
| `build.rs` | Copies an `ffmpeg` binary next to the kashot release binary if `KASHOT_FFMPEG` is set or one is on PATH; otherwise emits a warning. |

### Cross-cutting

- **Global hotkeys**: three independent bindings — region capture (bound by default), full-screen capture, and a record start/stop toggle (both optional, unset out of the box). `kashot-core::hotkeys` owns the action enum, the settings mapping and conflict detection; `kashot-platform::hotkey` registers the set and reports which action fired. Each binding is stored in `settings.json` as a Win32 modifier mask + virtual-key pair, with a virtual key of `0` meaning "not bound" — so a file written before the extra actions existed loads unchanged.
- **Settings** persist to `ProjectDirs::from("org", "kashot", "Kashot").config_dir()` (`~/.config/kashot/settings.json` on Linux).
- **High-DPI** — `kashot-core/src/dpi.rs` (`DisplayMap`) is the single logical↔physical mapping. A monitor's reported rect and the bitmap it captures are in different units on different platforms (macOS reports points and captures device pixels, X11 divides rects by `Xft.dpi/96`, Windows reports both in device pixels), so the scale is *measured* per monitor — captured size ÷ reported size — rather than taken from the OS factor, and the stitched capture is built at the sharpest monitor's scale. The overlay, the crop, pin placement and any future recording region all convert through that one map; at 1× every conversion is the identity. `DisplayMap::grab_region` hands a region to a recorder in both conventions (device pixels for x11grab/gdigrab, points for `screencapture -R`). The overlay's own chrome is still drawn in device pixels, so it renders physically smaller on a scaled display — scaling the hand-rolled UI is a separate change.
- **Theme colors** — each dialog currently re-declares its laser-green palette as private constants. Promoting to a shared `kashot-core/src/theme.rs` is a deferred cleanup item ([[feedback-release-gate]] fact-check, claim 13).
- **Multi-monitor**: `capture_all_screens` stitches every monitor into one bitmap and reports the virtual-desktop origin — the union's top-left, which is *negative* when a monitor sits left of or above the primary one. The overlay editor opens as a single borderless always-on-top window covering that union (winit has no all-monitors fullscreen mode, and one window per monitor would break a selection drag that crosses the seam), and maps the selection back through the origin for the crop, the pin window's position and the magnifier. `kashot-core/src/virtual_desktop.rs` owns that arithmetic: frame (framebuffer) space, bitmap space, virtual-screen space, plus the per-monitor bounds the floating tool panels lay themselves out inside. If a window manager refuses the requested placement the overlay re-derives its origin from where the window actually landed instead of cropping the wrong pixels.
- **Region recording**: tray → "Record region", or the Record button in the capture editor's action row, opens the overlay as a selection-only picker (Enter / Record confirms, Esc cancels) and starts the recorder limited to that rectangle. `kashot_core::region` clamps the selection to the virtual desktop and rounds it to the even dimensions H.264 needs; the rectangle rides on `Recorder::start_region`, so every backend honours it — `x11grab` via `-video_size` + `:0+X,Y`, `gdigrab` via `-offset_x`/`-offset_y`/`-video_size`, `screencapture -v` via `-R`, and the macOS audio path via an ffmpeg `crop` filter. Audio wiring is byte-identical to a full-screen recording. The REC indicator is placed outside the recorded rectangle, which is also what lets it appear at all on X11 (no capture-exclusion API there).
- **Recording**: Linux X11 via `ffmpeg -f x11grab` (PulseAudio mic + default-sink monitor source — `pactl` must be on PATH or audio is silently dropped, which is why the snap stages `pulseaudio-utils`); Windows native via `ffmpeg -f gdigrab` + `-f dshow` mic, system audio via WASAPI loopback piped to ffmpeg over TCP; macOS via built-in `screencapture -v` for the video-only case, switching to `ffmpeg -f avfoundation` when audio is requested, with system audio from a ScreenCaptureKit session over the same TCP path. **Wayland (Linux) capture is still queued** (`recorder.rs`). Audio is best-effort on Linux and for the macOS microphone — a source that won't open is dropped and the recording starts without it; on Windows, and for macOS system audio, a source that can't be opened fails the whole recording with an actionable error instead. Either way `Recorder::start` returns the *effective* options, so toasts must be rendered from its return value rather than from what was asked for.
- **Single instance**: `kashot-platform::instance` holds an advisory lock on
  `<config dir>/instance.lock` for the process lifetime — `flock(LOCK_EX|LOCK_NB)`
  on Unix, an exclusive `CreateFileW` share mode on Windows. A second launch
  drops a `capture.request` file next to it and exits; the running instance
  claims that file from its poll loop and opens the capture overlay. The
  self-updater's relaunch is the one legitimate overlap and waits out the
  handover (`RELAUNCH_ENV`).
- **Settings writes are atomic** — `kashot-core::atomic_file::write_atomic`
  (temp file in the same directory, fsync, rename). A crash mid-write leaves
  the previous settings intact instead of a truncated JSON that `load()` can
  only answer with `Default`.
- **The recorder child dies with the app**: `kashot-platform::child_guard`
  applies `PR_SET_PDEATHSIG` (Linux, via `pre_exec`), a Job Object with
  `KILL_ON_JOB_CLOSE` (Windows) or a detached `/bin/sh` pid watchdog (macOS),
  and records the live encoder's pid in `<config dir>/recorder.pid`. Startup
  reaps a record left by a previous run, but only after confirming the pid
  still runs a known recorder image — never a blind kill of a recycled pid.
- **User-visible failures**: save / copy / pin / settings-write failures are
  toasted through `tray_loop::notify`, with the wording built in
  `kashot-core::failure` so each message names the path and the OS reason.

## Keyboard shortcuts

Once a region is selected, single-letter keys switch tools:

| Key | Tool |
|---|---|
| P | Pen |
| L | Line |
| A | Arrow |
| R | Rectangle |
| E | Ellipse |
| M | Marker |
| T | Text |
| N | Numbered step |
| B | Blur / pixelate |
| S | Select mode (pick / move / delete existing annotations) |

Plus:
- `Esc` — cancel text input, cancel the active draw, drop the select-mode
  highlight, leave select mode; with annotations on the canvas it then asks
  before discarding anything (a second `Esc` or `Enter` confirms, any other
  key keeps editing)
- `Delete` / `Backspace` — in select mode, remove the selected annotation
- `Ctrl+Z` — undo (add, move, delete, and a confirmed Esc discard)
- `Ctrl+Y` or `Ctrl+Shift+Z` — redo
- `Ctrl+C` — copy final image to clipboard
- `Ctrl+S` — save final image via file picker
- `Ctrl+P` — pin the final image to the screen
- Drag selection edges/corners — resize
- Drag an annotation in select mode — move it (one undo step per drag)

### Select mode

`S` toggles it. Clicks stop starting new strokes and instead hit-test the
annotations already on the canvas, picking the topmost one under the cursor;
the pick gets a dashed laser-green box and can be dragged to a new position
or removed with `Delete` / `Backspace`. Any tool key (or a click on a tool
button) leaves select mode, so drawing is never intercepted. Hit-testing,
the move transform and the undo log live in `kashot-core`
(`edit.rs`, `history.rs`) and are unit-tested per annotation kind.

Rectangles and ellipses are hit-tested on their outline only — they are
hollow shapes, so a click in the middle of a big frame reaches whatever is
underneath it. Pen, marker, line and arrow test against their stroke width;
text, step markers and blur test against their painted box or disc.
