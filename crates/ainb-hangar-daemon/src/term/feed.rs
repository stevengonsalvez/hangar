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
//!   before the attach; the control stream only ever names the pane as `%N`.
//! * `refresh-client -f pause-after=N` is the first command on every attach,
//!   and a refused reply closes the feed with `pause_after_unsupported`
//!   rather than watching a pane tmux may throttle the agent for (S4).
//! * Every attach and every resume seeds the emulator from tmux's own grid
//!   with four commands on the SAME control stream (`display-message` flags,
//!   `display-message` title, `display-message` size, `capture-pane -p -e`),
//!   so the seed is ordered against the tail: pane output that arrives
//!   before the capture reply is already inside the capture and is dropped;
//!   output after it is new (S3). A layout change read while a seed is in
//!   flight is still asked about; its reply lands after the capture and
//!   either confirms the seeded size or resizes the fresh emulator.
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
//! * Feed loss (EOF, `%exit`, a dead client) sends `Gap{feed_lost}`, stamps
//!   the fleet row `restored_unconfirmed` (CRITIQUE 13) and reattaches with
//!   backoff. A session that no longer exists sends `Gap{session_gone}` then
//!   `Closed`. An emulator poisoned by a panic is re-seeded, at most
//!   [`MAX_POISON_RESEEDS`] times per attach, then the feed closes.
//!
//! Every command written to the client is recorded (the last
//! [`COMMAND_LOG_CAP`]) in [`FeedHandle::commands`] so a test can assert what
//! was, and was not, sent.

use std::collections::VecDeque;
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
    /// only ever passed through argv.
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
        /// `session_gone`, `pause_after_unsupported`, `emulator_poisoned`
        /// or `stopped`.
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

/// Live counters a caller may read.
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

/// A running feed.
pub struct FeedHandle {
    events: broadcast::Sender<FeedEvent>,
    pane: Arc<Mutex<Pane>>,
    commands: Arc<Mutex<VecDeque<String>>>,
    status: Arc<Mutex<FeedStatus>>,
    stop: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl FeedHandle {
    /// Start a feed. Returns at once; the first event is `Seeded` (or
    /// `Gap{session_gone}` and `Closed`).
    pub fn spawn(cfg: FeedConfig) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let pane = Arc::new(Mutex::new(Pane::default()));
        let commands = Arc::new(Mutex::new(VecDeque::new()));
        let status = Arc::new(Mutex::new(FeedStatus::default()));
        let stop = Arc::new(Notify::new());
        let actor = Actor {
            cfg,
            events: events.clone(),
            pane: Arc::clone(&pane),
            commands: Arc::clone(&commands),
            status: Arc::clone(&status),
            stop: Arc::clone(&stop),
        };
        let task = tokio::spawn(actor.run());
        Self {
            events,
            pane,
            commands,
            status,
            stop,
            task,
        }
    }

    /// A new subscriber. It sees events from now on; a subscriber that lags
    /// behind [`EVENT_CAPACITY`] events (at most [`EVENT_BUFFER_BYTES`] of
    /// output) gets `RecvError::Lagged` and must re-snapshot.
    pub fn subscribe(&self) -> broadcast::Receiver<FeedEvent> {
        self.events.subscribe()
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
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let pane = self.pane.lock().unwrap_or_else(|e| e.into_inner());
        status.epoch = pane.epoch;
        status.seq = pane.seq;
        status
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
    /// The pane size just before the capture, so a resize between the
    /// attach and the seed (or during it) is carried by the seed itself.
    SeedSize,
    SeedCapture,
    Continue,
    SizeQuery,
}

/// What the target resolves to, through argv.
struct Resolved {
    session: String,
    pane: PaneId,
    window: WindowId,
}

/// Why `serve` returned.
enum Outcome {
    Stopped,
    FeedLost,
    SessionGone,
    Fatal(&'static str),
}

struct Actor {
    cfg: FeedConfig,
    events: broadcast::Sender<FeedEvent>,
    pane: Arc<Mutex<Pane>>,
    commands: Arc<Mutex<VecDeque<String>>>,
    status: Arc<Mutex<FeedStatus>>,
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

    fn with_status(&self, f: impl FnOnce(&mut FeedStatus)) {
        f(&mut self.status.lock().unwrap_or_else(|e| e.into_inner()));
    }

    fn seq(&self) -> u64 {
        self.pane.lock().unwrap_or_else(|e| e.into_inner()).seq
    }

    async fn run(mut self) {
        let mut attempt = 0usize;
        loop {
            let Some(resolved) = self.resolve().await else {
                self.session_gone();
                return;
            };
            let outcome = match self.attach(&resolved.session).await {
                Ok(mut child) => {
                    let outcome = self.serve(&mut child, &resolved, &mut attempt).await;
                    let _ = child.kill().await;
                    self.with_status(|s| s.client_pid = None);
                    outcome
                }
                Err(error) => {
                    tracing::warn!(target = %self.cfg.target, %error, "terminal feed attach failed");
                    Outcome::FeedLost
                }
            };
            match outcome {
                Outcome::Stopped => {
                    self.emit(FeedEvent::Closed {
                        reason: "stopped".to_string(),
                    });
                    return;
                }
                Outcome::SessionGone => {
                    self.session_gone();
                    return;
                }
                Outcome::Fatal(reason) => {
                    self.emit(FeedEvent::Closed {
                        reason: reason.to_string(),
                    });
                    return;
                }
                Outcome::FeedLost => {
                    if self.resolve().await.is_none() {
                        self.session_gone();
                        return;
                    }
                    self.feed_lost().await;
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

    /// The session, pane and window the target names, or `None` when tmux
    /// cannot find it. Resolved through argv, never on the control stream:
    /// a target is operator data (a session name may hold quotes and
    /// semicolons), and a control-mode line is parsed like a shell command.
    /// After this, the only target the stream ever sees is the pane id.
    async fn resolve(&self) -> Option<Resolved> {
        let out = self
            .tmux()
            .args([
                "display-message",
                "-p",
                "-t",
                &self.cfg.target,
                "#{session_name}\t#{pane_id}\t#{window_id}",
            ])
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut f = text.trim_end_matches(['\r', '\n']).split('\t');
        let session = f.next()?.to_string();
        let pane = f.next()?.strip_prefix('%')?.parse().ok().map(PaneId)?;
        let window = f.next()?.strip_prefix('@')?.parse().ok().map(WindowId)?;
        (!session.is_empty()).then_some(Resolved {
            session,
            pane,
            window,
        })
    }

    async fn attach(&self, session: &str) -> std::io::Result<Child> {
        let child = self
            .tmux()
            .args(["-C", "attach-session", "-t", &format!("={session}")])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        self.with_status(|s| {
            s.client_pid = child.id();
            s.attaches += 1;
        });
        Ok(child)
    }

    fn session_gone(&self) {
        self.emit(FeedEvent::Gap {
            seq: self.seq(),
            reason: GapReason::SessionGone,
        });
        self.emit(FeedEvent::Closed {
            reason: "session_gone".to_string(),
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
        };
        self.with_status(|s| s.pane = Some(resolved.pane));
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
                    // chunk may open and close several blocks.
                    let mut rest = &buf[..n];
                    while !rest.is_empty() {
                        let end = rest.iter().position(|b| *b == b'\n').map_or(rest.len(), |i| i + 1);
                        let (line, tail) = rest.split_at(end);
                        rest = tail;
                        for event in at.parser.feed(line) {
                            let in_block = at.parser.in_block();
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
                for chunk in data.chunks(OUTPUT_CHUNK) {
                    self.emit(FeedEvent::Output {
                        seq,
                        data: chunk.to_vec(),
                    });
                    seq += chunk.len() as u64;
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
                    self.with_status(|s| s.paused = true);
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
            Event::Reply { ok, lines, .. } => {
                let kind = at.pending.pop_front();
                return self.reply(at, kind, ok, lines, attempt).await;
            }
            Event::LayoutChange { window, .. } => {
                // Asked even while a seed is in flight: the reply is ordered
                // after the capture, so it either confirms the seeded size
                // or resizes the fresh emulator.
                if window == at.window {
                    self.send(at, size_query(at.pane), Pending::SizeQuery).await?;
                }
            }
            Event::WindowClose(window) => {
                if window == at.window {
                    return Ok(Some(Outcome::SessionGone));
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
        self.with_status(|s| s.paused = false);
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
                if let Some((state, _)) = at.seeding.as_mut() {
                    if let Some(parsed) = lines.first().and_then(|l| SeedState::parse(l)) {
                        *state = parsed;
                    }
                }
            }
            Some(Pending::SeedTitle) => {
                if let Some((_, title)) = at.seeding.as_mut() {
                    *title = lines.first().cloned().unwrap_or_default();
                }
            }
            Some(Pending::SeedSize) => {
                if !ok {
                    return Ok(Some(Outcome::SessionGone));
                }
                let Some((cols, rows)) = lines.first().and_then(|l| parse_size(l)) else {
                    return Ok(Some(Outcome::SessionGone));
                };
                at.cols = cols;
                at.rows = rows;
            }
            Some(Pending::SeedCapture) => {
                let Some((state, title)) = at.seeding.take() else {
                    return Ok(None);
                };
                if !ok {
                    return Ok(Some(Outcome::SessionGone));
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
            Some(Pending::SizeQuery) => {
                let Some((cols, rows)) = lines.first().and_then(|l| parse_size(l)) else {
                    return Ok(None);
                };
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

/// The size query for a pane, newline-terminated. The pane id is digits
/// after `%`, quoted against the spike 7 parse trap.
fn size_query(pane: PaneId) -> String {
    format!("display-message -p -t \"{pane}\" \"#{{pane_width}} #{{pane_height}}\"\n")
}

/// `cols rows`.
fn parse_size(line: &[u8]) -> Option<(u16, u16)> {
    let text = std::str::from_utf8(line).ok()?;
    let mut f = text.split_whitespace();
    Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_size_reply_parses() {
        assert_eq!(parse_size(b"40 20"), Some((40, 20)));
        assert_eq!(parse_size(b"40"), None);
        assert_eq!(
            size_query(PaneId(7)),
            "display-message -p -t \"%7\" \"#{pane_width} #{pane_height}\"\n"
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
