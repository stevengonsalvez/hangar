//! A daemon boot removes the `ainb run` output a previous daemon left in
//! `logs/spawn/`. A run's files are removed once its outcome is read, but a
//! daemon killed mid-run never reads it, and nothing else would ever clear
//! them.
//!
//! The sweep touches nothing but a run's own files in a directory that is
//! this user's: it never follows a symlink, never removes a file by any other
//! name, and never recurses.
//!
//! Drives the real `ainb-hangar-daemon` binary in one-shot mode (`--once`)
//! against an isolated `$AINB_HANGAR_HOME`, as `it_boot_registers_runtime`
//! does.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;

/// A run's output file name, as the daemon mints it: 32 hex digits.
const RUN: &str = "0123456789abcdef0123456789abcdef";

fn spawn_logs(home: &Path) -> PathBuf {
    home.join("hangar/logs/spawn")
}

fn boot_once(home: &Path) {
    let output = Command::cargo_bin("ainb-hangar-daemon")
        .expect("binary builds")
        .arg("--once")
        .env("AINB_HANGAR_HOME", home)
        .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
        .output()
        .expect("run --once");
    assert!(
        output.status.success(),
        "--once should exit 0, stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn a_boot_sweeps_the_run_output_a_previous_daemon_left() {
    let home = tempfile::tempdir().unwrap();
    let dir = spawn_logs(home.path());
    std::fs::create_dir_all(&dir).unwrap();
    for name in [format!("{RUN}.stdout"), format!("{RUN}.stderr")] {
        std::fs::write(dir.join(name), "left by a killed daemon").unwrap();
    }

    boot_once(home.path());

    assert_eq!(
        names(&dir),
        Vec::<String>::new(),
        "a boot left the previous run output"
    );
}

/// Only a run's own names go: anything else in the directory is not the
/// daemon's to remove, however it got there.
#[test]
fn a_file_not_named_like_run_output_survives_the_sweep() {
    let home = tempfile::tempdir().unwrap();
    let dir = spawn_logs(home.path());
    std::fs::create_dir_all(&dir).unwrap();
    let kept = [
        "notes.txt".to_string(),
        "0123abcd.stdout".to_string(),
        format!("{RUN}.log"),
        format!("{}.stdout", RUN.to_uppercase()),
    ];
    for name in &kept {
        std::fs::write(dir.join(name), "not a run's").unwrap();
    }
    std::fs::write(dir.join(format!("{RUN}.stdout")), "a run's").unwrap();

    boot_once(home.path());

    let mut expected = kept.to_vec();
    expected.sort();
    assert_eq!(names(&dir), expected);
}

/// `logs/spawn` swapped for a symlink: the sweep does not follow it, so the
/// directory it points at keeps its files, even one named like run output.
#[test]
fn a_symlinked_spawn_dir_is_not_swept() {
    let home = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let precious = elsewhere.path().join(format!("{RUN}.stdout"));
    std::fs::write(&precious, "not the daemon's").unwrap();
    std::fs::create_dir_all(home.path().join("hangar/logs")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), spawn_logs(home.path())).unwrap();

    boot_once(home.path());

    assert_eq!(
        std::fs::read_to_string(&precious).ok().as_deref(),
        Some("not the daemon's"),
        "the sweep followed logs/spawn into another directory"
    );
}

/// A symlink inside the directory, named like run output, is not followed:
/// the file it points at stays.
#[test]
fn a_symlink_inside_the_spawn_dir_is_not_followed() {
    let home = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let precious = elsewhere.path().join("precious.txt");
    std::fs::write(&precious, "not the daemon's").unwrap();
    let dir = spawn_logs(home.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&precious, dir.join(format!("{RUN}.stderr"))).unwrap();

    boot_once(home.path());

    assert_eq!(
        std::fs::read_to_string(&precious).ok().as_deref(),
        Some("not the daemon's")
    );
}

/// A `logs/spawn` an older daemon made `0755` is made `0700` at boot:
/// `DirBuilder`'s mode only applies to a directory it creates.
#[test]
fn an_existing_open_spawn_dir_is_made_private_at_boot() {
    let home = tempfile::tempdir().unwrap();
    let dir = spawn_logs(home.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    boot_once(home.path());

    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "logs/spawn is {mode:o}");
}

/// A `logs/spawn` an older daemon made group-writable (`create_dir_all`
/// under umask 002, Ubuntu's default) is still this user's: it is made `0700`
/// and swept, not refused forever.
#[test]
fn an_existing_group_writable_spawn_dir_is_made_private_and_swept() {
    let home = tempfile::tempdir().unwrap();
    let dir = spawn_logs(home.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).unwrap();
    std::fs::write(dir.join(format!("{RUN}.stdout")), "left by a killed daemon").unwrap();

    boot_once(home.path());

    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "logs/spawn is {mode:o}");
    assert_eq!(
        names(&dir),
        Vec::<String>::new(),
        "the run output was not swept"
    );
}
