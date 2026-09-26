//! `worktree/create` with the switch on, end to end through dispatch, against
//! a stand-in `ainb` that records its argv and answers like `ainb --format json run`.
//!
//! The real `ainb --format json run` contract (worktree from `--base`, one JSON line)
//! is proven in `crates/ainb-core/tests/run_json_worktree.rs`. This binary
//! proves the daemon half: the request becomes the right argv, the one JSON
//! line becomes the result, and a failed run becomes an error carrying the
//! CLI's own last words.
//!
//! Its own process: `AINB_HANGAR_SPAWN` and `AINB_BIN` are process-global and
//! the switch is read once, so they are set before the first dispatch and the
//! tests run serially on one lock.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth, auth::Caller};
use ainb_hangar_proto::methods as m;
use ainb_hangar_proto::{RpcId, RpcRequest};
use ainb_hangar_store::Store;

static SERIAL: Mutex<()> = Mutex::new(());

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/spawn-verbs.sock".to_string(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

/// A fake `ainb`: writes its argv (one per line) to `argv.txt`, then either
/// prints one JSON line and exits 0, or prints to stderr and exits 3.
fn fake_ainb(dir: &Path, succeed: bool) -> std::path::PathBuf {
    let bin = dir.join("ainb");
    let argv_file = dir.join("argv.txt");
    let body = if succeed {
        r#"echo 'progress on stderr' >&2
printf '%s\n' '{"session_id":"11111111-1111-4111-8111-111111111111","tmux_session_name":"app-11111111","worktree_path":"/w/app","branch":"feat/x","claude_session_id":"c-1"}'
exit 0"#
    } else {
        r#"echo 'Error: Failed to create worktree' >&2
exit 3"#
    };
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\n: > '{argv}'\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{argv}'; done\n{body}\n",
            argv = argv_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

async fn call(params: serde_json::Value) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let request = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(7),
        method: m::WORKTREE_CREATE.to_string(),
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
    serde_json::to_value(response).unwrap()
}

fn switch_on(bin: &Path) {
    // Edition 2021: set_var is safe. Held under SERIAL for the whole test.
    std::env::set_var(ainb_hangar_daemon::spawn::SPAWN_ENV, "1");
    std::env::set_var("AINB_BIN", bin);
}

#[tokio::test]
async fn a_create_runs_ainb_run_with_the_request_and_returns_its_session() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    switch_on(&fake_ainb(tools.path(), true));

    let response = call(serde_json::json!({
        "repo_path": repo.path().display().to_string(),
        "agent": "claude",
        "branch": "feat/x",
        "base": "origin/main",
        "model": "opus",
        "prompt": "-y fix it",
    }))
    .await;

    let result = &response["result"];
    assert_eq!(result["tmux_session_name"], "app-11111111", "{response}");
    assert_eq!(result["branch"], "feat/x");
    assert_eq!(result["claude_session_id"], "c-1");

    let argv = std::fs::read_to_string(tools.path().join("argv.txt")).unwrap();
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(
        &argv[..4],
        ["--format", "json", "run", "--worktree"],
        "{argv:?}"
    );
    let pairs: Vec<(&str, &str)> = argv.windows(2).map(|w| (w[0], w[1])).collect();
    let repo_arg = repo.path().display().to_string();
    for pair in [
        ("--repo", repo_arg.as_str()),
        ("--tool", "claude"),
        ("--create-branch", "feat/x"),
        ("--base", "origin/main"),
        ("--model", "opus"),
    ] {
        assert!(pairs.contains(&pair), "missing {pair:?} in {argv:?}");
    }
    assert_eq!(argv.last(), Some(&"--prompt=-y fix it"));
}

#[tokio::test]
async fn a_failed_run_is_an_internal_error_with_the_cli_message() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    switch_on(&fake_ainb(tools.path(), false));

    let response = call(serde_json::json!({
        "repo_path": repo.path().display().to_string(),
        "agent": "codex",
    }))
    .await;

    assert!(response.get("result").is_none(), "{response}");
    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("Failed to create worktree"), "{response}");
}

#[tokio::test]
async fn a_bad_request_is_invalid_params_and_spawns_nothing() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    switch_on(&fake_ainb(tools.path(), true));

    let response = call(serde_json::json!({"repo_path": "relative/path", "agent": "claude"})).await;

    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(-32602),
        "{response}"
    );
    assert!(
        !tools.path().join("argv.txt").exists(),
        "a refused request must not run ainb"
    );
}
