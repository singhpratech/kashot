# Bundled typeface

`NotoSans-Regular.ttf` — **Noto Sans**, Regular, static hinted TTF.

Used by `kashot_core::text` for the annotation Text tool: it is compiled into
the binary with `include_bytes!` and rasterized at runtime by `fontdue`, so the
Text tool renders the same glyphs on Windows, Linux and macOS with no
dependency on any system font.

- Upstream: <https://github.com/notofonts/latin-greek-cyrillic>
- Version: Noto Sans 2022 (`Copyright 2022 The Noto Project Authors`)
- SHA256: `478c558ea716033cd60c03438f628dfa75694dcf6b5f6d505a2f05fd2b4f3823`
- License: **SIL Open Font License 1.1** — full text in `OFL.txt` next to this
  file, and summarized in `THIRD_PARTY_LICENSES.md` at the repo root.

Coverage is Latin (including the full set of accented and Central/Eastern
European forms), Greek, Cyrillic, general punctuation, currency and the common
math symbols. It deliberately does **not** carry CJK, Arabic, Hebrew, Indic, or
the dingbat/arrow blocks — those live in separate Noto families and are not
worth the binary size here. A character the font has no glyph for renders as
the font's `.notdef` box, so it stays visible and takes up space rather than
silently vanishing from the annotation.
