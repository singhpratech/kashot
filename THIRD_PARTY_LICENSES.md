# Third-party components

Kashot itself is licensed under **Apache-2.0** (see `LICENSE`). It bundles and
invokes the following third-party software.

## FFmpeg

Kashot bundles a prebuilt **FFmpeg** binary next to the application and calls it
as a **separate process** (subprocess) for:

- screen recording (encoding captured video/audio to MP4 / H.264 / AAC), and
- the in-app video / image format conversion dialogs.

FFmpeg is **not linked** into the Kashot executable — it is a standalone
binary launched via the operating system. Under the GPL this is "mere
aggregation": Kashot remains licensed under Apache-2.0.

- Project: <https://ffmpeg.org>
- The bundled builds include GPL-licensed components (notably **libx264** for
  H.264 video encoding), so the bundled FFmpeg binary is distributed under the
  **GNU General Public License, version 2 or later (GPL-2.0-or-later)**. The
  Linux builds are additionally configured with **libpulse** so the recorder
  can capture the microphone and system audio via PulseAudio.
- License text: <https://www.gnu.org/licenses/gpl-2.0.html>
- **Corresponding source** (GPLv2 §3) for the exact bundled builds is mirrored
  **into the same release as the binaries** — `deps-ffmpeg-v1` on
  <https://github.com/singhpratech/kashot/releases> — so the source ships
  alongside the binary and does not depend on any third-party server:
  - The **Linux** amd64 / arm64 binaries are BtbN static GPL builds; their exact
    source is the FFmpeg git tree at the embedded commit, mirrored as
    `ffmpeg-git-<commit>.tar.gz` (from <https://github.com/FFmpeg/FFmpeg>).
  - The **Windows** and **macOS** binaries are release builds; their source is
    the matching official `ffmpeg-<version>.tar.xz` from
    <https://ffmpeg.org/releases/>.
  `SOURCE.txt` in that release maps each binary to its source (with checksums)
  and includes a 3-year written offer. The exact build configuration for any
  binary is printed by `ffmpeg -version`.
- The **Snap** package is the exception: it does not bundle the mirrored
  binaries — it stages Ubuntu's `ffmpeg` package (also GPL-2.0-or-later, with
  libx264 + libpulse) via `snapcraft`. Its corresponding source is published by
  Ubuntu and obtainable with `apt-get source ffmpeg` from the matching release.

We gratefully credit the FFmpeg project and its contributors.

### How the binary is sourced (build reproducibility)

To avoid depending on third-party download servers at build time, the static
FFmpeg binaries are **mirrored to a Kashot-owned GitHub release**
(`deps-ffmpeg-v1`) by the one-time `.github/workflows/mirror-ffmpeg.yml`
workflow. Every Kashot release build downloads FFmpeg only from that
Kashot-owned release — never from an external server. The mirror is refreshed
manually (and the source link updated) when we choose to move to a newer
FFmpeg.
