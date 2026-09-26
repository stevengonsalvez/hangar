//! An in-process peer: the host end of the wire, standing in for the daemon's
//! peer listener (R1-06). It speaks the frozen wire byte for byte (WebSocket,
//! Noise IK responder over the PR-0 prologue, 16-byte frames, LSP-framed
//! JSON-RPC inside `Rpc`) and hands every request to a test-supplied handler,
//! so a test scripts the daemon's side of one conversation.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ainb_hangar_noise::{
    FrameHeader, HEADER_LEN, MAX_NOISE_MESSAGE, NOISE_PATTERN, Opcode, prologue,
};
use ainb_hangar_proto::hosts::{CarrierKind, HostId};
use ainb_hangar_proto::{RpcError, RpcRequest, RpcResponse};
use ainb_wire_mobile::custody::DeviceKey;
use ainb_wire_mobile::session::ConnectConfig;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

pub const HOST_ID: &str = "01K5A0000000000000000ABCDE";

/// What the handler answers.
pub enum Reply {
    Result(Value),
    Error {
        code: i32,
        message: String,
        data: Option<Value>,
    },
    /// Close the socket with this code instead of answering.
    Close(u16, String),
}

pub type Handler = Arc<dyn Fn(&str, Value) -> Reply + Send + Sync>;

#[derive(Clone)]
pub struct PeerOpts {
    pub answer_pings: bool,
    pub carrier: CarrierKind,
    pub host_id: HostId,
    /// Close with this code and reason as soon as the WebSocket opens,
    /// before any Noise message (a busy or draining host).
    pub refuse_before_handshake: Option<(u16, String)>,
    /// Accept the WebSocket and never answer (a black hole).
    pub hang: bool,
    /// After reading Noise message 1, drop the socket this way instead of
    /// answering: a clean end of stream, or a TCP reset.
    pub after_message_1: Option<DropKind>,
}

/// How a peer drops a socket after message 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropKind {
    /// FIN, no close frame.
    Eof,
    /// RST (linger zero), which the reader sees as a stream error.
    Reset,
}

impl Default for PeerOpts {
    fn default() -> Self {
        Self {
            answer_pings: true,
            carrier: CarrierKind::Lan,
            host_id: HostId::parse(HOST_ID).unwrap(),
            refuse_before_handshake: None,
            hang: false,
            after_message_1: None,
        }
    }
}

pub struct FakePeer {
    pub url: String,
    pub host_id: HostId,
    pub host_pubkey: [u8; 32],
    pub host_private: Vec<u8>,
    /// Push a notification to every live connection.
    pub notify: broadcast::Sender<(String, Value)>,
    /// Every request received, in order.
    pub received: Arc<Mutex<Vec<(String, Value)>>>,
    pub pings: Arc<AtomicU64>,
    pub handshakes: Arc<AtomicU64>,
}

impl FakePeer {
    pub fn received_methods(&self) -> Vec<String> {
        self.received.lock().unwrap().iter().map(|(m, _)| m.clone()).collect()
    }

    pub fn params_of(&self, method: &str) -> Vec<Value> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// A client config for `key` dialing this peer, heartbeat off.
    pub fn config(&self, key: &DeviceKey) -> ConnectConfig {
        let mut config = ConnectConfig::new(
            self.url.clone(),
            CarrierKind::Lan,
            self.host_id.clone(),
            self.host_pubkey,
            key_private(key),
        );
        config.heartbeat = None;
        config
    }
}

pub fn key_private(key: &DeviceKey) -> Vec<u8> {
    key.private().to_vec()
}

pub async fn spawn(handler: Handler, opts: PeerOpts) -> FakePeer {
    let keypair = snow::Builder::new(NOISE_PATTERN.parse().unwrap()).generate_keypair().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (notify, _) = broadcast::channel(4096);
    let peer = FakePeer {
        url: format!("ws://{addr}/peer"),
        host_id: opts.host_id.clone(),
        host_pubkey: keypair.public.clone().try_into().unwrap(),
        host_private: keypair.private.clone(),
        notify: notify.clone(),
        received: Arc::new(Mutex::new(Vec::new())),
        pings: Arc::new(AtomicU64::new(0)),
        handshakes: Arc::new(AtomicU64::new(0)),
    };
    let received = Arc::clone(&peer.received);
    let pings = Arc::clone(&peer.pings);
    let handshakes = Arc::clone(&peer.handshakes);
    let host_private = keypair.private;
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let handler = Arc::clone(&handler);
            let opts = opts.clone();
            let received = Arc::clone(&received);
            let pings = Arc::clone(&pings);
            let handshakes = Arc::clone(&handshakes);
            let notify = notify.subscribe();
            let host_private = host_private.clone();
            tokio::spawn(async move {
                let _ = serve(
                    tcp,
                    host_private,
                    opts,
                    handler,
                    received,
                    pings,
                    handshakes,
                    notify,
                )
                .await;
            });
        }
    });
    peer
}

fn lsp_encode(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

fn lsp_decode(buf: &[u8]) -> &[u8] {
    let sep = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    &buf[sep + 4..]
}

fn frames(opcode: Opcode, message: &[u8]) -> Vec<Vec<u8>> {
    let max = MAX_NOISE_MESSAGE - 16 - HEADER_LEN;
    let chunks: Vec<&[u8]> = if message.is_empty() {
        vec![&[][..]]
    } else {
        message.chunks(max).collect()
    };
    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let mut out = FrameHeader {
                opcode,
                fin: i == last,
                stream_id: 0,
                seq: i as u64,
            }
            .encode()
            .to_vec();
            out.extend_from_slice(c);
            out
        })
        .collect()
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn serve(
    tcp: tokio::net::TcpStream,
    host_private: Vec<u8>,
    opts: PeerOpts,
    handler: Handler,
    received: Arc<Mutex<Vec<(String, Value)>>>,
    pings: Arc<AtomicU64>,
    handshakes: Arc<AtomicU64>,
    mut notify: broadcast::Receiver<(String, Value)>,
) -> Result<(), String> {
    let ws = tokio_tungstenite::accept_async(tcp).await.map_err(|e| e.to_string())?;
    if opts.after_message_1 == Some(DropKind::Reset) {
        // Linger zero turns the drop below into an RST, which is the point;
        // tokio deprecates it because a non-zero linger blocks the drop.
        #[allow(deprecated)]
        ws.get_ref()
            .set_linger(Some(std::time::Duration::ZERO))
            .map_err(|e| e.to_string())?;
    }
    let (mut sink, mut stream) = ws.split();
    if let Some((code, reason)) = opts.refuse_before_handshake.clone() {
        let _ = sink
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::from(code),
                reason: reason.into(),
            })))
            .await;
        return Ok(());
    }
    if opts.hang {
        std::future::pending::<()>().await;
    }
    let prologue_bytes = prologue(opts.carrier, &opts.host_id).unwrap();
    let mut hs = snow::Builder::new(NOISE_PATTERN.parse().unwrap())
        .local_private_key(&host_private)
        .prologue(&prologue_bytes)
        .build_responder()
        .map_err(|e| e.to_string())?;
    let msg1 = match stream.next().await {
        Some(Ok(Message::Binary(b))) => b,
        other => return Err(format!("expected noise msg 1, got {other:?}")),
    };
    if opts.after_message_1.is_some() {
        // Drop both halves without a close frame: FIN, or RST under linger 0.
        drop(sink);
        drop(stream);
        return Ok(());
    }
    let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
    if hs.read_message(&msg1, &mut buf).is_err() {
        // A Noise failure is 4401 on the daemon (peer_close::UNAUTHENTICATED:
        // "a Noise failure, a bad token, ...").
        let _ = sink
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::from(4401),
                reason: "noise".into(),
            })))
            .await;
        return Err("noise msg 1 rejected".into());
    }
    let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
    sink.send(Message::Binary(buf[..n].to_vec())).await.map_err(|e| e.to_string())?;
    let mut transport = hs.into_transport_mode().map_err(|e| e.to_string())?;
    handshakes.fetch_add(1, Ordering::SeqCst);

    let mut reasm: Vec<u8> = Vec::new();
    let encrypt = |transport: &mut snow::TransportState, plain: Vec<u8>| {
        let mut cipher = vec![0u8; plain.len() + 16];
        let n = transport.write_message(&plain, &mut cipher).unwrap();
        cipher.truncate(n);
        cipher
    };
    loop {
        tokio::select! {
            pushed = notify.recv() => {
                let Ok((method, params)) = pushed else { continue };
                let body = serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0", "method": method, "params": params
                })).unwrap();
                for f in frames(Opcode::Rpc, &lsp_encode(&body)) {
                    let cipher = encrypt(&mut transport, f);
                    sink.send(Message::Binary(cipher)).await.map_err(|e| e.to_string())?;
                }
            }
            msg = stream.next() => {
                let cipher = match msg {
                    Some(Ok(Message::Binary(b))) => b,
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(e.to_string()),
                };
                let mut plain = vec![0u8; cipher.len()];
                let n = transport.read_message(&cipher, &mut plain).map_err(|e| e.to_string())?;
                let header = FrameHeader::decode(&plain[..n]).map_err(|e| e.to_string())?;
                let payload = &plain[HEADER_LEN..n];
                match header.opcode {
                    Opcode::Ping => {
                        pings.fetch_add(1, Ordering::SeqCst);
                        if opts.answer_pings {
                            let mut pong = FrameHeader { opcode: Opcode::Pong, ..header }.encode().to_vec();
                            pong.extend_from_slice(payload);
                            let cipher = encrypt(&mut transport, pong);
                            sink.send(Message::Binary(cipher)).await.map_err(|e| e.to_string())?;
                        }
                    }
                    Opcode::StreamEnd => return Ok(()),
                    Opcode::Rpc => {
                        reasm.extend_from_slice(payload);
                        if !header.fin { continue; }
                        let message = std::mem::take(&mut reasm);
                        let req: RpcRequest = serde_json::from_slice(lsp_decode(&message)).map_err(|e| e.to_string())?;
                        received.lock().unwrap().push((req.method.clone(), req.params.clone()));
                        let response = match handler(&req.method, req.params) {
                            Reply::Result(result) => RpcResponse { jsonrpc: "2.0".into(), id: req.id, result: Some(result), error: None },
                            Reply::Error { code, message, data } => RpcResponse { jsonrpc: "2.0".into(), id: req.id, result: None, error: Some(RpcError { code, message, data }) },
                            Reply::Close(code, reason) => {
                                let _ = sink.send(Message::Close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.into() }))).await;
                                return Ok(());
                            }
                        };
                        let body = serde_json::to_vec(&response).unwrap();
                        for f in frames(Opcode::Rpc, &lsp_encode(&body)) {
                            let cipher = encrypt(&mut transport, f);
                            sink.send(Message::Binary(cipher)).await.map_err(|e| e.to_string())?;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// A handler that answers `auth/hello` with a scoped result and everything
/// else through `rest`.
pub fn hello_then(
    scope: &'static str,
    rest: impl Fn(&str, Value) -> Reply + Send + Sync + 'static,
) -> Handler {
    Arc::new(move |method, params| {
        if method == "auth/hello" {
            Reply::Result(serde_json::json!({
                "protocol": {"min": 1, "max": 1},
                "selected": 1,
                "capabilities": ["hangar.scopes", "hangar.attention.fence", "hangar.mutation.op_id"],
                "daemon_version": "fake",
                "host_id": HOST_ID,
                "scope": {"base": scope, "admin": false},
                "device_expires_at_ms": 1_800_000_000_000i64
            }))
        } else {
            rest(method, params)
        }
    })
}

pub fn method_not_found(method: &str) -> Reply {
    Reply::Error {
        code: -32601,
        message: format!("{method} not found"),
        data: None,
    }
}
