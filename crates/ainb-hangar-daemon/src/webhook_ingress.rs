//! The local HTTP webhook ingress for webhook-triggered autopilots (e38.18).
//!
//! A lightweight, hand-rolled HTTP/1.1 handler over a `tokio` [`TcpListener`]
//! **bound to `127.0.0.1` only** — never `0.0.0.0`. It serves exactly one route,
//! `POST /hangar/webhook/<autopilot_id>`, and on a correctly-signed request fires
//! that autopilot through the existing P7.4 enqueue path
//! ([`fire_autopilot_tick`]).
//!
//! # Security model (treated with e38.1 rigor)
//!
//! 1. **Localhost bind.** [`bind`] uses `127.0.0.1` so the ingress is never
//!    reachable off-host. There is no `0.0.0.0` path.
//! 2. **HMAC-SHA256 of the body.** Every request must carry an
//!    `X-Hangar-Signature` header that is `HMAC-SHA256(secret, body)`. The secret
//!    is the per-autopilot recoverable plaintext from the 0600
//!    [`WebhookSecretStore`]; the database holds only its digest. Verification is
//!    constant-time ([`ainb_hangar_core::webhook::verify_body_signature`]).
//! 3. **Reject by default.** An unsigned request, a wrong-signature request, a
//!    request for an autopilot with no secret / webhook disabled, or an unknown
//!    autopilot id fires **nothing** and returns 401 / 403 / 404. Only a
//!    signed-and-filtered request fires.
//! 4. **No secret in logs.** The handler never logs the secret or the request
//!    body; it logs the autopilot id and the outcome only.
//!
//! Every processed request — fired, filtered, or rejected — is recorded in the
//! `autopilot_webhook_delivery` audit log (the inspection surface the
//! `ainb hangar autopilot deliveries <id>` CLI verb reads).
//!
//! # Cross-platform
//!
//! Nothing here is OS-specific: `tokio::net` and a byte-level HTTP parse compile
//! identically on macOS and Linux. The 0600 secret-file permission is the only
//! unix-gated bit and lives in the store crate behind `#[cfg(unix)]`.

use std::collections::HashMap;
use std::sync::Arc;

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::ids::AutopilotId;
use ainb_hangar_core::webhook::{
    EVENT_HEADER, SIGNATURE_HEADER, event_passes_filter, verify_body_signature,
};
use ainb_hangar_store::repo::autopilot_run::fire_autopilot_tick;
use ainb_hangar_store::repo::autopilot_webhook::{
    AutopilotWebhookRepo, DeliveryOutcome, NewDelivery, WebhookSecretStore,
};
use sqlx::SqlitePool;
use tokio::net::{TcpListener, TcpStream};

use crate::local_http;

/// The outcome of processing one webhook request: the HTTP status to return plus
/// the structured delivery record that was logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookResult {
    /// The HTTP status the ingress returns (200 fired/filtered, 401/403/404 on a
    /// rejection).
    pub status: u16,
    /// The delivery outcome (`fired` / `filtered` / `rejected`).
    pub outcome: DeliveryOutcome,
    /// The fired run id, only when `outcome == Fired`.
    pub run_id: Option<String>,
    /// A short, secret-free reason (surfaced to the client + recorded).
    pub detail: &'static str,
}

/// Process one webhook request end-to-end (the security core).
///
/// Resolves the autopilot, constant-time-verifies the HMAC body signature,
/// applies the optional event filter, fires on success via the P7.4 enqueue
/// path, and records the delivery. Kept IO-narrow + free of HTTP parsing so an
/// HTTP-level test drives [`serve`] while a focused test can drive this directly.
///
/// `signature` is the `X-Hangar-Signature` header value (or `None` when absent);
/// `event` is the `X-Hangar-Event` header or the body's `event` field; `body` is
/// the exact request body bytes the HMAC is computed over.
///
/// Returns the [`WebhookResult`] (the status + what was logged). It never fires
/// unless the signature verifies AND the autopilot is webhook-enabled with a
/// secret set AND the event filter passes.
pub async fn process_webhook(
    pool: &SqlitePool,
    secrets: &WebhookSecretStore,
    clock: &dyn HangarClock,
    autopilot_id: &str,
    signature: Option<&str>,
    event: Option<&str>,
    body: &[u8],
) -> WebhookResult {
    let now = clock.now_ms();

    // Unknown / malformed id → 404, fire nothing. We cannot record a delivery
    // (no parent autopilot row for the FK), so a 404 is the whole response.
    let unknown = WebhookResult {
        status: 404,
        outcome: DeliveryOutcome::Rejected,
        run_id: None,
        detail: "unknown autopilot",
    };
    let Ok(id) = AutopilotId::from_str(autopilot_id.to_string()) else {
        return unknown;
    };
    let resolved = match AutopilotWebhookRepo::get_for_webhook(pool, &id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(autopilot_id = %id, error = %e, "webhook: resolve failed");
            return WebhookResult {
                status: 500,
                outcome: DeliveryOutcome::Rejected,
                run_id: None,
                detail: "resolve failed",
            };
        }
    };
    let Some((autopilot, config)) = resolved else {
        return unknown;
    };

    // From here a parent autopilot row exists, so every outcome records a
    // delivery. Decide the outcome, then audit it uniformly.
    let outcome = authorize_and_fire(
        pool,
        secrets,
        clock,
        &autopilot,
        &config,
        autopilot_id,
        signature,
        event,
        body,
    )
    .await;
    record_delivery(pool, &id, now, event, &outcome).await;
    outcome
}

/// The auth + fire decision for a *resolved* autopilot: enabled + secret-set
/// gate, constant-time HMAC verify, event filter, then fire. Returns the
/// [`WebhookResult`] without recording it (the caller audits every outcome).
#[allow(clippy::too_many_arguments)]
async fn authorize_and_fire(
    pool: &SqlitePool,
    secrets: &WebhookSecretStore,
    clock: &dyn HangarClock,
    autopilot: &ainb_hangar_store::repo::autopilot::Autopilot,
    config: &ainb_hangar_store::repo::autopilot_webhook::WebhookConfig,
    autopilot_id: &str,
    signature: Option<&str>,
    event: Option<&str>,
    body: &[u8],
) -> WebhookResult {
    let reject = |status: u16, detail: &'static str| WebhookResult {
        status,
        outcome: DeliveryOutcome::Rejected,
        run_id: None,
        detail,
    };

    // Webhook must be explicitly enabled AND carry a secret. A half-configured
    // autopilot is never firable over HTTP.
    if !config.enabled {
        tracing::info!(autopilot_id, "webhook: rejected (disabled)");
        return reject(403, "webhook disabled");
    }
    if config.secret_sha256.is_none() {
        tracing::info!(autopilot_id, "webhook: rejected (no secret)");
        return reject(403, "no secret set");
    }

    // The recoverable secret to recompute the HMAC over the body. Its absence
    // (config says a secret exists but the 0600 file lost it) is a server-side
    // misconfiguration, not an auth pass — reject.
    let secret = match secrets.get(autopilot_id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            tracing::warn!(
                autopilot_id,
                "webhook: secret digest set but plaintext missing"
            );
            return reject(403, "secret unavailable");
        }
        Err(e) => {
            tracing::warn!(autopilot_id, error = %e, "webhook: secret read failed");
            return reject(500, "secret read failed");
        }
    };

    // A missing signature header → 401. A present-but-wrong signature → 401.
    // Constant-time verify; never log the secret or signature.
    let Some(signature) = signature else {
        tracing::info!(autopilot_id, "webhook: rejected (no signature)");
        return reject(401, "missing signature");
    };
    if !verify_body_signature(secret.as_bytes(), body, signature) {
        tracing::info!(autopilot_id, "webhook: rejected (bad signature)");
        return reject(401, "invalid signature");
    }

    // Signature verified. Apply the optional event filter (exact match).
    if !event_passes_filter(config.event_filter.as_deref(), event) {
        tracing::info!(autopilot_id, "webhook: filtered (event mismatch)");
        return WebhookResult {
            status: 200,
            outcome: DeliveryOutcome::Filtered,
            run_id: None,
            detail: "event filtered",
        };
    }

    // Fire the existing P7.4 enqueue path.
    match fire_autopilot_tick(pool, clock, autopilot).await {
        Ok((run_id, task_id)) => {
            tracing::info!(autopilot_id, run_id = %run_id, task_id = %task_id, "webhook: fired");
            WebhookResult {
                status: 200,
                outcome: DeliveryOutcome::Fired,
                run_id: Some(run_id.to_string()),
                detail: "fired",
            }
        }
        Err(e) => {
            tracing::error!(autopilot_id, error = %e, "webhook: fire failed");
            reject(500, "fire failed")
        }
    }
}

/// Write one delivery audit row for a processed request (best-effort: a log
/// write failure is warned, never propagated — the HTTP response still returns).
async fn record_delivery(
    pool: &SqlitePool,
    id: &AutopilotId,
    now: i64,
    event: Option<&str>,
    result: &WebhookResult,
) {
    if let Err(e) = AutopilotWebhookRepo::record_delivery(
        pool,
        &NewDelivery {
            autopilot_id: id.clone(),
            received_at: now,
            outcome: result.outcome,
            event: event.map(str::to_string),
            http_status: result.status,
            run_id: result.run_id.clone(),
            detail: Some(result.detail.to_string()),
        },
    )
    .await
    {
        tracing::warn!(autopilot_id = %id, error = %e, "webhook: delivery log write failed");
    }
}

/// Bind the webhook ingress to `127.0.0.1:<port>` (pass `0` for an ephemeral
/// port). Returns the listener; the caller reads
/// [`local_addr`](TcpListener::local_addr) for the actual port.
///
/// **Localhost only**: the bind address is hardcoded `127.0.0.1`, never
/// `0.0.0.0`, so the ingress is unreachable off-host.
///
/// # Errors
///
/// Returns an [`std::io::Error`] when the bind fails (e.g. the port is taken).
pub async fn bind(port: u16) -> std::io::Result<TcpListener> {
    // Hardcoded loopback: the untrusted HTTP surface must never listen on a
    // routable interface.
    TcpListener::bind(("127.0.0.1", port)).await
}

/// Serve the webhook ingress on `listener` until the process exits.
///
/// Each accepted connection is handled on its own task. The handler parses one
/// HTTP/1.1 request, routes `POST /hangar/webhook/<id>`, and delegates the
/// security logic to [`process_webhook`]. Any other method/path is a 404.
pub async fn serve(
    listener: TcpListener,
    pool: SqlitePool,
    secrets: Arc<WebhookSecretStore>,
    clock: Arc<dyn HangarClock + Send + Sync>,
) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (mut stream, _peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(error = %e, "webhook ingress: accept failed");
                continue;
            }
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            let _ = write_response(&mut stream, 503, "busy").await;
            continue;
        };
        let pool = pool.clone();
        let secrets = secrets.clone();
        let clock = clock.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_connection(stream, &pool, &secrets, &*clock).await {
                tracing::debug!(error = %e, "webhook ingress: connection error");
            }
        });
    }
}

/// Handle one HTTP/1.1 connection: parse the request, route it, write the
/// response. Errors are connection-level (read/write/parse) and closed quietly.
async fn handle_connection(
    mut stream: TcpStream,
    pool: &SqlitePool,
    secrets: &WebhookSecretStore,
    clock: &dyn HangarClock,
) -> std::io::Result<()> {
    let req = match read_request(&mut stream).await? {
        ReadOutcome::Request(req) => req,
        // Malformed request line / headers, or over the cap: answer 400 and close.
        ReadOutcome::Malformed => {
            write_response(&mut stream, 400, "bad request").await?;
            return Ok(());
        }
        ReadOutcome::TimedOut => {
            write_response(&mut stream, 408, "request timeout").await?;
            return Ok(());
        }
    };

    // Route: POST /hangar/webhook/<id>. Anything else is a 404.
    let Some(autopilot_id) = req
        .path
        .strip_prefix("/hangar/webhook/")
        .filter(|rest| !rest.is_empty() && !rest.contains('/'))
    else {
        write_response(&mut stream, 404, "not found").await?;
        return Ok(());
    };
    if req.method != "POST" {
        write_response(&mut stream, 405, "method not allowed").await?;
        return Ok(());
    }

    let signature = req.headers.get(SIGNATURE_HEADER).map(String::as_str);
    // The event name: the X-Hangar-Event header, else the body JSON's `event`.
    let event_from_header = req.headers.get(EVENT_HEADER).map(String::as_str);
    let event_from_body = event_from_header.is_none().then(|| event_from_body(&req.body)).flatten();
    let event = event_from_header.or(event_from_body.as_deref());

    let result = process_webhook(
        pool,
        secrets,
        clock,
        autopilot_id,
        signature,
        event,
        &req.body,
    )
    .await;
    write_response(&mut stream, result.status, result.detail).await?;
    Ok(())
}

/// A parsed HTTP request: method, path, lower-cased headers, and the body.
struct ParsedRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// Upper bound on a webhook request (head + body). Webhook payloads are tiny;
/// 64 KiB is generous and caps a memory-exhaustion attempt.
const MAX_REQUEST: usize = 64 * 1024;

/// Read and parse one HTTP/1.1 request from `stream`, strictly (see
/// [`local_http`]). Malformed, over [`MAX_REQUEST`], or short is
/// [`ReadOutcome::Malformed`] (400); head or body slower than
/// [`REQUEST_DEADLINE`] is [`ReadOutcome::TimedOut`] (408).
async fn read_request(stream: &mut TcpStream) -> std::io::Result<ReadOutcome> {
    let head = tokio::time::timeout(REQUEST_DEADLINE, local_http::read_head(stream, MAX_REQUEST));
    let Ok(head) = head.await else {
        return Ok(ReadOutcome::TimedOut);
    };
    let Some(mut head) = head? else {
        return Ok(ReadOutcome::Malformed);
    };
    let remaining = MAX_REQUEST.saturating_sub(head.head_len);
    let body = tokio::time::timeout(
        REQUEST_DEADLINE,
        local_http::read_body(stream, &mut head, remaining),
    );
    let Ok(body) = body.await else {
        return Ok(ReadOutcome::TimedOut);
    };
    let Some(body) = body? else {
        return Ok(ReadOutcome::Malformed);
    };
    Ok(ReadOutcome::Request(ParsedRequest {
        method: head.method,
        path: head.path,
        headers: head.headers,
        body,
    }))
}

/// What reading one request produced.
enum ReadOutcome {
    /// A well-formed request.
    Request(ParsedRequest),
    /// Malformed, oversized, or short: answer 400.
    Malformed,
    /// Head or body did not arrive within [`REQUEST_DEADLINE`]: answer 408.
    TimedOut,
}

/// How long the head, and then the body, may take to arrive.
const REQUEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Most connections served at once; past this a connection gets 503.
pub const MAX_CONNECTIONS: usize = 64;

/// Extract a top-level `"event"` string from a JSON body, if present. A
/// non-JSON or event-less body yields `None` (the request simply has no event).
fn event_from_body(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value.get("event")?.as_str().map(str::to_string)
}

/// Write a minimal HTTP/1.1 response with a plain-text body and close.
async fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    local_http::write_response(stream, status, "text/plain", body.as_bytes()).await
}
