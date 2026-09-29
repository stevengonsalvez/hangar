//! The window's "new agent here", end to end against the real daemon binary:
//! a listed session id resolves to its worktree on the host, the request
//! reaches `worktree/agent_add`, the daemon checks the tree against the disk
//! and runs `ainb run --existing-worktree`, and the new session comes back as
//! the tab to attach. Refusals read by their code.
//!
//! The daemon is booted by the sidecar in a private hangar home with its
//! default spawn verbs and `AINB_BIN` pointing at a stand-in `ainb` that
//! records its argv and answers like `ainb --format json run`.
//!
//! Its own process: the env it sets is process-global and inherited by the
//! daemon it spawns. The daemon is killed by its exact pid when the test ends.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_app::models::session::Session;
use ainb_app::models::workspace::Workspace;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{AppState, Keymap, SectionId};
use ainb_desktop::agent_add::request;
use ainb_desktop::executor::DesktopExecutor;
use ainb_desktop::host::DesktopHost;
use ainb_desktop::shell::Shell;
use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState, daemon_pid};
use ainb_desktop::worktree_target::WorktreeTarget;
use ainb_hangar_client::DaemonClient;
use ainb_hangar_proto::spawn::SpawnAgent;

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

/// A stand-in `ainb`: a `run` records its argv (one per line) to `argv.txt`
/// beside it and prints one created-session line. Anything else (the
/// daemon's own background `ainb list --format json`) lists nothing and
/// records nothing, so `argv.txt` is only ever a run's.
fn fake_ainb(dir: &Path) -> PathBuf {
    let bin = dir.join("ainb");
    let argv = dir.join("argv.txt");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nif [ \"$3\" != run ]; then echo '[]'; exit 0; fi\n\
             : > '{argv}'\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{argv}'; done\n\
             printf '%s\\n' '{{\"session_id\":\"33333333-3333-4333-8333-333333333333\",\"tmux_session_name\":\"tmux_app-33333333\",\"worktree_path\":\"/w/app\",\"branch\":\"feat/here\"}}'\n",
            argv = argv.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Run git in `dir` with no user config, asserting it succeeds.
fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(ok, "git {args:?} in {}", dir.display());
}

/// A repository at `repo` with one commit, and a linked worktree of it on
/// `branch` at `tree`: what `ainb run --worktree` leaves behind.
fn repo_with_tree(repo: &Path, tree: &Path, branch: &str) {
    std::fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    git(
        repo,
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
        ],
    );
    let tree = tree.to_str().unwrap();
    git(repo, &["worktree", "add", "-q", "-b", branch, tree]);
}

/// The window's shell hosting a session list of one workspace per
/// `(repo, session path)`, each holding one session with a fresh id: what the
/// `worktree_agent_add` command resolves ids against.
fn listed(rows: &[(&Path, &Path)]) -> (Shell<impl ainb_desktop::host::FrameSink>, Vec<String>) {
    let mut state = AppState::new();
    let mut ids = Vec::new();
    for (repo, path) in rows {
        let session = Session::new("agent".into(), path.display().to_string());
        ids.push(session.id.to_string());
        let mut workspace = Workspace::new("app".into(), repo.to_path_buf());
        workspace.add_session(session);
        state.sessions.workspaces.push(workspace);
    }
    let host = DesktopHost::hosting(
        state,
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Shell]),
        |_: FrameBatch| {},
    )
    .without_attention_poll();
    (Shell::new(host, DesktopExecutor::new(None)), ids)
}

/// The folder the window's command resolves a session id to: through the
/// shell's own session list, as `worktree_agent_add` does.
fn resolve(shell: &Shell<impl ainb_desktop::host::FrameSink>, id: &str) -> Result<String, String> {
    WorktreeTarget::Session { id: id.into() }.resolve(|id| shell.session_worktree(id), |_| None)
}

struct Home(tempfile::TempDir);

impl Home {
    fn path(&self) -> PathBuf {
        self.0.path().join(".agents-in-a-box")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(pid) = daemon_pid(&self.path()) {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(pid).expect("pid")),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_window_adds_an_agent_to_a_listed_worktree_through_the_daemon() {
    let tools = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let home_dir = std::fs::canonicalize(user_home.path()).unwrap();
    // The trees `ainb run --worktree` makes live directly in this folder; the
    // daemon adds agents to nothing else.
    let managed = home_dir.join(".agents-in-a-box/worktrees/by-name");
    std::fs::create_dir_all(&managed).unwrap();
    let repo = home_dir.join("code/app");
    let tree = managed.join("app-feat-here");
    repo_with_tree(&repo, &tree, "feat/here");
    // A tree whose repository is in no registered folder.
    let stray = home_dir.join("stray");
    let stray_tree = managed.join("stray-feat");
    repo_with_tree(&stray, &stray_tree, "feat/stray");
    let config = home_dir.join(".agents-in-a-box/config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[workspace_defaults]\nworkspace_scan_paths = [\"{}\"]\n",
            home_dir.join("code").display()
        ),
    )
    .unwrap();
    // Inherited by the daemon the sidecar spawns. Edition 2021: safe.
    std::env::set_var("HOME", &home_dir);
    std::env::remove_var("AINB_HOME");
    std::env::set_var("AINB_CODEX_MANAGED", "0");
    // Unset: the spawn verbs are on by default.
    std::env::remove_var("AINB_HANGAR_SPAWN");
    std::env::set_var("AINB_BIN", fake_ainb(tools.path()));

    let home = Home(tempfile::tempdir().expect("scratch home"));
    let mut sidecar_config = SidecarConfig::new(home.path(), daemon_bin());
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
    let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(&home.path()))
        .expect("daemon token");
    let client = DaemonClient::with_parts(
        ainb_hangar_client::socket_path_in(&home.path()),
        token.trim().to_string(),
    );

    // The host's own list: a session in the tree, one in the repository's
    // main checkout, and one in the stray tree.
    let (shell, ids) = listed(&[(&repo, &tree), (&repo, &repo), (&stray, &stray_tree)]);

    // An id the host did not list never reaches the daemon.
    let unknown = resolve(&shell, "44444444-4444-4444-8444-444444444444")
        .expect_err("an unlisted id is refused");
    assert!(unknown.contains("no longer in the list"), "{unknown}");
    assert!(
        !tools.path().join("argv.txt").exists(),
        "nothing ran for an unlisted id"
    );

    // A session outside ainb's worktree folder (here the repository's main
    // checkout) is refused by the daemon, and its detail is shown.
    let main_checkout = resolve(&shell, &ids[1]).expect("listed");
    let refused = request(&client, main_checkout, SpawnAgent::Claude)
        .await
        .expect_err("the main checkout is not a worktree ainb made");
    assert!(
        refused.starts_with(
            "Adding the agent failed: the daemon refused it: worktree_path is not a worktree ainb created"
        ),
        "{refused}"
    );

    // A tree cut from an unregistered repository: the code names the fix.
    let unregistered = resolve(&shell, &ids[2]).expect("listed");
    let refused = request(&client, unregistered, SpawnAgent::Claude)
        .await
        .expect_err("an unregistered source repository is refused");
    assert!(refused.contains("Add project"), "{refused}");
    assert!(
        !tools.path().join("argv.txt").exists(),
        "no refusal ran ainb"
    );

    // The listed tree: the daemon runs `ainb run --existing-worktree` there.
    let worktree = resolve(&shell, &ids[0]).expect("listed");
    let created = request(&client, worktree, SpawnAgent::Codex)
        .await
        .expect("the daemon added the agent");
    assert_eq!(created.session_id, "33333333-3333-4333-8333-333333333333");
    assert_eq!(created.tmux_session_name, "tmux_app-33333333");

    let argv = std::fs::read_to_string(tools.path().join("argv.txt")).expect("ainb ran");
    let argv: Vec<&str> = argv.lines().collect();
    let tree = tree.display().to_string();
    assert_eq!(
        argv,
        [
            "--format",
            "json",
            "run",
            "--existing-worktree",
            tree.as_str(),
            "--tool",
            "codex"
        ],
        "no model, no prompt, and permission prompts never skipped"
    );

    drop(sidecar);
}

/// The page reaches the host only through a registered command: a
/// `worktree_agent_add` that is defined but not in `generate_handler!` would
/// leave "+ Agent" rejecting every press, with every other test green.
#[test]
fn worktree_agent_add_is_a_command_the_window_can_invoke() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("main.rs");
    assert!(
        source.contains("async fn worktree_agent_add("),
        "the command is defined"
    );
    let handler = source
        .split("generate_handler![")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .expect("generate_handler!");
    assert!(
        handler.split(',').any(|name| name.trim() == "worktree_agent_add"),
        "worktree_agent_add is not registered"
    );
}
