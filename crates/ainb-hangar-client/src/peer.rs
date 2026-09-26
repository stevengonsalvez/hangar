//! The off-box peer transport (R1-09), part 1: the reconnect rules every peer
//! connection follows, before any socket exists.
//!
//! ```text
//!  host lost ──▶ Backoff::next_delay() ──▶ redial ──ok──▶ ResyncGate::acquire(host, last_active)
//!                  equal jitter, 1 s..60 s                   at most 2 resyncs at once,
//!                  doubling per failure                      most recently active host first
//! ```
//!
//! - [`Backoff`] spaces redials of ONE host: each delay is drawn from the
//!   upper half of a window that doubles per failure (2 s, 4 s, 8 s, ... capped
//!   at 60 s), so every delay stays within 1 s and 60 s and a fleet of clients
//!   that lost a host together does not redial it in lock step. The local
//!   unix leg keeps its own fixed 1/4/16 s schedule in `reconnect.rs`.
//! - [`ResyncGate`] bounds how many hosts a client resyncs at once. A resync
//!   replays `fleet/subscribe { after_revision }`, which can be heavy after a
//!   long gap, so a client that comes back to ten hosts replays at most
//!   [`MAX_CONCURRENT_RESYNCS`] of them together, the host the user touched
//!   most recently first.
//!
//! The WS + Noise session itself (one multiplexed session per host) lands in
//! part 2, on top of `ainb-hangar-noise`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ainb_hangar_proto::hosts::HostId;
use tokio::sync::oneshot;

/// The shortest redial delay.
pub const BACKOFF_FLOOR: Duration = Duration::from_secs(1);
/// The longest redial delay.
pub const BACKOFF_CEILING: Duration = Duration::from_mins(1);
/// How many hosts one client resyncs at once.
pub const MAX_CONCURRENT_RESYNCS: usize = 2;

/// Redial spacing for one host: equal jitter over a doubling window.
///
/// Failure `n` (0-based) waits a uniform delay in `[w / 2, w]`, where
/// `w = min(60 s, 2 s * 2^n)`, clamped to `[1 s, 60 s]`.
#[derive(Debug, Clone)]
pub struct Backoff {
    failures: u32,
    rng: u64,
}

impl Backoff {
    /// A backoff with a fixed jitter seed (tests, or a caller with its own
    /// entropy).
    #[must_use]
    pub const fn with_seed(seed: u64) -> Self {
        // xorshift needs a non-zero state.
        Self {
            failures: 0,
            rng: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// A backoff with a fresh random seed, so two clients started together
    /// still draw different delays.
    #[must_use]
    pub fn new() -> Self {
        use std::hash::BuildHasher as _;
        // `RandomState` is seeded per instance from the OS: enough entropy to
        // spread redials, and no dependency.
        Self::with_seed(std::collections::hash_map::RandomState::new().hash_one(std::process::id()))
    }

    const fn next_random(&mut self) -> u64 {
        // xorshift64: enough to spread redials; not a security primitive.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    /// The window for the current failure count.
    #[must_use]
    pub fn window(&self) -> Duration {
        let doubled = BACKOFF_FLOOR
            .saturating_mul(2)
            .saturating_mul(2u32.saturating_pow(self.failures.min(16)));
        doubled.min(BACKOFF_CEILING)
    }

    /// Record one more failure and return how long to wait before the next
    /// dial.
    pub fn next_delay(&mut self) -> Duration {
        let window = self.window();
        let low = window / 2;
        let span = u64::try_from(window.saturating_sub(low).as_millis()).unwrap_or(u64::MAX);
        let offset = if span == 0 {
            0
        } else {
            self.next_random() % (span + 1)
        };
        self.failures = self.failures.saturating_add(1);
        (low + Duration::from_millis(offset)).clamp(BACKOFF_FLOOR, BACKOFF_CEILING)
    }

    /// The dial succeeded: start again from the shortest window.
    pub const fn reset(&mut self) {
        self.failures = 0;
    }

    /// Failures since the last success.
    #[must_use]
    pub const fn failures(&self) -> u32 {
        self.failures
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// Queue order: most recently active first, then first come.
type WaitKey = (std::cmp::Reverse<i64>, u64);

#[derive(Debug, Default)]
struct GateState {
    running: usize,
    next_ticket: u64,
    waiting: BTreeMap<WaitKey, (HostId, oneshot::Sender<ResyncPermit>)>,
}

/// At most `limit` resyncs at once, admitted most recently active first.
///
/// Cheap to clone: every clone shares one budget. The budget is per client,
/// so a surface holds one gate for all its hosts.
#[derive(Debug, Clone)]
pub struct ResyncGate {
    limit: usize,
    state: Arc<Mutex<GateState>>,
}

impl Default for ResyncGate {
    fn default() -> Self {
        Self::new(MAX_CONCURRENT_RESYNCS)
    }
}

impl ResyncGate {
    /// A gate that runs at most `limit` resyncs at once (at least one).
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            state: Arc::new(Mutex::new(GateState::default())),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wait for a resync slot for `host_id`, whose user last acted at
    /// `last_active_ms`. The slot is held until the permit drops. A caller
    /// that stops waiting (its future dropped) gives up its place, and a slot
    /// handed to it in that moment returns to the gate.
    pub async fn acquire(&self, host_id: HostId, last_active_ms: i64) -> ResyncPermit {
        let receiver = {
            let mut state = self.lock();
            if state.running < self.limit && state.waiting.is_empty() {
                state.running += 1;
                return ResyncPermit {
                    gate: Some(self.clone()),
                    host_id,
                };
            }
            let ticket = state.next_ticket;
            state.next_ticket += 1;
            let (sender, receiver) = oneshot::channel();
            state.waiting.insert(
                (std::cmp::Reverse(last_active_ms), ticket),
                (host_id, sender),
            );
            receiver
        };
        // `self` holds the gate for the whole wait, and only `release` takes
        // an entry out of the queue, sending on it as it does. A dropped
        // sender is a broken invariant: fail loudly rather than hand out an
        // uncounted permit labelled with the wrong host.
        receiver.await.unwrap_or_else(|_| {
            unreachable!("a resync waiter lost its sender while the gate lives")
        })
    }

    /// Resyncs holding a slot now.
    #[must_use]
    pub fn running(&self) -> usize {
        self.lock().running
    }

    /// Resyncs queued for a slot.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.lock().waiting.len()
    }

    /// A slot came free: hand it to the best waiter still listening.
    // The lock is held for the whole hand-out on purpose: releasing it
    // between waiters would let a new `acquire` jump the queue.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the lock is held across the hand-out so no acquire jumps the queue"
    )]
    fn release(&self) {
        let mut state = self.lock();
        state.running = state.running.saturating_sub(1);
        while state.running < self.limit {
            let Some((_, (host_id, sender))) = state.waiting.pop_first() else {
                break;
            };
            state.running += 1;
            let permit = ResyncPermit {
                gate: Some(self.clone()),
                host_id,
            };
            if let Err(mut unsent) = sender.send(permit) {
                // That waiter gave up; take the slot back without re-entering
                // this lock through the permit's drop.
                unsent.gate = None;
                state.running -= 1;
            }
        }
    }
}

/// One resync slot. Dropping it frees the slot for the next waiter.
#[derive(Debug)]
#[must_use = "the slot is freed as soon as the permit drops"]
pub struct ResyncPermit {
    gate: Option<ResyncGate>,
    host_id: HostId,
}

impl ResyncPermit {
    /// The host this slot was granted for.
    #[must_use]
    pub const fn host_id(&self) -> &HostId {
        &self.host_id
    }
}

impl Drop for ResyncPermit {
    fn drop(&mut self) {
        if let Some(gate) = self.gate.take() {
            gate.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> HostId {
        HostId::parse(&format!("01K5A0000000000000000ABC{n:02}")).expect("a ULID")
    }

    #[test]
    fn every_delay_is_within_one_and_sixty_seconds_and_the_window_doubles() {
        let mut backoff = Backoff::with_seed(7);
        let windows: Vec<u64> = (0..8)
            .map(|_| {
                let window = backoff.window();
                let delay = backoff.next_delay();
                assert!(
                    delay >= window / 2 && delay <= window,
                    "{delay:?} in {window:?}"
                );
                assert!((BACKOFF_FLOOR..=BACKOFF_CEILING).contains(&delay));
                window.as_secs()
            })
            .collect();
        assert_eq!(windows, [2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(backoff.failures(), 8);
        for _ in 0..1_000 {
            assert!(backoff.next_delay() <= BACKOFF_CEILING);
        }
        backoff.reset();
        assert_eq!(backoff.failures(), 0);
        assert_eq!(backoff.window(), Duration::from_secs(2));
    }

    #[test]
    fn two_clients_that_lost_a_host_together_do_not_redial_in_lock_step() {
        let mut a = Backoff::with_seed(1);
        let mut b = Backoff::with_seed(2);
        let da: Vec<Duration> = (0..6).map(|_| a.next_delay()).collect();
        let db: Vec<Duration> = (0..6).map(|_| b.next_delay()).collect();
        assert_ne!(da, db);
        let mut spread = std::collections::BTreeSet::new();
        let mut c = Backoff::with_seed(3);
        for _ in 0..200 {
            c.reset();
            spread.insert(c.next_delay().as_millis());
        }
        assert!(
            spread.len() > 50,
            "the first delay is jittered: {}",
            spread.len()
        );
        assert!(spread.iter().all(|ms| (1_000..=2_000).contains(ms)));
    }

    #[test]
    fn a_zero_seed_still_jitters() {
        let mut backoff = Backoff::with_seed(0);
        let first = backoff.next_delay();
        assert!((BACKOFF_FLOOR..=Duration::from_secs(2)).contains(&first));
        let _ = Backoff::new().next_delay();
    }

    #[tokio::test]
    async fn at_most_two_run_at_once() {
        let gate = ResyncGate::default();
        let first = gate.acquire(id(1), 10).await;
        let _second = gate.acquire(id(2), 10).await;
        assert_eq!(gate.running(), 2);
        let third = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire(id(3), 99).await.host_id().clone() })
        };
        while gate.waiting() < 1 {
            tokio::task::yield_now().await;
        }
        assert_eq!(gate.running(), 2, "the third waits for a slot");
        drop(first);
        assert_eq!(third.await.expect("the third"), id(3));
        assert_eq!(
            gate.running(),
            1,
            "the third's permit dropped with its task"
        );
        assert_eq!(gate.waiting(), 0);
    }

    #[tokio::test]
    async fn admission_order_is_last_active_then_first_come() {
        let gate = ResyncGate::new(1);
        let hold = gate.acquire(id(1), 0).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for (n, last_active) in [(2u8, 100i64), (3, 300), (4, 200), (5, 300)] {
            let waiter = gate.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let permit = waiter.acquire(id(n), last_active).await;
                tx.send(permit.host_id().clone()).expect("report");
                // Hold briefly so admissions are strictly one at a time.
                tokio::task::yield_now().await;
            });
            while gate.waiting() < usize::from(n - 1) {
                tokio::task::yield_now().await;
            }
        }
        drop(hold);
        let mut order = Vec::new();
        for _ in 0..4 {
            order.push(rx.recv().await.expect("an admission"));
        }
        assert_eq!(order, vec![id(3), id(5), id(4), id(2)]);
    }

    #[tokio::test]
    async fn a_waiter_that_gives_up_hands_its_slot_on() {
        let gate = ResyncGate::new(1);
        let hold = gate.acquire(id(1), 0).await;
        let quitter = {
            let gate = gate.clone();
            tokio::spawn(async move {
                let _permit = gate.acquire(id(2), 999).await;
                unreachable!("aborted before admission");
            })
        };
        while gate.waiting() < 1 {
            tokio::task::yield_now().await;
        }
        let stayer = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire(id(3), 1).await.host_id().clone() })
        };
        while gate.waiting() < 2 {
            tokio::task::yield_now().await;
        }
        quitter.abort();
        let _ = quitter.await;
        drop(hold);
        assert_eq!(stayer.await.expect("the stayer"), id(3));
        assert_eq!(gate.running(), 0, "no slot leaked to the quitter");
    }
}
