//! Full-screen capture through `org.freedesktop.portal.Screenshot`.
//!
//! This is a **fallback**, not the Wayland capture path. `xcap` already knows
//! how to screenshot a Wayland session — `Monitor::capture_image` routes to
//! `org.gnome.Shell.Screenshot`, then to this same portal, then to wlroots'
//! `wlr-screencopy` — so under a normal Wayland desktop the ordinary capture
//! path in [`crate::capture`] works unchanged.
//!
//! What `xcap` cannot do without X11 is *enumerate* monitors: `Monitor::all()`
//! goes through XCB/RandR on every Linux session, Wayland included. That's
//! fine wherever XWayland is running (GNOME and KDE both start it by default),
//! and it fails outright on a Wayland-only session — taking capture down with
//! it even though the compositor would happily hand over a screenshot.
//!
//! So: when enumeration fails on a Wayland session, ask the portal for one
//! image of the whole screen and present it as a single-monitor capture. The
//! geometry is poorer than the native path's — one frame, origin assumed at
//! `(0, 0)`, no per-monitor rectangles and no scale factor — which is why this
//! only runs when the better path has already failed.

use crate::capture::{Captured, MonitorFrame};
use kashot_core::dpi::{DisplayMap, PhysicalRect};
use crate::{Error, Result};

use ashpd::desktop::screenshot::Screenshot;

/// Ask the portal for a screenshot of the whole screen and decode it.
///
/// `interactive(false)` asks for the shot without putting the portal's own
/// region picker in front of the user — Kashot has its own selection overlay
/// and running two in a row would be baffling. Portals may still show a
/// one-time permission prompt; that's theirs to decide, not something we can
/// or should route around.
pub fn capture_via_portal() -> Result<Captured> {
    let uri = pollster::block_on(async {
        Screenshot::request()
            .interactive(false)
            .modal(true)
            .send()
            .await
            .map_err(|e| portal_error("request", &e))?
            .response()
            .map_err(|e| portal_error("response", &e))
            .map(|shot| shot.uri().clone())
    })?;

    // The portal answers with a `file://` URI pointing at a PNG it wrote to a
    // temporary location it owns. Nothing else will delete it, so we do once
    // the pixels are decoded.
    let path = uri.to_file_path().map_err(|_| Error::Capture(format!(
        "the screenshot portal returned {uri}, which isn't a local file — Kashot \
         can only read a screenshot it can open from disk."
    )))?;

    let decoded = image::open(&path)
        .map_err(|e| Error::Capture(format!(
            "couldn't read the screenshot the portal wrote to {}: {e}", path.display()
        )));
    let _ = std::fs::remove_file(&path);
    let bitmap = decoded?.to_rgba8();

    let (width, height) = (bitmap.width(), bitmap.height());
    if width == 0 || height == 0 {
        return Err(Error::Capture(
            "the screenshot portal returned an empty image".into()));
    }

    Ok(Captured {
        bitmap,
        // One image covering everything the compositor chose to hand over. We
        // have no way to ask where that sits in a larger virtual screen, and
        // for a portal shot of "the screen" the answer is the origin.
        virtual_origin: (0, 0),
        // The portal image is already in device pixels; a 1x identity map
        // keeps every downstream coordinate calculation an identity.
        map: DisplayMap::identity(width, height),
        monitors: vec![MonitorFrame {
            x: 0,
            y: 0,
            width,
            height,
            name: "portal".to_string(),
            scale_factor: 1.0,
            effective_scale: 1.0,
            physical: PhysicalRect::new(0, 0, width, height),
        }],
    })
}

fn portal_error(stage: &str, e: &ashpd::Error) -> Error {
    Error::Capture(format!(
        "the desktop's screenshot portal failed at the {stage} stage ({e}). \
         Install and start xdg-desktop-portal plus the backend for your desktop \
         (for example xdg-desktop-portal-gnome, -kde or -wlr)."
    ))
}
