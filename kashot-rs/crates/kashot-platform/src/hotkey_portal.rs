//! Global hotkey on Wayland, via the `org.freedesktop.portal.GlobalShortcuts`
//! portal.
//!
//! On X11, Windows and macOS a global hotkey is a *grab*: the app tells the
//! window system "give me this key" and the window system obeys. Wayland
//! deliberately has no such call — a client cannot see, let alone intercept,
//! input aimed at another window. The replacement is a negotiation with
//! `xdg-desktop-portal`: we describe the shortcut we'd like, the compositor
//! decides what it's actually bound to (and shows it to the user in its own
//! keyboard settings), and it sends us an `Activated` signal when the user
//! presses it.
//!
//! Three consequences shape this module:
//!
//! * **The trigger is a request, not a command.** `preferred_trigger` is a
//!   hint. A compositor may hand the user a different combination, or none at
//!   all until they pick one. So the settings REBIND widget still works — it
//!   just asks for a binding instead of taking one — and the honest thing to
//!   tell users is where their desktop lists it.
//! * **Rebinding means a new session.** The portal only accepts `BindShortcuts`
//!   once per session, so [`PortalHotkeys::register_all`] closes the old
//!   session and opens a fresh one every time, binding the whole set at once.
//! * **Failure must be loud.** The X11 backend under Wayland *succeeds* and
//!   then never fires, which is the worst possible outcome; the whole point of
//!   this module is that "no portal" comes back as an error with something the
//!   user can act on.
//!
//! Threading: two detached threads, each blocking on its own future.
//!
//! * The **control** thread owns the portal proxy and the live session, and
//!   serves bind/unbind commands off a plain `std::sync::mpsc` channel.
//! * The **listener** thread owns the `Activated` signal stream and pushes a
//!   token per press into a channel that [`PortalHotkeys::drain_pressed`]
//!   polls, mirroring how the `global-hotkey` backend is drained from the
//!   winit loop.
//!
//! They can be separate threads because `ashpd` keeps a single process-wide
//! session-bus connection: the portal unicasts `Activated` back to the
//! connection that created the session, so a stream opened on any `ashpd`
//! proxy sees it. Neither thread blocks the UI — the only bounded waits are
//! the startup handshake and the bind reply, both on the caller's side.

use crate::{Error, Result};
use kashot_core::hotkeys::HotkeyAction;
use kashot_core::settings::Hotkey;
use kashot_core::shortcut::portal_trigger;

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;

/// Each action's portal id. They are persisted by the portal against these
/// strings, so changing one orphans every existing user's binding — treat
/// them as a wire format, not labels. `"capture"` predates the other two.
fn shortcut_id(action: HotkeyAction) -> &'static str {
    match action {
        HotkeyAction::Capture           => "capture",
        HotkeyAction::CaptureFullScreen => "capture-full-screen",
        HotkeyAction::ToggleRecording   => "toggle-recording",
    }
}

/// The inverse of [`shortcut_id`], for the `Activated` signal.
fn action_for_id(id: &str) -> Option<HotkeyAction> {
    HotkeyAction::ALL.iter().copied().find(|a| shortcut_id(*a) == id)
}

/// Shown to the user by the compositor in its own shortcut settings, so it
/// reads as a sentence about what the key does rather than as a symbol.
fn shortcut_description(action: HotkeyAction) -> &'static str {
    match action {
        HotkeyAction::Capture           => "Capture a screenshot",
        HotkeyAction::CaptureFullScreen => "Capture the full screen",
        HotkeyAction::ToggleRecording   => "Start or stop a screen recording",
    }
}

/// How long the constructor waits for the portal to answer "yes I exist" —
/// proxy creation plus one `CreateSession` round-trip. Both are non-interactive
/// D-Bus calls that answer in milliseconds when the service is running; this
/// budget is for a cold-started `xdg-desktop-portal`, not for a user.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long [`PortalHotkeys::register_all`] waits for the bind result before
/// giving up on *reporting* it (the bind itself is unaffected and continues in
/// the background).
///
/// GNOME and KDE answer `BindShortcuts` immediately. A portal that instead
/// puts a confirmation dialog in front of the user could take minutes, and
/// this call runs on the UI thread, so a timeout is treated as "probably
/// fine, still pending" rather than as a failure. Being wrong that way costs a
/// missing toast; blocking here would freeze the tray.
const BIND_TIMEOUT: Duration = Duration::from_secs(2);

/// A message to the control thread.
enum Cmd {
    /// Bind every `(action, trigger)` in one session, replacing whatever
    /// session is live. The outcome goes back over `reply` — `Ok` once the
    /// portal has accepted the request.
    Bind {
        shortcuts: Vec<(HotkeyAction, String)>,
        reply:     Sender<std::result::Result<(), String>>,
    },
    /// Drop the live session without opening a new one.
    Unbind,
}

/// A live GlobalShortcuts registration.
///
/// Dropping it closes the channels, which lands both threads' loops and closes
/// the portal session.
pub struct PortalHotkeys {
    cmds:    Sender<Cmd>,
    presses: Receiver<HotkeyAction>,
}

impl PortalHotkeys {
    /// Connect to the portal and prove it answers.
    ///
    /// Errors carry text meant to be shown to a user, not logged: reaching
    /// this path at all means the session is Wayland, where the alternative is
    /// a hotkey that reports success and never fires.
    pub fn new() -> Result<Self> {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();
        let (press_tx, press_rx) = std::sync::mpsc::channel::<HotkeyAction>();

        std::thread::Builder::new()
            .name("kashot-portal-hotkey".into())
            .spawn(move || control_loop(cmd_rx, ready_tx))
            .map_err(|e| Error::Hotkey(format!("couldn't start the portal thread: {e}")))?;

        match ready_rx.recv_timeout(HANDSHAKE_TIMEOUT) {
            Ok(Ok(()))   => {}
            Ok(Err(msg)) => return Err(Error::Hotkey(msg)),
            Err(_) => return Err(Error::Hotkey(format!(
                "the desktop's global-shortcuts portal didn't answer within {} seconds. \
                 Make sure xdg-desktop-portal and the backend for your desktop (for \
                 example xdg-desktop-portal-gnome or -kde) are installed and running; \
                 until then, capture from the tray menu.",
                HANDSHAKE_TIMEOUT.as_secs()
            ))),
        }

        // Only started once the portal has answered — a listener against a
        // portal that isn't there would sit on a stream that never yields.
        std::thread::Builder::new()
            .name("kashot-portal-hotkey-rx".into())
            .spawn(move || listen_loop(press_tx))
            .map_err(|e| Error::Hotkey(format!("couldn't start the portal listener: {e}")))?;

        Ok(PortalHotkeys { cmds: cmd_tx, presses: press_rx })
    }

    /// Ask the compositor to bind every action in `bindings`, replacing any
    /// previous request.
    ///
    /// Mirrors `HotkeyManager::register_all`: a binding whose key has no
    /// portal trigger is reported and skipped, the rest go out in one
    /// `BindShortcuts`. If the portal refuses that request every action in
    /// it is reported, since none of them will fire. `Ok`-ness of the bind
    /// means the portal accepted the request — *not* that the keys are live,
    /// which only the compositor can decide. See the module docs.
    pub fn register_all(&mut self, bindings: &[(HotkeyAction, Hotkey)])
        -> Vec<(HotkeyAction, Error)>
    {
        let mut failed = Vec::new();
        let mut shortcuts = Vec::with_capacity(bindings.len());
        for &(action, hk) in bindings {
            match portal_trigger(&hk) {
                Some(trigger) => shortcuts.push((action, trigger)),
                None => failed.push((action, Error::Hotkey(format!(
                    "{} can't be requested as a Wayland shortcut — pick a different key \
                     in Settings.", hk.describe()
                )))),
            }
        }
        if shortcuts.is_empty() {
            self.unregister_all();
            return failed;
        }
        let bound: Vec<HotkeyAction> = shortcuts.iter().map(|(a, _)| *a).collect();

        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if self.cmds.send(Cmd::Bind { shortcuts, reply: reply_tx }).is_err() {
            for a in bound {
                failed.push((a, Error::Hotkey("the portal thread stopped".into())));
            }
            return failed;
        }

        match reply_rx.recv_timeout(BIND_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => {
                for a in bound { failed.push((a, Error::Hotkey(msg.clone()))); }
            }
            // Still pending. Say nothing rather than invent a failure; see
            // BIND_TIMEOUT.
            Err(_) => {}
        }
        failed
    }

    /// Drop the binding. Best-effort: there is nothing a caller could do with
    /// a failure here, and the session is closed on drop regardless.
    pub fn unregister_all(&mut self) {
        let _ = self.cmds.send(Cmd::Unbind);
    }

    /// Take every press seen since the last call, reporting each action at
    /// most once so a burst is one event. Matches `HotkeyManager::
    /// drain_pressed`'s contract so the winit loop doesn't care which backend
    /// is underneath.
    pub fn drain_pressed(&self) -> Vec<HotkeyAction> {
        let mut fired: Vec<HotkeyAction> = Vec::new();
        loop {
            match self.presses.try_recv() {
                Ok(action) => { if !fired.contains(&action) { fired.push(action); } }
                Err(TryRecvError::Empty)      => break,
                // The listener thread is gone; nothing will ever arrive again.
                Err(TryRecvError::Disconnected) => break,
            }
        }
        fired
    }
}

/// Owns the portal proxy and the live session for the process's lifetime.
///
/// Runs one future to completion on its own thread: everything inside is
/// sequential, and the blocking `recv()` between commands is safe because
/// `zbus` drives its connection on its own executor thread, not on ours.
fn control_loop(cmds: Receiver<Cmd>, ready: Sender<std::result::Result<(), String>>) {
    pollster::block_on(async move {
        let shortcuts = match GlobalShortcuts::new().await {
            Ok(s)  => s,
            Err(e) => { let _ = ready.send(Err(portal_missing(&e))); return; }
        };

        // Creating the proxy can succeed against a bus that has no portal on
        // it, so the handshake isn't done until a real call comes back.
        let mut session = match shortcuts.create_session().await {
            Ok(s)  => Some(s),
            Err(e) => { let _ = ready.send(Err(portal_missing(&e))); return; }
        };
        let _ = ready.send(Ok(()));

        while let Ok(cmd) = cmds.recv() {
            match cmd {
                Cmd::Bind { shortcuts: wanted, reply } => {
                    // One BindShortcuts per session, so a rebind is a new
                    // session. Closing first means the compositor never shows
                    // two competing bindings for the same shortcut id.
                    if let Some(prev) = session.take() {
                        let _ = prev.close().await;
                    }
                    let fresh = match shortcuts.create_session().await {
                        Ok(s)  => s,
                        Err(e) => { let _ = reply.send(Err(portal_missing(&e))); continue; }
                    };

                    let requests: Vec<NewShortcut> = shortcuts_wanted(&wanted);

                    let outcome = match shortcuts.bind_shortcuts(&fresh, &requests, None).await {
                        Ok(req) => req.response().map(|_| ()).map_err(|e| bind_refused(&e)),
                        Err(e)  => Err(bind_refused(&e)),
                    };
                    session = Some(fresh);
                    let _ = reply.send(outcome);
                }
                Cmd::Unbind => {
                    if let Some(prev) = session.take() {
                        let _ = prev.close().await;
                    }
                }
            }
        }

        // Sender dropped: PortalHotkeys is gone, so release the session rather
        // than leaving a stale binding in the compositor's list.
        if let Some(prev) = session.take() {
            let _ = prev.close().await;
        }
    });
}

/// One `NewShortcut` per requested action.
fn shortcuts_wanted(wanted: &[(HotkeyAction, String)]) -> Vec<NewShortcut> {
    wanted.iter()
        .map(|(action, trigger)| {
            NewShortcut::new(shortcut_id(*action), shortcut_description(*action))
                .preferred_trigger(trigger.as_str())
        })
        .collect()
}

/// Forwards `Activated` signals for our shortcut ids into `presses`.
///
/// Exits when the receiving end is dropped or the stream ends (portal
/// restart). A dead listener is not fatal — the tray menu still captures — so
/// it reports to stderr and stops rather than taking the app down.
fn listen_loop(presses: Sender<HotkeyAction>) {
    pollster::block_on(async move {
        let shortcuts = match GlobalShortcuts::new().await {
            Ok(s)  => s,
            Err(e) => { eprintln!("kashot: portal shortcut listener failed to start: {e}"); return; }
        };
        let stream = match shortcuts.receive_activated().await {
            Ok(s)  => s,
            Err(e) => { eprintln!("kashot: portal shortcut listener failed to subscribe: {e}"); return; }
        };
        // The stream is an opaque future type with no Unpin guarantee.
        let mut stream = Box::pin(stream);

        while let Some(event) = stream.next().await {
            // The signal is per-connection, not per-session, and a rebind
            // gives us a new session handle — so the shortcut id, which is
            // stable, is what identifies our key.
            if let Some(action) = action_for_id(event.shortcut_id()) {
                if presses.send(action).is_err() {
                    return;
                }
            }
        }
        eprintln!("kashot: the global-shortcuts portal closed its signal stream; \
                   the capture shortcut won't fire until Kashot is restarted.");
    });
}

/// The message shown when the portal can't be reached at all.
fn portal_missing(e: &ashpd::Error) -> String {
    format!(
        "this is a Wayland session, where the capture shortcut has to be registered \
         through xdg-desktop-portal — and the portal didn't answer ({e}). Install and \
         start xdg-desktop-portal plus the backend for your desktop (for example \
         xdg-desktop-portal-gnome, -kde or -wlr), or capture from the tray menu instead."
    )
}

/// The message shown when the portal is there but refused the binding.
fn bind_refused(e: &ashpd::Error) -> String {
    format!(
        "your desktop refused the capture shortcut ({e}). Some desktops ask you to \
         assign it yourself — look for Kashot under Settings > Keyboard > Shortcuts. \
         The tray menu captures in the meantime."
    )
}
