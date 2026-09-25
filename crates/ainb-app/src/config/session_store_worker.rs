//! The one worker that writes the session store, for both hosts (P6e).
//!
//! One thread, not one per write: the writes are a queue, and two of them
//! running at once would race for the same `sessions.json` lock and land out
//! of order. A write can reach the hangar daemon and wait out its deadline,
//! so a host queues it here and hears about a failure later, as a
//! `persist_failed` report on the channel it handed in.

use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use crate::app::intent::Intent;
use crate::app::{Persist, reports};

/// The session-store worker, started on the first write.
pub struct SessionStoreWorker {
    /// Where a failed write is reported. The host reads it on its tick.
    reports: mpsc::Sender<Intent>,
    live: Option<Live>,
}

/// A running worker.
struct Live {
    /// Where a queued write goes. Dropping it ends the worker's loop.
    work: mpsc::Sender<Persist>,
    /// The worker sends once here when its queue is empty and it is leaving,
    /// which is what a finish waits on: a `JoinHandle` has no bounded wait.
    done: mpsc::Receiver<()>,
    handle: thread::JoinHandle<()>,
    /// Writes queued and not yet run. The worker holds the lock while it
    /// reports a failure, so a finish that counts under it sees each write
    /// once: still queued, or reported.
    queued: Arc<Mutex<usize>>,
}

impl SessionStoreWorker {
    /// A worker that reports failed writes on `reports`. No thread starts
    /// until the first write.
    #[must_use]
    pub fn new(reports: mpsc::Sender<Intent>) -> Self {
        Self {
            reports,
            live: None,
        }
    }

    /// Queue one write, starting the worker if there is none. `Some` is the
    /// failure report for a write that will never run.
    pub fn queue(&mut self, store: Persist) -> Option<Intent> {
        let store_id = store.store_id();
        let mut store = store;
        // Two turns at most: the first send can find a worker that is gone (a
        // panic inside a write ends the thread and drops the queue), and a
        // dead slot left in place would fail this write and every write after
        // it. The second turn is against a worker started here.
        for attempt in 0..2 {
            if self.live.is_none() {
                match self.start() {
                    Ok(live) => self.live = Some(live),
                    Err(error) => {
                        return Some(reports::persist_failed(
                            store_id,
                            &format!("the worker did not start: {error}"),
                        ));
                    }
                }
            }
            let live = self.live.as_ref()?;
            // Counted before the send, so the worker never runs a write it
            // cannot subtract.
            *lock(&live.queued) += 1;
            match live.work.send(store) {
                Ok(()) => return None,
                Err(error) => {
                    *lock(&live.queued) -= 1;
                    // The worker is gone. Clear the slot so the next turn, and
                    // every later write, starts a new one.
                    self.live = None;
                    if attempt == 1 {
                        return Some(reports::persist_failed(
                            store_id,
                            &format!("the session store worker is gone: {error}"),
                        ));
                    }
                    tracing::warn!("the session store worker was gone; starting another");
                    store = error.0;
                }
            }
        }
        None
    }

    fn start(&self) -> std::io::Result<Live> {
        let (work, work_rx) = mpsc::channel::<Persist>();
        let (done_tx, done) = mpsc::channel::<()>();
        let report_tx = self.reports.clone();
        let queued = Arc::new(Mutex::new(0));
        let counted = Arc::clone(&queued);
        let handle =
            thread::Builder::new().name("ainb-session-store-write".into()).spawn(move || {
                // In order, one at a time, until the sender is dropped.
                for store in work_rx {
                    let result = crate::config::persist::write(&store);
                    let mut pending = lock(&counted);
                    if let Err(error) = result {
                        let _ = report_tx.send(reports::persist_failed(store.store_id(), &error));
                    }
                    *pending -= 1;
                }
                // The queue is empty and the worker is leaving: a finish that
                // is waiting can stop waiting.
                let _ = done_tx.send(());
            })?;
        Ok(Live {
            work,
            done,
            handle,
            queued,
        })
    }

    /// Wait for every queued write, up to `within` for all of them together,
    /// and answer with the number not written: still queued when the bound
    /// ran out, or failed with its report still unread on `inbox`, the
    /// receiving end of the channel this worker reports on.
    ///
    /// A host calls this on its way out, once its loop has stopped reading
    /// reports, so an unread failure is a change the operator never heard
    /// about. The reports are counted, logged and put back, not consumed.
    /// Safe to call when no write ever ran; a later write starts the worker
    /// again.
    pub fn finish(&mut self, within: Duration, inbox: &mpsc::Receiver<Intent>) -> usize {
        let Some(Live {
            work,
            done,
            handle,
            queued,
        }) = self.live.take()
        else {
            return self.count_unread_failures(inbox);
        };
        // The worker's loop ends when the last sender goes, and only then
        // does it say it is done.
        drop(work);
        // Past the bound the writes that are left are left: the worker is
        // blocked on a lock or a daemon, and the exit does not wait on it.
        if done.recv_timeout(within).is_ok() {
            let _ = handle.join();
        }
        // Counted under the lock, so a write failing right now is counted
        // once: still queued, or reported.
        let pending = lock(&queued);
        *pending + self.count_unread_failures(inbox)
    }

    /// The `persist_failed` reports waiting on `inbox`, each logged with its
    /// error. Every report goes back on the channel, in order.
    fn count_unread_failures(&self, inbox: &mpsc::Receiver<Intent>) -> usize {
        let unread: Vec<Intent> = inbox.try_iter().collect();
        let mut failed = 0;
        for report in unread {
            if let Intent::Command(id, args) = &report {
                if id.as_str() == reports::ids::PERSIST_FAILED {
                    failed += 1;
                    tracing::warn!(
                        store = %args["store"],
                        error = %args["error"],
                        "a session-store write failed and no screen was left to say so"
                    );
                }
            }
            let _ = self.reports.send(report);
        }
        failed
    }

    /// Leave the slot holding a sender nothing reads, as a worker that
    /// panicked inside a write leaves it. The next queued write has to notice
    /// and start another worker.
    ///
    /// The live worker is drained and let go first, so the writes queued
    /// before this and the writes queued after it never run at the same time.
    #[doc(hidden)]
    pub fn break_for_tests(&mut self, inbox: &mpsc::Receiver<Intent>) {
        self.finish(Duration::from_secs(5), inbox);
        let (work, unread) = mpsc::channel();
        drop(unread);
        let (never, done) = mpsc::channel();
        drop(never);
        self.live = Some(Live {
            work,
            done,
            handle: thread::spawn(|| {}),
            queued: Arc::default(),
        });
    }
}

/// The count, even from a worker that panicked holding it: a count is still
/// worth reading.
fn lock(queued: &Mutex<usize>) -> std::sync::MutexGuard<'_, usize> {
    queued.lock().unwrap_or_else(PoisonError::into_inner)
}
