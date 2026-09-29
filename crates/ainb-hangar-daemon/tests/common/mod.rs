//! Shared helpers for the P2 Beads round-trip tripwire (and later phases).
//!
//! Lives at `tests/common/mod.rs` (not `tests/common.rs`) so Cargo treats it as
//! a shared module included via `mod common;` rather than compiling it as its own
//! test binary. These helpers cover the real-`bd` setup the P2.6 tripwire needs:
//! probing for the binary and initialising a fresh, isolated beads root, and
//! the private host a dispatching test runs on so it never reaches the real
//! user's home or tmux server.

#![allow(dead_code)] // not every including test uses every helper

use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether the real `bd` binary is resolvable on `PATH`.
///
/// The round-trip tripwire requires the genuine tracker; a test that calls this
/// should skip-with-hint (rather than fail) when it returns `false`, so local dev
/// without `bd` installed stays green.
#[must_use]
pub fn bd_available() -> bool {
    which::which("bd").is_ok()
}

/// `bd init` a fresh `$BEADS_DIR` under `home`, returning the beads root path.
///
/// `BEADS_DIR` is set explicitly (worktree discipline — `bd` cannot infer the
/// root from cwd inside a git worktree) and the command's cwd is pointed at
/// `home` so it does not latch onto an ambient repo's beads db. Panics if `bd
/// init` fails — a tripwire cannot proceed without a tracker root.
///
/// # Panics
///
/// Panics if the `bd init` subprocess cannot be spawned or exits non-zero.
#[must_use]
pub fn bd_init(home: &Path) -> PathBuf {
    let beads_dir = home.join(".beads");
    let status = Command::new("bd")
        .arg("init")
        .env("BEADS_DIR", &beads_dir)
        .current_dir(home)
        .status()
        .expect("spawn bd init");
    assert!(
        status.success(),
        "bd init failed in {}",
        beads_dir.display()
    );
    beads_dir
}

/// A host this process keeps to itself, set before any test dispatches: an
/// empty `HOME` (no registered folder, so a spawn verb's request is refused
/// before it runs anything), a private tmux server directory with no server
/// in it, and the spawn switch unset, as a default daemon has it. Every test
/// calls this first; the values outlive the tests on purpose.
///
/// # Panics
///
/// Panics if the home or the tmux server directory cannot be created.
pub fn private_host() {
    static SET: std::sync::Once = std::sync::Once::new();
    SET.call_once(|| {
        let home = tempfile::tempdir().unwrap().keep();
        // Under /tmp: macOS caps unix socket paths at 104 bytes.
        let tmux_dir = PathBuf::from("/tmp").join(format!("ainb-host-{}", std::process::id()));
        std::fs::create_dir_all(&tmux_dir).unwrap();
        // Edition 2021: set_var is safe, and the Once runs it before any
        // test goes on.
        std::env::set_var("HOME", home);
        std::env::remove_var("AINB_HOME");
        std::env::set_var("TMUX_TMPDIR", tmux_dir);
        std::env::remove_var("TMUX");
        std::env::remove_var(ainb_hangar_daemon::spawn::SPAWN_ENV);
    });
}
