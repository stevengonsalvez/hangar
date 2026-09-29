//! A daemon boot removes the `ainb run` output a previous daemon left in
//! `logs/spawn/`. A run's files are removed once its outcome is read, but a
//! daemon killed mid-run never reads it, and nothing else would ever clear
//! them.
//!
//! Drives the real `ainb-hangar-daemon` binary in one-shot mode (`--once`)
//! against an isolated `$AINB_HANGAR_HOME`, as `it_boot_registers_runtime`
//! does.

use std::process::Command;

use assert_cmd::prelude::*;

#[test]
fn a_boot_sweeps_the_run_output_a_previous_daemon_left() {
    let home = tempfile::tempdir().unwrap();
    let spawn_logs = home.path().join("hangar/logs/spawn");
    std::fs::create_dir_all(&spawn_logs).unwrap();
    for name in ["0123abcd.stdout", "0123abcd.stderr"] {
        std::fs::write(spawn_logs.join(name), "left by a killed daemon").unwrap();
    }

    let output = Command::cargo_bin("ainb-hangar-daemon")
        .expect("binary builds")
        .arg("--once")
        .env("AINB_HANGAR_HOME", home.path())
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
        .output()
        .expect("run --once");
    assert!(
        output.status.success(),
        "--once should exit 0, stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    let left: Vec<_> = std::fs::read_dir(&spawn_logs)
        .map(|entries| entries.filter_map(Result::ok).map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(
        left.is_empty(),
        "a boot left the previous run output: {left:?}"
    );
}
