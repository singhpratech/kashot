# Kashot — Product Roadmap

**Status legend:** ✅ shipped · 🔨 in progress (code partially landed) · 📋 backlog · ⏳ long-term · ❄ frozen

---

## ✅ Shipped (commit `fcc9e35`, on `main`)

### Capture core
- Tray-resident Windows app, single-instance mutex, registers global hotkey
- Multi-monitor virtual-screen capture
- Selection rectangle with magnifier + crosshair
- Selection resize (drag edges/corners) + move (Alt+drag)

### Annotation editor
- Tools: pen, line, arrow, rectangle, ellipse, marker, text, numbered step, blur/pixelate
- Tool keyboard shortcuts: P / L / A / R / E / M / T / N / B
- 4 color palettes (Vivid / Highlighter / Pastel / Pro), 16 colors each, switchable from popup
- Custom color picker
- Thickness cycle (1 / 2 / 3 / 5 / 8 px)
- Undo (Ctrl+Z) / Redo (Ctrl+Y)

### Output
- Save to PNG / JPEG / BMP
- Copy to clipboard
- Pin to screen (always-on-top draggable window)
- Watermark on saved/copied/pinned images (default text "PrateekSingh", toggleable in settings)

### Settings + lifecycle
- Settings dialog: rebindable hotkey, default save folder, start-with-Windows, watermark text/toggle, theme selector
- Light / Dark theme for Settings + About (overlay stays dark)
- About dialog with attribution + GitHub link
- Settings persisted to `%APPDATA%/Kashot/settings.json`

### Distribution
- Self-contained single-file `Kashot.exe` (~68 MB) for win-x64
- WiX 7 MSI installer (`Kashot.msi`, ~62 MB) with Start Menu + Desktop shortcuts, ARP icon, GitHub help link

---

## 🔨 In progress (this round — partially landed, needs UI wiring or polish)

| # | Feature | Code state | Remaining |
|---|---|---|---|
| R1 | New bow-and-arrow icon | Drawn-bow vector landed, multi-resolution `.ico` built. Iterations ongoing on composition. | Replace with image-model-generated master once delivered (see `kashot-icon-master.png` placeholder) |
| R2 | OCR (text from image) | `Kashot/OcrService.cs` ✅ — Tesseract 5.2.0 wrapper, lazy-downloads `eng.traineddata` to AppData on first use | Add tray menu item "Extract text…" + result popup (with copy-to-clipboard) |
| R3 | Screen recording | `Kashot/ScreenRecorder.cs` ✅ — `KashotRecorder` wraps ScreenRecorderLib 6.6.0, MP4 H.264 + AAC, mic + system loopback toggles | Wire tray "Record Screen" toggle, save dialog for output path, hotkey support, optional pre-recording region selector |
| R4 | Meme text annotation | `MemeAnnotation` in `Annotations.cs` ✅ — Impact-style font, white fill + black outline, uppercase auto | Wire `Tool.Meme` into `OverlayForm`: keyboard shortcut **K**, toolbar entry, `StartTextInput(meme: true)` path |
| R5 | PDF export | `PdfSharp` 6.2.4 added | Add `*.pdf` filter to `SaveToFile` dialog and a single-page export path |
| R6 | Image resize presets | (planned) | New `Resize…` action button → popup with 100 / 75 / 50 / 25 % + Max-1920 / Max-1280 / Max-640 + custom W×H. Apply to `GetFinalImage` output before save/copy/pin |

---

## 📋 Backlog (next rounds, in priority order)

### Editor / capture
- **Burst capture mode** — hotkey-tap captures N frames at intervals → produces a sequence ready for GIF export
- **Animated GIF export** — from burst captures or from a region of a recorded MP4
- **Image gallery / history** — auto-save every capture to `%APPDATA%/Kashot/history/`, browseable in a thumbnail grid form (date-sorted, search by filename)
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

---

## ⏳ Long-term

- **macOS port** — likely SwiftUI rewrite; Kashot logo and brand carry over. See `Kashot/Kashot.ico` size spec for icon needs (`.icns`)
- **Linux port** — Avalonia or GTK; freedesktop hicolor icon set required
- **Browser extension companion** — paste captures from Kashot directly into Gmail compose, Slack, etc.
- **Mobile capture-receiver** — phone takes a photo, beams to PC's Kashot for annotation
- **Webcam overlay during recording** — picture-in-picture circle, draggable
- **Video editor** — trim, crop, re-encode recorded MP4s; export GIF clip from video range
- **AI-assisted features**: caption generation for screenshots, auto-summarize recorded screens, intelligent redaction

---

## ❄ Frozen / decided against (don't propose again)

- **Lightshot-style cloud upload to a Kashot-hosted server** — server-hosted infra is out of scope for an open-source tool; user-provided endpoint only
- **Skeuomorphic or 3D icon** — kept the flat-with-light-depth iOS style instead

---

## How to contribute

1. Pick an item from "🔨 In progress" or "📋 Backlog"
2. Reference its ID (e.g., **R3**, or the section heading) in the PR title
3. PRs land on `main` after a clean build of `Kashot/Kashot.csproj` and a successful MSI build via `Installer/build.ps1`

---

*Last updated: 2026-04-30. Maintained by [@singhpratech](https://github.com/singhpratech).*
