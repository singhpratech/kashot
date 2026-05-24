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
- **Corresponding source** for the exact bundled build is published alongside
  the binaries on the Kashot dependency mirror release
  (`deps-ffmpeg-*` on <https://github.com/singhpratech/kashot/releases>) and is
  always available upstream at <https://ffmpeg.org/download.html> and
  <https://git.ffmpeg.org/ffmpeg.git>.

We gratefully credit the FFmpeg project and its contributors.

### How the binary is sourced (build reproducibility)

To avoid depending on third-party download servers at build time, the static
FFmpeg binaries are **mirrored to a Kashot-owned GitHub release**
(`deps-ffmpeg-v1`) by the one-time `.github/workflows/mirror-ffmpeg.yml`
workflow. Every Kashot release build downloads FFmpeg only from that
Kashot-owned release — never from an external server. The mirror is refreshed
manually (and the source link updated) when we choose to move to a newer
FFmpeg.
