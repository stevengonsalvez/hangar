//! Agent hooks over loopback HTTP (hooks-and-answers).
//!
//! OFF unless [`LISTEN_ENV`] is set at boot (DV16: phase switches are boot-time
//! environment variables, never `daemon_config` keys). Unset means no bind, no
//! files, and no code on any request path.
//!
//! ```text
//! hook script ──POST /hook/<source>──▶ 127.0.0.1:<random> ──▶ HookSink
//!   reads hook-endpoint.env (port)        guard: Host, Origin,
//!   curl -H @hook-headers (token)         token, route, rate,
//!                                          type, size; then body
//! ```
//!
//! Security model: bound to `127.0.0.1` only; a fresh UUID token per daemon
//! start, compared in constant time before any body byte is read; the token
//! lives only in the 0600 headers file and never in argv or logs; `Host` must
//! be this listener and any `Origin` is refused (DNS rebinding, browsers);
//! head, body, connection count and request rate are all bounded. The token
//! does not defend against a process running as the same user, which can read
//! the file; that is the same boundary as the RPC socket token.

mod endpoint;
mod guard;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_proto::hooks::{
    HookSource, PANE_KEY_HEADER, PARENT_HEADER, PaneKey, TMUX_PANE_HEADER,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

pub use endpoint::EndpointFiles;
pub use guard::{Judge, MAX_BODY, MAX_CONNECTIONS, MAX_HEAD, Refusal, Route};

use crate::local_http::{read_body, read_head, write_response};

/// The boot switch. Set to `1` (or `true`) to bind the listener.
pub const LISTEN_ENV: &str = "AINB_HANGAR_HOOK_LISTEN";

/// Deadline for the request head.
pub const HEAD_DEADLINE: Duration = Duration::from_secs(2);
/// Deadline for the body once the head is admitted.
pub const BODY_DEADLINE: Duration = Duration::from_secs(5);

/// Whether the switch is on in this process's environment.
#[must_use]
pub fn enabled_from_env() -> bool {
    std::env::var(LISTEN_ENV).is_ok_and(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes"))
}

/// One hook call, as the listener hands it on.
#[derive(Debug, Clone)]
pub struct HookEvent {
    /// Which agent fired it.
    pub source: HookSource,
    /// `true` for the blocking route.
    pub hold: bool,
    /// `$AINB_PANE_KEY`, when present and well formed.
    pub pane_key: Option<PaneKey>,
    /// `$TMUX_PANE` as sent (`%N`), when present and well formed.
    pub tmux_pane: Option<String>,
    /// `$AINB_PARENT_SESSION`, when present.
    pub parent: Option<String>,
    /// The raw hook JSON; always an object.
    pub payload: serde_json::Value,
}

/// What the listener writes back for an admitted call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookReply {
    /// `204`: nothing for the hook to print.
    NoContent,
    /// `200` with a JSON body the hook prints to its agent.
    Json(Vec<u8>),
}

/// Where admitted hook calls go.
pub trait HookSink: Send + Sync + 'static {
    /// Take one call and say what to answer.
    fn ingest(
        &self,
        event: HookEvent,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HookReply> + Send + '_>>;
}

/// A sink that records nothing: the listener is reachable, and every call is
/// acknowledged and dropped. Ingest wiring replaces it.
#[derive(Debug, Default)]
pub struct DiscardSink;

impl HookSink for DiscardSink {
    fn ingest(
        &self,
        event: HookEvent,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HookReply> + Send + '_>> {
        tracing::debug!(source = event.source.as_str(), "hook ingress: discarded");
        Box::pin(async { HookReply::NoContent })
    }
}

/// A running listener. Dropping it stops accepting and removes the files.
#[derive(Debug)]
pub struct Running {
    addr: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
    _files: EndpointFiles,
}

impl Running {
    /// The bound port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The bound address; always `127.0.0.1`.
    #[must_use]
    pub const fn local_addr(&self) -> std::net::SocketAddr {
        self.addr
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Bind `127.0.0.1:0`, mint a token, publish the files, and serve.
///
/// # Errors
/// A bind or file-publish failure. The caller logs it; the daemon boots on.
pub async fn start(hangar_home: &Path, sink: Arc<dyn HookSink>) -> std::io::Result<Running> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    let port = addr.port();
    let token = uuid::Uuid::new_v4().to_string();
    let files = EndpointFiles::publish(hangar_home, port, &token)?;
    let judge = Arc::new(Judge::new(token, port));
    let task = tokio::spawn(serve(listener, judge, sink));
    Ok(Running {
        addr,
        task,
        _files: files,
    })
}

async fn serve(listener: TcpListener, judge: Arc<Judge>, sink: Arc<dyn HookSink>) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(error = %e, "hook ingress: accept failed");
                continue;
            }
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            let _ = write_response(&mut stream, 503, "text/plain", b"busy").await;
            continue;
        };
        let judge = judge.clone();
        let sink = sink.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle(&mut stream, &judge, &*sink).await {
                tracing::debug!(error = %e, "hook ingress: connection error");
            }
        });
    }
}

async fn handle(stream: &mut TcpStream, judge: &Judge, sink: &dyn HookSink) -> std::io::Result<()> {
    let head = match tokio::time::timeout(HEAD_DEADLINE, read_head(stream, MAX_HEAD)).await {
        Err(_) => return write_response(stream, 408, "text/plain", b"timeout").await,
        Ok(r) => match r? {
            Some(head) => head,
            None => return write_response(stream, 400, "text/plain", b"bad request").await,
        },
    };
    let route = match judge.admit(&head.method, &head.path, &head.headers) {
        Ok(route) => route,
        Err(Refusal { status, reason }) => {
            tracing::debug!(status, "hook ingress: refused");
            return write_response(stream, status, "text/plain", reason.as_bytes()).await;
        }
    };
    // curl sends `Expect: 100-continue` for larger bodies and would otherwise
    // wait a full second before sending it.
    if head
        .headers
        .get("expect")
        .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
    {
        use tokio::io::AsyncWriteExt as _;
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
    }
    let pane_key = head
        .headers
        .get(&PANE_KEY_HEADER.to_ascii_lowercase())
        .and_then(|v| PaneKey::parse(v).ok());
    let tmux_pane = head
        .headers
        .get(&TMUX_PANE_HEADER.to_ascii_lowercase())
        .filter(|v| is_tmux_pane_id(v))
        .cloned();
    let parent = head
        .headers
        .get(&PARENT_HEADER.to_ascii_lowercase())
        .filter(|v| !v.is_empty() && v.len() <= 256)
        .cloned();
    let body = match tokio::time::timeout(BODY_DEADLINE, read_body(stream, head, MAX_BODY)).await {
        Err(_) => return write_response(stream, 408, "text/plain", b"timeout").await,
        Ok(r) => match r? {
            Some(body) => body,
            None => return write_response(stream, 413, "text/plain", b"payload too large").await,
        },
    };
    let payload = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(v) if v.is_object() => v,
        _ => return write_response(stream, 400, "text/plain", b"body is not a JSON object").await,
    };
    let (source, hold) = match route {
        Route::Event(s) => (s, false),
        Route::Hold(s) => (s, true),
    };
    let event = HookEvent {
        source,
        hold,
        pane_key,
        tmux_pane,
        parent,
        payload,
    };
    match sink.ingest(event).await {
        HookReply::NoContent => write_response(stream, 204, "text/plain", b"").await,
        HookReply::Json(body) => write_response(stream, 200, "application/json", &body).await,
    }
}

/// `%` followed by 1 to 10 digits.
fn is_tmux_pane_id(v: &str) -> bool {
    v.strip_prefix('%')
        .is_some_and(|d| !d.is_empty() && d.len() <= 10 && d.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_ids_are_percent_digits() {
        assert!(is_tmux_pane_id("%0"));
        assert!(is_tmux_pane_id("%123"));
        for bad in ["", "%", "12", "%1a", "%-1", "%12345678901"] {
            assert!(!is_tmux_pane_id(bad), "{bad}");
        }
    }
}
