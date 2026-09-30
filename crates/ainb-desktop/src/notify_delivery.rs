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
pub const MAX_WAITERS: usize = 8;

/// The platform's notification service, through `notify-rust`.
#[derive(Debug, Default, Clone)]
pub struct OsDelivery {
    waiting: Arc<AtomicUsize>,
}

impl OsDelivery {
    /// How many notices are waiting for a click now.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
}

impl Deliver for OsDelivery {
    fn deliver(&self, notice: Notice, clicked: Click) {
        let waiting = Arc::clone(&self.waiting);
        let spawned = std::thread::Builder::new()
            .name("os-notice".to_string())
            .spawn(move || send(&notice, &waiting, clicked));
        if let Err(error) = spawned {
            tracing::warn!(%error, "OS notification not sent: no thread to send it on");
        }
    }
}

/// Send `notice`, log what the OS answered, and wait for its click in one of
/// the [`MAX_WAITERS`] slots when one is free.
fn send(notice: &Notice, waiting: &AtomicUsize, clicked: Click) {
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
            handle
        }
        Err(error) => {
            tracing::warn!(session, %error, "OS notification not delivered");
            return;
        }
    };
    if waiting.fetch_add(1, Ordering::SeqCst) >= MAX_WAITERS {
        waiting.fetch_sub(1, Ordering::SeqCst);
        drop(handle);
        tracing::info!(
            session,
            "OS notification sent without waiting for its click: {MAX_WAITERS} already wait"
        );
        return;
    }
    let waited = handle.wait_for_response(move |response: &notify_rust::NotificationResponse| {
        if matches!(response, notify_rust::NotificationResponse::Default) {
            clicked();
        }
    });
    waiting.fetch_sub(1, Ordering::SeqCst);
    match waited {
        Ok(()) => {
            #[cfg(target_os = "macos")]
            tracing::info!(session, "OS notification delivered");
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
}
