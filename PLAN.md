# Kashot — roadmap

The project lives in two implementations side-by-side:

- **`Kashot/`** — C# / .NET 8 / WinForms build. Windows-only. The reference
  implementation; this is what ships today.
- **`kashot-rs/`** — Rust workspace targeting Windows + Linux + macOS from one
  codebase. Foundation (tray + hotkey + capture + save) is in place; will
  catch up to feature parity over the v0.1.x line.

Both share **brand, settings JSON format, hotkey wire format, tool shortcuts,
and color palettes**. See § "Architecture invariants" for the cross-cutting
decisions that must stay aligned.

---

## v0.1 — current line

Everything below is already in the tree, awaiting a release tag (which won't
be cut until you say so — staying on `0.1.x` for now):

- C# build at `0.1.0` (csproj / WiX / About dialog all aligned)
- `Installer/build.ps1` produces three artifacts in one run:
  `Kashot.msi` · `Kashot.exe` · `Kashot-portable.zip`
- Landing page at `docs/` with three Windows download buttons
- GDI handle leaks fixed (arrow-cap, button image, text font) so long-running
  tray sessions don't burn through GDI handles
- Save / clipboard failures show a balloon instead of crashing the overlay
- Tray-tooltip truncation removed
- More legible toolbar icons (Pen, Marker, Pixelate, Pin)
- Tray menu cleaned: Record-Screen placeholder removed; Open-Save-Folder added
- CI: `build-csharp.yml` builds and uploads on tag push

Everything stays on the `0.1.x` line — no `0.2`, no `1.0` — until the
maintainer explicitly bumps. Future patch releases will be `0.1.1`, `0.1.2`,
etc.

---

## What's next (order, not commitments)

These are the work items in priority order. Versions assigned when each
one's actually being worked on, not before.

1. **Native Linux + macOS** via the Rust port catching up to feature parity.
   Foundation is in `kashot-rs/`; the missing piece is the overlay editor
   (state machine + 9 annotation tools + magnifier + handles + toolbars +
   color picker + undo/redo). Most of `kashot-core` is already shared logic.
2. **Distribution breadth** — winget, chocolatey, scoop on Windows;
   AppImage, Flatpak, .deb / .rpm, AUR on Linux; Homebrew Cask + notarized
   .dmg on macOS. Each channel is small once a release pipeline exists.
3. **Code-signing** — Comodo/DigiCert cert eliminates SmartScreen warnings.
   Big trust upgrade; worth doing once direct-download numbers justify it.
4. **Microsoft Store** — MSIX repackaging, $19 dev fee, certification.
   Highest-friction surface; do last.

## Non-goals

To keep scope tight and one person able to ship this:

- **Screen recording / video / GIF** — different problem space.
- **OCR** — the OS already does this (macOS Live Text, Windows Snipping Tool).
- **Cloud upload / accounts / sync** — no backend, no auth flow, no telemetry.
  The clipboard is the share target.

The "Record Screen" tray entry currently shows a coming-soon balloon. Given
the scope decision above, that menu item should be **removed** from
`TrayContext.cs` (and its Rust equivalent) before the next release, so we
don't ship a promise we won't keep.

---

## Cross-platform parity — same app on Win / Linux / macOS

The Rust port targets **identical behavior on all three platforms**.
Same features, same shortcuts, same UX, same brand. Platform-specific code is
isolated to `kashot-platform`; everything in `kashot-core` and the iced UI
layer (when it lands) is shared.

- **Tests run on every platform.** `build-rust.yml` matrix
  `[ ubuntu-latest, windows-latest, macos-latest ]` runs `cargo test -p kashot-core`
  on each one. A test that passes on Linux must pass on Windows and macOS.
- **Each build job also runs `cargo test --workspace --release`** so
  platform-specific code is exercised on its own OS.
- **Settings JSON is wire-compatible.** Copy from a Windows machine to a
  macOS machine and it loads correctly.
- **Hotkey wire format is the same.** Win32 modifier mask + Win32 virtual-key
  on disk; Rust translates internally per platform.
- **No platform-only features** — if a feature can't work on one platform,
  it doesn't ship until it works everywhere.
- **Identical keyboard shortcuts**, including `Ctrl+Z` on macOS. Consistency
  across machines wins over Apple HIG conformance for a tool people use
  cross-platform.
- **`#[cfg(target_os = "...")]` outside `kashot-platform/` is a smell.**
  Push the difference down into the platform crate and expose a uniform API.

## Architecture invariants — keep these stable

These cross-cut both implementations; if you change one, change both.

- **Settings file path**:
  Windows `%APPDATA%\Kashot\settings.json`,
  Linux `~/.config/Kashot/settings.json`,
  macOS `~/Library/Application Support/Kashot/settings.json`.
- **Settings JSON keys** are PascalCase (`LastTool`, `HotkeyVirtualKey`,
  `WatermarkEnabled`, …). Don't rename them; both builds read the same file.
- **Hotkey wire format**: Win32 modifier mask (`MOD_*`) + Win32 virtual-key code.
- **Tools** (`Pen, Line, Arrow, Rectangle, Ellipse, Marker, Text, Step, Pixelate`)
  use single-letter shortcuts (`p l a r e m t n b`). Both builds must agree.
- **Color palettes** (`Vivid, Highlighter, Pastel, Pro`, 16 swatches each) — the
  exact ARGB values are part of the brand. Don't tweak without before/after screenshots.
- **Brand**: project, namespace, assembly, binary, every user-visible string is
  `Kashot`. The outer parent folder being named differently on disk is for
  historical reasons only; that name doesn't appear anywhere in code.

---

## Build & ship procedure (per release)

1. Bump version in:
   - `Kashot/Kashot.csproj` (`<Version>`)
   - `Kashot/AboutForm.cs` (label text)
   - `Installer/Kashot.wxs` (`Version=`)
   - `kashot-rs/Cargo.toml` (`workspace.package.version`)
2. Open a PR with the bumps; merge to `main`.
3. Locally tag and push: `git tag v0.1.X && git push --tags`.
4. CI fires:
   - `build-csharp.yml` produces `Kashot.msi` · `Kashot.exe` · `Kashot-portable.zip`
   - `build-rust.yml` produces `kashot-linux-x86_64.tar.gz` · `Kashot-windows-x64.exe`
     · `Kashot-macos-arm64` · `Kashot-macos-x64`
5. Edit the auto-generated GitHub Release: changelog notes, mark "latest".
6. Confirm `https://kashot.org` download buttons resolve to the new files
   (the page resolves `releases/latest/download/<asset>` automatically).
