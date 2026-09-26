//! Per-viewer outbound queue: credit window, chunking, `data_gap` and the
//! re-snapshot request (spec R2 row, RECONCILED T15, T16, M7).
//!
//! One queue per attached stream. The feed pushes every pane byte into every
//! viewer's queue; the daemon's writer drains each queue as far as that
//! viewer's credit allows. A viewer that stops acking stops receiving; one
//! that falls further than the pending cap behind is cut loose with
//! `data_gap{dropped}` and gets a fresh snapshot instead of a backlog, so a
//! slow phone never holds a byte buffer the size of the flood (spike 7: tmux
//! itself throttles the producing program when nobody drains it, which is
//! the one outcome R2 must never cause).
//!
//! ```text
//! feed ──push_output(seq, bytes)──▶ pending ──take_ready(usize::MAX)──▶ writer ──▶ socket
//!                                     │  ▲                      │
//!            cap hit: drop, DataGap ──┘  │    ack(consumed) ◀───┘ client
//!            needs_snapshot ──▶ daemon ──push_snapshot()
//! ```
//!
//! Credit is application-level and cumulative (RECONCILED M7): `consumed` is
//! the decoded payload bytes of every frame on the stream since attach,
//! snapshot frames included; a value lower than one already seen is ignored,
//! which makes a retried ack idempotent. Control frames (`resize`, `floor`,
//! `presence`, `closed`, `data_gap`) carry no payload and never wait for
//! credit. They keep their place in the order, so behind unsent output
//! they wait with it; to keep a stalled viewer's memory bounded, a new
//! `floor`, `presence`, `resize` or `data_gap` REPLACES any unsent one of
//! the same kind (the latest state is all the client needs), so at most
//! four control frames plus one `closed` ever wait.
//!
//! `seq` is the pane-feed offset at emit time (T16): each `output` chunk
//! carries the offset of ITS first byte, every frame of one snapshot
//! carries the offset N the snapshot was taken at. The caller supplies the
//! offset of a push, this queue adds the chunk offsets.
//!
//! The writer takes with [`ViewerQueue::take_ready`]`(max_frames)` no more
//! than its channel can accept, because a taken frame is counted as sent
//! and cannot be returned.

use std::collections::VecDeque;

use crate::floor::Holder;

/// The flow-control window: payload bytes a daemon sends ahead of the last
/// ack. Mirrors the frozen wire constant.
pub const WINDOW_BYTES: u64 = 2 * 1024 * 1024;
/// The largest decoded payload one `output` or `snapshot_chunk` frame
/// carries. Mirrors the frozen wire constant.
pub const CHUNK_BYTES: usize = 48 * 1024;

/// Why a stream skipped bytes. Mirrors the frozen wire `DataGapReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapReason {
    /// tmux paused the pane for the daemon's client; the resume is not
    /// contiguous.
    Paused,
    /// The daemon lost its pane feed and re-seeded.
    FeedLost,
    /// This viewer fell behind the pending cap and its backlog was dropped.
    Dropped,
    /// The session ended; `closed` follows.
    SessionGone,
}

/// One frame of an attached stream, before base64 and the wire envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
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
    SnapshotChunk(Vec<u8>),
    /// The snapshot is complete.
    SnapshotEnd,
    /// Live pane output.
    Output(Vec<u8>),
    /// The pane changed size.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// Bytes were skipped; the client waits for the next snapshot.
    DataGap {
        /// Why.
        reason: GapReason,
        /// How many bytes, when known.
        dropped_bytes: Option<u64>,
    },
    /// The floor changed.
    Floor {
        /// The new holder, or none when free.
        holder: Option<Holder>,
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

impl Frame {
    /// Decoded payload bytes this frame counts against the window.
    pub fn payload_len(&self) -> u64 {
        match self {
            Self::SnapshotChunk(data) | Self::Output(data) => data.len() as u64,
            _ => 0,
        }
    }

    /// The kind a later frame of the same kind supersedes while unsent.
    const fn coalesce_key(&self) -> Option<u8> {
        match self {
            Self::Resize { .. } => Some(0),
            Self::DataGap { .. } => Some(1),
            Self::Floor { .. } => Some(2),
            Self::Presence { .. } => Some(3),
            _ => None,
        }
    }
}

/// A frame with the feed offset it was emitted at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sequenced {
    /// The pane-feed offset at emit time.
    pub seq: u64,
    /// The frame.
    pub frame: Frame,
}

/// Queue limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerConfig {
    /// Payload bytes in flight (sent, not yet acked) before sending stops.
    pub window_bytes: u64,
    /// Largest payload per frame.
    pub chunk_bytes: usize,
    /// Payload bytes waiting BEYOND the credit before the viewer is cut
    /// loose: what is pending minus what the next `take_ready` can send.
    pub max_pending_bytes: u64,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            window_bytes: WINDOW_BYTES,
            chunk_bytes: CHUNK_BYTES,
            // ponytail: one window of backlog per viewer, so a viewer costs at
            // most two windows of memory; make it its own knob if phones
            // want a deeper buffer over a poor link.
            max_pending_bytes: WINDOW_BYTES,
        }
    }
}

/// The outbound state of one viewer.
#[derive(Debug, Clone)]
pub struct ViewerQueue {
    cfg: ViewerConfig,
    pending: VecDeque<Sequenced>,
    /// Payload bytes in `pending`.
    pending_bytes: u64,
    /// Cumulative payload bytes handed to the writer.
    sent_bytes: u64,
    /// The highest `consumed` acked.
    consumed: u64,
    /// Output is being discarded until the next snapshot.
    in_gap: bool,
    /// The daemon owes this viewer a snapshot.
    needs_snapshot: bool,
    /// Bytes discarded since the gap opened.
    dropped_since_gap: u64,
}

impl Default for ViewerQueue {
    fn default() -> Self {
        Self::new(ViewerConfig::default())
    }
}

impl ViewerQueue {
    /// An empty queue that owes the viewer its first snapshot.
    pub fn new(cfg: ViewerConfig) -> Self {
        let window_bytes = cfg.window_bytes.max(1);
        // A chunk larger than the window could never fit the credit.
        let chunk_bytes =
            cfg.chunk_bytes.max(1).min(usize::try_from(window_bytes).unwrap_or(usize::MAX));
        Self {
            cfg: ViewerConfig {
                window_bytes,
                chunk_bytes,
                ..cfg
            },
            pending: VecDeque::new(),
            pending_bytes: 0,
            sent_bytes: 0,
            consumed: 0,
            in_gap: true,
            needs_snapshot: true,
            dropped_since_gap: 0,
        }
    }

    /// Whether the daemon must take a snapshot from the emulator and
    /// [`push_snapshot`](Self::push_snapshot) it before output flows again.
    pub const fn needs_snapshot(&self) -> bool {
        self.needs_snapshot
    }

    /// Whether output is currently being discarded.
    pub const fn in_gap(&self) -> bool {
        self.in_gap
    }

    /// Payload bytes waiting for credit.
    pub const fn pending_bytes(&self) -> u64 {
        self.pending_bytes
    }

    /// Frames waiting, payload or not.
    pub fn pending_frames(&self) -> usize {
        self.pending.len()
    }

    /// Payload bytes sent and not yet acked.
    pub const fn in_flight(&self) -> u64 {
        self.sent_bytes - self.consumed
    }

    /// Payload bytes that may be sent now.
    pub const fn credit(&self) -> u64 {
        self.cfg.window_bytes.saturating_sub(self.in_flight())
    }

    /// Cumulative payload bytes handed to the writer.
    pub const fn sent_bytes(&self) -> u64 {
        self.sent_bytes
    }

    /// Bytes discarded since the current gap opened, or since the last
    /// snapshot closed one.
    pub const fn dropped_since_gap(&self) -> u64 {
        self.dropped_since_gap
    }

    /// The client acked: `consumed` cumulative payload bytes. Lower or equal
    /// values are ignored; a value above what was sent is clamped, so a
    /// buggy client cannot mint credit.
    pub fn ack(&mut self, consumed: u64) {
        let consumed = consumed.min(self.sent_bytes);
        if consumed > self.consumed {
            self.consumed = consumed;
        }
    }

    /// Payload bytes pending beyond what the credit can send now.
    pub const fn backlog(&self) -> u64 {
        self.pending_bytes.saturating_sub(self.credit())
    }

    /// Live pane bytes starting at feed offset `seq`. Discarded while a gap
    /// is open. Opens a `dropped` gap when the backlog (pending beyond the
    /// credit) would pass the cap. Each chunk carries its own offset.
    pub fn push_output(&mut self, seq: u64, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.in_gap {
            self.dropped_since_gap += bytes.len() as u64;
            return;
        }
        let after = (self.pending_bytes + bytes.len() as u64).saturating_sub(self.credit());
        if after > self.cfg.max_pending_bytes {
            let discarded = self.discard_output() + bytes.len() as u64;
            self.open_gap(seq, GapReason::Dropped, Some(discarded));
            self.dropped_since_gap = discarded;
            return;
        }
        for (i, chunk) in bytes.chunks(self.cfg.chunk_bytes).enumerate() {
            let offset = (i * self.cfg.chunk_bytes) as u64;
            self.enqueue(seq + offset, Frame::Output(chunk.to_vec()));
        }
    }

    /// A gap the feed declared for every viewer: `paused`, `feed_lost` or
    /// `session_gone`. Pending output is discarded; a snapshot is owed
    /// unless the session is gone.
    pub fn gap(&mut self, seq: u64, reason: GapReason) {
        let discarded = self.discard_output();
        self.open_gap(seq, reason, None);
        self.dropped_since_gap = discarded;
        if matches!(reason, GapReason::SessionGone) {
            self.needs_snapshot = false;
        }
    }

    fn open_gap(&mut self, seq: u64, reason: GapReason, dropped_bytes: Option<u64>) {
        self.in_gap = true;
        self.needs_snapshot = true;
        self.enqueue(
            seq,
            Frame::DataGap {
                reason,
                dropped_bytes,
            },
        );
    }

    /// Drop every `output` frame still pending, keeping control frames and
    /// a snapshot in flight in order: a snapshot is the recovery and its
    /// `snapshot_start{chunks}` promise must hold. Returns the output bytes
    /// discarded.
    fn discard_output(&mut self) -> u64 {
        let mut discarded = 0;
        self.pending.retain(|s| {
            if let Frame::Output(data) = &s.frame {
                discarded += data.len() as u64;
                false
            } else {
                true
            }
        });
        self.pending_bytes -= discarded;
        discarded
    }

    /// The snapshot taken at feed offset `seq`: closes the gap and resumes
    /// output. Chunked; not subject to the pending cap, because it is the
    /// recovery, but still paced by credit.
    pub fn push_snapshot(&mut self, seq: u64, cols: u16, rows: u16, epoch: u64, repaint: &[u8]) {
        let chunks: Vec<&[u8]> = repaint.chunks(self.cfg.chunk_bytes).collect();
        self.enqueue(
            seq,
            Frame::SnapshotStart {
                cols,
                rows,
                epoch,
                chunks: u32::try_from(chunks.len()).unwrap_or(u32::MAX),
            },
        );
        for chunk in chunks {
            self.enqueue(seq, Frame::SnapshotChunk(chunk.to_vec()));
        }
        self.enqueue(seq, Frame::SnapshotEnd);
        self.in_gap = false;
        self.needs_snapshot = false;
        self.dropped_since_gap = 0;
    }

    /// A control frame: `resize`, `floor`, `presence` or `closed`. Never
    /// discarded and never waits for credit; a new `resize`, `floor` or
    /// `presence` replaces any unsent one of the same kind.
    pub fn push_control(&mut self, seq: u64, frame: Frame) {
        debug_assert_eq!(frame.payload_len(), 0, "control frames carry no payload");
        self.enqueue(seq, frame);
    }

    fn enqueue(&mut self, seq: u64, frame: Frame) {
        if let Some(key) = frame.coalesce_key() {
            self.pending.retain(|s| s.frame.coalesce_key() != Some(key));
        }
        self.pending_bytes += frame.payload_len();
        self.pending.push_back(Sequenced { seq, frame });
    }

    /// Frames the writer may send now, in order, at most `max_frames` of
    /// them: control frames always, a payload frame only while it fits the
    /// credit. Stops at the first payload frame that does not fit, so order
    /// is preserved. A taken frame is counted as sent, so take no more than
    /// the outbound channel will accept.
    pub fn take_ready(&mut self, max_frames: usize) -> Vec<Sequenced> {
        let mut out = Vec::new();
        while out.len() < max_frames {
            let Some(front) = self.pending.front() else {
                break;
            };
            let len = front.frame.payload_len();
            if len > self.credit() {
                break;
            }
            let s = self.pending.pop_front().expect("front exists");
            self.pending_bytes -= len;
            self.sent_bytes += len;
            out.push(s);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: usize = 1024;

    fn small() -> ViewerConfig {
        ViewerConfig {
            window_bytes: 8 * KIB as u64,
            chunk_bytes: 2 * KIB,
            max_pending_bytes: 8 * KIB as u64,
        }
    }

    fn attached(cfg: ViewerConfig) -> ViewerQueue {
        let mut q = ViewerQueue::new(cfg);
        assert!(q.needs_snapshot(), "a new viewer owes a snapshot");
        q.push_snapshot(0, 40, 20, 1, b"\x1b[H\x1b[2Jhello");
        assert!(!q.needs_snapshot());
        q
    }

    fn payload(frames: &[Sequenced]) -> Vec<u8> {
        frames
            .iter()
            .filter_map(|s| match &s.frame {
                Frame::Output(d) | Frame::SnapshotChunk(d) => Some(d.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect()
    }

    fn sent_payload(frames: &[Sequenced]) -> u64 {
        frames.iter().map(|s| s.frame.payload_len()).sum()
    }

    #[test]
    fn a_fast_viewer_gets_every_byte_in_order_and_chunked() {
        let mut q = attached(small());
        let first = q.take_ready(usize::MAX);
        assert!(matches!(
            first.as_slice(),
            [
                Sequenced {
                    seq: 0,
                    frame: Frame::SnapshotStart {
                        cols: 40,
                        rows: 20,
                        epoch: 1,
                        chunks: 1
                    }
                },
                Sequenced {
                    seq: 0,
                    frame: Frame::SnapshotChunk(_)
                },
                Sequenced {
                    seq: 0,
                    frame: Frame::SnapshotEnd
                },
            ]
        ));
        let snap = sent_payload(&first);
        q.ack(snap);
        let mut all = Vec::new();
        let mut want = Vec::new();
        let mut seq = 0u64;
        for i in 0..50u8 {
            let bytes = vec![i; 3 * KIB];
            q.push_output(seq, &bytes);
            seq += bytes.len() as u64;
            want.extend_from_slice(&bytes);
            let ready = q.take_ready(usize::MAX);
            // Every frame respects the chunk size and carries the offset
            // of its own first byte.
            let mut expect = seq - bytes.len() as u64;
            for s in &ready {
                assert!(s.frame.payload_len() <= 2 * KIB as u64);
                assert_eq!(s.seq, expect);
                expect += s.frame.payload_len();
            }
            all.extend(ready);
            // A fast viewer acks everything it was sent.
            q.ack(snap + sent_payload(&all));
        }
        assert_eq!(payload(&all), want, "every byte, in order");
        assert!(!q.in_gap());
        assert_eq!(q.pending_bytes(), 0);
        assert_eq!(q.in_flight(), 0);
    }

    #[test]
    fn a_slow_viewer_is_cut_loose_with_dropped_then_gets_a_snapshot() {
        let cfg = small();
        let mut q = attached(cfg);
        let opening = q.take_ready(usize::MAX);
        q.ack(sent_payload(&opening));
        // Never acks again: 8 KiB goes out on credit, 8 KiB queues, the
        // next byte trips the cap.
        let mut sent = Vec::new();
        let mut seq = 0u64;
        while !q.in_gap() {
            q.push_output(seq, &[b'x'; KIB]);
            seq += KIB as u64;
            assert!(
                q.pending_bytes() <= cfg.max_pending_bytes,
                "pending never passes the cap"
            );
            sent.extend(q.take_ready(usize::MAX));
        }
        assert_eq!(seq, 17 * KIB as u64, "the 17th KiB opened the gap");
        assert_eq!(
            sent_payload(&sent),
            8 * KIB as u64,
            "exactly one window went out"
        );
        assert_eq!(q.in_flight(), 8 * KIB as u64);
        assert_eq!(q.pending_bytes(), 0, "the backlog was discarded");
        assert_eq!(
            sent.last(),
            Some(&Sequenced {
                seq: 16 * KIB as u64,
                frame: Frame::DataGap {
                    reason: GapReason::Dropped,
                    dropped_bytes: Some(9 * KIB as u64),
                }
            }),
            "the gap frame passes without credit and counts the 8 KiB backlog plus the 1 KiB that tripped it"
        );
        assert!(q.needs_snapshot());
        // Output during the gap is discarded and counted.
        q.push_output(seq, &[b'y'; 5 * KIB]);
        assert_eq!(q.pending_frames(), 0);
        assert_eq!(q.dropped_since_gap(), 14 * KIB as u64);
        // The daemon snapshots at the current offset; the viewer resumes.
        q.push_snapshot(seq + 5 * KIB as u64, 40, 20, 1, &[b's'; 3 * KIB]);
        assert!(!q.needs_snapshot());
        assert!(!q.in_gap());
        let start = q.take_ready(usize::MAX);
        assert!(
            matches!(
                start.as_slice(),
                [Sequenced {
                    frame: Frame::SnapshotStart { chunks: 2, .. },
                    ..
                }]
            ),
            "no credit yet: only the payload-free start frame goes, the chunks wait"
        );
        assert_eq!(start[0].seq, seq + 5 * KIB as u64);
        q.ack(sent_payload(&opening) + 8 * KIB as u64);
        let snap = q.take_ready(usize::MAX);
        assert_eq!(snap.len(), 3, "two chunks, end");
        assert_eq!(payload(&snap), vec![b's'; 3 * KIB]);
        q.push_output(seq + 5 * KIB as u64, b"after");
        assert_eq!(payload(&q.take_ready(usize::MAX)), b"after");
    }

    #[test]
    fn credit_returned_mid_window_keeps_the_stream_flowing() {
        let mut q = attached(small());
        let opening = q.take_ready(usize::MAX);
        q.ack(sent_payload(&opening));
        q.push_output(0, &[b'a'; 8 * KIB]);
        let first = q.take_ready(usize::MAX);
        assert_eq!(sent_payload(&first), 8 * KIB as u64, "one window");
        assert_eq!(q.credit(), 0);
        q.push_output(8 * KIB as u64, &[b'a'; 4 * KIB]);
        assert!(q.take_ready(usize::MAX).is_empty());
        // Ack the first 2 KiB of the stream (after the snapshot's bytes).
        let snap = sent_payload(&opening);
        q.ack(snap + 2 * KIB as u64);
        assert_eq!(q.credit(), 2 * KIB as u64);
        let more = q.take_ready(usize::MAX);
        assert_eq!(
            sent_payload(&more),
            2 * KIB as u64,
            "exactly the credit returned"
        );
        assert_eq!(q.pending_bytes(), 2 * KIB as u64);
        // A retried, lower ack changes nothing; a bogus higher one is clamped.
        q.ack(snap + KIB as u64);
        assert_eq!(q.credit(), 0);
        q.ack(u64::MAX);
        assert_eq!(q.in_flight(), 0);
        assert_eq!(sent_payload(&q.take_ready(usize::MAX)), 2 * KIB as u64);
        assert!(!q.in_gap());
    }

    #[test]
    fn control_frames_bypass_credit_and_survive_a_gap() {
        let mut q = attached(small());
        let opening = q.take_ready(usize::MAX);
        q.ack(sent_payload(&opening));
        q.push_output(0, &[b'a'; 8 * KIB]);
        q.take_ready(usize::MAX);
        q.push_output(8 * KIB as u64, &[b'b'; 4 * KIB]);
        assert_eq!(q.credit(), 0);
        q.push_control(
            12 * KIB as u64,
            Frame::Floor {
                holder: None,
                floor_gen: 3,
            },
        );
        // The floor frame waits behind the 4 KiB of output that has no credit.
        assert!(q.take_ready(usize::MAX).is_empty(), "order is preserved");
        q.gap(12 * KIB as u64, GapReason::Paused);
        // The output was discarded, so the floor frame and the gap go now.
        assert_eq!(
            q.take_ready(usize::MAX),
            vec![
                Sequenced {
                    seq: 12 * KIB as u64,
                    frame: Frame::Floor {
                        holder: None,
                        floor_gen: 3
                    }
                },
                Sequenced {
                    seq: 12 * KIB as u64,
                    frame: Frame::DataGap {
                        reason: GapReason::Paused,
                        dropped_bytes: None
                    }
                },
            ]
        );
        assert!(q.needs_snapshot());
        assert_eq!(q.dropped_since_gap(), 4 * KIB as u64);
    }

    #[test]
    fn session_gone_owes_no_snapshot_and_closed_follows() {
        let mut q = attached(small());
        q.take_ready(usize::MAX);
        q.push_output(0, b"tail");
        q.gap(4, GapReason::SessionGone);
        assert!(!q.needs_snapshot());
        assert!(q.in_gap());
        q.push_control(
            4,
            Frame::Closed {
                reason: "session_gone".to_string(),
            },
        );
        let frames = q.take_ready(usize::MAX);
        assert!(matches!(
            frames.as_slice(),
            [
                Sequenced {
                    frame: Frame::DataGap {
                        reason: GapReason::SessionGone,
                        ..
                    },
                    ..
                },
                Sequenced {
                    frame: Frame::Closed { .. },
                    ..
                },
            ]
        ));
        q.push_output(9, b"late");
        assert_eq!(q.pending_frames(), 0, "nothing after closed");
    }

    #[test]
    fn a_feed_lost_gap_replays_from_the_new_epoch() {
        let mut q = attached(small());
        let opening = q.take_ready(usize::MAX);
        q.ack(sent_payload(&opening));
        q.push_output(100, b"old epoch");
        q.gap(100, GapReason::FeedLost);
        q.push_snapshot(0, 40, 20, 2, b"fresh");
        let frames = q.take_ready(usize::MAX);
        let kinds: Vec<&Frame> = frames.iter().map(|s| &s.frame).collect();
        assert!(matches!(
            kinds.as_slice(),
            [
                Frame::DataGap {
                    reason: GapReason::FeedLost,
                    ..
                },
                Frame::SnapshotStart {
                    epoch: 2,
                    chunks: 1,
                    ..
                },
                Frame::SnapshotChunk(_),
                Frame::SnapshotEnd,
            ]
        ));
        assert_eq!(frames[1].seq, 0, "seq restarts with the epoch");
    }

    #[test]
    fn defaults_mirror_the_wire_constants() {
        let cfg = ViewerConfig::default();
        assert_eq!(cfg.window_bytes, 2 * 1024 * 1024);
        assert_eq!(cfg.chunk_bytes, 48 * 1024);
        let mut q = ViewerQueue::default();
        q.push_snapshot(0, 1, 1, 1, &[0u8; 100 * 1024]);
        let frames = q.take_ready(usize::MAX);
        assert_eq!(frames.len(), 5, "start, three 48 KiB-or-less chunks, end");
        assert!(matches!(
            frames[0].frame,
            Frame::SnapshotStart { chunks: 3, .. }
        ));
        assert_eq!(q.in_flight(), 100 * 1024);
    }

    #[test]
    fn each_output_chunk_carries_the_offset_of_its_first_byte() {
        let mut q = attached(small());
        let opening = q.take_ready(usize::MAX);
        q.ack(sent_payload(&opening));
        q.push_output(1000, &[b'z'; 6000]);
        let seqs: Vec<u64> = q.take_ready(usize::MAX).iter().map(|s| s.seq).collect();
        assert_eq!(seqs, vec![1000, 1000 + 2048, 1000 + 4096]);
        // Snapshot frames all carry the offset the snapshot was taken at.
        q.ack(u64::MAX);
        q.gap(7000, GapReason::Paused);
        q.push_snapshot(7000, 40, 20, 1, &[b's'; 5000]);
        let frames = q.take_ready(usize::MAX);
        assert!(frames.iter().all(|s| s.seq == 7000), "{frames:?}");
        assert_eq!(frames.len(), 1 + 1 + 3 + 1, "gap, start, three chunks, end");
    }

    #[test]
    fn a_cap_trip_never_tears_a_snapshot_in_flight() {
        let mut q = attached(small());
        let opening = q.take_ready(usize::MAX);
        q.ack(sent_payload(&opening));
        // A 6 KiB snapshot: with 8 KiB credit only its start and chunks go
        // out once taken; leave it pending and flood output behind it.
        q.gap(0, GapReason::Paused);
        q.push_snapshot(0, 40, 20, 2, &[b's'; 6 * KIB]);
        let first = q.take_ready(3); // gap, start, one chunk
        assert!(matches!(
            first.as_slice(),
            [
                Sequenced {
                    frame: Frame::DataGap { .. },
                    ..
                },
                Sequenced {
                    frame: Frame::SnapshotStart { chunks: 3, .. },
                    ..
                },
                Sequenced {
                    frame: Frame::SnapshotChunk(_),
                    ..
                },
            ]
        ));
        let mut seq = 0u64;
        while !q.in_gap() {
            q.push_output(seq, &[b'x'; KIB]);
            seq += KIB as u64;
        }
        // The two remaining chunks and the end are still there, in order,
        // ahead of the gap; the output behind them was dropped.
        let rest = q.take_ready(usize::MAX);
        let kinds: Vec<String> = rest
            .iter()
            .map(|s| match &s.frame {
                Frame::SnapshotChunk(_) => "chunk".to_string(),
                Frame::SnapshotEnd => "end".to_string(),
                Frame::DataGap { reason, .. } => format!("gap:{reason:?}"),
                Frame::Output(_) => "output".to_string(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(kinds, vec!["chunk", "chunk", "end", "gap:Dropped"]);
        assert!(q.needs_snapshot());
    }

    #[test]
    fn unsent_control_frames_coalesce_so_a_stalled_viewer_stays_bounded() {
        let mut q = attached(small());
        let opening = q.take_ready(usize::MAX);
        q.ack(sent_payload(&opening));
        // Fill the window and stall: nothing acks any more.
        q.push_output(0, &[b'a'; 8 * KIB]);
        q.take_ready(usize::MAX);
        q.push_output(8 * KIB as u64, &[b'b'; KIB]);
        for gen in 0..100_001u64 {
            q.push_control(
                9 * KIB as u64,
                Frame::Floor {
                    holder: None,
                    floor_gen: gen,
                },
            );
            q.push_control(9 * KIB as u64, Frame::Presence { native_clients: 1 });
            q.push_control(9 * KIB as u64, Frame::Resize { cols: 40, rows: 20 });
        }
        assert_eq!(
            q.pending_frames(),
            4,
            "one output plus one of each control kind"
        );
        // Repeated feed gaps do not pile up either.
        for _ in 0..10 {
            q.gap(9 * KIB as u64, GapReason::FeedLost);
        }
        assert_eq!(
            q.pending_frames(),
            4,
            "the output went with the first gap; one gap remains"
        );
        // Once released, the latest state is what goes out.
        q.ack(u64::MAX);
        let frames = q.take_ready(usize::MAX);
        assert!(matches!(
            frames.iter().find(|s| matches!(s.frame, Frame::Floor { .. })),
            Some(Sequenced {
                frame: Frame::Floor {
                    floor_gen: 100_000,
                    ..
                },
                ..
            })
        ));
        assert_eq!(frames.len(), 4);
    }

    #[test]
    fn take_ready_takes_no_more_than_the_writer_asked_for() {
        let mut q = attached(small());
        let opening = q.take_ready(usize::MAX);
        q.ack(sent_payload(&opening));
        q.push_output(0, &[b'a'; 6 * KIB]);
        q.push_control(6 * KIB as u64, Frame::Presence { native_clients: 2 });
        let two = q.take_ready(2);
        assert_eq!(two.len(), 2);
        assert_eq!(
            q.pending_frames(),
            2,
            "one chunk and the presence frame remain"
        );
        assert_eq!(q.sent_bytes(), sent_payload(&opening) + 4 * KIB as u64);
        assert!(q.take_ready(0).is_empty());
        assert_eq!(q.take_ready(usize::MAX).len(), 2);
    }

    #[test]
    fn the_cap_counts_only_the_backlog_beyond_the_credit() {
        let cfg = small();
        let mut q = attached(cfg);
        let opening = q.take_ready(usize::MAX);
        q.ack(sent_payload(&opening));
        // A fast viewer with a full window of credit is pushed 12 KiB
        // between two takes: 8 KiB can go now, 4 KiB is backlog, under
        // the 8 KiB cap, so it is not cut loose.
        q.push_output(0, &[b'a'; 12 * KIB]);
        assert!(!q.in_gap());
        assert_eq!(q.backlog(), 4 * KIB as u64);
        assert_eq!(sent_payload(&q.take_ready(usize::MAX)), 8 * KIB as u64);
        // With the window full, the cap applies to everything pending.
        q.push_output(12 * KIB as u64, &[b'b'; 4 * KIB]);
        assert!(!q.in_gap());
        q.push_output(16 * KIB as u64, &[b'b'; KIB]);
        assert!(
            q.in_gap(),
            "8 KiB pending beyond zero credit, plus one more"
        );
    }

    #[test]
    fn a_chunk_larger_than_the_window_is_clamped_to_it() {
        let mut q = ViewerQueue::new(ViewerConfig {
            window_bytes: 8 * KIB as u64,
            chunk_bytes: 100 * KIB,
            max_pending_bytes: 64 * KIB as u64,
        });
        q.push_snapshot(0, 40, 20, 1, &[b's'; 20 * KIB]);
        let frames = q.take_ready(usize::MAX);
        assert!(matches!(
            frames[0].frame,
            Frame::SnapshotStart { chunks: 3, .. }
        ));
        assert_eq!(
            sent_payload(&frames),
            8 * KIB as u64,
            "the first chunk fits exactly"
        );
        q.ack(8 * KIB as u64);
        assert_eq!(sent_payload(&q.take_ready(usize::MAX)), 8 * KIB as u64);
        q.ack(16 * KIB as u64);
        let last = q.take_ready(usize::MAX);
        assert_eq!(sent_payload(&last), 4 * KIB as u64);
        assert!(matches!(last.last().unwrap().frame, Frame::SnapshotEnd));
    }
}
