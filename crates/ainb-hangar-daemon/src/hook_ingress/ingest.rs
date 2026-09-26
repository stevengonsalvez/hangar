//! The ingest sink: an HTTP hook call becomes the same canonical hook line the
//! `events.jsonl` tail reads, and runs through the same reduction.
//!
//! One path, two transports. `ainb fleet atc hook` appends a line to
//! `events.jsonl` and the tail feeds it to [`AttentionIngest`]; this sink builds
//! the identical line from the HTTP call and feeds it to the same function, so
//! `fleet_session`, `fleet_provider_event` and the attention inbox cannot tell
//! the two apart. The full payload goes to the same raw sidecar
//! (`hangar/provider-events/<event_id>.json`) and the line embeds at most 3 KiB,
//! as the CLI does.

use std::path::{Path, PathBuf};

use ainb_hangar_proto::hooks::HookSource;
use serde_json::{Value, json};

use super::{HookEvent, HookReply, HookSink};
use crate::attention_ingest::AttentionIngest;

/// Largest payload embedded inline in a line, as `ainb fleet atc hook` caps it.
pub const MAX_INLINE_PAYLOAD: usize = 3 * 1024;

/// Feeds admitted hook calls to the attention ingest.
pub struct IngestSink {
    ingest: AttentionIngest,
    hangar_home: PathBuf,
}

impl std::fmt::Debug for IngestSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IngestSink")
            .field("hangar_home", &self.hangar_home)
            .finish_non_exhaustive()
    }
}

impl IngestSink {
    /// A sink over its own [`AttentionIngest`] (same pool, event sink and
    /// paths as the tail's) and the hangar home the sidecars live under.
    #[must_use]
    pub const fn new(ingest: AttentionIngest, hangar_home: PathBuf) -> Self {
        Self {
            ingest,
            hangar_home,
        }
    }
}

impl HookSink for IngestSink {
    fn ingest(
        &self,
        event: HookEvent,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HookReply> + Send + '_>> {
        Box::pin(async move {
            let now_ms =
                ainb_hangar_core::clock::HangarClock::now_ms(&ainb_hangar_core::clock::SystemClock);
            let event_id = uuid::Uuid::new_v4().to_string();
            let raw = event.payload.to_string();
            let stored = persist_sidecar(&self.hangar_home, &event_id, &raw).is_ok();
            let line = event_line(&event, &event_id, now_ms, &raw, stored);
            if self.ingest.ingest_line(&line.to_string(), now_ms).await {
                HookReply::NoContent
            } else {
                // A store fault: 503 makes the hook spool the event, and the
                // next daemon start replays it.
                HookReply::Unavailable
            }
        })
    }
}

/// The canonical hook line for one HTTP call: the same fields, in the same
/// shape, as `ainb fleet atc hook` appends to `events.jsonl`.
///
/// The pane address (`tmux_target`, `process_start_fingerprint`) is left null:
/// the daemon's pane binding resolves it from `(provider, cwd)` exactly as it
/// does for a hook that could not name its pane. The pane KEY rides along for
/// the exact-pane work.
#[must_use]
pub fn event_line(
    event: &HookEvent,
    event_id: &str,
    now_ms: i64,
    raw: &str,
    raw_stored: bool,
) -> Value {
    let p = &event.payload;
    let text = |key: &str| p.get(key).and_then(Value::as_str).unwrap_or_default();
    let base_event = text("hook_event_name").split(':').next().unwrap_or_default();
    let agent = match event.source {
        HookSource::Codex => "codex",
        // `ainb fleet atc hook` labels every other provider `claude`; the
        // ingest re-derives Codex from the transcript file name regardless.
        _ => "claude",
    };
    let inline = if raw.len() <= MAX_INLINE_PAYLOAD {
        p.clone()
    } else {
        json!({ "_truncated": true, "_bytes": raw.len() })
    };
    json!({
        "event_id": event_id,
        "ts": now_ms,
        "session_id": text("session_id"),
        "cwd": text("cwd"),
        "transcript_path": text("transcript_path"),
        "agent": agent,
        "event_type": base_event,
        "matcher": matcher_of(base_event, p),
        "parent": event.parent,
        "tmux_target": Value::Null,
        "process_start_fingerprint": Value::Null,
        "pane_key": event.pane_key.as_ref().map(ainb_hangar_proto::hooks::PaneKey::as_str),
        "payload": inline,
        "raw_payload_ref": raw_stored.then_some(event_id),
    })
}

/// The event's discriminator, read from the payload as `ainb fleet atc hook`
/// resolves it: the tool for tool and permission events, the notification
/// type, or the stop-failure error.
fn matcher_of(base_event: &str, payload: &Value) -> Option<String> {
    let key = match base_event {
        "PreToolUse" | "PermissionRequest" => "tool_name",
        "Notification" => "notification_type",
        "StopFailure" => {
            return payload
                .get("error")
                .or_else(|| payload.get("error_type"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        _ => return None,
    };
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Write the full payload where the ingest reads raw sidecars, by temp file
/// and rename so a concurrent reader never sees half of it.
fn persist_sidecar(hangar_home: &Path, event_id: &str, raw: &str) -> std::io::Result<()> {
    let dir = hangar_home.join("hangar").join("provider-events");
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(".{event_id}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, raw)?;
    std::fs::rename(tmp, dir.join(format!("{event_id}.json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(source: HookSource, payload: Value) -> HookEvent {
        HookEvent {
            source,
            hold: false,
            pane_key: ainb_hangar_proto::hooks::PaneKey::parse("v1:s-1").ok(),
            tmux_pane: Some("%3".into()),
            parent: Some("parent-1".into()),
            payload,
        }
    }

    #[test]
    fn the_line_has_the_cli_shape() {
        let e = event(
            HookSource::Claude,
            json!({
                "hook_event_name": "PreToolUse",
                "session_id": "abc",
                "cwd": "/w",
                "transcript_path": "/t.jsonl",
                "tool_name": "AskUserQuestion"
            }),
        );
        let raw = e.payload.to_string();
        let line = event_line(&e, "ev-1", 42, &raw, true);
        assert_eq!(line["event_id"], "ev-1");
        assert_eq!(line["ts"], 42);
        assert_eq!(line["session_id"], "abc");
        assert_eq!(line["cwd"], "/w");
        assert_eq!(line["transcript_path"], "/t.jsonl");
        assert_eq!(line["agent"], "claude");
        assert_eq!(line["event_type"], "PreToolUse");
        assert_eq!(line["matcher"], "AskUserQuestion");
        assert_eq!(line["parent"], "parent-1");
        assert_eq!(line["pane_key"], "v1:s-1");
        assert_eq!(line["raw_payload_ref"], "ev-1");
        assert_eq!(line["payload"]["tool_name"], "AskUserQuestion");
        assert!(line["tmux_target"].is_null());
    }

    #[test]
    fn codex_is_labelled_codex_and_matchers_follow_the_event() {
        let e = event(
            HookSource::Codex,
            json!({"hook_event_name": "Notification", "notification_type": "idle_prompt"}),
        );
        let line = event_line(&e, "ev", 1, "{}", false);
        assert_eq!(line["agent"], "codex");
        assert_eq!(line["matcher"], "idle_prompt");
        assert!(line["raw_payload_ref"].is_null());
        let stop = event(
            HookSource::Claude,
            json!({"hook_event_name": "StopFailure", "error": "rate_limit"}),
        );
        assert_eq!(
            event_line(&stop, "e", 1, "{}", false)["matcher"],
            "rate_limit"
        );
        let elicit = event(
            HookSource::Claude,
            json!({"hook_event_name": "Elicitation"}),
        );
        assert!(event_line(&elicit, "e", 1, "{}", false)["matcher"].is_null());
    }

    #[test]
    fn a_large_payload_is_referenced_not_embedded() {
        let big = "x".repeat(MAX_INLINE_PAYLOAD + 1);
        let e = event(
            HookSource::Claude,
            json!({"hook_event_name": "PostToolUse", "tool_response": big}),
        );
        let raw = e.payload.to_string();
        let line = event_line(&e, "ev", 1, &raw, true);
        assert_eq!(line["payload"]["_truncated"], true);
        assert_eq!(line["raw_payload_ref"], "ev");
    }

    #[test]
    fn the_sidecar_lands_where_the_ingest_reads_it() {
        let home = tempfile::tempdir().unwrap();
        persist_sidecar(home.path(), "ev-9", r#"{"a":1}"#).unwrap();
        let path = home.path().join("hangar/provider-events/ev-9.json");
        assert_eq!(std::fs::read_to_string(path).unwrap(), r#"{"a":1}"#);
    }
}
