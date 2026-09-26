//! The uniffi facade: what the Expo app calls.
//!
//! Every method here maps a proto request to a [`crate::records`] record. The
//! op id of every mutation is minted here from the crate's CSPRNG, and a retry
//! passes the same id back so the ledger deduplicates it. No token and no key
//! crosses this boundary: pairing keeps them in custody and `connect_host`
//! takes a host id.
//!
//! Async methods hop onto the crate's own tokio runtime, because a uniffi
//! future is polled by the foreign executor and `tokio::spawn` needs a
//! runtime context.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use ainb_hangar_noise::PairingOffer;
use ainb_hangar_proto::agent_status::RosterStatusResult;
use ainb_hangar_proto::auth::{DeviceInfo, HelloParams, HelloResult};
use ainb_hangar_proto::fleet::{
    ControlAction, FleetActionParams, FleetActionResult, FleetMessageSendParams,
    FleetMessageSendResult, FleetSubscribeParams, FleetSubscribeResult, FleetTranscriptListParams,
    FleetTranscriptListResult, FleetTranscriptSubscribeParams, FleetTranscriptSubscribeResult,
};
use ainb_hangar_proto::hosts::HostId;
use ainb_hangar_proto::methods;
use ainb_hangar_proto::mutation::{ACK_KEY, Fence, MutationAck, MutationEnvelope, OpId};
use ainb_hangar_proto::protocol::{ProtocolRange, catalogue_strings};
use ainb_hangar_proto::snapshots::{
    AnswerParams, AnswerResult, AttentionListResult, AttentionSubscribeParams,
    AttentionSubscribeResult,
};
use tokio::runtime::Runtime;

use crate::connlog::{ConnLog, Entry, Event};
use crate::custody::{CustodyReport, DeviceKey};
use crate::pairing::{self, EndpointRecord, PairingRecord};
use crate::records::{
    AnswerReply, AttentionRecord, FleetSubscribeSummary, HelloSummary, InterruptReply,
    MutationReceipt, RosterSnapshot, SendPromptReply, TranscriptDecoder, TranscriptPage, WireError,
    WireEvent,
};
use crate::session::{Session, SessionEvent, SessionStats, backoff_delay, classify_close};
use crate::terminal::{
    self, ResizeOutcome, Streams, TerminalAttachRecord, TerminalFloorOutcome, TerminalInputOutcome,
};

fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("ainb-wire")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

/// Split a mutation result into its typed half and the ledger ack under
/// [`ACK_KEY`].
fn split_ack<R: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> Result<(R, Option<MutationReceipt>), WireError> {
    let ack = value
        .get(ACK_KEY)
        .cloned()
        .and_then(|a| serde_json::from_value::<MutationAck>(a).ok())
        .map(MutationReceipt::from);
    let typed = serde_json::from_value(value).map_err(WireError::protocol)?;
    Ok((typed, ack))
}

fn open_log(log_dir: &str) -> Result<Arc<ConnLog>, WireError> {
    ConnLog::open(Path::new(log_dir))
}

/// A fresh 128-bit op id from the crate's CSPRNG, for an app that mints
/// before it sends so a lost reply retries under the same id.
#[uniffi::export]
pub fn mint_op_id() -> String {
    OpId::from_bytes(rand::random::<[u8; 16]>()).as_str().to_owned()
}

/// The version string, for the log.
#[uniffi::export]
pub fn wire_version() -> String {
    format!(
        "ainb-wire-mobile {} ({})",
        env!("CARGO_PKG_VERSION"),
        ainb_hangar_noise::NOISE_PATTERN
    )
}

/// The reconnect delay before `attempt` (0-based), in milliseconds, noted
/// in the connection log under `log_dir` when one is given.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn backoff_delay_ms(attempt: u32, log_dir: Option<String>) -> u64 {
    let delay_ms = u64::try_from(backoff_delay(attempt).as_millis()).unwrap_or(u64::MAX);
    if let Some(log) = log_dir.and_then(|d| ConnLog::open(Path::new(&d)).ok()) {
        log.log(Event::Backoff { attempt, delay_ms });
    }
    delay_ms
}

/// One line of the connection log.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConnLogEntry {
    /// Wall clock, epoch ms.
    pub t_ms: i64,
    /// The event name (`connect`, `handshake`, `hello`, `close`, ...).
    pub event: String,
    /// The event's fields as JSON text.
    pub detail: String,
}

impl From<Entry> for ConnLogEntry {
    fn from(e: Entry) -> Self {
        let mut value = serde_json::to_value(&e.event).unwrap_or_default();
        let event = value
            .as_object_mut()
            .and_then(|o| o.remove("event"))
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        Self {
            t_ms: e.t_ms,
            event,
            detail: value.to_string(),
        }
    }
}

/// The last `limit` lines of the connection log under `log_dir`, oldest
/// first; works before any host is connected.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn read_connection_log(log_dir: String, limit: u32) -> Result<Vec<ConnLogEntry>, WireError> {
    let log = ConnLog::open(Path::new(&log_dir))?;
    Ok(log.tail(limit as usize).into_iter().map(ConnLogEntry::from).collect())
}

/// The fingerprint of the device key kept under `custody_dir`, minting the
/// key on first use. All the app ever sees of the key.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn device_key_fingerprint(custody_dir: String) -> Result<String, WireError> {
    DeviceKey::load_or_create(Path::new(&custody_dir)).map(|k| k.fingerprint())
}

/// The five latch strings the app mirrors (`HostRow.repair`, `HostRow.notice`):
/// `revoked`, `unauthenticated`, `peer_changed`, then `incompatible`,
/// `unknown_code`. The app compares its literals to these in a test and
/// keeps no copy of the meaning.
#[uniffi::export]
pub fn latch_values() -> Vec<String> {
    pairing::LATCH_VALUES.iter().map(|s| (*s).to_owned()).collect()
}

/// Which backend holds the device secrets on this target, and whether that
/// is the degraded (file) one.
#[uniffi::export]
pub fn custody_report() -> CustodyReport {
    CustodyReport::for_target()
}

/// What a pairing offer says, for the pair screen.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OfferSummary {
    /// The host, a ULID.
    pub host_id: String,
    /// Where to dial, in preference order.
    pub endpoints: Vec<EndpointRecord>,
    /// The invite id.
    pub invite_id: String,
    /// When the invite expires, epoch ms.
    pub expires_at_ms: i64,
}

/// Parse an `ainb://pair#…` offer. The secret stays inside the crate: the
/// app shows the host and the expiry and calls `pair` with the same URI.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn parse_offer(uri: String) -> Result<OfferSummary, WireError> {
    let offer = PairingOffer::parse(&uri).map_err(|e| WireError::Offer {
        message: e.to_string(),
    })?;
    Ok(OfferSummary {
        host_id: offer.host_id.as_str().to_owned(),
        endpoints: offer
            .endpoints
            .into_iter()
            .map(|e| EndpointRecord {
                carrier: e.carrier.as_str().to_owned(),
                url: e.url,
            })
            .collect(),
        invite_id: offer.invite_id,
        expires_at_ms: offer.expires_at_ms,
    })
}

/// Redeem an offer as `display_name`: dial, `device/redeem`, `auth/hello`,
/// then save the pairing (token in custody). The session is closed; the app
/// calls `connect_host` with the returned host id.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub async fn pair(
    uri: String,
    display_name: String,
    custody_dir: String,
    log_dir: String,
) -> Result<PairingRecord, WireError> {
    rt().spawn(async move {
        let log = open_log(&log_dir)?;
        let (record, _hello) =
            pairing::pair(&uri, &display_name, Path::new(&custody_dir), Some(log)).await?;
        Ok(record)
    })
    .await
    .map_err(WireError::protocol)?
}

/// Every paired host, oldest first.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn list_pairings(custody_dir: String) -> Result<Vec<PairingRecord>, WireError> {
    pairing::list(Path::new(&custody_dir))
}

/// Forget a pairing: the token and the record. Only an explicit user action
/// calls this (the owner removes a host). A refusal never forgets: it sets
/// `PairingRecord.repair` (or `notice`) and the app shows "re-pair" while
/// keeping the pairing.
#[uniffi::export]
#[allow(clippy::needless_pass_by_value)]
pub fn forget_pairing(custody_dir: String, host_id: String) -> Result<(), WireError> {
    pairing::forget(Path::new(&custody_dir), &host_id)
}

/// What `connect_host` needs: which pairing, and where the secrets and the
/// log live.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConnectParams {
    /// The paired host.
    pub host_id: String,
    /// The directory the device secrets are kept under.
    pub custody_dir: String,
    /// The directory the connection log is kept under.
    pub log_dir: String,
}

/// `auth/hello` on an open session, as a paired device.
pub(crate) async fn hello(
    session: &Session,
    token: &str,
    device_id: &str,
    display_name: &str,
) -> Result<HelloSummary, WireError> {
    let params = HelloParams {
        token: token.to_owned(),
        surface: None,
        protocol: ProtocolRange::supported(),
        capabilities: catalogue_strings(),
        device: Some(DeviceInfo {
            device_id: device_id.to_owned(),
            display_name: Some(display_name.to_owned()),
        }),
        transient: false,
        host: None,
    };
    let result: Result<HelloResult, WireError> = session.call(methods::AUTH_HELLO, &params).await;
    if let Some(log) = session.log() {
        match &result {
            Ok(r) => log.log(Event::Hello {
                device_id: device_id.to_owned(),
                protocol: r.selected_or_legacy(),
                scope: r.scope.map(|s| s.base().as_str().to_owned()),
            }),
            Err(e) => log.log(Event::HelloFailed {
                detail: e.to_string(),
            }),
        }
    }
    Ok(result?.into())
}

/// One connected, authenticated host.
#[derive(uniffi::Object)]
pub struct MobileHost {
    session: Arc<Session>,
    hello: HelloSummary,
    streams: Arc<Streams>,
    custody_dir: std::path::PathBuf,
    host_id: String,
    /// The daemon's transcript classifier, one per host (it remembers tool
    /// calls across rows).
    transcript: std::sync::Mutex<TranscriptDecoder>,
}

/// A refusal from the host goes on the pairing record: `repair` for an
/// identity refusal (peer changed, 4401, 4403), `notice` for an incompatible
/// or unknown close; only a successful pair clears them. Retryable closes,
/// including a network loss, set nothing: `Closed { retryable, retry_after_ms }`
/// stays the app's only source of truth for a redial.
fn latch_repair(custody_dir: &Path, host_id: &str, err: &WireError) {
    let _ = pairing::mark_refusal(custody_dir, host_id, err);
}

impl MobileHost {
    fn transcript(&self) -> std::sync::MutexGuard<'_, TranscriptDecoder> {
        self.transcript.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl std::fmt::Debug for MobileHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileHost")
            .field("host_id", &self.hello.host_id)
            .field("closed", &self.session.is_closed())
            .finish_non_exhaustive()
    }
}

/// Dial the paired host, handshake with its pinned key, and `auth/hello` as
/// the paired device. A 4401 or 4403 comes back as `Unauthenticated` or
/// `Revoked` and sets the re-pair latch on the pairing record; the app shows
/// "re-pair" and keeps the pairing until the user forgets it or pairs again.
#[uniffi::export]
pub async fn connect_host(params: ConnectParams) -> Result<Arc<MobileHost>, WireError> {
    rt().spawn(async move {
        let custody_dir = Path::new(&params.custody_dir);
        let not_paired = || WireError::NotPaired {
            host_id: params.host_id.clone(),
        };
        let record = pairing::find(custody_dir, &params.host_id)?.ok_or_else(not_paired)?;
        let token = pairing::token(custody_dir, &params.host_id)?.ok_or_else(not_paired)?;
        let host_id = HostId::parse_minted(&record.host_id).map_err(|e| WireError::Protocol {
            message: e.to_string(),
        })?;
        let host_static_pubkey = <[u8; 32]>::try_from(record.host_static_pubkey.as_slice())
            .map_err(|_| WireError::Protocol {
                message: format!(
                    "pinned host key is {} bytes, expected 32",
                    record.host_static_pubkey.len()
                ),
            })?;
        let key = DeviceKey::load_or_create(custody_dir)?;
        let log = open_log(&params.log_dir)?;
        let session = match pairing::dial(
            &record.endpoints,
            &host_id,
            host_static_pubkey,
            &key,
            Some(log),
        )
        .await
        {
            Ok(session) => session,
            Err(e) => {
                // A wrong pinned host key, or 4401 before message 2, is the
                // host refusing this device's identity: latch re-pair.
                latch_repair(custody_dir, &params.host_id, &e);
                return Err(e);
            }
        };
        let hello = match hello(&session, &token, &record.device_id, &record.display_name).await {
            Ok(hello) => {
                // The host and this build agree again: a parked notice
                // (incompatible, unknown code) is over. The re-pair latch is
                // not: only a successful pair clears that.
                pairing::clear_notice(custody_dir, &params.host_id)?;
                hello
            }
            Err(e) => {
                session.close();
                latch_repair(custody_dir, &params.host_id, &e);
                return Err(e);
            }
        };
        Ok(Arc::new(MobileHost {
            session,
            hello,
            streams: Arc::new(Streams::default()),
            custody_dir: custody_dir.to_path_buf(),
            host_id: params.host_id.clone(),
            transcript: std::sync::Mutex::new(TranscriptDecoder::default()),
        }))
    })
    .await
    .map_err(WireError::protocol)?
}

impl MobileHost {
    async fn roster_status_inner(&self) -> Result<RosterSnapshot, WireError> {
        let result: RosterStatusResult =
            self.session.call(methods::FLEET_ROSTER_STATUS, &serde_json::json!({})).await?;
        Ok(result.into())
    }
}

#[uniffi::export]
impl MobileHost {
    /// What hello said.
    pub fn hello(&self) -> HelloSummary {
        self.hello.clone()
    }

    /// Whether the daemon advertised `capability`.
    #[allow(clippy::needless_pass_by_value)]
    pub fn advertises(&self, capability: String) -> bool {
        self.hello.capabilities.contains(&capability)
    }

    /// The D14 "one truth" read: every visible session with its status.
    pub async fn roster_status(self: Arc<Self>) -> Result<RosterSnapshot, WireError> {
        rt().spawn(async move { self.roster_status_inner().await })
            .await
            .map_err(WireError::protocol)?
    }

    /// Subscribe to Fleet revisions after `after_revision`; the events then
    /// arrive through `next_event`.
    pub async fn subscribe_fleet(
        self: Arc<Self>,
        after_revision: i64,
    ) -> Result<FleetSubscribeSummary, WireError> {
        rt().spawn(async move {
            let result: FleetSubscribeResult = self
                .session
                .call(
                    methods::FLEET_SUBSCRIBE,
                    &FleetSubscribeParams { after_revision },
                )
                .await?;
            Ok(result.into())
        })
        .await
        .map_err(WireError::protocol)?
    }

    /// The open attention rows, oldest first.
    pub async fn attention_list(self: Arc<Self>) -> Result<Vec<AttentionRecord>, WireError> {
        rt().spawn(async move {
            let result: AttentionListResult =
                self.session.call(methods::ATTENTION_LIST, &serde_json::json!({})).await?;
            Ok(result.attention.into_iter().map(Into::into).collect())
        })
        .await
        .map_err(WireError::protocol)?
    }

    /// Open the fleet-wide attention stream: the open snapshot now, then
    /// `AttentionRaised` / `AttentionAnswered` through `next_event`.
    pub async fn subscribe_attention(self: Arc<Self>) -> Result<Vec<AttentionRecord>, WireError> {
        rt().spawn(async move {
            let result: AttentionSubscribeResult = self
                .session
                .call(
                    methods::ATTENTION_SUBSCRIBE,
                    &AttentionSubscribeParams { workspace_id: None },
                )
                .await?;
            Ok(result.attention.into_iter().map(Into::into).collect())
        })
        .await
        .map_err(WireError::protocol)?
    }

    /// Answer one attention row, fenced on the `version` the app read.
    ///
    /// `op_id` is minted by the app with `mint_op_id` BEFORE the send and
    /// kept until a reply arrives, so a retry after a lost reply sends the
    /// same id and the daemon replays it rather than delivering twice.
    pub async fn answer(
        self: Arc<Self>,
        attention_id: String,
        answer: String,
        version: i64,
        op_id: String,
    ) -> Result<AnswerReply, WireError> {
        rt().spawn(async move {
            let op = OpId::parse(op_id).map_err(|e| WireError::Protocol { message: e })?;
            let params = AnswerParams {
                attention_id,
                answer,
                // The daemon stamps `device:<id>` from the credential (T11);
                // this is what a daemon without that arm records.
                answered_by: "mobile".to_owned(),
                is_answer: true,
                mutation: MutationEnvelope::fenced(op.clone(), Fence::AttentionVersion { version }),
            };
            let value = self
                .session
                .request(
                    methods::ATTENTION_ANSWER,
                    serde_json::to_value(params).map_err(WireError::protocol)?,
                )
                .await?;
            let (outcome, ack): (AnswerResult, _) = split_ack(value)?;
            Ok(AnswerReply {
                op_id: op.as_str().to_owned(),
                outcome: outcome.into(),
                ack,
            })
        })
        .await
        .map_err(WireError::protocol)?
    }

    /// Send a prompt to one session, fenced on the lifecycle clock the app
    /// read. A stale clock comes back as `Rpc { reason: "turn_advanced" }`.
    /// `op_id` is minted by the app with `mint_op_id` before the send and
    /// reused on a retry.
    pub async fn send_prompt(
        self: Arc<Self>,
        session_key: String,
        text: String,
        lifecycle_updated_at: i64,
        op_id: String,
    ) -> Result<SendPromptReply, WireError> {
        rt().spawn(async move {
            let op = OpId::parse(op_id).map_err(|e| WireError::Protocol { message: e })?;
            let params = FleetMessageSendParams {
                scope_key: None,
                // The daemon pins the actor to `device:<id>` (T12).
                actor: None,
                targets: vec![session_key],
                origin_message_id: None,
                text,
                request_id: op.as_str().to_owned(),
                mutation: MutationEnvelope::fenced(
                    op.clone(),
                    Fence::LifecycleUpdatedAt {
                        lifecycle_updated_at,
                    },
                ),
            };
            let value = self
                .session
                .request(
                    methods::FLEET_MESSAGE_SEND,
                    serde_json::to_value(params).map_err(WireError::protocol)?,
                )
                .await?;
            let (result, ack): (FleetMessageSendResult, _) = split_ack(value)?;
            Ok(SendPromptReply {
                op_id: op.as_str().to_owned(),
                message_id: result.message_id,
                ack,
            })
        })
        .await
        .map_err(WireError::protocol)?
    }

    /// Interrupt the current turn: `fleet/action {interrupt}`, the only
    /// action the mobile scope may send, fenced on the process incarnation.
    /// `op_id` is minted by the app with `mint_op_id` before the send and
    /// reused on a retry.
    pub async fn interrupt(
        self: Arc<Self>,
        session_key: String,
        version: i64,
        process_start_fingerprint: String,
        op_id: String,
    ) -> Result<InterruptReply, WireError> {
        rt().spawn(async move {
            let op = OpId::parse(op_id).map_err(|e| WireError::Protocol { message: e })?;
            let params = FleetActionParams {
                session_key,
                expected_version: version,
                request_id: op.as_str().to_owned(),
                action: ControlAction::Interrupt,
                mutation: MutationEnvelope::fenced(
                    op.clone(),
                    Fence::SessionIncarnation {
                        session_incarnation: process_start_fingerprint,
                    },
                ),
            };
            let value = self
                .session
                .request(
                    methods::FLEET_ACTION,
                    serde_json::to_value(params).map_err(WireError::protocol)?,
                )
                .await?;
            let (result, ack): (FleetActionResult, _) = split_ack(value)?;
            Ok(InterruptReply {
                op_id: op.as_str().to_owned(),
                receipt_status: serde_json::to_value(result.receipt.status)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                ack,
            })
        })
        .await
        .map_err(WireError::protocol)?
    }

    /// One transcript page after `after_order` (absent: the tail).
    pub async fn transcript_page(
        self: Arc<Self>,
        session_key: String,
        after_order: Option<i64>,
        limit: u32,
    ) -> Result<TranscriptPage, WireError> {
        rt().spawn(async move {
            let result: FleetTranscriptListResult = self
                .session
                .call(
                    methods::FLEET_TRANSCRIPT_LIST,
                    &FleetTranscriptListParams {
                        session_key,
                        after_order,
                        limit,
                    },
                )
                .await?;
            Ok(self.transcript().page(result))
        })
        .await
        .map_err(WireError::protocol)?
    }

    /// Tail a session's transcript after `after_order`; chunks arrive through
    /// `next_event`. Returns the head order, or absent when empty.
    pub async fn subscribe_transcript(
        self: Arc<Self>,
        session_key: String,
        after_order: Option<i64>,
    ) -> Result<Option<i64>, WireError> {
        rt().spawn(async move {
            let result: FleetTranscriptSubscribeResult = self
                .session
                .call(
                    methods::FLEET_TRANSCRIPT_SUBSCRIBE,
                    &FleetTranscriptSubscribeParams {
                        session_key,
                        after_order,
                    },
                )
                .await?;
            Ok(result.head_order)
        })
        .await
        .map_err(WireError::protocol)?
    }

    /// The next pushed event. After `Closed`, every call returns `Closed`.
    /// Terminal frames arrive decoded, and the crate acks them itself.
    pub async fn next_event(self: Arc<Self>) -> WireEvent {
        rt().spawn(async move {
            match self.session.next_event().await {
                SessionEvent::Notification(n) if n.method == methods::TERMINAL_FRAME => {
                    match terminal::on_frame(
                        Arc::clone(&self.session),
                        Arc::clone(&self.streams),
                        n.params,
                    ) {
                        Ok((stream_id, seq, frame)) => WireEvent::TerminalFrame {
                            stream_id,
                            seq,
                            frame,
                        },
                        // Only a frame that does not decode lands here; an
                        // ack failure is handled inside and the frame
                        // still arrives above.
                        Err(e) => WireEvent::Other {
                            method: format!("{}: {e}", methods::TERMINAL_FRAME),
                        },
                    }
                }
                SessionEvent::Notification(n) => {
                    WireEvent::from_notification(&n.method, n.params, &mut self.transcript())
                }
                SessionEvent::Lagged(dropped) => WireEvent::Lagged { dropped },
                SessionEvent::Closed { code, reason } => {
                    // The session's own record says whether this side closed
                    // on purpose (never retryable) or the host did.
                    let (retryable, retry_after_ms) = match self.session.closed() {
                        Some(closed) => {
                            let err = closed.error();
                            latch_repair(&self.custody_dir, &self.host_id, &err);
                            match err {
                                WireError::Closed {
                                    retryable,
                                    retry_after_ms,
                                    ..
                                } => (retryable, retry_after_ms),
                                _ => classify_close(code, &reason),
                            }
                        }
                        None => classify_close(code, &reason),
                    };
                    WireEvent::Closed {
                        code,
                        reason,
                        retryable,
                        retry_after_ms,
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|e| WireEvent::Closed {
            code: None,
            reason: e.to_string(),
            retryable: true,
            retry_after_ms: None,
        })
    }

    /// Close the socket. On background the app calls this after detaching
    /// its terminal, and reconnects with `after_revision` on foreground.
    pub fn close(&self) {
        self.session.close();
    }

    /// Whether the session is closed.
    pub fn is_closed(&self) -> bool {
        self.session.is_closed()
    }

    /// The counters, for the connection log.
    pub fn stats(&self) -> SessionStats {
        self.session.stats()
    }

    /// Whether this device may type: scope `mobile+type` or above AND the
    /// daemon advertises `terminal.input` (C-R2-8). Gates the type toggle.
    pub fn can_type(&self) -> bool {
        matches!(self.hello.scope.as_deref(), Some("mobile+type" | "desktop"))
            && self.hello.capabilities.iter().any(|c| c == "terminal.input")
    }

    /// `terminal/attach` for `session_key` on this host. With `want_input`
    /// and a free floor the attach takes the floor and becomes the resize
    /// owner (needs `mobile+type`).
    pub async fn terminal_attach(
        self: Arc<Self>,
        session_key: String,
        cols: Option<u16>,
        rows: Option<u16>,
        want_input: bool,
        scrollback_rows: Option<u32>,
    ) -> Result<TerminalAttachRecord, WireError> {
        rt().spawn(async move {
            let host_id = self
                .hello
                .host_id
                .as_deref()
                .ok_or_else(|| WireError::Protocol {
                    message: "the daemon did not name its host id at hello".to_owned(),
                })
                .and_then(|h| {
                    HostId::parse_minted(h).map_err(|e| WireError::Protocol {
                        message: e.to_string(),
                    })
                })?;
            terminal::attach(
                &self.session,
                &self.streams,
                host_id,
                session_key,
                cols,
                rows,
                want_input,
                scrollback_rows,
            )
            .await
        })
        .await
        .map_err(WireError::protocol)?
    }

    /// `terminal/detach`: releases the floor at once.
    pub async fn terminal_detach(self: Arc<Self>, stream_id: u64) -> Result<(), WireError> {
        rt().spawn(async move { terminal::detach(&self.session, &self.streams, stream_id).await })
            .await
            .map_err(WireError::protocol)?
    }

    /// Type `data` on `stream_id` under `floor_gen` (absent: acquire if
    /// free). Receipt tier: `op_id` is minted by the app with `mint_op_id`
    /// before the send and reused on a retry. A floor refusal comes back as
    /// a value.
    pub async fn terminal_input(
        self: Arc<Self>,
        stream_id: u64,
        data: Vec<u8>,
        floor_gen: Option<u64>,
        op_id: String,
    ) -> Result<TerminalInputOutcome, WireError> {
        rt().spawn(async move {
            let op = OpId::parse(op_id).map_err(|e| WireError::Protocol { message: e })?;
            terminal::input(
                &self.session,
                stream_id,
                floor_gen,
                &data,
                MutationEnvelope::with_op_id(op),
                split_ack,
            )
            .await
        })
        .await
        .map_err(WireError::protocol)?
    }

    /// `terminal/floor {acquire | release | take}` (dedupe tier); `op_id`
    /// minted by the app with `mint_op_id`.
    pub async fn terminal_floor(
        self: Arc<Self>,
        stream_id: u64,
        action: String,
        op_id: String,
    ) -> Result<TerminalFloorOutcome, WireError> {
        rt().spawn(async move {
            let action = terminal::floor_action(&action)?;
            let op = OpId::parse(op_id).map_err(|e| WireError::Protocol { message: e })?;
            terminal::floor(
                &self.session,
                stream_id,
                action,
                MutationEnvelope::with_op_id(op),
                split_ack,
            )
            .await
        })
        .await
        .map_err(WireError::protocol)?
    }

    /// `terminal/resize`: floor holder only, debounced by the daemon.
    pub async fn terminal_resize(
        self: Arc<Self>,
        stream_id: u64,
        cols: u16,
        rows: u16,
    ) -> Result<ResizeOutcome, WireError> {
        rt().spawn(async move { terminal::resize(&self.session, stream_id, cols, rows).await })
            .await
            .map_err(WireError::protocol)?
    }

    /// `terminal/scrollback`: `rows` rows above `before_row`, decoded.
    pub async fn terminal_scrollback(
        self: Arc<Self>,
        stream_id: u64,
        before_row: u64,
        rows: u32,
    ) -> Result<Vec<u8>, WireError> {
        rt().spawn(
            async move { terminal::scrollback(&self.session, stream_id, before_row, rows).await },
        )
        .await
        .map_err(WireError::protocol)?
    }

    /// How many `terminal/ack` calls were refused or lost since connect;
    /// each is also a line in the connection log.
    pub fn terminal_acks_failed(&self) -> u64 {
        self.streams.acks_failed()
    }

    /// The last `limit` lines of this host's connection log, oldest first.
    pub fn connection_log(&self, limit: u32) -> Vec<ConnLogEntry> {
        self.session
            .log()
            .map(|l| l.tail(limit as usize))
            .unwrap_or_default()
            .into_iter()
            .map(ConnLogEntry::from)
            .collect()
    }
}
