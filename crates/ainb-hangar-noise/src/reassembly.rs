//! Whole frames, and one logical Rpc message split across them.
//!
//! A [`Frame`] is the 16-byte [`FrameHeader`] plus its payload: exactly what
//! one Noise transport message decrypts to. A logical Rpc message (the same
//! LSP `Content-Length` JSON-RPC bytes the unix leg carries) that does not fit
//! one Noise message is cut by [`rpc_frames`] into fragments, every one but
//! the last with `FIN` clear, and put back together by a [`Reassembler`].
//!
//! ```text
//!  logical message (<= 16 MiB)
//!  ├── Frame { Rpc, fin: false, seq: 0, payload[..65,503] }
//!  ├── Frame { Rpc, fin: false, seq: 1, payload[..65,503] }
//!  └── Frame { Rpc, fin: true,  seq: 2, payload[rest] }
//! ```
//!
//! `seq` on an Rpc fragment is its index within the logical message, as in
//! spike 5/6. Noise already rejects a reordered, dropped or replayed message
//! (every transport message has its own nonce), so reassembly does not
//! re-check it.

use crate::frame::{FrameHeader, HEADER_LEN, HeaderError};
use crate::opcode::Opcode;
use crate::{MAX_NOISE_MESSAGE, MAX_PREAUTH_REASSEMBLED, MAX_REASSEMBLED};

/// The authentication tag Noise appends to every transport message.
pub const TAG_LEN: usize = 16;

/// The largest payload one frame carries: a Noise message minus its tag and
/// the frame header.
pub const MAX_FRAME_PAYLOAD: usize = MAX_NOISE_MESSAGE - TAG_LEN - HEADER_LEN;

/// One frame: a header and its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The header.
    pub header: FrameHeader,
    /// The payload.
    pub payload: Vec<u8>,
}

impl Frame {
    /// A single, final frame on stream 0 (Ping, Pong, a short Rpc).
    #[must_use]
    pub const fn control(opcode: Opcode, seq: u64, payload: Vec<u8>) -> Self {
        Self {
            header: FrameHeader {
                opcode,
                fin: true,
                stream_id: 0,
                seq,
            },
            payload,
        }
    }

    /// The header bytes followed by the payload.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&self.header.encode());
        out.extend_from_slice(&self.payload);
        out
    }

    /// Decode one frame from the plaintext of one Noise message.
    pub fn decode(bytes: &[u8]) -> Result<Self, HeaderError> {
        let header = FrameHeader::decode(bytes)?;
        Ok(Self {
            header,
            payload: bytes[HEADER_LEN..].to_vec(),
        })
    }
}

/// Split one logical Rpc message into frames that each fit a Noise message.
///
/// An empty message is one empty final frame.
#[must_use]
pub fn rpc_frames(lsp: &[u8]) -> Vec<Frame> {
    let chunks: Vec<&[u8]> = if lsp.is_empty() {
        vec![&[]]
    } else {
        lsp.chunks(MAX_FRAME_PAYLOAD).collect()
    };
    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| Frame {
            header: FrameHeader {
                opcode: Opcode::Rpc,
                fin: index == last,
                stream_id: 0,
                seq: index as u64,
            },
            payload: chunk.to_vec(),
        })
        .collect()
}

/// Why a frame cannot join the logical message being reassembled. Every one
/// is fatal to the session: the peer closes 4401.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReassemblyError {
    /// Only Rpc frames are reassembled.
    NotRpc(Opcode),
    /// Rpc rides stream 0.
    StreamId(u32),
    /// The logical message would pass the cap.
    TooLarge {
        /// The cap in bytes.
        cap: usize,
    },
}

impl std::fmt::Display for ReassemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRpc(op) => write!(f, "cannot reassemble a {} frame", op.name()),
            Self::StreamId(id) => write!(f, "rpc frame on stream {id}, not 0"),
            Self::TooLarge { cap } => write!(f, "rpc message passes the {cap}-byte cap"),
        }
    }
}

impl std::error::Error for ReassemblyError {}

/// Puts Rpc fragments back together until `FIN`.
#[derive(Debug)]
pub struct Reassembler {
    buf: Vec<u8>,
    cap: usize,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Reassembler {
    /// A reassembler capped at [`MAX_REASSEMBLED`], the unix leg's body cap.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_cap(MAX_REASSEMBLED)
    }

    /// A reassembler for a session that has not authenticated yet, capped at
    /// [`MAX_PREAUTH_REASSEMBLED`] (64 KiB). The peer leg starts every
    /// session here and calls [`Self::authenticated`] once `auth/hello` or
    /// `device/redeem` is accepted, so an unauthenticated socket can make the
    /// host hold at most 64 KiB, not 16 MiB.
    #[must_use]
    pub const fn pre_auth() -> Self {
        Self::with_cap(MAX_PREAUTH_REASSEMBLED)
    }

    /// A reassembler with its own cap on one logical message.
    #[must_use]
    pub const fn with_cap(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
        }
    }

    /// The session authenticated: raise the cap to [`MAX_REASSEMBLED`], the
    /// unix leg's body cap. Never lowers a cap.
    pub fn authenticated(&mut self) {
        self.cap = self.cap.max(MAX_REASSEMBLED);
    }

    /// The cap on one logical message.
    #[must_use]
    pub const fn cap(&self) -> usize {
        self.cap
    }

    /// Add a fragment. Returns the whole logical message on `FIN`.
    ///
    /// The cap is checked before the bytes are copied, so a peer that never
    /// sends `FIN` holds at most `cap` bytes.
    pub fn push(&mut self, frame: &Frame) -> Result<Option<Vec<u8>>, ReassemblyError> {
        if frame.header.opcode != Opcode::Rpc {
            return Err(ReassemblyError::NotRpc(frame.header.opcode));
        }
        if frame.header.stream_id != 0 {
            return Err(ReassemblyError::StreamId(frame.header.stream_id));
        }
        if self.buf.len() + frame.payload.len() > self.cap {
            self.buf = Vec::new();
            return Err(ReassemblyError::TooLarge { cap: self.cap });
        }
        self.buf.extend_from_slice(&frame.payload);
        Ok(frame.header.fin.then(|| std::mem::take(&mut self.buf)))
    }

    /// Whether no fragment is waiting for its `FIN`.
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        self.buf.is_empty()
    }
}

/// Why bytes are not one LSP-framed body. The rules are the daemon's own
/// unix-leg `read_frame`, so both legs refuse the same inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspError {
    /// No `\r\n\r\n` after the headers.
    NoTerminator,
    /// A header line without `name: value`, or not UTF-8.
    MalformedHeader,
    /// A header line holding a bare CR or LF: every header line ends with
    /// exactly one `\r\n` and holds no other line break.
    StrayLineBreak,
    /// A header other than `Content-Length`. The text is the peer's header
    /// name, truncated to [`PEER_TEXT_MAX`] characters and escaped, so it is
    /// safe to log.
    UnsupportedHeader(String),
    /// `Content-Length` given twice.
    DuplicateContentLength,
    /// No `Content-Length` header, or one that is not an unsigned decimal.
    ContentLength,
    /// `Content-Length` above [`MAX_REASSEMBLED`], the unix leg's body cap.
    TooLarge(usize),
    /// The body is not the declared length.
    Length {
        /// The declared length.
        declared: usize,
        /// The bytes after the headers.
        actual: usize,
    },
}

impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTerminator => f.write_str("no LSP header terminator"),
            Self::MalformedHeader => f.write_str("malformed frame header"),
            Self::StrayLineBreak => f.write_str("frame header line holds a bare CR or LF"),
            Self::UnsupportedHeader(name) => write!(f, "unsupported frame header: {name}"),
            Self::DuplicateContentLength => f.write_str("duplicate Content-Length header"),
            Self::ContentLength => {
                f.write_str("Content-Length must be an unsigned decimal byte length")
            }
            Self::TooLarge(len) => {
                write!(f, "Content-Length {len} exceeds cap {MAX_REASSEMBLED}")
            }
            Self::Length { declared, actual } => {
                write!(
                    f,
                    "Content-Length {declared} but the body is {actual} bytes"
                )
            }
        }
    }
}

impl std::error::Error for LspError {}

/// The most characters of peer-supplied text an error keeps.
pub const PEER_TEXT_MAX: usize = 32;

/// Peer text made safe to log: at most [`PEER_TEXT_MAX`] characters, each
/// escaped (`\n`, `\u{1b}`, quotes), with `...` when it was cut.
fn peer_text(text: &str) -> String {
    let mut out: String = text.chars().take(PEER_TEXT_MAX).flat_map(char::escape_default).collect();
    if text.chars().nth(PEER_TEXT_MAX).is_some() {
        out.push_str("...");
    }
    out
}

/// Frame `body` the way the unix leg does: `Content-Length: N\r\n\r\n` + body.
#[must_use]
pub fn lsp_encode(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// The body of one whole LSP-framed message, as strict as the unix leg's
/// `read_frame`, and stricter where that one trims: `\r\n` line ends with no
/// bare CR or LF inside a header line, `Content-Length` as the only header and
/// given once, an unsigned decimal value no larger than [`MAX_REASSEMBLED`],
/// and exactly that many body bytes (the message is whole, so a trailing byte
/// is refused too).
pub fn lsp_body(message: &[u8]) -> Result<&[u8], LspError> {
    let split = message
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or(LspError::NoTerminator)?;
    let headers = std::str::from_utf8(&message[..split]).map_err(|_| LspError::MalformedHeader)?;
    let mut declared = None;
    for line in headers.split("\r\n") {
        // A CR or LF left inside a line is a second line ending the split did
        // not see; `trim` below would otherwise swallow it.
        if line.bytes().any(|b| b == b'\r' || b == b'\n') {
            return Err(LspError::StrayLineBreak);
        }
        let (name, value) = line.split_once(':').ok_or(LspError::MalformedHeader)?;
        if !name.trim().eq_ignore_ascii_case("content-length") {
            return Err(LspError::UnsupportedHeader(peer_text(name.trim())));
        }
        if declared.is_some() {
            return Err(LspError::DuplicateContentLength);
        }
        let value = value.trim();
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(LspError::ContentLength);
        }
        let len: usize = value.parse().map_err(|_| LspError::ContentLength)?;
        if len > MAX_REASSEMBLED {
            return Err(LspError::TooLarge(len));
        }
        declared = Some(len);
    }
    let declared = declared.ok_or(LspError::ContentLength)?;
    let body = &message[split + 4..];
    if body.len() != declared {
        return Err(LspError::Length {
            declared,
            actual: body.len(),
        });
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips_through_its_bytes() {
        let frame = Frame::control(Opcode::Ping, 9, b"hi".to_vec());
        assert_eq!(Frame::decode(&frame.encode()), Ok(frame));
    }

    #[test]
    fn a_message_splits_at_the_frame_payload_limit_and_only_the_last_is_final() {
        let bytes = vec![7u8; MAX_FRAME_PAYLOAD * 2 + 5];
        let frames = rpc_frames(&bytes);
        assert_eq!(frames.len(), 3);
        assert_eq!(
            frames.iter().map(|f| f.header.fin).collect::<Vec<_>>(),
            [false, false, true]
        );
        assert_eq!(
            frames.iter().map(|f| f.header.seq).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        for frame in &frames {
            assert!(frame.encode().len() + TAG_LEN <= MAX_NOISE_MESSAGE);
        }
        let mut r = Reassembler::new();
        assert_eq!(r.push(&frames[0]), Ok(None));
        assert!(!r.is_idle());
        assert_eq!(r.push(&frames[1]), Ok(None));
        assert_eq!(r.push(&frames[2]), Ok(Some(bytes)));
        assert!(r.is_idle());
    }

    #[test]
    fn an_empty_message_is_one_empty_final_frame() {
        let frames = rpc_frames(&[]);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].header.fin);
        assert_eq!(Reassembler::new().push(&frames[0]), Ok(Some(Vec::new())));
    }

    #[test]
    fn reassembly_refuses_other_frames_and_the_cap() {
        let mut r = Reassembler::with_cap(4);
        assert_eq!(
            r.push(&Frame::control(Opcode::Ping, 0, Vec::new())),
            Err(ReassemblyError::NotRpc(Opcode::Ping))
        );
        let mut on_stream = rpc_frames(b"ab").remove(0);
        on_stream.header.stream_id = 3;
        assert_eq!(r.push(&on_stream), Err(ReassemblyError::StreamId(3)));
        let mut first = rpc_frames(b"abc").remove(0);
        first.header.fin = false;
        assert_eq!(r.push(&first), Ok(None));
        assert_eq!(
            r.push(&rpc_frames(b"de").remove(0)),
            Err(ReassemblyError::TooLarge { cap: 4 })
        );
        assert!(r.is_idle(), "a refused message is dropped whole");
        assert_eq!(
            r.push(&rpc_frames(b"abcd").remove(0)),
            Ok(Some(b"abcd".to_vec()))
        );
    }

    #[test]
    fn lsp_framing_round_trips_and_checks_the_length() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        assert_eq!(lsp_body(&lsp_encode(body)), Ok(&body[..]));
        assert_eq!(lsp_body(b"content-length: 2\r\n\r\nab"), Ok(&b"ab"[..]));
        assert_eq!(lsp_body(b"Content-Length: 2"), Err(LspError::NoTerminator));
        assert_eq!(
            lsp_body(b"Length: 2\r\n\r\nab"),
            Err(LspError::UnsupportedHeader("Length".to_string()))
        );
        assert_eq!(
            lsp_body(b"Content-Length: 3\r\n\r\nab"),
            Err(LspError::Length {
                declared: 3,
                actual: 2
            })
        );
    }

    /// Every input the daemon's unix-leg `read_frame` refuses, this refuses.
    #[test]
    fn lsp_body_is_as_strict_as_the_unix_leg() {
        let refused: [(&[u8], LspError); 9] = [
            (
                b"Content-Length: 2\r\nContent-Length: 2\r\n\r\nab",
                LspError::DuplicateContentLength,
            ),
            (
                b"Content-Length: 2\r\nX-Other: y\r\n\r\nab",
                LspError::UnsupportedHeader("X-Other".to_string()),
            ),
            (b"Content-Length: +2\r\n\r\nab", LspError::ContentLength),
            (b"Content-Length: -2\r\n\r\nab", LspError::ContentLength),
            (b"Content-Length: 0x2\r\n\r\nab", LspError::ContentLength),
            (b"Content-Length:\r\n\r\nab", LspError::ContentLength),
            (b"Content-Length 2\r\n\r\nab", LspError::MalformedHeader),
            (
                b"Content-Length: 99999999999999999999\r\n\r\n",
                LspError::ContentLength,
            ),
            (
                b"Content-Length: 2\r\n\r\nabc",
                LspError::Length {
                    declared: 2,
                    actual: 3,
                },
            ),
        ];
        for (input, want) in refused {
            assert_eq!(
                lsp_body(input),
                Err(want),
                "{}",
                String::from_utf8_lossy(input)
            );
        }
        let over = format!("Content-Length: {}\r\n\r\n", MAX_REASSEMBLED + 1);
        assert_eq!(
            lsp_body(over.as_bytes()),
            Err(LspError::TooLarge(MAX_REASSEMBLED + 1))
        );
        assert_eq!(
            lsp_body(b"Content-Length: 2\n\nab"),
            Err(LspError::NoTerminator),
            "bare LF line ends are refused, as on the unix leg"
        );
        assert_eq!(lsp_body(b"Content-Length:  2 \r\n\r\nab"), Ok(&b"ab"[..]));
    }

    #[test]
    fn an_unauthenticated_session_reassembles_at_most_64_kib() {
        let mut r = Reassembler::pre_auth();
        assert_eq!(r.cap(), MAX_PREAUTH_REASSEMBLED);
        let fits = vec![1u8; MAX_PREAUTH_REASSEMBLED];
        let mut whole = None;
        for frame in rpc_frames(&fits) {
            whole = r.push(&frame).unwrap();
        }
        assert_eq!(whole.as_deref(), Some(&fits[..]));
        let over = vec![1u8; MAX_PREAUTH_REASSEMBLED + 1];
        let result: Result<Vec<_>, _> = rpc_frames(&over).iter().map(|f| r.push(f)).collect();
        assert_eq!(
            result.unwrap_err(),
            ReassemblyError::TooLarge {
                cap: MAX_PREAUTH_REASSEMBLED
            }
        );
        r.authenticated();
        assert_eq!(r.cap(), MAX_REASSEMBLED);
        let mut whole = None;
        for frame in rpc_frames(&over) {
            whole = r.push(&frame).unwrap();
        }
        assert_eq!(whole.map(|w| w.len()), Some(MAX_PREAUTH_REASSEMBLED + 1));
        let mut small = Reassembler::with_cap(MAX_REASSEMBLED * 2);
        small.authenticated();
        assert_eq!(small.cap(), MAX_REASSEMBLED * 2, "never lowers a cap");
    }

    /// F1: a bare CR or LF inside a header line is refused, not trimmed
    /// away. Each of these was accepted before.
    #[test]
    fn a_header_line_with_a_bare_cr_or_lf_is_refused() {
        let probes: [&[u8]; 4] = [
            b"Content-Length: 2\n\r\n\r\nab",
            b"Content-Length: 2\r\r\n\r\nab",
            b"Content-Length:\n2\r\n\r\nab",
            b"Content-Length\r: 2\r\n\r\nab",
        ];
        for probe in probes {
            assert_eq!(
                lsp_body(probe),
                Err(LspError::StrayLineBreak),
                "{}",
                String::from_utf8_lossy(probe).escape_default()
            );
        }
    }

    /// F4: the peer's header name reaches an error truncated and escaped.
    #[test]
    fn an_unsupported_header_is_truncated_and_escaped() {
        let long = format!("X-{}: 1\r\n\r\n", "a".repeat(200));
        let Err(LspError::UnsupportedHeader(text)) = lsp_body(long.as_bytes()) else {
            panic!("refused as unsupported");
        };
        assert_eq!(text, format!("X-{}...", "a".repeat(PEER_TEXT_MAX - 2)));
        let Err(LspError::UnsupportedHeader(text)) = lsp_body(b"X\x1b[31m\t\"y: 1\r\n\r\n") else {
            panic!("refused as unsupported");
        };
        assert_eq!(text, "X\\u{1b}[31m\\t\\\"y");
        assert!(!text.contains('\u{1b}'), "no raw escape reaches a log line");
    }
}
