# Kashot — Product Roadmap

**Status legend:** ✅ shipped · 🔨 in progress (code partially landed) · 📋 backlog · ⏳ long-term · ❄ frozen

The project lives in a single Rust workspace under `kashot-rs/`
(`kashot-core`, `kashot-platform`, `kashot-app`), targeting Windows +
Linux + macOS from one codebase. **Canonical build as of v0.3.0** —
ships every release artifact on all three platforms.

The original C# / .NET 8 / WinForms build (`Kashot/`, `Installer/`,
`build-csharp.yml`) was retired in v0.3.0 once the Rust port covered
all three platforms. Git history retains it if you need to look back.

---

## ✅ Shipped (on `main`)

> The first four subsections below describe the v0.1 Windows-only C# /
> WinForms build, which **was retired in v0.3.0**. The same features
> (capture / annotation / output / settings) all ship today in the Rust
> port — see **Cross-platform foundation (Rust)** further down. The C#
> sections are kept as a historical reference of what each shipped surface
> covered before the port.

### Capture core (v0.1 — C#, retired)
- Tray-resident Windows app, single-instance mutex, registers global hotkey
- Multi-monitor virtual-screen capture
- Selection rectangle with magnifier + crosshair
- Selection resize (drag edges/corners) + move (Alt+drag)

### Annotation editor (v0.1 — C#, retired)
- Tools: pen, line, arrow, rectangle, ellipse, marker, text, numbered step, blur/pixelate
- Tool keyboard shortcuts: P / L / A / R / E / M / T / N / B
- 4 color palettes (Vivid / Highlighter / Pastel / Pro), 16 colors each
- Custom color picker, thickness cycle (1 / 2 / 3 / 5 / 8 px)
- Undo (Ctrl+Z) / Redo (Ctrl+Y)

### Output (v0.1 — C#, retired)
- Save to PNG / JPEG / BMP
- Copy to clipboard
- Pin to screen (always-on-top draggable window)
- Watermark on saved/copied/pinned images (default text "PrateekSingh", toggleable)

### Settings + lifecycle
- Settings dialog: rebindable hotkey, default save folder, start-with-Windows, watermark text/toggle, theme selector
- Light / Dark theme for Settings + About (overlay stays dark)
- About dialog with attribution + GitHub link
- Settings persisted to `%APPDATA%/Kashot/settings.json`
- **GDI handle leaks fixed** — arrow-cap, button image, text font no longer
  leak across long tray sessions
- **Save / clipboard failures show a balloon** instead of crashing the overlay

### Distribution (v0.1 — C# Windows, retired)
- Self-contained single-file `Kashot.exe` (~68 MB) for win-x64
- WiX 7 MSI installer (`Kashot.msi`, ~62 MB) with Start Menu + Desktop shortcuts
- `Kashot-portable.zip` — extract-and-run, no install
- All three artifacts produced by `Installer/build.ps1` in one run

### Cross-platform foundation (Rust) — `kashot-rs/`
- **`kashot-rs/` workspace** — `kashot-core` (logic, **17 unit tests pass on
  Linux + Windows + macOS**), `kashot-platform` (capture / hotkey / tray /
  clipboard / recorder via xcap, global-hotkey, tray-icon, arboard, ffmpeg
  / screencapture shell-out), `kashot-app` (winit + softbuffer overlay,
  Pin window, tray-resident main loop)
- Wire-compatible with C# `settings.json` — same PascalCase keys, same
  Win32-VK hotkey encoding, same ARGB color ints
- **All 9 annotation tools shipped** with hand-designed 5×7 ASCII bitmap
  font for text rendering, no TTF rasterizer dep
- **Floating panels** (tool column + action row) hugging the selection,
  matching `OverlayForm.PositionToolbars` 1:1; auto-flip when they'd clip
- **X11 override-redirect** + side-channel `XSetInputFocus` so the overlay
  layers above DOCK panels (Cinnamon / Plasma / GNOME) AND keeps keyboard
  focus for the Text tool — needed because plain `_NET_WM_STATE_ABOVE`
  sits at the same stratum as panels
- **Tray menu** parity with C# TrayContext: Capture / Capture-after-delay
  (3 / 5 / 10 s + Cancel pending) / Record Screen (mic-audio when
  PulseAudio is reachable) / Stop Recording / Settings / About / Exit
- **About modal** matches `AboutForm.cs` text 1:1 ("With love from
  PrateekSingh ❤" + copyright)
- **Watermark** (`WatermarkEnabled` / `WatermarkText`) applied on every
  save / copy / pin path — bottom-right corner, white-on-black-shadow so
  it stays legible on either light or dark screenshots
- **Hover tooltips** on every tool / utility / action button
- **Magnifier zoom** in Idle / Selecting state — 7× lens with red
  crosshair so the user can place selection edges by individual pixels
- **Edge-resize**, **Color palette popup** (4 palettes × 16 swatches),
  **Thickness cycle**, **Pin window** (always-on-top, drag-to-move),
  **Undo/Redo stack**, **Save / Copy / Pin / Close** action buttons,
  **Dimension chip** with real "W × H" text
- All Linux + macOS (arm64 + x64) + Windows-Rust release builds **green
  in CI** and downloadable from the GitHub Release on tag push

### Brand + asset pack
- Brand-stamped "KA" + camera + viewfinder icon, master at 1024×1024
- Multi-resolution `.ico` packed at 16/24/32/48/64/128/256
- `icons/` — full multi-platform asset pack: Windows .ico + PNG set, macOS
  iconset + monochrome menubar template, Linux freedesktop hicolor, iOS,
  Android (mipmap + Play Store), web/PWA favicons

### Online presence
- **Landing page** at `docs/` (served as `kashot.org` via GitHub Pages
  CNAME). Hero with the brand icon, three-platform download cards,
  features, keyboard shortcut reference. ~76 KB total, no framework.
- **CI workflows** (`.github/workflows/`):
  - `build-rust.yml` — matrix tests `kashot-core` on Ubuntu / Windows /
    macOS, release builds on each (Linux x86_64 + arm64, Windows x86_64,
    macOS arm64 + x64), wraps the Linux tarball into an AppImage, and
    attaches every artifact to the GitHub Release on tag push
  - `codeql.yml` — CodeQL static analysis over the workflow YAML
    (Rust CodeQL support is in preview; C# was dropped with the legacy
    WinForms build in v0.3.0)

---

## 🔨 In progress (code partially landed, needs wiring or polish)

| # | Feature | Code state | Remaining |
|---|---|---|---|
| R1 | Brand-stamped "KA" icon | Drawn-bow + camera + viewfinder vector landed, multi-resolution `.ico` built | Replace with image-model-generated master once delivered |
| R2 | OCR (text from image) | `Kashot/OcrService.cs` ✅ — Tesseract 5.2.0 wrapper, lazy-downloads `eng.traineddata` to AppData on first use | Tray menu item "Extract text…" + result popup with copy-to-clipboard |
| R3 | Screen recording | ✅ **shipped** in Rust (v0.3.0) on Linux X11 (`ffmpeg -f x11grab` + PulseAudio mic + monitor source), Windows (`ffmpeg -f gdigrab` + DirectShow mic — system-audio loopback queued), and macOS (built-in `screencapture -v`; AVFoundation audio queued). Floating REC indicator with STOP. | Wayland capture via `xdg-desktop-portal` (ashpd); WASAPI loopback on Windows; AVFoundation on macOS for mic + system. |
| R4 | Meme text annotation | `MemeAnnotation` in `Annotations.cs` ✅ — Impact-style font, white fill + black outline, uppercase auto | Wire `Tool.Meme` into `OverlayForm`: keyboard shortcut **K**, toolbar entry, `StartTextInput(meme: true)` path |
| R5 | PDF export | `PdfSharp` 6.2.4 added | `*.pdf` filter in `SaveToFile` dialog + single-page export path |
| R6 | Image resize presets | (planned) | New `Resize…` action button → popup with 100 / 75 / 50 / 25 % + Max-1920 / Max-1280 / Max-640 + custom W×H. Apply to `GetFinalImage` output before save/copy/pin |
| R7 | Rust editor port | ✅ **shipped** (PR #1 + PR #2 + PR #3) — winit + softbuffer overlay, all 9 annotation tools, region selector, edge resize, color palette popup with 4 palettes, thickness cycle, undo/redo, Save/Copy/Pin action panel, Pin window, magnifier, tooltips, dimension chip, watermark, mic-audio recording on Linux | — |
| R8 | Native Settings dialog (Rust) | ✅ **shipped** — themed dialog with save-folder picker, watermark text + opacity, marker opacity slider, and a live hotkey **REBIND** widget (PR #14). Edit-as-JSON button kept as an escape hatch. | Theme combo (when the shared `kashot-core::theme` extraction lands). |
| R9 | Video format conversion / export | ✅ **shipped** — `convert_video_form.rs` tray action: pick source MP4, choose target (MOV / WebM / MKV / GIF), shell to bundled ffmpeg, drop result next to source. Companion `convert_image_form.rs` handles PNG ↔ JPG / BMP / WEBP. | — |
| R10 | Bundled Wayland support | Linux X11 only via `xcap` + `x11rb`; Wayland sessions fall back to a portal-based capture (queued) | Wire `xdg-desktop-portal` Screenshot + ScreenCast portals through `ashpd` so kashot works on GNOME-on-Wayland / KDE-on-Wayland out of the box |

---

## 📋 Backlog (next rounds, in priority order)

### Editor / capture
- **Burst capture mode** — hotkey-tap captures N frames at intervals → produces a sequence ready for GIF export
- **Animated GIF export** — from burst captures or from a region of a recorded MP4
- **Image gallery / history** — auto-save every capture to `%APPDATA%/Kashot/history/`, browseable in a thumbnail grid form
- **Region selector for recording** — pick rectangle to record, not just full screen
- **Crop after capture** — re-crop the selection without redoing
- **Smart-shape recognition** — pen drawn near rectangle/circle snaps to clean shape
- **Auto-redact** — detect faces / IDs / credit-card patterns and offer one-click blur

### Output / sharing
- **Cloud upload** — imgur, custom S3, or self-hosted endpoint, returns short URL to clipboard
- **Print** — direct to default printer
- **Share sheet** — Win 10/11 native share contract for Mail / Teams / etc.
- **Multi-format batch export** — same capture saved to PNG + PDF + JPEG simultaneously

### App polish
- **Code-sign** the `Kashot.exe` and `Kashot.msi` (removes SmartScreen warning)
- **Auto-update** — check GitHub releases on launch, prompt to download new version
- **Custom hotkey per action** — separate hotkeys for Capture, Record, Pin-last, OCR-last
- **i18n / localization** — start with EN, allow community PRs for other languages
- **Per-monitor DPI tuning** — verify cleanup-pass on 4K + scaling combinations

### Distribution channels (Notepad++-style)
- **Windows**: winget (`winget install singhpratech.Kashot`), Chocolatey, Scoop, Microsoft Store, MSI installer (in flight)
- **Linux**: AppImage (✅ built in CI; bundle is uploaded on every tag), Flatpak, `.deb` (Debian/Ubuntu), `.rpm` (Fedora/RHEL — spec lives in `dist/rpm/`, COPR submission pending), Snap (`dist/snap/snapcraft.yaml` buildable, store upload pending), AUR
- **macOS**: Homebrew Cask (`brew install --cask kashot`), notarized signed `.dmg` (in flight)

---

## ⏳ Long-term

- **Browser extension companion** — paste captures from Kashot directly into Gmail compose, Slack, etc.
- **Mobile capture-receiver** — phone takes a photo, beams to PC's Kashot for annotation
- **Webcam overlay during recording** — picture-in-picture circle, draggable
- **Video editor** — trim, crop, re-encode recorded MP4s; export GIF clip from video range
- **AI-assisted features** — caption generation for screenshots, auto-summarize recorded screens, intelligent redaction

---

## ❄ Frozen / decided against (don't propose again)

- **Lightshot-style cloud upload to a Kashot-hosted server** — server-hosted infra is out of scope for an open-source tool; user-provided endpoint only
- **Skeuomorphic or 3D icon** — kept the flat-with-light-depth iOS style instead

---

## Cross-platform parity — same app on Win / Linux / macOS

The Rust port targets **identical behavior on all three platforms**.
Same features, same shortcuts, same UX, same brand. Platform-specific code is
isolated to `kashot-platform`; everything in `kashot-core` and the iced UI
layer is shared.

- **Tests run on every platform.** `build-rust.yml` matrix
  `[ ubuntu-latest, windows-latest, macos-latest ]` runs
  `cargo test -p kashot-core` on each one. A test that passes on Linux must
  pass on Windows and macOS — anything else is a parity bug.
- **Each build job also runs `cargo test --workspace --release`** so
  platform-specific code is exercised on its own OS.
- **Settings JSON is wire-compatible.** Copy from a Windows machine to a
  macOS machine and it loads correctly.
- **Hotkey wire format is the same.** Win32 modifier mask + Win32 virtual-key
  on disk; Rust translates internally per platform.
- **Identical keyboard shortcuts**, including `Ctrl+Z` on macOS. Consistency
  across machines wins over Apple HIG conformance for a tool people use
  cross-platform.
- **`#[cfg(target_os = "...")]` outside `kashot-platform/` is a smell.**
  Push the difference down into the platform crate and expose a uniform API.

---

## Architecture invariants — keep these stable

These cross-cut both implementations; if you change one, change both.

- **Settings file path**:
  Windows `%APPDATA%\Kashot\settings.json`,
  Linux `~/.config/Kashot/settings.json`,
  macOS `~/Library/Application Support/Kashot/settings.json`.
- **Settings JSON keys** are PascalCase (`LastTool`, `HotkeyVirtualKey`,
  `WatermarkEnabled`, …). Don't rename them; both builds read the same file.
- **Hotkey wire format**: Win32 modifier mask (`MOD_*`) + Win32 virtual-key code.
- **Tools** (`Pen, Line, Arrow, Rectangle, Ellipse, Marker, Text, Step,
  Pixelate, Meme`) use single-letter shortcuts (`p l a r e m t n b k`).
  Both builds must agree.
- **Color palettes** (`Vivid, Highlighter, Pastel, Pro`, 16 swatches each) — the
  exact ARGB values are part of the brand. Don't tweak without before/after screenshots.
- **Brand**: project, namespace, assembly, binary, every user-visible string is
  `Kashot`. The outer parent folder being named differently on disk is for
  historical reasons only; that name doesn't appear anywhere in code.

---

## Build & ship procedure (per release)

1. Bump `kashot-rs/Cargo.toml` (`workspace.package.version`).
2. Run the **doc-freshness sweep** (see [`feedback-release-gate`] memory):
   - `docs/assets/og.svg` — bump the two `v0.X` strings (social preview card)
   - `docs/index.html` — `data-version` spans (auto-sync via `docs/app.js`
     but seed the first paint correctly)
   - `dist/*` package-channel manifests — bump every release URL + clear
     placeholder SHA256s if a channel is going live this round
   - This `PLAN.md` example tag in step 4 below
3. Open a PR with the bumps; merge to `main`.
4. Locally tag and push: `git tag vX.Y.Z && git push --tags`.
5. `build-rust.yml` fires on the tag and produces:
   `kashot-linux-x86_64.tar.gz` · `kashot-linux-arm64.tar.gz` ·
   `kashot-windows-x86_64.zip` · `Kashot-macos-arm64` · `Kashot-macos-x64`
   (canonical — attaches to Release automatically).
6. CI auto-attaches the artifacts to the GitHub Release with generated
   notes; edit if you want a curated changelog.
7. Confirm `https://kashot.org` download buttons resolve to the new files
   (the page resolves `releases/latest/download/<asset>` automatically via
   the GitHub API call in `docs/app.js`).

---

## How to contribute

1. Pick an item from "🔨 In progress" or "📋 Backlog"
2. Reference its ID (e.g., **R3**, or the section heading) in the PR title
3. PRs land on `main` after a green `cargo test --workspace --release` and a
   successful `cargo build --release --bin kashot` on all three CI platforms
   (Linux x86_64 + arm64, Windows x86_64, macOS arm64 + x64).

---

*Last updated: 2026-05-18 (v0.3.6 release — snap-only fix: `audio-playback` plug declared alongside `audio-record` to satisfy Snap Store policy). Maintained by [@singhpratech](https://github.com/singhpratech).*
