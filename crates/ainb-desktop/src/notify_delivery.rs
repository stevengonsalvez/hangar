//! Showing a notice on the OS and hearing its click (`notify.rs` decides
//! which notices there are).
//!
//! The window sends through [`OsDelivery`]: `notify-rust`, straight to the
//! platform (D-Bus on Linux, the notification centre on macOS). The Tauri
//! notification plugin drops the handle a click arrives on, so a click on its
//! banner could never reach the window. Each notice gets a short-lived thread
//! of its own: it sends, logs what the OS answered, and waits for the click or
//! the close. At most [`MAX_WAITERS`] wait at once; a notice past them is still
//! sent, without a click.
//!
//! ```text
//!  Notice ──announce──▶ Deliver::deliver ──thread──▶ OS ──click──▶ open(session)
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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

/// The most notices waiting for a click at once: each holds a thread until
/// the OS reports the click or the close.
pub const MAX_WAITERS: usize = 64;

/// How long a waiter gives an unresponsive OS notification server before
/// giving up on that one notice's click, on platforms where the wait has no
/// deadline of its own (Linux/D-Bus). `ActionInvoked`/`NotificationClosed`
/// never arrive for a crashed or hung notification daemon, so without a
/// bound here that notice's thread, and the waiter slot it holds, would
/// never be released; see [`wait_for_click`] for the platform split.
const WAIT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The platform's notification service, through `notify-rust`.
#[derive(Debug, Clone)]
pub struct OsDelivery {
    waiting: Arc<AtomicUsize>,
    wait_timeout: Duration,
}

impl Default for OsDelivery {
    fn default() -> Self {
        Self {
            waiting: Arc::new(AtomicUsize::new(0)),
            wait_timeout: WAIT_TIMEOUT,
        }
    }
}

impl OsDelivery {
    /// How many notices are waiting for a click now.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }

    /// Test-only: a stuck wait gives up after `timeout` instead of
    /// [`WAIT_TIMEOUT`], so a test can prove the bound without waiting for
    /// the real one.
    #[cfg(any(test, feature = "test-seams"))]
    #[must_use]
    pub fn with_wait_timeout(timeout: Duration) -> Self {
        Self {
            wait_timeout: timeout,
            ..Self::default()
        }
    }
}

impl Deliver for OsDelivery {
    fn deliver(&self, notice: Notice, clicked: Click) {
        let waiting = Arc::clone(&self.waiting);
        let wait_timeout = self.wait_timeout;
        let spawned = std::thread::Builder::new()
            .name("os-notice".to_string())
            .spawn(move || send(&notice, &waiting, clicked, wait_timeout));
        if let Err(error) = spawned {
            tracing::warn!(%error, "OS notification not sent: no thread to send it on");
        }
    }
}

/// Releases one counted waiter slot on drop: a normal return, an early
/// return past [`MAX_WAITERS`], or a panic unwinding through the wait all go
/// through the same release, so there is exactly one place that frees a
/// claimed slot.
struct WaiterGuard<'a> {
    waiting: &'a AtomicUsize,
}

impl<'a> WaiterGuard<'a> {
    /// Claims a slot, returning the guard and how many were already claimed
    /// (before this one) so the caller can compare against [`MAX_WAITERS`].
    fn claim(waiting: &'a AtomicUsize) -> (Self, usize) {
        let already_waiting = waiting.fetch_add(1, Ordering::SeqCst);
        (Self { waiting }, already_waiting)
    }
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        self.waiting.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A caller gave up waiting for `work`, run on its own thread, before it
/// finished.
struct TimedOut;

/// Runs `work` on a thread named `thread_name` and waits at most `timeout`
/// for it to finish.
///
/// On timeout the thread is abandoned, not cancelled: nothing in
/// `notify-rust`'s blocking API lets us interrupt a call already parked on
/// the OS notification server, so a notice whose server never answers still
/// leaks one OS thread. What this bounds is the caller's own accounting (a
/// waiter slot held by [`WaiterGuard`]) so a single stuck notice cannot make
/// every later notice silently unclickable too. A late, genuine response
/// `work` produces after the timeout is not lost: it still runs, on its own
/// thread, so a click sent by the server a moment after we stopped waiting
/// still reaches the caller.
fn bounded_wait<T: Send + 'static>(
    timeout: Duration,
    thread_name: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, TimedOut> {
    let (sent, received) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name(thread_name.to_string()).spawn(move || {
        let _ = sent.send(work());
    });
    if spawned.is_err() {
        return Err(TimedOut);
    }
    received.recv_timeout(timeout).map_err(|_| TimedOut)
}

/// Waits for `handle`'s click or close, running `clicked` on a click.
///
/// On Linux the D-Bus signal this waits on carries no deadline, so the wait
/// is bounded by `wait_timeout` (see [`bounded_wait`]). macOS's
/// `wait_for_response` already returns once the notification center has
/// closed the banner, so it is not further bounded here.
#[cfg(all(unix, not(target_os = "macos")))]
fn wait_for_click(
    handle: notify_rust::NotificationHandle,
    clicked: Click,
    wait_timeout: Duration,
) -> notify_rust::error::Result<()> {
    let waited = bounded_wait(wait_timeout, "os-notice-wait", move || {
        handle.wait_for_response(move |response: &notify_rust::NotificationResponse| {
            if matches!(response, notify_rust::NotificationResponse::Default) {
                clicked();
            }
        })
    });
    match waited {
        Ok(result) => result,
        Err(TimedOut) => Err(
            "OS notification click wait timed out: the notification server never answered".into(),
        ),
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn wait_for_click(
    handle: notify_rust::NotificationHandle,
    clicked: Click,
    _wait_timeout: Duration,
) -> notify_rust::error::Result<()> {
    handle.wait_for_response(move |response: &notify_rust::NotificationResponse| {
        if matches!(response, notify_rust::NotificationResponse::Default) {
            clicked();
        }
    })
}

/// Send `notice`, log what the OS answered, and wait for its click in one of
/// the [`MAX_WAITERS`] slots when one is free.
fn send(notice: &Notice, waiting: &AtomicUsize, clicked: Click, wait_timeout: Duration) {
    let session = notice.session_key.as_deref().unwrap_or("summary");
    let mut notification = notify_rust::Notification::new();
    notification.summary(&notice.title).body(&notice.body).appname("ainb");
    // A click on the banner is the `default` action on D-Bus. macOS reports
    // it without one, and a named action there would draw a button.
    #[cfg(not(target_os = "macos"))]
    notification.action("default", "Open");
    // On D-Bus `show` is the send, and its result is the server's answer. On
    // macOS it only queues the notice: the send is `wait_for_response` below,
    // or the handle's drop, and that is where its result is.
    let handle = match notification.show() {
        Ok(handle) => {
            #[cfg(not(target_os = "macos"))]
            tracing::info!(session, "OS notification delivered");
            #[cfg(target_os = "macos")]
            tracing::info!(
                session,
                "OS notification queued with the notification center"
            );
            handle
        }
        Err(error) => {
            tracing::warn!(session, %error, "OS notification not delivered");
            return;
        }
    };
    let (guard, already_waiting) = WaiterGuard::claim(waiting);
    if already_waiting >= MAX_WAITERS {
        drop(guard);
        drop(handle);
        tracing::warn!(
            session,
            waiting = already_waiting,
            "OS notification sent without waiting for its click: {MAX_WAITERS} already wait"
        );
        return;
    }
    let waited = wait_for_click(handle, clicked, wait_timeout);
    drop(guard);
    match waited {
        Ok(()) => {
            #[cfg(target_os = "macos")]
            tracing::info!(
                session,
                "OS notification delivered: the notification center confirmed the send"
            );
            tracing::debug!(session, "OS notification clicked or closed");
        }
        Err(error) => tracing::warn!(session, %error, "OS notification not delivered"),
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

    #[test]
    fn a_claimed_waiter_guard_releases_its_slot_on_normal_drop() {
        let waiting = AtomicUsize::new(0);
        {
            let (_guard, already_waiting) = WaiterGuard::claim(&waiting);
            assert_eq!(
                already_waiting, 0,
                "the first claim sees nobody ahead of it"
            );
            assert_eq!(
                waiting.load(Ordering::SeqCst),
                1,
                "the claim is counted while held"
            );
        }
        assert_eq!(
            waiting.load(Ordering::SeqCst),
            0,
            "dropping the guard frees its slot"
        );
    }

    #[test]
    fn a_claimed_waiter_guard_releases_its_slot_on_an_early_return() {
        let waiting = AtomicUsize::new(0);
        // Mirrors the cap-hit branch in `send`: it returns early while a
        // guard from `WaiterGuard::claim` is still in scope.
        fn claim_then_bail(waiting: &AtomicUsize) {
            let (_guard, _already_waiting) = WaiterGuard::claim(waiting);
        }
        claim_then_bail(&waiting);
        assert_eq!(
            waiting.load(Ordering::SeqCst),
            0,
            "an early return still drops the guard"
        );
    }

    #[test]
    fn a_claimed_waiter_guard_releases_its_slot_on_a_panic() {
        let waiting = AtomicUsize::new(0);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (_guard, _already_waiting) = WaiterGuard::claim(&waiting);
            panic!("simulated failure while a waiter is held");
        }));
        assert!(
            result.is_err(),
            "the panic is expected to propagate out of catch_unwind"
        );
        assert_eq!(
            waiting.load(Ordering::SeqCst),
            0,
            "unwinding through the guard still runs its Drop and frees the slot"
        );
    }

    #[test]
    fn bounded_wait_returns_the_result_when_work_finishes_in_time() {
        let result = bounded_wait(Duration::from_secs(5), "bounded-wait-test-fast", || 42);
        assert_eq!(result.ok(), Some(42));
    }

    #[test]
    fn bounded_wait_times_out_on_a_stuck_worker_instead_of_blocking_forever() {
        // The worker sleeps far longer than the bound; a fixed `recv()` here
        // (no timeout) is exactly the bug this closes, and would hang this
        // test until the sandbox's own runner timeout killed it.
        let result = bounded_wait(Duration::from_millis(20), "bounded-wait-test-stuck", || {
            std::thread::sleep(Duration::from_secs(2));
            "too late"
        });
        assert!(
            result.is_err(),
            "a worker slower than the bound must not be waited for"
        );
    }

    // The cap-hit path's `tracing::warn!` is exercised end to end, against a
    // real (private, test-only) D-Bus notification daemon, in
    // `tests/notify_click_dbus.rs`: it drives `send()` past `MAX_WAITERS` and
    // asserts a `WARN` event naming the count. A unit test here would have
    // to re-invoke `notify_rust::Notification::show()` without an OS to
    // answer it, so it cannot reach that branch on its own.
}
