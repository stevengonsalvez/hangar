//! The hook listener's security model, driven over real loopback sockets.
//!
//! Each test starts a listener in a temp hangar home, reads the token the way a
//! hook script does (from the published headers file), and speaks raw HTTP/1.1
//! so every refusal is observed exactly as a client sees it.

use std::os::unix::fs::PermissionsExt as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_daemon::hook_ingress::{self, HookEvent, HookReply, HookSink};
use ainb_hangar_proto::hooks::{ENDPOINT_FILE_NAME, HookEndpoint, HookSource};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

#[derive(Default)]
struct Recorder(Mutex<Vec<HookEvent>>);

impl HookSink for Recorder {
    fn ingest(
        &self,
        event: HookEvent,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HookReply> + Send + '_>> {
        let hold = event.hold;
        self.0.lock().unwrap().push(event);
        Box::pin(async move {
            if hold {
                HookReply::Json(br#"{"ok":true}"#.to_vec())
            } else {
                HookReply::NoContent
            }
        })
    }
}

struct World {
    home: tempfile::TempDir,
    sink: Arc<Recorder>,
    running: hook_ingress::Running,
}

impl World {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let sink = Arc::new(Recorder::default());
        let running = hook_ingress::start(home.path(), sink.clone()).await.unwrap();
        Self {
            home,
            sink,
            running,
        }
    }

    fn endpoint(&self) -> HookEndpoint {
        let text =
            std::fs::read_to_string(self.home.path().join("hangar").join(ENDPOINT_FILE_NAME))
                .unwrap();
        HookEndpoint::parse_env_file(&text).unwrap()
    }

    fn token(&self) -> String {
        let line = std::fs::read_to_string(self.headers_path()).unwrap();
        line.trim_end().strip_prefix("X-Ainb-Hook-Token: ").unwrap().to_string()
    }

    fn headers_path(&self) -> std::path::PathBuf {
        self.home.path().join("hangar").join("hook-headers")
    }

    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.running.port())
    }

    fn events(&self) -> Vec<HookEvent> {
        self.sink.0.lock().unwrap().clone()
    }
}

/// Send `raw` and read the whole response (the server closes after one).
async fn exchange(port: u16, raw: &[u8]) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(raw).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out))
        .await
        .expect("server answered")
        .unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

fn post(path: &str, host: &str, extra: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn status(resp: &str) -> u16 {
    resp.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[tokio::test]
async fn a_good_event_is_ingested_with_its_pane_headers() {
    let w = World::start().await;
    let extra = format!(
        "X-Ainb-Hook-Token: {}\r\nX-Ainb-Pane-Key: v1:abc-123\r\nX-Ainb-Tmux-Pane: %7\r\nX-Ainb-Parent: parent-1\r\n",
        w.token()
    );
    let resp = exchange(
        w.running.port(),
        &post(
            "/hook/claude",
            &w.host(),
            &extra,
            r#"{"hook_event_name":"Stop"}"#,
        ),
    )
    .await;
    assert_eq!(status(&resp), 204, "{resp}");
    let events = w.events();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.source, HookSource::Claude);
    assert!(!e.hold);
    assert_eq!(e.pane_key.as_ref().map(|k| k.session_id()), Some("abc-123"));
    assert_eq!(e.tmux_pane.as_deref(), Some("%7"));
    assert_eq!(e.parent.as_deref(), Some("parent-1"));
    assert_eq!(e.payload["hook_event_name"], "Stop");
}

#[tokio::test]
async fn the_hold_route_returns_the_sinks_json() {
    let w = World::start().await;
    let extra = format!("X-Ainb-Hook-Token: {}\r\n", w.token());
    let resp = exchange(
        w.running.port(),
        &post("/hook/claude/hold", &w.host(), &extra, "{}"),
    )
    .await;
    assert_eq!(status(&resp), 200, "{resp}");
    assert!(resp.ends_with(r#"{"ok":true}"#), "{resp}");
    assert!(w.events()[0].hold);
}

#[tokio::test]
async fn a_wrong_token_is_refused_before_the_body_is_read() {
    let w = World::start().await;
    // Declare a body and never send it: a server that waited for the body
    // would time out (408) instead of answering 403 at once.
    let head = format!(
        "POST /hook/claude HTTP/1.1\r\nHost: {}\r\nX-Ainb-Hook-Token: not-the-token\r\nContent-Type: application/json\r\nContent-Length: 10\r\n\r\n",
        w.host()
    );
    let mut s = TcpStream::connect(("127.0.0.1", w.running.port())).await.unwrap();
    s.write_all(head.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_millis(1500), s.read_to_end(&mut out))
        .await
        .expect("refused without waiting for the body")
        .unwrap();
    assert_eq!(status(&String::from_utf8_lossy(&out)), 403);
    assert!(w.events().is_empty());
}

#[tokio::test]
async fn missing_token_foreign_host_and_origin_are_forbidden() {
    let w = World::start().await;
    let port = w.running.port();
    let tok = format!("X-Ainb-Hook-Token: {}\r\n", w.token());
    assert_eq!(
        status(&exchange(port, &post("/hook/claude", &w.host(), "", "{}")).await),
        403
    );
    assert_eq!(
        status(&exchange(port, &post("/hook/claude", "localhost", &tok, "{}")).await),
        403
    );
    let with_origin = format!("{tok}Origin: http://evil.example\r\n");
    assert_eq!(
        status(&exchange(port, &post("/hook/claude", &w.host(), &with_origin, "{}")).await),
        403
    );
    assert!(w.events().is_empty());
}

#[tokio::test]
async fn routes_methods_types_sizes_and_bodies() {
    let w = World::start().await;
    let port = w.running.port();
    let tok = format!("X-Ainb-Hook-Token: {}\r\n", w.token());
    assert_eq!(
        status(&exchange(port, &post("/hook/gemini", &w.host(), &tok, "{}")).await),
        404
    );
    let get = format!(
        "GET /hook/claude HTTP/1.1\r\nHost: {}\r\n{tok}\r\n",
        w.host()
    );
    assert_eq!(status(&exchange(port, get.as_bytes()).await), 405);
    let text = format!(
        "POST /hook/claude HTTP/1.1\r\nHost: {}\r\n{tok}Content-Type: text/plain\r\nContent-Length: 2\r\n\r\n{{}}",
        w.host()
    );
    assert_eq!(status(&exchange(port, text.as_bytes()).await), 415);
    let too_big = format!(
        "POST /hook/claude HTTP/1.1\r\nHost: {}\r\n{tok}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        w.host(),
        hook_ingress::MAX_BODY + 1
    );
    assert_eq!(status(&exchange(port, too_big.as_bytes()).await), 413);
    assert_eq!(
        status(&exchange(port, &post("/hook/claude", &w.host(), &tok, "[1]")).await),
        400
    );
    assert_eq!(
        status(&exchange(port, &post("/hook/claude", &w.host(), &tok, "nope")).await),
        400
    );
    assert!(w.events().is_empty());
}

#[tokio::test]
async fn expect_continue_is_answered_so_curl_does_not_stall() {
    let w = World::start().await;
    let extra = format!(
        "X-Ainb-Hook-Token: {}\r\nExpect: 100-continue\r\n",
        w.token()
    );
    let head = post("/hook/claude", &w.host(), &extra, "");
    let mut s = TcpStream::connect(("127.0.0.1", w.running.port())).await.unwrap();
    // Send the head declaring 2 bytes, wait for 100 Continue, then the body.
    let head = String::from_utf8(head)
        .unwrap()
        .replace("Content-Length: 0", "Content-Length: 2");
    s.write_all(head.as_bytes()).await.unwrap();
    let mut first = [0_u8; 25];
    tokio::time::timeout(Duration::from_millis(500), s.read_exact(&mut first))
        .await
        .expect("100 Continue arrives without the body")
        .unwrap();
    assert_eq!(&first, b"HTTP/1.1 100 Continue\r\n\r\n");
    s.write_all(b"{}").await.unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    assert_eq!(status(&String::from_utf8_lossy(&out)), 204);
}

#[tokio::test]
async fn bad_pane_headers_are_dropped_not_trusted() {
    let w = World::start().await;
    let extra = format!(
        "X-Ainb-Hook-Token: {}\r\nX-Ainb-Pane-Key: v1:../../etc\r\nX-Ainb-Tmux-Pane: $(id)\r\n",
        w.token()
    );
    let resp = exchange(
        w.running.port(),
        &post("/hook/codex", &w.host(), &extra, "{}"),
    )
    .await;
    assert_eq!(status(&resp), 204);
    let e = &w.events()[0];
    assert_eq!(e.source, HookSource::Codex);
    assert!(e.pane_key.is_none());
    assert!(e.tmux_pane.is_none());
}

#[tokio::test]
async fn files_are_private_and_the_endpoint_holds_no_token() {
    let w = World::start().await;
    let dir = w.home.path().join("hangar");
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join(ENDPOINT_FILE_NAME)), 0o600);
    assert_eq!(mode(&w.headers_path()), 0o600);
    let endpoint = std::fs::read_to_string(dir.join(ENDPOINT_FILE_NAME)).unwrap();
    assert!(!endpoint.contains(&w.token()));
    assert_eq!(w.endpoint().port, w.running.port());
}

#[tokio::test]
async fn a_restart_mints_a_new_token_and_the_old_one_is_refused() {
    let w = World::start().await;
    let old = w.token();
    drop(w.running);
    // A new daemon on the same home.
    let sink = Arc::new(Recorder::default());
    let running = hook_ingress::start(w.home.path(), sink.clone()).await.unwrap();
    let w2 = World {
        home: w.home,
        sink,
        running,
    };
    assert_ne!(w2.token(), old);
    let host = w2.host();
    let resp = exchange(
        w2.running.port(),
        &post(
            "/hook/claude",
            &host,
            &format!("X-Ainb-Hook-Token: {old}\r\n"),
            "{}",
        ),
    )
    .await;
    assert_eq!(status(&resp), 403);
}

#[tokio::test]
async fn dropping_the_listener_removes_both_files() {
    let w = World::start().await;
    let dir = w.home.path().join("hangar");
    let headers = w.headers_path();
    drop(w.running);
    assert!(!dir.join(ENDPOINT_FILE_NAME).exists());
    assert!(!headers.exists());
}

#[tokio::test]
async fn it_binds_loopback_only() {
    let w = World::start().await;
    let addr = w.running.local_addr();
    assert!(addr.ip().is_loopback(), "{addr}");
    assert_eq!(addr.ip().to_string(), "127.0.0.1");
}

#[tokio::test]
async fn connections_past_the_cap_are_turned_away() {
    let w = World::start().await;
    let port = w.running.port();
    let mut idle = Vec::new();
    for _ in 0..hook_ingress::MAX_CONNECTIONS {
        idle.push(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
    }
    // Give the accept loop time to take every slot.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let resp = exchange(port, b"").await;
    assert_eq!(status(&resp), 503, "{resp}");
    drop(idle);
}

#[tokio::test]
async fn hostile_framing_is_refused_and_never_reaches_the_sink() {
    let w = World::start().await;
    let tok = format!("X-Ainb-Hook-Token: {}\r\n", w.token());
    let host = w.host();
    let cases = [
        format!(
            "POST /hook/claude HTTP/1.1\r\nHost: {host}\r\n{tok}Content-Type: application/json\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n{{}}"
        ),
        format!(
            "POST /hook/claude HTTP/1.1\r\nHost: {host}\r\n{tok}Content-Type: application/json\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{{}}"
        ),
        format!(
            "POST /hook/claude HTTP/1.1\r\nHost: {host}\r\n{tok}Content-Type: application/json\r\nContent-Length: -1\r\n\r\n{{}}"
        ),
        format!(
            "POST /hook/claude HTTP/1.1\r\nHost: {host}\r\n{tok}Host: {host}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}"
        ),
    ];
    for raw in cases {
        let resp = exchange(w.running.port(), raw.as_bytes()).await;
        assert_eq!(status(&resp), 400, "{raw:?}");
    }
    // A short body: the head declares 10 bytes, the peer sends 2 and closes.
    let mut s = TcpStream::connect(("127.0.0.1", w.running.port())).await.unwrap();
    s.write_all(
        format!(
            "POST /hook/claude HTTP/1.1\r\nHost: {host}\r\n{tok}Content-Type: application/json\r\nContent-Length: 10\r\n\r\n{{}}"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    s.shutdown().await.unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    assert_eq!(status(&String::from_utf8_lossy(&out)), 400);
    assert!(w.events().is_empty());
}

/// A sink that never answers, for the deadline and cap tests.
struct Stuck;

impl HookSink for Stuck {
    fn ingest(
        &self,
        _event: HookEvent,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HookReply> + Send + '_>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            HookReply::NoContent
        })
    }
}

async fn stuck_world(limits: hook_ingress::Limits) -> (tempfile::TempDir, hook_ingress::Running) {
    let home = tempfile::tempdir().unwrap();
    let running = hook_ingress::start_with(home.path(), Arc::new(Stuck), limits).await.unwrap();
    (home, running)
}

/// The headers file's line, as an HTTP header line (CRLF, not the file's LF).
fn token_line(home: &std::path::Path) -> String {
    let line = std::fs::read_to_string(home.join("hangar").join("hook-headers")).unwrap();
    format!("{}\r\n", line.trim_end())
}

#[tokio::test]
async fn a_slow_head_gets_408() {
    let w = World::start().await;
    let mut s = TcpStream::connect(("127.0.0.1", w.running.port())).await.unwrap();
    s.write_all(b"POST /hook/claude HTTP/1.1\r\n").await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out))
        .await
        .expect("answered within the head deadline")
        .unwrap();
    assert_eq!(status(&String::from_utf8_lossy(&out)), 408);
}

#[tokio::test]
async fn a_sink_past_its_deadline_is_answered_204() {
    let (home, running) = stuck_world(hook_ingress::Limits {
        event_deadline: Duration::from_millis(200),
        hold_deadline: Duration::from_millis(200),
        max_holds: 4,
    })
    .await;
    let host = format!("127.0.0.1:{}", running.port());
    for path in ["/hook/claude", "/hook/claude/hold"] {
        let started = std::time::Instant::now();
        let resp = exchange(
            running.port(),
            &post(path, &host, &token_line(home.path()), "{}"),
        )
        .await;
        assert_eq!(status(&resp), 204, "{path}");
        assert!(started.elapsed() < Duration::from_secs(3), "{path}");
    }
}

#[tokio::test]
async fn a_hold_past_the_cap_is_answered_at_once() {
    let (home, running) = stuck_world(hook_ingress::Limits {
        event_deadline: Duration::from_secs(1),
        hold_deadline: Duration::from_secs(30),
        max_holds: 1,
    })
    .await;
    let host = format!("127.0.0.1:{}", running.port());
    let tok = token_line(home.path());
    // The first hold takes the only slot and waits on the stuck sink.
    let first = {
        let (port, host, tok) = (running.port(), host.clone(), tok.clone());
        tokio::spawn(
            async move { exchange(port, &post("/hook/claude/hold", &host, &tok, "{}")).await },
        )
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    // The second is recorded as status and answered without waiting the 30s.
    let started = std::time::Instant::now();
    let second = tokio::time::timeout(
        Duration::from_secs(15),
        exchange(
            running.port(),
            &post("/hook/claude/hold", &host, &tok, "{}"),
        ),
    )
    .await;
    // With the stuck sink the second waits its event deadline (1s), never
    // the hold deadline (30s).
    let second = second.expect("answered on the event deadline, not the hold deadline");
    assert_eq!(status(&second), 204);
    assert!(started.elapsed() < Duration::from_secs(5));
    first.abort();
}
