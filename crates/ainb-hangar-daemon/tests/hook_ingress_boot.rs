//! The hook listener's files across real daemon boots (`--once`).
//!
//! A crash, a SIGKILL or a forced second-signal exit leaves the endpoint and
//! headers files behind. Every boot removes them once it owns the home,
//! whatever the switch says; with the switch off nothing is ever written.

use std::path::Path;

use assert_cmd::Command;

const ENDPOINT: &str = "hook-endpoint.env";
const HEADERS: &str = "hook-headers";

fn boot_once(home: &Path, listen: bool) {
    let mut cmd = Command::cargo_bin("ainb-hangar-daemon").expect("binary builds");
    cmd.arg("--once")
        .env("AINB_HANGAR_HOME", home)
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
        .env_remove("AINB_HANGAR_HOOK_LISTEN");
    if listen {
        cmd.env("AINB_HANGAR_HOOK_LISTEN", "1");
    }
    let output = cmd.output().expect("run --once");
    assert!(
        output.status.success(),
        "--once should exit 0, stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn plant_leftovers(home: &Path) {
    let dir = home.join("hangar");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(ENDPOINT),
        "AINB_HOOK_PORT=45678\nAINB_HOOK_VERSION=1\nAINB_HOOK_PID=999999\n",
    )
    .unwrap();
    std::fs::write(dir.join(HEADERS), "X-Ainb-Hook-Token: stale\n").unwrap();
}

#[test]
fn with_the_switch_off_a_boot_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    boot_once(home.path(), false);
    assert!(!home.path().join("hangar").join(ENDPOINT).exists());
    assert!(!home.path().join("hangar").join(HEADERS).exists());
}

#[test]
fn a_boot_with_the_switch_off_removes_a_crashed_daemons_files() {
    let home = tempfile::tempdir().unwrap();
    plant_leftovers(home.path());
    boot_once(home.path(), false);
    assert!(!home.path().join("hangar").join(ENDPOINT).exists());
    assert!(!home.path().join("hangar").join(HEADERS).exists());
}

#[test]
fn a_boot_with_the_switch_on_replaces_leftovers_and_cleans_up_on_exit() {
    let home = tempfile::tempdir().unwrap();
    plant_leftovers(home.path());
    // `--once` binds the listener, publishes fresh files, then exits cleanly,
    // and the clean exit removes what it published.
    boot_once(home.path(), true);
    assert!(!home.path().join("hangar").join(ENDPOINT).exists());
    assert!(!home.path().join("hangar").join(HEADERS).exists());
}
