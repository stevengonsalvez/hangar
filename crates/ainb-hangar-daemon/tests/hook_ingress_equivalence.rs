//! One hook sequence, two transports, one result.
//!
//! The same Claude events are delivered to one store through the
//! `events.jsonl` tail (the shape `ainb fleet atc hook` appends) and to a
//! second store through the HTTP hook listener. The Fleet row and the
//! attention inbox must come out the same, so a session cannot read
//! differently depending on which transport its hooks use.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_daemon::attention_ingest::AttentionIngest;
use ainb_hangar_daemon::hook_ingress::{self, IngestSink};
use ainb_hangar_proto::hooks::{ENDPOINT_FILE_NAME, HookEndpoint};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::AttentionRepo;
use ainb_hangar_store::repo::fleet::FleetRepo;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

const SESSION: &str = "5b3f9f7e-7a51-4bd5-9a64-0e3ac2f7d101";

fn sequence(cwd: &str) -> Vec<Value> {
    let base = |event: &str| {
        json!({
            "hook_event_name": event,
            "session_id": SESSION,
            "cwd": cwd,
            "transcript_path": "",
        })
    };
    let with = |event: &str, extra: Value| {
        let mut v = base(event);
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    };
    vec![
        with("SessionStart", json!({"source": "startup"})),
        with("UserPromptSubmit", json!({"prompt": "ask me something"})),
        with(
            "PreToolUse",
            json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_use_id": "t1"}),
        ),
        with(
            "PostToolUse",
            json!({"tool_name": "Bash", "tool_use_id": "t1"}),
        ),
        with(
            "PreToolUse",
            json!({
                "tool_name": "AskUserQuestion",
                "tool_use_id": "t2",
                "tool_input": {"questions": [{
                    "question": "Which colour?",
                    "header": "Colour",
                    "multiSelect": false,
                    "options": [{"label": "Red", "description": ""}, {"label": "Blue", "description": ""}]
                }]}
            }),
        ),
        with(
            "Notification",
            json!({"notification_type": "idle_prompt", "message": "waiting"}),
        ),
        with("Elicitation", json!({"message": "server wants input"})),
        with("Stop", json!({"stop_hook_active": false})),
    ]
}

/// The line `ainb fleet atc hook` appends for one payload.
fn cli_line(payload: &Value, n: usize) -> String {
    let event = payload["hook_event_name"].as_str().unwrap();
    let matcher = match event {
        "PreToolUse" | "PermissionRequest" => payload["tool_name"].as_str(),
        "Notification" => payload["notification_type"].as_str(),
        _ => None,
    };
    json!({
        "event_id": format!("cli-{n}"),
        "ts": 1_700_000_000_000_i64 + i64::try_from(n).unwrap(),
        "session_id": payload["session_id"],
        "cwd": payload["cwd"],
        "transcript_path": "",
        "agent": "claude",
        "event_type": event,
        "matcher": matcher,
        "parent": Value::Null,
        "tmux_target": Value::Null,
        "process_start_fingerprint": Value::Null,
        "payload": payload,
        "raw_payload_ref": Value::Null,
    })
    .to_string()
}

async fn via_jsonl(dir: &Path, cwd: &str) -> Store {
    let store = Store::open_in(dir).await.unwrap();
    let events = dir.join("events.jsonl");
    let lines: Vec<String> =
        sequence(cwd).iter().enumerate().map(|(n, p)| cli_line(p, n)).collect();
    std::fs::write(&events, format!("{}\n", lines.join("\n"))).unwrap();
    let broker = ainb_hangar_daemon::events::EventBroker::new();
    let ingest = AttentionIngest::new(
        store.pool().clone(),
        broker.sink(),
        events,
        dir.join("cursor"),
    );
    ingest.ingest_once(1_700_000_001_000).await;
    store
}

async fn via_http(dir: &Path, cwd: &str) -> Store {
    let store = Store::open_in(dir).await.unwrap();
    let broker = ainb_hangar_daemon::events::EventBroker::new();
    let ingest = AttentionIngest::new(
        store.pool().clone(),
        broker.sink(),
        dir.join("unused-events.jsonl"),
        dir.join("unused-cursor"),
    );
    let sink = Arc::new(IngestSink::new(ingest, dir.to_path_buf()));
    let running = hook_ingress::start(dir, sink).await.unwrap();
    let endpoint = HookEndpoint::parse_env_file(
        &std::fs::read_to_string(dir.join("hangar").join(ENDPOINT_FILE_NAME)).unwrap(),
    )
    .unwrap();
    let token_line = std::fs::read_to_string(&endpoint.headers_path).unwrap();
    for payload in sequence(cwd) {
        let body = payload.to_string();
        let request = format!(
            "POST /hook/claude HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{token_line}Content-Type: application/json\r\nContent-Length: {len}\r\n\r\n{body}",
            port = running.port(),
            len = body.len()
        );
        let mut s = TcpStream::connect(("127.0.0.1", running.port())).await.unwrap();
        s.write_all(request.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut out))
            .await
            .unwrap()
            .unwrap();
        let out = String::from_utf8_lossy(&out);
        assert!(out.starts_with("HTTP/1.1 204"), "{out}");
    }
    drop(running);
    store
}

/// The fields a surface reads, without clocks or ids.
async fn observed(store: &Store) -> Value {
    let session = FleetRepo::get_session(store.pool(), &format!("claude:{SESSION}"))
        .await
        .unwrap()
        .expect("the session was reduced");
    let mut attention: Vec<Value> = AttentionRepo::list_fleet(store.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|row| json!({"kind": format!("{:?}", row.kind), "session": row.session_id, "cwd": row.cwd}))
        .collect();
    attention.sort_by_key(ToString::to_string);
    json!({
        "provider": session.provider,
        "provider_session_id": session.provider_session_id,
        "cwd": session.cwd,
        "lifecycle_state": session.lifecycle_state,
        "attention_state": session.attention_state,
        "current_request_fingerprint": session.current_request_fingerprint,
        "management_state": session.management_state,
        "attention": attention,
    })
}

#[tokio::test]
async fn http_and_jsonl_ingest_yield_the_same_fleet_row_and_inbox() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    // One cwd with no transcript under it, shared by both runs.
    let cwd = format!("/tmp/hook-equivalence-{}", std::process::id());
    let jsonl = observed(&via_jsonl(a.path(), &cwd).await).await;
    let http = observed(&via_http(b.path(), &cwd).await).await;
    assert_eq!(jsonl, http);
    // And the sequence did something a surface can see.
    assert_ne!(jsonl["lifecycle_state"], Value::Null);
}
