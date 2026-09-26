//! R2 WP8: the per-pane control-mode feed against a REAL tmux server.
//!
//! Every test drives its own tmux server on a private socket path
//! (`tmux -S <tmpdir>/feed.sock`, `TMUX` unset), creates sessions it names,
//! and kills only those names and only that server, printing each target
//! before the kill. Self-skips with `SKIP:` when tmux is not installed; the
//! nightly tmux lane fails on that line so the proof never ships vacuously.
//!
//! What is proved:
//! * the seed is ordered against the tail: the emulator built from the seed
//!   plus the live tail matches `capture-pane` row for row;
//! * `pause-after` is asserted at boot, and the feed client never sends
//!   `refresh-client -C`;
//! * a real `%pause` (the reader stalled behind a flood) is followed by the
//!   feed's own `continue`, a re-seed under a new epoch, and 0 wrong rows;
//! * a `%continue` printed by the pane, which arrives inside a reply body,
//!   never triggers a re-seed;
//! * a killed client reattaches with `feed_lost`, and stamps the fleet row
//!   `restored_unconfirmed`;
//! * a killed session ends the feed with `session_gone` then `closed`;
//! * the held grapheme is flushed on idle;
//! * a session name carrying a quote and a semicolon never reaches the
//!   control stream, so it cannot inject a command;
//! * a resize read while the seed is in flight is never lost.

#![cfg(feature = "terminal-stream")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ainb_hangar_daemon::term::feed::{FeedConfig, FeedEvent, FeedHandle, GapReason};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::fleet::{
    FleetRepo, FleetSessionPatch, NewFleetEvent, ObservationAuthority,
};
use ainb_term::canon::canon;
use ainb_term::emulator::PaneEmulator;
use tokio::sync::broadcast;

/// A private tmux server. Dropping it kills that server and nothing else.
struct Tmux {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    sessions: Vec<String>,
}

impl Tmux {
    fn available() -> bool {
        Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
    }

    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("feed.sock");
        Self {
            _dir: dir,
            socket,
            sessions: Vec::new(),
        }
    }

    fn cmd(&self, args: &[&str]) -> String {
        let out = Command::new("tmux")
            .env_remove("TMUX")
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .output()
            .expect("tmux runs");
        assert!(
            out.status.success(),
            "tmux {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A detached session running `command` at `cols` x `rows`, status off.
    fn session(&mut self, name: &str, command: &str, cols: u16, rows: u16) -> String {
        let name = format!("{name}-{}", std::process::id());
        self.cmd(&[
            "new-session",
            "-d",
            "-s",
            &name,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
            command,
        ]);
        self.cmd(&["set-option", "-t", &name, "status", "off"]);
        self.sessions.push(name.clone());
        name
    }

    fn send(&self, session: &str, keys: &str) {
        self.cmd(&["send-keys", "-t", session, "-l", keys]);
        self.cmd(&["send-keys", "-t", session, "Enter"]);
    }

    fn capture(&self, session: &str) -> Vec<String> {
        self.cmd(&["capture-pane", "-p", "-t", session])
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn kill_session(&mut self, session: &str) {
        eprintln!("kill-session {session} on {}", self.socket.display());
        self.cmd(&["kill-session", "-t", &format!("={session}")]);
        self.sessions.retain(|s| s != session);
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        eprintln!(
            "kill-server on OWN private socket {}",
            self.socket.display()
        );
        let _ = Command::new("tmux")
            .env_remove("TMUX")
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .output();
    }
}

fn config(tmux: &Tmux, session: &str) -> FeedConfig {
    let mut cfg = FeedConfig::new(session.to_string());
    cfg.socket = Some(tmux.socket.clone());
    cfg
}

async fn next_matching(
    rx: &mut broadcast::Receiver<FeedEvent>,
    timeout: Duration,
    pred: impl Fn(&FeedEvent) -> bool,
) -> FeedEvent {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "no matching feed event within {timeout:?}");
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(event)) if pred(&event) => return event,
            Ok(Ok(_)) => {}
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) => panic!("feed closed its channel"),
            Err(_) => panic!("no matching feed event within {timeout:?}"),
        }
    }
}

/// The emulator's viewport as text rows, the way `capture-pane -p` prints
/// them (trailing blanks trimmed).
fn rows_of(handle: &FeedHandle) -> Vec<String> {
    let mut guard = handle.pane().lock().unwrap();
    let pane = guard.emulator.as_mut().expect("seeded");
    let c = canon(pane, 0);
    let (_, rows) = pane.size();
    (0..usize::from(rows)).map(|r| c.row_text(r).trim_end().to_string()).collect()
}

fn trimmed(rows: Vec<String>) -> Vec<String> {
    rows.into_iter().map(|r| r.trim_end().to_string()).collect()
}

/// Wait until the pane stops changing.
fn wait_quiet(tmux: &Tmux, session: &str) -> Vec<String> {
    let mut last = tmux.capture(session);
    let mut stable = 0;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(150));
        let now = tmux.capture(session);
        if now == last {
            stable += 1;
            if stable >= 3 {
                return now;
            }
        } else {
            stable = 0;
            last = now;
        }
    }
    panic!("pane never went quiet");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_seed_is_ordered_against_the_tail_and_the_feed_never_resizes() {
    if !Tmux::available() {
        eprintln!("SKIP: tmux not installed");
        return;
    }
    let mut tmux = Tmux::start();
    let session = tmux.session("seed", "sh", 60, 12);
    tmux.send(&session, "for i in 1 2 3 4 5; do echo before-$i; done");
    std::thread::sleep(Duration::from_millis(300));

    let handle = FeedHandle::spawn(config(&tmux, &session));
    let mut rx = handle.subscribe();
    let seeded = next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, FeedEvent::Seeded { .. })
    })
    .await;
    assert_eq!(
        seeded,
        FeedEvent::Seeded {
            epoch: 1,
            cols: 60,
            rows: 12
        }
    );
    assert_eq!(
        rows_of(&handle),
        trimmed(tmux.capture(&session)),
        "the seeded grid matches tmux's"
    );

    // The tail: new output after the seed reaches the emulator in order.
    tmux.send(&session, "for i in 6 7 8; do echo after-$i; done");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_output = false;
    loop {
        if let Ok(Ok(FeedEvent::Output { data, .. })) =
            tokio::time::timeout(Duration::from_millis(300), rx.recv()).await
        {
            saw_output = true;
            if data.windows(7).any(|w| w == b"after-8") {
                break;
            }
        }
        assert!(Instant::now() < deadline, "tail never arrived");
    }
    assert!(saw_output);
    // The snapshot's offset equals the bytes the emulator holds, read under
    // the one lock, and matches what the status reports afterwards.
    let (epoch, seq, cols, rows, bytes) = handle.snapshot(0).expect("seeded");
    assert_eq!((epoch, cols, rows), (1, 60, 12));
    assert!(seq > 0 && !bytes.is_empty());
    assert!(handle.status().seq >= seq);
    let expected = wait_quiet(&tmux, &session);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(rows_of(&handle), trimmed(expected));

    // Boot assertion and the rule the feed keeps.
    let commands = handle.commands();
    assert_eq!(commands[0], "refresh-client -f pause-after=2");
    assert!(
        commands[1].starts_with("display-message -p -t"),
        "{commands:?}"
    );
    assert!(
        commands.iter().all(|c| !c.contains("-C ") && !c.contains("resize-window")),
        "the feed client never resizes: {commands:?}"
    );
    assert_eq!(
        commands.iter().filter(|c| c.starts_with("capture-pane")).count(),
        1,
        "one seed"
    );
    assert_eq!(handle.status().attaches, 1);
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_pause_is_continued_and_reseeded_with_no_wrong_row() {
    if !Tmux::available() {
        eprintln!("SKIP: tmux not installed");
        return;
    }
    let mut tmux = Tmux::start();
    let session = tmux.session("pause", "sh", 80, 40);
    let gate = Arc::new(tokio::sync::RwLock::new(()));
    let mut cfg = config(&tmux, &session);
    cfg.pause_after_s = 1;
    cfg.read_gate = Some(Arc::clone(&gate));
    let handle = FeedHandle::spawn(cfg);
    let mut rx = handle.subscribe();
    next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, FeedEvent::Seeded { epoch: 1, .. })
    })
    .await;

    // Stall the reader, flood the pane past the pipe, and let tmux's age
    // clock pass pause-after.
    let stall = gate.write().await;
    tmux.send(&session, "seq 1 300000; echo FLOOD-DONE");
    tokio::time::sleep(Duration::from_millis(3500)).await;
    drop(stall);

    next_matching(&mut rx, Duration::from_secs(10), |e| {
        matches!(
            e,
            FeedEvent::Gap {
                reason: GapReason::Paused,
                ..
            }
        )
    })
    .await;
    next_matching(&mut rx, Duration::from_secs(10), |e| {
        matches!(e, FeedEvent::Seeded { epoch: 2, .. })
    })
    .await;
    let expected = wait_quiet(&tmux, &session);
    assert!(expected.iter().any(|r| r == "FLOOD-DONE"), "{expected:?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let got = rows_of(&handle);
    let expected = trimmed(expected);
    let wrong = got.iter().zip(expected.iter()).filter(|(a, b)| a != b).count();
    assert_eq!(
        wrong, 0,
        "rows wrong after the re-seed:\n got {got:?}\n want {expected:?}"
    );
    let commands = handle.commands();
    let continues = commands.iter().filter(|c| c.ends_with(":continue\"")).count();
    assert!(
        continues >= 1,
        "the feed sent its own continue: {commands:?}"
    );
    assert!(
        commands.iter().filter(|c| c.starts_with("capture-pane")).count() >= 2,
        "the resume re-seeded: {commands:?}"
    );
    assert!(!handle.status().paused);
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_continue_printed_by_the_pane_never_reseeds() {
    if !Tmux::available() {
        eprintln!("SKIP: tmux not installed");
        return;
    }
    let mut tmux = Tmux::start();
    let session = tmux.session("forge", "sh", 60, 10);
    // These land in the seed's capture-pane reply body, where the parser
    // lifts them as Pause/Continue events for %0.
    tmux.send(&session, "printf '%s\\n' '%pause %0' '%continue %0'");
    std::thread::sleep(Duration::from_millis(300));

    let handle = FeedHandle::spawn(config(&tmux, &session));
    let mut rx = handle.subscribe();
    next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, FeedEvent::Seeded { epoch: 1, .. })
    })
    .await;
    // The same lines again after the seed: now they are pane OUTPUT, not a
    // notification, and the capture of a later size query would carry them
    // in a body once more.
    tmux.send(&session, "printf '%s\\n' '%continue %0'");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let status = handle.status();
    assert_eq!(status.epoch, 1, "no re-seed");
    assert!(!status.paused);
    let commands = handle.commands();
    assert!(
        commands.iter().all(|c| !c.contains(":continue\"")),
        "no continue was sent: {commands:?}"
    );
    assert_eq!(
        commands.iter().filter(|c| c.starts_with("capture-pane")).count(),
        1
    );
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_client_reattaches_with_feed_lost_and_stamps_the_row() {
    if !Tmux::available() {
        eprintln!("SKIP: tmux not installed");
        return;
    }
    let mut tmux = Tmux::start();
    let session = tmux.session("lost", "sh", 60, 10);
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let key = "claude:feed-lost";
    FleetRepo::apply_event(
        store.pool(),
        &NewFleetEvent {
            event_id: "e-1".to_string(),
            session_key: key.to_string(),
            observed_at: 100,
            authority: ObservationAuthority::Authoritative,
            event_type: "SessionStart".to_string(),
            payload: "{}".to_string(),
            patch: FleetSessionPatch {
                provider: Some("claude".to_string()),
                lifecycle_state: Some("RUNNING".to_string()),
                tier: Some("hook".to_string()),
                tmux_target: Some(session.clone()),
                ..FleetSessionPatch::default()
            },
        },
    )
    .await
    .expect("row applies");

    let mut cfg = config(&tmux, &session);
    cfg.store = Some((store.pool().clone(), key.to_string()));
    let handle = FeedHandle::spawn(cfg);
    let mut rx = handle.subscribe();
    next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, FeedEvent::Seeded { epoch: 1, .. })
    })
    .await;
    let pid = handle.status().client_pid.expect("a control client is running");
    eprintln!("kill -9 the feed's own control client pid {pid}");
    let killed = Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap();
    assert!(killed.success());

    next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(
            e,
            FeedEvent::Gap {
                reason: GapReason::FeedLost,
                ..
            }
        )
    })
    .await;
    next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, FeedEvent::Seeded { epoch: 2, .. })
    })
    .await;
    assert_eq!(handle.status().attaches, 2);
    let row = FleetRepo::get_session(store.pool(), key).await.unwrap().unwrap();
    assert!(
        row.restored_unconfirmed,
        "the watched row is unconfirmed after the gap"
    );
    assert_eq!(rows_of(&handle), trimmed(tmux.capture(&session)));
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_session_ends_the_feed_with_session_gone() {
    if !Tmux::available() {
        eprintln!("SKIP: tmux not installed");
        return;
    }
    let mut tmux = Tmux::start();
    let session = tmux.session("gone", "sh", 60, 10);
    let handle = FeedHandle::spawn(config(&tmux, &session));
    let mut rx = handle.subscribe();
    next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, FeedEvent::Seeded { epoch: 1, .. })
    })
    .await;
    tmux.kill_session(&session);
    next_matching(&mut rx, Duration::from_secs(10), |e| {
        matches!(
            e,
            FeedEvent::Gap {
                reason: GapReason::SessionGone,
                ..
            }
        )
    })
    .await;
    let closed = next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, FeedEvent::Closed { .. })
    })
    .await;
    assert_eq!(
        closed,
        FeedEvent::Closed {
            reason: "session_gone".to_string()
        }
    );
    assert!(handle.status().client_pid.is_none());
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_feed_on_a_missing_session_closes_at_once() {
    if !Tmux::available() {
        eprintln!("SKIP: tmux not installed");
        return;
    }
    let mut tmux = Tmux::start();
    // The server must exist for the lookup to answer at all.
    let _anchor = tmux.session("anchor", "sh", 20, 5);
    let handle = FeedHandle::spawn(config(&tmux, "no-such-session"));
    let mut rx = handle.subscribe();
    next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(
            e,
            FeedEvent::Gap {
                reason: GapReason::SessionGone,
                ..
            }
        )
    })
    .await;
    next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, FeedEvent::Closed { .. })
    })
    .await;
    assert!(handle.commands().is_empty(), "no client was attached");
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_held_grapheme_is_flushed_on_idle() {
    if !Tmux::available() {
        eprintln!("SKIP: tmux not installed");
        return;
    }
    let mut tmux = Tmux::start();
    let session = tmux.session("idle", "sh", 60, 10);
    let handle = FeedHandle::spawn(config(&tmux, &session));
    let mut rx = handle.subscribe();
    next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, FeedEvent::Seeded { epoch: 1, .. })
    })
    .await;
    // Typed text echoes back; the emulator holds its last cluster until the
    // feed goes idle.
    tmux.cmd(&["send-keys", "-t", &session, "-l", "abc"]);
    next_matching(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, FeedEvent::Output { data, .. } if data.ends_with(b"c")),
    )
    .await;
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        let held = handle
            .pane()
            .lock()
            .unwrap()
            .emulator
            .as_ref()
            .is_some_and(PaneEmulator::has_held_text);
        if !held {
            break;
        }
        assert!(Instant::now() < deadline, "held text was never flushed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let rows = rows_of(&handle);
    assert!(rows.iter().any(|r| r.ends_with("abc")), "{rows:?}");
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hostile_session_name_never_reaches_the_control_stream() {
    if !Tmux::available() {
        eprintln!("SKIP: tmux not installed");
        return;
    }
    let mut tmux = Tmux::start();
    // Sent unescaped inside quotes on the control stream, this name closes
    // the quote, runs `set-option`, and reopens it.
    let session = tmux.session("inj\"; set-option -w @pwned 1; echo \"", "sh", 60, 10);
    let handle = FeedHandle::spawn(config(&tmux, &session));
    let mut rx = handle.subscribe();
    next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, FeedEvent::Seeded { epoch: 1, .. })
    })
    .await;
    let pwned = tmux.cmd(&["show-options", "-wqv", "-t", &session, "@pwned"]);
    assert_eq!(pwned.trim(), "", "the injected set-option ran");
    let commands = handle.commands();
    assert!(
        commands.iter().all(|c| !c.contains("set-option") && !c.contains("inj")),
        "the name reached the stream: {commands:?}"
    );
    for c in commands.iter().filter(|c| c.contains("-t ")) {
        let target = c.split("-t ").nth(1).unwrap().split(' ').next().unwrap();
        assert!(
            target.starts_with("\"%") && target.ends_with('"'),
            "a stream command names something other than a quoted pane id: {c}"
        );
    }
    assert_eq!(rows_of(&handle), trimmed(tmux.capture(&session)));
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resize_read_during_the_seed_is_never_lost() {
    if !Tmux::available() {
        eprintln!("SKIP: tmux not installed");
        return;
    }
    let mut tmux = Tmux::start();
    let session = tmux.session("grow", "sh", 60, 12);
    tmux.cmd(&["set-option", "-t", &session, "-w", "window-size", "manual"]);
    tmux.send(&session, "for i in 1 2 3; do echo row-$i; done");
    std::thread::sleep(Duration::from_millis(300));
    let gate = Arc::new(tokio::sync::RwLock::new(()));
    let mut cfg = config(&tmux, &session);
    cfg.read_gate = Some(Arc::clone(&gate));
    // The reader is stalled from the start: the attach and its boot
    // commands go out, the window is resized underneath, then the replies
    // (and the %layout-change between them) are read in one go.
    let stall = gate.write().await;
    let handle = FeedHandle::spawn(cfg);
    let mut rx = handle.subscribe();
    tokio::time::sleep(Duration::from_millis(400)).await;
    tmux.cmd(&["resize-window", "-t", &session, "-x", "100", "-y", "30"]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(stall);

    // Either the seed's own size query saw the new size (the capture was
    // taken at it), or the %layout-change read during the seed was still
    // answered and resized the fresh emulator. Both are correct; losing the
    // resize is the bug.
    let seeded = next_matching(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, FeedEvent::Seeded { epoch: 1, .. })
    })
    .await;
    if seeded
        != (FeedEvent::Seeded {
            epoch: 1,
            cols: 100,
            rows: 30,
        })
    {
        next_matching(&mut rx, Duration::from_secs(3), |e| {
            matches!(
                e,
                FeedEvent::Resize {
                    cols: 100,
                    rows: 30,
                    ..
                }
            )
        })
        .await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let size = handle.pane().lock().unwrap().emulator.as_ref().unwrap().size();
    assert_eq!(size, (100, 30), "the emulator follows the pane's size");
    assert_eq!(rows_of(&handle), trimmed(tmux.capture(&session)));
    assert_eq!(handle.status().epoch, 1, "no second seed was needed");
    handle.stop().await;
}
