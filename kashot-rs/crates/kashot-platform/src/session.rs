//! Facts about the desktop session the app is running inside.
//!
//! Today that's one question — "is this a Wayland session?" — but it's asked
//! from more than one place (the recorder refuses `-f x11grab` there, and any
//! future X11-only path needs the same answer), and getting it wrong is not a
//! visible failure: XWayland hands `x11grab` a black frame instead of an error.
//! Keeping the probe in one module means every caller agrees, and the answer is
//! testable without a display server.

/// Whether the current desktop session is Wayland.
///
/// Two independent signals, either of which is decisive:
///
/// * `XDG_SESSION_TYPE=wayland` — what the login manager stamped on the
///   session. Authoritative when present.
/// * a non-empty `WAYLAND_DISPLAY` — a compositor socket is exported into the
///   environment. This catches sessions where `XDG_SESSION_TYPE` is missing or
///   stale: nested compositors, `su`/`sudo` environments, and login managers
///   that never set it.
///
/// Always `false` off Linux — Windows and macOS have no Wayland session, so
/// callers can branch on this unconditionally instead of re-writing the `cfg`.
#[cfg(target_os = "linux")]
pub fn is_wayland() -> bool {
    let typed = std::env::var("XDG_SESSION_TYPE")
        .map(|s| s.trim().eq_ignore_ascii_case("wayland"))
        .unwrap_or(false);
    let socket = std::env::var("WAYLAND_DISPLAY")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    typed || socket
}

#[cfg(not(target_os = "linux"))]
pub fn is_wayland() -> bool {
    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// The env vars are process-global, so the cases can't run in parallel —
    /// they're driven from one `#[test]` that restores what it found.
    #[test]
    fn wayland_detection_reads_both_signals() {
        let prev_type   = std::env::var("XDG_SESSION_TYPE").ok();
        let prev_socket = std::env::var("WAYLAND_DISPLAY").ok();

        std::env::remove_var("XDG_SESSION_TYPE");
        std::env::remove_var("WAYLAND_DISPLAY");
        assert!(!is_wayland(), "no signals at all means not Wayland");

        std::env::set_var("XDG_SESSION_TYPE", "x11");
        assert!(!is_wayland(), "an X11 session is not Wayland");

        std::env::set_var("XDG_SESSION_TYPE", "Wayland");
        assert!(is_wayland(), "session type match is case-insensitive");

        // An empty WAYLAND_DISPLAY is exported by some shells; it is not a
        // compositor socket and must not count on its own.
        std::env::set_var("XDG_SESSION_TYPE", "x11");
        std::env::set_var("WAYLAND_DISPLAY", "");
        assert!(!is_wayland(), "empty WAYLAND_DISPLAY is not a socket");

        std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
        assert!(is_wayland(), "a compositor socket alone is decisive");

        match prev_type {
            Some(v) => std::env::set_var("XDG_SESSION_TYPE", v),
            None    => std::env::remove_var("XDG_SESSION_TYPE"),
        }
        match prev_socket {
            Some(v) => std::env::set_var("WAYLAND_DISPLAY", v),
            None    => std::env::remove_var("WAYLAND_DISPLAY"),
        }
    }
}
