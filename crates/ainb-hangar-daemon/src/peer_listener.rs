//! The off-box peer leg's transport (R1-06a): the WebSocket listener, the
//! Noise IK responder and the pre-auth gate.
//!
//! ```text
//! TCP ──▶ admit per source FIRST (30 handshakes/min -> 4429; 8 unauthenticated
//!         or 16 sockets -> 1013), THEN a global slot (full -> 1013); a refusal
//!         spends its own small budget, and past it the TCP is just dropped
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
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
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
/// Sockets of any state one source may hold at once before 1013, so no
/// source can take the global slots from the rest.
const SOCKETS_PER_SOURCE: usize = 16;
/// Peer sockets served at once, the peer leg's own cap (the unix leg has
/// its own). Taken only AFTER a source is admitted.
const MAX_PEER_CONNECTIONS: usize = 64;
/// Refusals (4429, 1013) in flight at once. A refusal costs an upgrade and a
/// close frame; past this budget the TCP connection is dropped unanswered,
/// so a refused source cannot tie up the served ones.
const REFUSAL_BUDGET: usize = 8;
/// The close reason a 4429 carries: back off at least this long.
const RATE_LIMITED_REASON: &str = "retry-after=60";
/// The close reason a 1013 carries: back off at least this long.
const OVER_CAPACITY_REASON: &str = "retry-after=5";
/// How long shutdown waits for open peer sockets to send their 4503 before
/// the daemon exits anyway.
pub const DRAIN_BOUND: Duration = Duration::from_secs(5);
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

/// A running peer leg: the token that drains it and its accept-loop task.
pub struct PeerLeg {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl PeerLeg {
    fn spawn(listener: TcpListener, carrier: CarrierKind, host: Arc<PeerHost>) -> Self {
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(listener, carrier, host, cancel.clone()));
        Self { cancel, task }
    }

    /// Stop accepting, close every open peer socket 4503, and wait for those
    /// closes to be sent, bounded by [`DRAIN_BOUND`]. Cancels first, so it
    /// also drains on the exits no shutdown signal announces (a one-shot boot,
    /// a boot error), and is harmless after a signal already cancelled it.
    pub async fn drain(self) {
        self.cancel.cancel();
        let bound = DRAIN_BOUND + Duration::from_secs(1);
        if tokio::time::timeout(bound, self.task).await.is_err() {
            tracing::warn!("peer leg did not drain in time");
        }
    }
}

/// Bind the peer leg and serve it until `shutdown` fires (or the returned leg
/// is drained), when every open peer socket closes 4503.
///
/// # Errors
///
/// The bind error, or a Noise responder that cannot be built from this host's
/// key and id.
pub async fn start(
    config: ListenConfig,
    host: PeerHost,
    mut shutdown: crate::shutdown::Handle,
) -> std::io::Result<PeerLeg> {
    // Refuse to bind a leg whose every handshake would fail: a bad key or host
    // id is a boot error to report, not a socket that answers 4401 to all.
    ainb_hangar_noise::responder(&host.secret, config.carrier, &host.host_id).map_err(|error| {
        std::io::Error::other(format!(
            "the peer leg's Noise responder cannot be built: {error}"
        ))
    })?;
    let listener = TcpListener::bind(config.addr).await?;
    if config.carrier == CarrierKind::Lan {
        tracing::warn!(
            addr = %config.addr,
            "peer leg bound on a LAN address: prefer loopback (ssh -L) or the tailnet"
        );
    }
    tracing::info!(addr = %config.addr, carrier = %config.carrier, "peer leg listening");
    let leg = PeerLeg::spawn(listener, config.carrier, Arc::new(host));
    let on_shutdown = leg.cancel.clone();
    tokio::spawn(async move {
        let cause = shutdown.recv().await;
        tracing::info!(?cause, "peer leg draining");
        on_shutdown.cancel();
    });
    Ok(leg)
}

/// The accept loop. Ends when `cancel` fires, then drains every connection
/// task it spawned (each watches the same token and closes 4503), bounded by
/// [`DRAIN_BOUND`], so the closes are sent before the daemon exits.
async fn run(
    listener: TcpListener,
    carrier: CarrierKind,
    host: Arc<PeerHost>,
    cancel: CancellationToken,
) {
    let limits = Arc::new(Mutex::new(Limits::default()));
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_PEER_CONNECTIONS));
    let refusals = Arc::new(tokio::sync::Semaphore::new(REFUSAL_BUDGET));
    let tasks = tokio_util::task::TaskTracker::new();
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
        // Per source first, so one source's sockets can never be the reason
        // another source finds the global slots full.
        let admission = match Limits::admit(&limits, source.ip(), Instant::now()) {
            Admission::Admitted(source_slot) => match Arc::clone(&slots).try_acquire_owned() {
                Ok(global) => Admission::Admitted(source_slot.holding(global)),
                Err(_) => Admission::OverCapacity,
            },
            refused => refused,
        };
        let refusal = if matches!(admission, Admission::Admitted(_)) {
            None
        } else {
            match Arc::clone(&refusals).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    drop(stream);
                    continue;
                }
            }
        };
        let host = Arc::clone(&host);
        let cancel = cancel.clone();
        tasks.spawn(async move {
            let _refusal = refusal;
            serve_socket(stream, admission, carrier, &host, &cancel).await;
        });
    }
    tasks.close();
    if tokio::time::timeout(DRAIN_BOUND, tasks.wait()).await.is_err() {
        tracing::warn!(
            open = tasks.len(),
            "peer leg: sockets still open after the drain bound"
        );
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
        Admission::RateLimited => Ending::Close(peer_close::RATE_LIMITED, RATE_LIMITED_REASON),
        Admission::OverCapacity => Ending::Close(peer_close::OVER_CAPACITY, OVER_CAPACITY_REASON),
        Admission::Admitted(_guard) => tokio::select! {
            () = cancel.cancelled() => Ending::Close(peer_close::DRAINING, "draining"),
            ending = pre_auth(&mut ws, carrier, host) => ending,
        },
    };
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

/// Per-source accounting.
#[derive(Debug, Default)]
struct Limits {
    sources: HashMap<IpAddr, Source>,
    /// Whether the shared loopback budget has been reported this run.
    warned_loopback: bool,
}

#[derive(Debug, Default)]
struct Source {
    handshakes: VecDeque<Instant>,
    unauthenticated: usize,
    sockets: usize,
}

/// What admission decided for one new socket.
enum Admission {
    /// Admitted; the guard holds its per-source and global slots.
    Admitted(SourceSlot),
    /// Over [`HANDSHAKES_PER_WINDOW`]: close 4429.
    RateLimited,
    /// Over [`UNAUTHENTICATED_PER_SOURCE`] or [`SOCKETS_PER_SOURCE`], or no
    /// global slot: close 1013.
    OverCapacity,
}

/// One admitted socket's slots, released on drop: its source's socket and
/// unauthenticated counts (R1-06b releases the unauthenticated one at hello),
/// and its global slot.
///
/// Until R1-06b every socket stays unauthenticated for its whole life, so the
/// 8-unauthenticated cap always binds first and the 16-socket cap
/// ([`SOCKETS_PER_SOURCE`]) cannot bite yet; it starts to matter once hellos
/// are accepted and authenticated sockets stop counting against the 8.
struct SourceSlot {
    limits: Arc<Mutex<Limits>>,
    ip: IpAddr,
    global: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl SourceSlot {
    fn holding(mut self, global: tokio::sync::OwnedSemaphorePermit) -> Self {
        self.global = Some(global);
        self
    }
}

impl Drop for SourceSlot {
    fn drop(&mut self) {
        let mut limits = self.limits.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(source) = limits.sources.get_mut(&self.ip) {
            source.unauthenticated = source.unauthenticated.saturating_sub(1);
            source.sockets = source.sockets.saturating_sub(1);
        }
    }
}

/// The budget a source address counts against. IPv4 and tailnet IPv6 are
/// keyed by address (tailnet addresses are one per node); any other IPv6 by
/// its /64, which one host can hand itself freely, so rotating the low bits
/// never buys a fresh budget. An IPv4-mapped address counts as its IPv4.
fn source_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return IpAddr::V4(v4);
            }
            if carrier_of(ip) == Some(CarrierKind::Tailnet) || v6.is_loopback() {
                return ip;
            }
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        IpAddr::V4(_) => ip,
    }
}

impl Limits {
    /// Count one new socket from `ip`. Every `ssh -L` client arrives as
    /// loopback, so loopback shares one budget (RECONCILED S11: accepted for
    /// v1, and logged when it bites).
    fn admit(limits: &Arc<Mutex<Self>>, ip: IpAddr, now: Instant) -> Admission {
        let ip = source_key(ip);
        let mut guard = limits.lock().unwrap_or_else(PoisonError::into_inner);
        guard.sources.retain(|_, s| {
            s.sockets > 0
                || s.handshakes.back().is_some_and(|t| now.duration_since(*t) < RATE_WINDOW)
        });
        let source = guard.sources.entry(ip).or_default();
        while source.handshakes.front().is_some_and(|t| now.duration_since(*t) >= RATE_WINDOW) {
            source.handshakes.pop_front();
        }
        let refused = if source.handshakes.len() >= HANDSHAKES_PER_WINDOW {
            Some(Admission::RateLimited)
        } else if source.unauthenticated >= UNAUTHENTICATED_PER_SOURCE
            || source.sockets >= SOCKETS_PER_SOURCE
        {
            Some(Admission::OverCapacity)
        } else {
            None
        };
        if let Some(refused) = refused {
            if ip.is_loopback() && !guard.warned_loopback {
                guard.warned_loopback = true;
                tracing::warn!("peer leg: the shared loopback (ssh -L) budget is exhausted");
            }
            return refused;
        }
        source.handshakes.push_back(now);
        source.unauthenticated += 1;
        source.sockets += 1;
        drop(guard);
        Admission::Admitted(SourceSlot {
            limits: Arc::clone(limits),
            ip,
            global: None,
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

    /// Review follow-up F1: one source's silent sockets cannot lock out
    /// another. 64 connections from 127.0.0.2 that never even send the HTTP
    /// upgrade take at most their source's share and the refusal budget; a
    /// device on 127.0.0.1 still completes its handshake. Skips where
    /// 127.0.0.2 is not configured (the macOS default).
    #[tokio::test]
    async fn a_noisy_source_does_not_lock_out_another() {
        let rig = rig().await;
        let mut silent = Vec::new();
        for _ in 0..MAX_PEER_CONNECTIONS {
            let socket = tokio::net::TcpSocket::new_v4().expect("socket");
            if socket.bind("127.0.0.2:0".parse().unwrap()).is_err() {
                eprintln!("SKIP: 127.0.0.2 is not a local address here");
                return;
            }
            silent.push(socket.connect(rig.addr).await.expect("connect"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (_ws, done) = handshake(&rig, CarrierKind::SshL, HOST_ID, rig.host_public).await;
        assert!(
            done.is_ok(),
            "the other source must still get msg2: {:?}",
            done.err()
        );
        drop(silent);
    }

    /// IPv6 sources outside the tailnet count per /64, so rotating the low
    /// bits buys no fresh budget; tailnet, loopback and IPv4 keep their
    /// address, and an IPv4-mapped address counts as its IPv4.
    #[test]
    fn ipv6_sources_share_a_budget_per_slash_64() {
        let key = |s: &str| source_key(s.parse().unwrap());
        assert_eq!(key("2001:db8:1:2:aaaa::1"), key("2001:db8:1:2:bbbb::9"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_ne!(
            key("fd7a:115c:a1e0::1"),
            key("fd7a:115c:a1e0::2"),
            "tailnet per node"
        );
        assert_eq!(key("::1"), "::1".parse::<IpAddr>().unwrap());
        assert_eq!(key("192.0.2.7"), "192.0.2.7".parse::<IpAddr>().unwrap());
        assert_eq!(
            key("::ffff:192.0.2.7"),
            "192.0.2.7".parse::<IpAddr>().unwrap()
        );
    }

    /// The reviewer's probe, kept as the regression test for the drain: the
    /// leg runs on its own current-thread runtime, a peer holds an open
    /// socket, and the runtime is dropped. Dropped straight away, the
    /// connection task dies with it and the peer never reads 4503; drained
    /// first ([`PeerLeg::drain`]), the peer reads 4503. So the drain, not
    /// scheduling luck, is what gets the close out before exit.
    #[test]
    fn draining_before_the_runtime_drops_is_what_sends_4503() {
        let close_code_with = |drain: bool| -> Option<u16> {
            let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            std_listener.set_nonblocking(true).expect("nonblocking");
            let addr = std_listener.local_addr().expect("addr");
            let keypair = generate_keypair().expect("host key");
            let host = Arc::new(PeerHost::new(
                HostId::parse_minted(HOST_ID).expect("host id"),
                &keypair.private,
            ));
            let (connected_tx, connected_rx) = std::sync::mpsc::channel::<()>();
            let leg_thread = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                runtime.block_on(async move {
                    let listener = TcpListener::from_std(std_listener).expect("listener");
                    let leg = PeerLeg::spawn(listener, CarrierKind::SshL, host);
                    while connected_rx.try_recv().is_err() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    if drain {
                        leg.drain().await;
                    }
                });
                // Without a drain this drops the connection task mid-wait.
                drop(runtime);
            });
            let client = tokio::runtime::Runtime::new().expect("client runtime");
            let code = client.block_on(async move {
                let mut ws = connect(addr, ainb_hangar_noise::PEER_PATH).await.expect("upgrade");
                connected_tx.send(()).expect("signal");
                close_code(&mut ws).await
            });
            leg_thread.join().expect("leg thread");
            code
        };
        assert_eq!(close_code_with(true), Some(peer_close::DRAINING));
        assert_ne!(
            close_code_with(false),
            Some(peer_close::DRAINING),
            "without the drain the runtime drop kills the close: this probe must see the difference"
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
