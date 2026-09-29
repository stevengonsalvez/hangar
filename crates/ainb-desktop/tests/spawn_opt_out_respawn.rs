//! A spawn opt-out survives the sidecar respawning the daemon: after the
//! first daemon is killed and the supervisor starts a fresh one, all five
//! spawn verbs still answer `METHOD_NOT_FOUND`, whether the opt-out is the
//! file key (`[hangar] spawn = false`) or the app's `AINB_HANGAR_SPAWN=0`.
//! A daemon with neither serves them after a respawn, which proves the binary
//! under test has the verbs at all.
//!
//! Its own process: `HOME` and `TMUX_TMPDIR` point at private directories
//! the spawned daemons inherit, so nothing reaches the user's home or tmux,
//! and `AINB_HANGAR_SPAWN` is removed from this process so each test's opt-out
//! comes only from its own home or its own [`SidecarConfig::spawn_switch`].
//! Every daemon is killed by its exact pid when its test ends.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState, daemon_pid};
use ainb_hangar_client::{DaemonClient, DaemonError};
use ainb_hangar_proto::methods as m;
use tokio::sync::watch;

/// A cold runner needs time to migrate a fresh store and mint the token.
const BOOT_BUDGET: Duration = Duration::from_secs(90);

const METHOD_NOT_FOUND: i32 = -32601;

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

/// Point `HOME` and `TMUX_TMPDIR` at private directories and clear the
/// switch, once, before any daemon starts.
fn private_env() {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = tempfile::tempdir().expect("private root").keep();
        let tmux = root.join("tmux");
        std::fs::create_dir_all(&tmux).unwrap();
        // Inherited by every daemon the sidecar spawns. Edition 2021: safe.
        std::env::set_var("HOME", &root);
        std::env::set_var("TMUX_TMPDIR", &tmux);
        // The codex manager's boot reaper signals processes outside the home.
        std::env::set_var("AINB_CODEX_MANAGED", "0");
        std::env::remove_var(ainb_hangar_daemon::spawn::SPAWN_ENV);
        root
    });
}

/// A private hangar home, and the daemon running in it killed on drop.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        private_env();
        Self {
            dir: tempfile::tempdir().expect("scratch home"),
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join(".agents-in-a-box")
    }

    /// Write `text` as this home's `config/config.toml`.
    fn write_config(&self, text: &str) {
        let path = ainb_hangar_daemon::spawn::config_path_in(&self.home());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn config(&self, spawn_switch: Option<&str>) -> SidecarConfig {
        let mut config = SidecarConfig::new(self.home(), daemon_bin());
        config.hello_budget = BOOT_BUDGET;
        config.spawn_switch = spawn_switch.map(Into::into);
        config
    }

    fn client(&self) -> DaemonClient {
        let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(&self.home()))
            .expect("daemon token");
        DaemonClient::with_parts(
            ainb_hangar_client::socket_path_in(&self.home()),
            token.trim().to_string(),
        )
    }

    /// Start the supervisor, kill the daemon it spawned, and return once it
    /// is connected to the fresh one it started in its place.
    async fn respawned(&self, config: SidecarConfig) -> Sidecar {
        let sidecar = Sidecar::start(config);
        let mut state = sidecar.state();
        let SidecarState::Connected {
            daemon_pid: Some(first),
            spawned: true,
            ..
        } = wait_for(&mut state, "connected", connected).await
        else {
            panic!("the first daemon was not one this supervisor spawned");
        };
        kill(first);
        wait_for(&mut state, "reconnecting", |state| {
            matches!(state, SidecarState::Reconnecting { .. })
        })
        .await;
        let SidecarState::Connected {
            daemon_pid: Some(second),
            spawned: true,
            ..
        } = wait_for(&mut state, "connected again", connected).await
        else {
            panic!("the supervisor did not respawn the daemon");
        };
        assert_ne!(first, second, "a fresh daemon took the home");
        sidecar
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(pid) = daemon_pid(&self.home()) {
            kill(pid);
        }
    }
}

fn kill(pid: u32) {
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).expect("pid")),
        nix::sys::signal::Signal::SIGKILL,
    );
}

fn connected(state: &SidecarState) -> bool {
    matches!(state, SidecarState::Connected { .. })
}

async fn wait_for(
    state: &mut watch::Receiver<SidecarState>,
    what: &str,
    matches: impl Fn(&SidecarState) -> bool,
) -> SidecarState {
    tokio::time::timeout(BOOT_BUDGET, async {
        loop {
            let current = state.borrow_and_update().clone();
            if matches(&current) {
                return current;
            }
            state.changed().await.expect("supervisor running");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("never {what}; last state {:?}", *state.borrow()))
}

/// Every spawn verb, each with a well-formed request.
fn spawn_verbs() -> [(&'static str, serde_json::Value); 5] {
    [
        (
            m::WORKTREE_CREATE,
            serde_json::json!({"repo_path": "/tmp", "agent": "claude"}),
        ),
        (
            m::WORKTREE_AGENT_ADD,
            serde_json::json!({"worktree_path": "/tmp", "agent": "claude"}),
        ),
        (
            m::SHELL_CREATE,
            serde_json::json!({"worktree_path": "/tmp"}),
        ),
        (m::SHELL_LIST, serde_json::json!({})),
        (
            m::SHELL_CLOSE,
            serde_json::json!({"tmux_session_name": "ainb-dsh-0a1b2c3d"}),
        ),
    ]
}

async fn assert_all_dark(client: &DaemonClient) {
    for (method, params) in spawn_verbs() {
        match client.call_typed::<_, serde_json::Value>(method, &params).await {
            Err(DaemonError::Rpc { code, .. }) if code == METHOD_NOT_FOUND => {}
            other => panic!("{method} after a respawn: want METHOD_NOT_FOUND, got {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_opt_out_survives_a_respawn() {
    let world = World::new();
    world.write_config("[hangar]\nspawn = false\n");
    let sidecar = world.respawned(world.config(None)).await;
    assert_all_dark(&world.client()).await;
    drop(sidecar);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_apps_env_opt_out_survives_a_respawn() {
    let world = World::new();
    let sidecar = world.respawned(world.config(Some("0"))).await;
    assert_all_dark(&world.client()).await;
    drop(sidecar);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_no_opt_out_a_respawned_daemon_serves_the_verbs() {
    let world = World::new();
    let sidecar = world.respawned(world.config(None)).await;
    let listed = world
        .client()
        .call_typed::<_, serde_json::Value>(m::SHELL_LIST, &serde_json::json!({}))
        .await;
    assert!(listed.is_ok(), "shell/list is served: {listed:?}");
    drop(sidecar);
}
