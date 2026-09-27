//! A daemon started without the spawn switch does not serve `worktree/create`,
//! and the window says what to do about it instead of failing blankly.
//!
//! Its own process: the spawn switch is an environment variable the daemon
//! reads at boot, and `create_worktree.rs` turns it on. The daemon is killed
//! by its exact pid when the test ends.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_desktop::create::{CreateWorktreeArgs, request};
use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState, daemon_pid};
use ainb_hangar_client::DaemonClient;
use ainb_hangar_proto::spawn::SpawnAgent;

const BOOT_BUDGET: Duration = Duration::from_secs(90);

fn daemon_bin() -> PathBuf {
    let bin = std::env::var_os("AINB_DESKTOP_DAEMON_BIN").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ainb-hangar-daemon"),
        PathBuf::from,
    );
    assert!(bin.is_file(), "no daemon binary at {}", bin.display());
    bin
}

struct Home(tempfile::TempDir);

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(pid) = daemon_pid(&self.0.path().join(".agents-in-a-box")) {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(pid).expect("pid")),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_without_the_switch_is_named_as_the_reason() {
    std::env::set_var("AINB_CODEX_MANAGED", "0");
    std::env::remove_var("AINB_HANGAR_SPAWN");
    let home = Home(tempfile::tempdir().unwrap());
    let hangar = home.0.path().join(".agents-in-a-box");
    let mut config = SidecarConfig::new(hangar.clone(), daemon_bin());
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

    let token =
        std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(&hangar)).expect("token");
    let client = DaemonClient::with_parts(
        ainb_hangar_client::socket_path_in(&hangar),
        token.trim().to_string(),
    );
    let refusal = request(
        &client,
        CreateWorktreeArgs {
            repo_path: "/repos/app".into(),
            branch: None,
            base: None,
            agent: SpawnAgent::Claude,
            model: None,
            prompt: None,
        },
    )
    .await
    .expect_err("the verb is dark without the switch");
    assert!(refusal.contains("AINB_HANGAR_SPAWN=1"), "{refusal}");
    drop(sidecar);
}
