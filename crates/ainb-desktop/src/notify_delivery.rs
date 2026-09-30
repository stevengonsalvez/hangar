//! Showing a notice on the OS and hearing its click (`notify.rs` decides
//! which notices there are).
//!
//! The window sends through [`OsDelivery`]: `notify-rust`, straight to the
//! platform (D-Bus on Linux, the notification centre on macOS). The Tauri
//! notification plugin drops the handle a click arrives on, so a click on its
//! banner could never reach the window.
//!
//! The two platforms answer very differently, so each gets its own design:
//!
//! - **Linux** sends over D-Bus, and a click or close arrives later as its
//!   own `ActionInvoked`/`NotificationClosed` signal naming the notification
//!   by its D-Bus id. One shared listener (one thread, one connection,
//!   started the first time a notice is sent) owns that signal stream for
//!   the life of an [`OsDelivery`] and dispatches to whichever notice's id it
//!   names. Sending a notice is: call `show()`, note the id it returns, hand
//!   the click to the listener's map. No thread, wait, or connection per
//!   notice; see the [`linux`] module.
//! - **macOS**'s `notify-rust` handle is lazy: `show()` only prepares the
//!   notification locally, and the actual, synchronous call to the
//!   notification centre is inside `wait_for_response`. So macOS still runs
//!   one thread per notice, holding a counted slot (see [`MAX_WAITERS`]) for
//!   as long as that call runs; see the [`macos`] module.
//!
//! ```text
//!  Linux:  Notice ──announce──▶ show() ──id──▶ listener's map ──signal──▶ open(session)
//!  macOS:  Notice ──announce──▶ thread ──wait_for_response (the actual send)──▶ open(session)
//! ```

use std::sync::Arc;

use crate::notify::Notice;

/// What a click on a notice runs: the session it names, `None` for a summary.
pub type Open = Arc<dyn Fn(Option<String>) + Send + Sync>;

/// One notice's click: run at most once, on any thread.
pub type Click = Box<dyn FnOnce() + Send>;

/// Where a notice goes: the OS in the window, a recorder in tests.
pub trait Deliver {
    /// Show `notice`. `clicked` runs, at most once and on any thread, when the
    /// person clicks it.
    fn deliver(&self, notice: Notice, clicked: Click);
}

/// Hand every notice to `os`; a click on one runs `open` with its session.
pub fn announce(os: &dyn Deliver, notices: Vec<Notice>, open: &Open) {
    for notice in notices {
        let open = Arc::clone(open);
        let session = notice.session_key.clone();
        os.deliver(notice, Box::new(move || open(session)));
    }
}

/// The most notices whose click is tracked at once: on Linux, entries in the
/// shared listener's id map (see [`linux::Listener`]); on macOS, threads each
/// holding a counted slot while `wait_for_response` runs. A notice past the
/// cap is still sent; its click is just not tracked.
pub const MAX_WAITERS: usize = 64;

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

/// The platform's notification service, through `notify-rust`.
#[derive(Clone)]
pub struct OsDelivery {
    #[cfg(all(unix, not(target_os = "macos")))]
    linux: Arc<linux::Listener>,
    #[cfg(target_os = "macos")]
    waiting: Arc<std::sync::atomic::AtomicUsize>,
}

impl std::fmt::Debug for OsDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OsDelivery").field("waiting", &self.waiting()).finish()
    }
}

impl Default for OsDelivery {
    fn default() -> Self {
        Self {
            #[cfg(all(unix, not(target_os = "macos")))]
            linux: linux::Listener::new(),
            #[cfg(target_os = "macos")]
            waiting: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}

impl OsDelivery {
    /// How many notices' clicks are tracked right now: pending id -> click
    /// entries on Linux, threads still waiting on macOS.
    #[must_use]
    pub fn waiting(&self) -> usize {
        #[cfg(all(unix, not(target_os = "macos")))]
        return self.linux.waiting();
        #[cfg(target_os = "macos")]
        return self.waiting.load(std::sync::atomic::Ordering::SeqCst);
    }

    /// Test-only (Linux): entries older than `pending_ttl` are swept away as
    /// abandoned, and the sweep runs every `sweep_interval`, instead of the
    /// real (much longer) bounds, so a test can prove the sweep without
    /// waiting for them.
    #[cfg(all(unix, not(target_os = "macos"), any(test, feature = "test-seams")))]
    #[must_use]
    pub fn with_linux_pending_ttl(
        pending_ttl: std::time::Duration,
        sweep_interval: std::time::Duration,
    ) -> Self {
        Self {
            linux: linux::Listener::with_bounds(pending_ttl, sweep_interval),
        }
    }

    /// Test-only (Linux): how many times the shared listener has dialed a
    /// session bus connection, so a test can prove one notice, or a burst of
    /// them, shares a single connection, and that a dropped connection is
    /// re-dialed rather than left dead.
    #[cfg(all(unix, not(target_os = "macos"), any(test, feature = "test-seams")))]
    #[must_use]
    pub fn linux_connect_attempts(&self) -> usize {
        self.linux.connect_attempts()
    }
}

impl Deliver for OsDelivery {
    #[cfg(all(unix, not(target_os = "macos")))]
    fn deliver(&self, notice: Notice, clicked: Click) {
        linux::send(&notice, &self.linux, clicked);
    }

    #[cfg(target_os = "macos")]
    fn deliver(&self, notice: Notice, clicked: Click) {
        let waiting = Arc::clone(&self.waiting);
        let spawned = std::thread::Builder::new()
            .name("os-notice".to_string())
            .spawn(move || macos::send(&notice, &waiting, clicked));
        if let Err(error) = spawned {
            tracing::warn!(%error, "OS notification not sent: no thread to send it on");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::notify::NoticeKind;

    /// Records each notice and keeps its click, for the test to press.
    #[derive(Default)]
    struct Fake {
        shown: Mutex<Vec<(Notice, Click)>>,
    }

    impl Deliver for Fake {
        fn deliver(&self, notice: Notice, clicked: Click) {
            self.shown.lock().unwrap().push((notice, clicked));
        }
    }

    fn notice(kind: NoticeKind, session: Option<&str>) -> Notice {
        Notice {
            kind,
            session_key: session.map(str::to_string),
            title: "t".to_string(),
            body: "b".to_string(),
        }
    }

    #[test]
    fn a_click_opens_the_session_its_notice_names() {
        let fake = Fake::default();
        let opened = Arc::new(Mutex::new(Vec::new()));
        let open: Open = {
            let opened = Arc::clone(&opened);
            Arc::new(move |session| opened.lock().unwrap().push(session))
        };
        announce(
            &fake,
            vec![
                notice(NoticeKind::Needs, Some("claude:a")),
                notice(NoticeKind::Done, Some("claude:b")),
                notice(NoticeKind::Summary, None),
            ],
            &open,
        );
        let shown = std::mem::take(&mut *fake.shown.lock().unwrap());
        assert_eq!(shown.len(), 3, "every notice is handed to the OS");
        assert!(opened.lock().unwrap().is_empty(), "nothing opens unclicked");
        for (_, clicked) in shown.into_iter().rev() {
            clicked();
        }
        assert_eq!(
            *opened.lock().unwrap(),
            [
                None,
                Some("claude:b".to_string()),
                Some("claude:a".to_string())
            ]
        );
    }

    // Everything platform-specific (the Linux shared listener's connection
    // lifecycle and TTL sweep, the macOS waiter guard and its cap) has its
    // own unit tests in its module, plus the real-D-Bus end-to-end proof in
    // `tests/notify_click_dbus.rs`.
}
