//! Unicode text layout + rasterization for the Text annotation.
//!
//! `bitmap_font.rs` in `kashot-app` still draws the app's own chrome — button
//! labels, the dimension chip, step numbers — where ASCII is all we ever emit
//! and a crisp 5×7 grid is the look we want. What it could never do is render
//! what the *user* types: accents, Greek, Cyrillic, punctuation, currency,
//! arrows. That's this module.
//!
//! Everything here is pure logic with no system dependency: the typeface is
//! compiled into the binary (`assets/fonts/NotoSans-Regular.ttf`, OFL-1.1) and
//! rasterized by `fontdue`, so a Text annotation comes out pixel-identical on
//! Windows, Linux and macOS regardless of what fonts the machine has
//! installed.
//!
//! Layout space has its origin at the **top-left of the text block** and grows
//! down/right, matching the painter's surface coordinates. A caller blits a
//! `TextBlock` by adding the annotation's position to every glyph's `x`/`y`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use fontdue::{Font, FontSettings, Metrics};
use unicode_segmentation::UnicodeSegmentation;

/// The bundled typeface. See `assets/fonts/README.md` for provenance and
/// `assets/fonts/OFL.txt` for the license.
pub const FONT_DATA: &[u8] = include_bytes!("../assets/fonts/NotoSans-Regular.ttf");

/// Smallest / largest pixel em-size we will rasterize at. The lower bound
/// keeps sub-pixel garbage off screen; the upper bound stops a pathological
/// `font_size` in a hand-edited session file from asking for a 100 MB glyph.
pub const MIN_PX: f32 = 6.0;
pub const MAX_PX: f32 = 400.0;

/// Em-size used when nothing else says otherwise — matches the middle entry
/// of the editor's thickness cycle.
pub const DEFAULT_PX: f32 = 28.0;

/// Parsed once, shared for the process lifetime.
pub fn font() -> &'static Font {
    static FONT: OnceLock<Font> = OnceLock::new();
    FONT.get_or_init(|| {
        Font::from_bytes(FONT_DATA, FontSettings::default())
            .expect("bundled Noto Sans is malformed — the asset is checked into the repo")
    })
}

/// Clamp an arbitrary requested size into the range we will actually draw.
pub fn clamp_px(px: f32) -> f32 {
    if px.is_finite() { px.clamp(MIN_PX, MAX_PX) } else { DEFAULT_PX }
}

/// Text em-size for a given stroke thickness. The editor cycles thickness
/// through 2 / 4 / 8 px, which maps to 21 / 28 / 42 px text — three clearly
/// distinct sizes that stay legible when the screenshot is viewed at 100 %.
pub fn font_size_for_thickness(thickness: f32) -> f32 {
    // `f32::max` swallows NaN rather than propagating it, so screen the
    // input before the arithmetic instead of leaning on `clamp_px`.
    if !thickness.is_finite() { return DEFAULT_PX; }
    clamp_px(14.0 + thickness.max(0.0) * 3.5)
}

/// Baseline-to-baseline distance at `px`.
pub fn line_height(px: f32) -> f32 {
    let px = clamp_px(px);
    match font().horizontal_line_metrics(px) {
        Some(m) => m.new_line_size,
        None    => px * 1.35,
    }
}

/// Distance from the top of the block to the first baseline.
pub fn ascent(px: f32) -> f32 {
    let px = clamp_px(px);
    match font().horizontal_line_metrics(px) {
        Some(m) => m.ascent,
        None    => px,
    }
}

// ── measurement ─────────────────────────────────────────────────────────────

/// Advance width of one line (no newlines expected), kerning included.
pub fn line_width(line: &str, px: f32) -> f32 {
    let px = clamp_px(px);
    let f  = font();
    let mut w = 0.0f32;
    let mut prev: Option<char> = None;
    for ch in line.chars() {
        if let Some(p) = prev {
            w += f.horizontal_kern(p, ch, px).unwrap_or(0.0);
        }
        w += f.metrics(ch, px).advance_width;
        prev = Some(ch);
    }
    w.max(0.0)
}

/// Per-line advance widths. A trailing newline yields a final empty line, so
/// the caret has somewhere to sit after the user presses Shift+Enter.
pub fn line_widths(text: &str, px: f32) -> Vec<f32> {
    split_lines(text).iter().map(|l| line_width(l, px)).collect()
}

/// Bounding size of the whole block: widest line × (line count × line height).
/// Cheap — metrics only, no rasterization — so the editor can call it on
/// every redraw to size the typing frame.
pub fn measure(text: &str, px: f32) -> (f32, f32) {
    let px = clamp_px(px);
    let widths = line_widths(text, px);
    let w = widths.iter().copied().fold(0.0f32, f32::max);
    let h = widths.len() as f32 * line_height(px);
    (w, h)
}

/// Split on `\n`, tolerating `\r\n`. Always at least one (possibly empty)
/// line, so an empty string still measures one line tall.
pub fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect()
}

// ── rectangles ──────────────────────────────────────────────────────────────

/// Axis-aligned box in layout space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl TextRect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self { Self { x, y, w, h } }

    pub fn right(self)  -> f32 { self.x + self.w }
    pub fn bottom(self) -> f32 { self.y + self.h }

    pub fn is_empty(self) -> bool { self.w <= 0.0 || self.h <= 0.0 }

    /// Do the two boxes share any area? Touching edges do not count.
    pub fn overlaps(self, other: TextRect) -> bool {
        !self.is_empty() && !other.is_empty()
            && self.x < other.right() && other.x < self.right()
            && self.y < other.bottom() && other.y < self.bottom()
    }

    /// Overlapping area of the two boxes, or `None` when they are disjoint.
    pub fn intersect(self, other: TextRect) -> Option<TextRect> {
        if !self.overlaps(other) { return None; }
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        Some(TextRect::new(x, y, self.right().min(other.right()) - x,
                                 self.bottom().min(other.bottom()) - y))
    }
}

// ── layout ──────────────────────────────────────────────────────────────────

/// One rasterized glyph, placed in layout space.
#[derive(Debug, Clone)]
pub struct PlacedGlyph {
    pub ch:   char,
    /// Zero-based line the glyph belongs to.
    pub line: usize,
    /// Top-left of the coverage bitmap.
    pub x: i32,
    pub y: i32,
    pub w: usize,
    pub h: usize,
    /// `w * h` coverage bytes, row-major, 0 = nothing, 255 = solid ink.
    /// Shared with the glyph cache — never mutate through the `Arc`.
    pub coverage: Arc<Vec<u8>>,
}

impl PlacedGlyph {
    pub fn bounds(&self) -> TextRect {
        TextRect::new(self.x as f32, self.y as f32, self.w as f32, self.h as f32)
    }

    /// Coverage at a pixel inside the glyph bitmap.
    pub fn coverage_at(&self, col: usize, row: usize) -> u8 {
        if col >= self.w || row >= self.h { return 0; }
        self.coverage[row * self.w + col]
    }
}

/// A laid-out (and rasterized) run of text.
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub px:          f32,
    pub line_height: f32,
    pub ascent:      f32,
    /// Advance width of each line, including empty ones.
    pub line_widths: Vec<f32>,
    /// Every glyph that has ink. Whitespace contributes advance but no entry.
    pub glyphs:      Vec<PlacedGlyph>,
}

impl TextBlock {
    pub fn line_count(&self) -> usize { self.line_widths.len() }

    pub fn width(&self) -> f32 {
        self.line_widths.iter().copied().fold(0.0f32, f32::max)
    }

    pub fn height(&self) -> f32 {
        self.line_widths.len() as f32 * self.line_height
    }

    /// Advance box of the block — what the editor draws its typing frame
    /// around. Note this is the *metric* box, not the ink box: a glyph with
    /// a wide overhang may poke a pixel or two outside it.
    pub fn bounds(&self) -> TextRect {
        TextRect::new(0.0, 0.0, self.width(), self.height())
    }

    /// Where the insertion caret goes: `(x, top_y)` at the end of the last
    /// line, with the caret's own height being `line_height`.
    pub fn caret(&self) -> (f32, f32) {
        let last = self.line_widths.len().saturating_sub(1);
        (self.line_widths.get(last).copied().unwrap_or(0.0), last as f32 * self.line_height)
    }

    /// Glyphs with any ink inside `clip`. Used by callers that need to skip
    /// work for text scrolled or dragged outside the visible region; the
    /// painter also clips per pixel, so this is an optimization, not the
    /// thing that keeps writes in bounds.
    pub fn visible_glyphs(&self, clip: TextRect) -> impl Iterator<Item = &PlacedGlyph> {
        self.glyphs.iter().filter(move |g| g.bounds().overlaps(clip))
    }
}

/// Lay out and rasterize `text` at em-size `px`.
pub fn layout(text: &str, px: f32) -> TextBlock {
    let px    = clamp_px(px);
    let f     = font();
    let lh    = line_height(px);
    let asc   = ascent(px);
    let lines = split_lines(text);

    let mut widths = Vec::with_capacity(lines.len());
    let mut glyphs = Vec::new();

    for (line_idx, line) in lines.iter().enumerate() {
        let baseline = asc + line_idx as f32 * lh;
        let mut pen  = 0.0f32;
        let mut prev: Option<char> = None;
        for ch in line.chars() {
            if let Some(p) = prev {
                pen += f.horizontal_kern(p, ch, px).unwrap_or(0.0);
            }
            let (metrics, coverage) = raster(ch, px);
            if metrics.width > 0 && metrics.height > 0 {
                // fontdue reports `ymin` as the offset from the baseline to
                // the *bottom* of the bitmap, positive upward. Flip it into
                // our y-down layout space.
                let top = baseline - (metrics.ymin + metrics.height as i32) as f32;
                glyphs.push(PlacedGlyph {
                    ch,
                    line: line_idx,
                    x: (pen + metrics.xmin as f32).round() as i32,
                    y: top.round() as i32,
                    w: metrics.width,
                    h: metrics.height,
                    coverage,
                });
            }
            pen += metrics.advance_width;
            prev = Some(ch);
        }
        widths.push(pen.max(0.0));
    }

    TextBlock { px, line_height: lh, ascent: asc, line_widths: widths, glyphs }
}

// ── glyph cache ─────────────────────────────────────────────────────────────

type CacheEntry = (Metrics, Arc<Vec<u8>>);

/// Rasterized glyphs keyed by (char, exact em-size). The editor re-renders
/// every annotation on every redraw — up to 30 fps while the panel pulses —
/// so caching keeps a screenshot full of text from re-rasterizing per frame.
/// Bounded by a hard flush: annotation text is short and the size set is
/// tiny, so we never get near the cap in practice.
fn cache() -> &'static Mutex<HashMap<(char, u32), CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<(char, u32), CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

const CACHE_CAP: usize = 4096;

fn raster(ch: char, px: f32) -> CacheEntry {
    let key = (ch, px.to_bits());
    // A poisoned cache mutex must not take the editor down with it — we can
    // always rasterize again from scratch.
    if let Ok(map) = cache().lock() {
        if let Some(hit) = map.get(&key) {
            return hit.clone();
        }
    }
    let (metrics, bitmap) = font().rasterize(ch, px);
    let entry: CacheEntry = (metrics, Arc::new(bitmap));
    if let Ok(mut map) = cache().lock() {
        if map.len() >= CACHE_CAP { map.clear(); }
        map.insert(key, entry.clone());
    }
    entry
}

// ── editing helpers ─────────────────────────────────────────────────────────

/// Remove the last **grapheme cluster** from `s`, returning what was removed.
///
/// A plain `String::pop` deletes one `char`, which on any composed text is the
/// wrong unit: backspacing "é" written as `e` + U+0301 would leave a bare `e`
/// behind, and backspacing a flag or a ZWJ emoji sequence would peel it apart
/// one scalar at a time. Users expect one keypress to remove one thing they
/// can see.
pub fn pop_grapheme(s: &mut String) -> Option<String> {
    let last = s.graphemes(true).next_back()?.to_owned();
    let cut  = s.len() - last.len();
    s.truncate(cut);
    Some(last)
}

/// Number of user-visible characters — what a caret would step over.
pub fn grapheme_count(s: &str) -> usize {
    s.graphemes(true).count()
}

/// Filter text arriving from a key event or an IME commit down to what a
/// single-line-plus-newlines annotation can hold: control characters are
/// dropped, except `\n`, which we keep so Shift+Enter can start a new line.
/// `\r` and `\r\n` both normalize to a single `\n`.
pub fn sanitize_input(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') { chars.next(); }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_font_parses() {
        // Sanity on the checked-in asset: if this fails the TTF is corrupt.
        assert!(font().horizontal_line_metrics(24.0).is_some());
    }

    #[test]
    fn line_width_grows_with_text_and_size() {
        assert!(line_width("Hello", 24.0) > line_width("Hell", 24.0));
        assert!(line_width("Hello", 48.0) > line_width("Hello", 24.0));
        assert_eq!(line_width("", 24.0), 0.0);
    }

    #[test]
    fn accented_and_cyrillic_text_measures_nonzero() {
        // The whole point of the change: these used to render as '?' boxes.
        for s in ["café", "Grüße", "Привет", "Ελληνικά", "½ € ± ·"] {
            assert!(line_width(s, 24.0) > 0.0, "{s} measured as zero-width");
            let block = layout(s, 24.0);
            assert!(!block.glyphs.is_empty(), "{s} produced no glyphs");
        }
    }

    #[test]
    fn unsupported_script_falls_back_to_a_visible_box() {
        // The bundled face carries no CJK. Those characters must still take
        // up space and draw the font's `.notdef` box — silently dropping
        // them would make text the user typed disappear.
        let block = layout("\u{65e5}", 32.0);
        assert_eq!(block.glyphs.len(), 1);
        assert!(block.line_widths[0] > 0.0);
        assert!(block.glyphs[0].coverage.iter().any(|&c| c > 0));
    }

    #[test]
    fn split_lines_handles_empties_and_crlf() {
        assert_eq!(split_lines(""),          vec![""]);
        assert_eq!(split_lines("a\nb"),      vec!["a", "b"]);
        assert_eq!(split_lines("a\r\nb"),    vec!["a", "b"]);
        // A trailing newline leaves an empty last line for the caret.
        assert_eq!(split_lines("a\n"),       vec!["a", ""]);
    }

    #[test]
    fn measure_uses_widest_line_and_counts_every_line() {
        let px = 24.0;
        let (w, h) = measure("short\nmuch longer line", px);
        assert!((w - line_width("much longer line", px)).abs() < 0.01);
        assert!((h - 2.0 * line_height(px)).abs() < 0.01);

        // An empty string still occupies one line so the typing frame has
        // height the moment the caret is placed.
        let (w0, h0) = measure("", px);
        assert_eq!(w0, 0.0);
        assert!((h0 - line_height(px)).abs() < 0.01);
    }

    #[test]
    fn measure_agrees_with_layout() {
        let px = 32.0;
        let text = "Über\nстрока\nthird";
        let (w, h) = measure(text, px);
        let block = layout(text, px);
        assert!((block.width()  - w).abs() < 0.01);
        assert!((block.height() - h).abs() < 0.01);
        assert_eq!(block.line_count(), 3);
    }

    #[test]
    fn glyphs_stack_down_the_block_per_line() {
        let block = layout("A\nA", 32.0);
        let first: Vec<_> = block.glyphs.iter().filter(|g| g.line == 0).collect();
        let second: Vec<_> = block.glyphs.iter().filter(|g| g.line == 1).collect();
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        // Same glyph, one line height apart, same x.
        assert_eq!(first[0].x, second[0].x);
        let dy = second[0].y - first[0].y;
        assert!((dy as f32 - block.line_height).abs() <= 1.0,
                "line spacing {dy} should be ~{}", block.line_height);
    }

    #[test]
    fn whitespace_carries_advance_but_no_ink() {
        let block = layout("a b", 24.0);
        assert!(block.glyphs.iter().all(|g| g.ch != ' '), "space should not rasterize");
        assert!(block.line_widths[0] > line_width("ab", 24.0));
    }

    #[test]
    fn caret_sits_at_the_end_of_the_last_line() {
        let px = 24.0;
        let block = layout("ab\nlonger", px);
        let (cx, cy) = block.caret();
        assert!((cx - line_width("longer", px)).abs() < 0.01);
        assert!((cy - block.line_height).abs() < 0.01);

        // After a trailing newline the caret drops to a fresh empty line.
        let block = layout("ab\n", px);
        let (cx, cy) = block.caret();
        assert_eq!(cx, 0.0);
        assert!((cy - block.line_height).abs() < 0.01);
    }

    #[test]
    fn rect_intersection_and_overlap() {
        let a = TextRect::new(0.0, 0.0, 10.0, 10.0);
        let b = TextRect::new(5.0, 5.0, 10.0, 10.0);
        assert_eq!(a.intersect(b), Some(TextRect::new(5.0, 5.0, 5.0, 5.0)));
        // Edge-touching is not an overlap.
        assert!(!a.overlaps(TextRect::new(10.0, 0.0, 5.0, 5.0)));
        assert_eq!(a.intersect(TextRect::new(50.0, 50.0, 1.0, 1.0)), None);
        // Degenerate boxes never overlap.
        assert!(!a.overlaps(TextRect::new(1.0, 1.0, 0.0, 5.0)));
    }

    #[test]
    fn visible_glyphs_clips_to_the_requested_box() {
        let px    = 32.0;
        let block = layout("MMMMMMMMMM", px);
        let all   = block.glyphs.len();
        assert_eq!(all, 10);

        // Everything is inside a box covering the whole block.
        let full = block.bounds();
        let generous = TextRect::new(full.x - 8.0, full.y - 8.0, full.w + 16.0, full.h + 16.0);
        assert_eq!(block.visible_glyphs(generous).count(), all);

        // A narrow left-hand box keeps only the leading glyphs.
        let narrow = TextRect::new(0.0, 0.0, block.width() / 4.0, block.height());
        let kept   = block.visible_glyphs(narrow).count();
        assert!(kept > 0 && kept < all, "expected a partial clip, kept {kept} of {all}");

        // A box entirely off to the side keeps nothing.
        let away = TextRect::new(block.width() + 100.0, 0.0, 50.0, block.height());
        assert_eq!(block.visible_glyphs(away).count(), 0);
    }

    #[test]
    fn coverage_is_antialiased_not_binary() {
        let block = layout("S", 48.0);
        let g = &block.glyphs[0];
        assert_eq!(g.coverage.len(), g.w * g.h);
        let partial = g.coverage.iter().filter(|&&c| c > 0 && c < 255).count();
        assert!(partial > 0, "a 48px 'S' should have soft edges");
        assert_eq!(g.coverage_at(g.w, 0), 0, "out-of-range reads are transparent");
    }

    #[test]
    fn font_size_tracks_the_thickness_cycle() {
        let sizes: Vec<f32> = [2.0, 4.0, 8.0].iter().map(|t| font_size_for_thickness(*t)).collect();
        assert!(sizes[0] < sizes[1] && sizes[1] < sizes[2]);
        assert!(sizes[0] >= 16.0, "smallest text must still be readable, got {}", sizes[0]);
        assert_eq!(font_size_for_thickness(4.0), DEFAULT_PX);
        // Junk input can't escape the rasterizable range.
        assert_eq!(font_size_for_thickness(f32::NAN), DEFAULT_PX);
        assert!(font_size_for_thickness(1.0e9) <= MAX_PX);
    }

    #[test]
    fn clamp_px_rejects_nonsense() {
        assert_eq!(clamp_px(f32::NAN),       DEFAULT_PX);
        assert_eq!(clamp_px(f32::INFINITY),  DEFAULT_PX);
        assert_eq!(clamp_px(0.0),            MIN_PX);
        assert_eq!(clamp_px(-5.0),           MIN_PX);
        assert_eq!(clamp_px(1.0e6),          MAX_PX);
    }

    #[test]
    fn pop_grapheme_removes_one_visible_character() {
        // Precomposed.
        let mut s = String::from("café");
        assert_eq!(pop_grapheme(&mut s).as_deref(), Some("é"));
        assert_eq!(s, "caf");

        // Decomposed: 'e' + COMBINING ACUTE must go together.
        let mut s = String::from("cafe\u{0301}");
        assert_eq!(grapheme_count(&s), 4);
        assert_eq!(pop_grapheme(&mut s).as_deref(), Some("e\u{0301}"));
        assert_eq!(s, "caf");

        // CRLF is a single cluster.
        let mut s = String::from("a\r\n");
        assert_eq!(pop_grapheme(&mut s).as_deref(), Some("\r\n"));
        assert_eq!(s, "a");

        // Empty string: nothing to remove, no panic.
        let mut s = String::new();
        assert_eq!(pop_grapheme(&mut s), None);
        assert!(s.is_empty());
    }

    #[test]
    fn pop_grapheme_handles_multi_scalar_clusters() {
        // Regional-indicator flag pair.
        let mut s = String::from("hi\u{1F1EE}\u{1F1F3}");
        assert_eq!(grapheme_count(&s), 3);
        pop_grapheme(&mut s);
        assert_eq!(s, "hi");

        // ZWJ sequence.
        let mut s = String::from("x\u{1F469}\u{200D}\u{1F4BB}");
        pop_grapheme(&mut s);
        assert_eq!(s, "x");
    }

    #[test]
    fn sanitize_input_keeps_unicode_and_drops_controls() {
        assert_eq!(sanitize_input("é"),        "é");
        assert_eq!(sanitize_input("añ日"),     "añ日");
        assert_eq!(sanitize_input("\u{8}"),    "");   // backspace
        assert_eq!(sanitize_input("\u{1b}"),   "");   // escape
        assert_eq!(sanitize_input("\t"),       "");   // tab
        assert_eq!(sanitize_input(" "),        " ");  // space survives
        assert_eq!(sanitize_input("a\r\nb"),   "a\nb");
        assert_eq!(sanitize_input("a\rb"),     "a\nb");
        assert_eq!(sanitize_input("a\nb"),     "a\nb");
    }

    #[test]
    fn layout_is_stable_across_the_glyph_cache() {
        // Second pass comes out of the cache; it must be byte-identical.
        let a = layout("Caché ✓", 26.0);
        let b = layout("Caché ✓", 26.0);
        assert_eq!(a.glyphs.len(), b.glyphs.len());
        for (ga, gb) in a.glyphs.iter().zip(b.glyphs.iter()) {
            assert_eq!((ga.x, ga.y, ga.w, ga.h), (gb.x, gb.y, gb.w, gb.h));
            assert_eq!(ga.coverage, gb.coverage);
        }
    }
}
