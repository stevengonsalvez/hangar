//! `worktree/create`, `worktree/agent_add` and the `shell/*` verbs are served
//! by default, and `AINB_HANGAR_SPAWN=0` at boot keeps them off: the daemon
//! then answers `METHOD_NOT_FOUND`, exactly as a daemon from before the
//! verbs does. Every mutating one of them is in the mutation registry,
//! whatever the switch says.
//!
//! Its own test binary: the switch is read once per process, so the default
//! path lives in `spawn_verbs.rs` and `shell_verbs.rs`, separate processes
//! that leave it unset.

use std::time::Instant;

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth, auth::Caller};
use ainb_hangar_proto::methods as m;
use ainb_hangar_proto::{RpcId, RpcRequest};
use ainb_hangar_store::Store;

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/spawn-dark.sock".to_string(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

/// Set the opt-out before any dispatch in this process reads the switch.
/// Every dispatching test calls this first.
fn opted_out() {
    static SET: std::sync::Once = std::sync::Once::new();
    // Edition 2021: set_var is safe.
    SET.call_once(|| std::env::set_var(ainb_hangar_daemon::spawn::SPAWN_ENV, "0"));
}

#[tokio::test]
async fn worktree_create_is_method_not_found_with_the_opt_out() {
    assert_method_not_found(
        m::WORKTREE_CREATE,
        serde_json::json!({"repo_path": "/tmp", "agent": "claude"}),
    )
    .await;
}

#[tokio::test]
async fn worktree_agent_add_is_method_not_found_with_the_opt_out() {
    assert_method_not_found(
        m::WORKTREE_AGENT_ADD,
        serde_json::json!({"worktree_path": "/tmp", "agent": "claude"}),
    )
    .await;
}

#[tokio::test]
async fn shell_create_is_method_not_found_with_the_opt_out() {
    assert_method_not_found(
        m::SHELL_CREATE,
        serde_json::json!({"worktree_path": "/tmp"}),
    )
    .await;
}

#[tokio::test]
async fn shell_list_is_method_not_found_with_the_opt_out() {
    assert_method_not_found(m::SHELL_LIST, serde_json::json!({})).await;
}

#[tokio::test]
async fn shell_close_is_method_not_found_with_the_opt_out() {
    assert_method_not_found(
        m::SHELL_CLOSE,
        serde_json::json!({"tmux_session_name": "ainb-dsh-0a1b2c3d"}),
    )
    .await;
}

/// A well-formed request for `method` gets `METHOD_NOT_FOUND` and no result.
async fn assert_method_not_found(method: &str, params: serde_json::Value) {
    opted_out();
    assert!(
        !ainb_hangar_daemon::spawn::enabled(),
        "{}=0 must keep the spawn verbs off",
        ainb_hangar_daemon::spawn::SPAWN_ENV
    );
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let request = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: method.to_string(),
        params,
    };
    let response = rpc::dispatch_as(
        store.pool(),
        &request,
        &health(),
        &broker.sink(),
        &Caller::Operator,
    )
    .await;
    let response = serde_json::to_value(response).unwrap();
    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(-32601),
        "{response}"
    );
    assert!(response.get("result").is_none(), "{response}");
}

/// Every spawn verb that changes the host is in the ledger: a retried op id
/// replays the first answer (the same worktree, agent or shell) rather than
/// running again. Listing is a read.
#[test]
fn the_mutating_spawn_verbs_are_in_the_mutation_registry() {
    for method in [
        m::WORKTREE_CREATE,
        m::WORKTREE_AGENT_ADD,
        m::SHELL_CREATE,
        m::SHELL_CLOSE,
    ] {
        assert!(
            ainb_hangar_proto::mutation::is_mutating(method),
            "{method}"
        );
    }
    assert!(!ainb_hangar_proto::mutation::is_mutating(m::SHELL_LIST));
}

#[test]
fn the_spawn_switch_is_an_env_name() {
    assert_eq!(ainb_hangar_daemon::spawn::SPAWN_ENV, "AINB_HANGAR_SPAWN");
}
