//! Clipboard image copy. Uses `arboard`, which speaks the platform-native
//! clipboard protocol (Win32 OLE, X11/Wayland selections, NSPasteboard).

use crate::{Error, Result};
use image::{ImageBuffer, Rgba};

/// Copy `bitmap` to the system clipboard as image data.
pub fn copy_image_png(bitmap: &ImageBuffer<Rgba<u8>, Vec<u8>>) -> Result<()> {
    let mut cb = arboard::Clipboard::new()
        .map_err(|e| Error::Clipboard(e.to_string()))?;

    let img = arboard::ImageData {
        width:  bitmap.width()  as usize,
        height: bitmap.height() as usize,
        bytes:  std::borrow::Cow::Borrowed(bitmap.as_raw()),
    };

    cb.set_image(img).map_err(|e| Error::Clipboard(e.to_string()))?;
    Ok(())
}

/// Copy `text` to the system clipboard.
///
/// Used by the Updates dialog to hand over the package-manager command for
/// installs the in-app updater must not touch. Same `arboard` backend as the
/// image path, so it behaves identically on all three platforms.
pub fn copy_text(text: &str) -> Result<()> {
    let mut cb = arboard::Clipboard::new()
        .map_err(|e| Error::Clipboard(e.to_string()))?;
    cb.set_text(text.to_owned())
        .map_err(|e| Error::Clipboard(e.to_string()))?;
    Ok(())
}
