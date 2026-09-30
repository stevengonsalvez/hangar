//! The shared D-Bus notification-click listener.
//!
//! A notice's click or close arrives as its own `ActionInvoked` /
//! `NotificationClosed` signal on the session bus, naming the notification
//! by the id `Notify` returned when it was sent. One [`Listener`], started
//! the first time a notice is sent through it, owns a single session-bus
//! connection and a single pair of match rules for the life of an
//! [`super::OsDelivery`]; sending a notice never opens a connection, spawns a
//! thread, or blocks waiting for anything beyond the `Notify` call itself
//! (see [`send`]).
//!
//! ```text
//!  send() ──Notify──▶ id ──insert──▶ pending map
//!  listener thread ──ActionInvoked/NotificationClosed(id)──▶ pending map ──remove+call──▶ click
//! ```
//!
//! An entry with no signal ever coming (a crashed or hung notification
//! daemon) would otherwise sit in the map forever; a second, tiny thread
//! sweeps entries past [`PENDING_TTL`] every [`SWEEP_INTERVAL`]. If the
//! listener's connection ends (the notification daemon, and with it the
//! session bus's memory of this connection's match rules, restarted), the
//! listener re-dials with a backoff instead of going silently dead.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::{Click, MAX_WAITERS};
use crate::notify::Notice;

/// How long an unregistered-click entry stays pending before the sweep
/// drops it as abandoned. Repurposed from this module's earlier per-notice
/// wait timeout (same ten minutes): it used to bound how long a thread
/// blocked, now it bounds how long an entry sits in the shared map.
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);

/// How often the sweep looks for entries past [`PENDING_TTL`].
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// The bus interface every notification server implements.
const NOTIFICATION_INTERFACE: &str = "org.freedesktop.Notifications";

/// The longest backoff between reconnect attempts after the listener's
/// connection ends.
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(30);

struct Entry {
    clicked: Click,
    inserted_at: Instant,
}

/// One shared D-Bus listener: one connection, one pair of match rules, for
/// the life of an [`super::OsDelivery`]. Started lazily on the first notice
/// sent through it (not at construction), so a delivery target that never
/// sends a notice never dials a bus.
pub(super) struct Listener {
    pending: Mutex<HashMap<u32, Entry>>,
    started: OnceLock<()>,
    pending_ttl: Duration,
    sweep_interval: Duration,
    connect_attempts: AtomicUsize,
}

impl Listener {
    pub(super) fn new() -> Arc<Self> {
        Self::with_bounds(PENDING_TTL, SWEEP_INTERVAL)
    }

    pub(super) fn with_bounds(pending_ttl: Duration, sweep_interval: Duration) -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(HashMap::new()),
            started: OnceLock::new(),
            pending_ttl,
            sweep_interval,
            connect_attempts: AtomicUsize::new(0),
        })
    }

    pub(super) fn waiting(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    pub(super) fn connect_attempts(&self) -> usize {
        self.connect_attempts.load(Ordering::SeqCst)
    }

    /// Registers `id`'s click, starting the listener on first use. Returns
    /// `false` without registering if [`MAX_WAITERS`] entries are already
    /// pending.
    fn register(self: &Arc<Self>, id: u32, clicked: Click) -> bool {
        self.ensure_started();
        self.try_insert(id, clicked)
    }

    /// The cap-check-and-insert on its own, without starting the listener,
    /// so a unit test can prove the cap without dialing a real bus.
    fn try_insert(&self, id: u32, clicked: Click) -> bool {
        let mut pending = self.pending.lock().unwrap();
        if pending.len() >= MAX_WAITERS {
            return false;
        }
        pending.insert(
            id,
            Entry {
                clicked,
                inserted_at: Instant::now(),
            },
        );
        true
    }

    fn ensure_started(self: &Arc<Self>) {
        self.started.get_or_init(|| {
            spawn_named("os-notice-listener", {
                let listener = Arc::clone(self);
                move || run_listener(&listener)
            });
            spawn_named("os-notice-sweep", {
                let listener = Arc::clone(self);
                move || run_sweeper(&listener)
            });
        });
    }

    fn sweep(&self) {
        let ttl = self.pending_ttl;
        self.pending
            .lock()
            .unwrap()
            .retain(|_, entry| entry.inserted_at.elapsed() < ttl);
    }

    /// Resolves `id`, if it is still pending, removing the entry so a later
    /// duplicate signal for the same id cannot look it up (and run its
    /// click) again. `run_click` is `false` for a close with no action.
    fn resolve(&self, id: u32, run_click: bool) {
        let entry = self.pending.lock().unwrap().remove(&id);
        if let (Some(entry), true) = (entry, run_click) {
            (entry.clicked)();
        }
    }
}

fn spawn_named(name: &str, work: impl FnOnce() + Send + 'static) {
    let spawned = std::thread::Builder::new().name(name.to_string()).spawn(work);
    if let Err(error) = spawned {
        tracing::warn!(%error, thread = name, "OS notification listener thread not started");
    }
}

fn run_sweeper(listener: &Arc<Listener>) {
    loop {
        std::thread::sleep(listener.sweep_interval);
        listener.sweep();
    }
}

/// Runs until the process exits: connects, subscribes to both signals, and
/// dispatches by id. Reconnects with an increasing backoff (capped at
/// [`MAX_RECONNECT_BACKOFF`]) if the connection ends, resetting the backoff
/// after a connection that ran long enough to be a real session rather than
/// a bus that immediately refused it.
fn run_listener(listener: &Arc<Listener>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let connected_at = Instant::now();
        if let Err(error) = listen_once(listener) {
            tracing::warn!(
                %error,
                "OS notification click listener lost its session bus connection, reconnecting"
            );
        }
        backoff = if connected_at.elapsed() >= SWEEP_INTERVAL {
            Duration::from_secs(1)
        } else {
            (backoff * 2).min(MAX_RECONNECT_BACKOFF)
        };
        std::thread::sleep(backoff);
    }
}

fn listen_once(listener: &Arc<Listener>) -> zbus::Result<()> {
    listener.connect_attempts.fetch_add(1, Ordering::SeqCst);
    let connection = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::fdo::DBusProxy::new(&connection)?;
    proxy.add_match_rule(signal_rule("ActionInvoked")?)?;
    proxy.add_match_rule(signal_rule("NotificationClosed")?)?;
    for message in zbus::blocking::MessageIterator::from(&connection) {
        dispatch(listener, &message?);
    }
    Ok(())
}

fn signal_rule(member: &'static str) -> zbus::Result<zbus::MatchRule<'static>> {
    Ok(zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(NOTIFICATION_INTERFACE)?
        .member(member)?
        .build())
}

fn dispatch(listener: &Arc<Listener>, message: &zbus::message::Message) {
    let header = message.header();
    if !matches!(header.message_type(), zbus::message::Type::Signal) {
        return;
    }
    match header.member() {
        Some(name) if name == "ActionInvoked" => {
            if let Ok((id, action)) = message.body().deserialize::<(u32, String)>() {
                listener.resolve(id, action == "default");
            }
        }
        Some(name) if name == "NotificationClosed" => {
            if let Ok((id, _reason)) = message.body().deserialize::<(u32, u32)>() {
                listener.resolve(id, false);
            }
        }
        _ => {}
    }
}

/// Sends `notice` and hands its click to the shared listener. `show()` is
/// the whole send here (D-Bus's `Notify` call and its reply are both
/// synchronous), so this never waits beyond that one round trip; the click
/// itself is heard later, by the listener, not by this call. Past
/// [`MAX_WAITERS`] pending entries the notice is still sent, just untracked.
pub(super) fn send(notice: &Notice, listener: &Arc<Listener>, clicked: Click) {
    let session = notice.session_key.as_deref().unwrap_or("summary");
    let mut notification = notify_rust::Notification::new();
    notification.summary(&notice.title).body(&notice.body).appname("ainb");
    // A click on the banner is the `default` action on D-Bus.
    notification.action("default", "Open");
    let handle = match notification.show() {
        Ok(handle) => {
            tracing::info!(session, "OS notification delivered");
            handle
        }
        Err(error) => {
            tracing::warn!(session, %error, "OS notification not delivered");
            return;
        }
    };
    let id = handle.id();
    // The listener owns the click from here; nothing in this call waits on
    // it, so the connection notify-rust made to send it can close now.
    drop(handle);
    if !listener.register(id, clicked) {
        tracing::warn!(
            session,
            waiting = listener.waiting(),
            "OS notification sent without waiting for its click: {MAX_WAITERS} already wait"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn a_resolved_id_cannot_be_resolved_again() {
        let listener = Listener::new();
        let calls = Arc::new(Mutex::new(0));
        {
            let calls = Arc::clone(&calls);
            listener.pending.lock().unwrap().insert(
                7,
                Entry {
                    clicked: Box::new(move || *calls.lock().unwrap() += 1),
                    inserted_at: Instant::now(),
                },
            );
        }
        listener.resolve(7, true);
        listener.resolve(7, true);
        assert_eq!(
            *calls.lock().unwrap(),
            1,
            "a duplicate signal for the same id must not re-fire the click"
        );
        assert_eq!(
            listener.waiting(),
            0,
            "the entry is gone after its first resolution"
        );
    }

    #[test]
    fn a_close_without_an_action_does_not_click() {
        let listener = Listener::new();
        let clicked = Arc::new(Mutex::new(false));
        {
            let clicked = Arc::clone(&clicked);
            listener.pending.lock().unwrap().insert(
                3,
                Entry {
                    clicked: Box::new(move || *clicked.lock().unwrap() = true),
                    inserted_at: Instant::now(),
                },
            );
        }
        listener.resolve(3, false);
        assert!(!*clicked.lock().unwrap(), "a plain close is not a click");
        assert_eq!(listener.waiting(), 0, "the entry is still removed");
    }

    #[test]
    fn the_sweep_drops_only_entries_past_the_ttl() {
        let listener = Listener::with_bounds(Duration::from_millis(20), Duration::from_secs(3600));
        listener.pending.lock().unwrap().insert(
            1,
            Entry {
                clicked: Box::new(|| {}),
                inserted_at: Instant::now(),
            },
        );
        assert_eq!(listener.waiting(), 1);
        std::thread::sleep(Duration::from_millis(50));
        listener.sweep();
        assert_eq!(
            listener.waiting(),
            0,
            "an entry past its TTL is swept even with no signal"
        );
    }

    #[test]
    fn a_fresh_entry_survives_a_sweep_within_its_ttl() {
        let listener = Listener::with_bounds(Duration::from_secs(3600), Duration::from_secs(3600));
        listener.pending.lock().unwrap().insert(
            1,
            Entry {
                clicked: Box::new(|| {}),
                inserted_at: Instant::now(),
            },
        );
        listener.sweep();
        assert_eq!(
            listener.waiting(),
            1,
            "a sweep must not drop an entry still within its TTL"
        );
    }

    #[test]
    fn inserting_past_the_cap_is_rejected_without_dialing_a_bus() {
        let listener = Listener::new();
        for id in 0..MAX_WAITERS as u32 {
            assert!(
                listener.try_insert(id, Box::new(|| {})),
                "room for entry {id}"
            );
        }
        assert!(
            !listener.try_insert(9999, Box::new(|| {})),
            "the cap is already full"
        );
        assert_eq!(
            listener.waiting(),
            MAX_WAITERS,
            "the rejected entry is not inserted"
        );
    }
}
