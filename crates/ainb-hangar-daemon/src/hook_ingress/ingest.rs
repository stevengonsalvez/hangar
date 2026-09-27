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

use ainb_hangar_core::channel::ChannelSet;
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_proto::hooks::HookSource;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::hold::{HOLD_DEADLINE, HeldRequest, HoldEnd, HoldRegistry};
use super::{HookEvent, HookReply, HookSink};
use crate::attention_ingest::AttentionIngest;
use crate::events::EventSink;

/// `answered_by` on an approval row the agent moved past without an answer
/// through the daemon (it was answered at the terminal, or the tool ran).
pub const RESOLVED_BY_AGENT: &str = "resolved:agent";
/// `answered_by` on an approval row whose hold ended with no decision (the
/// deadline passed, the hook went away): the agent's own prompt took over.
pub const RESOLVED_NATIVE: &str = "resolved:native";

/// Largest payload embedded inline in a line, as `ainb fleet atc hook` caps it.
pub const MAX_INLINE_PAYLOAD: usize = 3 * 1024;

/// Feeds admitted hook calls to the attention ingest, and holds the blocking
/// ones for a human.
pub struct IngestSink {
    ingest: AttentionIngest,
    hangar_home: PathBuf,
    pool: SqlitePool,
    events: EventSink,
    holds: &'static HoldRegistry,
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
    /// paths as the tail's), the hangar home the sidecars live under, and the
    /// store and event sink approval rows are raised on. Holds go to the
    /// process's [`super::hold::registry`].
    #[must_use]
    pub fn new(
        ingest: AttentionIngest,
        hangar_home: PathBuf,
        pool: SqlitePool,
        events: EventSink,
    ) -> Self {
        Self::with_registry(ingest, hangar_home, pool, events, super::hold::registry())
    }

    /// [`Self::new`] over an explicit registry (tests).
    #[must_use]
    pub const fn with_registry(
        ingest: AttentionIngest,
        hangar_home: PathBuf,
        pool: SqlitePool,
        events: EventSink,
        holds: &'static HoldRegistry,
    ) -> Self {
        Self {
            ingest,
            hangar_home,
            pool,
            events,
            holds,
        }
    }

    /// Hold a blocking request for a human. `NoContent` for anything that
    /// does not hold, cannot be registered, or ends without a decision.
    async fn hold(&self, event: &HookEvent, event_id: &str, now_ms: i64) -> HookReply {
        let Some(request) = HeldRequest::from_payload(&event.payload) else {
            return HookReply::NoContent;
        };
        let text = |k: &str| event.payload.get(k).and_then(Value::as_str).unwrap_or_default();
        let session = text("session_id");
        if session.is_empty() {
            return HookReply::NoContent;
        }
        let key = hold_key(session, &event.payload);
        // Bind to exactly the row THIS request raised, never "the latest
        // open row": that could be another tool call's, and its answer would
        // then reach this hook.
        let attention_id = match &request {
            HeldRequest::Permission => {
                self.raise_approval(event, session, event_id, &key, now_ms).await
            }
            HeldRequest::Ask { tool_input } => self.ask_row(session, event_id, tool_input).await,
        };
        let Some(attention_id) = attention_id else {
            return HookReply::NoContent;
        };
        // Declared before the waiter so the waiter drops first: the guard
        // then sees whether this was the last hook on the request.
        let mut guard = RetireOnDrop {
            armed: true,
            attention_id: attention_id.clone(),
            pool: self.pool.clone(),
            events: self.events.clone(),
            holds: self.holds,
        };
        let Some(waiter) = self.holds.register(&key, &attention_id, request) else {
            guard.armed = false;
            tracing::warn!("hook ingress: hold not registered; the agent prompts itself");
            return HookReply::NoContent;
        };
        match waiter.wait(HOLD_DEADLINE).await {
            HoldEnd::Decided(request, decision) => {
                guard.armed = false;
                request.render(&decision).map_or(HookReply::NoContent, HookReply::Json)
            }
            // The agent moved on: the status event that released the hold
            // has already retired the row as resolved:agent.
            HoldEnd::Released => {
                guard.armed = false;
                HookReply::NoContent
            }
            // No decision in time: the guard retires the row as
            // resolved:native, and the agent's own prompt takes over.
            HoldEnd::TimedOut => HookReply::NoContent,
        }
    }

    /// Raise (or find) the `approval` row a Claude permission request holds on.
    async fn raise_approval(
        &self,
        event: &HookEvent,
        session: &str,
        event_id: &str,
        key: &str,
        now_ms: i64,
    ) -> Option<String> {
        let p = &event.payload;
        let context = json!({
            "source": "hook_hold",
            "tool_name": p.get("tool_name"),
            "tool_input": p.get("tool_input"),
            "tool_use_id": p.get("tool_use_id"),
        });
        let channels: ChannelSet =
            crate::notify::resolve_channels(&self.pool, AttentionKind::Approval, None).await;
        let row = NewAttention {
            id: format!("att:{session}:{event_id}"),
            session_id: session.to_string(),
            cwd: p.get("cwd").and_then(Value::as_str).unwrap_or_default().to_string(),
            workspace_id: None,
            kind: AttentionKind::Approval,
            payload: context.to_string(),
            degraded: false,
            created_at: now_ms,
            raise_transcript: p
                .get("transcript_path")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .map(str::to_string),
            channels,
        };
        let request_key = format!("hold:{key}");
        match AttentionRepo::insert_if_absent(&self.pool, &row, Some(&request_key)).await {
            Ok(true) => {
                self.events.emit_attention(HangarEvent::AttentionRaised {
                    attention_id: row.id.clone(),
                    session_id: row.session_id.clone(),
                    workspace_id: None,
                    kind: AttentionKind::Approval.as_str().to_string(),
                    degraded: false,
                    created_at: now_ms,
                    channels,
                });
                Some(row.id)
            }
            // A duplicate of a request already open: join ITS row, found by
            // this request's own key.
            Ok(false) => AttentionRepo::open_id_for_request_key(&self.pool, session, &request_key)
                .await
                .ok()
                .flatten(),
            Err(e) => {
                tracing::warn!(error = %e, "hook ingress: approval row insert failed");
                None
            }
        }
    }

    /// The open ask row this question raised: the row the ingest just wrote
    /// for this very line, else (a re-announced question) the open row with
    /// this question's own request key.
    async fn ask_row(&self, session: &str, event_id: &str, tool_input: &Value) -> Option<String> {
        let own = format!("att:{session}:{event_id}");
        if let Ok(Some(row)) = AttentionRepo::get(&self.pool, &own).await {
            if row.state == "open" {
                return Some(own);
            }
        }
        let key = crate::attention_ingest::ask_request_key(tool_input)?;
        AttentionRepo::open_id_for_request_key(&self.pool, session, &key)
            .await
            .ok()
            .flatten()
    }

    /// Close an approval row that ended without an answer through the daemon.
    async fn retire_approval(&self, attention_id: &str, by: &str) {
        retire_approval_row(&self.pool, &self.events, attention_id, by).await;
    }

    /// A status event that shows the agent moved past a held request ends it:
    /// the same tool's `PostToolUse`/`PostToolUseFailure`, or a new prompt,
    /// a stop, or the session's end. The hold (if still live) answers `{}`,
    /// and the approval row it raised is retired, live hold or not.
    async fn release_passed(&self, event: &HookEvent) {
        let p = &event.payload;
        let Some(session) = p.get("session_id").and_then(Value::as_str) else {
            return;
        };
        let name = p.get("hook_event_name").and_then(Value::as_str).unwrap_or_default();
        let this_call = match name {
            "PostToolUse" | "PostToolUseFailure" => {
                Some(p.get("tool_use_id").and_then(Value::as_str))
            }
            "UserPromptSubmit" | "Stop" | "SessionEnd" => None,
            _ => return,
        };
        let keys = match this_call {
            Some(_) => vec![hold_key(session, p)],
            None => self.holds.keys_for_session(session),
        };
        // Retire first, release second: by the time a released hook answers
        // `{}`, its row already reads resolved:agent.
        if let Ok(open) = AttentionRepo::open_approval_ids_for_session(&self.pool, session).await {
            self.retire_passed(this_call, open).await;
        }
        for key in keys {
            self.holds.cancel(&key);
        }
    }

    /// Retire the hook-hold approval rows among `open` that the moved-on
    /// event covers: the one call, or every hold of the session.
    async fn retire_passed(&self, this_call: Option<Option<&str>>, open: Vec<String>) {
        for id in open {
            let Ok(Some(row)) = AttentionRepo::get(&self.pool, &id).await else {
                continue;
            };
            let Ok(payload) = serde_json::from_str::<Value>(&row.payload) else {
                continue;
            };
            if payload.get("source").and_then(Value::as_str) != Some("hook_hold") {
                continue;
            }
            let same_call = match this_call {
                None => true,
                Some(call) => payload.get("tool_use_id").and_then(Value::as_str) == call,
            };
            if same_call {
                self.retire_approval(&id, RESOLVED_BY_AGENT).await;
            }
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
            // The script-minted id makes a spooled copy of an event the daemon
            // already recorded a replay of the same event: the ingest is
            // idempotent by event id.
            let event_id = event.event_id.as_deref().map_or_else(
                || uuid::Uuid::new_v4().to_string(),
                |id| format!("hook-{id}"),
            );
            let raw = event.payload.to_string();
            let stored = persist_sidecar(&self.hangar_home, &event_id, &raw).is_ok();
            let line = event_line(
                &event,
                &event_id,
                event.received_at_ms.unwrap_or(now_ms),
                &raw,
                stored,
            );
            if !self.ingest.ingest_line(&line.to_string(), now_ms).await {
                // A store fault: 503 makes a status hook spool the event, and
                // the next daemon start replays it. A hold never spools.
                return HookReply::Unavailable;
            }
            if event.hold {
                self.hold(&event, &event_id, now_ms).await
            } else {
                // A replayed event is history: a spooled Stop from before
                // this daemon started must never end a hold that is live now.
                if event.received_at_ms.is_none() {
                    self.release_passed(&event).await;
                }
                HookReply::NoContent
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

/// Close a hook-hold approval row that ended without an answer through the
/// daemon. Only an open `approval` row changes; anything else is untouched.
async fn retire_approval_row(pool: &SqlitePool, events: &EventSink, attention_id: &str, by: &str) {
    let now = ainb_hangar_core::clock::HangarClock::now_ms(&ainb_hangar_core::clock::SystemClock);
    if let Ok(Some(row)) = AttentionRepo::get(pool, attention_id).await {
        if row.kind == AttentionKind::Approval
            && AttentionRepo::mark_answered_if_open(pool, attention_id, by, "", now)
                .await
                .unwrap_or(0)
                > 0
        {
            events.emit_attention(HangarEvent::AttentionAnswered {
                attention_id: attention_id.to_string(),
                by: by.to_string(),
            });
        }
    }
}

/// Retires a hold's approval row as `resolved:native` when the hold's future
/// ends without a decision by any route: the deadline, or the listener
/// dropping it because the hook's connection closed (a `Drop`, which no
/// `match` arm sees). Disarmed on a decision and on a release, whose owner
/// retires the row itself. Retires only once no other hook still holds the
/// same request.
struct RetireOnDrop {
    armed: bool,
    attention_id: String,
    pool: SqlitePool,
    events: EventSink,
    holds: &'static HoldRegistry,
}

impl Drop for RetireOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (id, pool, events, holds) = (
            std::mem::take(&mut self.attention_id),
            self.pool.clone(),
            self.events.clone(),
            self.holds,
        );
        runtime.spawn(async move {
            if holds.request_for(&id).is_none() {
                retire_approval_row(&pool, &events, &id, RESOLVED_NATIVE).await;
            }
        });
    }
}

/// The stable key of one held request: the session and the tool call it is
/// about (`tool_use_id`), so a duplicate hook for the same call joins the
/// live hold instead of opening a second one.
fn hold_key(session: &str, payload: &Value) -> String {
    let call = payload.get("tool_use_id").and_then(Value::as_str).map_or_else(
        || {
            // No id: the tool and its input name the call.
            let tool = payload.get("tool_name").map(Value::to_string).unwrap_or_default();
            let input = payload.get("tool_input").map(Value::to_string).unwrap_or_default();
            format!("{tool}:{input}")
        },
        str::to_string,
    );
    format!("{session}:{call}")
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
            event_id: None,
            received_at_ms: None,
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
