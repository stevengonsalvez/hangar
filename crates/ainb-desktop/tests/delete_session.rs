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

/// Ends one tmux session by exact name on the test's own socket when the
/// test is done, whatever the delete did.
struct EndOnDrop {
    socket: PathBuf,
    name: String,
}

impl Drop for EndOnDrop {
    fn drop(&mut self) {
        eprintln!(
            "ending tmux session {} on {}",
            self.name,
            self.socket.display()
        );
        let _ = tmux(
            &self.socket,
            &["kill-session", "-t", &format!("={}", self.name)],
        );
    }
}

/// Clears the test server's `session-closed` hook, so the sessions can end.
struct Unhook(PathBuf);

impl Drop for Unhook {
    fn drop(&mut self) {
        let _ = tmux(&self.0, &["set-hook", "-gu", "session-closed"]);
    }
}

fn tmux_available() -> bool {
    let found = Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success());
    assert!(
        found || std::env::var_os("CI").is_none(),
        "tmux is missing under CI"
    );
    found
}

/// The socket a bare `tmux` reaches under the private TMUX_TMPDIR [`home`]
/// set: the one the removal's own tmux calls reach.
fn private_socket() -> PathBuf {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let tmux_dir = PathBuf::from(std::env::var_os("TMUX_TMPDIR").expect("home() set it"));
    let uid = std::fs::metadata(&tmux_dir).unwrap().uid();
    let socket_dir = tmux_dir.join(format!("tmux-{uid}"));
    std::fs::create_dir_all(&socket_dir).unwrap();
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    socket_dir.join("default")
}

/// The name `ainb run` gives `session`'s tmux session, for workspace `repo`.
fn ainb_run_name(session: Uuid) -> String {
    format!("tmux_repo-{}", &session.simple().to_string()[..8])
}

fn tmux(socket: &Path, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .args(args)
        .output()
        .expect("tmux runs")
}

fn start(socket: &Path, name: &str, dir: &Path) {
    let dir = dir.to_str().unwrap();
    let started = tmux(
        socket,
        &["new-session", "-d", "-s", name, "-c", dir, "sleep 600"],
    );
    assert!(started.status.success(), "{started:?}");
}

fn running(socket: &Path, name: &str) -> bool {
    tmux(socket, &["has-session", "-t", &format!("={name}")]).status.success()
}

#[tokio::test]
async fn deleting_a_session_ends_its_agents_tmux_session() {
    // The desktop makes sessions through `ainb run`, which names the tmux
    // session `tmux_<workspace>-<id>`, not the `tmux_<folder>_<branch>` a
    // name derived from the worktree would be. The delete must end the one
    // the row names, or the agent keeps running in a deleted folder.
    if !tmux_available() {
        return;
    }
    let _one = STORE.lock().await;
    let base = home();
    let (session, tree) = managed_tree(&base);
    let socket = private_socket();
    let name = ainb_run_name(session);
    let _end = EndOnDrop {
        socket: socket.clone(),
        name: name.clone(),
    };
    start(&socket, &name, &tree);
    let mut agent = row(session, &tree);
    agent.tmux_session_name = name.clone();
    save(&[agent]);

    delete(&session.to_string(), TreeFate::Removed, Some(0), false)
        .await
        .expect("deleted");

    assert!(!tree.exists(), "the worktree folder is gone");
    assert!(!listed(session), "the row is gone");
    assert!(
        !running(&socket, &name),
        "the agent's tmux session {name} is still running in a deleted folder"
    );
}

#[tokio::test]
async fn a_tmux_session_that_will_not_end_refuses_the_delete() {
    // A kill that leaves the agent running must not be logged and passed
    // over: the folder and the row stay, and the window hears why.
    if !tmux_available() {
        return;
    }
    let _one = STORE.lock().await;
    let base = home();
    let (session, tree) = managed_tree(&base);
    let socket = private_socket();
    let name = ainb_run_name(session);
    let keeper = format!("keeper-{}", session.simple());
    let _end = EndOnDrop {
        socket: socket.clone(),
        name: name.clone(),
    };
    let _end_keeper = EndOnDrop {
        socket: socket.clone(),
        name: keeper.clone(),
    };
    // The server outlives the agent's session, and brings it straight back.
    start(&socket, &keeper, &tree);
    start(&socket, &name, &tree);
    let revive = format!(
        "new-session -d -s {name} -c {} \"sleep 600\"",
        tree.display()
    );
    assert!(tmux(&socket, &["set-hook", "-g", "session-closed", &revive]).status.success());
    // Dropped first: the hook goes before the sessions are ended.
    let _unhook = Unhook(socket.clone());
    let mut agent = row(session, &tree);
    agent.tmux_session_name = name.clone();
    save(&[agent]);

    let error = delete(&session.to_string(), TreeFate::Removed, Some(0), false)
        .await
        .expect_err("refused while the agent runs");

    assert!(error.contains("still running"), "{error}");
    assert!(tree.is_dir(), "the running agent's folder stays");
    assert!(listed(session), "the row stays");
}

#[tokio::test]
async fn a_session_whose_name_prefixes_another_leaves_the_other_running() {
    // The row's own session has already exited; another, whose name starts
    // with the row's, still runs. A prefix or pattern match would end it.
    if !tmux_available() {
        return;
    }
    let _one = STORE.lock().await;
    let base = home();
    let (session, tree) = managed_tree(&base);
    let socket = private_socket();
    let name = ainb_run_name(session);
    let sibling = format!("{name}x");
    let _end = EndOnDrop {
        socket: socket.clone(),
        name: sibling.clone(),
    };
    start(&socket, &sibling, base.parent().unwrap());
    let mut agent = row(session, &tree);
    agent.tmux_session_name = name.clone();
    save(&[agent]);

    delete(&session.to_string(), TreeFate::Removed, Some(0), false)
        .await
        .expect("deleted");

    assert!(!listed(session), "the row is gone");
    assert!(
        running(&socket, &sibling),
        "{sibling} was ended by deleting {name}"
    );
}

/// Puts a socket's mode back when the test is done, before its sessions end.
struct RestoreMode(PathBuf, u32);

impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(self.1));
    }
}

#[tokio::test]
async fn a_tmux_that_cannot_say_whether_the_agent_runs_refuses_the_delete() {
    // Only "no such session", "no server" or "no socket" mean the agent is
    // gone. A socket tmux may not open says nothing about it: the folder
    // and the row stay.
    if !tmux_available() {
        return;
    }
    let _one = STORE.lock().await;
    let base = home();
    let (session, tree) = managed_tree(&base);
    let socket = private_socket();
    let name = ainb_run_name(session);
    let _end = EndOnDrop {
        socket: socket.clone(),
        name: name.clone(),
    };
    start(&socket, &name, &tree);
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o7777;
    // Dropped first: the mode is back before the session is ended.
    let _restore = RestoreMode(socket.clone(), mode);
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut agent = row(session, &tree);
    agent.tmux_session_name = name.clone();
    save(&[agent]);

    let error = delete(&session.to_string(), TreeFate::Removed, Some(0), false)
        .await
        .expect_err("refused while tmux cannot answer");

    assert!(error.contains("could not say"), "{error}");
    assert!(tree.is_dir(), "the folder stays");
    assert!(listed(session), "the row stays");
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
