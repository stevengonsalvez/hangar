//! What the delete dialog is told before anything goes: a tree only this
//! session uses is removed and its uncommitted files counted, a tree another
//! session shares is kept, and a folder ainb did not make is never offered.
//!
//! Real git trees in a scratch folder, and the same `WorktreeManager` and
//! session-store types the terminal's delete reads.

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb_app::git::WorktreeManager;
use ainb_app::interactive::session_manager::{SessionMetadata, SessionStore};
use ainb_desktop::delete::{DeletePreview, TreeFate, plan};
use uuid::Uuid;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

/// A repository with one commit, and a linked worktree of it in the
/// manager's `by-name` folder, linked to `session` from `by-session`.
struct Fixture {
    _scratch: tempfile::TempDir,
    manager: WorktreeManager,
    tree: PathBuf,
    session: Uuid,
}

fn fixture() -> Fixture {
    let scratch = tempfile::tempdir().expect("scratch");
    let repo = scratch.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "hi\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let base = scratch.path().join("worktrees");
    std::fs::create_dir_all(base.join("by-name")).unwrap();
    std::fs::create_dir_all(base.join("by-session")).unwrap();
    let tree = base.join("by-name").join("repo--feat");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat",
            tree.to_str().unwrap(),
        ],
    );
    let session = Uuid::new_v4();
    std::os::unix::fs::symlink(&tree, base.join("by-session").join(session.to_string())).unwrap();

    Fixture {
        manager: WorktreeManager::with_base_dir(base).expect("manager"),
        tree,
        session,
        _scratch: scratch,
    }
}

fn row(session: Uuid, tmux: &str, tree: &Path) -> SessionMetadata {
    serde_json::from_value(serde_json::json!({
        "session_id": session,
        "tmux_session_name": tmux,
        "worktree_path": tree,
        "workspace_name": "repo",
        "created_at": "2024-01-01T00:00:00Z",
    }))
    .expect("a minimal row")
}

#[test]
fn a_tree_only_this_session_uses_is_removed_with_its_changes_counted() {
    let f = fixture();
    let mut store = SessionStore::default();
    store.upsert(row(f.session, "one", &f.tree));
    std::fs::write(f.tree.join("README"), "edited\n").unwrap();
    std::fs::write(f.tree.join("notes.txt"), "new\n").unwrap();

    assert_eq!(
        plan(&f.manager, &store, f.session),
        Ok(DeletePreview {
            tree: TreeFate::Removed,
            changes: Some(2),
        })
    );
}

#[test]
fn a_clean_tree_reports_no_changes_rather_than_unknown() {
    let f = fixture();
    let mut store = SessionStore::default();
    store.upsert(row(f.session, "one", &f.tree));

    let preview = plan(&f.manager, &store, f.session).unwrap();
    assert_eq!(preview.changes, Some(0));
}

#[test]
fn a_tree_another_session_works_in_is_kept() {
    let f = fixture();
    let mut store = SessionStore::default();
    store.upsert(row(f.session, "one", &f.tree));
    store.upsert(row(Uuid::new_v4(), "two", &f.tree));
    std::fs::write(f.tree.join("notes.txt"), "new\n").unwrap();

    assert_eq!(
        plan(&f.manager, &store, f.session),
        Ok(DeletePreview {
            tree: TreeFate::Shared,
            changes: None,
        })
    );
}

#[test]
fn a_folder_ainb_did_not_make_is_kept() {
    let f = fixture();
    // A session that joined the person's own checkout: no link, and its row
    // names a folder outside `by-name`.
    let joined = Uuid::new_v4();
    let own = f._scratch.path().join("repo");
    let mut store = SessionStore::default();
    store.upsert(row(joined, "joined", &own));

    assert_eq!(
        plan(&f.manager, &store, joined),
        Ok(DeletePreview {
            tree: TreeFate::Kept,
            changes: None,
        })
    );
}

#[test]
fn a_session_the_list_does_not_hold_is_refused() {
    // A Boss session keeps its link and no row.
    let f = fixture();
    let store = SessionStore::default();
    let refused = plan(&f.manager, &store, f.session).expect_err("refused");
    assert!(
        refused.contains("not in this machine's session list"),
        "{refused}"
    );
}
