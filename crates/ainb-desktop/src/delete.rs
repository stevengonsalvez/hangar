//! Delete a session from the window, and say first what that removes.
//!
//! The delete is the terminal's own: [`InteractiveSessionManager::remove_session`]
//! kills the session's tmux, removes its worktree unless another session still
//! works in it ([`tree_in_use_by_another`]), and drops its row. The preview asks
//! the same questions that removal does, without doing anything, so the dialog
//! can say whether the folder goes and how much uncommitted work goes with it.
//!
//! ```text
//!  dialog ──session_delete_preview──▶ host: which tree, shared?, git status
//!         ──session_delete(fate)────▶ host: same fate still? remove_session,
//!                                     then a rescan
//! ```

use std::path::PathBuf;

use ainb_app::git::WorktreeManager;
use ainb_app::interactive::InteractiveSessionManager;
use ainb_app::interactive::session_manager::{SessionStore, tree_in_use_by_another};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What deleting a session does to the folder it runs in. Sent back with the
/// delete, so the host deletes only what the person was told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum TreeFate {
    /// No other session uses the worktree: it is removed from git and disk.
    Removed,
    /// Another session still works in the worktree: only this session goes.
    Shared,
    /// Not a worktree ainb made (the person's own checkout), or nothing on
    /// disk: only the session goes.
    Kept,
}

/// What the delete dialog says before anything is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct DeletePreview {
    pub tree: TreeFate,
    /// Uncommitted and untracked files the removal takes with it: only asked
    /// when the tree is [`TreeFate::Removed`], and `None` there when git could
    /// not answer.
    pub changes: Option<u32>,
}

/// The id the window sent, or the sentence it shows when that is not one.
fn session_id(raw: &str) -> Result<Uuid, String> {
    Uuid::parse_str(raw).map_err(|_| format!("{raw:?} is not a session id"))
}

/// The tree [`remove_session_worktree`] would act on for `session`: its
/// `by-session` link, else its row's path when that is a tree ainb made.
///
/// [`remove_session_worktree`]: ainb_app::interactive::session_manager::remove_session_worktree
fn tree_of(
    manager: &WorktreeManager,
    store: &SessionStore,
    session: Uuid,
) -> Result<Option<PathBuf>, String> {
    let linked = manager
        .session_dir(session)
        .map_err(|error| format!("the worktree link could not be read: {error}"))?;
    Ok(linked.or_else(|| {
        store
            .sessions()
            .values()
            .find(|row| row.session_id == session)
            .and_then(|row| manager.managed_tree(&row.worktree_path))
    }))
}

/// What deleting `session` does to its folder, and which folder.
///
/// Only a session in the session list: a Boss session keeps no row (its
/// container is the terminal's to remove), and a remote one is not on this
/// machine, so an id the list does not hold is refused rather than guessed at.
fn fate(
    manager: &WorktreeManager,
    store: &SessionStore,
    session: Uuid,
) -> Result<(TreeFate, Option<PathBuf>), String> {
    if !store.sessions().values().any(|row| row.session_id == session) {
        return Err(
            "This session is not in this machine's session list, so the window does not delete it."
                .into(),
        );
    }
    Ok(match tree_of(manager, store, session)? {
        None => (TreeFate::Kept, None),
        Some(tree) if tree_in_use_by_another(manager, store, &tree, Some(session)) => {
            (TreeFate::Shared, Some(tree))
        }
        Some(tree) => (TreeFate::Removed, Some(tree)),
    })
}

/// What deleting `session` would do, read from `manager` and `store` as they
/// are now. Blocking: it may run `git status`.
///
/// # Errors
/// The session is not in `store`, or its worktree link exists and could not
/// be followed.
pub fn plan(
    manager: &WorktreeManager,
    store: &SessionStore,
    session: Uuid,
) -> Result<DeletePreview, String> {
    let (tree, path) = fate(manager, store, session)?;
    let changes = match (tree, path) {
        (TreeFate::Removed, Some(path)) => WorktreeManager::uncommitted_file_count_at(&path)
            .map_err(|error| tracing::warn!(%error, "delete preview: git status failed"))
            .ok()
            .map(|count| u32::try_from(count).unwrap_or(u32::MAX)),
        _ => None,
    };
    Ok(DeletePreview { tree, changes })
}

/// Run `read` against the session store and the worktree folder as they are
/// now, off the async thread.
async fn read_now<T: Send + 'static>(
    read: impl FnOnce(&WorktreeManager, &SessionStore) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let store = ainb_app::cli::util::load_session_store_async()
        .await
        .map_err(|error| format!("The session store could not be read: {error}"))?;
    tokio::task::spawn_blocking(move || {
        let manager = WorktreeManager::for_reading()
            .map_err(|error| format!("The worktree folder is unknown: {error}"))?;
        read(&manager, &store)
    })
    .await
    .map_err(|error| format!("The check stopped: {error}"))?
}

/// [`plan`] for the window's dialog.
///
/// # Errors
/// A sentence for the dialog: a bad id, a store that cannot be read (so who
/// else is in the tree is unknown), a session not in it, or a link that
/// cannot be followed.
pub async fn preview(raw: &str) -> Result<DeletePreview, String> {
    let session = session_id(raw)?;
    read_now(move |manager, store| plan(manager, store, session)).await
}

/// Delete `raw`'s session the way the terminal's `d` then Delete does, if
/// what that removes is still what the dialog said: `expected`, the fate the
/// person confirmed. Two sessions sharing a tree can both be confirmed while
/// the first delete runs; the second must not then take the folder it
/// promised to keep.
///
/// # Errors
/// A sentence for a toast: a bad id, a fate that changed (nothing is
/// deleted), a failed removal, or a folder the removal left behind.
pub async fn delete(raw: &str, expected: TreeFate) -> Result<(), String> {
    let session = session_id(raw)?;
    let (now, tree) = read_now(move |manager, store| fate(manager, store, session)).await?;
    if now != expected {
        return Err("What deleting this session removes has changed since the dialog asked. Nothing was deleted: open Delete again.".into());
    }
    let mut manager = InteractiveSessionManager::new()
        .map_err(|error| format!("The session was not deleted: {error}"))?;
    manager
        .remove_session(session)
        .await
        .map_err(|error| format!("The session was not deleted: {error}"))?;
    // The removal logs a tree it could not remove and goes on to the row, so
    // the folder's own presence is the answer to whether it went.
    if now == TreeFate::Removed && tree.is_some_and(|tree| tree.exists()) {
        return Err(
            "The session was deleted, but its worktree folder could not be removed: see desktop.log."
                .into(),
        );
    }
    tracing::info!(%session, ?now, "window deleted a session");
    Ok(())
}
