//! The per-pane control-mode feed actor (R2 WP8, spec R2 row, spike 2 and 7).
//!
//! One `tmux -C attach-session` client per watched pane. Its stdout goes
//! through [`ainb_term::control::ControlParser`]; pane bytes go into one
//! [`PaneEmulator`] and out to subscribers as [`FeedEvent::Output`], with
//! `seq` the pane-feed offset within the current epoch (T16).
//!
//! ```text
//! tmux -C ──bytes──▶ ControlParser ──▶ Output ──▶ PaneEmulator ──▶ FeedEvent::Output
//!    ▲                    │ Pause ──▶ Gap{paused}, refresh-client -A"%N:continue"
//!    │                    │ Continue (in OUR reply block only) ──▶ re-seed
//!    │                    └ Exit / EOF ──▶ Gap{feed_lost} ──▶ reattach 250 ms / 1 s / 4 s
//!    └── commands: pause-after, seed (flags, title, size, capture), continue, size query
//! ```
//!
//! Rules this actor keeps:
//!
//! * The target (a session name is operator data) is resolved through argv
//!   ONCE, with tmux's `=` exact match for a name, to a pane id; every later
//!   lookup and every reattach uses that pane id, so a feed never drifts to
//!   a similarly named session or to the session's newly active pane. The
//!   control stream only ever names the pane as `%N`.
//! * `refresh-client -f pause-after=N` is the first command on every attach,
//!   and a refused reply closes the feed with `pause_after_unsupported`
//!   rather than watching a pane tmux may throttle the agent for (S4).
//! * Every attach and every resume seeds the emulator from tmux's own grid
//!   with four commands on the SAME control stream (`display-message` flags,
//!   `display-message` title, `display-message` size and window,
//!   `capture-pane -p -e`), so the seed is ordered against the tail: pane
//!   output that arrives before the capture reply is already inside the
//!   capture and is dropped; output after it is new (S3). A layout change
//!   read while a seed is in flight is still asked about; its reply lands
//!   after the capture and either confirms the seeded size or resizes the
//!   fresh emulator. A capture row that reads `%pause %N` or `%continue %N`
//!   is lifted out of the reply body by the parser; the actor puts it back
//!   at its row, so the repaint never shifts a line.
//! * `%pause` is acted on only at top level. A `%continue` is acted on only
//!   while this pane is recorded as paused AND it arrives inside the reply
//!   block of this actor's own `refresh-client -A"%N:continue"` (the oldest
//!   un-replied command). Anywhere else it is pane text and ignored: a reply
//!   body is raw pane output and a pane can print `%continue %0`. A continue
//!   reply that succeeded without an in-block `%continue` unpauses too.
//! * The feed client never sends `refresh-client -C` (spike 2 rule 8). The
//!   sizer (WP9c) is a separate client.
//! * The emulator, its epoch and its offset live under ONE lock, and output
//!   advances the offset before that lock is released, so a snapshot can
//!   never report an offset older than the bytes it holds.
//! * The last grapheme of a feed is held by the emulator; the actor flushes
//!   it after [`FeedConfig::idle_flush`] of silence.
//! * Feed loss (EOF, `%exit`, a dead client) sends `Gap{feed_lost}` once,
//!   stamps the fleet row `restored_unconfirmed` once (CRITIQUE 13) and
//!   reattaches with backoff. A pane that no longer resolves (killed, or its
//!   session gone) sends `Gap{session_gone}` then `Closed{pane_gone}` or
//!   `Closed{session_gone}`. A blank size reply on the stream is how a dead
//!   pane shows up mid-attach. An emulator poisoned by a panic is re-seeded,
//!   at most [`MAX_POISON_RESEEDS`] times per attach, then the feed closes.
//!
//! Every command written to the client is recorded (the last
//! [`COMMAND_LOG_CAP`]) in [`FeedHandle::commands`] so a test can assert what
//! was, and was not, sent.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_term::control::{
    ControlParser, Event, PaneAction, PaneId, WindowId, refresh_client_pane,
    refresh_client_pause_after,
};
use ainb_term::emulator::PaneEmulator;
use ainb_term::seed::{SEED_FLAGS_FORMAT, SEED_TITLE_FORMAT, SeedState, seed_repaint};
pub use ainb_term::viewer::GapReason;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Notify, broadcast};

/// `pause-after` seconds the feed asks for: a client this far behind is
/// paused instead of throttling the pane (spec R2 row).
pub const DEFAULT_PAUSE_AFTER_S: u32 = 2;
/// Silence after which the emulator's held grapheme is performed.
pub const DEFAULT_IDLE_FLUSH: Duration = Duration::from_millis(8);
/// Reattach delays after a lost feed; the last one repeats.
pub const REATTACH_BACKOFF: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_secs(1),
    Duration::from_secs(4),
];
/// The most bytes one `Output` event carries; longer runs are split.
pub const OUTPUT_CHUNK: usize = 16 * 1024;
/// Events buffered per subscriber before it lags and must re-snapshot.
pub const EVENT_CAPACITY: usize = 256;
/// The most output bytes a subscriber's buffer can hold:
/// [`EVENT_CAPACITY`] events of at most [`OUTPUT_CHUNK`] each (4 MiB).
pub const EVENT_BUFFER_BYTES: usize = EVENT_CAPACITY * OUTPUT_CHUNK;
/// Commands kept in [`FeedHandle::commands`].
pub const COMMAND_LOG_CAP: usize = 256;
/// Re-seeds after an emulator panic allowed per attach before the feed
/// closes with `emulator_poisoned`: crafted output that panics the fork on
/// every repaint must not turn into an endless `capture-pane` loop.
pub const MAX_POISON_RESEEDS: u32 = 3;

/// How one feed is set up.
#[derive(Clone)]
pub struct FeedConfig {
    /// `tmux -S <socket>`; `None` is the default server. Tests always name a
    /// private socket.
    pub socket: Option<PathBuf>,
    /// A tmux target that resolves to exactly one pane
    /// (`fleet_session.tmux_target`, or a bare `%N`). Operator data: it is
    /// only ever passed through argv, once.
    pub target: String,
    /// Whole seconds; see [`DEFAULT_PAUSE_AFTER_S`].
    pub pause_after_s: u32,
    /// See [`DEFAULT_IDLE_FLUSH`].
    pub idle_flush: Duration,
    /// Scrollback rows the emulator keeps.
    pub live_rows: usize,
    /// The fleet row to stamp `restored_unconfirmed` when the feed is lost:
    /// the store pool and the `fleet_session.session_key`.
    pub store: Option<(sqlx::SqlitePool, String)>,
    /// Test seam: the reader takes and releases a read lock before every
    /// read, so a test holding the write lock stalls the client until tmux
    /// pauses the pane.
    pub read_gate: Option<Arc<tokio::sync::RwLock<()>>>,
}

impl FeedConfig {
    /// A feed on `target` with the defaults.
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            socket: None,
            target: target.into(),
            pause_after_s: DEFAULT_PAUSE_AFTER_S,
            idle_flush: DEFAULT_IDLE_FLUSH,
            live_rows: ainb_term::emulator::live_rows_from_env(),
            store: None,
            read_gate: None,
        }
    }
}

/// What the feed tells its subscribers. `seq` is the pane-feed offset within
/// the epoch at emit time; a seed restarts it at 0 under a new epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedEvent {
    /// The emulator was (re)built from tmux's grid; take a snapshot from it.
    Seeded {
        /// The new epoch.
        epoch: u64,
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// Pane bytes, exactly as the program wrote them, at most
    /// [`OUTPUT_CHUNK`] per event.
    Output {
        /// The offset of the first byte.
        seq: u64,
        /// The bytes.
        data: Vec<u8>,
    },
    /// Bytes were skipped; a `Seeded` follows unless the reason is
    /// `SessionGone`.
    Gap {
        /// The offset at which the gap opened.
        seq: u64,
        /// Why.
        reason: GapReason,
    },
    /// The pane changed size; the emulator has been resized.
    Resize {
        /// The offset at which the size changed.
        seq: u64,
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// The feed is over; nothing follows.
    Closed {
        /// `session_gone`, `pane_gone`, `pause_after_unsupported`,
        /// `seed_unreadable`, `emulator_poisoned` or `stopped`.
        reason: String,
    },
}

/// The emulator with the epoch and offset it stands at, under one lock.
#[derive(Default)]
pub struct Pane {
    /// `None` until the first seed.
    pub emulator: Option<PaneEmulator>,
    /// The current epoch; 0 until the first seed.
    pub epoch: u64,
    /// The pane-feed offset within the epoch: the bytes the emulator holds.
    pub seq: u64,
}

/// Live counters a caller may read; `epoch` and `seq` come from [`Pane`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedStatus {
    /// The current epoch; 0 until the first seed.
    pub epoch: u64,
    /// The pane-feed offset within the epoch.
    pub seq: u64,
    /// The pane is paused for this client.
    pub paused: bool,
    /// The control client's pid, when one is running.
    pub client_pid: Option<u32>,
    /// The pane, once resolved.
    pub pane: Option<PaneId>,
    /// Attaches so far, including the first.
    pub attaches: u32,
}

/// The part of the status the actor owns; the offset lives in [`Pane`].
#[derive(Debug, Default)]
struct Counters {
    paused: bool,
    client_pid: Option<u32>,
    pane: Option<PaneId>,
    attaches: u32,
}

/// A running feed.
pub struct FeedHandle {
    events: broadcast::Sender<FeedEvent>,
    /// The receiver created before the actor started, so the first
    /// subscriber sees every event from the start (a fast `session_gone` or
    /// the first `Seeded` would otherwise be sent to nobody).
    first: Mutex<Option<broadcast::Receiver<FeedEvent>>>,
    pane: Arc<Mutex<Pane>>,
    commands: Arc<Mutex<VecDeque<String>>>,
    counters: Arc<Mutex<Counters>>,
    stop: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl FeedHandle {
    /// Start a feed. Returns at once; the first subscriber sees the events
    /// from the start: `Seeded`, or `Gap{session_gone}` and `Closed`.
    pub fn spawn(cfg: FeedConfig) -> Self {
        let (events, first) = broadcast::channel(EVENT_CAPACITY);
        let pane = Arc::new(Mutex::new(Pane::default()));
        let commands = Arc::new(Mutex::new(VecDeque::new()));
        let counters = Arc::new(Mutex::new(Counters::default()));
        let stop = Arc::new(Notify::new());
        let actor = Actor {
            cfg,
            events: events.clone(),
            pane: Arc::clone(&pane),
            commands: Arc::clone(&commands),
            counters: Arc::clone(&counters),
            stop: Arc::clone(&stop),
        };
        let task = tokio::spawn(actor.run());
        Self {
            events,
            first: Mutex::new(Some(first)),
            pane,
            commands,
            counters,
            stop,
            task,
        }
    }

    /// A subscriber. The first call gets the receiver that existed before
    /// the actor started; later calls see events from now on. A subscriber
    /// that lags behind [`EVENT_CAPACITY`] events (at most
    /// [`EVENT_BUFFER_BYTES`] of output) gets `RecvError::Lagged` and must
    /// re-snapshot.
    pub fn subscribe(&self) -> broadcast::Receiver<FeedEvent> {
        self.first
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| self.events.subscribe())
    }

    /// The emulator with its epoch and offset. Lock it briefly.
    pub fn pane(&self) -> &Arc<Mutex<Pane>> {
        &self.pane
    }

    /// The last [`COMMAND_LOG_CAP`] command lines written to the client.
    pub fn commands(&self) -> Vec<String> {
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// The live counters.
    pub fn status(&self) -> FeedStatus {
        let (epoch, seq) = {
            let pane = self.pane.lock().unwrap_or_else(|e| e.into_inner());
            (pane.epoch, pane.seq)
        };
        let c = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        FeedStatus {
            epoch,
            seq,
            paused: c.paused,
            client_pid: c.client_pid,
            pane: c.pane,
            attaches: c.attaches,
        }
    }

    /// A snapshot of the emulator with up to `scrollback_rows` of history,
    /// with the epoch and offset it stands at, all read under the one lock.
    /// `None` before the first seed or while the emulator is poisoned (a
    /// re-seed is under way).
    pub fn snapshot(&self, scrollback_rows: usize) -> Option<(u64, u64, u16, u16, Vec<u8>)> {
        let mut guard = self.pane.lock().unwrap_or_else(|e| e.into_inner());
        let (epoch, seq) = (guard.epoch, guard.seq);
        let emu = guard.emulator.as_mut()?;
        let bytes = ainb_term::snapshot::snapshot(emu, scrollback_rows).ok()?;
        let (cols, rows) = emu.size();
        Some((epoch, seq, cols, rows, bytes))
    }

    /// Stop the feed: the control client is killed and `Closed{stopped}` is
    /// the last event.
    pub async fn stop(mut self) {
        self.stop.notify_one();
        let _ = (&mut self.task).await;
    }
}

impl Drop for FeedHandle {
    fn drop(&mut self) {
        self.stop.notify_one();
    }
}

/// Which command a pending reply belongs to. tmux answers in order, so the
/// front of the queue is the block currently open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// The empty block tmux emits on attach.
    Greeting,
    PauseAfter,
    SeedFlags,
    SeedTitle,
    /// The pane size and window just before the capture, so a resize
    /// between the attach and the seed (or during it) is carried by the
    /// seed itself, and a pane moved to another window is followed.
    SeedSize,
    SeedCapture,
    Continue,
    SizeQuery,
}

/// What the target resolves to, through argv.
struct Resolved {
    /// The session name as tmux printed it, any bytes: only ever an argv.
    session: OsString,
    pane: PaneId,
    window: WindowId,
}

/// Why `serve` returned.
enum Outcome {
    Stopped,
    FeedLost,
    /// The pane no longer resolves; its session may or may not exist.
    PaneGone,
    Fatal(&'static str),
}

struct Actor {
    cfg: FeedConfig,
    events: broadcast::Sender<FeedEvent>,
    pane: Arc<Mutex<Pane>>,
    commands: Arc<Mutex<VecDeque<String>>>,
    counters: Arc<Mutex<Counters>>,
    stop: Arc<Notify>,
}

/// One attach's mutable state.
struct Attach {
    stdin: ChildStdin,
    parser: ControlParser,
    pending: VecDeque<Pending>,
    pane: PaneId,
    window: WindowId,
    cols: u16,
    rows: u16,
    paused: bool,
    /// A seed is in flight: flags and title collected so far. Output for the
    /// pane is dropped until the capture reply lands.
    seeding: Option<(SeedState, Vec<u8>)>,
    /// Re-seeds forced by an emulator panic on this attach.
    poison_reseeds: u32,
    /// Lines fed into the open reply block so far, and the `%pause` or
    /// `%continue` rows the parser lifted out of it, with their row index,
    /// so a capture can be put back together.
    block_lines: usize,
    lifted: Vec<(usize, Vec<u8>)>,
}

impl Actor {
    fn tmux(&self) -> Command {
        let mut cmd = Command::new("tmux");
        cmd.env_remove("TMUX");
        if let Some(sock) = &self.cfg.socket {
            cmd.arg("-S").arg(sock);
        }
        cmd
    }

    fn emit(&self, event: FeedEvent) {
        let _ = self.events.send(event);
    }

    fn with_counters(&self, f: impl FnOnce(&mut Counters)) {
        f(&mut self.counters.lock().unwrap_or_else(|e| e.into_inner()));
    }

    fn seq(&self) -> u64 {
        self.pane.lock().unwrap_or_else(|e| e.into_inner()).seq
    }

    async fn run(mut self) {
        // The operator target is resolved once; from here on the pane id
        // is the only name this feed uses.
        let Some(first) = self.resolve(&self.cfg.target.clone(), true).await else {
            self.gone("session_gone");
            return;
        };
        let pane_target = first.pane.to_string();
        let mut resolved = first;
        let mut attempt = 0usize;
        let mut lost_announced = false;
        loop {
            let epoch_before = self.pane.lock().unwrap_or_else(|e| e.into_inner()).epoch;
            let outcome = match self.attach(&resolved.session).await {
                Ok(mut child) => {
                    let outcome = self.serve(&mut child, &resolved, &mut attempt).await;
                    let _ = child.kill().await;
                    self.with_counters(|c| c.client_pid = None);
                    outcome
                }
                Err(error) => {
                    tracing::warn!(target = %self.cfg.target, %error, "terminal feed attach failed");
                    Outcome::FeedLost
                }
            };
            if self.pane.lock().unwrap_or_else(|e| e.into_inner()).epoch > epoch_before {
                // This attach seeded: the next loss is a new one.
                lost_announced = false;
            }
            match outcome {
                Outcome::Stopped => {
                    self.emit(FeedEvent::Closed {
                        reason: "stopped".to_string(),
                    });
                    return;
                }
                Outcome::PaneGone => {
                    self.gone(self.gone_reason(&resolved.session).await);
                    return;
                }
                Outcome::Fatal(reason) => {
                    self.emit(FeedEvent::Closed {
                        reason: reason.to_string(),
                    });
                    return;
                }
                Outcome::FeedLost => {
                    // The pane, not the operator target: the session's
                    // active pane may have changed since the first resolve.
                    match self.resolve(&pane_target, false).await {
                        Some(again) => resolved = again,
                        None => {
                            self.gone(self.gone_reason(&resolved.session).await);
                            return;
                        }
                    }
                    if !lost_announced {
                        // Once per loss, not once per retry.
                        self.feed_lost().await;
                        lost_announced = true;
                    }
                }
            }
            let delay = REATTACH_BACKOFF[attempt.min(REATTACH_BACKOFF.len() - 1)];
            attempt += 1;
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = self.stop.notified() => {
                    self.emit(FeedEvent::Closed { reason: "stopped".to_string() });
                    return;
                }
            }
        }
    }

    /// `session_gone` when the pane's session is gone too, else `pane_gone`.
    async fn gone_reason(&self, session: &OsString) -> &'static str {
        let mut exact = OsString::from("=");
        exact.push(session);
        let alive = self
            .tmux()
            .args(["has-session", "-t"])
            .arg(exact)
            .output()
            .await
            .is_ok_and(|out| out.status.success());
        if alive { "pane_gone" } else { "session_gone" }
    }

    /// The session, pane and window `target` names, or `None` when tmux
    /// cannot find it. Resolved through argv, never on the control stream:
    /// a target is operator data (a session name may hold quotes,
    /// semicolons, tabs or any byte), and a control-mode line is parsed
    /// like a shell command.
    ///
    /// `exact` spells a NAME target with tmux's `=` so a prefix or pattern
    /// match can never bind the feed to a similarly named session: a bare
    /// name becomes `=name:` (a bare `=name` is not a pane target and
    /// answers blank), `sess:win.pane` becomes `=sess:win.pane`; an id
    /// target (`%N`, `@N`, `$N`) takes no prefix. The ids come first in the
    /// reply and the name last, so the name can hold anything.
    async fn resolve(&self, target: &str, exact: bool) -> Option<Resolved> {
        let is_id = target.starts_with(['%', '@', '$']);
        let argv_target = match (exact, is_id, target.contains(':')) {
            (true, false, true) => format!("={target}"),
            (true, false, false) => format!("={target}:"),
            _ => target.to_string(),
        };
        let out = self
            .tmux()
            .args([
                "display-message",
                "-p",
                "-t",
                &argv_target,
                "#{pane_id}\t#{window_id}\t#{session_name}",
            ])
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let mut line = out.stdout;
        while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            line.pop();
        }
        let mut fields = line.splitn(3, |b| *b == b'\t');
        let pane = std::str::from_utf8(fields.next()?)
            .ok()?
            .strip_prefix('%')?
            .parse()
            .ok()
            .map(PaneId)?;
        let window = std::str::from_utf8(fields.next()?)
            .ok()?
            .strip_prefix('@')?
            .parse()
            .ok()
            .map(WindowId)?;
        let session = fields.next()?;
        (!session.is_empty()).then(|| Resolved {
            session: OsString::from_vec(session.to_vec()),
            pane,
            window,
        })
    }

    async fn attach(&self, session: &OsString) -> std::io::Result<Child> {
        let mut exact = OsString::from("=");
        exact.push(session);
        let child = self
            .tmux()
            .args(["-C", "attach-session", "-t"])
            .arg(exact)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        self.with_counters(|c| {
            c.client_pid = child.id();
            c.attaches += 1;
        });
        Ok(child)
    }

    /// The pane (or its session) is gone: the terminal gap, then `Closed`.
    fn gone(&self, reason: &'static str) {
        self.emit(FeedEvent::Gap {
            seq: self.seq(),
            reason: GapReason::SessionGone,
        });
        self.emit(FeedEvent::Closed {
            reason: reason.to_string(),
        });
    }

    async fn feed_lost(&self) {
        self.emit(FeedEvent::Gap {
            seq: self.seq(),
            reason: GapReason::FeedLost,
        });
        if let Some((pool, session_key)) = &self.cfg.store {
            if let Err(error) =
                ainb_hangar_store::repo::fleet::FleetRepo::mark_session_restored_unconfirmed(
                    pool,
                    session_key,
                )
                .await
            {
                tracing::warn!(%session_key, %error, "could not stamp restored_unconfirmed");
            }
        }
    }

    async fn send(&self, at: &mut Attach, line: String, kind: Pending) -> std::io::Result<()> {
        debug_assert!(line.ends_with('\n'));
        {
            let mut log = self.commands.lock().unwrap_or_else(|e| e.into_inner());
            if log.len() == COMMAND_LOG_CAP {
                log.pop_front();
            }
            log.push_back(line.trim_end().to_string());
        }
        at.pending.push_back(kind);
        at.stdin.write_all(line.as_bytes()).await?;
        at.stdin.flush().await
    }

    /// Ask for the seed: flags, title, size, capture, in that order on the
    /// stream. Every command names the pane as `%N`.
    async fn start_seed(&self, at: &mut Attach) -> std::io::Result<()> {
        let pane = at.pane;
        at.seeding = Some((SeedState::default(), Vec::new()));
        self.send(
            at,
            format!("display-message -p -t \"{pane}\" \"{SEED_FLAGS_FORMAT}\"\n"),
            Pending::SeedFlags,
        )
        .await?;
        self.send(
            at,
            format!("display-message -p -t \"{pane}\" \"{SEED_TITLE_FORMAT}\"\n"),
            Pending::SeedTitle,
        )
        .await?;
        self.send(at, size_query(pane), Pending::SeedSize).await?;
        self.send(
            at,
            format!("capture-pane -p -e -t \"{pane}\"\n"),
            Pending::SeedCapture,
        )
        .await
    }

    /// Drive one attached client until it ends.
    async fn serve(
        &mut self,
        child: &mut Child,
        resolved: &Resolved,
        attempt: &mut usize,
    ) -> Outcome {
        let Some(stdin) = child.stdin.take() else {
            return Outcome::FeedLost;
        };
        let Some(mut stdout) = child.stdout.take() else {
            return Outcome::FeedLost;
        };
        let mut at = Attach {
            stdin,
            parser: ControlParser::new(),
            pending: VecDeque::new(),
            pane: resolved.pane,
            window: resolved.window,
            cols: 0,
            rows: 0,
            paused: false,
            seeding: None,
            poison_reseeds: 0,
            block_lines: 0,
            lifted: Vec::new(),
        };
        self.with_counters(|c| {
            c.pane = Some(resolved.pane);
            c.paused = false;
        });
        at.pending.push_back(Pending::Greeting);
        let boot = async {
            self.send(
                &mut at,
                refresh_client_pause_after(self.cfg.pause_after_s),
                Pending::PauseAfter,
            )
            .await?;
            // The first command after pause-after is the seed; every command
            // names the pane as `%N`, never the target.
            self.start_seed(&mut at).await
        };
        if boot.await.is_err() {
            return Outcome::FeedLost;
        }

        let mut buf = vec![0u8; OUTPUT_CHUNK];
        let idle = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(idle);
        loop {
            tokio::select! {
                () = self.stop.notified() => return Outcome::Stopped,
                () = &mut idle => {
                    self.flush_held();
                    idle.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(3600));
                }
                read = gated_read(self.cfg.read_gate.as_ref(), &mut stdout, &mut buf) => {
                    let n = match read {
                        Ok(0) | Err(_) => return Outcome::FeedLost,
                        Ok(n) => n,
                    };
                    // One line at a time, so `in_block()` answers for THIS
                    // line: a lifted `%pause` or `%continue` is in-block only
                    // while its block is still open after it, and a whole
                    // chunk may open and close several blocks. The line
                    // count per block lets a lifted capture row go back.
                    let mut rest = &buf[..n];
                    while !rest.is_empty() {
                        let end = rest.iter().position(|b| *b == b'\n').map_or(rest.len(), |i| i + 1);
                        let (line, tail) = rest.split_at(end);
                        rest = tail;
                        let was_in_block = at.parser.in_block();
                        let events = at.parser.feed(line);
                        let in_block = at.parser.in_block();
                        if !was_in_block && in_block {
                            at.block_lines = 0;
                            at.lifted.clear();
                        } else if was_in_block && in_block && line.ends_with(b"\n") {
                            // A body line, or a lifted one: both count.
                            if events.iter().any(|e| matches!(e, Event::Pause(_) | Event::Continue(_))) {
                                let mut row = line.to_vec();
                                while row.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                                    row.pop();
                                }
                                at.lifted.push((at.block_lines, row));
                            }
                            at.block_lines += 1;
                        }
                        for event in events {
                            match self.handle(&mut at, event, in_block, attempt).await {
                                Ok(None) => {}
                                Ok(Some(outcome)) => return outcome,
                                Err(_) => return Outcome::FeedLost,
                            }
                        }
                    }
                    if self.has_held() {
                        idle.as_mut().reset(tokio::time::Instant::now() + self.cfg.idle_flush);
                    }
                }
            }
        }
    }

    fn has_held(&self) -> bool {
        self.pane
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .emulator
            .as_ref()
            .is_some_and(PaneEmulator::has_held_text)
    }

    fn flush_held(&self) {
        if let Some(emu) = self.pane.lock().unwrap_or_else(|e| e.into_inner()).emulator.as_mut() {
            let _ = emu.flush();
        }
    }

    /// The emulator panicked: re-seed, a bounded number of times per attach.
    async fn poisoned(&self, at: &mut Attach) -> std::io::Result<Option<Outcome>> {
        at.poison_reseeds += 1;
        if at.poison_reseeds > MAX_POISON_RESEEDS {
            tracing::error!(
                target = %self.cfg.target,
                "pane emulator poisoned {} times on one attach; closing the feed",
                at.poison_reseeds
            );
            return Ok(Some(Outcome::Fatal("emulator_poisoned")));
        }
        tracing::warn!(target = %self.cfg.target, "pane emulator poisoned; re-seeding");
        self.emit(FeedEvent::Gap {
            seq: self.seq(),
            reason: GapReason::FeedLost,
        });
        self.start_seed(at).await?;
        Ok(None)
    }

    /// One parsed event. `Ok(Some(_))` ends the attach.
    async fn handle(
        &mut self,
        at: &mut Attach,
        event: Event,
        in_block: bool,
        attempt: &mut usize,
    ) -> std::io::Result<Option<Outcome>> {
        match event {
            Event::Output { pane, data, .. } => {
                if pane != at.pane || at.seeding.is_some() || at.paused {
                    // Another pane of the session, or bytes the pending
                    // capture already contains, or bytes after a pause that
                    // tmux should not have sent.
                    return Ok(None);
                }
                // Feed, then advance the offset, all under the one lock: a
                // snapshot taken in between would otherwise carry these
                // bytes under the offset before them.
                let fed = {
                    let mut guard = self.pane.lock().unwrap_or_else(|e| e.into_inner());
                    let ok = match guard.emulator.as_mut() {
                        Some(emu) => emu.feed(&data).is_ok(),
                        None => true,
                    };
                    if ok {
                        let start = guard.seq;
                        guard.seq += data.len() as u64;
                        Some(start)
                    } else {
                        None
                    }
                };
                let Some(mut seq) = fed else {
                    return self.poisoned(at).await;
                };
                if data.len() <= OUTPUT_CHUNK {
                    // The common case moves the bytes; only an oversize line
                    // is split and copied.
                    self.emit(FeedEvent::Output { seq, data });
                } else {
                    for chunk in data.chunks(OUTPUT_CHUNK) {
                        self.emit(FeedEvent::Output {
                            seq,
                            data: chunk.to_vec(),
                        });
                        seq += chunk.len() as u64;
                    }
                }
            }
            Event::Pause(pane) => {
                if pane != at.pane {
                    return Ok(None);
                }
                if in_block {
                    tracing::debug!(%pane, "in-block %pause ignored (advisory)");
                    return Ok(None);
                }
                if !at.paused {
                    at.paused = true;
                    self.with_counters(|c| c.paused = true);
                    self.emit(FeedEvent::Gap {
                        seq: self.seq(),
                        reason: GapReason::Paused,
                    });
                    self.send(
                        at,
                        refresh_client_pane(pane, PaneAction::Continue),
                        Pending::Continue,
                    )
                    .await?;
                }
            }
            Event::Continue(pane) => {
                let ours = pane == at.pane
                    && at.paused
                    && in_block
                    && at.pending.front() == Some(&Pending::Continue);
                if !ours {
                    tracing::debug!(%pane, "%continue outside our continue reply ignored");
                    return Ok(None);
                }
                self.resume(at).await?;
            }
            Event::Reply { ok, mut lines, .. } => {
                let kind = at.pending.pop_front();
                if kind == Some(Pending::SeedCapture) {
                    // Put the lifted rows back where the pane printed them.
                    for (index, row) in at.lifted.drain(..) {
                        let index = index.min(lines.len());
                        lines.insert(index, row);
                    }
                }
                return self.reply(at, kind, ok, lines, attempt).await;
            }
            Event::LayoutChange { .. } => {
                // Asked for any window, even while a seed is in flight: the
                // reply carries the pane's current size and window, so it
                // confirms the seeded size, resizes the fresh emulator, or
                // follows a pane moved to another window.
                self.send(at, size_query(at.pane), Pending::SizeQuery).await?;
            }
            Event::WindowClose(window) => {
                if window == at.window {
                    // Either the pane died with its window, or it was moved
                    // out first; the size reply tells which.
                    self.send(at, size_query(at.pane), Pending::SizeQuery).await?;
                }
            }
            Event::Exit { .. } => return Ok(Some(Outcome::FeedLost)),
            Event::Other { .. } => {}
        }
        Ok(None)
    }

    /// The pane resumed after a pause: the stream is not a continuation, so
    /// re-seed.
    async fn resume(&self, at: &mut Attach) -> std::io::Result<()> {
        at.paused = false;
        self.with_counters(|c| c.paused = false);
        self.start_seed(at).await
    }

    async fn reply(
        &mut self,
        at: &mut Attach,
        kind: Option<Pending>,
        ok: bool,
        lines: Vec<Vec<u8>>,
        attempt: &mut usize,
    ) -> std::io::Result<Option<Outcome>> {
        match kind {
            Some(Pending::Greeting) | None => {}
            Some(Pending::Continue) => {
                if !ok {
                    // The pane stays paused for this client and nothing
                    // else can lift it: a fresh attach can.
                    tracing::warn!(target = %self.cfg.target, "tmux refused the continue; reattaching");
                    return Ok(Some(Outcome::FeedLost));
                }
                if at.paused {
                    // Accepted, but no `%continue` arrived inside the block.
                    self.resume(at).await?;
                }
            }
            Some(Pending::PauseAfter) => {
                if !ok {
                    tracing::error!(
                        target = %self.cfg.target,
                        "tmux refused pause-after; terminal streams need tmux 3.2 or newer"
                    );
                    return Ok(Some(Outcome::Fatal("pause_after_unsupported")));
                }
            }
            Some(Pending::SeedFlags) => {
                let Some((state, _)) = at.seeding.as_mut() else {
                    return Ok(None);
                };
                match lines.first().and_then(|l| SeedState::parse(l)) {
                    Some(parsed) => *state = parsed,
                    None if lines.first().is_none_or(|l| l.trim_ascii().is_empty()) => {
                        // A blank reply is how a dead pane answers.
                        return Ok(Some(Outcome::PaneGone));
                    }
                    None => {
                        // A wrong screen with no diagnostic is worse than no
                        // feed: this tmux does not render the seed formats.
                        tracing::error!(
                            target = %self.cfg.target,
                            reply = %String::from_utf8_lossy(lines.first().map_or(&[][..], Vec::as_slice)),
                            "seed flags reply does not parse; closing the feed"
                        );
                        return Ok(Some(Outcome::Fatal("seed_unreadable")));
                    }
                }
            }
            Some(Pending::SeedTitle) => {
                if let Some((_, title)) = at.seeding.as_mut() {
                    *title = lines.first().cloned().unwrap_or_default();
                }
            }
            Some(Pending::SeedSize) | Some(Pending::SizeQuery) => {
                // A blank or refused reply is a dead pane (display-message
                // answers a missing target with an empty body and `%end`).
                let Some((cols, rows, window)) =
                    ok.then(|| lines.first().and_then(|l| parse_size(l))).flatten()
                else {
                    return Ok(Some(Outcome::PaneGone));
                };
                at.window = window;
                if (cols, rows) == (at.cols, at.rows) {
                    return Ok(None);
                }
                at.cols = cols;
                at.rows = rows;
                if at.seeding.is_some() {
                    // The capture that follows is taken at this size.
                    return Ok(None);
                }
                let seq = {
                    let mut guard = self.pane.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(emu) = guard.emulator.as_mut() {
                        let _ = emu.resize(cols, rows);
                    }
                    guard.seq
                };
                self.emit(FeedEvent::Resize { seq, cols, rows });
            }
            Some(Pending::SeedCapture) => {
                let Some((state, title)) = at.seeding.take() else {
                    return Ok(None);
                };
                if !ok {
                    return Ok(Some(Outcome::PaneGone));
                }
                let repaint = seed_repaint(&lines, &title, &state, at.rows);
                let mut emu = PaneEmulator::new(at.cols, at.rows, self.cfg.live_rows);
                if emu.feed(&repaint).is_err() || emu.flush().is_err() {
                    return self.poisoned(at).await;
                }
                let epoch = {
                    let mut guard = self.pane.lock().unwrap_or_else(|e| e.into_inner());
                    guard.emulator = Some(emu);
                    guard.epoch += 1;
                    guard.seq = 0;
                    guard.epoch
                };
                *attempt = 0;
                self.emit(FeedEvent::Seeded {
                    epoch,
                    cols: at.cols,
                    rows: at.rows,
                });
            }
        }
        Ok(None)
    }
}

/// Read through the test gate, when one is set.
async fn gated_read(
    gate: Option<&Arc<tokio::sync::RwLock<()>>>,
    stdout: &mut tokio::process::ChildStdout,
    buf: &mut [u8],
) -> std::io::Result<usize> {
    if let Some(gate) = gate {
        // Taken and released BEFORE the read: a guard held across an idle
        // read would block the test's writer for as long as the pane is
        // silent. tokio's RwLock is write-preferring, so once the test holds
        // the write lock the next read here waits.
        drop(gate.read().await);
    }
    stdout.read(buf).await
}

/// The size-and-window query for a pane, newline-terminated. The pane id is
/// digits after `%`, quoted against the spike 7 parse trap.
fn size_query(pane: PaneId) -> String {
    format!(
        "display-message -p -t \"{pane}\" \"#{{pane_width}} #{{pane_height}} #{{window_id}}\"\n"
    )
}

/// `cols rows @window`.
fn parse_size(line: &[u8]) -> Option<(u16, u16, WindowId)> {
    let text = std::str::from_utf8(line).ok()?;
    let mut f = text.split_whitespace();
    let cols = f.next()?.parse().ok()?;
    let rows = f.next()?.parse().ok()?;
    let window = f.next()?.strip_prefix('@')?.parse().ok().map(WindowId)?;
    Some((cols, rows, window))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_size_reply_parses() {
        assert_eq!(parse_size(b"40 20 @3"), Some((40, 20, WindowId(3))));
        assert_eq!(parse_size(b"40 20"), None);
        assert_eq!(parse_size(b""), None);
        assert_eq!(parse_size(b" "), None);
        assert_eq!(
            size_query(PaneId(7)),
            "display-message -p -t \"%7\" \"#{pane_width} #{pane_height} #{window_id}\"\n"
        );
    }

    #[test]
    fn the_seed_commands_quote_the_pane_id() {
        // `%0` as a bare argument is a tmux parse error (spike 7); every
        // command that names the pane quotes it.
        let pane = PaneId(0);
        let line = format!("capture-pane -p -e -t \"{pane}\"\n");
        assert_eq!(line, "capture-pane -p -e -t \"%0\"\n");
    }

    #[test]
    fn the_event_buffer_is_bounded_in_bytes() {
        assert_eq!(EVENT_BUFFER_BYTES, 4 * 1024 * 1024);
        assert!(OUTPUT_CHUNK <= ainb_term::viewer::CHUNK_BYTES);
    }
}
