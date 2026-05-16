//! Single source for the winit window icon that every Kashot window
//! (overlay, pin, settings, about, updates, convert-image, convert-video,
//! recording indicator) attaches to itself.
//!
//! Loads the same 256×256 brand PNG that the tray icon, the Linux desktop
//! launcher, the Windows .ico, and the macOS .icns all derive from — so
//! the taskbar / dock / Alt-Tab thumbnail are visually identical to the
//! tray icon and the installer artwork. One source of truth.
//!
//! The decode happens once on first call; subsequent calls reuse the
//! lazily-built `Icon`. Failure is non-fatal — a window with no icon
//! still works, the OS just falls back to its generic placeholder.

use std::sync::OnceLock;
use winit::window::Icon;

/// 256×256 is big enough that any OS thumbnail size downsamples cleanly,
/// and small enough that the embedded PNG (~6 KB) doesn't bloat the
/// binary. The same file feeds the macOS .icns build and is referenced
/// by the Linux .desktop entry.
const ICON_PNG: &[u8] = include_bytes!(
    "../../../../icons/linux_hicolor/256x256/apps/kashot.png"
);

/// Shared lazily-built icon. `OnceLock` avoids re-decoding the PNG every
/// time we open a window, which would otherwise happen 5+ times in a
/// normal tray session (one per dialog open).
static ICON: OnceLock<Option<Icon>> = OnceLock::new();

/// Returns the shared brand icon, decoded on first call. `None` means the
/// PNG bytes failed to decode (shouldn't happen — embedded at compile
/// time) or `Icon::from_rgba` rejected them. Callers should treat `None`
/// as "no icon" rather than a hard error.
pub fn shared() -> Option<Icon> {
    ICON.get_or_init(|| {
        let img = image::load_from_memory(ICON_PNG).ok()?.into_rgba8();
        let (w, h) = img.dimensions();
        Icon::from_rgba(img.into_raw(), w, h).ok()
    }).clone()
}
