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

/// A home whose config registers `<home>/code`, with a git repository at
/// `<home>/code/app`, set as `$HOME` for the daemon's root lookup. The
/// daemon only creates from a registered folder's repository top.
struct Registered {
    home: tempfile::TempDir,
}

impl Registered {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("code/app");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ][..],
        ] {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .is_ok_and(|o| o.status.success());
            assert!(ok, "git {args:?}");
        }
        let config = home.path().join(".agents-in-a-box/config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            format!(
                "[workspace_defaults]\nworkspace_scan_paths = [\"{}\"]\n",
                home.path().join("code").display()
            ),
        )
        .unwrap();
        std::env::set_var("HOME", home.path());
        Self { home }
    }

    fn repo(&self) -> String {
        std::fs::canonicalize(self.home.path().join("code/app"))
            .unwrap()
            .display()
            .to_string()
    }
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
    let repo = Registered::new();
    switch_on(&fake_ainb(tools.path(), true));

    let response = call(serde_json::json!({
        "repo_path": repo.repo(),
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
    let repo_arg = repo.repo();
    for pair in [
        ("--repo", repo_arg.as_str()),
        ("--tool", "claude"),
        ("--create-branch", "feat/x"),
        ("--base", "origin/main"),
    ] {
        assert!(pairs.contains(&pair), "missing {pair:?} in {argv:?}");
    }
    assert!(argv.contains(&"--model=opus"), "{argv:?}");
    assert_eq!(argv.last(), Some(&"--prompt=-y fix it"));
}

#[tokio::test]
async fn an_existing_branch_is_refused_before_ainb_runs() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let repo = Registered::new();
    let ok = std::process::Command::new("git")
        .args(["branch", "taken"])
        .current_dir(repo.repo())
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(ok);
    switch_on(&fake_ainb(tools.path(), true));

    let response = call(serde_json::json!({
        "repo_path": repo.repo(),
        "agent": "claude",
        "branch": "taken",
        "base": "main",
    }))
    .await;

    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(-32602),
        "{response}"
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("already exists"),
        "{response}"
    );
    assert!(!tools.path().join("argv.txt").exists(), "nothing ran");
}

#[tokio::test]
async fn a_repo_outside_every_registered_folder_is_refused() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let _registered = Registered::new();
    let elsewhere = tempfile::tempdir().unwrap();
    switch_on(&fake_ainb(tools.path(), true));

    let response = call(serde_json::json!({
        "repo_path": elsewhere.path().display().to_string(),
        "agent": "claude",
    }))
    .await;

    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(-32602),
        "{response}"
    );
    assert!(!tools.path().join("argv.txt").exists(), "nothing ran");
}

#[tokio::test]
async fn a_failed_run_is_an_internal_error_with_the_cli_message() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let repo = Registered::new();
    switch_on(&fake_ainb(tools.path(), false));

    let response = call(serde_json::json!({
        "repo_path": repo.repo(),
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

/// A create that times out keeps draining `ainb run`'s output. A run still
/// working past the timeout (the prompt wait, a slow first thread) writes
/// later; if its pipes had been closed, that write would kill it on EPIPE
/// before its own rollback, leaving the tmux session, worktree and branch
/// behind. The stand-in writes to both streams after the timeout and only
/// then leaves its marker, so the marker says it lived through the writes.
#[tokio::test]
async fn a_timed_out_create_keeps_draining_so_a_late_write_does_not_kill_the_run() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let repo = Registered::new();
    let marker = tools.path().join("lived.txt");
    let bin = tools.path().join("ainb");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nsleep 1\necho 'late progress' >&2\necho 'late line'\necho 'more progress' >&2\n: > '{}'\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    switch_on(&bin);
    ainb_hangar_daemon::spawn::set_run_timeout_for_test(Some(std::time::Duration::from_millis(
        200,
    )));

    let response = call(serde_json::json!({ "repo_path": repo.repo(), "agent": "claude" })).await;
    ainb_hangar_daemon::spawn::set_run_timeout_for_test(None);
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("still running")),
        "the caller hears the create is still running: {response}"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !marker.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        marker.exists(),
        "the run died on a write after the timeout: its output was no longer drained"
    );
}

/// A `name` from an older client is not a field any more: it never reaches
/// `ainb run` as `--name`, which would kill a live tmux session of that name.
#[tokio::test]
async fn a_name_field_never_reaches_ainb_run() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let repo = Registered::new();
    switch_on(&fake_ainb(tools.path(), true));

    let response =
        call(serde_json::json!({ "repo_path": repo.repo(), "agent": "claude", "name": "x" })).await;
    // The fake really ran (an argv that was never written would pass the
    // check below for the wrong reason), and the create went through.
    let argv = std::fs::read_to_string(tools.path().join("argv.txt"))
        .unwrap_or_else(|e| panic!("the fake ainb never ran: {e}; {response}"));
    assert!(argv.lines().any(|arg| arg == "run"), "{argv:?}");
    assert!(response["result"]["session_id"].is_string(), "{response}");
    assert!(
        !argv
            .lines()
            .any(|arg| arg == "--name" || arg.starts_with("--name=") || arg == "x"),
        "{argv:?}"
    );
}

/// `ainb run` writes to files under the daemon's log dir rather than pipes (a
/// pipe the daemon stops reading kills the run on its next write). The files
/// are removed once the outcome is read, so creates do not pile up output.
#[tokio::test]
async fn a_create_leaves_no_run_output_behind() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let repo = Registered::new();
    switch_on(&fake_ainb(tools.path(), true));

    let response = call(serde_json::json!({ "repo_path": repo.repo(), "agent": "claude" })).await;
    assert_eq!(
        response["result"]["tmux_session_name"], "app-11111111",
        "{response}"
    );
    let spawn_logs = repo.home.path().join(".agents-in-a-box/hangar/logs/spawn");
    assert!(
        spawn_logs.is_dir(),
        "the run's output went to {}",
        spawn_logs.display()
    );
    let left: Vec<_> = std::fs::read_dir(&spawn_logs)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    assert!(left.is_empty(), "run output left behind: {left:?}");
}
