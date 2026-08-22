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
| `kashot-core`      | Pure logic: `Tool`, `Annotation`, `AppSettings`, theme, state machine |
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

Tray-resident screenshot tool with an annotation editor. The `kashot-app` binary boots a `winit` event loop that owns a tray-icon, a global hotkey, and a per-purpose framebuffer window for each surface (overlay editor, settings, about, updates, convert-image, convert-video, pinned image, recording indicator).

### File map (`kashot-rs/crates/kashot-app/src/`)

| File | Role |
|---|---|
| `main.rs` | Entry. Boots `TrayLoop`, registers global hotkey, runs winit event loop. |
| `tray_loop.rs` | Owns tray menu state, hotkey routing, lifetime of every window/dialog. The orchestrator. |
| `editor.rs` | Capture surface + annotation editor. State machine: Idle / Selecting / Selected / Drawing / TextInput / Resizing. |
| `painter.rs` | tiny-skia + softbuffer wrapper. The shared rendering layer every dialog uses. |
| `settings_form.rs` | Themed Settings dialog (paths, watermark, appearance, marker opacity). Live REBIND widget for the global hotkey, plus an Edit-as-JSON button as an escape hatch. |
| `about_form.rs` | Themed About dialog. |
| `updates_form.rs` | Themed Update-check dialog. Background `curl` to `api.github.com/repos/singhpratech/kashot/releases/latest`. |
| `convert_image_form.rs` | PNG ↔ JPG / BMP / WEBP (the `image` crate must have `webp` feature for the last one). |
| `convert_video_form.rs` | MP4 → MOV / WEBM / MKV / GIF. Spawns bundled ffmpeg. |
| `recording_indicator.rs` | 220×56 floating window with flashing REC dot, MM:SS timer, STOP button. |
| `pin.rs` | Pinned-to-screen image window (drag-to-move). |
| `brand_icon.rs` | Shared brand-PNG decoded once into a `winit::Icon`. |
| `build.rs` | Copies an `ffmpeg` binary next to the kashot release binary if `KASHOT_FFMPEG` is set or one is on PATH; otherwise emits a warning. |

### Cross-cutting

- **Settings** persist to `ProjectDirs::from("org", "kashot", "Kashot").config_dir()` (`~/.config/kashot/settings.json` on Linux).
- **High-DPI** — `kashot-core/src/dpi.rs` (`DisplayMap`) is the single logical↔physical mapping. A monitor's reported rect and the bitmap it captures are in different units on different platforms (macOS reports points and captures device pixels, X11 divides rects by `Xft.dpi/96`, Windows reports both in device pixels), so the scale is *measured* per monitor — captured size ÷ reported size — rather than taken from the OS factor, and the stitched capture is built at the sharpest monitor's scale. The overlay, the crop, pin placement and any future recording region all convert through that one map; at 1× every conversion is the identity. `DisplayMap::grab_region` hands a region to a recorder in both conventions (device pixels for x11grab/gdigrab, points for `screencapture -R`). The overlay's own chrome is still drawn in device pixels, so it renders physically smaller on a scaled display — scaling the hand-rolled UI is a separate change.
- **Theme colors** — each dialog currently re-declares its laser-green palette as private constants. Promoting to a shared `kashot-core/src/theme.rs` is a deferred cleanup item ([[feedback-release-gate]] fact-check, claim 13).
- **Recording**: Linux X11 via `ffmpeg -f x11grab` (PulseAudio mic + default-sink monitor source — `pactl` must be on PATH or audio is silently dropped, which is why the snap stages `pulseaudio-utils`); Windows native via `ffmpeg -f gdigrab` + `-f dshow` mic, system audio via WASAPI loopback piped to ffmpeg over TCP; macOS via built-in `screencapture -v` for the video-only case, switching to `ffmpeg -f avfoundation` when audio is requested, with system audio from a ScreenCaptureKit session over the same TCP path. **Wayland (Linux) capture is still queued** (`recorder.rs`). Audio is best-effort on Linux and for the macOS microphone — a source that won't open is dropped and the recording starts without it; on Windows, and for macOS system audio, a source that can't be opened fails the whole recording with an actionable error instead. Either way `Recorder::start` returns the *effective* options, so toasts must be rendered from its return value rather than from what was asked for.

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

Plus:
- `Esc` — cancel text input / cancel active draw / close overlay
- `Ctrl+Z` — undo
- `Ctrl+Y` or `Ctrl+Shift+Z` — redo
- `Ctrl+C` — copy final image to clipboard
- `Ctrl+S` — save final image via file picker
- Drag selection edges/corners — resize
