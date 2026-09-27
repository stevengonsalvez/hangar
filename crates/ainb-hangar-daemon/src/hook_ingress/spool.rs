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
//! - Only this user's regular files are read (no symlinks), each capped at
//!   [`MAX_SPOOL_FILE`]. A file is renamed aside before it is read, so a hook
//!   appending meanwhile starts a fresh file instead of racing the drain.
//! - A store fault (503 from the sink) stops the drain and keeps the file; the
//!   next start replays it again, harmlessly, by id.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use ainb_hangar_proto::hooks::{
    HookSource, PaneKey, SPOOL_DIR_NAME, SpoolLine, is_replayable, is_valid_event_id,
};

use super::{HookEvent, HookReply, HookSink};

/// Largest spool file the drain reads. The script stops appending at 5 MiB.
pub const MAX_SPOOL_FILE: u64 = 6 * 1024 * 1024;

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
}

/// Drain every spool file under `hangar_home` through `sink`.
pub async fn drain_spool(hangar_home: &Path, sink: &dyn HookSink) -> DrainReport {
    let dir = hangar_home.join("hangar").join(SPOOL_DIR_NAME);
    let mut report = DrainReport::default();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return report;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| is_spool_name(p))
        .collect();
    files.sort();
    for path in files {
        let Some(draining) = claim(&path) else {
            report.files_kept += 1;
            continue;
        };
        if drain_file(&draining, sink, &mut report).await {
            let _ = std::fs::remove_file(&draining);
            report.files_done += 1;
        } else {
            report.files_kept += 1;
        }
    }
    if report != DrainReport::default() {
        tracing::info!(?report, "hook spool drained");
    }
    report
}

/// A spool file (`*.jsonl`) or one left mid-drain by an earlier start.
fn is_spool_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".jsonl") || n.contains(DRAINING))
}

/// Check the file is this user's regular file within the cap, then rename it
/// aside (unless it already is). `None` leaves it where it is.
fn claim(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let ours = meta.uid() == nix::unistd::geteuid().as_raw();
    if !meta.file_type().is_file() || !ours || meta.len() > MAX_SPOOL_FILE {
        tracing::warn!(path = %path.display(), "hook spool: refusing a file that is not ours, not regular, or too large");
        return None;
    }
    let name = path.file_name()?.to_str()?;
    if name.contains(DRAINING) {
        return Some(path.to_path_buf());
    }
    let aside = path.with_file_name(format!(".{name}{DRAINING}{}", std::process::id()));
    std::fs::rename(path, &aside).ok()?;
    Some(aside)
}

/// Replay one claimed file. `false` on a store fault (keep the file).
async fn drain_file(path: &Path, sink: &dyn HookSink, report: &mut DrainReport) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    for raw in text.lines().filter(|l| !l.trim().is_empty()) {
        let Some(event) = replayable_event(raw) else {
            report.skipped += 1;
            continue;
        };
        match sink.ingest(event).await {
            HookReply::Unavailable => return false,
            HookReply::NoContent | HookReply::Json(_) => report.replayed += 1,
        }
    }
    true
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
        received_at_ms: (line.received_at_ms > 0).then_some(line.received_at_ms),
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

    fn spool(home: &Path, name: &str, lines: &[String]) -> PathBuf {
        let dir = home.join("hangar").join(SPOOL_DIR_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
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

    #[tokio::test]
    async fn a_store_fault_keeps_the_file_for_the_next_start() {
        let home = tempfile::tempdir().unwrap();
        spool(
            home.path(),
            "s.jsonl",
            &[line("Stop", Some("bbbbbbbb-0001"))],
        );
        let report = drain_spool(home.path(), &recorder(HookReply::Unavailable)).await;
        assert_eq!(report.files_kept, 1);
        let again = recorder(HookReply::NoContent);
        let report = drain_spool(home.path(), &again).await;
        assert_eq!(
            (report.replayed, report.files_done),
            (1, 1),
            "picked up mid-drain"
        );
        let dir = home.path().join("hangar").join(SPOOL_DIR_NAME);
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
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
