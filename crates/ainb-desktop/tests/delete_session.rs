//! The window's delete, end to end through the terminal's own removal: the
//! preview and the delete read the same session store and worktree folder,
//! a tree only this session uses goes with it, and a tree another session
//! works in stays for that session.
//!
//! Its own process: it points `AINB_HOME` at a scratch home, and tmux at a
//! private socket folder with no server in it, so the removal's
//! `tmux kill-session` can never reach a session anyone is using.

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb_app::interactive::session_manager::{SessionMetadata, SessionStore};
use ainb_desktop::delete::{DeletePreview, TreeFate, delete, preview};
use uuid::Uuid;

mod support;
use support::isolated_home;

/// The store is one file in the one scratch home: one test at a time.
static STORE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// A home whose tmux has no server, and the ainb worktree folder in it.
fn home() -> PathBuf {
    let home = isolated_home();
    let tmux = home.join("tmux");
    std::fs::create_dir_all(&tmux).unwrap();
    std::env::set_var("TMUX_TMPDIR", &tmux);
    std::env::remove_var("TMUX");
    home.join(".agents-in-a-box").join("worktrees")
}

/// A fresh repository with a linked worktree in `by-name`, linked from
/// `by-session` to a new session, which is returned with the tree.
fn managed_tree(base: &Path) -> (Uuid, PathBuf) {
    let session = Uuid::new_v4();
    let repo = base.parent().unwrap().join(format!("repo-{}", session.simple()));
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "hi\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    std::fs::create_dir_all(base.join("by-name")).unwrap();
    std::fs::create_dir_all(base.join("by-session")).unwrap();
    let tree = base.join("by-name").join(format!("wt-{}", session.simple()));
    let branch = format!("feat-{}", session.simple());
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            tree.to_str().unwrap(),
        ],
    );
    std::os::unix::fs::symlink(&tree, base.join("by-session").join(session.to_string())).unwrap();
    (session, tree)
}

fn row(session: Uuid, tree: &Path) -> SessionMetadata {
    serde_json::from_value(serde_json::json!({
        "session_id": session,
        "tmux_session_name": format!("o6-test-{}", session.simple()),
        "worktree_path": tree,
        "workspace_name": "repo",
        "created_at": "2024-01-01T00:00:00Z",
    }))
    .expect("a minimal row")
}

fn save(rows: &[SessionMetadata]) {
    let mut store = SessionStore::load();
    for one in rows {
        store.upsert(one.clone());
    }
    store.save().expect("store written");
}

fn listed(session: Uuid) -> bool {
    SessionStore::load().sessions().values().any(|one| one.session_id == session)
}

#[tokio::test]
async fn deleting_the_only_session_in_a_tree_takes_the_tree_and_its_row() {
    let _one = STORE.lock().await;
    let base = home();
    let (session, tree) = managed_tree(&base);
    save(&[row(session, &tree)]);
    std::fs::write(tree.join("notes.txt"), "unsaved\n").unwrap();

    assert_eq!(
        preview(&session.to_string()).await,
        Ok(DeletePreview {
            tree: TreeFate::Removed,
            changes: Some(1),
        })
    );
    delete(&session.to_string(), TreeFate::Removed, Some(1), true)
        .await
        .expect("deleted");

    assert!(!tree.exists(), "the worktree folder is gone");
    assert!(!listed(session), "the row is gone");
}

#[tokio::test]
async fn a_file_written_after_the_preview_refuses_the_delete_and_survives() {
    // A live agent writes while the dialog is open: the tree was clean when
    // counted, the confirm is a plain Delete, and the new file must not go.
    let _one = STORE.lock().await;
    let base = home();
    let (session, tree) = managed_tree(&base);
    save(&[row(session, &tree)]);
    let shown = preview(&session.to_string()).await.expect("previewed");
    assert_eq!(
        shown,
        DeletePreview {
            tree: TreeFate::Removed,
            changes: Some(0)
        }
    );

    std::fs::write(tree.join("agent-wrote.txt"), "fresh work\n").unwrap();
    let error = delete(&session.to_string(), shown.tree, shown.changes, false)
        .await
        .expect_err("refused");

    assert!(error.contains("Nothing was deleted"), "{error}");
    assert!(
        tree.join("agent-wrote.txt").is_file(),
        "the new file survives"
    );
    assert!(listed(session), "the session is untouched");

    // Nor does Force Delete cover a file the person was never shown.
    std::fs::write(tree.join("more.txt"), "more\n").unwrap();
    let error = delete(&session.to_string(), TreeFate::Removed, Some(1), true)
        .await
        .expect_err("refused even forced");
    assert!(error.contains("Nothing was deleted"), "{error}");
    assert!(tree.join("more.txt").is_file());
}

#[tokio::test]
async fn dirty_work_without_force_is_refused_by_the_host() {
    // The window asks for Force Delete on a dirty tree, but the host does not
    // rely on it: a plain delete of shown dirty work deletes nothing.
    let _one = STORE.lock().await;
    let base = home();
    let (session, tree) = managed_tree(&base);
    save(&[row(session, &tree)]);
    std::fs::write(tree.join("unsaved.txt"), "work\n").unwrap();
    let shown = preview(&session.to_string()).await.expect("previewed");
    assert_eq!(shown.changes, Some(1));

    let error = delete(&session.to_string(), shown.tree, shown.changes, false)
        .await
        .expect_err("refused without force");

    assert!(error.contains("Nothing was deleted"), "{error}");
    assert!(
        tree.join("unsaved.txt").is_file(),
        "the dirty file survives"
    );
    assert!(listed(session));
}

#[tokio::test]
async fn deleting_one_of_two_sessions_in_a_tree_keeps_the_tree_for_the_other() {
    let _one = STORE.lock().await;
    let base = home();
    let (session, tree) = managed_tree(&base);
    let other = Uuid::new_v4();
    save(&[row(session, &tree), row(other, &tree)]);

    assert_eq!(
        preview(&session.to_string()).await,
        Ok(DeletePreview {
            tree: TreeFate::Shared,
            changes: None,
        })
    );
    delete(&session.to_string(), TreeFate::Shared, None, false)
        .await
        .expect("deleted");

    assert!(tree.is_dir(), "the other session's tree stays");
    assert!(!listed(session), "this session's row is gone");
    assert!(listed(other), "the other session's row stays");
}

#[tokio::test]
async fn a_shared_tree_confirmed_as_kept_is_not_taken_once_the_other_session_has_gone() {
    // Both rows are confirmed as sharing the tree; the first delete runs
    // before the second is confirmed. The second must not now take the
    // folder its dialog promised to keep.
    let _one = STORE.lock().await;
    let base = home();
    let (first, tree) = managed_tree(&base);
    let second = Uuid::new_v4();
    save(&[row(first, &tree), row(second, &tree)]);
    std::fs::write(tree.join("notes.txt"), "unsaved\n").unwrap();
    assert_eq!(
        preview(&second.to_string()).await.map(|p| p.tree),
        Ok(TreeFate::Shared)
    );

    delete(&first.to_string(), TreeFate::Shared, None, false)
        .await
        .expect("first deleted");
    let error = delete(&second.to_string(), TreeFate::Shared, None, false)
        .await
        .expect_err("the fate changed under the dialog");

    assert!(error.contains("Nothing was deleted"), "{error}");
    assert!(
        tree.join("notes.txt").is_file(),
        "the unsaved work is still on disk"
    );
    assert!(listed(second), "the second session is untouched");
}

#[tokio::test]
async fn deleting_a_session_in_the_persons_own_checkout_leaves_the_checkout() {
    let _one = STORE.lock().await;
    let base = home();
    let (_made, tree) = managed_tree(&base);
    // A session that joined a checkout ainb did not make: no link, and its
    // row names a folder outside `by-name`.
    let own = tree.parent().unwrap().parent().unwrap().parent().unwrap().join("own");
    std::fs::create_dir_all(&own).unwrap();
    let joined = Uuid::new_v4();
    save(&[row(joined, &own)]);

    assert_eq!(
        preview(&joined.to_string()).await.map(|p| p.tree),
        Ok(TreeFate::Kept)
    );
    delete(&joined.to_string(), TreeFate::Kept, None, false).await.expect("deleted");

    assert!(own.is_dir(), "the person's own folder stays");
    assert!(!listed(joined));
}

#[tokio::test]
async fn a_session_the_list_does_not_hold_is_refused_and_its_tree_left() {
    // A Boss session keeps a link and no row; its container is the
    // terminal's to remove, so the window touches nothing of it.
    let _one = STORE.lock().await;
    let base = home();
    let (boss, tree) = managed_tree(&base);

    assert!(preview(&boss.to_string()).await.is_err());
    let error = delete(&boss.to_string(), TreeFate::Removed, Some(0), false)
        .await
        .expect_err("refused");
    assert!(
        error.contains("not in this machine's session list"),
        "{error}"
    );
    assert!(tree.is_dir(), "its tree is left");
}

#[tokio::test]
async fn a_bad_session_id_is_refused_before_anything_runs() {
    let error = delete("../etc", TreeFate::Removed, Some(0), false).await.expect_err("refused");
    assert!(error.contains("not a session id"), "{error}");
    assert!(preview("").await.is_err());
}
