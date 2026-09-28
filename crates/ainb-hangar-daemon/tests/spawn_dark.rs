//! `worktree/create`, `worktree/agent_add` and `shell/create` ship dark: with
//! `AINB_HANGAR_SPAWN` unset at boot the daemon answers `METHOD_NOT_FOUND`,
//! exactly as a v1.29.0 daemon does, and none is in the mutation registry
//! yet.
//!
//! Its own test binary: the switch is read once per process, so the enabled
//! path lives in `spawn_verbs.rs`, a separate process that sets it.

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

#[tokio::test]
async fn worktree_create_is_method_not_found_by_default() {
    assert_method_not_found(
        m::WORKTREE_CREATE,
        serde_json::json!({"repo_path": "/tmp", "agent": "claude"}),
    )
    .await;
}

#[tokio::test]
async fn worktree_agent_add_is_method_not_found_by_default() {
    assert_method_not_found(
        m::WORKTREE_AGENT_ADD,
        serde_json::json!({"worktree_path": "/tmp", "agent": "claude"}),
    )
    .await;
}

#[tokio::test]
async fn shell_create_is_method_not_found_by_default() {
    assert_method_not_found(
        m::SHELL_CREATE,
        serde_json::json!({"worktree_path": "/tmp"}),
    )
    .await;
}

/// A well-formed request for `method` gets `METHOD_NOT_FOUND` and no result.
async fn assert_method_not_found(method: &str, params: serde_json::Value) {
    assert!(
        std::env::var_os(ainb_hangar_daemon::spawn::SPAWN_ENV).is_none(),
        "run with {} unset: that is the default being proven",
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

#[test]
fn worktree_create_is_not_in_the_mutation_registry_while_dark() {
    assert!(!ainb_hangar_proto::mutation::is_mutating(
        m::WORKTREE_CREATE
    ));
}

#[test]
fn worktree_agent_add_is_not_in_the_mutation_registry_while_dark() {
    assert!(!ainb_hangar_proto::mutation::is_mutating(
        m::WORKTREE_AGENT_ADD
    ));
}

#[test]
fn shell_create_is_not_in_the_mutation_registry_while_dark() {
    assert!(!ainb_hangar_proto::mutation::is_mutating(m::SHELL_CREATE));
}

#[test]
fn the_spawn_switch_is_an_env_name() {
    assert_eq!(ainb_hangar_daemon::spawn::SPAWN_ENV, "AINB_HANGAR_SPAWN");
}
