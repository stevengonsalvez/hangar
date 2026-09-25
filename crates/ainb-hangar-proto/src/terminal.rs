//! Terminal streams: attach, frames, flow control, input floor (R2).
//!
//! Frozen by PR-0. Nothing in this tree dispatches these methods: a daemon
//! built from it answers every `terminal/*` request `METHOD_NOT_FOUND`, and the
//! capabilities [`crate::protocol::CAP_TERMINAL_STREAM`] and
//! [`crate::protocol::CAP_TERMINAL_INPUT`] stay dark until R2's flip.
//!
//! ```text
//! client ──terminal/attach{session}──────────▶ daemon
//!        ◀─{stream_id, epoch, snapshot_seq, floor}─
//!        ◀─terminal/frame snapshot_start … snapshot_end (seq = offset N)
//!        ◀─terminal/frame output (seq >= N) …
//!        ──terminal/ack{consumed}────────────▶ (every 256 KiB, window 2 MiB)
//!        ──terminal/input{floor_gen?, data, op_id?}──▶ receipt tier
//! ```
//!
//! ## Wire rules
//!
//! * Frames travel as base64 JSON inside the ordinary Rpc opcode on every leg.
//!   The binary opcodes reserved in `ainb-hangar-noise` bind only if a binary
//!   lane is ever added.
//! * `seq` is the pane-feed byte offset within one `epoch`, at emit time. A
//!   snapshot is taken at offset N and every snapshot frame carries N; a client
//!   applies `output` with `seq >= N`. A re-seed bumps `epoch`. After a
//!   [`TerminalFrame::DataGap`] the client waits for the next snapshot.
//! * `consumed` in [`TerminalAckParams`] is the CUMULATIVE decoded payload
//!   bytes of every frame on the stream since attach, snapshot frames
//!   included. A value lower than one already seen is ignored, so a retried ack
//!   is idempotent.
//! * Every method taking a `stream_id` checks that the stream belongs to the
//!   calling connection; a foreign stream is `UNAUTHORIZED`.
//! * The input and floor mutations carry the D18 envelope FLATTENED onto their
//!   params, like every other mutation: `{ .., op_id?, fence? }`.
//! * The floor is keyed per STREAM, not per principal: two browser tabs share
//!   one operator principal and still arbitrate.
//! * A refused floor is [`crate::mutation::MUTATION_REJECTED`] (`-32008`) with
//!   `data.reason =` [`REASON_FLOOR_DENIED`], never a new error code.

use serde::{Deserialize, Serialize};

use crate::mutation::MutationEnvelope;
use crate::session_ref::SessionRef;

pub use crate::mutation::REASON_FLOOR_DENIED;

/// The flow-control window: bytes a daemon sends ahead of the last ack.
pub const WINDOW_BYTES: u64 = 2 * 1024 * 1024;
/// The largest decoded payload one `output` or `snapshot_chunk` frame carries.
pub const CHUNK_BYTES: u64 = 48 * 1024;
/// A client acks at least this often, in decoded payload bytes.
pub const ACK_EVERY_BYTES: u64 = 256 * 1024;

/// A daemon-assigned stream id, unique per connection while attached.
///
/// Below 2^53 so a JavaScript client reads it exactly.
pub type StreamId = u64;

/// Who holds a stream's input floor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloorHolder {
    /// The ledger principal (`local` or `device:<id>`).
    pub principal: String,
    /// A label for the holder: the device registry row's `display_name` for a
    /// device, the surface kind for a local connection. Never the name a
    /// client declared in its hello.
    pub label: String,
    /// The stream holding the floor.
    pub stream_id: StreamId,
}

/// The input floor of one pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloorState {
    /// The holder, or absent when the floor is free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<FloorHolder>,
    /// Bumped on every change of holder.
    pub floor_gen: u64,
}

/// `terminal/attach` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAttachParams {
    /// The session whose pane to stream. A `host_id` other than this daemon's
    /// is `INVALID_PARAMS`.
    pub session: SessionRef,
    /// Requested columns: applied only when this attach takes the floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    /// Requested rows: applied only when this attach takes the floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    /// Acquire the floor if free and become the resize owner. Needs scope
    /// `mobile+type` or above; a `mobile` device may attach only without it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub want_input: bool,
    /// Scrollback rows to include in the snapshot (default 0, at most the live
    /// window).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback_rows: Option<u32>,
}

/// `terminal/attach` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAttachResult {
    /// The new stream.
    pub stream_id: StreamId,
    /// The feed epoch the snapshot belongs to.
    pub epoch: u64,
    /// The feed offset N the snapshot was taken at.
    pub snapshot_seq: u64,
    /// Pane columns.
    pub cols: u16,
    /// Pane rows.
    pub rows: u16,
    /// The floor at attach.
    pub floor: FloorState,
    /// Native tmux clients attached, whose input is not arbitrated.
    pub native_clients: u32,
}

/// `terminal/detach` params.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalDetachParams {
    /// The stream to close. Releases the floor at once, no grace.
    pub stream_id: StreamId,
}

/// `terminal/ack` params.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAckParams {
    /// The stream.
    pub stream_id: StreamId,
    /// Cumulative decoded payload bytes consumed since attach.
    pub consumed: u64,
}

/// `terminal/ack` result: empty.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAckResult {}

/// `terminal/scrollback` params.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalScrollbackParams {
    /// The stream.
    pub stream_id: StreamId,
    /// Read rows above this row of the pane history.
    pub before_row: u64,
    /// How many rows.
    pub rows: u32,
}

/// `terminal/scrollback` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalScrollbackResult {
    /// ANSI bytes of the rows, base64.
    pub data: String,
}

/// `terminal/input` params (receipt tier).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInputParams {
    /// The stream typing.
    pub stream_id: StreamId,
    /// The floor generation the client holds. Absent means "acquire if free".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor_gen: Option<u64>,
    /// The bytes to type, base64.
    pub data: String,
    /// The D18 envelope, flattened: `op_id?`, `fence?` at top level.
    #[serde(flatten)]
    pub mutation: MutationEnvelope,
}

/// `terminal/input` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInputResult {
    /// The floor generation the input was typed under.
    pub floor_gen: u64,
}

/// A `terminal/floor` action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FloorAction {
    /// Take the floor if free.
    Acquire,
    /// Give the floor up.
    Release,
    /// Take the floor from its holder.
    Take,
}

/// `terminal/floor` params (dedupe tier). Result: [`FloorState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalFloorParams {
    /// The stream acting.
    pub stream_id: StreamId,
    /// What to do.
    pub action: FloorAction,
    /// The D18 envelope, flattened: `op_id?`, `fence?` at top level.
    #[serde(flatten)]
    pub mutation: MutationEnvelope,
}

/// `terminal/resize` params. Floor holder only; debounced 100 ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalResizeParams {
    /// The stream resizing.
    pub stream_id: StreamId,
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

/// `terminal/resize` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TerminalResizeResult {
    /// The pane now has this size.
    Applied {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// The session's `window-size` is not `latest`, so the daemon does not
    /// size it.
    NotApplicable {
        /// The session's `window-size` option.
        window_size: String,
    },
}

/// Why a stream skipped bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataGapReason {
    /// The client stopped acking and the window closed.
    Paused,
    /// The daemon lost its pane feed and re-seeded.
    FeedLost,
    /// The viewer fell behind and bytes were dropped (the spec's word).
    Dropped,
    /// The session ended.
    SessionGone,
}

/// One frame of an attached stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalFrame {
    /// A snapshot begins: `chunks` chunks follow, then `snapshot_end`.
    SnapshotStart {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
        /// The feed epoch.
        epoch: u64,
        /// Chunks that follow.
        chunks: u32,
    },
    /// One chunk of ANSI repaint bytes, base64.
    SnapshotChunk {
        /// Base64 bytes.
        data: String,
    },
    /// The snapshot is complete.
    SnapshotEnd,
    /// Live pane output, base64.
    Output {
        /// Base64 bytes.
        data: String,
    },
    /// The pane changed size.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// Bytes were skipped; wait for the next snapshot.
    DataGap {
        /// Why.
        reason: DataGapReason,
        /// How many bytes, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dropped_bytes: Option<u64>,
    },
    /// The floor changed.
    Floor {
        /// The new holder, or absent when free.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        holder: Option<FloorHolder>,
        /// The new generation.
        floor_gen: u64,
    },
    /// The count of native tmux clients changed.
    Presence {
        /// Native clients attached.
        native_clients: u32,
    },
    /// The stream is closed; no frame follows.
    Closed {
        /// A machine-readable reason.
        reason: String,
    },
}

/// Params of the [`crate::methods::TERMINAL_FRAME`] notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalFrameParams {
    /// The stream.
    pub stream_id: StreamId,
    /// The feed offset at emit time.
    pub seq: u64,
    /// The frame.
    pub frame: TerminalFrame,
}

/// `error.data` of a floor refusal, beside the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloorDeniedData {
    /// Always [`REASON_FLOOR_DENIED`].
    pub reason: String,
    /// The current holder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<FloorHolder>,
    /// The current generation.
    pub floor_gen: u64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::hosts::HostId;
    use crate::mutation::{Fence, OpId};

    fn holder() -> FloorHolder {
        FloorHolder {
            principal: "device:01K5A0000000000000000ABCDE".to_string(),
            label: "phone".to_string(),
            stream_id: 3,
        }
    }

    #[test]
    fn attach_params_are_frozen_and_want_input_is_optional() {
        let wire = json!({
            "session": {"host_id": "local", "session_key": "claude:a"},
        });
        let params: TerminalAttachParams = serde_json::from_value(wire.clone()).unwrap();
        assert!(!params.want_input);
        assert_eq!(params.session.host_id, HostId::local());
        assert_eq!(serde_json::to_value(&params).unwrap(), wire);
        let full = json!({
            "session": {"host_id": "local", "session_key": "claude:a"},
            "cols": 40, "rows": 20, "want_input": true, "scrollback_rows": 100,
        });
        let params: TerminalAttachParams = serde_json::from_value(full.clone()).unwrap();
        assert!(params.want_input);
        assert_eq!(serde_json::to_value(&params).unwrap(), full);
    }

    #[test]
    fn attach_result_is_frozen() {
        let result = TerminalAttachResult {
            stream_id: 1,
            epoch: 2,
            snapshot_seq: 300,
            cols: 80,
            rows: 24,
            floor: FloorState {
                holder: None,
                floor_gen: 0,
            },
            native_clients: 1,
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({"stream_id":1,"epoch":2,"snapshot_seq":300,"cols":80,"rows":24,
                   "floor":{"floor_gen":0},"native_clients":1})
        );
    }

    /// The W0 rule: a mutation's envelope is flattened onto its params.
    #[test]
    fn input_and_floor_carry_the_envelope_at_top_level() {
        let op = OpId::from_bytes([0x11; 16]);
        let input = TerminalInputParams {
            stream_id: 1,
            floor_gen: Some(4),
            data: "eWVz".to_string(),
            mutation: MutationEnvelope::with_op_id(op.clone()),
        };
        let wire = serde_json::to_value(&input).unwrap();
        assert_eq!(wire["op_id"], op.as_str());
        assert!(wire.get("envelope").is_none());
        assert!(wire.get("mutation").is_none());
        let floor = TerminalFloorParams {
            stream_id: 1,
            action: FloorAction::Take,
            mutation: MutationEnvelope::fenced(op, Fence::RegistryVersion { version: 2 }),
        };
        let wire = serde_json::to_value(&floor).unwrap();
        assert_eq!(wire["action"], "take");
        assert_eq!(wire["fence"]["kind"], "registry_version");
    }

    #[test]
    fn frames_are_frozen() {
        let cases = [
            (
                TerminalFrame::SnapshotStart {
                    cols: 80,
                    rows: 24,
                    epoch: 1,
                    chunks: 2,
                },
                json!({"kind":"snapshot_start","cols":80,"rows":24,"epoch":1,"chunks":2}),
            ),
            (
                TerminalFrame::SnapshotChunk {
                    data: "AA==".to_string(),
                },
                json!({"kind":"snapshot_chunk","data":"AA=="}),
            ),
            (TerminalFrame::SnapshotEnd, json!({"kind":"snapshot_end"})),
            (
                TerminalFrame::Output {
                    data: "AA==".to_string(),
                },
                json!({"kind":"output","data":"AA=="}),
            ),
            (
                TerminalFrame::Resize { cols: 40, rows: 20 },
                json!({"kind":"resize","cols":40,"rows":20}),
            ),
            (
                TerminalFrame::DataGap {
                    reason: DataGapReason::Dropped,
                    dropped_bytes: Some(9),
                },
                json!({"kind":"data_gap","reason":"dropped","dropped_bytes":9}),
            ),
            (
                TerminalFrame::Floor {
                    holder: Some(holder()),
                    floor_gen: 5,
                },
                json!({"kind":"floor","holder":{"principal":"device:01K5A0000000000000000ABCDE",
                       "label":"phone","stream_id":3},"floor_gen":5}),
            ),
            (
                TerminalFrame::Presence { native_clients: 2 },
                json!({"kind":"presence","native_clients":2}),
            ),
            (
                TerminalFrame::Closed {
                    reason: "session_gone".to_string(),
                },
                json!({"kind":"closed","reason":"session_gone"}),
            ),
        ];
        for (frame, wire) in cases {
            assert_eq!(serde_json::to_value(&frame).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<TerminalFrame>(wire).unwrap(),
                frame
            );
        }
        let notification = TerminalFrameParams {
            stream_id: 1,
            seq: 42,
            frame: TerminalFrame::SnapshotEnd,
        };
        assert_eq!(
            serde_json::to_value(&notification).unwrap(),
            json!({"stream_id":1,"seq":42,"frame":{"kind":"snapshot_end"}})
        );
    }

    /// Only the four reasons; R2's early `lagged` is not one of them.
    #[test]
    fn data_gap_reasons_are_the_four() {
        for (reason, token) in [
            (DataGapReason::Paused, "paused"),
            (DataGapReason::FeedLost, "feed_lost"),
            (DataGapReason::Dropped, "dropped"),
            (DataGapReason::SessionGone, "session_gone"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), token);
        }
        assert!(serde_json::from_value::<DataGapReason>(json!("lagged")).is_err());
    }

    #[test]
    fn resize_results_are_frozen() {
        assert_eq!(
            serde_json::to_value(TerminalResizeResult::Applied { cols: 40, rows: 20 }).unwrap(),
            json!({"outcome":"applied","cols":40,"rows":20})
        );
        assert_eq!(
            serde_json::to_value(TerminalResizeResult::NotApplicable {
                window_size: "manual".to_string()
            })
            .unwrap(),
            json!({"outcome":"not_applicable","window_size":"manual"})
        );
    }

    #[test]
    fn flow_control_numbers_are_frozen() {
        assert_eq!(WINDOW_BYTES, 2_097_152);
        assert_eq!(CHUNK_BYTES, 49_152);
        assert_eq!(ACK_EVERY_BYTES, 262_144);
        assert_eq!(
            serde_json::to_value(TerminalAckParams {
                stream_id: 1,
                consumed: 10
            })
            .unwrap(),
            json!({"stream_id":1,"consumed":10})
        );
        assert_eq!(
            serde_json::to_value(TerminalAckResult {}).unwrap(),
            json!({})
        );
    }

    /// A floor refusal reuses `MUTATION_REJECTED` with a reason, not a code.
    #[test]
    fn a_floor_refusal_is_a_rejection_reason() {
        assert_eq!(REASON_FLOOR_DENIED, "floor_denied");
        let data = FloorDeniedData {
            reason: REASON_FLOOR_DENIED.to_string(),
            holder: Some(holder()),
            floor_gen: 7,
        };
        let wire = serde_json::to_value(&data).unwrap();
        assert_eq!(wire["reason"], "floor_denied");
        assert_eq!(wire["floor_gen"], 7);
        assert_eq!(crate::mutation::MUTATION_REJECTED, -32008);
    }
}
