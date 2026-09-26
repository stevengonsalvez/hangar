//! The off-box peer leg's transport (R1-06a): the WebSocket listener, the
//! Noise IK responder and the pre-auth gate.
//!
//! ```text
//! TCP ──▶ admit (per source: 30 handshakes/min -> 4429, 8 unauthenticated -> 1013)
//!     ──▶ WS upgrade on /peer (65535-byte message and frame caps)
//!     ──▶ Noise msg1 within 2 s ──▶ IK responder (carrier + host id prologue)
//!           wrong carrier / host id / unpinned key ──▶ 4401
//!     ──▶ first Rpc message within 2 s; ANY other frame first ──▶ 4401
//!     ──▶ device hello (R1-07): until then UNAUTHORIZED, then 4401
//! shutdown at any point ──▶ 4503 (draining), never 1001 or a bare close
//! ```
//!
//! Dark: the listener binds only when [`LISTEN_ENV`] names an address at boot,
//! and only with a loaded host key and a minted host id. It is an environment
//! variable, never a `daemon_config` key: a paired desktop can reach
//! `hangar/daemon_config_set` nowhere, but a boot-time switch keeps a dark
//! feature out of every RPC path. Unset, which is the default, there is no
//! socket and no new code on any request path, so the daemon behaves exactly as
//! v1.29.0.
//!
//! Before authentication nothing the peer sent is logged as text: only sizes,
//! opcode numbers and close codes.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ainb_hangar_noise::{NoiseError, Opcode, Reassembler, lsp_body, lsp_encode, rpc_frames};
use ainb_hangar_proto::hosts::{CarrierKind, HostId};
use ainb_hangar_proto::peer_close;
use ainb_hangar_proto::{RpcError, RpcId, RpcRequest, RpcResponse};
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// The boot-time switch: `AINB_HANGAR_PEER_LISTEN=<addr>` binds the peer leg.
pub const LISTEN_ENV: &str = "AINB_HANGAR_PEER_LISTEN";

/// How long each pre-auth step may take: Noise msg1 after the upgrade, and
/// the first Rpc message after the handshake.
const PRE_AUTH_STEP: Duration = Duration::from_secs(2);
/// Handshakes one source may start in [`RATE_WINDOW`] before 4429.
const HANDSHAKES_PER_WINDOW: usize = 30;
/// The window [`HANDSHAKES_PER_WINDOW`] counts over.
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Unauthenticated sockets one source may hold at once before 1013.
const UNAUTHENTICATED_PER_SOURCE: usize = 8;
/// Peer sockets served at once, the peer leg's own cap (the unix leg has
/// its own); an accept past it is dropped at once.
const MAX_PEER_CONNECTIONS: usize = 64;
/// The retry hint a 4429 carries, in seconds.
const RATE_LIMITED_RETRY_S: u64 = 60;
/// The retry hint a 1013 carries, in seconds.
const OVER_CAPACITY_RETRY_S: u64 = 5;
/// Longest one outbound WebSocket message (a handshake reply, a refusal, a
/// close frame) may take before the socket is dropped: a peer that stops
/// reading cannot hold the task.
const WRITE_DEADLINE: Duration = Duration::from_secs(2);

/// Whether the peer leg is switched on for this boot: [`LISTEN_ENV`] is set
/// and not blank. Read once at boot by everything the leg needs (its host key
/// first), so an unset variable leaves every one of them untouched.
#[must_use]
pub fn switched_on() -> bool {
    names_an_address(std::env::var_os(LISTEN_ENV).as_deref())
}

fn names_an_address(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| !value.to_string_lossy().trim().is_empty())
}

/// Where the peer leg binds, and the carrier every handshake on it is bound
/// to (the prologue names it, so a device that dialed a different route
/// fails msg1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenConfig {
    /// The socket address.
    pub addr: SocketAddr,
    /// The carrier this address is.
    pub carrier: CarrierKind,
}

/// Parse [`LISTEN_ENV`]: `ip:port`, `[ipv6]:port`, or a bare IP on
/// [`ainb_hangar_noise::DEFAULT_PORT`].
///
/// # Errors
///
/// A message naming the problem: not an address, or an unspecified address
/// (`0.0.0.0` / `::`), whose carrier cannot be known.
pub fn listen_config(value: &str) -> Result<ListenConfig, String> {
    let value = value.trim();
    let addr = value
        .parse::<SocketAddr>()
        .or_else(|_| {
            value
                .parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, ainb_hangar_noise::DEFAULT_PORT))
        })
        .map_err(|_| format!("{LISTEN_ENV} is not an address: {value:?}"))?;
    let carrier = carrier_of(addr.ip()).ok_or_else(|| {
        format!("{LISTEN_ENV} names an unspecified address; name the interface to bind")
    })?;
    Ok(ListenConfig { addr, carrier })
}

/// The carrier an address is: loopback is `ssh -L`, the tailnet ranges
/// (100.64.0.0/10, fd7a:115c:a1e0::/48) are `tailnet`, anything else is a
/// LAN. `None` for an unspecified address.
fn carrier_of(ip: IpAddr) -> Option<CarrierKind> {
    if ip.is_unspecified() {
        return None;
    }
    if ip.is_loopback() {
        return Some(CarrierKind::SshL);
    }
    let tailnet = match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (64..128).contains(&b)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    };
    Some(if tailnet {
        CarrierKind::Tailnet
    } else {
        CarrierKind::Lan
    })
}

/// This host's identity on the peer leg: the minted host id and the Noise
/// static secret (zeroed on drop).
pub struct PeerHost {
    host_id: HostId,
    secret: Zeroizing<[u8; 32]>,
}

impl PeerHost {
    /// The identity from the loaded host key and the minted host id.
    #[must_use]
    pub fn new(host_id: HostId, secret: &[u8; 32]) -> Self {
        let mut held = Zeroizing::new([0u8; 32]);
        held.copy_from_slice(secret);
        Self {
            host_id,
            secret: held,
        }
    }
}

impl std::fmt::Debug for PeerHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerHost")
            .field("host_id", &self.host_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Bind the peer leg and serve it until `shutdown` fires, when every open
/// peer socket closes 4503.
///
/// # Errors
///
/// The bind error.
pub async fn start(
    config: ListenConfig,
    host: PeerHost,
    mut shutdown: crate::shutdown::Handle,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let listener = TcpListener::bind(config.addr).await?;
    if config.carrier == CarrierKind::Lan {
        tracing::warn!(
            addr = %config.addr,
            "peer leg bound on a LAN address: prefer loopback (ssh -L) or the tailnet"
        );
    }
    tracing::info!(addr = %config.addr, carrier = %config.carrier, "peer leg listening");
    let cancel = CancellationToken::new();
    let on_shutdown = cancel.clone();
    tokio::spawn(async move {
        let cause = shutdown.recv().await;
        tracing::info!(?cause, "peer leg draining");
        on_shutdown.cancel();
    });
    Ok(tokio::spawn(run(
        listener,
        config.carrier,
        Arc::new(host),
        cancel,
    )))
}

/// The accept loop. Ends when `cancel` fires; every connection it spawned
/// watches the same token and closes 4503.
async fn run(
    listener: TcpListener,
    carrier: CarrierKind,
    host: Arc<PeerHost>,
    cancel: CancellationToken,
) {
    let limits = Arc::new(Mutex::new(Limits::default()));
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_PEER_CONNECTIONS));
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, source) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "peer leg accept failed");
                continue;
            }
        };
        let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let admission = Limits::admit(&limits, source.ip(), Instant::now());
        let host = Arc::clone(&host);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let _slot = slot;
            serve_socket(stream, admission, carrier, &host, &cancel).await;
        });
    }
}

/// How one socket ended, and so how it is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// Close with this code and reason.
    Close(u16, &'static str),
    /// The peer went away, or its socket is unusable: nothing to send.
    Dropped,
}

/// Serve one TCP connection through the pre-auth gate.
async fn serve_socket(
    stream: TcpStream,
    admission: Admission,
    carrier: CarrierKind,
    host: &PeerHost,
    cancel: &CancellationToken,
) {
    let config = WebSocketConfig {
        max_message_size: Some(ainb_hangar_noise::MAX_NOISE_MESSAGE),
        max_frame_size: Some(ainb_hangar_noise::MAX_NOISE_MESSAGE),
        ..WebSocketConfig::default()
    };
    let upgraded = tokio::time::timeout(
        PRE_AUTH_STEP,
        tokio_tungstenite::accept_hdr_async_with_config(stream, only_peer_path, Some(config)),
    )
    .await;
    let Ok(Ok(mut ws)) = upgraded else {
        return;
    };
    let ending = match admission {
        Admission::RateLimited => Ending::Close(peer_close::RATE_LIMITED, "retry-after=60"),
        Admission::OverCapacity => Ending::Close(peer_close::OVER_CAPACITY, "retry-after=5"),
        Admission::Admitted(_guard) => tokio::select! {
            () = cancel.cancelled() => Ending::Close(peer_close::DRAINING, "draining"),
            ending = pre_auth(&mut ws, carrier, host) => ending,
        },
    };
    debug_assert_eq!(RATE_LIMITED_RETRY_S, 60);
    debug_assert_eq!(OVER_CAPACITY_RETRY_S, 5);
    if let Ending::Close(code, reason) = ending {
        close(&mut ws, code, reason).await;
    }
}

/// The upgrade callback: only [`ainb_hangar_noise::PEER_PATH`] is served.
#[allow(clippy::result_large_err)]
fn only_peer_path(request: &Request, response: Response) -> Result<Response, ErrorResponse> {
    if request.uri().path() == ainb_hangar_noise::PEER_PATH {
        return Ok(response);
    }
    let mut refused = ErrorResponse::new(None);
    *refused.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
    Err(refused)
}

/// The Noise handshake and the first message, each within [`PRE_AUTH_STEP`].
///
/// Everything here ends the socket: the device hello that would carry an
/// authenticated session on is R1-07, so the first message is answered
/// `UNAUTHORIZED` and closed 4401 until then.
async fn pre_auth<S>(ws: &mut WebSocketStream<S>, carrier: CarrierKind, host: &PeerHost) -> Ending
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    const UNAUTHENTICATED: Ending = Ending::Close(peer_close::UNAUTHENTICATED, "unauthenticated");

    // Noise msg1.
    let msg1 = match tokio::time::timeout(PRE_AUTH_STEP, ws.next()).await {
        Ok(Some(Ok(Message::Binary(bytes)))) => bytes,
        Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) => return Ending::Dropped,
        // Text, ping, anything that is not a binary Noise message, or silence.
        Ok(Some(Ok(_))) | Err(_) => return UNAUTHENTICATED,
    };
    let Ok(mut handshake) = ainb_hangar_noise::responder(&host.secret, carrier, &host.host_id)
    else {
        return Ending::Close(peer_close::DRAINING, "draining");
    };
    // A wrong carrier, a wrong host id, or a key the device did not pin all
    // fail here, before any Rpc frame is read.
    if handshake.read_message(&msg1).is_err() {
        tracing::debug!(bytes = msg1.len(), "peer leg: Noise msg1 refused");
        return UNAUTHENTICATED;
    }
    let Ok(msg2) = handshake.write_message() else {
        return UNAUTHENTICATED;
    };
    if !send(ws, Message::Binary(msg2)).await {
        return Ending::Dropped;
    }
    let Ok(session) = handshake.into_session() else {
        return UNAUTHENTICATED;
    };
    let (mut sealer, mut opener) = session.split();

    // The first Rpc message. Before a hello is accepted, ANY frame that is
    // not part of it (Ping, StreamEnd, a reserved or unknown opcode, one that
    // fails to open) closes 4401, and the whole message must arrive within
    // one step.
    let deadline = tokio::time::Instant::now() + PRE_AUTH_STEP;
    let mut reassembler = Reassembler::pre_auth();
    let message = loop {
        let next = match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(Message::Binary(bytes)))) => bytes,
            Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) => return Ending::Dropped,
            Ok(Some(Ok(_))) | Err(_) => return UNAUTHENTICATED,
        };
        let frame = match opener.open(&next) {
            Ok(frame) => frame,
            Err(error) => {
                let unknown_opcode = matches!(&error, NoiseError::Frame(_)) && !error.is_fatal();
                tracing::debug!(
                    bytes = next.len(),
                    unknown_opcode,
                    "peer leg: pre-auth frame refused"
                );
                return UNAUTHENTICATED;
            }
        };
        if frame.header.opcode != Opcode::Rpc {
            tracing::debug!(
                opcode = frame.header.opcode.as_u8(),
                "peer leg: non-Rpc frame before hello"
            );
            return UNAUTHENTICATED;
        }
        match reassembler.push(&frame) {
            Ok(Some(message)) => break message,
            Ok(None) => {}
            Err(_) => return UNAUTHENTICATED,
        }
    };
    let Ok(body) = lsp_body(&message) else {
        return UNAUTHENTICATED;
    };
    let id = serde_json::from_slice::<RpcRequest>(body).map_or(RpcId::Number(0), |r| r.id);
    // The device hello (`auth/hello` with a device token, or `device/redeem`)
    // is R1-07. The operator token is never accepted here, so until then no
    // request authenticates.
    let refusal = RpcResponse {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id,
        result: None,
        error: Some(RpcError {
            code: ainb_hangar_proto::auth::UNAUTHORIZED,
            message: "unauthenticated".to_string(),
            data: None,
        }),
    };
    let Ok(body) = serde_json::to_vec(&refusal) else {
        return UNAUTHENTICATED;
    };
    for frame in rpc_frames(&lsp_encode(&body)) {
        let Ok(sealed) = sealer.seal(&frame) else {
            return UNAUTHENTICATED;
        };
        if !send(ws, Message::Binary(sealed)).await {
            return Ending::Dropped;
        }
    }
    UNAUTHENTICATED
}

/// Send one message within [`WRITE_DEADLINE`]; `false` when it could not be.
async fn send<S>(ws: &mut WebSocketStream<S>, message: Message) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    matches!(
        tokio::time::timeout(WRITE_DEADLINE, ws.send(message)).await,
        Ok(Ok(()))
    )
}

/// Close with `code` and `reason`, bounded by [`WRITE_DEADLINE`].
async fn close<S>(ws: &mut WebSocketStream<S>, code: u16, reason: &'static str)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let frame = CloseFrame {
        code: CloseCode::from(code),
        reason: reason.into(),
    };
    let _ = send(ws, Message::Close(Some(frame))).await;
    let _ = tokio::time::timeout(WRITE_DEADLINE, ws.close(None)).await;
}

/// Per-source pre-auth accounting.
#[derive(Debug, Default)]
struct Limits {
    sources: HashMap<IpAddr, Source>,
}

#[derive(Debug, Default)]
struct Source {
    handshakes: VecDeque<Instant>,
    unauthenticated: usize,
}

/// What admission decided for one new socket.
enum Admission {
    /// Admitted; the guard holds its unauthenticated slot.
    Admitted(UnauthenticatedSlot),
    /// Over [`HANDSHAKES_PER_WINDOW`]: close 4429.
    RateLimited,
    /// Over [`UNAUTHENTICATED_PER_SOURCE`]: close 1013.
    OverCapacity,
}

/// One source's unauthenticated slot, released on drop.
struct UnauthenticatedSlot {
    limits: Arc<Mutex<Limits>>,
    ip: IpAddr,
}

impl Drop for UnauthenticatedSlot {
    fn drop(&mut self) {
        let mut limits = self.limits.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(source) = limits.sources.get_mut(&self.ip) {
            source.unauthenticated = source.unauthenticated.saturating_sub(1);
        }
    }
}

impl Limits {
    /// Count one new socket from `ip`. Every `ssh -L` client arrives as
    /// loopback, so loopback shares one budget (RECONCILED S11: accepted for
    /// v1, and logged when it bites).
    fn admit(limits: &Arc<Mutex<Self>>, ip: IpAddr, now: Instant) -> Admission {
        let mut guard = limits.lock().unwrap_or_else(PoisonError::into_inner);
        guard.sources.retain(|_, s| {
            s.unauthenticated > 0
                || s.handshakes.back().is_some_and(|t| now.duration_since(*t) < RATE_WINDOW)
        });
        let source = guard.sources.entry(ip).or_default();
        while source.handshakes.front().is_some_and(|t| now.duration_since(*t) >= RATE_WINDOW) {
            source.handshakes.pop_front();
        }
        let refused = if source.handshakes.len() >= HANDSHAKES_PER_WINDOW {
            Some(Admission::RateLimited)
        } else if source.unauthenticated >= UNAUTHENTICATED_PER_SOURCE {
            Some(Admission::OverCapacity)
        } else {
            None
        };
        if let Some(refused) = refused {
            if ip.is_loopback() {
                tracing::warn!("peer leg: the shared loopback (ssh -L) budget is exhausted");
            }
            return refused;
        }
        source.handshakes.push_back(now);
        source.unauthenticated += 1;
        drop(guard);
        Admission::Admitted(UnauthenticatedSlot {
            limits: Arc::clone(limits),
            ip,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use ainb_hangar_noise::{Frame, Opener, Sealer, generate_keypair, initiator};

    use super::*;

    /// Unset is the default and means off; so does a blank value, which a
    /// shell profile that exports the name with nothing after it produces.
    #[test]
    fn only_a_non_blank_value_switches_the_leg_on() {
        assert!(!names_an_address(None));
        assert!(!names_an_address(Some(OsStr::new(""))));
        assert!(!names_an_address(Some(OsStr::new("  "))));
        assert!(names_an_address(Some(OsStr::new("127.0.0.1:47300"))));
    }

    #[test]
    fn the_carrier_comes_from_the_bind_address() {
        let carrier = |v: &str| listen_config(v).map(|c| c.carrier);
        assert_eq!(carrier("127.0.0.1:47300"), Ok(CarrierKind::SshL));
        assert_eq!(carrier("[::1]:47300"), Ok(CarrierKind::SshL));
        assert_eq!(carrier("100.100.1.2:47300"), Ok(CarrierKind::Tailnet));
        assert_eq!(carrier("100.64.0.1:1"), Ok(CarrierKind::Tailnet));
        assert_eq!(carrier("100.128.0.1:1"), Ok(CarrierKind::Lan));
        assert_eq!(carrier("[fd7a:115c:a1e0::1]:1"), Ok(CarrierKind::Tailnet));
        assert_eq!(carrier("192.168.1.2:47300"), Ok(CarrierKind::Lan));
        assert!(
            carrier("0.0.0.0:47300").is_err(),
            "unspecified has no carrier"
        );
        assert!(carrier("[::]:47300").is_err());
        assert!(carrier("not an address").is_err());
        assert_eq!(
            listen_config("127.0.0.1").map(|c| c.addr.port()),
            Ok(ainb_hangar_noise::DEFAULT_PORT),
            "a bare IP takes the default port"
        );
    }

    const HOST_ID: &str = "01J0H0STAAAAAAAAAAAAAAAAAA";

    /// One listener on loopback with its own limits, a fresh host key, and a
    /// token to shut it down.
    struct Rig {
        addr: SocketAddr,
        host_public: [u8; 32],
        cancel: CancellationToken,
    }

    async fn rig() -> Rig {
        let keypair = generate_keypair().expect("host key");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let host = PeerHost::new(
            HostId::parse_minted(HOST_ID).expect("host id"),
            &keypair.private,
        );
        let cancel = CancellationToken::new();
        tokio::spawn(run(
            listener,
            CarrierKind::SshL,
            Arc::new(host),
            cancel.clone(),
        ));
        Rig {
            addr,
            host_public: keypair.public,
            cancel,
        }
    }

    type Client = WebSocketStream<TcpStream>;

    async fn connect(
        addr: SocketAddr,
        path: &str,
    ) -> Result<Client, tokio_tungstenite::tungstenite::Error> {
        let tcp = TcpStream::connect(addr).await.expect("tcp");
        tokio_tungstenite::client_async(format!("ws://{addr}{path}"), tcp)
            .await
            .map(|(ws, _)| ws)
    }

    /// A device that completes the handshake it was set up for.
    async fn handshake(
        rig: &Rig,
        carrier: CarrierKind,
        host_id: &str,
        pinned: [u8; 32],
    ) -> (Client, Result<(Sealer, Opener), Option<u16>>) {
        let mut ws = connect(rig.addr, ainb_hangar_noise::PEER_PATH).await.expect("upgrade");
        let device = generate_keypair().expect("device key");
        let mut hs = initiator(
            &device.private,
            &pinned,
            carrier,
            &HostId::parse_minted(host_id).expect("host id"),
        )
        .expect("initiator");
        ws.send(Message::Binary(hs.write_message().expect("msg1")))
            .await
            .expect("send msg1");
        let reply = match ws.next().await {
            Some(Ok(Message::Binary(bytes))) => bytes,
            // No msg2: the close code the host sent instead.
            Some(Ok(Message::Close(frame))) => return (ws, Err(frame.map(|f| u16::from(f.code)))),
            _ => return (ws, Err(None)),
        };
        hs.read_message(&reply).expect("msg2");
        let session = hs.into_session().expect("session");
        (ws, Ok(session.split()))
    }

    /// The close code the host sends, reading past anything before it.
    async fn close_code(ws: &mut Client) -> Option<u16> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, ws.next()).await {
                Ok(Some(Ok(Message::Close(frame)))) => return frame.map(|f| u16::from(f.code)),
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(_)) | None) | Err(_) => return None,
            }
        }
    }

    fn sealed(sealer: &mut Sealer, frame: &Frame) -> Message {
        Message::Binary(sealer.seal(frame).expect("seal"))
    }

    /// G5 (spike 3): a wrong carrier, a wrong host id and a key the device
    /// did not pin each fail msg1 and close 4401 before any Rpc frame.
    #[tokio::test]
    async fn a_wrong_route_or_key_closes_4401_at_msg1() {
        let rig = rig().await;
        let wrong_key = generate_keypair().expect("other key").public;
        for (carrier, host_id, pinned) in [
            (CarrierKind::Tailnet, HOST_ID, rig.host_public),
            (
                CarrierKind::SshL,
                "01J0ANTHERH0STAAAAAAAAAAAA",
                rig.host_public,
            ),
            (CarrierKind::SshL, HOST_ID, wrong_key),
        ] {
            let (_ws, done) = handshake(&rig, carrier, host_id, pinned).await;
            assert_eq!(
                done.err(),
                Some(Some(peer_close::UNAUTHENTICATED)),
                "no msg2, and 4401, for {carrier:?} {host_id}"
            );
        }
    }

    /// Before a hello, any frame that is not an Rpc hello closes 4401: a
    /// Ping, a reserved opcode, a StreamEnd.
    #[tokio::test]
    async fn any_frame_but_an_rpc_before_hello_closes_4401() {
        let rig = rig().await;
        for opcode in [Opcode::Ping, Opcode::Output, Opcode::StreamEnd] {
            let (mut ws, done) = handshake(&rig, CarrierKind::SshL, HOST_ID, rig.host_public).await;
            let (mut sealer, _opener) = done.expect("handshake");
            ws.send(sealed(&mut sealer, &Frame::control(opcode, 0, Vec::new())))
                .await
                .expect("send");
            assert_eq!(
                close_code(&mut ws).await,
                Some(peer_close::UNAUTHENTICATED),
                "{opcode:?}"
            );
        }
    }

    /// The first message of a session is refused until the device hello
    /// exists (R1-07): an UNAUTHORIZED reply, then 4401. An operator-token
    /// hello is refused the same way.
    #[tokio::test]
    async fn a_hello_is_refused_until_the_device_hello_lands() {
        let rig = rig().await;
        let (mut ws, done) = handshake(&rig, CarrierKind::SshL, HOST_ID, rig.host_public).await;
        let (mut sealer, mut opener) = done.expect("handshake");
        let hello = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "auth/hello",
            "params": {"token": "mdt_operator"},
        }))
        .unwrap();
        for frame in rpc_frames(&lsp_encode(&hello)) {
            ws.send(sealed(&mut sealer, &frame)).await.expect("send");
        }
        let reply = match ws.next().await {
            Some(Ok(Message::Binary(bytes))) => bytes,
            other => panic!("expected the refusal, got {other:?}"),
        };
        let frame = opener.open(&reply).expect("open");
        let mut reassembler = Reassembler::new();
        let message = reassembler.push(&frame).expect("push").expect("one frame");
        let response: serde_json::Value =
            serde_json::from_slice(lsp_body(&message).expect("lsp")).expect("json");
        assert_eq!(response["id"], 7);
        assert_eq!(
            response["error"]["code"],
            ainb_hangar_proto::auth::UNAUTHORIZED
        );
        assert_eq!(close_code(&mut ws).await, Some(peer_close::UNAUTHENTICATED));
    }

    /// Silence: no msg1 within 2 s, or no first frame within 2 s of the
    /// handshake, closes 4401.
    #[tokio::test]
    async fn a_silent_peer_closes_4401_within_the_step() {
        let rig = rig().await;
        let mut ws = connect(rig.addr, ainb_hangar_noise::PEER_PATH).await.expect("upgrade");
        let started = Instant::now();
        assert_eq!(close_code(&mut ws).await, Some(peer_close::UNAUTHENTICATED));
        assert!(started.elapsed() < Duration::from_secs(4));

        let (mut ws, done) = handshake(&rig, CarrierKind::SshL, HOST_ID, rig.host_public).await;
        done.expect("handshake");
        assert_eq!(close_code(&mut ws).await, Some(peer_close::UNAUTHENTICATED));
    }

    /// A text message is never a Noise message.
    #[tokio::test]
    async fn a_text_message_closes_4401() {
        let rig = rig().await;
        let mut ws = connect(rig.addr, ainb_hangar_noise::PEER_PATH).await.expect("upgrade");
        ws.send(Message::Text("hello".to_string())).await.expect("send");
        assert_eq!(close_code(&mut ws).await, Some(peer_close::UNAUTHENTICATED));
    }

    #[tokio::test]
    async fn only_the_peer_path_upgrades() {
        let rig = rig().await;
        assert!(connect(rig.addr, "/elsewhere").await.is_err());
    }

    /// Shutdown or restart closes every open peer socket 4503 (draining),
    /// never 1001 or a bare close: phones redial on 4503 and give up on codes
    /// they do not know.
    #[tokio::test]
    async fn shutdown_closes_open_peer_sockets_4503() {
        let rig = rig().await;
        let (mut after_handshake, done) =
            handshake(&rig, CarrierKind::SshL, HOST_ID, rig.host_public).await;
        done.expect("handshake");
        let mut before_msg1 =
            connect(rig.addr, ainb_hangar_noise::PEER_PATH).await.expect("upgrade");
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.cancel.cancel();
        assert_eq!(
            close_code(&mut after_handshake).await,
            Some(peer_close::DRAINING)
        );
        assert_eq!(
            close_code(&mut before_msg1).await,
            Some(peer_close::DRAINING)
        );
    }

    /// Nine unauthenticated sockets from one source: the ninth closes 1013
    /// with a retry hint.
    #[tokio::test]
    async fn the_ninth_unauthenticated_socket_closes_1013() {
        let rig = rig().await;
        let mut held = Vec::new();
        for _ in 0..UNAUTHENTICATED_PER_SOURCE {
            held.push(connect(rig.addr, ainb_hangar_noise::PEER_PATH).await.expect("upgrade"));
        }
        let mut ninth = connect(rig.addr, ainb_hangar_noise::PEER_PATH).await.expect("upgrade");
        assert_eq!(
            close_code(&mut ninth).await,
            Some(peer_close::OVER_CAPACITY)
        );
    }

    /// Thirty handshakes a minute from one source; the thirty-first closes
    /// 4429 with a retry hint, even with every earlier socket gone.
    #[tokio::test]
    async fn the_thirty_first_handshake_in_a_minute_closes_4429() {
        let rig = rig().await;
        for _ in 0..HANDSHAKES_PER_WINDOW {
            let mut ws = connect(rig.addr, ainb_hangar_noise::PEER_PATH).await.expect("upgrade");
            ws.send(Message::Text("x".to_string())).await.expect("send");
            assert_eq!(close_code(&mut ws).await, Some(peer_close::UNAUTHENTICATED));
        }
        let mut limited = connect(rig.addr, ainb_hangar_noise::PEER_PATH).await.expect("upgrade");
        assert_eq!(
            close_code(&mut limited).await,
            Some(peer_close::RATE_LIMITED)
        );
    }
}
