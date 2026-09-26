//! One multiplexed Noise session to one host.
//!
//! ```text
//! connect ──▶ ws://…/peer ──▶ Noise IK msg 1 ──▶ msg 2 ──▶ transport
//!                                                            │
//!             request(method, params) ──Rpc frames──▶        │ ◀── Rpc reply (pending map)
//!             heartbeat: Ping every 15 s, Skip missed ticks  │ ◀── Pong (resets the count)
//!             next_event()  ◀── notification queue  ◀────────┘ ◀── Rpc notification
//! ```
//!
//! The frame layouts come from `ainb-hangar-noise` (PR-0). This module adds
//! what a phone needs above them: request-id correlation, a bounded
//! notification queue that reports its own overflow, the heartbeat rule
//! (dead after [`MISSED_PONGS_DEAD`] unanswered pings) and the close-code
//! mapping (4401 re-hello, 4403 re-pair).

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ainb_hangar_noise::{
    FrameHeader, HEADER_LEN, MAX_NOISE_MESSAGE, MISSED_PONGS_DEAD, NOISE_PATTERN, Opcode,
    PING_INTERVAL_SECS, prologue,
};
use ainb_hangar_proto::hosts::{CarrierKind, HostId};
use ainb_hangar_proto::{RpcId, RpcRequest, RpcResponse, jsonrpc_version, peer_close};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::records::{WireError, error_reason};

/// The AEAD tag Noise appends to every transport message.
const NOISE_TAG_LEN: usize = 16;
/// The most payload one frame carries: one Noise message minus tag and header.
pub const MAX_FRAME_PAYLOAD: usize = MAX_NOISE_MESSAGE - NOISE_TAG_LEN - HEADER_LEN;
/// Notifications queued ahead of the app before the queue reports overflow.
const EVENT_QUEUE: usize = 1024;
/// The default request timeout.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(10);
/// The heartbeat period, from the frozen wire.
pub const HEARTBEAT: Duration = Duration::from_secs(PING_INTERVAL_SECS);
/// The shortest reconnect delay.
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
/// The longest reconnect delay.
pub const BACKOFF_MAX: Duration = Duration::from_mins(1);

/// What a session dials.
#[derive(Debug, Clone)]
pub struct ConnectConfig {
    /// `ws://host:port/peer`.
    pub url: String,
    /// The carrier the URL is on; mixed into the prologue.
    pub carrier: CarrierKind,
    /// The host the offer named; mixed into the prologue.
    pub host_id: HostId,
    /// The host's Noise static public key, pinned at pairing.
    pub host_static_pubkey: [u8; 32],
    /// This device's Noise static private key.
    pub device_private_key: Vec<u8>,
    /// The ping period; `None` disables the heartbeat (tests only).
    pub heartbeat: Option<Duration>,
    /// How long a request waits for its reply.
    pub rpc_timeout: Duration,
}

impl ConnectConfig {
    /// A config with the frozen heartbeat and the default timeout.
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        carrier: CarrierKind,
        host_id: HostId,
        host_static_pubkey: [u8; 32],
        device_private_key: Vec<u8>,
    ) -> Self {
        Self {
            url: url.into(),
            carrier,
            host_id,
            host_static_pubkey,
            device_private_key,
            heartbeat: Some(HEARTBEAT),
            rpc_timeout: RPC_TIMEOUT,
        }
    }
}

/// A notification the daemon pushed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// The method.
    pub method: String,
    /// The params.
    pub params: serde_json::Value,
}

/// What [`Session::next_event`] yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// A pushed notification.
    Notification(Notification),
    /// The queue overflowed and this many notifications were dropped.
    Lagged(u64),
    /// The session is closed; every later call yields this again.
    Closed {
        /// The WebSocket close code, when the peer sent one.
        code: Option<u16>,
        /// Why.
        reason: String,
    },
}

/// Why the session closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closed {
    /// The WebSocket close code, when the peer sent one.
    pub code: Option<u16>,
    /// Why.
    pub reason: String,
}

impl Closed {
    /// The error a call on this closed session gets.
    #[must_use]
    pub fn error(&self) -> WireError {
        match self.code {
            Some(peer_close::UNAUTHENTICATED) => WireError::Unauthenticated,
            Some(peer_close::REVOKED) => WireError::Revoked,
            code => WireError::Closed {
                code,
                reason: self.reason.clone(),
            },
        }
    }
}

/// Counters for the connection log.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SessionStats {
    /// How long the WebSocket took to open.
    pub ws_connect_ms: f64,
    /// How long the IK handshake took.
    pub noise_handshake_ms: f64,
    /// Pings sent.
    pub pings_sent: u64,
    /// Pongs received.
    pub pongs_received: u64,
    /// The last measured round trip.
    pub last_rtt_ms: f64,
    /// Notifications dropped by queue overflow, cumulative.
    pub events_dropped: u64,
    /// Whether the session is closed.
    pub closed: bool,
    /// The close code, when the peer sent one.
    pub close_code: Option<u16>,
    /// Why it closed, when it did.
    pub close_reason: Option<String>,
}

/// The heartbeat rule, clock-free so it is testable by tick.
///
/// The first tick fires at connect (tokio's `interval` ticks immediately), so
/// two consecutive unanswered pings mean dead at `2 * period`: 30 s on the
/// frozen wire, inside the host's 35 s silence close (C7).
#[derive(Debug, Default)]
pub struct Heartbeat {
    unanswered: u32,
}

/// What a heartbeat tick decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Beat {
    /// Send a ping.
    Ping,
    /// The peer is dead.
    Dead,
}

impl Heartbeat {
    /// One tick.
    pub fn tick(&mut self) -> Beat {
        if self.unanswered >= MISSED_PONGS_DEAD {
            Beat::Dead
        } else {
            self.unanswered += 1;
            Beat::Ping
        }
    }

    /// A pong arrived.
    pub fn pong(&mut self) {
        self.unanswered = 0;
    }
}

/// The reconnect delay before `attempt` (0-based): doubling from 1 s, capped
/// at 60 s, with full jitter down to half the step so a fleet of phones
/// waking together does not dial together.
#[must_use]
pub fn backoff_delay(attempt: u32) -> Duration {
    use rand::Rng as _;
    let step = BACKOFF_MIN.saturating_mul(1u32 << attempt.min(6)).min(BACKOFF_MAX);
    let low = (step / 2).max(BACKOFF_MIN);
    let millis = rand::thread_rng().gen_range(low.as_millis()..=step.as_millis());
    Duration::from_millis(u64::try_from(millis).unwrap_or(u64::MAX))
}

/// One live session.
pub struct Session {
    transport: Mutex<snow::TransportState>,
    /// Ciphertext to the socket; an empty buffer is the close signal.
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Mutex<HashMap<i64, oneshot::Sender<RpcResponse>>>,
    next_id: AtomicI64,
    events: tokio::sync::Mutex<mpsc::Receiver<Notification>>,
    events_dropped: AtomicU64,
    lag_unreported: AtomicU64,
    closed: Mutex<Option<Closed>>,
    heartbeat: Mutex<Heartbeat>,
    started: Instant,
    ws_connect_ms: f64,
    noise_handshake_ms: f64,
    pings_sent: AtomicU64,
    pongs_received: AtomicU64,
    last_rtt_us: AtomicU64,
    ping_seq: AtomicU64,
    rpc_timeout: Duration,
}

fn lsp_encode(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

fn lsp_decode(buf: &[u8]) -> Result<&[u8], String> {
    let sep = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no Content-Length terminator")?;
    let header = std::str::from_utf8(&buf[..sep]).map_err(|e| e.to_string())?;
    let len: usize = header
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length:"))
        .ok_or("no Content-Length")?
        .trim()
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    let body = &buf[sep + 4..];
    if body.len() != len {
        return Err(format!("Content-Length {len} but body {}", body.len()));
    }
    Ok(body)
}

/// Encode one frame: header then payload.
fn frame(header: FrameHeader, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&header.encode());
    out.extend_from_slice(payload);
    out
}

/// Split one logical Rpc message into frames that each fit a Noise message.
fn rpc_frames(message: &[u8]) -> Vec<Vec<u8>> {
    let chunks: Vec<&[u8]> = if message.is_empty() {
        vec![&[][..]]
    } else {
        message.chunks(MAX_FRAME_PAYLOAD).collect()
    };
    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            frame(
                FrameHeader {
                    opcode: Opcode::Rpc,
                    fin: i == last,
                    stream_id: 0,
                    seq: i as u64,
                },
                chunk,
            )
        })
        .collect()
}

/// Reassembles Rpc fragments until FIN.
#[derive(Default)]
struct Reassembler {
    buf: Vec<u8>,
}

impl Reassembler {
    fn push(&mut self, fin: bool, payload: &[u8]) -> Option<Vec<u8>> {
        self.buf.extend_from_slice(payload);
        fin.then(|| std::mem::take(&mut self.buf))
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn handshake_error(e: impl std::fmt::Display) -> WireError {
    WireError::Handshake {
        message: e.to_string(),
    }
}

impl Session {
    /// Dial, handshake, and start the reader, writer and heartbeat tasks.
    ///
    /// Must run inside a tokio runtime. A peer that closes at message 1 or
    /// answers with a static key other than the pinned one is
    /// [`WireError::PeerChanged`].
    #[allow(clippy::too_many_lines)]
    pub async fn connect(config: ConnectConfig) -> Result<Arc<Self>, WireError> {
        let t0 = Instant::now();
        let (ws, _) = tokio_tungstenite::connect_async(config.url.as_str()).await.map_err(|e| {
            WireError::Connect {
                message: e.to_string(),
            }
        })?;
        let ws_connect_ms = ms(t0.elapsed());
        let (mut sink, mut stream) = ws.split();

        let t1 = Instant::now();
        let prologue_bytes = prologue(config.carrier, &config.host_id).map_err(handshake_error)?;
        let mut hs = snow::Builder::new(NOISE_PATTERN.parse().map_err(handshake_error)?)
            .local_private_key(&config.device_private_key)
            .remote_public_key(&config.host_static_pubkey)
            .prologue(&prologue_bytes)
            .build_initiator()
            .map_err(handshake_error)?;
        let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
        let n = hs.write_message(&[], &mut buf).map_err(handshake_error)?;
        sink.send(Message::Binary(buf[..n].to_vec())).await.map_err(handshake_error)?;
        let reply = loop {
            match stream.next().await {
                Some(Ok(Message::Binary(b))) => break b,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                // Closed before message 2: the responder could not open
                // message 1, which is what a key or prologue mismatch does.
                Some(Ok(Message::Close(_))) | None => return Err(WireError::PeerChanged),
                Some(Ok(other)) => {
                    return Err(handshake_error(format!("unexpected {other:?}")));
                }
                Some(Err(e)) => return Err(handshake_error(e)),
            }
        };
        // Message 2 authenticates the responder's static key; a different one
        // fails here.
        hs.read_message(&reply, &mut buf).map_err(|_| WireError::PeerChanged)?;
        let transport = hs.into_transport_mode().map_err(handshake_error)?;
        let noise_handshake_ms = ms(t1.elapsed());

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (events_tx, events_rx) = mpsc::channel::<Notification>(EVENT_QUEUE);
        let session = Arc::new(Self {
            transport: Mutex::new(transport),
            out: out_tx,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            events: tokio::sync::Mutex::new(events_rx),
            events_dropped: AtomicU64::new(0),
            lag_unreported: AtomicU64::new(0),
            closed: Mutex::new(None),
            heartbeat: Mutex::new(Heartbeat::default()),
            started: Instant::now(),
            ws_connect_ms,
            noise_handshake_ms,
            pings_sent: AtomicU64::new(0),
            pongs_received: AtomicU64::new(0),
            last_rtt_us: AtomicU64::new(0),
            ping_seq: AtomicU64::new(0),
            rpc_timeout: config.rpc_timeout,
        });

        let writer = Arc::clone(&session);
        tokio::spawn(async move {
            while let Some(bytes) = out_rx.recv().await {
                if bytes.is_empty() {
                    break;
                }
                if let Err(e) = sink.send(Message::Binary(bytes)).await {
                    writer.mark_closed(None, format!("write: {e}"));
                    break;
                }
            }
            let _ = sink.send(Message::Close(None)).await;
            let _ = sink.close().await;
        });

        let reader = Arc::clone(&session);
        tokio::spawn(async move {
            let mut reasm = Reassembler::default();
            let (code, reason) = loop {
                match stream.next().await {
                    Some(Ok(Message::Binary(b))) => {
                        if let Err(e) = reader.on_ciphertext(&b, &mut reasm, &events_tx) {
                            break (None, format!("read: {e}"));
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let code = frame.as_ref().map(|f| u16::from(f.code));
                        let reason = frame
                            .map(|f| f.reason.into_owned())
                            .filter(|r| !r.is_empty())
                            .unwrap_or_else(|| "peer closed".to_owned());
                        break (code, reason);
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => break (None, format!("ws: {e}")),
                    None => break (None, "eof".to_owned()),
                }
            };
            reader.mark_closed(code, reason);
            // `events_tx` drops here, which is what wakes `next_event` with
            // the close.
        });

        if let Some(period) = config.heartbeat {
            let weak = Arc::downgrade(&session);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(period);
                // One ping after a suspend, never a burst of catch-up ticks.
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tick.tick().await;
                    let Some(s) = weak.upgrade() else { break };
                    if s.is_closed() {
                        break;
                    }
                    let beat = s.heartbeat.lock().unwrap().tick();
                    match beat {
                        Beat::Dead => {
                            s.mark_closed(
                                None,
                                format!("heartbeat: {MISSED_PONGS_DEAD} pings unanswered"),
                            );
                            let _ = s.out.send(Vec::new());
                            break;
                        }
                        Beat::Ping => {
                            if s.send_ping().is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }

        Ok(session)
    }

    fn send_ping(&self) -> Result<(), WireError> {
        let seq = self.ping_seq.fetch_add(1, Ordering::SeqCst);
        let elapsed_us = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.send_frame(
            FrameHeader {
                opcode: Opcode::Ping,
                fin: true,
                stream_id: 0,
                seq,
            },
            &elapsed_us.to_le_bytes(),
        )?;
        self.pings_sent.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn mark_closed(&self, code: Option<u16>, reason: String) {
        let mut closed = self.closed.lock().unwrap();
        if closed.is_none() {
            *closed = Some(Closed { code, reason });
        }
        drop(closed);
        self.pending.lock().unwrap().clear();
    }

    /// Whether the session is closed.
    pub fn is_closed(&self) -> bool {
        self.closed.lock().unwrap().is_some()
    }

    /// Why the session closed, once it has.
    pub fn closed(&self) -> Option<Closed> {
        self.closed.lock().unwrap().clone()
    }

    fn closed_error(&self) -> WireError {
        self.closed().map_or_else(
            || WireError::Closed {
                code: None,
                reason: "writer gone".to_owned(),
            },
            |c| c.error(),
        )
    }

    fn send_frame(&self, header: FrameHeader, payload: &[u8]) -> Result<(), WireError> {
        if self.is_closed() {
            return Err(self.closed_error());
        }
        let plain = frame(header, payload);
        let mut cipher = vec![0u8; plain.len() + NOISE_TAG_LEN];
        // Encrypt and enqueue under one lock so nonce order equals wire order.
        let mut transport = self.transport.lock().unwrap();
        let n = transport.write_message(&plain, &mut cipher).map_err(WireError::protocol)?;
        cipher.truncate(n);
        self.out.send(cipher).map_err(|_| self.closed_error())
    }

    fn on_ciphertext(
        &self,
        cipher: &[u8],
        reasm: &mut Reassembler,
        events: &mpsc::Sender<Notification>,
    ) -> Result<(), String> {
        let mut plain = vec![0u8; cipher.len()];
        let n = self
            .transport
            .lock()
            .unwrap()
            .read_message(cipher, &mut plain)
            .map_err(|e| e.to_string())?;
        let plain = &plain[..n];
        let header = match FrameHeader::decode(plain) {
            Ok(h) => h,
            // A reserved or newer opcode: drop and count, never fail.
            Err(ainb_hangar_noise::frame::HeaderError::UnknownOpcode(_)) => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        let payload = &plain[HEADER_LEN..];
        match header.opcode {
            Opcode::Pong => {
                if let Some(sent) = payload.get(..8) {
                    let sent = u64::from_le_bytes(sent.try_into().unwrap_or([0; 8]));
                    let now = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
                    self.last_rtt_us.store(now.saturating_sub(sent), Ordering::SeqCst);
                }
                self.pongs_received.fetch_add(1, Ordering::SeqCst);
                self.heartbeat.lock().unwrap().pong();
            }
            Opcode::Ping => {
                let _ = self.send_frame(
                    FrameHeader {
                        opcode: Opcode::Pong,
                        ..header
                    },
                    payload,
                );
            }
            Opcode::StreamEnd => return Err("peer sent StreamEnd".to_owned()),
            Opcode::Rpc => {
                if let Some(message) = reasm.push(header.fin, payload) {
                    let body = lsp_decode(&message)?;
                    let value: serde_json::Value =
                        serde_json::from_slice(body).map_err(|e| e.to_string())?;
                    self.on_rpc_message(value, events)?;
                }
            }
            // Every other opcode is reserved for a binary lane this build
            // never negotiates.
            _ => {}
        }
        Ok(())
    }

    fn on_rpc_message(
        &self,
        value: serde_json::Value,
        events: &mpsc::Sender<Notification>,
    ) -> Result<(), String> {
        let has_id = value.get("id").is_some_and(|id| !id.is_null());
        if has_id {
            let response: RpcResponse = serde_json::from_value(value).map_err(|e| e.to_string())?;
            if let RpcId::Number(id) = response.id {
                if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(response);
                }
            }
            return Ok(());
        }
        let Some(method) = value.get("method").and_then(serde_json::Value::as_str) else {
            return Err("rpc message with neither id nor method".to_owned());
        };
        let notification = Notification {
            method: method.to_owned(),
            params: value.get("params").cloned().unwrap_or(serde_json::Value::Null),
        };
        if events.try_send(notification).is_err() {
            self.events_dropped.fetch_add(1, Ordering::SeqCst);
            self.lag_unreported.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Send a request and wait for its reply, or the timeout.
    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, WireError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = RpcRequest {
            jsonrpc: jsonrpc_version(),
            id: RpcId::Number(id),
            method: method.to_owned(),
            params,
        };
        let body = serde_json::to_vec(&request).map_err(WireError::protocol)?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        for fragment in rpc_frames(&lsp_encode(&body)) {
            let header = FrameHeader::decode(&fragment).map_err(WireError::protocol)?;
            self.send_frame(header, &fragment[HEADER_LEN..])?;
        }
        let response = match tokio::time::timeout(self.rpc_timeout, rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return Err(self.closed_error()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                return Err(WireError::Timeout {
                    method: method.to_owned(),
                });
            }
        };
        match (response.result, response.error) {
            (_, Some(e)) => Err(WireError::Rpc {
                code: e.code,
                message: e.message,
                reason: error_reason(e.data.as_ref()),
            }),
            (Some(result), None) => Ok(result),
            (None, None) => Ok(serde_json::Value::Null),
        }
    }

    /// Send a typed request and decode its typed reply.
    pub async fn call<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R, WireError> {
        let params = serde_json::to_value(params).map_err(WireError::protocol)?;
        let value = self.request(method, params).await?;
        serde_json::from_value(value).map_err(WireError::protocol)
    }

    /// The next pushed notification, an overflow report, or the close.
    pub async fn next_event(&self) -> SessionEvent {
        let lagged = self.lag_unreported.swap(0, Ordering::SeqCst);
        if lagged > 0 {
            return SessionEvent::Lagged(lagged);
        }
        let mut events = self.events.lock().await;
        if let Some(n) = events.recv().await {
            SessionEvent::Notification(n)
        } else {
            let closed = self.closed().unwrap_or(Closed {
                code: None,
                reason: "closed".to_owned(),
            });
            SessionEvent::Closed {
                code: closed.code,
                reason: closed.reason,
            }
        }
    }

    /// Close: a `StreamEnd`, then the WebSocket close frame.
    pub fn close(&self) {
        let _ = self.send_frame(
            FrameHeader {
                opcode: Opcode::StreamEnd,
                fin: true,
                stream_id: 0,
                seq: 0,
            },
            &[],
        );
        let _ = self.out.send(Vec::new());
        self.mark_closed(None, "closed by client".to_owned());
    }

    /// The counters.
    #[allow(clippy::cast_precision_loss)]
    pub fn stats(&self) -> SessionStats {
        let closed = self.closed();
        SessionStats {
            ws_connect_ms: self.ws_connect_ms,
            noise_handshake_ms: self.noise_handshake_ms,
            pings_sent: self.pings_sent.load(Ordering::SeqCst),
            pongs_received: self.pongs_received.load(Ordering::SeqCst),
            last_rtt_ms: self.last_rtt_us.load(Ordering::SeqCst) as f64 / 1000.0,
            events_dropped: self.events_dropped.load(Ordering::SeqCst),
            closed: closed.is_some(),
            close_code: closed.as_ref().and_then(|c| c.code),
            close_reason: closed.map(|c| c.reason),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.out.send(Vec::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_unanswered_pings_are_dead_and_a_pong_resets() {
        let mut hb = Heartbeat::default();
        assert_eq!(hb.tick(), Beat::Ping);
        assert_eq!(hb.tick(), Beat::Ping);
        assert_eq!(hb.tick(), Beat::Dead);
        hb.pong();
        assert_eq!(hb.tick(), Beat::Ping);
        hb.pong();
        assert_eq!(hb.tick(), Beat::Ping);
        assert_eq!(hb.tick(), Beat::Ping);
        assert_eq!(hb.tick(), Beat::Dead);
    }

    #[test]
    fn backoff_stays_between_one_and_sixty_seconds_and_grows() {
        for attempt in 0..20 {
            let d = backoff_delay(attempt);
            assert!(
                d >= BACKOFF_MIN && d <= BACKOFF_MAX,
                "attempt {attempt}: {d:?}"
            );
        }
        assert!(backoff_delay(0) <= Duration::from_secs(1));
        assert!(backoff_delay(10) >= Duration::from_secs(30));
    }

    #[test]
    fn rpc_messages_fragment_and_reassemble() {
        let big = vec![7u8; MAX_FRAME_PAYLOAD * 2 + 5];
        let frames = rpc_frames(&big);
        assert_eq!(frames.len(), 3);
        let mut reasm = Reassembler::default();
        let mut out = None;
        for f in &frames {
            assert!(f.len() <= MAX_NOISE_MESSAGE - NOISE_TAG_LEN);
            let header = FrameHeader::decode(f).unwrap();
            out = reasm.push(header.fin, &f[HEADER_LEN..]);
        }
        assert_eq!(out.unwrap(), big);
        let body = br#"{"jsonrpc":"2.0"}"#;
        assert_eq!(lsp_decode(&lsp_encode(body)).unwrap(), body);
    }

    #[tokio::test(start_paused = true)]
    async fn a_skipping_interval_fires_once_after_a_long_gap() {
        let mut tick = tokio::time::interval(HEARTBEAT);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tick.tick().await;
        tokio::time::advance(Duration::from_mins(10)).await;
        let mut fired = 0;
        while tokio::time::timeout(Duration::from_millis(1), tick.tick()).await.is_ok() {
            fired += 1;
        }
        assert_eq!(fired, 1, "a 10 min gap must yield one ping, not forty");
    }
}
