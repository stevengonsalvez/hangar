//! The 16-byte little-endian frame header (the layout both spikes used).
//!
//! ```text
//!  0     1      2       3      4..7       8..11     12..15   16..
//!  0x74  ver=1  opcode  flags  stream_id  seq_high  seq_low  payload
//!  flags bit0 = FIN (last fragment of a logical message)
//! ```
//!
//! `stream_id` is 0 for [`crate::Opcode::Rpc`]. `seq` is a u64 split into two
//! little-endian u32 halves, high half first.

use crate::opcode::Opcode;

/// The first header byte.
pub const MAGIC: u8 = 0x74;
/// The header version this build writes and reads.
pub const VERSION: u8 = 1;
/// The header length in bytes.
pub const HEADER_LEN: usize = 16;
/// `flags` bit 0: this is the last fragment of a logical message.
pub const FLAG_FIN: u8 = 0b0000_0001;

/// Why bytes are not a frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    /// Fewer than [`HEADER_LEN`] bytes.
    Short(usize),
    /// The first byte is not [`MAGIC`].
    Magic(u8),
    /// A header version this build does not read.
    Version(u8),
    /// A free or retired opcode: the receiver drops the frame and counts it.
    UnknownOpcode(u8),
    /// A flags bit this version does not define.
    Flags(u8),
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Short(n) => write!(f, "frame header needs {HEADER_LEN} bytes, got {n}"),
            Self::Magic(b) => write!(f, "bad frame magic {b:#04x}"),
            Self::Version(v) => write!(f, "unsupported frame version {v}"),
            Self::UnknownOpcode(o) => write!(f, "unknown opcode {o}"),
            Self::Flags(b) => write!(f, "undefined frame flags {b:#04x}"),
        }
    }
}

impl std::error::Error for HeaderError {}

/// One frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// The opcode.
    pub opcode: Opcode,
    /// Whether this is the last fragment.
    pub fin: bool,
    /// The stream; 0 for Rpc.
    pub stream_id: u32,
    /// The sequence number.
    pub seq: u64,
}

impl FrameHeader {
    /// Encode to the 16 wire bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0] = MAGIC;
        out[1] = VERSION;
        out[2] = self.opcode.as_u8();
        out[3] = if self.fin { FLAG_FIN } else { 0 };
        out[4..8].copy_from_slice(&self.stream_id.to_le_bytes());
        let high = u32::try_from(self.seq >> 32).unwrap_or(u32::MAX);
        let low = u32::try_from(self.seq & u64::from(u32::MAX)).unwrap_or(u32::MAX);
        out[8..12].copy_from_slice(&high.to_le_bytes());
        out[12..16].copy_from_slice(&low.to_le_bytes());
        out
    }

    /// Decode the first [`HEADER_LEN`] bytes of `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<Self, HeaderError> {
        let Some(head) = bytes.get(..HEADER_LEN) else {
            return Err(HeaderError::Short(bytes.len()));
        };
        if head[0] != MAGIC {
            return Err(HeaderError::Magic(head[0]));
        }
        if head[1] != VERSION {
            return Err(HeaderError::Version(head[1]));
        }
        let opcode = Opcode::from_u8(head[2]).ok_or(HeaderError::UnknownOpcode(head[2]))?;
        if head[3] & !FLAG_FIN != 0 {
            return Err(HeaderError::Flags(head[3]));
        }
        let word =
            |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
        Ok(Self {
            opcode,
            fin: head[3] & FLAG_FIN != 0,
            stream_id: word(4),
            seq: (u64::from(word(8)) << 32) | u64::from(word(12)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_round_trips() {
        let header = FrameHeader {
            opcode: Opcode::Ping,
            fin: true,
            stream_id: 0x0102_0304,
            seq: 0x0506_0708_090a_0b0c,
        };
        let bytes = header.encode();
        assert_eq!(
            bytes,
            [
                0x74, 1, 22, 1, 0x04, 0x03, 0x02, 0x01, 0x08, 0x07, 0x06, 0x05, 0x0c, 0x0b, 0x0a,
                0x09
            ]
        );
        assert_eq!(FrameHeader::decode(&bytes), Ok(header));
    }

    #[test]
    fn bad_headers_are_refused() {
        let good = FrameHeader {
            opcode: Opcode::Rpc,
            fin: false,
            stream_id: 0,
            seq: 1,
        }
        .encode();
        assert_eq!(
            FrameHeader::decode(&good[..15]),
            Err(HeaderError::Short(15))
        );
        let mut bad = good;
        bad[0] = 0x75;
        assert_eq!(FrameHeader::decode(&bad), Err(HeaderError::Magic(0x75)));
        let mut bad = good;
        bad[1] = 2;
        assert_eq!(FrameHeader::decode(&bad), Err(HeaderError::Version(2)));
        let mut bad = good;
        bad[2] = 7;
        assert_eq!(
            FrameHeader::decode(&bad),
            Err(HeaderError::UnknownOpcode(7))
        );
        let mut bad = good;
        bad[3] = 0b10;
        assert_eq!(FrameHeader::decode(&bad), Err(HeaderError::Flags(0b10)));
    }
}
