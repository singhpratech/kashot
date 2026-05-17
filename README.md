<div align="center">

<img src="docs/assets/kashot-256.png" width="140" height="140" alt="KAShot" />

# KAShot

### The lightweight screenshot tool every platform deserves.

**Drag a region. Annotate. Save, copy, pin, record — get back to work.**

[**kashot.org**](https://kashot.org)
&nbsp;·&nbsp; [Download](https://kashot.org/#download)
&nbsp;·&nbsp; [Roadmap](PLAN.md)
&nbsp;·&nbsp; [Architecture](CLAUDE.md)

[![License](https://img.shields.io/badge/license-Apache--2.0-22c55e.svg)](LICENSE)
![Platform](https://img.shields.io/badge/platforms-Windows%20%7C%20Linux%20%7C%20macOS-0ea5e9)
![Binary](https://img.shields.io/badge/binary-~7MB%20stripped-eab308)
![No telemetry](https://img.shields.io/badge/telemetry-none-ec4899)
[![Build](https://github.com/singhpratech/kashot/actions/workflows/build-rust.yml/badge.svg)](https://github.com/singhpratech/kashot/actions)

</div>

---

## What is KAShot

A tray-resident screenshot + screen-recording tool that ships **the same binary discipline** to Windows, Linux, and macOS. No Electron, no Wine, no bundled browser, no accounts, no telemetry. Hit your hotkey → drag → annotate → save / copy / pin / record → done.

<div align="center">

| Capture | Annotate | Save / Share |
|:---:|:---:|:---:|
| 🎯 Pixel-accurate region select with 7× live magnifier | ✏️ 9 tools, 4 palettes, 16 swatches each | 💾 PNG · JPG · BMP · clipboard · pin-to-screen |
| 🖥️ Spans every monitor, single virtual desktop | ⌨️ Single-key shortcuts for every tool | 🎬 Screen recording → MP4 with mic + system audio |
| ⏱️ Capture-in-3s / 5s / 10s with tray countdown | 🔢 Numbered steps, blur / pixelate, free-text | 🔄 Convert image · MP4 → MOV/WEBM/MKV/GIF |

</div>

---

## The story

Three machines. Three different screenshot tools. Three different sets of habits.

On **Windows**, Snipping Tool is fine but its annotation story is "scribble in MS Paint." Greenshot feels frozen in 2014. ShareX has a thousand options I'd never use. On **Linux**, every native option — Flameshot, Shutter, ksnip, GNOME Screenshot — is either heavyweight, opinionated, or missing the basics. On **macOS**, the built-in capture is genuinely good, but nothing else matches it on the other two operating systems I use every day.

I wanted **one** screenshot tool. Same hotkey, same overlay, same shortcuts, same JSON settings — on every machine I touch.

So I started building. The first cut was a Windows-only C# / WinForms app. Once the workflow felt right I ported it to **Rust** so Linux and macOS could have the same thing natively. No Electron. No Wine. No bundled browser. No accounts. No telemetry.

That's KAShot. Lightweight. One workflow. Everywhere.

— [PrateekSingh](https://github.com/singhpratech)

---

## Install

<table>
<tr>
<th width="33%" align="center">

<img src="docs/assets/kashot-64.png" width="40" alt="Win"/><br>**Windows**

</th>
<th width="33%" align="center">

<img src="docs/assets/kashot-64.png" width="40" alt="Linux"/><br>**Linux**

</th>
<th width="33%" align="center">

<img src="docs/assets/kashot-64.png" width="40" alt="macOS"/><br>**macOS**

</th>
</tr>
<tr>
<td valign="top">

[**Download .zip**](https://github.com/singhpratech/kashot/releases/latest/download/kashot-windows-x86_64.zip)

Unzip → run `kashot.exe`. Same Rust binary that ships on Linux and macOS.

```powershell
# coming soon
winget install singhpratech.Kashot
choco  install kashot
scoop  install kashot
```

The legacy WiX MSI (`Kashot.msi`) is still available as a CI artifact on the `Build C# (Windows, legacy)` workflow run for anyone who needs an installer-style package.

</td>
<td valign="top">

```bash
curl -fsSL https://kashot.org/install.sh | sh
```

One-liner auto-detects **x86_64** and **arm64**, downloads the matching tarball, drops the binary in `~/.local/bin`. Or fetch the tarball directly:

```bash
# x86_64
curl -L https://github.com/singhpratech/kashot/releases/latest/download/kashot-linux-x86_64.tar.gz | tar -xz
# arm64 (Raspberry Pi 4/5, Ampere, Graviton, Asahi)
curl -L https://github.com/singhpratech/kashot/releases/latest/download/kashot-linux-arm64.tar.gz | tar -xz
./kashot/kashot
```

```bash
# coming soon
flatpak install flathub org.kashot.Kashot
yay -S kashot         # AUR
```

</td>
<td valign="top">

[**Apple Silicon**](https://github.com/singhpratech/kashot/releases/latest/download/Kashot-macos-arm64) &nbsp;·&nbsp;
[Intel](https://github.com/singhpratech/kashot/releases/latest/download/Kashot-macos-x64)

```bash
chmod +x Kashot-macos-arm64
./Kashot-macos-arm64
```

```bash
# coming soon
brew install --cask kashot
```

</td>
</tr>
</table>

---

## What you get out of the box

<div align="center">

|  | Feature |
|:---:|:---|
| 🎯 | **Pixel-accurate region select** — 7× live magnifier, drag any edge to resize, `Alt`+drag to move |
| ✏️ | **9 annotation tools** — pen, line, arrow, rectangle, ellipse, marker, text, numbered steps, blur / pixelate |
| 🎨 | **4 palettes × 16 swatches** — Vivid · Highlighter · Pastel · Pro, plus a custom color picker |
| 📌 | **Pin to screen** — borderless top-most window, drag anywhere on the desktop |
| 🎬 | **Screen recording** — MP4 with optional mic + system audio, floating STOP control (Linux X11 today; Windows + macOS recording on the roadmap) |
| 🔄 | **Format conversion** — PNG ↔ JPG / WEBP / BMP · MP4 → MOV / WEBM / MKV / GIF |
| 🏷️ | **Watermark** — editable text, 4 anchors, 0–100 % opacity slider |
| ⌨️ | **Global hotkey** — defaults to `PrintScreen`; remappable via settings |
| ⏱️ | **Delayed capture** — 3 s / 5 s / 10 s countdown with tray indicator |
| 🖥️ | **Multi-monitor** — single virtual-desktop capture, no per-screen switching |
| 🌗 | **Themed dialogs** — Settings · About · Updates · Convert — same laser-green skin everywhere |
| 🔒 | **No accounts. No telemetry. No upsell.** Free, open source (Apache-2.0) |

</div>

---

## Keyboard shortcuts

Once a region is selected:

<table>
<tr><th colspan="2">Tools</th><th colspan="2">Actions</th></tr>
<tr>
<td><kbd>P</kbd> Pen</td><td><kbd>M</kbd> Marker</td>
<td><kbd>Ctrl</kbd>+<kbd>Z</kbd> Undo</td><td><kbd>Ctrl</kbd>+<kbd>C</kbd> Copy</td>
</tr>
<tr>
<td><kbd>L</kbd> Line</td><td><kbd>T</kbd> Text</td>
<td><kbd>Ctrl</kbd>+<kbd>Y</kbd> Redo</td><td><kbd>Ctrl</kbd>+<kbd>S</kbd> Save</td>
</tr>
<tr>
<td><kbd>A</kbd> Arrow</td><td><kbd>N</kbd> Numbered step</td>
<td><kbd>Esc</kbd> Cancel / close</td><td><kbd>Alt</kbd>+drag Move</td>
</tr>
<tr>
<td><kbd>R</kbd> Rectangle</td><td><kbd>B</kbd> Blur / pixelate</td>
<td>Drag edges Resize</td><td></td>
</tr>
<tr>
<td><kbd>E</kbd> Ellipse</td><td></td>
<td></td><td></td>
</tr>
</table>

---

## Build from source

Two implementations live side-by-side in this repo:

```text
Kashot/        C# / .NET 8 / WinForms — Windows-only, the v0.1 reference build
kashot-rs/     Rust workspace — cross-platform, ships the same UX natively
```

### Windows (C#)
```powershell
dotnet publish Kashot/Kashot.csproj -c Release
./Installer/build.ps1     # → Kashot.msi + Kashot.exe + Kashot-portable.zip
```

### Cross-platform (Rust)
```sh
cd kashot-rs
cargo test  -p kashot-core               # 9 tests, no system deps
cargo build --release --bin kashot       # 7 MB stripped binary
./target/release/kashot
```

Linux build deps:
```sh
sudo apt install libwayland-dev libxkbcommon-dev libxcb*-dev \
                 libgtk-3-dev libdbus-1-dev libayatana-appindicator3-dev \
                 libxdo-dev pkg-config ffmpeg
```

macOS + Windows need no extra system packages — winit, softbuffer, tray-icon, and global-hotkey use Cocoa / Win32 directly.

### Bundling ffmpeg for shipping
`kashot-rs/crates/kashot-app/build.rs` copies an ffmpeg binary next to the kashot executable so the Convert-video dialog works without a system install. For release builds, point it at a static binary:
```sh
KASHOT_FFMPEG=/path/to/static/ffmpeg cargo build --release --bin kashot
```

Full architecture notes in [`CLAUDE.md`](CLAUDE.md).

---

## Project layout

```text
Kashot/                C# / WinForms reference build (Windows)
kashot-rs/             Rust workspace (cross-platform port)
  crates/kashot-core      Tool · Annotation · AppSettings · ThemeColors — pure logic
  crates/kashot-platform  capture · hotkey · tray · recorder · clipboard
  crates/kashot-app       tray-resident binary + overlay editor + themed dialogs
docs/                  kashot.org landing page (GitHub Pages)
dist/                  package-channel metadata: winget, choco, scoop, brew, flatpak, AUR, deb
icons/                 branded icon pack (every platform size, one source PNG)
.github/workflows/     CI: matrix tests + multi-platform release builds
```

---

## Status

| Surface | Windows | Linux | macOS |
|---|:---:|:---:|:---:|
| Tray + global hotkey | ✅ | ✅ | ✅ |
| Capture + 9-tool overlay editor | ✅ | ✅ | ✅ |
| Save · Copy · Pin · Watermark | ✅ | ✅ | ✅ |
| Screen recording (MP4 + audio) | ⏳ | ✅ | ⏳ |
| Themed Settings · About · Updates | ✅ | ✅ | ✅ |
| Image + video format conversion | ✅ | ✅ | ✅ |
| Release artifact | `.zip` | `.tar.gz` | raw binary (`.dmg` planned) |

**One Rust binary, three platforms.** Same source, same editor, same feature set — the `kashot-rs/` workspace is the canonical build on Windows, Linux, and macOS. The original C# WinForms version (`Kashot/`) is retained as a reference implementation and PR compile-check; it no longer attaches to releases. Both stay aligned on settings JSON shape and hotkey wire format — see [`PLAN.md`](PLAN.md) § "Architecture invariants".

---

## License

Licensed under the **Apache License, Version 2.0** — see [`LICENSE`](LICENSE).

`SPDX-License-Identifier: Apache-2.0`.

Contributions are accepted under the same Apache-2.0 terms (Apache-2.0 §5).

## Credits

Built by [Prateek Singh](https://github.com/singhpratech). Bug reports and PRs welcome at [github.com/singhpratech/kashot](https://github.com/singhpratech/kashot).

---

<div align="center">

<img src="docs/assets/kashot-64.png" width="48" alt="KAShot" />

[**kashot.org**](https://kashot.org)

</div>
