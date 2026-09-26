//! The records the app receives, mapped from the proto types.
//!
//! Every type here is a uniffi Record or Enum: plain data with `String`,
//! integer, `bool`, `Option` and `Vec` members only. The mapping from the
//! proto type is one function per record, next to the record, so a contract
//! change fails to compile here and nowhere in JavaScript.

use ainb_hangar_proto::agent_status::{RosterStatusResult, RosterStatusRow};
use ainb_hangar_proto::auth::HelloResult;
use ainb_hangar_proto::events::{AttentionRow, HangarEvent};
use ainb_hangar_proto::fleet::{
    FleetEvent, FleetReplayState, FleetSubscribeResult, FleetTranscriptChunk,
    FleetTranscriptListResult,
};
use ainb_hangar_proto::mutation::MutationAck;
use ainb_hangar_proto::snapshots::AnswerResult;

/// Why a call failed. Variants are what the app branches on; `message` is
/// for the connection log and never for control flow.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum WireError {
    /// The WebSocket did not open.
    #[error("connect failed: {message}")]
    Connect {
        /// The transport's words.
        message: String,
    },
    /// The host at the address is not the host whose key was pinned at
    /// pairing: the IK handshake failed or the peer closed at message 1.
    #[error("peer changed: the host static key is not the pinned one")]
    PeerChanged,
    /// The Noise handshake failed for a reason other than the pinned key.
    #[error("noise handshake failed: {message}")]
    Handshake {
        /// The transport's words.
        message: String,
    },
    /// A frame, a JSON envelope or a result did not decode.
    #[error("protocol error: {message}")]
    Protocol {
        /// What did not decode.
        message: String,
    },
    /// The daemon answered with a JSON-RPC error.
    #[error("rpc error {code}: {message}")]
    Rpc {
        /// The JSON-RPC error code.
        code: i32,
        /// The daemon's message.
        message: String,
        /// `error.data.reason`, the D18 vocabulary, when the daemon sent one.
        reason: Option<String>,
        /// The whole `error.data` as JSON text, when the daemon sent one.
        data: Option<String>,
    },
    /// The request outlived its timeout with no reply.
    #[error("{method} timed out")]
    Timeout {
        /// The method that timed out.
        method: String,
    },
    /// The session is closed, or the host refused the connection with a
    /// WebSocket close. `code` is what the host sent (4401 unauthenticated,
    /// 4403 revoked, 4409 protocol incompatible, 4429 rate limited, 1013 over
    /// capacity, 4503 draining, or another code), absent for a network loss.
    /// `retryable` and `retry_after_ms` are the crate's classification, so
    /// the app never rebuilds the close-code table: a network loss and 4429,
    /// 1013, 4503 are retryable; 4401, 4403, 4409 and a code this build does
    /// not know are not.
    #[error("session closed ({}): {reason}", code.map_or("no code".to_owned(), |c| c.to_string()))]
    Closed {
        /// The WebSocket close code, when the host sent one.
        code: Option<u16>,
        /// The host's reason, or the transport's words.
        reason: String,
        /// Whether a reconnect can succeed without a person acting.
        retryable: bool,
        /// The host's `retry-after`, in milliseconds, when it named one.
        retry_after_ms: Option<u64>,
    },
    /// A pairing offer did not parse.
    #[error("bad offer: {message}")]
    Offer {
        /// Why.
        message: String,
    },
    /// A device secret could not be read, minted or stored.
    #[error("key custody: {message}")]
    Custody {
        /// Why.
        message: String,
    },
    /// No pairing is saved for the host.
    #[error("not paired with {host_id}")]
    NotPaired {
        /// The host.
        host_id: String,
    },
}

impl WireError {
    /// Whether a reconnect can succeed without a person acting: a network
    /// loss, a timeout, a busy or draining host. An identity refusal, a
    /// revocation or an incompatible protocol is not.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::Connect { .. } | Self::Timeout { .. } => true,
            Self::Closed { retryable, .. } => *retryable,
            _ => false,
        }
    }

    pub(crate) fn protocol(e: impl std::fmt::Display) -> Self {
        Self::Protocol {
            message: e.to_string(),
        }
    }
}

/// The reason string under a JSON-RPC `error.data.reason`, when present.
pub(crate) fn error_reason(data: Option<&serde_json::Value>) -> Option<String> {
    data?.get("reason")?.as_str().map(str::to_owned)
}

/// What `auth/hello` said.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HelloSummary {
    /// The negotiated protocol version (1 from a silent daemon).
    pub selected_protocol: u32,
    /// The daemon's capability catalogue.
    pub capabilities: Vec<String>,
    /// The daemon build, for the log only.
    pub daemon_version: Option<String>,
    /// The daemon's minted host id.
    pub host_id: Option<String>,
    /// The device scope base (`mobile`, `mobile+type`, `desktop`), when the
    /// daemon advertises `hangar.scopes`.
    pub scope: Option<String>,
    /// Whether the scope carries admin.
    pub admin: bool,
    /// When the device token expires unless a hello slides it.
    pub device_expires_at_ms: Option<i64>,
}

impl From<HelloResult> for HelloSummary {
    fn from(r: HelloResult) -> Self {
        Self {
            selected_protocol: r.selected_or_legacy(),
            capabilities: r.capabilities,
            daemon_version: r.daemon_version,
            host_id: r.host_id,
            scope: r.scope.map(|s| s.base().as_str().to_owned()),
            admin: r.scope.is_some_and(ainb_hangar_proto::devices::DeviceScope::admin),
            device_expires_at_ms: r.device_expires_at_ms,
        }
    }
}

/// One session as `fleet/roster_status` describes it: the roster half and
/// the status half joined by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RosterRow {
    /// The D14 store key, and the `session_key` of every mutation.
    pub session_key: String,
    /// The provider wire token.
    pub provider: String,
    /// The display name, when the roster has one.
    pub display_name: Option<String>,
    /// The working directory.
    pub cwd: String,
    /// The agent state wire token (`working`, `waiting`, `idle`, ...).
    pub state: String,
    /// Where the state came from (`hook`, `acp_feed`, `osc_frame`, ...).
    pub provenance: String,
    /// The evidence tier wire token.
    pub tier: String,
    /// Whether the session has an open question or approval.
    pub has_open_request: bool,
    /// The lifecycle clock: the `LifecycleUpdatedAt` fence for send-prompt.
    pub lifecycle_updated_at: i64,
    /// The process incarnation: the `SessionIncarnation` fence for interrupt.
    pub process_start_fingerprint: Option<String>,
    /// The session's optimistic concurrency version: `expected_version` of
    /// `fleet/action`.
    pub version: i64,
    /// The Fleet revision this row was read at.
    pub read_revision: i64,
}

fn token<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

impl From<RosterStatusRow> for RosterRow {
    fn from(r: RosterStatusRow) -> Self {
        Self {
            session_key: r.session.session_key,
            provider: token(&r.session.provider),
            display_name: r.session.display_name,
            cwd: r.session.cwd,
            state: token(&r.status.state),
            provenance: token(&r.status.provenance),
            tier: token(&r.status.tier),
            has_open_request: r.status.has_open_request,
            lifecycle_updated_at: r.session.lifecycle_updated_at,
            process_start_fingerprint: r.session.process_start_fingerprint,
            version: r.session.version,
            read_revision: r.read_revision,
        }
    }
}

/// The `fleet/roster_status` read.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RosterSnapshot {
    /// One row per visible session, by `session_key`.
    pub rows: Vec<RosterRow>,
    /// The Fleet revision the read was taken at.
    pub read_revision: i64,
    /// The daemon's clock at the read, epoch ms; ages are computed from it.
    pub read_at_ms: i64,
}

impl From<RosterStatusResult> for RosterSnapshot {
    fn from(r: RosterStatusResult) -> Self {
        Self {
            rows: r.rows.into_iter().map(RosterRow::from).collect(),
            read_revision: r.read_revision,
            read_at_ms: r.read_at_ms,
        }
    }
}

/// One committed Fleet revision.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FleetEventRecord {
    /// The global revision.
    pub revision: i64,
    /// The session the event is about.
    pub session_key: String,
    /// The normalized event discriminator.
    pub event_type: String,
    /// Observation time, epoch ms.
    pub observed_at: i64,
    /// Whether the event changed canonical state.
    pub applied: bool,
}

impl From<FleetEvent> for FleetEventRecord {
    fn from(e: FleetEvent) -> Self {
        Self {
            revision: e.revision,
            session_key: e.session_key,
            event_type: e.event_type,
            observed_at: e.observed_at,
            applied: e.applied,
        }
    }
}

/// The `fleet/subscribe` acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FleetSubscribeSummary {
    /// The snapshot head; the cursor to resume from.
    pub head_revision: i64,
    /// `true` when `replay` covers `(after_revision, head_revision]`
    /// exactly; `false` when the snapshot replaced the interval.
    pub replay_complete: bool,
    /// Why the replay was reset, when it was.
    pub reset_reason: Option<String>,
    /// The replayed events, oldest first.
    pub replay: Vec<FleetEventRecord>,
    /// The session keys in the snapshot.
    pub session_keys: Vec<String>,
}

impl From<FleetSubscribeResult> for FleetSubscribeSummary {
    fn from(r: FleetSubscribeResult) -> Self {
        let (replay_complete, reset_reason) = match r.replay_state {
            FleetReplayState::Complete => (true, None),
            FleetReplayState::SnapshotReset { reason } => (false, Some(token(&reason))),
        };
        Self {
            head_revision: r.snapshot.head_revision,
            replay_complete,
            reset_reason,
            replay: r.replay.into_iter().map(FleetEventRecord::from).collect(),
            session_keys: r.snapshot.sessions.into_iter().map(|s| s.session_key).collect(),
        }
    }
}

/// The most characters any hook-written text keeps; the payload is whatever
/// a hook wrote, so it is bounded here, where it is parsed.
const MAX_PAYLOAD_TEXT_CHARS: usize = 512;
/// The most options a sheet offers, for the same reason.
const MAX_PAYLOAD_OPTIONS: usize = 16;

fn bounded(text: &str) -> String {
    text.chars().take(MAX_PAYLOAD_TEXT_CHARS).collect()
}

/// One option an ASK offers.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AttentionOptionRecord {
    /// The label the user picks; delivered as the answer text.
    pub label: String,
    /// The option's own explanation, or empty.
    pub description: String,
}

/// The attention payload, decoded: every producer nests it differently, so
/// this walks the shapes the TUI walks (`NeedsContext`, Claude
/// `AskUserQuestion`, the web card, approval prose, an ATC escalation) and
/// gives up rather than inventing a line. A row with no question renders as
/// its kind alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct AttentionPayload {
    /// The one-line question, when the payload says one.
    pub question: Option<String>,
    /// The structured options, empty for free text.
    pub options: Vec<AttentionOptionRecord>,
    /// Free text beside the question (`text`), when any.
    pub text: Option<String>,
    /// Approval or notification prose (`message`), when any.
    pub message: Option<String>,
}

impl AttentionPayload {
    /// Decode the stored payload JSON. A payload that is not JSON, or JSON
    /// that names none of the known fields, is the empty payload.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
            return Self::default();
        };
        let first = |paths: &[&str]| {
            paths
                .iter()
                .find_map(|p| value.pointer(p).and_then(serde_json::Value::as_str))
                .map(|s| bounded(s.trim()))
                .filter(|s| !s.is_empty())
        };
        let options = [
            "/context/options",
            "/tool_input/questions/0/options",
            "/payload/tool_input/questions/0/options",
            "/options",
        ]
        .iter()
        .find_map(|p| value.pointer(p).and_then(serde_json::Value::as_array))
        .map(|options| {
            options
                .iter()
                .take(MAX_PAYLOAD_OPTIONS)
                .filter_map(|o| {
                    if let Some(label) = o.as_str() {
                        return Some(AttentionOptionRecord {
                            label: bounded(label),
                            description: String::new(),
                        });
                    }
                    let label = o.get("label").and_then(serde_json::Value::as_str)?;
                    Some(AttentionOptionRecord {
                        label: bounded(label),
                        description: bounded(
                            o.get("description")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default(),
                        ),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
        Self {
            question: first(&[
                "/context/question",
                "/tool_input/questions/0/question",
                "/payload/tool_input/questions/0/question",
                "/question",
                "/reason",
            ]),
            options,
            text: first(&["/context/text", "/text"]),
            message: first(&["/message", "/payload/message"]),
        }
    }
}

/// One open attention row.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AttentionRecord {
    /// The row id: the answer target.
    pub id: String,
    /// The raising session.
    pub session_id: String,
    /// The owning workspace, when any.
    pub workspace_id: Option<String>,
    /// The request family (`ask_user_question`, `approval`, ...).
    pub kind: String,
    /// The `AttentionVersion` fence for the answer.
    pub version: i64,
    /// The request context, decoded.
    pub payload: AttentionPayload,
    /// Whether the row came from the degraded pane classifier.
    pub degraded: bool,
    /// Ingest time, epoch ms.
    pub created_at: i64,
}

impl From<AttentionRow> for AttentionRecord {
    fn from(r: AttentionRow) -> Self {
        Self {
            id: r.id,
            session_id: r.session_id,
            workspace_id: r.workspace_id,
            kind: r.kind,
            version: r.version,
            payload: AttentionPayload::parse(&r.payload),
            degraded: r.degraded,
            created_at: r.created_at,
        }
    }
}

/// What the daemon's ledger said about a mutation (`result.mutation`).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MutationReceipt {
    /// `accepted`, `rejected` or `unknown`.
    pub status: String,
    /// `created` or `replayed`, when the operation ran.
    pub outcome: Option<String>,
    /// The D18 reason, for a non-accepted status.
    pub reason: Option<String>,
    /// The receipt state, for a receipt-tier mutation.
    pub receipt: Option<String>,
}

impl From<MutationAck> for MutationReceipt {
    fn from(a: MutationAck) -> Self {
        Self {
            status: token(&a.status),
            outcome: a.outcome.as_ref().map(token),
            reason: a.reason,
            receipt: a.receipt.map(|r| r.token().to_owned()),
        }
    }
}

/// Where an answer ended up.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum AnswerOutcome {
    /// Delivered into the session.
    Delivered {
        /// How (`tmux (name)`).
        via: String,
    },
    /// Another surface answered first.
    AlreadyAnswered {
        /// Who.
        by: String,
    },
    /// Refused: the target was ambiguous.
    Ambiguous {
        /// Why.
        reason: String,
    },
    /// Refused: no live session matched.
    NoTarget {
        /// Why.
        reason: String,
    },
    /// The row flipped to answered but the last-mile send failed; the row
    /// stays answered and the send can be retried.
    DeliveryFailed {
        /// Why.
        reason: String,
    },
}

impl From<AnswerResult> for AnswerOutcome {
    fn from(r: AnswerResult) -> Self {
        match r {
            AnswerResult::Delivered { via } => Self::Delivered { via },
            AnswerResult::AlreadyAnswered { by } => Self::AlreadyAnswered { by },
            AnswerResult::Ambiguous { reason } => Self::Ambiguous { reason },
            AnswerResult::NoTarget { reason } => Self::NoTarget { reason },
            AnswerResult::DeliveryFailed { reason } => Self::DeliveryFailed { reason },
        }
    }
}

/// The `attention/answer` reply.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AnswerReply {
    /// The op id the crate minted; a retry sends the same one.
    pub op_id: String,
    /// Where the answer ended up.
    pub outcome: AnswerOutcome,
    /// The ledger's word, when the daemon runs the ledger.
    pub ack: Option<MutationReceipt>,
}

/// The `fleet/message_send` reply.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SendPromptReply {
    /// The op id the crate minted.
    pub op_id: String,
    /// The stored message id.
    pub message_id: String,
    /// The ledger's word, when present.
    pub ack: Option<MutationReceipt>,
}

/// The `fleet/action {interrupt}` reply.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct InterruptReply {
    /// The op id the crate minted.
    pub op_id: String,
    /// The action receipt status (`PENDING`, `DELIVERED`, `FAILED`, ...).
    pub receipt_status: String,
    /// The ledger's word, when present.
    pub ack: Option<MutationReceipt>,
}

/// One ACP transcript chunk.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TranscriptChunkRecord {
    /// The commit-ordered cursor.
    pub ingest_order: i64,
    /// The chunk id.
    pub event_id: String,
    /// The owning session.
    pub session_key: String,
    /// `acp.<kind>`.
    pub event_type: String,
    /// The chunk body as JSON text.
    pub payload: String,
    /// Observation time, epoch ms.
    pub observed_at: i64,
}

impl From<FleetTranscriptChunk> for TranscriptChunkRecord {
    fn from(c: FleetTranscriptChunk) -> Self {
        Self {
            ingest_order: c.ingest_order,
            event_id: c.event_id,
            session_key: c.session_key,
            event_type: c.event_type,
            payload: c.payload.to_string(),
            observed_at: c.observed_at,
        }
    }
}

/// One `fleet/transcript_list` page.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TranscriptPage {
    /// Chunks in ascending order.
    pub chunks: Vec<TranscriptChunkRecord>,
    /// The cursor for the next page, or absent when this page is empty.
    pub next_after_order: Option<i64>,
    /// Whether the uncursored tail read left older rows behind.
    pub truncated: bool,
}

impl From<FleetTranscriptListResult> for TranscriptPage {
    fn from(r: FleetTranscriptListResult) -> Self {
        Self {
            chunks: r.chunks.into_iter().map(TranscriptChunkRecord::from).collect(),
            next_after_order: r.next_after_order,
            truncated: r.truncated,
        }
    }
}

/// A notification the daemon pushed, or a session-level condition.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum WireEvent {
    /// One committed Fleet revision after `fleet/subscribe`.
    FleetRevision {
        /// The event.
        event: FleetEventRecord,
    },
    /// The subscriber lagged: resubscribe from a fresh snapshot.
    FleetResyncRequired,
    /// An attention row opened.
    AttentionRaised {
        /// The row id.
        attention_id: String,
        /// The raising session.
        session_id: String,
        /// The owning workspace, when any.
        workspace_id: Option<String>,
        /// The request family.
        kind: String,
        /// Whether the row came from the degraded classifier.
        degraded: bool,
        /// Ingest time, epoch ms.
        created_at: i64,
    },
    /// An attention row was answered.
    AttentionAnswered {
        /// The row id.
        attention_id: String,
        /// Who won.
        by: String,
    },
    /// One committed transcript chunk after `fleet/transcript_subscribe`.
    TranscriptChunk {
        /// The chunk.
        chunk: TranscriptChunkRecord,
    },
    /// The event queue overflowed and events were dropped: reconcile from
    /// `fleet/subscribe {after_revision}` and `attention/subscribe`.
    Lagged {
        /// How many were dropped.
        dropped: u64,
    },
    /// The session closed; no event follows. `retryable` and
    /// `retry_after_ms` are the crate's reading of `code`, the same one
    /// [`WireError::Closed`] carries, so the app redials on the crate's word.
    Closed {
        /// The WebSocket close code, when the host sent one.
        code: Option<u16>,
        /// Why.
        reason: String,
        /// Whether a reconnect can succeed without a person acting.
        retryable: bool,
        /// The host's `retry-after`, in milliseconds, when it named one.
        retry_after_ms: Option<u64>,
    },
    /// One frame of an attached terminal stream, bytes already decoded.
    TerminalFrame {
        /// The stream.
        stream_id: u64,
        /// The feed offset at emit time.
        seq: u64,
        /// The frame.
        frame: crate::terminal::TerminalFrameRecord,
    },
    /// A notification this build does not map; the app ignores it.
    Other {
        /// The method.
        method: String,
    },
}

impl WireEvent {
    /// Map a pushed notification to an event.
    #[must_use]
    pub fn from_notification(method: &str, params: serde_json::Value) -> Self {
        let other = || Self::Other {
            method: method.to_owned(),
        };
        match method {
            "fleet/event" => serde_json::from_value::<FleetEvent>(params)
                .map_or_else(|_| other(), |e| Self::FleetRevision { event: e.into() }),
            "fleet/resync_required" => Self::FleetResyncRequired,
            "fleet/transcript_event" => params
                .get("chunk")
                .cloned()
                .and_then(|c| serde_json::from_value::<FleetTranscriptChunk>(c).ok())
                .map_or_else(other, |c| Self::TranscriptChunk { chunk: c.into() }),
            ainb_hangar_proto::events::EVENT_METHOD => {
                match serde_json::from_value::<HangarEvent>(params) {
                    Ok(HangarEvent::AttentionRaised {
                        attention_id,
                        session_id,
                        workspace_id,
                        kind,
                        degraded,
                        created_at,
                        ..
                    }) => Self::AttentionRaised {
                        attention_id,
                        session_id,
                        workspace_id,
                        kind,
                        degraded,
                        created_at,
                    },
                    Ok(HangarEvent::AttentionAnswered { attention_id, by }) => {
                        Self::AttentionAnswered { attention_id, by }
                    }
                    _ => other(),
                }
            }
            _ => other(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payloads_decode_every_known_shape_and_give_up_honestly() {
        let needs = AttentionPayload::parse(
            &json!({"kind": "ASK", "context": {"question": " deploy? ", "options": [
                {"label": "yes", "description": "ship it"}, {"label": "no"}]}})
            .to_string(),
        );
        assert_eq!(needs.question.as_deref(), Some("deploy?"));
        assert_eq!(needs.options.len(), 2);
        assert_eq!(needs.options[0].description, "ship it");
        assert_eq!(needs.options[1].description, "");
        let claude = AttentionPayload::parse(
            &json!({"tool_input": {"questions": [{"question": "which?", "options": [{"label": "a"}]}]}})
                .to_string(),
        );
        assert_eq!(claude.question.as_deref(), Some("which?"));
        assert_eq!(claude.options[0].label, "a");
        let web = AttentionPayload::parse(
            &json!({"question": "pick", "options": ["one", "two"], "text": "free"}).to_string(),
        );
        assert_eq!(
            web.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(web.text.as_deref(), Some("free"));
        let approval = AttentionPayload::parse(&json!({"message": "allow rm -rf?"}).to_string());
        assert_eq!(approval.message.as_deref(), Some("allow rm -rf?"));
        assert_eq!(approval.question, None);
        assert_eq!(
            AttentionPayload::parse("not json"),
            AttentionPayload::default()
        );
        let long = "x".repeat(2_000);
        let bounded = AttentionPayload::parse(&json!({"question": long}).to_string());
        assert_eq!(bounded.question.unwrap().len(), MAX_PAYLOAD_TEXT_CHARS);
        let many: Vec<String> = (0..40).map(|i| i.to_string()).collect();
        assert_eq!(
            AttentionPayload::parse(&json!({"options": many}).to_string()).options.len(),
            MAX_PAYLOAD_OPTIONS
        );
    }

    #[test]
    fn hello_maps_scope_and_legacy_protocol() {
        let bare: HelloResult = serde_json::from_value(json!({})).unwrap();
        let s = HelloSummary::from(bare);
        assert_eq!(s.selected_protocol, 1);
        assert_eq!(s.scope, None);
        let scoped: HelloResult = serde_json::from_value(json!({
            "selected": 1,
            "capabilities": ["hangar.scopes"],
            "host_id": "01K5A0000000000000000ABCDE",
            "scope": {"base": "mobile+type", "admin": false},
            "device_expires_at_ms": 5
        }))
        .unwrap();
        let s = HelloSummary::from(scoped);
        assert_eq!(s.scope.as_deref(), Some("mobile+type"));
        assert!(!s.admin);
        assert_eq!(s.device_expires_at_ms, Some(5));
    }

    #[test]
    fn attention_events_map_and_unknown_methods_are_other() {
        let raised = WireEvent::from_notification(
            "hangar/event",
            json!({"event": "attention_raised", "attention_id": "a1", "session_id": "s1",
                    "kind": "approval", "created_at": 7}),
        );
        assert!(
            matches!(raised, WireEvent::AttentionRaised { ref attention_id, ref kind, .. }
            if attention_id == "a1" && kind == "approval")
        );
        let answered = WireEvent::from_notification(
            "hangar/event",
            json!({"event": "attention_answered", "attention_id": "a1", "by": "device:d1"}),
        );
        assert_eq!(
            answered,
            WireEvent::AttentionAnswered {
                attention_id: "a1".into(),
                by: "device:d1".into()
            }
        );
        assert_eq!(
            WireEvent::from_notification("fleet/message_event", json!({})),
            WireEvent::Other {
                method: "fleet/message_event".into()
            }
        );
        assert_eq!(
            WireEvent::from_notification("fleet/resync_required", json!(null)),
            WireEvent::FleetResyncRequired
        );
    }

    #[test]
    fn answer_outcome_reads_the_tagged_result_beside_the_ack() {
        let value = json!({"outcome": "delivered", "via": "tmux (x)",
                           "mutation": {"status": "accepted", "outcome": "created", "receipt": "delivered"}});
        let outcome: AnswerResult = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            AnswerOutcome::from(outcome),
            AnswerOutcome::Delivered {
                via: "tmux (x)".into()
            }
        );
        let ack: MutationAck = serde_json::from_value(value["mutation"].clone()).unwrap();
        let receipt = MutationReceipt::from(ack);
        assert_eq!(receipt.status, "accepted");
        assert_eq!(receipt.receipt.as_deref(), Some("delivered"));
    }
}
