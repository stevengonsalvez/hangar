//! `[hangar] spawn = false` in the hangar home's `config/config.toml` keeps
//! the spawn verbs off with no `AINB_HANGAR_SPAWN` in the environment: all
//! five answer `METHOD_NOT_FOUND`, as they do with the variable set to `0`.
//!
//! Its own test binary: the switch is read once per process, and this one
//! reads it from a private hangar home. `HOME` and `TMUX_TMPDIR` point at
//! private directories too, so nothing here reaches the user's home or tmux.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth, auth::Caller};
use ainb_hangar_proto::methods as m;
use ainb_hangar_proto::{RpcId, RpcRequest};
use ainb_hangar_store::Store;

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/spawn-dark-file.sock".to_string(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

/// A private home whose config opts out, published before any dispatch in
/// this process reads the switch. Every dispatching test calls this first.
///
/// The hangar home is NOT `$HOME/.agents-in-a-box`: a reader that wrongly
/// roots at `$HOME` would then find the opt-out anyway and hide the bug.
fn opted_out_by_file() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let root = tempfile::tempdir().unwrap().keep();
        let hangar = root.join("hangar-home");
        let config = ainb_hangar_daemon::spawn::config_path_in(&hangar);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "[hangar]\nspawn = false\n").unwrap();
        let tmux = root.join("tmux");
        std::fs::create_dir_all(&tmux).unwrap();
        // Edition 2021: set_var is safe.
        std::env::set_var("HOME", &root);
        std::env::set_var("TMUX_TMPDIR", &tmux);
        std::env::set_var("AINB_HANGAR_HOME", &hangar);
        std::env::remove_var(ainb_hangar_daemon::spawn::SPAWN_ENV);
        hangar
    })
}

#[tokio::test]
async fn worktree_create_is_method_not_found_with_the_file_opt_out() {
    assert_method_not_found(
        m::WORKTREE_CREATE,
        serde_json::json!({"repo_path": "/tmp", "agent": "claude"}),
    )
    .await;
}

#[tokio::test]
async fn worktree_agent_add_is_method_not_found_with_the_file_opt_out() {
    assert_method_not_found(
        m::WORKTREE_AGENT_ADD,
        serde_json::json!({"worktree_path": "/tmp", "agent": "claude"}),
    )
    .await;
}

#[tokio::test]
async fn shell_create_is_method_not_found_with_the_file_opt_out() {
    assert_method_not_found(
        m::SHELL_CREATE,
        serde_json::json!({"worktree_path": "/tmp"}),
    )
    .await;
}

#[tokio::test]
async fn shell_list_is_method_not_found_with_the_file_opt_out() {
    assert_method_not_found(m::SHELL_LIST, serde_json::json!({})).await;
}

#[tokio::test]
async fn shell_close_is_method_not_found_with_the_file_opt_out() {
    assert_method_not_found(
        m::SHELL_CLOSE,
        serde_json::json!({"tmux_session_name": "ainb-dsh-0a1b2c3d"}),
    )
    .await;
}

/// A well-formed request for `method` gets `METHOD_NOT_FOUND` and no result.
async fn assert_method_not_found(method: &str, params: serde_json::Value) {
    let hangar = opted_out_by_file();
    assert!(
        std::env::var_os(ainb_hangar_daemon::spawn::SPAWN_ENV).is_none(),
        "the file alone must be the opt-out here"
    );
    assert!(
        !ainb_hangar_daemon::spawn::enabled(),
        "`{}` = false in {} must keep the spawn verbs off",
        ainb_hangar_daemon::spawn::SPAWN_CONFIG_KEY,
        ainb_hangar_daemon::spawn::config_path_in(hangar).display()
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
fn the_spawn_file_key_is_hangar_spawn() {
    assert_eq!(ainb_hangar_daemon::spawn::SPAWN_CONFIG_KEY, "hangar.spawn");
}
