//! `shell/create`, `shell/list` and `shell/close` with the switch on, end to
//! end through dispatch, against real git and a real tmux server.
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
    call_method(m::SHELL_CREATE, params).await
}

async fn call_method(method: &str, params: serde_json::Value) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    call_in(dir.path(), method, params).await
}

/// Dispatch against the daemon store in `dir`, so calls that share a `dir`
/// share the D18 ledger, as a real daemon's do.
async fn call_in(dir: &Path, method: &str, params: serde_json::Value) -> serde_json::Value {
    let store = Store::open_in(dir).await.unwrap();
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
        // No UTF-8 locale, as a daemon launchd started has none: tmux then
        // mangles tabs and non-ASCII in its formats unless it is told `-u`.
        for locale in ["LANG", "LC_ALL", "LC_CTYPE"] {
            std::env::remove_var(locale);
        }
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
            // The locale is gone (see `new`): read names and paths as UTF-8.
            .arg("-u")
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

    /// Create a shell with `op_id` against this world's one daemon store (so
    /// a retry meets the ledger the first call wrote), remembering its
    /// session for cleanup.
    async fn shell_with_op(&mut self, path: &str, op_id: &str) -> serde_json::Value {
        let ledger = self.home.path().join("daemon-store");
        std::fs::create_dir_all(&ledger).unwrap();
        let response = call_in(
            &ledger,
            m::SHELL_CREATE,
            serde_json::json!({ "worktree_path": path, "op_id": op_id }),
        )
        .await;
        if let Some(name) = response["result"]["tmux_session_name"].as_str() {
            self.made.push(name.to_string());
        }
        response
    }

    /// A session made directly on the private server, remembered for
    /// cleanup: one the daemon did not open.
    fn foreign(&mut self, name: &str) {
        let dir = self.home.path().display().to_string();
        let out = self.tmux(&["new-session", "-d", "-s", name, "-c", &dir]);
        assert!(out.status.success(), "{out:?}");
        self.made.push(name.to_string());
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
    assert!(
        ainb_hangar_proto::spawn::is_daemon_shell_name(&name),
        "a daemon shell has the daemon's own prefix, not the TUI's: {name}"
    );
    assert!(name.starts_with("ainb-dsh-"), "{name}");
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

/// A tmux server some other process started WITH the daemon's token in its
/// environment, then shell/create on it: the shell's pane holds neither
/// token, and the session marks both removed for later windows.
#[tokio::test]
async fn a_shell_on_a_server_already_holding_the_token_gets_neither() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        return;
    }
    let mut world = World::new();
    const SECRETS: [(&str, &str); 2] = [
        ("HANGAR_CLAUDE_OAUTH_TOKEN", "sk-ant-oat-r2-override"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat-r2-child"),
    ];
    let base = format!("r2-base-{}", std::process::id());
    world.made.push(base.clone());
    let started = Command::new("tmux")
        .env("TMUX_TMPDIR", &world.tmux_dir)
        .env_remove("TMUX")
        .envs(SECRETS)
        .args(["new-session", "-d", "-s", &base])
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    let global = world.tmux(&["show-environment", "-g"]);
    assert!(
        String::from_utf8_lossy(&global.stdout).contains(SECRETS[0].1),
        "the server holds the token"
    );

    let tree = world.worktree();
    let response = world.shell(&tree).await;
    let name = response["result"]["tmux_session_name"]
        .as_str()
        .unwrap_or_else(|| panic!("a shell was made: {response}"))
        .to_string();

    let dump = world.home.path().join("r2.env");
    let keys = format!("env > '{}'", dump.display());
    let target = format!("={name}:");
    assert!(world.tmux(&["send-keys", "-t", &target, "-l", &keys]).status.success());
    assert!(world.tmux(&["send-keys", "-t", &target, "Enter"]).status.success());
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    let env = loop {
        let env = std::fs::read_to_string(&dump).unwrap_or_default();
        if env.contains("PATH=") {
            break env;
        }
        assert!(Instant::now() < deadline, "the shell never ran the dump");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    for (secret, value) in SECRETS {
        assert!(
            !env.contains(secret) && !env.contains(value),
            "{secret} reached the shell's pane"
        );
    }
    let marked = world.tmux(&["show-environment", "-t", &format!("={name}")]);
    let marked = String::from_utf8_lossy(&marked.stdout);
    for (secret, _) in SECRETS {
        assert!(
            marked.contains(&format!("-{secret}")),
            "the session keeps {secret}: {marked}"
        );
    }
}
/// The shell a create answered with, without the ledger's ack.
fn shell_of(response: &serde_json::Value) -> (serde_json::Value, serde_json::Value) {
    (
        response["result"]["tmux_session_name"].clone(),
        response["result"]["worktree_path"].clone(),
    )
}

fn shell_name(response: &serde_json::Value) -> String {
    response["result"]["tmux_session_name"]
        .as_str()
        .unwrap_or_else(|| panic!("a shell was made: {response}"))
        .to_string()
}

/// `shell/list` shows the daemon's shells with their folders, and never the
/// TUI's shells or a session the user made.
#[tokio::test]
async fn the_list_shows_the_daemon_shells_and_nothing_else() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    let (tree, repo) = (world.worktree(), world.repo());
    let in_tree = shell_name(&world.shell(&tree).await);
    let in_repo = shell_name(&world.shell(&repo).await);
    world.foreign("ainb-sh-0a1b2c3d");
    world.foreign("work");

    let listed = call_method(m::SHELL_LIST, serde_json::json!({})).await;

    let mut expected = vec![
        serde_json::json!({ "tmux_session_name": in_tree, "worktree_path": tree }),
        serde_json::json!({ "tmux_session_name": in_repo, "worktree_path": repo }),
    ];
    expected.sort_by_key(|shell| shell["tmux_session_name"].as_str().unwrap().to_string());
    assert_eq!(
        listed["result"]["shells"],
        serde_json::Value::Array(expected),
        "{listed}"
    );
}

/// No tmux server at all is no shells, not an error.
#[tokio::test]
async fn the_list_is_empty_without_a_tmux_server() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let world = World::new();
    assert!(world.sessions().is_empty());

    let listed = call_method(m::SHELL_LIST, serde_json::json!({})).await;

    assert_eq!(
        listed["result"]["shells"],
        serde_json::json!([]),
        "{listed}"
    );
}

/// `shell/close` ends exactly the named shell. A second close of it answers
/// `closed: false`, so a retry reads the same as the first.
#[tokio::test]
async fn a_close_ends_exactly_the_named_shell() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    let tree = world.worktree();
    let first = shell_name(&world.shell(&tree).await);
    let second = shell_name(&world.shell(&tree).await);

    let closed = call_method(
        m::SHELL_CLOSE,
        serde_json::json!({ "tmux_session_name": first }),
    )
    .await;
    assert_eq!(
        closed["result"],
        serde_json::json!({ "closed": true }),
        "{closed}"
    );
    assert!(!world.alive(&first));
    assert!(world.alive(&second), "closing one shell ended another");

    let again = call_method(
        m::SHELL_CLOSE,
        serde_json::json!({ "tmux_session_name": first }),
    )
    .await;
    assert_eq!(
        again["result"],
        serde_json::json!({ "closed": false }),
        "{again}"
    );
}

/// A close reaches only a daemon shell, by its exact name: the TUI's shell
/// is refused before tmux runs, and a longer name sharing the prefix is not
/// matched the way a bare `-t` would match it.
#[tokio::test]
async fn a_close_never_reaches_a_session_the_daemon_did_not_open() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    world.foreign("ainb-sh-0a1b2c3d");
    world.foreign("ainb-dsh-0a1b2c3d-work");
    world.foreign("main");

    for name in ["ainb-sh-0a1b2c3d", "main", "ainb-dsh-0a1b2c3d-work"] {
        let refused = call_method(
            m::SHELL_CLOSE,
            serde_json::json!({ "tmux_session_name": name }),
        )
        .await;
        assert_eq!(
            refused["error"]["code"].as_i64(),
            Some(-32602),
            "{name}: {refused}"
        );
    }
    let prefix = call_method(
        m::SHELL_CLOSE,
        serde_json::json!({ "tmux_session_name": "ainb-dsh-0a1b2c3d" }),
    )
    .await;
    assert_eq!(
        prefix["result"],
        serde_json::json!({ "closed": false }),
        "{prefix}"
    );
    for name in ["ainb-sh-0a1b2c3d", "ainb-dsh-0a1b2c3d-work", "main"] {
        assert!(world.alive(name), "{name} was closed");
    }
}

/// A session under a daemon shell's exact name that the daemon did not open
/// (no `@ainb_owner`) is neither listed nor closed; the daemon's own shell,
/// which carries the option from the moment it exists, is both.
#[tokio::test]
async fn a_same_prefix_session_without_the_owner_is_neither_listed_nor_closed() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    let tree = world.worktree();
    let ours = shell_name(&world.shell(&tree).await);
    world.foreign("ainb-dsh-0a1b2c3d");

    let listed = call_method(m::SHELL_LIST, serde_json::json!({})).await;
    assert_eq!(
        listed["result"]["shells"],
        serde_json::json!([{ "tmux_session_name": ours, "worktree_path": tree }]),
        "{listed}"
    );

    let refused = call_method(
        m::SHELL_CLOSE,
        serde_json::json!({ "tmux_session_name": "ainb-dsh-0a1b2c3d" }),
    )
    .await;
    assert_eq!(
        refused["result"],
        serde_json::json!({ "closed": false }),
        "{refused}"
    );
    assert!(
        world.alive("ainb-dsh-0a1b2c3d"),
        "the user's session was closed"
    );

    let owner = world.tmux(&[
        "show-options",
        "-qv",
        "-t",
        &format!("={ours}:"),
        "@ainb_owner",
    ]);
    assert_eq!(
        String::from_utf8_lossy(&owner.stdout).trim(),
        "daemon",
        "{owner:?}"
    );

    let closed = call_method(
        m::SHELL_CLOSE,
        serde_json::json!({ "tmux_session_name": ours }),
    )
    .await;
    assert_eq!(
        closed["result"],
        serde_json::json!({ "closed": true }),
        "{closed}"
    );
    assert!(!world.alive(&ours));
}

/// The owner is read from the session itself. A `set -g @ainb_owner daemon`
/// (a tmux.conf line, or any other tool on the server) is a global option,
/// which a `#{@ainb_owner}` format falls back to: read that way it would
/// mark every `ainb-dsh-` session on the server as the daemon's.
#[tokio::test]
async fn a_global_owner_option_does_not_make_a_foreign_shell_ours() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    let tree = world.worktree();
    let ours = shell_name(&world.shell(&tree).await);
    world.foreign("ainb-dsh-0a1b2c3d");
    let global = world.tmux(&["set-option", "-g", "@ainb_owner", "daemon"]);
    assert!(global.status.success(), "{global:?}");

    let listed = call_method(m::SHELL_LIST, serde_json::json!({})).await;
    assert_eq!(
        listed["result"]["shells"],
        serde_json::json!([{ "tmux_session_name": ours, "worktree_path": tree }]),
        "{listed}"
    );

    let refused = call_method(
        m::SHELL_CLOSE,
        serde_json::json!({ "tmux_session_name": "ainb-dsh-0a1b2c3d" }),
    )
    .await;
    assert_eq!(
        refused["result"],
        serde_json::json!({ "closed": false }),
        "{refused}"
    );
    assert!(
        world.alive("ainb-dsh-0a1b2c3d"),
        "a session marked only by the global option was closed"
    );
}

/// A create retried with the same op id (a lost reply) replays the shell the
/// first attempt made, through the D18 ledger, and opens no second one. The
/// same op id for another folder is rejected; a new op id is a new shell.
#[tokio::test]
async fn a_retried_create_with_the_same_op_id_returns_the_same_shell() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    let (tree, repo) = (world.worktree(), world.repo());

    let first = world.shell_with_op(&tree, "0123456789abcdef0123456789abcdef").await;
    let retry = world.shell_with_op(&tree, "0123456789abcdef0123456789abcdef").await;

    assert_eq!(shell_of(&first), shell_of(&retry), "{first} / {retry}");
    assert_eq!(
        retry["result"]["mutation"]["outcome"], "replayed",
        "{retry}"
    );
    assert_eq!(
        world.sessions(),
        vec![shell_name(&first)],
        "one shell, not two"
    );

    let elsewhere = world.shell_with_op(&repo, "0123456789abcdef0123456789abcdef").await;
    assert_eq!(
        elsewhere["error"]["code"].as_i64(),
        Some(i64::from(ainb_hangar_proto::mutation::MUTATION_REJECTED)),
        "{elsewhere}"
    );

    let other = world.shell_with_op(&tree, "fedcba9876543210fedcba9876543210").await;
    assert_ne!(shell_name(&other), shell_name(&first));
    assert_eq!(world.sessions().len(), 2);
}

/// A name already taken is never attached to or replaced: the create moves
/// on to a fresh one, and gives up after its tries with the taken session
/// untouched.
#[tokio::test]
async fn a_taken_shell_name_is_retried_with_a_fresh_one() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    world.foreign("ainb-dsh-aaaaaaaa");
    let params = ainb_hangar_proto::spawn::ShellCreateParams {
        worktree_path: world.worktree(),
        mutation: ainb_hangar_proto::mutation::MutationEnvelope::default(),
    };

    let mut ids = ["aaaaaaaa", "bbbbbbbb"].into_iter().map(str::to_string);
    let made = ainb_hangar_daemon::spawn::shell_create_named(&params, || ids.next().unwrap())
        .await
        .expect("a fresh name after the taken one");
    world.made.push(made.tmux_session_name.clone());
    assert_eq!(made.tmux_session_name, "ainb-dsh-bbbbbbbb");

    let refused =
        ainb_hangar_daemon::spawn::shell_create_named(&params, || "aaaaaaaa".into()).await;
    assert!(
        matches!(&refused, Err(ainb_hangar_daemon::spawn::SpawnError::Failed(why)) if why.contains("duplicates")),
        "{refused:?}"
    );
    assert!(world.alive("ainb-dsh-aaaaaaaa"));
    assert_eq!(
        world.pane_path("ainb-dsh-aaaaaaaa"),
        canonical(world.home.path())
    );
}

/// A folder whose name is not ASCII and ends in `;`: tmux would read the
/// `;` as the end of the command and, without a UTF-8 locale, hand the name
/// back mangled. The shell opens there, lists there, and a retried create
/// with its op id finds it there.
#[tokio::test]
async fn an_odd_folder_name_opens_lists_and_replays_as_itself() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }
    let mut world = World::new();
    let sibling = world.home.path().join("code/caf\u{e9}");
    std::fs::create_dir_all(&sibling).unwrap();
    git(&sibling, &["init", "-q", "-b", "main"]);
    let odd = world.home.path().join("code/caf\u{e9};");
    std::fs::create_dir_all(&odd).unwrap();
    git(&odd, &["init", "-q", "-b", "main"]);
    let odd = canonical(&odd);

    let first = world.shell_with_op(&odd, "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a").await;
    let name = shell_name(&first);
    assert_eq!(first["result"]["worktree_path"], odd.as_str());
    assert_eq!(
        world.pane_path(&name),
        odd,
        "not the sibling without the `;`"
    );

    let listed = call_method(m::SHELL_LIST, serde_json::json!({})).await;
    assert_eq!(
        listed["result"]["shells"],
        serde_json::json!([{ "tmux_session_name": name, "worktree_path": odd }]),
        "{listed}"
    );

    let retry = world.shell_with_op(&odd, "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a").await;
    assert_eq!(shell_of(&retry), shell_of(&first), "{retry}");
    assert_eq!(world.sessions(), vec![name]);
}
