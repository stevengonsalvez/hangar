//! The terminal on the wire (M1-13): `terminal/attach`, `input`, `resize`,
//! `floor`, `scrollback`, `detach`, and the `terminal/frame` notifications
//! decoded from base64 here so the webview gets raw bytes and JavaScript
//! parses no wire JSON.
//!
//! ```text
//! attach ──▶ {stream_id, epoch, snapshot_seq, cols, rows, floor, native_clients}
//!   frames: snapshot_start ▶ snapshot_chunk* ▶ snapshot_end ▶ output* ▶ ...
//!   data_gap ▶ (a fresh snapshot follows)      floor / presence / closed
//!   ack: every ACK_EVERY_BYTES of decoded payload the crate sends
//!        terminal/ack {stream_id, consumed}, consumed cumulative since attach
//! ```
//!
//! Input is receipt-tier: every `terminal/input` carries a crate-minted op id
//! and a retry passes it back. A refusal by the floor arrives as
//! [`WireError::FloorDenied`] with the holder, mapped by the session.

use std::collections::HashMap;
use std::sync::Mutex;

use ainb_hangar_proto::hosts::HostId;
use ainb_hangar_proto::methods;
use ainb_hangar_proto::mutation::MutationEnvelope;
use ainb_hangar_proto::mutation::REASON_FLOOR_DENIED;
use ainb_hangar_proto::session_ref::SessionRef;
use ainb_hangar_proto::terminal::{
    ACK_EVERY_BYTES, DataGapReason, FloorAction, FloorDeniedData, FloorHolder, FloorState,
    StreamId, TerminalAckParams, TerminalAckResult, TerminalAttachParams, TerminalAttachResult,
    TerminalDetachParams, TerminalFloorParams, TerminalFrame, TerminalFrameParams,
    TerminalInputParams, TerminalInputResult, TerminalResizeParams, TerminalResizeResult,
    TerminalScrollbackParams, TerminalScrollbackResult,
};
use base64::Engine as _;

use crate::records::{MutationReceipt, WireError};
use crate::session::Session;

/// Who holds a stream's floor.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FloorHolderRecord {
    /// The ledger principal (`local` or `device:<id>`).
    pub principal: String,
    /// The registry display name for a device, the surface kind for local.
    pub label: String,
    /// The stream holding the floor.
    pub stream_id: u64,
}

impl From<FloorHolder> for FloorHolderRecord {
    fn from(h: FloorHolder) -> Self {
        Self {
            principal: h.principal,
            label: h.label,
            stream_id: h.stream_id,
        }
    }
}

/// The input floor of one pane.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FloorRecord {
    /// The holder, or absent when free.
    pub holder: Option<FloorHolderRecord>,
    /// Bumped on every change of holder.
    pub floor_gen: u64,
}

impl From<FloorState> for FloorRecord {
    fn from(f: FloorState) -> Self {
        Self {
            holder: f.holder.map(Into::into),
            floor_gen: f.floor_gen,
        }
    }
}

/// What `terminal/attach` answered.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TerminalAttachRecord {
    /// The new stream.
    pub stream_id: u64,
    /// The feed epoch the snapshot belongs to.
    pub epoch: u64,
    /// The feed offset the snapshot was taken at.
    pub snapshot_seq: u64,
    /// Pane columns.
    pub cols: u16,
    /// Pane rows.
    pub rows: u16,
    /// The floor at attach.
    pub floor: FloorRecord,
    /// Native tmux clients attached, whose input is not arbitrated.
    pub native_clients: u32,
}

impl From<TerminalAttachResult> for TerminalAttachRecord {
    fn from(r: TerminalAttachResult) -> Self {
        Self {
            stream_id: r.stream_id,
            epoch: r.epoch,
            snapshot_seq: r.snapshot_seq,
            cols: r.cols,
            rows: r.rows,
            floor: r.floor.into(),
            native_clients: r.native_clients,
        }
    }
}

/// What `terminal/input` answered. A floor refusal is a value, not an
/// error: the app renders the holder and offers "take".
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum TerminalInputOutcome {
    /// The bytes were typed.
    Typed {
        /// The op id the crate minted; a retry sends the same one.
        op_id: String,
        /// The floor generation the input was typed under.
        floor_gen: u64,
        /// The ledger's word, when present.
        ack: Option<MutationReceipt>,
    },
    /// `MUTATION_REJECTED` with `reason: floor_denied` (M11).
    FloorDenied {
        /// The holder.
        holder: Option<FloorHolderRecord>,
        /// The current generation.
        floor_gen: u64,
    },
}

/// What `terminal/floor` answered.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum TerminalFloorOutcome {
    /// The floor after the action.
    Floor {
        /// The op id the crate minted.
        op_id: String,
        /// The floor.
        floor: FloorRecord,
        /// The ledger's word, when present.
        ack: Option<MutationReceipt>,
    },
    /// `MUTATION_REJECTED` with `reason: floor_denied` (M11).
    FloorDenied {
        /// The holder.
        holder: Option<FloorHolderRecord>,
        /// The current generation.
        floor_gen: u64,
    },
}

/// The floor refusal carried by an RPC error, when that is what it is.
fn floor_denied(err: &WireError) -> Option<(Option<FloorHolderRecord>, u64)> {
    let WireError::Rpc { reason, data, .. } = err else {
        return None;
    };
    if reason.as_deref() != Some(REASON_FLOOR_DENIED) {
        return None;
    }
    let data: FloorDeniedData = serde_json::from_str(data.as_deref()?).ok()?;
    Some((data.holder.map(Into::into), data.floor_gen))
}

/// What `terminal/resize` answered.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ResizeOutcome {
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
    /// An outcome a newer daemon added.
    Unknown,
}

impl From<TerminalResizeResult> for ResizeOutcome {
    fn from(r: TerminalResizeResult) -> Self {
        match r {
            TerminalResizeResult::Applied { cols, rows } => Self::Applied { cols, rows },
            TerminalResizeResult::NotApplicable { window_size } => {
                Self::NotApplicable { window_size }
            }
            TerminalResizeResult::Unknown => Self::Unknown,
        }
    }
}

/// One frame of an attached stream, with its bytes decoded.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum TerminalFrameRecord {
    /// A snapshot begins: `chunks` chunks follow, then `SnapshotEnd`.
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
    /// One chunk of ANSI repaint bytes.
    SnapshotChunk {
        /// The bytes.
        data: Vec<u8>,
    },
    /// The snapshot is complete.
    SnapshotEnd,
    /// Live pane output.
    Output {
        /// The bytes.
        data: Vec<u8>,
    },
    /// The pane changed size.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// Bytes were skipped: clear and wait for the next snapshot.
    DataGap {
        /// `paused`, `feed_lost`, `dropped`, `session_gone` or `unknown`.
        reason: String,
        /// How many bytes, when known.
        dropped_bytes: Option<u64>,
    },
    /// The floor changed.
    Floor {
        /// The new holder, or absent when free.
        holder: Option<FloorHolderRecord>,
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
    /// A frame kind a newer daemon added; the app skips it.
    Unknown,
}

fn gap_reason(reason: DataGapReason) -> String {
    match serde_json::to_value(reason) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "unknown".to_owned(),
    }
}

fn decode(data: &str) -> Result<Vec<u8>, WireError> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| WireError::Protocol {
            message: format!("terminal frame data is not base64: {e}"),
        })
}

impl TerminalFrameRecord {
    /// Decode one wire frame; the returned count is its decoded payload
    /// bytes, what `terminal/ack` counts.
    pub fn from_wire(frame: TerminalFrame) -> Result<(Self, u64), WireError> {
        Ok(match frame {
            TerminalFrame::SnapshotStart {
                cols,
                rows,
                epoch,
                chunks,
            } => (
                Self::SnapshotStart {
                    cols,
                    rows,
                    epoch,
                    chunks,
                },
                0,
            ),
            TerminalFrame::SnapshotChunk { data } => {
                let data = decode(&data)?;
                let n = data.len() as u64;
                (Self::SnapshotChunk { data }, n)
            }
            TerminalFrame::SnapshotEnd => (Self::SnapshotEnd, 0),
            TerminalFrame::Output { data } => {
                let data = decode(&data)?;
                let n = data.len() as u64;
                (Self::Output { data }, n)
            }
            TerminalFrame::Resize { cols, rows } => (Self::Resize { cols, rows }, 0),
            TerminalFrame::DataGap {
                reason,
                dropped_bytes,
            } => (
                Self::DataGap {
                    reason: gap_reason(reason),
                    dropped_bytes,
                },
                0,
            ),
            TerminalFrame::Floor { holder, floor_gen } => (
                Self::Floor {
                    holder: holder.map(Into::into),
                    floor_gen,
                },
                0,
            ),
            TerminalFrame::Presence { native_clients } => (Self::Presence { native_clients }, 0),
            TerminalFrame::Closed { reason } => (Self::Closed { reason }, 0),
            TerminalFrame::Unknown => (Self::Unknown, 0),
        })
    }
}

/// Per-stream flow-control accounting: decoded bytes handed to the app,
/// and the last value acked.
#[derive(Debug, Default)]
struct StreamAcct {
    consumed: u64,
    acked: u64,
}

/// The streams of one host, keyed by stream id.
#[derive(Debug, Default)]
pub struct Streams {
    accts: Mutex<HashMap<StreamId, StreamAcct>>,
}

impl Streams {
    fn open(&self, stream_id: StreamId) {
        self.accts.lock().unwrap().insert(stream_id, StreamAcct::default());
    }

    fn close(&self, stream_id: StreamId) {
        self.accts.lock().unwrap().remove(&stream_id);
    }

    /// Count `bytes` consumed on `stream_id`; `Some(consumed)` when an ack is
    /// due, which also marks it sent.
    fn consume(&self, stream_id: StreamId, bytes: u64) -> Option<u64> {
        let mut accts = self.accts.lock().unwrap();
        let acct = accts.get_mut(&stream_id)?;
        acct.consumed += bytes;
        if acct.consumed - acct.acked >= ACK_EVERY_BYTES {
            acct.acked = acct.consumed;
            Some(acct.consumed)
        } else {
            None
        }
    }

    /// The cumulative consumed bytes, for tests and the log.
    #[must_use]
    pub fn consumed(&self, stream_id: StreamId) -> Option<u64> {
        self.accts.lock().unwrap().get(&stream_id).map(|a| a.consumed)
    }
}

/// Decode a `terminal/frame` notification, account its bytes, and send the
/// ack when one is due. Returns the frame for the app.
pub(crate) async fn on_frame(
    session: &Session,
    streams: &Streams,
    params: serde_json::Value,
) -> Result<(StreamId, u64, TerminalFrameRecord), WireError> {
    let params: TerminalFrameParams =
        serde_json::from_value(params).map_err(WireError::protocol)?;
    let (frame, bytes) = TerminalFrameRecord::from_wire(params.frame)?;
    if matches!(frame, TerminalFrameRecord::Closed { .. }) {
        streams.close(params.stream_id);
    } else if let Some(consumed) = streams.consume(params.stream_id, bytes) {
        let _: TerminalAckResult = session
            .call(
                methods::TERMINAL_ACK,
                &TerminalAckParams {
                    stream_id: params.stream_id,
                    consumed,
                },
            )
            .await?;
    }
    Ok((params.stream_id, params.seq, frame))
}

/// `terminal/attach` for `session_key` on `host_id`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn attach(
    session: &Session,
    streams: &Streams,
    host_id: HostId,
    session_key: String,
    cols: Option<u16>,
    rows: Option<u16>,
    want_input: bool,
    scrollback_rows: Option<u32>,
) -> Result<TerminalAttachRecord, WireError> {
    let result: TerminalAttachResult = session
        .call(
            methods::TERMINAL_ATTACH,
            &TerminalAttachParams {
                session: SessionRef {
                    host_id,
                    session_key,
                },
                cols,
                rows,
                want_input,
                scrollback_rows,
            },
        )
        .await?;
    streams.open(result.stream_id);
    Ok(result.into())
}

/// `terminal/detach`.
pub(crate) async fn detach(
    session: &Session,
    streams: &Streams,
    stream_id: StreamId,
) -> Result<(), WireError> {
    let _: serde_json::Value = session
        .call(
            methods::TERMINAL_DETACH,
            &TerminalDetachParams { stream_id },
        )
        .await?;
    streams.close(stream_id);
    Ok(())
}

/// `terminal/input` under `mutation`; `ack` splits the ledger ack off the
/// result. A floor refusal is returned as a value.
pub(crate) async fn input(
    session: &Session,
    stream_id: StreamId,
    floor_gen: Option<u64>,
    data: &[u8],
    mutation: MutationEnvelope,
    ack: impl FnOnce(
        serde_json::Value,
    ) -> Result<(TerminalInputResult, Option<MutationReceipt>), WireError>,
) -> Result<TerminalInputOutcome, WireError> {
    let op_id = mutation.op_id.as_ref().map(|o| o.as_str().to_owned()).unwrap_or_default();
    let params = TerminalInputParams {
        stream_id,
        floor_gen,
        data: base64::engine::general_purpose::STANDARD.encode(data),
        mutation,
    };
    let value = match session
        .request(
            methods::TERMINAL_INPUT,
            serde_json::to_value(params).map_err(WireError::protocol)?,
        )
        .await
    {
        Ok(value) => value,
        Err(e) => {
            return floor_denied(&e)
                .map(|(holder, floor_gen)| TerminalInputOutcome::FloorDenied { holder, floor_gen })
                .ok_or(e);
        }
    };
    let (result, ack) = ack(value)?;
    Ok(TerminalInputOutcome::Typed {
        op_id,
        floor_gen: result.floor_gen,
        ack,
    })
}

/// `terminal/floor`. A floor refusal is returned as a value.
pub(crate) async fn floor(
    session: &Session,
    stream_id: StreamId,
    action: FloorAction,
    mutation: MutationEnvelope,
    ack: impl FnOnce(serde_json::Value) -> Result<(FloorState, Option<MutationReceipt>), WireError>,
) -> Result<TerminalFloorOutcome, WireError> {
    let op_id = mutation.op_id.as_ref().map(|o| o.as_str().to_owned()).unwrap_or_default();
    let value = match session
        .request(
            methods::TERMINAL_FLOOR,
            serde_json::to_value(TerminalFloorParams {
                stream_id,
                action,
                mutation,
            })
            .map_err(WireError::protocol)?,
        )
        .await
    {
        Ok(value) => value,
        Err(e) => {
            return floor_denied(&e)
                .map(|(holder, floor_gen)| TerminalFloorOutcome::FloorDenied { holder, floor_gen })
                .ok_or(e);
        }
    };
    let (state, ack) = ack(value)?;
    Ok(TerminalFloorOutcome::Floor {
        op_id,
        floor: state.into(),
        ack,
    })
}

/// `terminal/resize`.
pub(crate) async fn resize(
    session: &Session,
    stream_id: StreamId,
    cols: u16,
    rows: u16,
) -> Result<ResizeOutcome, WireError> {
    let result: TerminalResizeResult = session
        .call(
            methods::TERMINAL_RESIZE,
            &TerminalResizeParams {
                stream_id,
                cols,
                rows,
            },
        )
        .await?;
    Ok(result.into())
}

/// `terminal/scrollback`, decoded.
pub(crate) async fn scrollback(
    session: &Session,
    stream_id: StreamId,
    before_row: u64,
    rows: u32,
) -> Result<Vec<u8>, WireError> {
    let result: TerminalScrollbackResult = session
        .call(
            methods::TERMINAL_SCROLLBACK,
            &TerminalScrollbackParams {
                stream_id,
                before_row,
                rows,
            },
        )
        .await?;
    decode(&result.data)
}

/// Parse a floor action name.
pub(crate) fn floor_action(name: &str) -> Result<FloorAction, WireError> {
    match name {
        "acquire" => Ok(FloorAction::Acquire),
        "release" => Ok(FloorAction::Release),
        "take" => Ok(FloorAction::Take),
        other => Err(WireError::Protocol {
            message: format!("unknown floor action {other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frames_decode_their_bytes_and_count_only_payload() {
        let chunk: TerminalFrame =
            serde_json::from_value(json!({"kind": "snapshot_chunk", "data": "aGVsbG8="})).unwrap();
        let (frame, n) = TerminalFrameRecord::from_wire(chunk).unwrap();
        assert_eq!(
            frame,
            TerminalFrameRecord::SnapshotChunk {
                data: b"hello".to_vec()
            }
        );
        assert_eq!(n, 5);
        let gap: TerminalFrame = serde_json::from_value(
            json!({"kind": "data_gap", "reason": "dropped", "dropped_bytes": 9}),
        )
        .unwrap();
        assert_eq!(
            TerminalFrameRecord::from_wire(gap).unwrap(),
            (
                TerminalFrameRecord::DataGap {
                    reason: "dropped".into(),
                    dropped_bytes: Some(9)
                },
                0
            )
        );
        let newer: TerminalFrame =
            serde_json::from_value(json!({"kind": "hologram", "x": 1})).unwrap();
        assert_eq!(
            TerminalFrameRecord::from_wire(newer).unwrap().0,
            TerminalFrameRecord::Unknown
        );
        let bad: TerminalFrame =
            serde_json::from_value(json!({"kind": "output", "data": "***"})).unwrap();
        assert!(TerminalFrameRecord::from_wire(bad).is_err());
    }

    #[test]
    fn an_ack_is_due_every_256_kib_and_carries_the_cumulative_count() {
        let streams = Streams::default();
        streams.open(7);
        assert_eq!(streams.consume(7, ACK_EVERY_BYTES - 1), None);
        assert_eq!(streams.consume(7, 1), Some(ACK_EVERY_BYTES));
        assert_eq!(streams.consume(7, ACK_EVERY_BYTES - 1), None);
        assert_eq!(streams.consume(7, 2), Some(2 * ACK_EVERY_BYTES + 1));
        assert_eq!(streams.consumed(7), Some(2 * ACK_EVERY_BYTES + 1));
        assert_eq!(
            streams.consume(8, 10),
            None,
            "an unknown stream is not counted"
        );
        streams.close(7);
        assert_eq!(streams.consumed(7), None);
        assert!(floor_action("take").is_ok());
        assert!(floor_action("steal").is_err());
    }
}
