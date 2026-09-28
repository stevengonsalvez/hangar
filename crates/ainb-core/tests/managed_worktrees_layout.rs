//! The daemon and the worktree manager name the same managed worktree folder.
//!
//! `worktree/agent_add` refuses any tree that is not a folder directly in the
//! daemon's `managed_worktrees` directory, and `ainb run --worktree` mints
//! every tree in the manager's `by-name` directory. The daemon cannot link the
//! manager (`ainb-app` depends on the daemon), so the layout is written down
//! twice; this pins the two copies equal, with and without `$AINB_HOME`.
//!
//! Its own process: it sets `HOME` and `AINB_HOME`, which are process-global.

use ainb::git::worktree_manager::WorktreeManager;
use ainb_hangar_daemon::spawn::managed_worktrees;

#[test]
fn the_daemon_and_the_manager_agree_on_the_managed_worktree_folder() {
    let home = tempfile::tempdir().expect("home");
    let override_home = tempfile::tempdir().expect("override home");

    // Edition 2021: set_var is safe. This binary has one test, so nothing
    // else reads the environment while it changes.
    std::env::set_var("HOME", home.path());
    std::env::remove_var("AINB_HOME");
    let manager = WorktreeManager::for_reading().expect("manager");
    assert_eq!(
        manager.base_dir().join("by-name"),
        managed_worktrees(home.path()),
        "without $AINB_HOME"
    );

    std::env::set_var("AINB_HOME", override_home.path());
    let manager = WorktreeManager::for_reading().expect("manager");
    assert_eq!(
        manager.base_dir().join("by-name"),
        managed_worktrees(home.path()),
        "with $AINB_HOME"
    );
    assert!(
        managed_worktrees(home.path()).starts_with(override_home.path()),
        "$AINB_HOME wins over the home"
    );
    std::env::remove_var("AINB_HOME");
}
