//! `shell/create` with the switch on, end to end through dispatch, against
//! real git and a real tmux server.
//!
//! The server is PRIVATE: `TMUX_TMPDIR` points at a per-test directory and
//! `TMUX` is removed, so the daemon's own `tmux new-session` lands on a server
//! only this test can see. Every session a test made is killed by its exact
//! name (`=<name>`) on that server, never the server itself.
//!
//! Its own process: `AINB_HANGAR_SPAWN`, `HOME` and `TMUX_TMPDIR` are
//! process-global and the switch is read once, so they are set before the
//! first dispatch and the tests run serially on one lock.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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
        socket_path: "/tmp/shell-verbs.sock".to_string(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

/// Whether tmux is here to test against. Missing under CI (`CI` set) is a
/// failure, not a skip: a job that cannot run these must not report them
/// green. Only a local run without tmux skips.
fn tmux_available() -> bool {
    let here = Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success());
    assert!(
        here || std::env::var_os("CI").is_none(),
        "tmux is missing under CI, so these tests cannot run"
    );
    here
}

async fn call(params: serde_json::Value) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let request = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(7),
        method: m::SHELL_CREATE.to_string(),
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

fn git(cwd: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(ok, "git {args:?}");
}

/// One isolated world: a home whose config registers `<home>/code`, a
/// repository at `<home>/code/app`, a linked worktree of it where
/// `ainb run --worktree` puts them, and a private tmux server dir. Sets
/// `HOME`, `TMUX_TMPDIR` and the switch for the daemon; held under `SERIAL`.
struct World {
    home: tempfile::TempDir,
    tmux_dir: PathBuf,
    made: Vec<String>,
}

impl World {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("code/app");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let managed = home.path().join(".agents-in-a-box/worktrees/by-name");
        std::fs::create_dir_all(&managed).unwrap();
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                &managed.join("app--feat--1a2b3c4d").display().to_string(),
            ],
        );
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

        // Under /tmp: macOS caps unix socket paths at 104 bytes.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let tmux_dir =
            PathBuf::from("/tmp").join(format!("ainb-shell-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&tmux_dir).unwrap();

        // Edition 2021: set_var is safe. Held under SERIAL for the whole test.
        std::env::set_var("HOME", home.path());
        std::env::remove_var("AINB_HOME");
        std::env::set_var("TMUX_TMPDIR", &tmux_dir);
        std::env::remove_var("TMUX");
        // tmux reads `$XDG_CONFIG_HOME/tmux/tmux.conf` too; a developer's
        // could set a `default-shell` or hooks that move the pane.
        std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
        // A plain shell with no startup files: a developer's rc could `cd`
        // somewhere else and move the pane off the directory under test.
        std::env::set_var("SHELL", "/bin/sh");
        std::env::set_var(ainb_hangar_daemon::spawn::SPAWN_ENV, "1");
        Self {
            home,
            tmux_dir,
            made: Vec::new(),
        }
    }

    fn repo(&self) -> String {
        canonical(&self.home.path().join("code/app"))
    }

    fn worktree(&self) -> String {
        canonical(&self.home.path().join(".agents-in-a-box/worktrees/by-name/app--feat--1a2b3c4d"))
    }

    fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .env("TMUX_TMPDIR", &self.tmux_dir)
            .env_remove("TMUX")
            .args(args)
            .output()
            .expect("run tmux")
    }

    /// Create a shell, remember its session for cleanup, and return the
    /// response.
    async fn shell(&mut self, path: &str) -> serde_json::Value {
        let response = call(serde_json::json!({ "worktree_path": path })).await;
        if let Some(name) = response["result"]["tmux_session_name"].as_str() {
            self.made.push(name.to_string());
        }
        response
    }

    fn alive(&self, name: &str) -> bool {
        self.tmux(&["has-session", "-t", &format!("={name}")]).status.success()
    }

    /// Every session on the private server; empty when no server runs.
    fn sessions(&self) -> Vec<String> {
        let out = self.tmux(&["list-sessions", "-F", "#{session_name}"]);
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
    }

    fn pane_path(&self, name: &str) -> String {
        let out = self.tmux(&[
            "display-message",
            "-p",
            "-t",
            &format!("={name}:"),
            "#{pane_current_path}",
        ]);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        for name in &self.made {
            let _ = self.tmux(&["kill-session", "-t", &format!("={name}")]);
        }
        let _ = std::fs::remove_dir_all(&self.tmux_dir);
    }
}

fn canonical(path: &Path) -> String {
    std::fs::canonicalize(path).unwrap().display().to_string()
}

#[tokio::test]
async fn a_shell_opens_in_the_requested_worktree() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    let tree = world.worktree();

    let response = world.shell(&tree).await;

    let name = response["result"]["tmux_session_name"]
        .as_str()
        .unwrap_or_else(|| panic!("a shell was made: {response}"))
        .to_string();
    assert!(name.starts_with("ainb-sh-"), "{name}");
    assert_eq!(name.len(), "ainb-sh-".len() + 8, "{name}");
    assert_eq!(response["result"]["worktree_path"], tree.as_str());
    assert!(world.alive(&name));
    assert_eq!(world.pane_path(&name), tree, "the shell starts in the tree");
}

#[tokio::test]
async fn a_shell_opens_in_a_registered_repository_top() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    let repo = world.repo();

    let response = world.shell(&repo).await;

    let name = response["result"]["tmux_session_name"]
        .as_str()
        .unwrap_or_else(|| panic!("a shell was made: {response}"))
        .to_string();
    assert_eq!(world.pane_path(&name), repo);
}

/// tmux reads `-c` as a format. A registered folder whose name holds one is
/// still the folder the shell opens in, not what the format expands to.
#[tokio::test]
async fn a_folder_name_that_reads_as_a_tmux_format_is_taken_literally() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    let odd = world.home.path().join("code/c#{session_name}");
    std::fs::create_dir_all(&odd).unwrap();
    git(&odd, &["init", "-q", "-b", "main"]);
    let odd = canonical(&odd);

    let response = world.shell(&odd).await;

    let name = response["result"]["tmux_session_name"]
        .as_str()
        .unwrap_or_else(|| panic!("a shell was made: {response}"))
        .to_string();
    assert_eq!(response["result"]["worktree_path"], odd.as_str());
    assert_eq!(
        world.pane_path(&name),
        odd,
        "the literal folder, not a format"
    );
}

/// A directory outside every registered folder, or one reached through
/// `..`, is refused before tmux runs: no session, not even a server.
#[tokio::test]
async fn a_path_outside_the_registered_folders_or_with_dots_is_refused() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    let elsewhere = tempfile::tempdir().unwrap();
    git(elsewhere.path(), &["init", "-q"]);
    let dotted = format!("{}/../app", world.repo());

    for path in [elsewhere.path().display().to_string(), dotted] {
        let response = world.shell(&path).await;
        assert_eq!(
            response["error"]["code"].as_i64(),
            Some(-32602),
            "{path}: {response}"
        );
    }
    assert!(world.sessions().is_empty(), "no session was made");
}

/// A second shell is a second session: nothing reuses, attaches to or
/// replaces the first.
#[tokio::test]
async fn two_shells_are_two_sessions_and_the_first_survives() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    let tree = world.worktree();

    let first = world.shell(&tree).await;
    let second = world.shell(&tree).await;

    let first = first["result"]["tmux_session_name"].as_str().unwrap_or_default().to_string();
    let second = second["result"]["tmux_session_name"].as_str().unwrap_or_default().to_string();
    assert!(!first.is_empty() && !second.is_empty(), "both were made");
    assert_ne!(first, second);
    assert!(world.alive(&first), "the first shell survives the second");
    assert!(world.alive(&second));
}

/// The first shell can start the tmux server, and a server keeps the
/// environment it started with for every pane after. The daemon's credential
/// must not be in it.
#[tokio::test]
async fn a_shell_that_starts_the_tmux_server_does_not_hand_it_the_daemon_secrets() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    assert!(world.sessions().is_empty(), "this shell starts the server");
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
    std::env::set_var("HANGAR_CLAUDE_OAUTH_TOKEN", "sk-ant-oat-shell-verbs-test");
    std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat-shell-verbs-test");

    let tree = world.worktree();
    let response = world.shell(&tree).await;
    std::env::remove_var("HANGAR_CLAUDE_OAUTH_TOKEN");
    std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");

    let name = response["result"]["tmux_session_name"]
        .as_str()
        .unwrap_or_else(|| panic!("a shell was made: {response}"))
        .to_string();
    let session = world.tmux(&["show-environment", "-t", &format!("={name}")]);
    let session = String::from_utf8_lossy(&session.stdout);
    // `-NAME` is tmux marking the variable removed from the session, which
    // is the scrub itself; only a `NAME=value` line would carry it.
    for secret in [
        "HANGAR_CLAUDE_OAUTH_TOKEN=",
        "CLAUDE_CODE_OAUTH_TOKEN=",
        "sk-ant-oat",
    ] {
        assert!(!session.contains(secret), "{secret} reached the session");
    }
    let global = world.tmux(&["show-environment", "-g"]);
    assert!(global.status.success(), "the server is up");
    let global = String::from_utf8_lossy(&global.stdout);
    assert!(
        global.lines().any(|line| line.starts_with("HOME=")),
        "the server's environment was read: {global}"
    );
    for secret in [
        "HANGAR_CLAUDE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "sk-ant-oat",
    ] {
        assert!(!global.contains(secret), "{secret} reached the tmux server");
    }
}
