//! Replay of the hook spool: events a hook script could not hand to a daemon.
//!
//! `ainb-hook.sh` appends a status event to
//! `<hangar_home>/hangar/hook-spool/<pane-key-or-session>.jsonl` when the
//! daemon certainly did not record it (nothing listening, 429, 503). The
//! listener drains the spool when it starts, through the same sink a live
//! call uses, so a replayed event reduces exactly like the original would
//! have.
//!
//! Safety:
//! - Replay is idempotent. Each line carries the event id the script minted
//!   and also sent as `X-Ainb-Event-Id`; the ingest is keyed by that id, so an
//!   event the daemon did record before it answered 503 is not recorded twice.
//!   A line from an older script with no id is keyed by a hash of the line.
//! - Holds never replay. `PermissionRequest` and the tool events are refused
//!   here even if a file names them (the script never spools them): a
//!   replayed approval would be a phantom.
//! - The spool directory must be this user's, not group or world writable.
//!   Each file is opened `O_NOFOLLOW` and checked ON THE OPEN DESCRIPTOR: a
//!   regular file this user owns, mode `0600` or tighter, one link, at most
//!   [`MAX_SPOOL_FILE`], and younger than [`MAX_SPOOL_AGE`] (an older one is
//!   removed unreplayed). Lines over the listener's body cap are skipped.
//! - A file is moved aside before it is read, by link-then-unlink under a
//!   unique, time-ordered name, so a hook appending meanwhile starts a fresh
//!   file and no aside is ever overwritten.
//! - A store fault (503 from the sink) stops the WHOLE drain and keeps every
//!   file not yet finished; the next start replays them, oldest first,
//!   harmlessly by id.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use ainb_hangar_proto::hooks::{
    HookSource, PaneKey, SPOOL_DIR_NAME, SpoolLine, is_replayable, is_valid_event_id,
};

use super::{HookEvent, HookReply, HookSink};

/// Largest spool file the drain reads. The script stops appending at 5 MiB.
pub const MAX_SPOOL_FILE: u64 = 6 * 1024 * 1024;

/// Oldest spool file the drain replays; the script also resets a file this
/// old. A week-old status event would only move a session backwards.
pub const MAX_SPOOL_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// Marker in the name a file is renamed to while it drains.
const DRAINING: &str = ".draining.";

/// What one drain did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Files fully replayed and removed.
    pub files_done: usize,
    /// Files kept for the next start (a store fault, or refused unread).
    pub files_kept: usize,
    /// Lines handed to the sink and recorded.
    pub replayed: usize,
    /// Lines refused: holds, tool events, unknown sources, malformed lines.
    pub skipped: usize,
    /// Lines whose event id was already recorded with different content (a
    /// live event later spooled): nothing to replay, and the drain moves on.
    pub already_recorded: usize,
}

/// Drain every spool file under `hangar_home` through `sink`.
pub async fn drain_spool(hangar_home: &Path, sink: &dyn HookSink) -> DrainReport {
    let dir = hangar_home.join("hangar").join(SPOOL_DIR_NAME);
    let mut report = DrainReport::default();
    if !dir_is_ours(&dir) {
        return report;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return report;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| is_spool_name(p))
        .collect();
    // Asides (`.<name>.draining.<ms>-<uuid>`) sort before live files, and
    // among one file's asides by time: oldest events replay first.
    files.sort();
    for path in files {
        let Some(aside) = claim(&path) else {
            report.files_kept += 1;
            continue;
        };
        let Some(text) = read_checked(&aside).await else {
            // An aside we could not read (loosened mode, a second link) is
            // still only ours to drop: remove it once it is past its age.
            if aside_expired(&aside) {
                let _ = std::fs::remove_file(&aside);
            } else {
                report.files_kept += 1;
            }
            continue;
        };
        match drain_text(&text, sink, &mut report).await {
            Drained::Done => {
                let _ = std::fs::remove_file(&aside);
                report.files_done += 1;
            }
            Drained::Expired => {
                let _ = std::fs::remove_file(&aside);
            }
            Drained::StoreFault => {
                // Stop everything: the store is not taking events, and every
                // file left untouched keeps its place for the next start.
                report.files_kept += 1;
                break;
            }
        }
    }
    if report != DrainReport::default() {
        tracing::info!(?report, "hook spool drained");
    }
    report
}

/// The spool directory is a real directory this user owns that nobody else
/// can write.
fn dir_is_ours(dir: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    let ok = meta.file_type().is_dir()
        && meta.uid() == nix::unistd::geteuid().as_raw()
        && meta.mode() & 0o022 == 0;
    if !ok {
        tracing::warn!(dir = %dir.display(), "hook spool: refusing a directory that is not ours or is writable by others");
    }
    ok
}

/// A spool file (`*.jsonl`) or one moved aside by an earlier start.
fn is_spool_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".jsonl") || n.contains(DRAINING))
}

/// Move a live spool file aside under a unique, time-ordered name, never
/// onto an existing file (link, then unlink the original). An aside is
/// already claimed. `None` leaves the file where it is.
fn claim(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    if name.contains(DRAINING) {
        return Some(path.to_path_buf());
    }
    // Refuse a symlink before touching it; the descriptor checks follow.
    if !std::fs::symlink_metadata(path).ok()?.file_type().is_file() {
        tracing::warn!(path = %path.display(), "hook spool: refusing a non-regular file");
        return None;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    // Time, then a per-process sequence (two claims in one millisecond keep
    // their order), then a uuid (two processes never collide).
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let aside = path.with_file_name(format!(
        ".{name}{DRAINING}{now_ms:013}-{seq:010}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    // `hard_link` fails if `aside` exists, so nothing is ever overwritten.
    std::fs::hard_link(path, &aside).ok()?;
    if std::fs::remove_file(path).is_err() {
        let _ = std::fs::remove_file(&aside);
        return None;
    }
    Some(aside)
}

/// Open `path` without following a symlink and check the open descriptor:
/// this user's regular file, `0600` or tighter, one link, within the size
/// cap. `Some("")` for a file past [`MAX_SPOOL_AGE`], which the caller
/// removes unreplayed. Read on a blocking thread.
async fn read_checked(path: &Path) -> Option<Vec<u8>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&path)
            .ok()?;
        let meta = file.metadata().ok()?;
        let ok = meta.file_type().is_file()
            && meta.uid() == nix::unistd::geteuid().as_raw()
            && meta.mode() & 0o077 == 0
            && meta.nlink() == 1
            && meta.len() <= MAX_SPOOL_FILE;
        if !ok {
            tracing::warn!(path = %path.display(), "hook spool: refusing a file that is not ours, not private, linked elsewhere, or too large");
            return None;
        }
        let expired = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > MAX_SPOOL_AGE);
        if expired {
            return Some(Vec::new());
        }
        // Bytes, not a String: one invalid UTF-8 byte must cost one line,
        // not the whole file.
        let mut bytes = Vec::new();
        file.take(MAX_SPOOL_FILE).read_to_end(&mut bytes).ok()?;
        Some(bytes)
    })
    .await
    .ok()
    .flatten()
}

/// Whether `path` is an aside (a name this drain gave) older than
/// [`MAX_SPOOL_AGE`]. Unlinking a name removes only that link.
fn aside_expired(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(DRAINING))
        && std::fs::symlink_metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > MAX_SPOOL_AGE)
}

/// How replaying one file ended.
enum Drained {
    /// Every line was replayed or refused.
    Done,
    /// The file was past its age; nothing replayed.
    Expired,
    /// The sink answered 503; stop the drain.
    StoreFault,
}

/// Replay one file's lines.
async fn drain_text(bytes: &[u8], sink: &dyn HookSink, report: &mut DrainReport) -> Drained {
    if bytes.is_empty() {
        return Drained::Expired;
    }
    for raw in bytes.split(|&b| b == b'\n') {
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let event = std::str::from_utf8(raw)
            .ok()
            .filter(|l| l.len() <= super::MAX_BODY)
            .and_then(replayable_event);
        let Some(event) = event else {
            report.skipped += 1;
            continue;
        };
        match sink.ingest(event).await {
            // Only a store fault stops the drain.
            HookReply::Unavailable => return Drained::StoreFault,
            HookReply::AlreadyRecorded => report.already_recorded += 1,
            HookReply::Rejected => report.skipped += 1,
            HookReply::NoContent | HookReply::Json(_) => report.replayed += 1,
        }
    }
    Drained::Done
}

/// The status event one spool line replays, or `None` when it must not be.
fn replayable_event(raw: &str) -> Option<HookEvent> {
    let line: SpoolLine = serde_json::from_str(raw).ok()?;
    let event = if line.event.is_empty() {
        line.payload.get("hook_event_name")?.as_str()?.to_string()
    } else {
        line.event.clone()
    };
    let payload_event = line
        .payload
        .get("hook_event_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&event);
    if !is_replayable(&event) || !is_replayable(payload_event) || !line.payload.is_object() {
        return None;
    }
    let source = HookSource::from_route(&line.source)?;
    let event_id = line.event_id.filter(|id| is_valid_event_id(id)).unwrap_or_else(|| {
        // An older script's line: its content names it.
        ainb_hangar_core::token::sha256_hex(raw)[..32].to_string()
    });
    let non_empty = |s: String| (!s.is_empty()).then_some(s);
    Some(HookEvent {
        source,
        hold: false,
        pane_key: PaneKey::parse(&line.pane_key).ok(),
        tmux_pane: non_empty(line.tmux_pane).filter(|p| super::is_tmux_pane_id(p)),
        parent: non_empty(line.parent).filter(|p| p.len() <= 256),
        payload: line.payload,
        event_id: Some(event_id),
        // Always set: it is what marks the event as a replay for the sink.
        received_at_ms: Some(if line.received_at_ms > 0 {
            line.received_at_ms
        } else {
            ainb_hangar_core::clock::HangarClock::now_ms(&ainb_hangar_core::clock::SystemClock)
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records what it is handed; answers `reply`.
    struct Recorder {
        seen: Mutex<Vec<HookEvent>>,
        reply: HookReply,
    }

    impl HookSink for Recorder {
        fn ingest(
            &self,
            event: HookEvent,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HookReply> + Send + '_>> {
            self.seen.lock().unwrap().push(event);
            let reply = self.reply.clone();
            Box::pin(async move { reply })
        }
    }

    fn recorder(reply: HookReply) -> Recorder {
        Recorder {
            seen: Mutex::new(Vec::new()),
            reply,
        }
    }

    fn line(event: &str, id: Option<&str>) -> String {
        serde_json::json!({
            "v": 1, "source": "claude", "event": event, "pane_key": "v1:p-1",
            "tmux_pane": "%4", "parent": "", "received_at_ms": 1_790_000_000_000_i64,
            "event_id": id, "payload": {"hook_event_name": event, "session_id": "s1"}
        })
        .to_string()
    }

    /// Write a spool file as the script does: 0600 in a 0700 directory.
    fn spool(home: &Path, name: &str, lines: &[String]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = home.join("hangar").join(SPOOL_DIR_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    #[tokio::test]
    async fn status_lines_replay_in_order_with_their_ids_and_the_file_goes() {
        let home = tempfile::tempdir().unwrap();
        let path = spool(
            home.path(),
            "v1:p-1.jsonl",
            &[
                line("SessionStart", Some("aaaaaaaa-0001")),
                line("PermissionRequest", Some("aaaaaaaa-0002")),
                line("PostToolUse", Some("aaaaaaaa-0003")),
                line("Stop", Some("aaaaaaaa-0004")),
                "not json".to_string(),
                line("Notification", None),
            ],
        );
        let sink = recorder(HookReply::NoContent);
        let report = drain_spool(home.path(), &sink).await;
        assert_eq!(report.replayed, 3);
        assert_eq!(report.skipped, 3, "the hold, the tool event, the junk");
        assert_eq!(report.files_done, 1);
        assert!(!path.exists());
        let seen = sink.seen.lock().unwrap();
        let names: Vec<_> = seen
            .iter()
            .map(|e| e.payload["hook_event_name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["SessionStart", "Stop", "Notification"]);
        assert!(seen.iter().all(|e| !e.hold));
        assert_eq!(seen[0].event_id.as_deref(), Some("aaaaaaaa-0001"));
        assert_eq!(seen[0].received_at_ms, Some(1_790_000_000_000));
        assert_eq!(seen[0].tmux_pane.as_deref(), Some("%4"));
        let hashed = seen[2].event_id.clone().unwrap();
        assert_eq!(hashed.len(), 32, "an id-less line is named by its content");
        assert_eq!(
            replayable_event(&line("Notification", None)).unwrap().event_id,
            Some(hashed),
            "stably"
        );
    }

    /// Answers 503 for the first `faults` calls, then records.
    struct Flaky {
        faults: Mutex<usize>,
        seen: Mutex<Vec<String>>,
    }

    impl HookSink for Flaky {
        fn ingest(
            &self,
            event: HookEvent,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HookReply> + Send + '_>> {
            let mut faults = self.faults.lock().unwrap();
            let reply = if *faults > 0 {
                *faults -= 1;
                HookReply::Unavailable
            } else {
                self.seen.lock().unwrap().push(event.event_id.unwrap());
                HookReply::NoContent
            };
            Box::pin(async move { reply })
        }
    }

    fn spool_dir(home: &Path) -> PathBuf {
        home.join("hangar").join(SPOOL_DIR_NAME)
    }

    #[tokio::test]
    async fn a_store_fault_stops_the_drain_and_a_restart_loses_nothing() {
        let home = tempfile::tempdir().unwrap();
        spool(
            home.path(),
            "a.jsonl",
            &[line("Stop", Some("aaaaaaaa-0001"))],
        );
        spool(
            home.path(),
            "b.jsonl",
            &[line("Stop", Some("bbbbbbbb-0001"))],
        );
        let sink = Flaky {
            faults: Mutex::new(1),
            seen: Mutex::new(Vec::new()),
        };
        let report = drain_spool(home.path(), &sink).await;
        assert_eq!(report.replayed, 0, "the drain stopped at the first fault");
        assert!(
            spool_dir(home.path()).join("b.jsonl").exists(),
            "b was never touched"
        );

        // The hook spools more for `a` while the daemon is down again.
        let dir = spool_dir(home.path());
        spool(
            home.path(),
            "a.jsonl",
            &[line("Stop", Some("aaaaaaaa-0002"))],
        );
        // Another start faults again on the oldest aside and stops there: the
        // new `a.jsonl` is left untouched, the first aside is not overwritten.
        let report = drain_spool(
            home.path(),
            &Flaky {
                faults: Mutex::new(1),
                seen: Mutex::new(Vec::new()),
            },
        )
        .await;
        assert_eq!(report.replayed, 0);

        // A clean start replays everything, oldest aside first, nothing lost.
        let report = drain_spool(home.path(), &sink).await;
        assert_eq!(report.replayed, 3, "{report:?}");
        let seen = sink.seen.lock().unwrap().clone();
        assert_eq!(seen, ["aaaaaaaa-0001", "aaaaaaaa-0002", "bbbbbbbb-0001"]);
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
    }

    #[test]
    fn claiming_never_overwrites_an_existing_aside() {
        let home = tempfile::tempdir().unwrap();
        let first = claim(&spool(
            home.path(),
            "c.jsonl",
            &[line("Stop", Some("cccccccc-0001"))],
        ))
        .unwrap();
        let second = claim(&spool(
            home.path(),
            "c.jsonl",
            &[line("Stop", Some("cccccccc-0002"))],
        ))
        .unwrap();
        assert_ne!(first, second);
        assert!(std::fs::read_to_string(&first).unwrap().contains("cccccccc-0001"));
        assert!(std::fs::read_to_string(&second).unwrap().contains("cccccccc-0002"));
        assert!(first < second, "asides sort by time");
    }

    #[tokio::test]
    async fn loose_files_and_directories_are_refused_and_old_files_expire() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = tempfile::tempdir().unwrap();
        let loose = spool(
            home.path(),
            "loose.jsonl",
            &[line("Stop", Some("dddddddd-0001"))],
        );
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o644)).unwrap();
        let linked = spool(
            home.path(),
            "linked.jsonl",
            &[line("Stop", Some("dddddddd-0002"))],
        );
        std::fs::hard_link(&linked, home.path().join("elsewhere")).unwrap();
        let old = spool(
            home.path(),
            "old.jsonl",
            &[line("Stop", Some("dddddddd-0003"))],
        );
        let week_ago =
            std::time::SystemTime::now() - MAX_SPOOL_AGE - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(week_ago)
            .unwrap();
        let sink = recorder(HookReply::NoContent);
        let report = drain_spool(home.path(), &sink).await;
        assert_eq!(report.replayed, 0, "{report:?}");
        assert_eq!(report.files_kept, 2, "0644 and a second link are refused");
        assert!(!old.exists(), "an expired file is removed unreplayed");

        // A group-writable spool directory is refused whole.
        let dir = spool_dir(home.path());
        spool(
            home.path(),
            "ok.jsonl",
            &[line("Stop", Some("dddddddd-0004"))],
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert_eq!(
            drain_spool(home.path(), &sink).await,
            DrainReport::default()
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(drain_spool(home.path(), &sink).await.replayed, 1);
    }

    #[tokio::test]
    async fn a_symlink_is_never_read() {
        let home = tempfile::tempdir().unwrap();
        let elsewhere = home.path().join("secret.jsonl");
        std::fs::write(
            &elsewhere,
            format!("{}\n", line("Stop", Some("cccccccc-0001"))),
        )
        .unwrap();
        let dir = home.path().join("hangar").join(SPOOL_DIR_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("link.jsonl")).unwrap();
        let sink = recorder(HookReply::NoContent);
        let report = drain_spool(home.path(), &sink).await;
        assert_eq!((report.replayed, report.files_kept), (0, 1));
        assert!(elsewhere.exists());
    }

    #[test]
    fn a_hold_is_refused_even_when_only_the_payload_names_it() {
        let sneaky = serde_json::json!({
            "v": 1, "source": "claude", "event": "Notification",
            "payload": {"hook_event_name": "PermissionRequest", "session_id": "s"}
        })
        .to_string();
        assert!(replayable_event(&sneaky).is_none());
        let unknown = line("Stop", None).replace("\"claude\"", "\"nobody\"");
        assert!(replayable_event(&unknown).is_none());
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;
    use std::sync::Mutex;

    /// Answers `AlreadyRecorded` for one id, records the rest.
    struct Recorded {
        known: String,
        seen: Mutex<Vec<String>>,
    }

    impl HookSink for Recorded {
        fn ingest(
            &self,
            event: HookEvent,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HookReply> + Send + '_>> {
            let id = event.event_id.unwrap();
            let reply = if id == self.known {
                HookReply::AlreadyRecorded
            } else {
                self.seen.lock().unwrap().push(id);
                HookReply::NoContent
            };
            Box::pin(async move { reply })
        }
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn stop(id: &str) -> String {
        serde_json::json!({"v": 1, "source": "claude", "event": "Stop", "event_id": id,
                           "payload": {"hook_event_name": "Stop", "session_id": "s"}})
        .to_string()
    }

    #[tokio::test]
    async fn an_already_recorded_line_is_skipped_and_the_drain_goes_on() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("hangar").join(SPOOL_DIR_NAME);
        write(
            &dir,
            "a.jsonl",
            format!("{}\n", stop("aaaaaaaa-known")).as_bytes(),
        );
        write(
            &dir,
            "b.jsonl",
            format!("{}\n", stop("bbbbbbbb-0001")).as_bytes(),
        );
        let sink = Recorded {
            known: "aaaaaaaa-known".into(),
            seen: Mutex::new(Vec::new()),
        };
        let report = drain_spool(home.path(), &sink).await;
        assert_eq!(
            (report.already_recorded, report.replayed, report.files_done),
            (1, 1, 2)
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "both files gone"
        );
    }

    #[tokio::test]
    async fn one_invalid_utf8_byte_costs_one_line() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("hangar").join(SPOOL_DIR_NAME);
        let mut bytes = format!("{}\n", stop("cccccccc-0001")).into_bytes();
        bytes.extend_from_slice(b"{\"v\":1,\"source\":\"claude\",\"event\":\"Stop\xff\"}\n");
        bytes.extend_from_slice(format!("{}\n", stop("cccccccc-0002")).as_bytes());
        write(&dir, "c.jsonl", &bytes);
        let sink = Recorded {
            known: String::new(),
            seen: Mutex::new(Vec::new()),
        };
        let report = drain_spool(home.path(), &sink).await;
        assert_eq!((report.replayed, report.skipped), (2, 1));
        assert_eq!(
            sink.seen.lock().unwrap().clone(),
            ["cccccccc-0001", "cccccccc-0002"]
        );
    }

    #[tokio::test]
    async fn an_unreadable_aside_expires() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("hangar").join(SPOOL_DIR_NAME);
        let name = format!(".x.jsonl{DRAINING}0000000000001-0000000000-abc");
        write(
            &dir,
            &name,
            format!("{}\n", stop("dddddddd-0001")).as_bytes(),
        );
        let aside = dir.join(&name);
        std::fs::set_permissions(&aside, std::fs::Permissions::from_mode(0o644)).unwrap();
        let sink = Recorded {
            known: String::new(),
            seen: Mutex::new(Vec::new()),
        };
        assert_eq!(
            drain_spool(home.path(), &sink).await.files_kept,
            1,
            "young: kept"
        );
        let old = std::time::SystemTime::now() - MAX_SPOOL_AGE - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&aside)
            .unwrap()
            .set_modified(old)
            .unwrap();
        drain_spool(home.path(), &sink).await;
        assert!(!aside.exists(), "an old unreadable aside is removed");
        assert!(sink.seen.lock().unwrap().is_empty());
    }
}
