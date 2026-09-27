//! The one blocking, structured reply path, end to end over real sockets.
//!
//! A Claude `PermissionRequest` or `AskUserQuestion` hook POSTs to the hold
//! route and waits. `attention/answer` (the daemon's own answer function)
//! resolves it: the hook receives the daemon-built decision, first answer
//! wins, duplicates join one hold, a status event showing the agent moved on
//! ends it, and an answer that does not fit the question claims nothing.

use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_daemon::attention_ingest::AttentionIngest;
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::hook_ingress::{self, IngestSink, RESOLVED_BY_AGENT};
use ainb_hangar_proto::mutation::MutationEnvelope;
use ainb_hangar_proto::snapshots::{AnswerParams, AnswerResult};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

struct World {
    home: tempfile::TempDir,
    store: Store,
    broker: EventBroker,
    running: hook_ingress::Running,
    sink: Arc<IngestSink>,
}

impl World {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open_in(home.path()).await.unwrap();
        let broker = EventBroker::new();
        let ingest = AttentionIngest::new(
            store.pool().clone(),
            broker.sink(),
            home.path().join("events.jsonl"),
            home.path().join("cursor"),
        );
        let sink = IngestSink::new(
            ingest,
            home.path().to_path_buf(),
            store.pool().clone(),
            broker.sink(),
        );
        let sink = Arc::new(sink);
        let running = hook_ingress::start(home.path(), sink.clone()).await.unwrap();
        Self {
            home,
            store,
            broker,
            running,
            sink,
        }
    }

    fn token_line(&self) -> String {
        let line = std::fs::read_to_string(self.home.path().join("hangar/hook-headers")).unwrap();
        format!("{}\r\n", line.trim_end())
    }

    /// POST one hook call and return `(status, body)`.
    async fn post(&self, path: &str, payload: &Value) -> (u16, String) {
        let body = payload.to_string();
        let raw = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{tok}Content-Type: application/json\r\nContent-Length: {len}\r\n\r\n{body}",
            port = self.running.port(),
            tok = self.token_line(),
            len = body.len()
        );
        let mut s = TcpStream::connect(("127.0.0.1", self.running.port())).await.unwrap();
        s.write_all(raw.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        tokio::time::timeout(Duration::from_secs(20), s.read_to_end(&mut out))
            .await
            .expect("the hold answered")
            .unwrap();
        let text = String::from_utf8_lossy(&out).into_owned();
        let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
        let body = text.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
        (status, body)
    }

    /// Start a hold in the background.
    fn hold(self: &Arc<Self>, payload: Value) -> tokio::task::JoinHandle<(u16, String)> {
        let w = self.clone();
        tokio::spawn(async move { w.post("/hook/claude/hold", &payload).await })
    }

    async fn answer(&self, attention_id: &str, answer: &str) -> AnswerResult {
        let params = AnswerParams {
            attention_id: attention_id.to_string(),
            answer: answer.to_string(),
            answered_by: "desktop@test".to_string(),
            is_answer: true,
            mutation: MutationEnvelope::default(),
        };
        ainb_hangar_daemon::answer::answer(self.store.pool(), &self.broker.sink(), &params, 1)
            .await
            .unwrap()
    }

    /// Wait until the session has an open row of `kind`; return its id.
    async fn open_row(&self, session: &str, kind: AttentionKind) -> String {
        for _ in 0..100 {
            let rows = AttentionRepo::list_fleet(self.store.pool()).await.unwrap();
            if let Some(row) = rows
                .into_iter()
                .find(|r| r.session_id == session && r.kind == kind && r.state == "open")
            {
                return row.id;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no open {kind:?} row for {session}");
    }
}

fn permission(session: &str, call: &str) -> Value {
    json!({
        "hook_event_name": "PermissionRequest",
        "session_id": session,
        "cwd": "/tmp/hold-test",
        "tool_name": "Bash",
        "tool_input": {"command": "ls"},
        "tool_use_id": call,
    })
}

fn ask(session: &str, call: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": "/tmp/hold-test",
        "tool_name": "AskUserQuestion",
        "tool_use_id": call,
        "tool_input": {"questions": [{
            "question": "Which colour?",
            "header": "Colour",
            "multiSelect": false,
            "options": [{"label": "Red", "description": ""}, {"label": "Blue", "description": ""}]
        }]}
    })
}

fn session(tag: &str) -> String {
    format!("hold-{tag}-{}", std::process::id())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_approval_answered_from_a_surface_reaches_the_held_hook_exactly() {
    let w = Arc::new(World::start().await);
    let s = session("approve");
    let held = w.hold(permission(&s, "call-1"));
    let id = w.open_row(&s, AttentionKind::Approval).await;
    let result = w.answer(&id, "allow").await;
    assert_eq!(
        result,
        AnswerResult::Delivered {
            via: "hook hold".into()
        }
    );
    let (status, body) = held.await.unwrap();
    assert_eq!(status, 200);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"],
        "PermissionRequest"
    );
    assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
    assert!(!body.contains("updatedInput"));
    let row = AttentionRepo::get(w.store.pool(), &id).await.unwrap().unwrap();
    assert_eq!(row.state, "answered");
    assert_eq!(row.answered_by.as_deref(), Some("desktop@test"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_hooks_join_one_hold_and_the_first_answer_wins() {
    let w = Arc::new(World::start().await);
    let s = session("join");
    let first = w.hold(permission(&s, "call-2"));
    let id = w.open_row(&s, AttentionKind::Approval).await;
    let second = w.hold(permission(&s, "call-2"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let open: Vec<_> = AttentionRepo::list_fleet(w.store.pool())
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.session_id == s && r.state == "open")
        .collect();
    assert_eq!(open.len(), 1, "one row for one request");

    let (a, b) = tokio::join!(w.answer(&id, "deny"), w.answer(&id, "allow"));
    let delivered = [&a, &b].iter().filter(|r| matches!(r, AnswerResult::Delivered { .. })).count();
    let lost = [&a, &b]
        .iter()
        .filter(|r| matches!(r, AnswerResult::AlreadyAnswered { .. }))
        .count();
    assert_eq!((delivered, lost), (1, 1), "{a:?} {b:?}");
    let winner = if matches!(a, AnswerResult::Delivered { .. }) {
        "deny"
    } else {
        "allow"
    };
    for held in [first, second] {
        let (status, body) = held.await.unwrap();
        assert_eq!(status, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], winner);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_gets_its_answer_and_a_label_not_offered_claims_nothing() {
    let w = Arc::new(World::start().await);
    let s = session("ask");
    let held = w.hold(ask(&s, "call-3"));
    let id = w.open_row(&s, AttentionKind::AskUserQuestion).await;

    let refused = w.answer(&id, "Green").await;
    assert!(
        matches!(refused, AnswerResult::NoTarget { .. }),
        "{refused:?}"
    );
    let row = AttentionRepo::get(w.store.pool(), &id).await.unwrap().unwrap();
    assert_eq!(row.state, "open", "nothing was claimed");

    assert!(matches!(
        w.answer(&id, "Blue").await,
        AnswerResult::Delivered { .. }
    ));
    let (status, body) = held.await.unwrap();
    assert_eq!(status, 200);
    let v: Value = serde_json::from_str(&body).unwrap();
    let out = &v["hookSpecificOutput"];
    assert_eq!(out["hookEventName"], "PreToolUse");
    assert_eq!(out["permissionDecision"], "allow");
    assert_eq!(out["updatedInput"]["answers"]["Which colour?"], "Blue");
    assert_eq!(
        out["updatedInput"]["questions"],
        ask(&s, "call-3")["tool_input"]["questions"],
        "the held questions, copied back verbatim"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agent_moving_on_ends_the_hold_and_retires_its_row() {
    let w = Arc::new(World::start().await);
    let s = session("moved");
    let held = w.hold(permission(&s, "call-4"));
    let id = w.open_row(&s, AttentionKind::Approval).await;
    // Answered at the terminal: the tool ran, so its PostToolUse arrives.
    let (status, _) = w
        .post(
            "/hook/claude",
            &json!({"hook_event_name": "PostToolUse", "session_id": s, "cwd": "/tmp/hold-test",
                    "tool_name": "Bash", "tool_use_id": "call-4"}),
        )
        .await;
    assert_eq!(status, 204);
    let (status, body) = held.await.unwrap();
    assert_eq!(status, 204, "the hold ends with no decision: {body}");
    let row = AttentionRepo::get(w.store.pool(), &id).await.unwrap().unwrap();
    assert_eq!(row.state, "answered");
    assert_eq!(row.answered_by.as_deref(), Some(RESOLVED_BY_AGENT));
    // And a late answer cannot type at the agent's prompt.
    assert!(matches!(
        w.answer(&id, "allow").await,
        AnswerResult::AlreadyAnswered { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hook_that_goes_away_retires_its_row_and_nothing_is_typed() {
    let w = Arc::new(World::start().await);
    let s = session("ended");
    // Raise the row through a hold, then drop the hook's connection.
    let held = w.hold(permission(&s, "call-5"));
    let id = w.open_row(&s, AttentionKind::Approval).await;
    held.abort();
    let mut row = None;
    for _ in 0..100 {
        let r = AttentionRepo::get(w.store.pool(), &id).await.unwrap().unwrap();
        if r.state != "open" {
            row = Some(r);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let row = row.expect("the row was retired when its hook went away");
    assert_eq!(
        row.answered_by.as_deref(),
        Some(hook_ingress::RESOLVED_NATIVE)
    );
    // A late answer is a loser, never keystrokes at the agent's own prompt.
    assert!(matches!(
        w.answer(&id, "allow").await,
        AnswerResult::AlreadyAnswered { .. }
    ));
}

fn permission_for(session: &str, call: &str, command: &str) -> Value {
    json!({
        "hook_event_name": "PermissionRequest",
        "session_id": session,
        "cwd": "/tmp/hold-test",
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "tool_use_id": call,
    })
}

/// The open approval row raised for one tool call.
async fn row_for_call(w: &World, session: &str, call: &str) -> String {
    for _ in 0..100 {
        let rows = AttentionRepo::list_fleet(w.store.pool()).await.unwrap();
        if let Some(row) = rows.into_iter().find(|r| {
            r.session_id == session
                && r.kind == AttentionKind::Approval
                && r.state == "open"
                && serde_json::from_str::<Value>(&r.payload).is_ok_and(|p| p["tool_use_id"] == call)
        }) {
            return row.id;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no open approval row for {call}");
}

/// The hostile rebind from the review of #187: a dangerous request's hook
/// drops, a harmless request is held, the dangerous one re-hooks, and the
/// harmless one is approved. The approval must reach only the harmless hook.
/// Repeated because the old binding failed by hash order, in 4 of 6 runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_never_reaches_another_requests_hook() {
    for run in 0..6 {
        let w = Arc::new(World::start().await);
        let s = session(&format!("rebind{run}"));
        let danger = permission_for(&s, "call-danger", "rm -rf /important");
        let harmless = permission_for(&s, "call-ls", "ls");

        let first = w.hold(danger.clone());
        let _ = row_for_call(&w, &s, "call-danger").await;
        first.abort(); // the dangerous hook's connection drops
        tokio::time::sleep(Duration::from_millis(300)).await;

        let held_ls = w.hold(harmless);
        let ls_row = row_for_call(&w, &s, "call-ls").await;

        let danger_again = w.hold(danger);
        tokio::time::sleep(Duration::from_millis(300)).await;
        // The dropped hook's row was retired; the re-hook raised its own.
        let danger_row = row_for_call(&w, &s, "call-danger").await;
        assert_ne!(danger_row, ls_row);

        assert!(matches!(
            w.answer(&ls_row, "allow").await,
            AnswerResult::Delivered { .. }
        ));
        let (status, body) = held_ls.await.unwrap();
        assert_eq!(status, 200, "run {run}");
        assert!(body.contains("\"allow\""), "run {run}: {body}");

        // The dangerous hook got nothing from that approval: still waiting.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !danger_again.is_finished(),
            "run {run}: the approval leaked"
        );

        // Its own row still answers it, and only it.
        assert!(matches!(
            w.answer(&danger_row, "deny").await,
            AnswerResult::Delivered { .. }
        ));
        let (status, body) = danger_again.await.unwrap();
        assert_eq!(status, 200, "run {run}");
        assert!(body.contains("\"deny\""), "run {run}: {body}");
    }
}

fn health() -> ainb_hangar_daemon::rpc::DaemonHealth {
    ainb_hangar_daemon::rpc::DaemonHealth {
        socket_path: "/tmp/hook-hold.sock".to_string(),
        pid: std::process::id(),
        started_at: std::time::Instant::now(),
        version: "0.1.0".into(),
        stats: Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

/// `attention/answer` over the RPC dispatcher as `caller`.
async fn rpc_answer(
    w: &World,
    caller: &ainb_hangar_daemon::rpc::auth::Caller,
    attention_id: &str,
    answer: &str,
) -> Value {
    let req = ainb_hangar_proto::RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: ainb_hangar_proto::RpcId::Number(1),
        method: ainb_hangar_proto::methods::ATTENTION_ANSWER.to_string(),
        params: json!({"attention_id": attention_id, "answer": answer, "answered_by": "x"}),
    };
    let resp = ainb_hangar_daemon::rpc::dispatch_as(
        w.store.pool(),
        &req,
        &health(),
        &w.broker.sink(),
        caller,
    )
    .await;
    serde_json::to_value(resp).unwrap()
}

fn is_scope_refusal(resp: &Value) -> bool {
    resp["error"]["code"] == ainb_hangar_proto::mutation::MUTATION_REJECTED
        && resp["error"].to_string().contains(&format!(
            "\"{}\"",
            ainb_hangar_proto::mutation::REASON_SCOPE
        ))
}

/// Review of #187 (M5): phone scopes and Pal are refused an approval row and
/// a held ask over the real dispatcher, and nothing is claimed; a desktop
/// device may answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phones_and_pal_are_refused_approvals_over_rpc_and_desktop_is_not() {
    use ainb_hangar_daemon::rpc::auth::Caller;
    use ainb_hangar_proto::devices::DeviceScope;
    let w = Arc::new(World::start().await);
    let s = session("rpc-scope");
    let approval = w.hold(permission(&s, "call-r1"));
    let approval_row = w.open_row(&s, AttentionKind::Approval).await;
    let ask_hold = w.hold(ask(&s, "call-r2"));
    let ask_row = w.open_row(&s, AttentionKind::AskUserQuestion).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let refused = [
        Caller::Device {
            device_id: "phone-1".into(),
            scope: DeviceScope::MOBILE,
        },
        Caller::Device {
            device_id: "phone-2".into(),
            scope: DeviceScope::MOBILE_TYPE,
        },
        Caller::Pal {
            scope_key: "pal-1".into(),
        },
    ];
    for caller in &refused {
        for (row, answer) in [(&approval_row, "allow"), (&ask_row, "Blue")] {
            let resp = rpc_answer(&w, caller, row, answer).await;
            assert!(is_scope_refusal(&resp), "{caller:?} {row}: {resp}");
            let state = AttentionRepo::get(w.store.pool(), row).await.unwrap().unwrap();
            assert_eq!(state.state, "open", "{caller:?} claimed {row}");
        }
    }
    assert!(
        !approval.is_finished() && !ask_hold.is_finished(),
        "no hook was answered"
    );

    let desktop = Caller::Device {
        device_id: "laptop".into(),
        scope: DeviceScope::DESKTOP,
    };
    let resp = rpc_answer(&w, &desktop, &approval_row, "allow").await;
    assert_eq!(resp["result"]["outcome"], "delivered", "{resp}");
    let (status, body) = approval.await.unwrap();
    assert_eq!(status, 200);
    assert!(body.contains("\"allow\""), "{body}");
    ask_hold.abort();
}

/// Review of #192 (M2): a replayed Stop is history. Drained while a hold is
/// live in the same session, it must not end that hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replayed_stop_never_ends_a_live_hold() {
    use std::os::unix::fs::PermissionsExt as _;
    let w = Arc::new(World::start().await);
    let s = session("replayed-stop");
    let held = w.hold(permission(&s, "call-live"));
    let id = w.open_row(&s, AttentionKind::Approval).await;

    let dir = w.home.path().join("hangar/hook-spool");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let spooled = dir.join("stale.jsonl");
    for event in ["Stop", "UserPromptSubmit", "SessionEnd"] {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&spooled).unwrap();
        writeln!(
            f,
            "{}",
            json!({"v": 1, "source": "claude", "event": event,
                   "received_at_ms": 1_700_000_000_000_i64,
                   "event_id": format!("0123456789abcdef-{event}"),
                   "payload": {"hook_event_name": event, "session_id": s, "cwd": "/tmp/hold-test"}})
        )
        .unwrap();
    }
    std::fs::set_permissions(&spooled, std::fs::Permissions::from_mode(0o600)).unwrap();
    let report = hook_ingress::drain_spool(w.home.path(), &*w.sink).await;
    assert_eq!(report.replayed, 3, "{report:?}");

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!held.is_finished(), "a replayed event ended a live hold");
    let row = AttentionRepo::get(w.store.pool(), &id).await.unwrap().unwrap();
    assert_eq!(row.state, "open");
    assert!(matches!(
        w.answer(&id, "allow").await,
        AnswerResult::Delivered { .. }
    ));
    assert_eq!(held.await.unwrap().0, 200);
}
