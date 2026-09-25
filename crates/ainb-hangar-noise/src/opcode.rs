//! The single opcode registry (D17: every number is permanent).
//!
//! | #  | Name            | Dir  | Status                                  |
//! |----|-----------------|------|-----------------------------------------|
//! | 20 | `Rpc`           | both | in use                                  |
//! | 21 | `StreamEnd`     | both | in use                                  |
//! | 22 | `Ping`          | both | in use                                  |
//! | 23 | `Pong`          | both | in use                                  |
//! | 1  | `Output`        | d→c  | reserved for R2's binary lane           |
//! | 2  | `SnapshotStart` | d→c  | reserved                                |
//! | 3  | `SnapshotChunk` | d→c  | reserved                                |
//! | 4  | `SnapshotEnd`   | d→c  | reserved                                |
//! | 5  | `DataGap`       | d→c  | reserved                                |
//! | 6  | `Resize`        | d→c  | reserved                                |
//! | 7  | (`Input`)       | c→d  | RETIRED spike 3 number, never reused    |
//! | 8  | `Floor`         | d→c  | reserved                                |
//! | 9  | (`Subscribe`)   | c→d  | RETIRED spike 3 number, never reused    |
//! | 10 | `Presence`      | d→c  | reserved                                |
//! | 11 | `Closed`        | d→c  | reserved                                |
//! | 13 | `Ack`           | c→d  | reserved                                |
//!
//! 0, 12, 14 to 19 and 24 and up are free, assigned only by an edit here.
//! Terminal frames travel today as base64 JSON `terminal/frame` notifications
//! inside [`Opcode::Rpc`]; the reserved numbers are what they become if a
//! binary lane is ever added. A receiver drops a frame with an unknown or
//! retired opcode and counts it; a sender uses a reserved opcode only after a
//! capability that names it was advertised.

use serde::{Deserialize, Serialize};

/// Opcodes retired from spike 3 (`Input` 7, `Subscribe` 9). Never reused.
pub const RETIRED: [u8; 2] = [7, 9];

/// A frame opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum Opcode {
    /// R2: live pane output. Reserved.
    Output = 1,
    /// R2: snapshot start. Reserved.
    SnapshotStart = 2,
    /// R2: snapshot chunk. Reserved.
    SnapshotChunk = 3,
    /// R2: snapshot end. Reserved.
    SnapshotEnd = 4,
    /// R2: data gap. Reserved.
    DataGap = 5,
    /// R2: pane resize. Reserved.
    Resize = 6,
    /// R2: floor change. Reserved.
    Floor = 8,
    /// R2: native client presence. Reserved.
    Presence = 10,
    /// R2: stream closed. Reserved.
    Closed = 11,
    /// R2: flow-control ack, client to daemon. Reserved.
    Ack = 13,
    /// JSON-RPC bytes, LSP `Content-Length` framed, `stream_id = 0`. In use.
    Rpc = 20,
    /// End of a stream. In use.
    StreamEnd = 21,
    /// Heartbeat request; the peer echoes a [`Self::Pong`] with the same `seq`.
    /// In use.
    Ping = 22,
    /// Heartbeat reply. In use.
    Pong = 23,
}

impl Opcode {
    /// Every assigned opcode, in number order.
    pub const ALL: [Self; 14] = [
        Self::Output,
        Self::SnapshotStart,
        Self::SnapshotChunk,
        Self::SnapshotEnd,
        Self::DataGap,
        Self::Resize,
        Self::Floor,
        Self::Presence,
        Self::Closed,
        Self::Ack,
        Self::Rpc,
        Self::StreamEnd,
        Self::Ping,
        Self::Pong,
    ];

    /// The wire byte.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The opcode for `byte`, or `None` for a free or retired number.
    #[must_use]
    pub const fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Output,
            2 => Self::SnapshotStart,
            3 => Self::SnapshotChunk,
            4 => Self::SnapshotEnd,
            5 => Self::DataGap,
            6 => Self::Resize,
            8 => Self::Floor,
            10 => Self::Presence,
            11 => Self::Closed,
            13 => Self::Ack,
            20 => Self::Rpc,
            21 => Self::StreamEnd,
            22 => Self::Ping,
            23 => Self::Pong,
            _ => return None,
        })
    }

    /// Whether this opcode is on the wire in version 1 (the rest are reserved).
    #[must_use]
    pub const fn in_use(self) -> bool {
        matches!(self, Self::Rpc | Self::StreamEnd | Self::Ping | Self::Pong)
    }

    /// The registry name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Output => "Output",
            Self::SnapshotStart => "SnapshotStart",
            Self::SnapshotChunk => "SnapshotChunk",
            Self::SnapshotEnd => "SnapshotEnd",
            Self::DataGap => "DataGap",
            Self::Resize => "Resize",
            Self::Floor => "Floor",
            Self::Presence => "Presence",
            Self::Closed => "Closed",
            Self::Ack => "Ack",
            Self::Rpc => "Rpc",
            Self::StreamEnd => "StreamEnd",
            Self::Ping => "Ping",
            Self::Pong => "Pong",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_opcode_round_trips_and_retired_numbers_decode_to_none() {
        for op in Opcode::ALL {
            assert_eq!(Opcode::from_u8(op.as_u8()), Some(op));
            assert!(
                !RETIRED.contains(&op.as_u8()),
                "{op:?} reuses a retired number"
            );
        }
        for retired in RETIRED {
            assert_eq!(Opcode::from_u8(retired), None);
        }
        assert_eq!(Opcode::from_u8(0), None);
        assert_eq!(Opcode::from_u8(12), None);
        assert_eq!(Opcode::from_u8(24), None);
    }

    #[test]
    fn only_rpc_stream_end_ping_pong_are_in_use() {
        let used: Vec<u8> = Opcode::ALL.iter().filter(|o| o.in_use()).map(|o| o.as_u8()).collect();
        assert_eq!(used, vec![20, 21, 22, 23]);
    }
}
