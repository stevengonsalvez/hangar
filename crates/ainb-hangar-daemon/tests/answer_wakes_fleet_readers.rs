//! An answer wakes every `fleet/subscribe` reader, so the agent-status card
//! it closed stops reading `waiting` at once, and a failed delivery that
//! reopens the request wakes them again.
//!
//! The agent-status read already derives `waiting` from the open inbox
//! (#962), but a reader re-reads only on a new fleet revision. Answering wrote
//! none, so a driven run's desktop board kept an answered question for 20 s
//! to 3 min, until the agent's next hook happened to write one.
//!
//! ```text
//! hook: Notification ──▶ card waiting, inbox open
//! attention/answer ──▶ delivered ──▶ fleet revision ──▶ re-read: not waiting
//! attention/answer ──▶ delivery failed, reopened ──▶ fleet revision ──▶ waiting
//! ```
//!
//! The forced-delivery seam, `$AINB_BIN`, `$HOME` and `$TMUX_TMPDIR` are
//! process-global, so the tests here take `SEAM` for their whole run.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::fleet::{self, HookObservation};
use ainb_hangar_daemon::rpc::{self, DaemonHealth, auth::Caller};
use ainb_hangar_proto::agent_status::AgentState;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};

static SEAM: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const SESSION: &str = "sess-wake";
const CWD: &str = "/work/wake";

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/answer-wake.sock".to_string(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

/// One running session in `CWD`, so the answer's target resolution is
/// unambiguous.
fn install_fake_ainb(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let rows = serde_json::json!([{
        "session_id": SESSION,
        "tmux_session_name": "wake-pane",
        "workspace_name": "wake",
        "worktree_path": CWD,
        "created_at": "2026-09-27T00:00:00Z",
        "is_running": true,
        "claude_active": true,
    }])
    .to_string();
    let script = dir.join("fake-ainb");
    std::fs::write(&script, format!("#!/bin/sh\ncat <<'JSON'\n{rows}\nJSON\n")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

async fn hook(store: &Store, broker: &EventBroker, event_id: &str, event_type: &str, at: i64) {
    fleet::apply_hook(
        store.pool(),
        &broker.sink(),
        HookObservation {
            event_id: event_id.to_string(),
            provider: "claude",
            provider_session_id: SESSION,
            cwd: CWD,
            event_type,
            payload: &serde_json::json!({ "notification_type": "permission_prompt" }),
            observed_at: at,
            transcript_model: None,
        },
    )
    .await
    .unwrap();
}

/// The card for `SESSION`: its state, whether the inbox holds a request, and
/// the head revision the read covers.
async fn card(store: &Store) -> (AgentState, bool, i64) {
    let status = fleet::status_rows(store.pool()).await.unwrap();
    let row = status
        .rows
        .iter()
        .find(|row| row.session_key.ends_with(SESSION))
        .unwrap_or_else(|| panic!("no card for {SESSION}: {:?}", status.rows));
    (row.state, row.has_open_request, status.head_revision)
}

/// A session blocked on a question: its card waiting, its inbox row open.
/// Everything the process would reach outside the test lives under `dir`: a
/// fake `ainb`, a `$HOME` with no broker, and a tmux socket dir with no
/// server, so a real delivery attempt fails rather than reaching anything.
struct Asked {
    store: Store,
    broker: EventBroker,
    attention_id: String,
    head: i64,
}

async fn asked(dir: &std::path::Path) -> Asked {
    std::env::set_var("AINB_BIN", install_fake_ainb(dir));
    std::env::set_var("HOME", dir);
    let tmux = dir.join("tmux");
    std::fs::create_dir_all(&tmux).unwrap();
    std::env::set_var("TMUX_TMPDIR", &tmux);
    std::env::remove_var("TMUX");

    let store = Store::open_in(dir).await.unwrap();
    let broker = EventBroker::new();
    hook(&store, &broker, "wake-start", "SessionStart", 100).await;
    hook(&store, &broker, "wake-ask", "Notification", 101).await;
    // The inbox's own row for the question, unless the hook raised one.
    let open = AttentionRepo::list_fleet(store.pool()).await.unwrap();
    let attention_id = if let Some(row) = open.iter().find(|row| row.session_id == SESSION) {
        row.id.clone()
    } else {
        AttentionRepo::insert(
            store.pool(),
            &NewAttention {
                id: "att-wake".into(),
                session_id: SESSION.into(),
                cwd: CWD.into(),
                workspace_id: None,
                kind: AttentionKind::Approval,
                payload: r#"{"kind":"APPROVAL","id":"att-wake"}"#.into(),
                degraded: false,
                created_at: 1_000,
                raise_transcript: None,
                channels: ainb_hangar_core::channel::ChannelSet::NONE,
            },
        )
        .await
        .unwrap();
        "att-wake".to_string()
    };
    let (state, open_request, head) = card(&store).await;
    assert_eq!(
        state,
        AgentState::Waiting,
        "the question starts the card waiting"
    );
    assert!(open_request);
    Asked {
        store,
        broker,
        attention_id,
        head,
    }
}

async fn answer(asked: &Asked, op_id: &str) -> serde_json::Value {
    let reply = rpc::dispatch_as(
        asked.store.pool(),
        &RpcRequest {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: RpcId::Number(1),
            method: methods::ATTENTION_ANSWER.to_string(),
            params: serde_json::json!({
                "attention_id": asked.attention_id,
                "answer": "approve",
                "answered_by": "desktop",
                "op_id": op_id,
            }),
        },
        &health(),
        &asked.broker.sink(),
        &Caller::Operator,
    )
    .await;
    serde_json::to_value(reply).unwrap()
}

/// The next revision a reader is told of, within a bound.
async fn next_revision(revisions: &mut tokio::sync::broadcast::Receiver<i64>, why: &str) -> i64 {
    tokio::time::timeout(Duration::from_secs(5), revisions.recv())
        .await
        .unwrap_or_else(|_| {
            panic!("no fleet revision after {why}: every reader keeps its stale card")
        })
        .expect("the fleet channel is open")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delivered_answer_wakes_readers_and_the_card_stops_waiting() {
    let _seam = SEAM.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let asked = asked(dir.path()).await;

    // Subscribed before the answer: a broadcast reaches only receivers that
    // already exist, so one taken afterwards would see nothing either way.
    let mut revisions = asked.broker.subscribe_fleet();
    ainb_hangar_daemon::answer::set_forced_delivery_for_test(true);
    let reply = answer(&asked, "op-wake").await;
    ainb_hangar_daemon::answer::set_forced_delivery_for_test(false);
    assert!(
        reply.to_string().contains("delivered"),
        "the answer must be delivered: {reply}"
    );

    // What a reader acts on: a revision past the one it last read.
    let woke = next_revision(&mut revisions, "the answer").await;
    assert!(
        woke > asked.head,
        "the wake is a new revision ({woke} > {})",
        asked.head
    );

    // And the read it triggers no longer says waiting.
    let (state, open_request, read_head) = card(&asked.store).await;
    assert!(!open_request, "the answered request left the inbox");
    assert_ne!(state, AgentState::Waiting, "the re-read card stops waiting");
    assert!(
        read_head >= woke,
        "the read covers the wake ({read_head} >= {woke})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_delivery_reopens_the_request_and_wakes_readers_again() {
    let _seam = SEAM.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let asked = asked(dir.path()).await;

    // No forced delivery: the answer is claimed, the real send finds no tmux
    // server and no broker, and the claim is reverted. A reader that re-read
    // between the claim and the revert would otherwise be left showing the
    // question as answered while the agent is still blocked on it.
    let mut revisions = asked.broker.subscribe_fleet();
    let reply = answer(&asked, "op-wake-fail").await;
    assert!(
        !reply.to_string().contains("\"delivered\""),
        "the answer must not be delivered here: {reply}"
    );
    assert_eq!(
        AttentionRepo::get(asked.store.pool(), &asked.attention_id)
            .await
            .unwrap()
            .expect("the row")
            .state,
        "open",
        "the failed delivery reopened the request: {reply}"
    );

    let woke = next_revision(&mut revisions, "the reopen").await;
    assert!(
        woke > asked.head,
        "the wake is a new revision ({woke} > {})",
        asked.head
    );
    let (state, open_request, read_head) = card(&asked.store).await;
    assert!(open_request, "the request is open again");
    assert_eq!(
        state,
        AgentState::Waiting,
        "the re-read card is waiting again"
    );
    assert!(read_head >= woke);
}
