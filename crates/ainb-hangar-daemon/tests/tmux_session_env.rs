//! `tmux_session::tmux_new_session` keeps the daemon's OAuth token out of
//! the panes it makes, whoever started the tmux server: a server that
//! already holds the token in its environment, or one the helper starts
//! from a daemon that holds it.
//!
//! Its own process, on a PRIVATE tmux server: `TMUX_TMPDIR` points at a
//! per-test directory under `/tmp` and `TMUX` is removed, so the helper's
//! bare `tmux` lands on a server only this test can see. Every session is
//! killed by its exact name on that socket, never the server.

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ainb_hangar_daemon::tmux_session::tmux_new_session;

static SERIAL: Mutex<()> = Mutex::new(());

const SECRETS: [(&str, &str); 2] = [
    (
        "HANGAR_CLAUDE_OAUTH_TOKEN",
        "sk-ant-oat-tmux-session-override",
    ),
    ("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat-tmux-session-child"),
];

/// Missing under CI is a failure, not a skip; only a local run skips.
fn tmux_available() -> bool {
    let here = Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success());
    assert!(
        here || std::env::var_os("CI").is_none(),
        "tmux is missing under CI, so these tests cannot run"
    );
    if !here {
        eprintln!("skipping: tmux not available");
    }
    here
}

/// A private tmux world: its own `TMUX_TMPDIR` for the helper, the explicit
/// socket inside it for this test's own calls, and the sessions it made.
struct World {
    dir: tempfile::TempDir,
    socket: PathBuf,
    made: Vec<String>,
}

impl World {
    fn new() -> Self {
        // Under /tmp: macOS caps unix socket paths at 104 bytes.
        let dir = tempfile::Builder::new().prefix("ainb-tse-").tempdir_in("/tmp").unwrap();
        let uid = std::fs::metadata(dir.path()).unwrap().uid();
        let socket_dir = dir.path().join(format!("tmux-{uid}"));
        std::fs::create_dir_all(&socket_dir).unwrap();
        std::fs::set_permissions(
            &socket_dir,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        // Edition 2021: set_var is safe. Held under SERIAL for the whole test.
        std::env::set_var("TMUX_TMPDIR", dir.path());
        std::env::remove_var("TMUX");
        // A plain shell with no startup files, and no tmux.conf of the
        // developer's: either could set the pane's environment itself.
        std::env::set_var("SHELL", "/bin/sh");
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config"));
        std::env::set_var("HOME", dir.path());
        let socket = socket_dir.join("default");
        Self {
            dir,
            socket,
            made: Vec::new(),
        }
    }

    fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .env_remove("TMUX")
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .output()
            .expect("run tmux")
    }

    /// Start the server from a client that holds both secrets, the way a
    /// server some other path started would hold them.
    fn start_server_with_secrets(&mut self) {
        let name = format!("tse-base-{}", std::process::id());
        let out = Command::new("tmux")
            .env_remove("TMUX")
            .envs(SECRETS)
            .arg("-S")
            .arg(&self.socket)
            .args(["new-session", "-d", "-s", &name])
            .output()
            .unwrap();
        // Remembered before the check, so a half-made session is still killed.
        self.made.push(name);
        assert!(out.status.success(), "{out:?}");
        let global = self.tmux(&["show-environment", "-g"]);
        let global = String::from_utf8_lossy(&global.stdout);
        assert!(
            global.contains(SECRETS[0].1),
            "the server holds the secret: {global}"
        );
    }

    /// Run `cmd` (a helper-built `new-session` for `name`) and remember it.
    async fn open(&mut self, name: &str, mut cmd: tokio::process::Command) {
        self.made.push(name.to_string());
        let out = cmd.output().await.unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    fn env_file(&self, name: &str) -> PathBuf {
        self.dir.path().join(format!("{name}.env"))
    }
}

impl Drop for World {
    fn drop(&mut self) {
        for name in &self.made {
            eprintln!("killing {name} on {}", self.socket.display());
            let _ = self.tmux(&["kill-session", "-t", &format!("={name}")]);
        }
    }
}

/// Takes the secrets back out of this process's environment on the way out,
/// panic or not, so no later test inherits them.
struct UnsetOnDrop;

impl Drop for UnsetOnDrop {
    fn drop(&mut self) {
        for (name, _) in SECRETS {
            std::env::remove_var(name);
        }
    }
}

/// Wait for `path` to hold a whole `env` dump, bounded.
fn read_env(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(env) = std::fs::read_to_string(path) {
            if env.contains("PATH=") {
                return env;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no env dump at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn assert_no_secret(env: &str, what: &str) {
    for (name, value) in SECRETS {
        assert!(!env.contains(name), "{name} reached {what}");
        assert!(!env.contains(value), "the {name} value reached {what}");
    }
}

/// A plain shell on a server that already holds the secrets: its pane (and
/// a window opened in the session later) starts without them.
#[tokio::test]
async fn a_shell_on_a_server_holding_the_secrets_gets_neither() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        return;
    }
    let mut world = World::new();
    world.start_server_with_secrets();
    let name = format!("tse-shell-{}", std::process::id());
    let dir = world.dir.path().display().to_string();

    world.open(&name, tmux_new_session(&name, &dir, &[], &[])).await;

    let dump = world.env_file(&name);
    let keys = format!("env > '{}'", dump.display());
    let sent = world.tmux(&["send-keys", "-t", &format!("={name}:"), "-l", &keys]);
    assert!(sent.status.success(), "{sent:?}");
    world.tmux(&["send-keys", "-t", &format!("={name}:"), "Enter"]);
    assert_no_secret(&read_env(&dump), "the shell's pane");

    let window = world.tmux(&["new-window", "-t", &format!("={name}:"), "-d"]);
    assert!(window.status.success(), "{window:?}");
    let later = world.tmux(&["show-environment", "-t", &format!("={name}")]);
    let later = String::from_utf8_lossy(&later.stdout);
    for (secret, _) in SECRETS {
        assert!(
            later.contains(&format!("-{secret}")),
            "the session drops {secret}: {later}"
        );
    }
}

/// A command session on a server that already holds the secrets: the
/// command runs without them.
#[tokio::test]
async fn a_command_on_a_server_holding_the_secrets_gets_neither() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        return;
    }
    let mut world = World::new();
    world.start_server_with_secrets();
    let name = format!("tse-cmd-{}", std::process::id());
    let dump = world.env_file(&name);
    let argv = [
        OsString::from("/bin/sh"),
        OsString::from("-c"),
        OsString::from(format!("env > '{}'; sleep 30", dump.display())),
    ];
    let dir = world.dir.path().display().to_string();

    world
        .open(
            &name,
            tmux_new_session(&name, &dir, &["-x", "80", "-y", "24"], &argv),
        )
        .await;

    assert_no_secret(&read_env(&dump), "the command's pane");
    let later = world.tmux(&["show-environment", "-t", &format!("={name}")]);
    let later = String::from_utf8_lossy(&later.stdout);
    for (secret, _) in SECRETS {
        assert!(
            later.contains(&format!("-{secret}")),
            "a later window in the session would get {secret}: {later}"
        );
    }
}

/// The helper starting the server from a daemon that holds the secrets:
/// the server's own environment, and so every later pane on it, has
/// neither.
#[tokio::test]
async fn a_server_the_helper_starts_holds_neither() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        return;
    }
    let mut world = World::new();
    let _unset = UnsetOnDrop;
    for (name, value) in SECRETS {
        std::env::set_var(name, value);
    }
    let name = format!("tse-first-{}", std::process::id());
    let dir = world.dir.path().display().to_string();
    // Still set while the client runs: an inherited environment would carry
    // them into the server it starts.
    world.open(&name, tmux_new_session(&name, &dir, &[], &[])).await;
    for (secret, _) in SECRETS {
        std::env::remove_var(secret);
    }

    let global = world.tmux(&["show-environment", "-g"]);
    assert!(
        global.status.success(),
        "the helper started the server: {global:?}"
    );
    let global = String::from_utf8_lossy(&global.stdout);
    assert!(
        global.lines().any(|line| line.starts_with("PATH=")),
        "{global}"
    );
    assert_no_secret(&global, "the tmux server");
}

/// An interactive run starts through the helper and finishes: the wrapper
/// under a folder with a space is executed directly, the provider runs in a
/// work folder whose name reads as a tmux format, and its exit code is
/// recorded as the run's outcome.
#[tokio::test]
async fn an_interactive_run_starts_through_the_helper_and_finishes() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !tmux_available() {
        return;
    }
    let mut world = World::new();
    world.start_server_with_secrets();
    let logs = world.dir.path().join("logs dir");
    std::fs::create_dir_all(&logs).unwrap();
    let work = world.dir.path().join("work#{session_name}");
    std::fs::create_dir_all(&work).unwrap();
    let dump = world.env_file("run");
    let argv = vec![
        "-c".to_string(),
        format!("pwd > '{0}.pwd'; env > '{0}'", dump.display()),
    ];
    let child_env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];
    let name = format!("tse-run-{}", std::process::id());

    let run = ainb_hangar_daemon::interactive::spawn(
        Path::new("/bin/sh"),
        &work,
        &argv,
        &child_env,
        &name,
        &logs,
        Duration::from_secs(20),
    )
    .await;
    world.made.push(name);
    let run = run.expect("the run starts");
    let outcome = run.wait().await.expect("the run is waited on");

    assert!(
        matches!(outcome, ainb_hangar_daemon::runner::RunOutcome::Success(_)),
        "{outcome:?}"
    );
    assert_no_secret(&read_env(&dump), "the interactive provider");
    let pwd = std::fs::read_to_string(format!("{}.pwd", dump.display())).unwrap();
    assert_eq!(
        Path::new(pwd.trim()).canonicalize().unwrap(),
        work.canonicalize().unwrap(),
        "the literal folder, not a format"
    );
}
