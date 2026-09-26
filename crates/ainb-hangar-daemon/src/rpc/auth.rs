//! Socket-connection authentication for the daemon's RPC server (e38.1).
//!
//! Two layers gate every accepted connection before any `hangar/*` method is
//! reachable:
//!
//! 1. **Same-uid peer credentials** — the kernel-reported peer uid must match
//!    the daemon's own uid ([`same_uid_peer`]). `tokio`'s
//!    [`peer_cred`](tokio::net::UnixStream::peer_cred) reads `SO_PEERCRED` on
//!    Linux and `LOCAL_PEERCRED`/`getpeereid` on macOS, so both cfg paths are
//!    covered by the one call.
//! 2. **First-frame token auth** — the first decoded frame must be an
//!    `auth/hello` request ([`ainb_hangar_proto::auth`]) whose token verifies
//!    against the stored digest through the constant-time
//!    [`ainb_hangar_core::token::verify`] seam
//!    ([`SocketTokenRepo::verify`]). Anything else is answered with an
//!    [`UNAUTHORIZED`](ainb_hangar_proto::auth::UNAUTHORIZED) error and the
//!    connection is closed.
//!
//! ## Token lifecycle
//!
//! [`ensure_socket_token`] runs at boot, **before** the socket binds: when the
//! stored digest and the on-disk plaintext agree, the credential is reused;
//! otherwise a fresh `mdt_…` token is minted (CSPRNG), only its SHA-256 hex
//! digest is persisted (`daemon_socket_token`, migration 0011), and the
//! plaintext is written exactly once to `{hangar_home}/hangar/daemon.token`
//! with `0600` permissions. Clients (the hangar-tui plugin, test harnesses)
//! read that file and present it on their first frame.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::token::{TokenKind, mint, sha256_hex};
use ainb_hangar_proto::auth::{DeviceInfo, HelloParams, HelloResult, UNAUTHORIZED};
use ainb_hangar_proto::connections::{SurfaceHost, SurfaceInfo};
use ainb_hangar_proto::devices::{CallParams, DeviceScope, EventFamily, Grantor};
use ainb_hangar_proto::fleet::FleetActionParams;
use ainb_hangar_proto::protocol::{
    PROTOCOL_INCOMPATIBLE, ProtocolRange, catalogue_strings, negotiate,
};
use ainb_hangar_proto::terminal::TerminalAttachParams;
use ainb_hangar_proto::{RpcError, RpcId, RpcRequest, RpcResponse, methods};
use ainb_hangar_store::repo::token::SocketTokenRepo;
use sqlx::SqlitePool;

/// Which surface a connection authenticated as.
///
/// The daemon used to have exactly ONE credential, so every authenticated
/// connection was the operator by definition. That stopped being true the
/// moment a MODEL's tool call could mint a confirm card a human is supposed to
/// be the only one who can answer: Pal's tool server is a process the
/// operator's agent steers, and handing it the operator's own token would let
/// it answer its own cards, forge `fleet/message_send {actor: "operator"}`, and
/// call `attention/answer` around the gate entirely.
///
/// So Pal gets its OWN token, minted per channel scope, and the
/// connection carries what that token means for as long as it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// An operator surface (the TUI plugin, the CLI, the macOS client): the
    /// daemon's own `0600` token.
    Operator,
    /// Pal's MCP tool server, on a token minted for ONE channel scope.
    Pal {
        /// The Pal channel the token was minted against. The gate files its
        /// cards here, so a card's `scope_key` names the conversation the call
        /// actually came from.
        scope_key: String,
    },
    /// A paired device on the off-box peer leg (R1, spec D13).
    ///
    /// Constructed ONLY by the peer leg, from the device's token row after the
    /// Noise session proved the bound key: both fields come from the
    /// credential, never from the request. A unix-leg hello that carries
    /// `HelloParams.device` is still [`Caller::Operator`] or [`Caller::Pal`]
    /// and gains no identity from it.
    Device {
        /// The registry id, which is also the ledger principal
        /// `device:<device_id>` and the `answered_by` / `actor` stamp.
        device_id: String,
        /// The scope granted at pairing, read from the row at hello.
        scope: DeviceScope,
    },
}

/// Identity established by one successful `auth/hello` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedHello {
    /// Credential authority carried for every later request on the connection.
    pub caller: Caller,
    /// Optional client-declared surface metadata for the live registry.
    pub surface: Option<SurfaceInfo>,
    /// The protocol version the two peers settled on (D17).
    ///
    /// Always a version this build serves: a connection whose range did not
    /// overlap never reaches this struct, it is closed with
    /// [`PROTOCOL_INCOMPATIBLE`].
    pub protocol: u32,
    /// The capability strings the CLIENT declared it understands.
    ///
    /// Advisory: the daemon does not gate on it. It exists so a surface census
    /// can answer "which of my clients can already read the new event kind"
    /// without auditing release notes.
    pub capabilities: Vec<String>,
    /// The paired device this connection belongs to (R1, off-box only).
    pub device: Option<DeviceInfo>,
    /// The client ASKED that this call connection not be listed because its
    /// process's presence is held by another connection. A request only: the
    /// registry honours it solely beside a listed row at the same pid.
    pub transient: bool,
    /// The surface a plugin connection says hosts it (#1040). A claim: the
    /// registry checks it against the connection's peer before folding.
    pub host: Option<ainb_hangar_proto::connections::SurfaceHost>,
}

/// Every method a Pal connection may call, and nothing else.
///
/// The read tools, the gate, and the two writes the tool table can reach after
/// the gate said run. Deliberately absent: `fleet/confirm_answer` (answering its
/// own cards), `fleet/pal_configure` (rewriting its own persona),
/// `fleet/acp_session_create`, and every `hangar/*` method.
///
/// `attention/answer` IS here, because `answer_need` is a real tool. Binding it
/// to the gate verdict that approved it needs a per-call capability the gate
/// would have to issue; until then the confirm card is what stands between an
/// injected transcript and that call.
const PAL_METHODS: &[&str] = &[
    methods::PING,
    methods::FLEET_PAL_GATE,
    methods::FLEET_SNAPSHOT,
    methods::FLEET_STATUS,
    methods::ATTENTION_LIST,
    methods::ATTENTION_ANSWER,
    methods::FLEET_TRANSCRIPT_LIST,
    methods::FLEET_MESSAGE_SEND,
];

impl Caller {
    /// Refuse a method this caller's surface is not allowed to reach.
    ///
    /// This is the per-CONNECTION check. `require_fleet_capability` is a
    /// build-time gate over a static const array — it says what the daemon
    /// serves, never who may ask for it.
    ///
    /// A device is checked against the frozen scope table
    /// ([`ainb_hangar_proto::devices::method_allowed`]), which classifies
    /// every method with no default. The two params rules (`fleet/action`
    /// interrupt-only, `terminal/attach` watch-only) read the TYPED params:
    /// `params` is parsed into the method's own params struct, the one its
    /// handler parses, so a parser differential cannot smuggle `kill` past an
    /// interrupt-only grant. Params that do not parse earn only a plain
    /// `allow`, never a params rule.
    ///
    /// # Errors
    ///
    /// [`UNAUTHORIZED`] when a Pal connection asks for a method outside
    /// [`PAL_METHODS`], or a device for a method (or params) its scope does
    /// not grant.
    pub fn authorize(&self, method: &str, params: &serde_json::Value) -> Result<(), RpcError> {
        match self {
            Self::Operator => Ok(()),
            Self::Pal { .. } if PAL_METHODS.contains(&method) => Ok(()),
            Self::Pal { .. } => Err(RpcError {
                code: UNAUTHORIZED,
                message: format!("the Pal credential may not call {method}"),
                data: None,
            }),
            Self::Device { scope, .. } => {
                let allowed = match method {
                    methods::FLEET_ACTION => {
                        match serde_json::from_value::<FleetActionParams>(params.clone()) {
                            Ok(typed) => ainb_hangar_proto::devices::method_allowed(
                                scope,
                                method,
                                &CallParams::FleetAction(&typed.action),
                            ),
                            Err(_) => ainb_hangar_proto::devices::method_allowed(
                                scope,
                                method,
                                &CallParams::Untyped,
                            ),
                        }
                    }
                    methods::TERMINAL_ATTACH => {
                        match serde_json::from_value::<TerminalAttachParams>(params.clone()) {
                            Ok(typed) => ainb_hangar_proto::devices::method_allowed(
                                scope,
                                method,
                                &CallParams::TerminalAttach(&typed),
                            ),
                            Err(_) => ainb_hangar_proto::devices::method_allowed(
                                scope,
                                method,
                                &CallParams::Untyped,
                            ),
                        }
                    }
                    _ => ainb_hangar_proto::devices::method_allowed(
                        scope,
                        method,
                        &CallParams::Untyped,
                    ),
                };
                if allowed {
                    Ok(())
                } else {
                    Err(RpcError {
                        code: UNAUTHORIZED,
                        message: format!(
                            "a device with scope {} may not call {method} with these params",
                            scope.base().as_str()
                        ),
                        data: None,
                    })
                }
            }
        }
    }

    /// Whether this caller receives events of `family` on a stream it may
    /// subscribe to. The operator and Pal receive every family their
    /// subscriptions carry; a device only what its scope's row grants.
    #[must_use]
    pub fn receives(&self, family: EventFamily) -> bool {
        match self {
            Self::Operator | Self::Pal { .. } => true,
            Self::Device { scope, .. } => ainb_hangar_proto::devices::event_allowed(scope, family),
        }
    }

    /// Whether this caller may give a device `target`, by invite or rescope.
    ///
    /// `subject` is the scope the device being rescoped holds now, `None` for
    /// a new device. On top of the frozen [`Grantor::may_grant`]:
    /// - a device grants only when it is itself `admin`; the scope table
    ///   already refuses `device/*` to non-admins, and this keeps the rule true
    ///   if a row ever changes;
    /// - no device may change the scope of an `admin` device, so one admin
    ///   desktop cannot demote another. Only the operator can.
    ///
    /// Pal grants nothing.
    #[must_use]
    pub fn may_grant(&self, target: DeviceScope, subject: Option<DeviceScope>) -> bool {
        match self {
            Self::Operator => Grantor::Operator.may_grant(target),
            Self::Pal { .. } => false,
            Self::Device { scope, .. } => {
                scope.admin()
                    && !subject.is_some_and(DeviceScope::admin)
                    && Grantor::Device(*scope).may_grant(target)
            }
        }
    }

    /// The Pal channel scope this connection is bound to, if any.
    #[must_use]
    pub fn pal_scope(&self) -> Option<&str> {
        match self {
            Self::Operator | Self::Device { .. } => None,
            Self::Pal { scope_key } => Some(scope_key),
        }
    }

    /// The actor a write from this caller is authored as: the one stamp for
    /// every author, creator and acting-actor field a caller could otherwise
    /// fill in itself (security review follow-up A).
    ///
    /// - The operator: `claimed`, else the local member. Unchanged, because
    ///   the operator token is shared by the local surfaces and by the agents
    ///   the operator runs, which author as themselves.
    /// - A device: `member:device:<device_id>`, whatever it claimed. Pinned,
    ///   not validated, like its `fleet/message_send.actor`, so a phone never
    ///   comments as the operator or as an agent.
    /// - Pal: `agent:<PAL_ACTOR>`. Pal reaches none of these methods today
    ///   ([`PAL_METHODS`]); this is the safe answer if one is ever added.
    #[must_use]
    pub fn stamp(&self, claimed: Option<ActorRef>) -> ActorRef {
        match self {
            Self::Operator => claimed.unwrap_or_else(ainb_hangar_core::actor::local_member),
            Self::Device { device_id, .. } => {
                ActorRef::new(ActorKind::Member, format!("device:{device_id}"))
                    .expect("a device id is never empty")
            }
            Self::Pal { .. } => ActorRef::new(ActorKind::Agent, crate::pal::PAL_ACTOR)
                .expect("PAL_ACTOR is a non-empty constant"),
        }
    }

    /// Whether `actor` is this caller's own device stamp. A device's stamp has
    /// no `member` row, and it is the daemon's own value, so the workspace
    /// membership gate for caller-named actors does not apply to it.
    #[must_use]
    pub fn is_own_device_stamp(&self, actor: &ActorRef) -> bool {
        matches!(self, Self::Device { .. }) && *actor == self.stamp(None)
    }

    /// The same identity as a chat-bus sender: `operator`, `device:<id>`, or
    /// Pal's actor.
    #[must_use]
    pub fn sender(&self) -> String {
        match self {
            Self::Operator => "operator".to_string(),
            Self::Device { device_id, .. } => format!("device:{device_id}"),
            Self::Pal { .. } => crate::pal::PAL_ACTOR.to_string(),
        }
    }

    /// The paired device this connection is, if any.
    #[must_use]
    pub fn device_id(&self) -> Option<&str> {
        match self {
            Self::Operator | Self::Pal { .. } => None,
            Self::Device { device_id, .. } => Some(device_id),
        }
    }
}

tokio::task_local! {
    /// The caller of the request this task is serving, for the few effects
    /// that run frames below the handler (the ACP prompt path) and so cannot
    /// take the caller as an argument without threading it through every
    /// shared executor.
    static CURRENT: Caller;
}

/// Run `request` with `caller` as [`current_sender`]'s answer.
pub async fn serving<F: std::future::Future>(caller: Caller, request: F) -> F::Output {
    CURRENT.scope(caller, request).await
}

/// The sender stamp of the request this task is serving, or `None` outside a
/// request (a daemon-internal effect, which keeps its own attribution).
#[must_use]
pub fn current_sender() -> Option<String> {
    CURRENT.try_with(Caller::sender).ok()
}

/// Live Pal credentials: `sha256(plaintext) -> scope_key`.
///
/// ponytail: process-memory, not a table. A Pal token is only useful to the
/// tool-server process the daemon spawned through an ACP adapter it owns, and
/// that process dies with the daemon — so a credential that does not survive a
/// restart cannot strand anything. Move it into the store if the tool server
/// ever outlives its daemon.
static PAL_TOKENS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn pal_tokens() -> MutexGuard<'static, HashMap<String, String>> {
    PAL_TOKENS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Mint the credential Pal's tool server presents, bound to `scope_key`.
///
/// Returns the PLAINTEXT, which the caller writes to a `0600` file and nowhere
/// else. Any previous credential for the same scope is revoked here: a session
/// is re-configured on `session/load`, and the adapter holding the old token is
/// already gone.
#[must_use]
pub fn mint_pal_token(scope_key: &str) -> String {
    let minted = mint(TokenKind::Daemon, &mut rand::rngs::OsRng);
    let mut tokens = pal_tokens();
    tokens.retain(|_, bound| bound != scope_key);
    tokens.insert(minted.sha256_hex, scope_key.to_string());
    minted.plaintext
}

/// The scope a presented token is bound to, when it is a Pal credential.
fn pal_scope_for(token: &str) -> Option<String> {
    pal_tokens().get(&sha256_hex(token)).cloned()
}

/// Ensure a valid socket-auth credential exists, returning the token file path.
///
/// Reuses the existing credential when the database digest and the on-disk
/// plaintext still agree (so daemon restarts do not invalidate connected
/// clients' token files); otherwise mints a fresh token, stores its digest,
/// and (re)writes the plaintext file with `0600` permissions.
///
/// # Errors
///
/// Returns an error when the store read/write fails or the token file cannot
/// be written.
pub async fn ensure_socket_token(pool: &SqlitePool, hangar_home: &Path) -> anyhow::Result<PathBuf> {
    let path = ainb_hangar_proto::auth::token_file_in(hangar_home);

    // Reuse: both halves present and still matching.
    if let Some(stored) = SocketTokenRepo::get(pool).await? {
        if let Ok(existing) = std::fs::read_to_string(&path) {
            if ainb_hangar_core::token::verify(existing.trim(), &stored) {
                return Ok(path);
            }
        }
    }

    // Mint fresh: either half missing (or drifted) makes the pair unusable —
    // the plaintext is unrecoverable from the digest, so replace both.
    let minted = mint(TokenKind::Daemon, &mut rand::rngs::OsRng);
    SocketTokenRepo::set(pool, &minted.sha256_hex, SystemClock.now_ms()).await?;
    write_token_file(&path, &minted.plaintext)?;
    Ok(path)
}

/// Write the plaintext token to `path` with `0600` permissions.
///
/// The file is created fresh (any previous file is removed first) so the mode
/// is applied at create time and never widened by a pre-existing file's perms.
///
/// # Errors
///
/// Propagates the create/write failure.
pub fn write_token_file(path: &Path, plaintext: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    writeln!(f, "{plaintext}")?;
    Ok(())
}

/// `true` when the connection's kernel-reported peer uid matches this
/// process's uid. Covers `SO_PEERCRED` (Linux) and `LOCAL_PEERCRED`/
/// `getpeereid` (macOS) through tokio's one cross-platform call.
///
/// # Errors
///
/// Propagates the `getsockopt` failure. Callers must go through
/// [`PeerGate::classify`] rather than collapsing that error into `false`: a
/// credential the daemon could not READ is a different fact from a credential
/// that names someone else, and only one of them is worth an operator's
/// attention.
pub fn same_uid_peer(stream: &tokio::net::UnixStream) -> std::io::Result<bool> {
    let cred = stream.peer_cred()?;
    Ok(cred.uid() == nix::unistd::Uid::current().as_raw())
}

/// What the peer-credential gate concluded about one accepted connection.
///
/// Three outcomes, not two, because the gate has always had three and the
/// missing one was being reported as the alarming one. `same_uid_peer(..)
/// .unwrap_or(false)` folded a FAILED credential read into "foreign uid", so
/// the daemon logged an intrusion-shaped warning for a case that carries no
/// identity claim at all.
///
/// On macOS that case is not exotic, it is the norm. tokio's `peer_cred` reads
/// `LOCAL_PEEREPID` first, and that `getsockopt` returns `ENOTCONN` the instant
/// the peer disconnects — so every connect-and-drop liveness probe (the shape
/// `socket_is_listening` uses to ask "is anyone accepting?") loses the race
/// against the accepting task and is charged as a foreign-uid peer. A real
/// mismatch then hides inside thousands of lines describing something that
/// never happened.
///
/// All three outcomes still fail CLOSED: only [`Self::SameUid`] is served.
/// This type changes what the daemon SAYS, never what it allows.
#[derive(Debug)]
pub enum PeerGate {
    /// The kernel named this process's own uid. Serve the connection.
    SameUid,
    /// The kernel named a DIFFERENT uid. A genuine refusal, and the only
    /// outcome that deserves a warning.
    ForeignUid,
    /// The credentials could not be read, so the peer's uid was never
    /// established. Closed unverified — but not accused of anything.
    Unreadable(std::io::Error),
}

impl PeerGate {
    /// Map a [`same_uid_peer`] read onto the three outcomes. Split out from
    /// [`classify_peer`] so the mapping is testable without a socket pair.
    #[must_use]
    pub fn classify(read: std::io::Result<bool>) -> Self {
        match read {
            Ok(true) => Self::SameUid,
            Ok(false) => Self::ForeignUid,
            Err(e) => Self::Unreadable(e),
        }
    }
}

/// Read one accepted connection's peer credentials and classify them.
#[must_use]
pub fn classify_peer(stream: &tokio::net::UnixStream) -> PeerGate {
    PeerGate::classify(same_uid_peer(stream))
}

/// Validate a connection's first frame: it must be a well-formed `auth/hello`
/// whose token verifies against the stored digest, or against a live Pal
/// credential.
///
/// Returns `Ok((ack, authenticated))` — the `{}` success envelope to write
/// back plus WHO the connection is and its optional surface metadata — or an
/// error envelope which the caller writes before closing the connection.
pub async fn authenticate_first_frame(
    pool: &SqlitePool,
    body: &[u8],
) -> Result<(RpcResponse, AuthenticatedHello), RpcResponse> {
    let Ok(req) = serde_json::from_slice::<RpcRequest>(body) else {
        return Err(unauthorized(
            RpcId::Number(0),
            "first frame must be a well-formed auth/hello request",
        ));
    };
    if req.method != methods::AUTH_HELLO {
        return Err(unauthorized(
            req.id,
            "unauthenticated: first frame must be auth/hello with the daemon token",
        ));
    }
    let Ok(params) = serde_json::from_value::<HelloParams>(req.params.clone()) else {
        return Err(unauthorized(
            req.id,
            "auth/hello params must be { token, surface?, protocol?, capabilities?, device?, transient? }",
        ));
    };
    // D17: version before credential. A build this daemon cannot speak is not
    // an authentication failure and must not be reported as one: the remedy
    // is a different binary, never a different token, and a client that
    // conflates the two retries forever with a credential that was fine.
    let Some(selected) = negotiate(params.protocol, ProtocolRange::supported()) else {
        return Err(incompatible(req.id, params.protocol));
    };

    let settled = |caller: Caller| AuthenticatedHello {
        // A Pal connection is an agent-held socket the operator must always be
        // able to see, so it cannot opt out of the listing.
        transient: params.transient && matches!(caller, Caller::Operator),
        caller,
        // `mobile` is a peer-leg identity from a device credential; a local
        // process that claims it is recorded as `unknown`.
        surface: params.surface.clone().map(SurfaceInfo::for_local_leg),
        protocol: selected,
        capabilities: params.capabilities.clone(),
        device: params.device.clone(),
        host: params.host.map(SurfaceHost::for_local_leg),
    };

    // The Pal credential FIRST, and it is never the daemon token: a scoped
    // credential that also verified as the operator's would be no scope at all.
    if let Some(scope_key) = pal_scope_for(&params.token) {
        let host_id = minted_host_id(pool).await;
        return Ok((
            ack(req.id, selected, host_id),
            settled(Caller::Pal { scope_key }),
        ));
    }
    match SocketTokenRepo::verify(pool, &params.token).await {
        Ok(true) => {
            let host_id = minted_host_id(pool).await;
            Ok((ack(req.id, selected, host_id), settled(Caller::Operator)))
        }
        Ok(false) => Err(unauthorized(req.id, "invalid daemon token")),
        Err(e) => {
            tracing::warn!(error = %e, "hangar rpc: socket-token lookup failed");
            Err(unauthorized(req.id, "token verification unavailable"))
        }
    }
}

/// This daemon's minted `HostId` for an authenticated hello (#1066), or `None`
/// when it has none or the read fails: a hello never fails on the identity.
///
/// Read only after authentication, so an unauthenticated peer never learns it.
async fn minted_host_id(pool: &SqlitePool) -> Option<String> {
    match ainb_hangar_store::repo::daemon_identity::DaemonIdentityRepo::read(pool).await {
        Ok(identity) => identity.map(|identity| identity.host_id),
        Err(error) => {
            tracing::warn!(%error, "hangar rpc: daemon identity read failed");
            None
        }
    }
}

/// Test-only switch that added the sessions capability to hello while it was
/// dark.
///
/// P6d shipped the sessions table and its RPCs dark, and tests turned them on
/// here to drive the daemon path against the real handlers. Since the flip
/// (P6e-6) the capability is in the catalogue, so this switch adds nothing:
/// the catalogue is checked first and the id is never listed twice. Kept so
/// the tests written against it still read, and so a build that ever takes it
/// back out of the catalogue has its seam. It does not exist outside
/// `test`/`test-support` builds.
#[cfg(any(test, feature = "test-support"))]
static ADVERTISE_WORKSPACE_SESSIONS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Advertise (or stop advertising) the dark sessions capability on hello.
/// Process-wide; test builds only.
#[cfg(any(test, feature = "test-support"))]
pub fn advertise_workspace_sessions_for_tests(on: bool) {
    ADVERTISE_WORKSPACE_SESSIONS.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// The capabilities a hello reply carries: the catalogue, plus the sessions
/// capability when a test has switched it on and the catalogue does not
/// already carry it. Since the flip it always does, so the switch is a no-op
/// and no id is ever listed twice.
fn advertised_capabilities() -> Vec<String> {
    #[allow(unused_mut)]
    let mut capabilities = catalogue_strings();
    #[cfg(any(test, feature = "test-support"))]
    if !ainb_hangar_proto::protocol::advertises(ainb_hangar_proto::protocol::CAP_WORKSPACE_SESSIONS)
        && (ADVERTISE_WORKSPACE_SESSIONS.load(std::sync::atomic::Ordering::SeqCst)
            || std::env::var("AINB_TEST_WORKSPACE_SESSIONS").is_ok_and(|v| v == "1"))
    {
        capabilities.push(ainb_hangar_proto::protocol::CAP_WORKSPACE_SESSIONS.to_string());
    }
    capabilities
}

/// The success envelope echoing `id`, carrying what this daemon speaks.
///
/// A pre-W0-wire client deserializes this as the empty struct it always did
/// (serde ignores members it does not know), so the added members cost that
/// half of the skew matrix nothing.
fn ack(id: RpcId, selected: u32, host_id: Option<String>) -> RpcResponse {
    let result = HelloResult {
        protocol: ProtocolRange::supported(),
        selected: Some(selected),
        capabilities: advertised_capabilities(),
        daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        host_id,
        // The unix leg is never a device; the peer leg fills these (R1).
        scope: None,
        device_expires_at_ms: None,
    };
    RpcResponse {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id,
        result: serde_json::to_value(result).ok(),
        error: None,
    }
}

/// Refuse a connection whose protocol range does not overlap this build's.
///
/// The message names the fix, because the operator reading it is looking at
/// two binaries and needs to know which one to move.
fn incompatible(id: RpcId, client: ProtocolRange) -> RpcResponse {
    let ours = ProtocolRange::supported();
    RpcResponse {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id,
        result: None,
        error: Some(RpcError {
            code: PROTOCOL_INCOMPATIBLE,
            message: format!(
                "daemon protocol {}-{} cannot serve a client speaking {}-{}; \
                 restart from the newer binary",
                ours.min, ours.max, client.min, client.max
            ),
            data: serde_json::to_value(HelloResult {
                protocol: ours,
                selected: None,
                capabilities: catalogue_strings(),
                daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                // Never before authentication (#1066).
                host_id: None,
                scope: None,
                device_expires_at_ms: None,
            })
            .ok(),
        }),
    }
}

/// Build an `UNAUTHORIZED` error envelope echoing `id`.
fn unauthorized(id: RpcId, message: &str) -> RpcResponse {
    RpcResponse {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id,
        result: None,
        error: Some(RpcError {
            code: UNAUTHORIZED,
            message: message.to_string(),
            data: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P6e-6, the flip: a daemon's hello advertises the sessions capability,
    /// from the catalogue, exactly once. The switch that used to add it is a
    /// no-op now, and a second copy of the id would be a hello that names the
    /// same capability twice.
    #[test]
    fn hello_advertises_the_sessions_capability_once() {
        let cap = ainb_hangar_proto::protocol::CAP_WORKSPACE_SESSIONS;
        advertise_workspace_sessions_for_tests(true);
        let advertised = advertised_capabilities();
        assert_eq!(
            advertised.iter().filter(|c| *c == cap).count(),
            1,
            "the sessions capability is not advertised exactly once"
        );
        advertise_workspace_sessions_for_tests(false);
        assert!(advertised_capabilities().iter().any(|c| c == cap));
    }
    use ainb_hangar_store::Store;

    /// The gate's three outcomes stay three. The regression this pins is the
    /// old `same_uid_peer(..).unwrap_or(false)`, which collapsed a failed
    /// credential READ onto the same branch as a uid that named someone else —
    /// so the daemon warned about a foreign-uid peer for a connection that had
    /// made no identity claim at all.
    #[test]
    fn a_credential_read_fault_is_not_a_foreign_uid() {
        let fault = std::io::Error::from(std::io::ErrorKind::NotConnected);
        assert!(
            matches!(PeerGate::classify(Err(fault)), PeerGate::Unreadable(_)),
            "an unreadable credential must not be reported as a foreign uid"
        );
    }

    /// The real refusal still reads as a refusal — the fix must not soften the
    /// one case an operator is meant to see.
    #[test]
    fn a_uid_mismatch_is_still_a_foreign_uid() {
        assert!(matches!(
            PeerGate::classify(Ok(false)),
            PeerGate::ForeignUid
        ));
    }

    /// Only our own uid is served.
    #[test]
    fn our_own_uid_is_served() {
        assert!(matches!(PeerGate::classify(Ok(true)), PeerGate::SameUid));
    }

    /// macOS only, because this is a macOS kernel behaviour: tokio's
    /// `peer_cred` reads `LOCAL_PEEREPID` before `getpeereid`, and that
    /// `getsockopt` fails `ENOTCONN` once the peer has disconnected. Linux's
    /// `SO_PEERCRED` latches the credentials and keeps answering, so there is
    /// nothing to assert there.
    ///
    /// This is the live case: a probe from THIS uid that hangs up before the
    /// accepting task reads its credentials. It must classify as unreadable,
    /// never as a foreign uid.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_same_uid_peer_that_hung_up_first_is_unreadable_not_foreign() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gate.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();

        let client = tokio::net::UnixStream::connect(&path).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        // While the peer is still there its uid reads fine: this is us.
        assert!(
            matches!(classify_peer(&server), PeerGate::SameUid),
            "a live same-uid peer must be served"
        );

        drop(client);
        // The disconnect is what breaks the credential read; give the kernel a
        // moment to tear the pair down before asking again.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        match classify_peer(&server) {
            PeerGate::Unreadable(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::NotConnected, "{e}");
            }
            other => panic!("a same-uid peer that hung up was classified {other:?}"),
        }
    }

    /// `ensure_socket_token` mints once and is then stable across calls: the
    /// digest in the database matches the sha256 of the on-disk plaintext, the
    /// file is `0600`, and a second call reuses (not replaces) the credential.
    #[tokio::test]
    async fn ensure_mints_once_then_reuses() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();

        let path = ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        assert_eq!(path, dir.path().join("hangar").join("daemon.token"));

        let plaintext = std::fs::read_to_string(&path).unwrap().trim().to_string();
        assert!(plaintext.starts_with("mdt_"), "{plaintext}");
        let stored = SocketTokenRepo::get(store.pool()).await.unwrap().unwrap();
        assert_eq!(stored, ainb_hangar_core::token::sha256_hex(&plaintext));

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be owner-only");

        // Second call: same plaintext survives (no re-mint).
        ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let again = std::fs::read_to_string(&path).unwrap().trim().to_string();
        assert_eq!(
            again, plaintext,
            "a valid pair must be reused, not replaced"
        );
    }

    /// A Pal credential authenticates as the Pal, bound to its scope,
    /// and the daemon's own token still authenticates as the operator.
    #[tokio::test]
    async fn a_pal_token_authenticates_as_pal_and_not_the_operator() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let path = ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let daemon = std::fs::read_to_string(&path).unwrap().trim().to_string();

        let pal = mint_pal_token("channel:01J0COPILOT");
        assert_ne!(
            pal, daemon,
            "Pal must not be handed the daemon's credential"
        );

        let hello = |token: &str| {
            serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": methods::AUTH_HELLO,
                "params": { "token": token }
            }))
            .unwrap()
        };
        let (_, authenticated) = authenticate_first_frame(store.pool(), &hello(&pal))
            .await
            .expect("the Pal credential must authenticate");
        assert_eq!(
            authenticated.caller,
            Caller::Pal {
                scope_key: "channel:01J0COPILOT".to_string()
            }
        );
        let (_, authenticated) = authenticate_first_frame(store.pool(), &hello(&daemon))
            .await
            .expect("the daemon token still authenticates");
        assert_eq!(authenticated.caller, Caller::Operator);

        // A re-mint for the same scope REVOKES the previous credential: the
        // adapter holding it is already gone.
        let replacement = mint_pal_token("channel:01J0COPILOT");
        assert_ne!(replacement, pal);
        assert!(
            authenticate_first_frame(store.pool(), &hello(&pal)).await.is_err(),
            "a revoked Pal credential still authenticated"
        );
    }

    /// A local hello cannot label itself a phone: `mobile`, as the surface or
    /// as a plugin's host, is recorded as `unknown` on the unix leg.
    #[tokio::test]
    async fn a_local_hello_cannot_declare_the_mobile_surface() {
        use ainb_hangar_proto::connections::SurfaceKind;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let path = ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let daemon = std::fs::read_to_string(&path).unwrap().trim().to_string();
        let hello = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": methods::AUTH_HELLO,
            "params": {
                "token": daemon,
                "surface": {"kind": "mobile", "pid": 42},
                "host": {"kind": "mobile", "pid": 7},
            }
        }))
        .unwrap();
        let (_, authenticated) = authenticate_first_frame(store.pool(), &hello)
            .await
            .expect("the operator token authenticates");
        let surface = authenticated.surface.expect("surface kept");
        assert_eq!(surface.kind, SurfaceKind::Unknown);
        assert_eq!(surface.pid, 42);
        assert_eq!(
            authenticated.host.map(|h| h.kind),
            Some(SurfaceKind::Unknown)
        );
    }

    /// `transient` hides a call connection from the registry listing (#963).
    /// An operator may ask for that; a Pal connection may not, because the
    /// listing is how an operator sees agent-held sockets.
    #[tokio::test]
    async fn only_an_operator_connection_can_be_transient() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let path = ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let daemon = std::fs::read_to_string(&path).unwrap().trim().to_string();
        let pal = mint_pal_token("channel:01J0TRANSIENT");

        let hello = |token: &str| {
            serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": methods::AUTH_HELLO,
                "params": { "token": token, "transient": true }
            }))
            .unwrap()
        };
        let (_, operator) = authenticate_first_frame(store.pool(), &hello(&daemon))
            .await
            .expect("operator authenticates");
        assert!(operator.transient);
        let (_, pal) = authenticate_first_frame(store.pool(), &hello(&pal))
            .await
            .expect("Pal authenticates");
        assert!(!pal.transient, "a Pal connection must stay listed");
    }

    /// #1066: an authenticated hello names the daemon's minted host, a daemon
    /// with no identity names none, and a refused hello never names it.
    #[tokio::test]
    async fn an_authenticated_hello_names_the_minted_host_and_a_refused_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let path = ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let daemon = std::fs::read_to_string(&path).unwrap().trim().to_string();
        let hello = |token: &str| {
            serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": methods::AUTH_HELLO,
                "params": { "token": token }
            }))
            .unwrap()
        };
        let host_of = |response: &RpcResponse| {
            let value = response
                .result
                .clone()
                .or_else(|| response.error.as_ref().and_then(|error| error.data.clone()));
            value.and_then(|value| value.get("host_id").cloned())
        };

        let (unminted, _) = authenticate_first_frame(store.pool(), &hello(&daemon))
            .await
            .expect("operator authenticates");
        assert_eq!(host_of(&unminted), None, "no identity, no host");

        let minted = ainb_hangar_store::repo::daemon_identity::DaemonIdentityRepo::mint_or_read(
            store.pool(),
            &ainb_hangar_core::idgen::SystemIdGen,
            &ainb_hangar_core::clock::SystemClock,
        )
        .await
        .unwrap()
        .identity
        .host_id;
        let (ack, _) = authenticate_first_frame(store.pool(), &hello(&daemon))
            .await
            .expect("operator authenticates");
        assert_eq!(host_of(&ack), Some(serde_json::Value::String(minted)));

        let refused = authenticate_first_frame(store.pool(), &hello("mdt_wrong"))
            .await
            .expect_err("a wrong token is refused");
        assert_eq!(host_of(&refused), None, "never before authentication");
    }

    /// Pal's allowed method set is exactly the tool table's reach.
    /// Everything a card's own answer flows through is refused.
    #[test]
    fn a_pal_connection_cannot_answer_its_own_cards() {
        let pal = Caller::Pal {
            scope_key: "channel:01J0COPILOT".to_string(),
        };
        for allowed in PAL_METHODS {
            assert!(
                pal.authorize(allowed, &serde_json::Value::Null).is_ok(),
                "{allowed} must be reachable"
            );
        }
        for refused in [
            methods::FLEET_CONFIRM_ANSWER,
            methods::FLEET_CONFIRM_LIST,
            methods::FLEET_PAL_CONFIGURE,
            methods::FLEET_ACP_SESSION_CREATE,
            methods::FLEET_CHANNEL_CREATE,
            methods::FLEET_ACTION,
        ] {
            let error = pal
                .authorize(refused, &serde_json::Value::Null)
                .expect_err("{refused} must be refused");
            assert_eq!(error.code, UNAUTHORIZED, "{refused}: {error:?}");
            // The operator's own surfaces are unaffected.
            assert!(Caller::Operator.authorize(refused, &serde_json::Value::Null).is_ok());
        }
    }

    /// A missing token file (digest present in the DB) forces a re-mint — the
    /// plaintext is unrecoverable from the digest alone.
    #[tokio::test]
    async fn missing_file_forces_remint() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();

        let path = ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let first = SocketTokenRepo::get(store.pool()).await.unwrap().unwrap();
        std::fs::remove_file(&path).unwrap();

        ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let second = SocketTokenRepo::get(store.pool()).await.unwrap().unwrap();
        assert_ne!(first, second, "a lost plaintext must rotate the digest");
        let plaintext = std::fs::read_to_string(&path).unwrap().trim().to_string();
        assert!(SocketTokenRepo::verify(store.pool(), &plaintext).await.unwrap());
    }

    fn device(scope: DeviceScope) -> Caller {
        Caller::Device {
            device_id: "01J0DEVICE0000000000000000".to_string(),
            scope,
        }
    }

    const DEVICE_SCOPES: [DeviceScope; 4] = [
        DeviceScope::MOBILE,
        DeviceScope::MOBILE_TYPE,
        DeviceScope::DESKTOP,
        DeviceScope::DESKTOP_ADMIN,
    ];

    /// R1-04's table test: for EVERY method const and every device scope, the
    /// daemon's gate says exactly what the frozen scope table says. No method
    /// falls through to a default, and the operator is still refused nothing.
    #[test]
    fn every_method_is_decided_by_the_scope_table_for_every_device_scope() {
        use ainb_hangar_proto::devices::method_allowed;
        for method in methods::ALL_METHODS {
            assert!(Caller::Operator.authorize(method, &serde_json::Value::Null).is_ok());
            for scope in DEVICE_SCOPES {
                let expected = method_allowed(&scope, method, &CallParams::Untyped);
                let got = device(scope).authorize(method, &serde_json::Value::Null);
                assert_eq!(
                    got.is_ok(),
                    expected,
                    "{method} for {scope:?}: gate {got:?}, table {expected}"
                );
                if let Err(refusal) = got {
                    assert_eq!(refusal.code, UNAUTHORIZED);
                }
            }
        }
    }

    /// RECONCILED S4: a phone may send `fleet/action` only as an interrupt,
    /// judged on the TYPED action. A kill, or params that do not parse as
    /// `FleetActionParams`, never earn the interrupt-only grant.
    #[test]
    fn a_phone_may_only_interrupt_and_only_on_typed_params() {
        let action = |action: serde_json::Value| {
            let mut params = serde_json::json!({
                "session_key": "s1",
                "expected_version": 1,
                "request_id": "r1",
            });
            params.as_object_mut().unwrap().extend(action.as_object().unwrap().clone());
            params
        };
        // The action is nested: `FleetActionParams.action` is a
        // `ControlAction` internally tagged on its own `action` field.
        let interrupt = action(serde_json::json!({"action": {"action": "interrupt"}}));
        let kill = action(serde_json::json!({"action": {"action": "kill"}}));
        // The flat spelling a raw-JSON check would have read as an interrupt.
        let untyped = action(serde_json::json!({"action": "interrupt"}));

        for phone in [DeviceScope::MOBILE, DeviceScope::MOBILE_TYPE] {
            let phone = device(phone);
            assert!(phone.authorize(methods::FLEET_ACTION, &interrupt).is_ok());
            assert!(phone.authorize(methods::FLEET_ACTION, &kill).is_err());
            assert!(
                phone.authorize(methods::FLEET_ACTION, &untyped).is_err(),
                "params that do not parse earn no params rule"
            );
        }
        let desktop = device(DeviceScope::DESKTOP);
        assert!(desktop.authorize(methods::FLEET_ACTION, &kill).is_ok());
    }

    /// RECONCILED S5: `terminal/attach` with `want_input` needs `mobile+type`.
    #[test]
    fn a_read_only_phone_attaches_to_watch_and_never_to_type() {
        let attach = |want_input: bool| {
            serde_json::json!({
                "session": {"host_id": "local", "session_key": "s1"},
                "want_input": want_input,
            })
        };
        let mobile = device(DeviceScope::MOBILE);
        assert!(mobile.authorize(methods::TERMINAL_ATTACH, &attach(false)).is_ok());
        assert!(mobile.authorize(methods::TERMINAL_ATTACH, &attach(true)).is_err());
        let typing = device(DeviceScope::MOBILE_TYPE);
        assert!(typing.authorize(methods::TERMINAL_ATTACH, &attach(true)).is_ok());
    }

    /// RECONCILED C6: `ConnectionsChanged` rides the attention stream, and
    /// only an admin (or the local operator) receives that family.
    #[test]
    fn only_admins_receive_the_connections_family() {
        assert!(Caller::Operator.receives(EventFamily::Connections));
        assert!(device(DeviceScope::DESKTOP_ADMIN).receives(EventFamily::Connections));
        for scope in [
            DeviceScope::DESKTOP,
            DeviceScope::MOBILE_TYPE,
            DeviceScope::MOBILE,
        ] {
            assert!(
                !device(scope).receives(EventFamily::Connections),
                "{scope:?}"
            );
            assert!(device(scope).receives(EventFamily::Attention), "{scope:?}");
        }
    }

    /// A subscribe method a scope may call must never open a stream whose
    /// family that scope may not receive; otherwise the event filter, not the
    /// method table, would be the only thing between a phone and the events.
    #[test]
    fn every_subscribe_a_scope_may_call_carries_families_it_receives() {
        let streams: [(&str, &[EventFamily]); 5] = [
            (
                methods::WORKSPACE_SUBSCRIBE,
                &[EventFamily::Workspace, EventFamily::TaskStream],
            ),
            (methods::ATTENTION_SUBSCRIBE, &[EventFamily::Attention]),
            (methods::FLEET_SUBSCRIBE, &[EventFamily::Fleet]),
            (
                methods::FLEET_MESSAGE_SUBSCRIBE,
                &[EventFamily::Message, EventFamily::Notification],
            ),
            (
                methods::FLEET_TRANSCRIPT_SUBSCRIBE,
                &[EventFamily::Transcript],
            ),
        ];
        for scope in DEVICE_SCOPES {
            let caller = device(scope);
            for (method, families) in streams {
                if caller.authorize(method, &serde_json::Value::Null).is_ok() {
                    for family in families {
                        assert!(
                            caller.receives(*family),
                            "{scope:?} may call {method} but not receive {family:?}"
                        );
                    }
                }
            }
        }
    }

    /// DECISIONS (PR-0 follow-up): a device grants only when it is admin, on
    /// top of `Grantor::may_grant`, and no device may change an admin device's
    /// scope, so one admin desktop cannot demote another. The operator can.
    #[test]
    fn only_an_admin_grants_and_no_device_rescopes_an_admin() {
        let admin = device(DeviceScope::DESKTOP_ADMIN);
        let phone = Some(DeviceScope::MOBILE_TYPE);
        assert!(admin.may_grant(DeviceScope::MOBILE, phone));
        assert!(admin.may_grant(DeviceScope::MOBILE_TYPE, None));
        assert!(
            !admin.may_grant(DeviceScope::DESKTOP, None),
            "DV5: desktop is operator-only"
        );
        assert!(!admin.may_grant(DeviceScope::DESKTOP_ADMIN, None));
        assert!(
            !admin.may_grant(DeviceScope::MOBILE, Some(DeviceScope::DESKTOP_ADMIN)),
            "an admin desktop may not demote another admin"
        );

        for scope in [
            DeviceScope::DESKTOP,
            DeviceScope::MOBILE_TYPE,
            DeviceScope::MOBILE,
        ] {
            assert!(
                !device(scope).may_grant(DeviceScope::MOBILE, phone),
                "{scope:?} is not admin and grants nothing"
            );
        }
        assert!(
            !Caller::Pal {
                scope_key: "channel:x".into()
            }
            .may_grant(DeviceScope::MOBILE, None)
        );
        for target in DEVICE_SCOPES {
            assert!(Caller::Operator.may_grant(target, Some(DeviceScope::DESKTOP_ADMIN)));
        }
    }

    /// RECONCILED S3: a unix-leg hello that carries `device` is still the
    /// operator. The claimed device id names no principal, no answer stamp and
    /// no actor.
    #[tokio::test]
    async fn a_unix_hello_that_claims_a_device_gains_no_device_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let path = ensure_socket_token(store.pool(), dir.path()).await.unwrap();
        let token = std::fs::read_to_string(&path).unwrap().trim().to_string();
        let hello = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": methods::AUTH_HELLO,
            "params": {
                "token": token,
                "device": {"device_id": "01J0IMPOSTOR00000000000000", "display_name": "phone"}
            }
        }))
        .unwrap();
        let (_, authenticated) = authenticate_first_frame(store.pool(), &hello)
            .await
            .expect("the operator token authenticates");
        assert_eq!(authenticated.caller, Caller::Operator);
        assert_eq!(authenticated.caller.device_id(), None);
        assert_eq!(
            crate::rpc::mutation::principal_of(&authenticated.caller),
            "local"
        );
        assert_eq!(
            crate::answer::answered_by_caller(&authenticated.caller, None),
            None
        );
    }

    /// Review follow-up A: the stamp. The operator keeps its claim or the
    /// local member; a device is always itself; the sender string matches.
    #[tokio::test]
    async fn the_stamp_pins_a_device_and_leaves_the_operator_alone() {
        let agent: ActorRef = "agent:agent-1".parse().unwrap();
        assert_eq!(Caller::Operator.stamp(Some(agent.clone())), agent);
        assert_eq!(
            Caller::Operator.stamp(None),
            ainb_hangar_core::actor::local_member()
        );
        let laptop = device(DeviceScope::DESKTOP);
        assert_eq!(
            laptop.stamp(Some(agent)).to_string(),
            "member:device:01J0DEVICE0000000000000000"
        );
        assert!(laptop.is_own_device_stamp(&laptop.stamp(None)));
        assert!(!Caller::Operator.is_own_device_stamp(&Caller::Operator.stamp(None)));
        assert_eq!(laptop.sender(), "device:01J0DEVICE0000000000000000");
        assert_eq!(Caller::Operator.sender(), "operator");

        // What the ACP SendPrompt path reads: the serving request's caller,
        // and nothing outside a request.
        assert_eq!(current_sender(), None);
        let inside = serving(laptop.clone(), async { current_sender() }).await;
        assert_eq!(inside.as_deref(), Some("device:01J0DEVICE0000000000000000"));
    }
}
