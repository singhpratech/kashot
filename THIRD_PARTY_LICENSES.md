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
- The bundled build includes GPL-licensed components (notably **libx264** for
  H.264 video encoding), so the bundled FFmpeg binary is distributed under the
  **GNU General Public License, version 2 or later (GPL-2.0-or-later)**.
- License text: <https://www.gnu.org/licenses/gpl-2.0.html>
- **Corresponding source** (GPLv2 §3) for the exact bundled builds is mirrored
  **into the same release as the binaries** — `deps-ffmpeg-v1` on
  <https://github.com/singhpratech/kashot/releases> — so the source ships
  alongside the binary and does not depend on any third-party server:
  - `ffmpeg-7.0.2.tar.xz` — source for the Linux amd64 / arm64 binaries (7.0.2)
  - `ffmpeg-8.1.1.tar.xz` — source for the Windows + macOS binaries (8.1.1)
  These are the official, version-matched release tarballs from
  <https://ffmpeg.org/releases/>. `SOURCE.txt` in that release maps each binary
  to its source tarball (with checksums) and includes a 3-year written offer.
  The exact build configuration for any binary is printed by `ffmpeg -version`.

We gratefully credit the FFmpeg project and its contributors.

### How the binary is sourced (build reproducibility)

To avoid depending on third-party download servers at build time, the static
FFmpeg binaries are **mirrored to a Kashot-owned GitHub release**
(`deps-ffmpeg-v1`) by the one-time `.github/workflows/mirror-ffmpeg.yml`
workflow. Every Kashot release build downloads FFmpeg only from that
Kashot-owned release — never from an external server. The mirror is refreshed
manually (and the source link updated) when we choose to move to a newer
FFmpeg.
