//! `worktree/create` and `worktree/agent_add` as a daemon serves them by default, end to end
//! through dispatch, against a stand-in `ainb` that records its argv and
//! answers like `ainb --format json run`.
//!
//! The real `ainb --format json run` contract (worktree from `--base`, one JSON line)
//! is proven in `crates/ainb-core/tests/run_json_worktree.rs`. This binary
//! proves the daemon half: the request becomes the right argv, the one JSON
//! line becomes the result, and a failed run becomes an error carrying the
//! CLI's own last words.
//!
//! Its own process: `AINB_HANGAR_SPAWN` and `AINB_BIN` are process-global and
//! the switch is read once, so they are settled before the first dispatch and
//! the tests run serially on one lock. The switch is left UNSET: that the
//! verbs answer at all is the default being proven.

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

/// A fake `ainb`: writes its environment to `env.txt` and its argv (one per
/// line) to `argv.txt`, then either
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
            "#!/bin/sh\nenv > '{env}'\n: > '{argv}'\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{argv}'; done\n{body}\n",
            argv = argv_file.display(),
            env = dir.join("env.txt").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

async fn call(params: serde_json::Value) -> serde_json::Value {
    call_method(m::WORKTREE_CREATE, params).await
}

async fn call_method(method: &str, params: serde_json::Value) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let request = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(7),
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
        // The managed worktree directory follows `$AINB_HOME` when it is set;
        // these tests place their trees under `$HOME`.
        std::env::remove_var("AINB_HOME");
        Self { home }
    }

    /// A linked worktree of `repo` (on a new branch) at `by-name/<dir>`, where
    /// `ainb run --worktree` puts the trees it makes.
    fn managed_worktree(&self, repo: &Path, dir: &str, branch: &str) -> String {
        let managed = self.home.path().join(".agents-in-a-box/worktrees/by-name");
        std::fs::create_dir_all(&managed).unwrap();
        let tree = managed.join(dir);
        let ok = std::process::Command::new("git")
            .args(["worktree", "add", "-q", "-b", branch])
            .arg(&tree)
            .current_dir(repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "git worktree add {}", tree.display());
        std::fs::canonicalize(tree).unwrap().display().to_string()
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
    // Unset, not `1`: the verbs are on by default.
    std::env::remove_var(ainb_hangar_daemon::spawn::SPAWN_ENV);
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

/// `ainb run` starts tmux itself, and a tmux server keeps the environment it
/// started with for every pane after, so the daemon's OAuth token must not
/// reach `ainb run` at all.
#[tokio::test]
async fn ainb_run_gets_no_daemon_oauth_token() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let repo = Registered::new();
    switch_on(&fake_ainb(tools.path(), true));
    // Taken back out on the way out, panic or not, so no later test in this
    // process inherits them.
    struct Unset;
    impl Drop for Unset {
        fn drop(&mut self) {
            std::env::remove_var("HANGAR_CLAUDE_OAUTH_TOKEN");
            std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
        }
    }
    let _unset = Unset;
    std::env::set_var(
        "HANGAR_CLAUDE_OAUTH_TOKEN",
        "sk-ant-oat-spawn-verbs-override",
    );
    std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat-spawn-verbs-child");

    let response = call(serde_json::json!({ "repo_path": repo.repo(), "agent": "claude" })).await;

    assert_eq!(
        response["result"]["tmux_session_name"], "app-11111111",
        "{response}"
    );
    let env = std::fs::read_to_string(tools.path().join("env.txt")).unwrap();
    assert!(
        env.lines().any(|line| line.starts_with("PATH=")),
        "the env was read: {env}"
    );
    for secret in [
        "HANGAR_CLAUDE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "sk-ant-oat",
    ] {
        assert!(!env.contains(secret), "{secret} reached `ainb run`");
    }
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

    // Its own code, so a client offers the fix without reading the sentence.
    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(i64::from(ainb_hangar_proto::spawn::REPO_NOT_REGISTERED)),
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

/// The run's output files under `home`: `(name, mode)` for each.
fn run_output(home: &Path) -> Vec<(String, u32)> {
    let dir = home.join(".agents-in-a-box/hangar/logs/spawn");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            let mode = entry.metadata().map_or(0, |m| m.permissions().mode() & 0o777);
            (entry.file_name().to_string_lossy().into_owned(), mode)
        })
        .collect();
    files.sort();
    files
}

/// A create that times out leaves `ainb run` writing to its files. A run still
/// working past the timeout (the prompt wait, a slow first thread) writes
/// later; if its output had gone to pipes the daemon stopped reading, that
/// write would kill it on EPIPE before its own rollback, leaving the tmux
/// session, worktree and branch behind. The stand-in writes to both streams
/// after the timeout, leaves its marker, and holds until released, so the
/// files are read while the run is live: private to the daemon's user, with
/// the late lines in them. Once it exits, nothing is left behind.
#[tokio::test]
async fn a_late_write_after_timeout_lands_in_the_file() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let repo = Registered::new();
    let marker = tools.path().join("lived.txt");
    let release = tools.path().join("release.txt");
    let bin = tools.path().join("ainb");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nsleep 1\necho 'late progress' >&2\necho 'late line'\necho 'more progress' >&2\n: > '{marker}'\n\
             n=0; while [ ! -e '{release}' ] && [ $n -lt 200 ]; do sleep 0.05; n=$((n+1)); done\n",
            marker = marker.display(),
            release = release.display()
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

    // Live: the late lines are in the files, and only the daemon's user can
    // read them or list the directory they sit in.
    let spawn_logs = repo.home.path().join(".agents-in-a-box/hangar/logs/spawn");
    let dir_mode = std::fs::metadata(&spawn_logs).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "{} is {dir_mode:o}", spawn_logs.display());
    let live = run_output(repo.home.path());
    assert_eq!(live.len(), 2, "one stdout and one stderr file: {live:?}");
    for (name, mode) in &live {
        assert_eq!(*mode, 0o600, "{name} is {mode:o} while the run is live");
        let text = std::fs::read_to_string(spawn_logs.join(name)).unwrap();
        let late = if name.ends_with(".stdout") {
            "late line"
        } else {
            "more progress"
        };
        assert!(text.contains(late), "{name} lost the late write: {text:?}");
    }

    std::fs::write(&release, "").unwrap();
    while !run_output(repo.home.path()).is_empty() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        run_output(repo.home.path()),
        Vec::<(String, u32)>::new(),
        "the run's output outlived the run"
    );
}

/// An `ainb` that cannot be started still had its output files opened for it;
/// the error arm removes them, or every failed create would leave two behind.
#[tokio::test]
async fn a_run_that_cannot_start_leaves_no_run_output_behind() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let repo = Registered::new();
    switch_on(&tools.path().join("no-such-ainb"));

    let response = call(serde_json::json!({ "repo_path": repo.repo(), "agent": "claude" })).await;
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("could not run")),
        "{response}"
    );
    assert_eq!(
        run_output(repo.home.path()),
        Vec::<(String, u32)>::new(),
        "a failed spawn left its output files"
    );
}

/// `logs/spawn` swapped for a symlink to another directory: the create is
/// refused with a clear error before `ainb` runs, and nothing is written
/// where the link points.
#[tokio::test]
async fn a_create_refuses_a_spawn_log_dir_that_is_a_symlink() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let repo = Registered::new();
    let logs = repo.home.path().join(".agents-in-a-box/hangar/logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), logs.join("spawn")).unwrap();
    switch_on(&fake_ainb(tools.path(), true));

    let response = call(serde_json::json!({ "repo_path": repo.repo(), "agent": "claude" })).await;
    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("not a private directory"), "{response}");
    assert!(!tools.path().join("argv.txt").exists(), "ainb ran anyway");
    let written: Vec<_> = std::fs::read_dir(elsewhere.path()).unwrap().collect();
    assert!(
        written.is_empty(),
        "run output went through the link: {written:?}"
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

#[tokio::test]
async fn an_agent_add_runs_ainb_run_in_the_existing_worktree() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let registered = Registered::new();
    let tree =
        registered.managed_worktree(Path::new(&registered.repo()), "app--feat--1a2b3c4d", "feat");
    switch_on(&fake_ainb(tools.path(), true));

    let response = call_method(
        m::WORKTREE_AGENT_ADD,
        serde_json::json!({
            "worktree_path": tree,
            "agent": "codex",
            "model": "-gpt",
            "skip_permissions": true,
            "prompt": "-y review it",
        }),
    )
    .await;

    assert_eq!(
        response["result"]["tmux_session_name"], "app-11111111",
        "{response}"
    );
    let argv = std::fs::read_to_string(tools.path().join("argv.txt")).unwrap();
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(
        &argv[..7],
        [
            "--format",
            "json",
            "run",
            "--existing-worktree",
            tree.as_str(),
            "--tool",
            "codex"
        ],
        "{argv:?}"
    );
    assert!(argv.contains(&"--model=-gpt"), "{argv:?}");
    assert!(argv.contains(&"--dangerously-skip-permissions"), "{argv:?}");
    assert_eq!(argv.last(), Some(&"--prompt=-y review it"));
    for create_only in ["--worktree", "--repo", "--create-branch", "--base"] {
        assert!(!argv.contains(&create_only), "{create_only} in {argv:?}");
    }
}

/// An agent joins only a tree ainb made from a registered repository: the
/// repository's own checkout, and a managed-looking tree cut from a
/// repository outside every registered folder, are refused before `ainb`
/// runs. The unregistered source answers `REPO_NOT_REGISTERED`, as
/// `worktree/create` does, so a client offers Add project by code.
#[tokio::test]
async fn an_agent_add_refuses_a_main_checkout_or_an_unregistered_repos_worktree() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tools = tempfile::tempdir().unwrap();
    let registered = Registered::new();
    let elsewhere = tempfile::tempdir().unwrap();
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
            .current_dir(elsewhere.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "git {args:?}");
    }
    let stray = registered.managed_worktree(elsewhere.path(), "stray--x--00000000", "x");
    // A whole repository sitting where a managed tree would: in the right
    // place, but a main checkout, not a linked worktree.
    let planted = registered
        .home
        .path()
        .join(".agents-in-a-box/worktrees/by-name/planted--main--00000000");
    std::fs::create_dir_all(&planted).unwrap();
    let ok = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&planted)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(ok, "git init {}", planted.display());
    let planted = std::fs::canonicalize(planted).unwrap().display().to_string();
    switch_on(&fake_ainb(tools.path(), true));

    for (path, why, code) in [
        (registered.repo(), "directly in", -32602),
        (planted, "main checkout", -32602),
        (
            stray,
            "registered",
            ainb_hangar_proto::spawn::REPO_NOT_REGISTERED,
        ),
    ] {
        let response = call_method(
            m::WORKTREE_AGENT_ADD,
            serde_json::json!({ "worktree_path": path, "agent": "claude" }),
        )
        .await;
        assert_eq!(
            response["error"]["code"].as_i64(),
            Some(i64::from(code)),
            "{path}: {response}"
        );
        assert!(
            response["error"]["message"].as_str().is_some_and(|m| m.contains(why)),
            "{path}: {response}"
        );
        assert!(
            !tools.path().join("argv.txt").exists(),
            "nothing ran for {path}"
        );
    }
}
