//! macOS's `notify-rust` handle is lazy: `Notification::show()` only builds
//! a local `NotificationHandle` wrapping the notification (see
//! `notify-rust`'s `macos::nsusernotifications::NotificationHandle::new`); it
//! calls nothing on the OS. The actual, synchronous send
//! (`mac_notification_sys::send_notification`, called from inside
//! `wait_for_response`/`wait_for_action`/`on_close`) only happens once
//! something starts waiting for a response. So there is no "sent" state
//! before that wait begins, and if the cap below is hit and the handle is
//! dropped without ever waiting, the notice is never sent at all, not merely
//! untracked.
//!
//! That is why this still runs one thread per notice, holding a counted
//! slot in `waiting` for as long as `wait_for_response` runs: the wait *is*
//! the send here, unlike Linux where `show()` alone reaches the D-Bus
//! notification daemon (see the sibling `linux` module).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Click, MAX_WAITERS};
use crate::notify::Notice;

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

/// Sends `notice`. `show()` only prepares it locally (see the module doc);
/// the real send is the blocking `wait_for_response` call below, which is
/// why a notice past [`MAX_WAITERS`] is not sent at all, unlike Linux.
pub(super) fn send(notice: &Notice, waiting: &AtomicUsize, clicked: Click) {
    let session = notice.session_key.as_deref().unwrap_or("summary");
    let mut notification = notify_rust::Notification::new();
    notification.summary(&notice.title).body(&notice.body).appname("ainb");
    let handle = match notification.show() {
        Ok(handle) => {
            tracing::debug!(
                session,
                "OS notification prepared: notify-rust sends it once a wait for its response begins"
            );
            handle
        }
        Err(error) => {
            tracing::warn!(session, %error, "OS notification not prepared");
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
            "OS notification not sent: {MAX_WAITERS} already wait, and this platform only sends while waiting for a response"
        );
        return;
    }
    let waited = handle.wait_for_response(move |response: &notify_rust::NotificationResponse| {
        if matches!(response, notify_rust::NotificationResponse::Default) {
            clicked();
        }
    });
    drop(guard);
    match waited {
        Ok(()) => {
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::WaiterGuard;

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
        // Mirrors the cap-hit branch in `send`: it returns while a guard
        // from `WaiterGuard::claim` is still in scope.
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
}
