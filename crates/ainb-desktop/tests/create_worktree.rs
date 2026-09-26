//! The window's worktree create, end to end against the real daemon binary:
//! the desktop's request reaches `worktree/create`, the daemon runs `ainb`,
//! and the created session comes back as the tab to attach.
//!
//! The daemon is booted by the sidecar in a private hangar home with the
//! spawn switch on and `AINB_BIN` pointing at a stand-in `ainb` that records
//! its argv and answers like `ainb --format json run`. The real CLI half is
//! proven in `crates/ainb-core/tests/run_json_worktree.rs`.
//!
//! Its own process: the env it sets is process-global and inherited by the
//! daemon it spawns. The daemon is killed by its exact pid when the test ends.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_desktop::create::{CreateWorktreeArgs, request};
use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState, daemon_pid};
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

/// A stand-in `ainb`: records argv (one per line) to `argv.txt` beside it,
/// then prints one created-session line.
fn fake_ainb(dir: &Path) -> PathBuf {
    let bin = dir.join("ainb");
    let argv = dir.join("argv.txt");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\n: > '{argv}'\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{argv}'; done\n\
             printf '%s\\n' '{{\"session_id\":\"22222222-2222-4222-8222-222222222222\",\"tmux_session_name\":\"tmux_app-22222222\",\"worktree_path\":\"/w/app\",\"branch\":\"feat/window\"}}'\n",
            argv = argv.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
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
async fn the_window_creates_a_worktree_session_through_the_daemon() {
    let tools = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    // Inherited by the daemon the sidecar spawns. Edition 2021: safe.
    std::env::set_var("AINB_CODEX_MANAGED", "0");
    std::env::set_var("AINB_HANGAR_SPAWN", "1");
    std::env::set_var("AINB_BIN", fake_ainb(tools.path()));

    let home = Home(tempfile::tempdir().expect("scratch home"));
    let mut config = SidecarConfig::new(home.path(), daemon_bin());
    config.hello_budget = BOOT_BUDGET;
    let sidecar = Sidecar::start(config);
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
    let created = request(
        &client,
        CreateWorktreeArgs {
            repo_path: repo.path().display().to_string(),
            branch: Some("feat/window".into()),
            base: Some("main".into()),
            agent: "claude".into(),
            model: None,
            prompt: Some("hello from the window".into()),
            name: None,
        },
    )
    .await
    .expect("the daemon created the session");

    assert_eq!(created.tmux_session_name, "tmux_app-22222222");
    assert_eq!(created.branch, "feat/window");

    let argv = std::fs::read_to_string(tools.path().join("argv.txt")).expect("ainb ran");
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(
        &argv[..4],
        ["--format", "json", "run", "--worktree"],
        "{argv:?}"
    );
    let pairs: Vec<(&str, &str)> = argv.windows(2).map(|w| (w[0], w[1])).collect();
    assert!(pairs.contains(&("--tool", "claude")), "{argv:?}");
    assert!(pairs.contains(&("--base", "main")), "{argv:?}");
    assert!(
        !argv.contains(&"--dangerously-skip-permissions"),
        "the window never skips permission prompts: {argv:?}"
    );
    assert_eq!(argv.last(), Some(&"--prompt=hello from the window"));

    drop(sidecar);
}
