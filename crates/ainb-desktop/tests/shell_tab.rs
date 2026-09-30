//! "New terminal" end to end against the real daemon binary and a real tmux:
//! the window's open reaches `shell/create`, the shell attaches as a tab, a
//! fresh window reattaches it from `shell/list`, and closing the tab ends it
//! through `shell/close`. A session the daemon did not open is never ended.
//!
//! tmux runs on a private server: `TMUX_TMPDIR` points at this test's own
//! directory before the daemon starts, so the daemon, the tab clients and the
//! checks here all reach that server and nobody else's. Every session is
//! ended by its exact name, so the server exits on its own when the test is
//! done; nothing here kills a server.
//!
//! Its own process: the env it sets is process-global and inherited by the
//! daemon it spawns. The daemon is killed by its exact pid when the test ends.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ainb_desktop::shell_tab;
use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState, daemon_pid};
use ainb_desktop::terminal::{TabEvents, TabState, TabTarget, TabsView, Terminals, Tmux};
use ainb_hangar_client::DaemonClient;

/// A cold runner needs time to migrate a fresh store and mint the token.
const BOOT_BUDGET: Duration = Duration::from_secs(90);

fn daemon_bin() -> PathBuf {
    let bin = std::env::var_os("AINB_DESKTOP_DAEMON_BIN").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ainb-hangar-daemon"),
        PathBuf::from,
    );
    assert!(
        bin.is_file(),
        "no daemon binary at {}: run `cargo build -p ainb-hangar-daemon` at the \
         workspace root or set AINB_DESKTOP_DAEMON_BIN",
        bin.display()
    );
    bin
}

struct Quiet;

impl TabEvents for Quiet {
    fn tabs(&self, _: TabsView) {}
    fn toast(&self, _: String) {}
}

/// A tmux command against the private server `TMUX_TMPDIR` names.
fn tmux() -> Command {
    let mut command = Command::new("tmux");
    command.env_remove("TMUX");
    command
}

fn alive(name: &str) -> bool {
    tmux()
        .args(["has-session", "-t", &format!("={name}")])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The daemon and every session this test made, ended by exact pid and name.
struct Cleanup {
    hangar: PathBuf,
    sessions: Vec<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for name in &self.sessions {
            let _ = tmux()
                .args(["kill-session", "-t", &format!("={name}")])
                .stderr(std::process::Stdio::null())
                .status();
        }
        if let Some(pid) = daemon_pid(&self.hangar) {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(pid).expect("pid")),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

fn git_init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let ok = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(ok, "git init {}", dir.display());
}

fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_tab_opens_reattaches_and_ends_through_the_daemon() {
    // Under /tmp: a tmux socket path must stay short.
    let sockets = tempfile::Builder::new().prefix("dsh").tempdir_in("/tmp").unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let repo = user_home.path().join("code/app");
    git_init(&repo);
    let config = user_home.path().join(".agents-in-a-box/config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[workspace_defaults]\nworkspace_scan_paths = [\"{}\"]\n",
            user_home.path().join("code").display()
        ),
    )
    .unwrap();
    let repo = std::fs::canonicalize(repo).unwrap();
    // Inherited by the daemon the sidecar spawns. Edition 2021: safe.
    std::env::set_var("HOME", user_home.path());
    std::env::set_var("TMUX_TMPDIR", sockets.path());
    std::env::remove_var("TMUX");
    std::env::set_var("AINB_CODEX_MANAGED", "0");
    // Unset: the spawn verbs are on by default.
    std::env::remove_var("AINB_HANGAR_SPAWN");

    let hangar = tempfile::tempdir().expect("scratch hangar home");
    let hangar_home = hangar.path().join(".agents-in-a-box");
    let mut cleanup = Cleanup {
        hangar: hangar_home.clone(),
        sessions: Vec::new(),
    };
    let mut sidecar_config = SidecarConfig::new(hangar_home.clone(), daemon_bin());
    sidecar_config.hello_budget = BOOT_BUDGET;
    let sidecar = Sidecar::start(sidecar_config);
    let mut state = sidecar.state();
    tokio::time::timeout(BOOT_BUDGET, async {
        loop {
            if matches!(*state.borrow_and_update(), SidecarState::Connected { .. }) {
                return;
            }
            state.changed().await.expect("supervisor running");
        }
    })
    .await
    .expect("the daemon connected");
    let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(&hangar_home))
        .expect("daemon token");
    let client = DaemonClient::with_parts(
        ainb_hangar_client::socket_path_in(&hangar_home),
        token.trim().to_string(),
    );
    let program = ainb_desktop::terminal::find_tmux().expect("tmux on PATH");
    let (reports, _reports_rx) = mpsc::channel();
    let terminals = Terminals::new(Tmux::new(program.clone()), Quiet, reports.clone());

    // A folder outside every registered one is refused by code, with the fix.
    let stray = user_home.path().join("stray");
    git_init(&stray);
    let refused = shell_tab::open(&client, &stray.display().to_string())
        .await
        .expect_err("an unregistered folder is refused");
    assert!(refused.contains("Add project"), "{refused}");

    // Open: the daemon makes the shell, and it attaches as a shell tab.
    let shell = shell_tab::open(&client, &repo.display().to_string())
        .await
        .expect("the daemon opened a shell");
    cleanup.sessions.push(shell.tmux_session_name.clone());
    let key = shell.tmux_session_name.clone();
    assert!(
        ainb_hangar_proto::spawn::is_daemon_shell_name(&key),
        "{key}"
    );
    assert_eq!(shell.worktree_path, repo.display().to_string());
    assert!(alive(&key), "the shell runs on the private server");
    assert_eq!(
        terminals.open(shell_tab::target(&shell)),
        None,
        "the tab attached"
    );
    assert!(shell_tab::is_shell_tab(&terminals, &key));

    // A shell whose tab cannot attach is closed again: the window shows no
    // tab for it, so nothing else would end it. This window's tmux cannot
    // run, so every attach fails.
    let broken = Terminals::new(
        Tmux::new(PathBuf::from("/nonexistent/tmux")),
        Quiet,
        reports.clone(),
    );
    let never = |_| panic!("a shell the daemon made is not looked for again");
    let failed = shell_tab::open_tab(&client, &broken, &repo.display().to_string(), never)
        .await
        .expect_err("the tab cannot attach");
    assert!(
        failed.report.is_some(),
        "the reducer hears the failed attach"
    );
    assert!(
        failed.message.contains("closed again"),
        "{}",
        failed.message
    );
    let running = client.shell_list().await.expect("listed");
    // Whatever runs is ended by name when the test ends, a leak included.
    cleanup
        .sessions
        .extend(running.shells.iter().map(|s| s.tmux_session_name.clone()));
    assert_eq!(
        running.shells.iter().map(|s| s.tmux_session_name.as_str()).collect::<Vec<_>>(),
        [key.as_str()],
        "shell/close ended the unattached shell; only the first one runs"
    );
    drop(broken);

    // Relaunch: a window with no tabs lists the daemon's shells and
    // reattaches them, unfocused.
    let relaunched = Terminals::new(Tmux::new(program), Quiet, reports);
    let (listed, failed) = shell_tab::restore(&client, &relaunched)
        .await
        .expect("the daemon lists its shells");
    assert_eq!((listed, failed.len()), (1, 0));
    let view = relaunched.view();
    assert_eq!(view.tabs.len(), 1);
    assert_eq!(
        view.tabs[0].target,
        TabTarget::Shell {
            tmux: key.clone(),
            dir: repo.display().to_string()
        }
    );
    assert_eq!(view.tabs[0].state, TabState::Attached);
    drop(relaunched);

    // A session under a daemon shell's name that the daemon did not open
    // (no `@ainb_owner`): its tab is not a shell tab, and the daemon will
    // not end it even when asked by name.
    let impostor = "ainb-dsh-deadbeef".to_string();
    let made = tmux()
        .args(["-f", "/dev/null", "new-session", "-d", "-s", &impostor])
        .status()
        .is_ok_and(|status| status.success());
    assert!(made, "the impostor session started");
    cleanup.sessions.push(impostor.clone());
    assert_eq!(
        terminals.open(TabTarget::Tmux {
            tmux: impostor.clone()
        }),
        None
    );
    let not_shell = shell_tab::close_tab(&terminals, &client, &impostor)
        .await
        .expect_err("a tab that is not a shell's is refused");
    assert!(not_shell.contains("not a terminal tab"), "{not_shell}");
    assert!(terminals.target(&impostor).is_some(), "its tab stays open");
    assert_eq!(shell_tab::close(&client, &impostor).await, Ok(false));
    assert!(
        alive(&impostor),
        "the daemon left a session it does not own"
    );

    // Close: the tab goes and the shell ends.
    assert_eq!(
        shell_tab::close_tab(&terminals, &client, &key).await,
        Ok(true)
    );
    assert!(terminals.target(&key).is_none(), "the tab closed");
    eventually("the shell ended", || !alive(&key));
    // Closing again is an answer, not an error: it is already gone.
    assert_eq!(shell_tab::close(&client, &key).await, Ok(false));
    let after = client.shell_list().await.expect("listed");
    assert!(after.shells.is_empty(), "{after:?}");

    terminals.close(&impostor);
    drop(terminals);
    drop(cleanup);
    drop(sidecar);
}
