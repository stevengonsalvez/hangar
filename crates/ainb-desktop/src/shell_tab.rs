//! A plain shell tab in a worktree, from the window.
//!
//! The daemon opens and ends the shell (`shell/create`, `shell/close`); the
//! window only attaches it as a tab. The page names a session, never a path:
//! the host finds that session's worktree in its own state and hands the
//! daemon that folder, so a page cannot open a shell anywhere else.
//!
//! ```text
//!  "New terminal" ──shell_open(session id)──▶ host ──shell/create──▶ daemon
//!                  ◀── tab on ainb-dsh-<id8> ─┘
//!  tab ×  ──shell_close(tab key)──▶ host: tab closed ──shell/close──▶ daemon
//!  launch ──shell/list──▶ daemon: each running shell comes back as a tab
//! ```
//!
//! As in Orca, closing a terminal tab ends its shell, and a relaunch
//! reattaches the shells still running: the daemon, not the window, owns
//! them, so they outlive the window as tmux does.

use ainb_app::Intent;
use ainb_app::app::state::AppState;
use ainb_hangar_client::{DaemonClient, DaemonError};
use ainb_hangar_proto::mutation::{MutationEnvelope, OpId};
use ainb_hangar_proto::spawn::{ShellCloseParams, ShellCreateParams, ShellCreateResult};
use uuid::Uuid;

use crate::create::{SpawnVerb, mint_op_id, spawn_refusal_text};
use crate::terminal::{TabTarget, Terminals};

/// The worktree folder of the listed session `session_id`, from the host's
/// own state. `None` for a session the host does not list.
#[must_use]
pub fn session_worktree(state: &AppState, session_id: Uuid) -> Option<String> {
    state
        .sessions
        .workspaces
        .iter()
        .find_map(|workspace| workspace.get_session(&session_id))
        .map(|session| session.workspace_path.clone())
}

/// The `shell/create` params for a shell in `worktree_path`.
#[must_use]
pub fn create_params(worktree_path: &str, op_id: OpId) -> ShellCreateParams {
    ShellCreateParams {
        worktree_path: worktree_path.to_string(),
        mutation: MutationEnvelope {
            op_id: Some(op_id),
            fence: None,
        },
    }
}

/// The `shell/close` params for the shell `tmux_session_name`.
#[must_use]
pub fn close_params(tmux_session_name: &str, op_id: OpId) -> ShellCloseParams {
    ShellCloseParams {
        tmux_session_name: tmux_session_name.to_string(),
        mutation: MutationEnvelope {
            op_id: Some(op_id),
            fence: None,
        },
    }
}

/// What an open that may have made its shell says: the shell is not known to
/// be gone, so another press could make a second one.
pub const MAY_STILL_OPEN: &str = "The terminal may still open; check before opening another.";

/// Ask the daemon for a shell in `worktree_path`.
///
/// # Errors
/// The sentence for the window ([`spawn_refusal_text`]).
pub async fn open(client: &DaemonClient, worktree_path: &str) -> Result<ShellCreateResult, String> {
    client
        .shell_create(&create_params(
            worktree_path,
            mint_op_id(SpawnVerb::OpenTerminal),
        ))
        .await
        .map_err(|error| spawn_refusal_text(&error, SpawnVerb::OpenTerminal))
}

/// Ask the daemon to end the shell `tmux_session_name`. `Ok(false)` when it
/// was already gone, or is not one the daemon opened (`@ainb_owner`).
///
/// # Errors
/// The sentence for the window ([`spawn_refusal_text`]).
pub async fn close(client: &DaemonClient, tmux_session_name: &str) -> Result<bool, String> {
    client
        .shell_close(&close_params(
            tmux_session_name,
            mint_op_id(SpawnVerb::CloseTerminal),
        ))
        .await
        .map(|closed| closed.closed)
        .map_err(|error| spawn_refusal_text(&error, SpawnVerb::CloseTerminal))
}

/// Why a new shell tab did not open.
#[derive(Debug)]
pub struct OpenTabError {
    /// The sentence for the window.
    pub message: String,
    /// The reducer's report of a failed attach, for the host to dispatch.
    pub report: Option<Intent>,
}

/// Open a shell in `worktree_path` and attach it as a tab; answer the tab's
/// key.
///
/// A shell whose tab cannot attach is closed again before this returns: the
/// window shows no tab for it, so nothing else would ever end it.
///
/// # Errors
/// The daemon's refusal, or the failed attach with its report.
pub async fn open_tab(
    client: &DaemonClient,
    terminals: &Terminals,
    worktree_path: &str,
) -> Result<String, OpenTabError> {
    let shell = open(client, worktree_path).await.map_err(|message| OpenTabError {
        message,
        report: None,
    })?;
    let name = shell.tmux_session_name.clone();
    let Some(report) = terminals.open(target(&shell)) else {
        return Ok(name);
    };
    let message = match close(client, &name).await {
        Ok(_) => "The terminal opened but its tab could not attach, so it was closed again.".into(),
        Err(error) => {
            format!(
                "The terminal opened but its tab could not attach, and closing it failed: {error}"
            )
        }
    };
    Err(OpenTabError {
        message,
        report: Some(report),
    })
}

/// The tab a daemon shell is attached as.
#[must_use]
pub fn target(shell: &ShellCreateResult) -> TabTarget {
    TabTarget::Shell {
        tmux: shell.tmux_session_name.clone(),
        dir: shell.worktree_path.clone(),
    }
}

/// Whether the listed tab `key` is a daemon shell's, the only tabs the
/// shell commands act on.
#[must_use]
pub fn is_shell_tab(terminals: &Terminals, key: &str) -> bool {
    matches!(terminals.target(key), Some(TabTarget::Shell { .. }))
}

/// Close the shell tab `key` and end its shell.
///
/// The tab goes first: ending the shell under an attached tab would read as
/// a session that died, with its own notice. A tab that is not a daemon
/// shell's is refused and left open, so the page cannot end any other tmux
/// session by naming it.
///
/// # Errors
/// The sentence for the window: not a shell tab, or the daemon's refusal
/// (the shell then still runs, and the next launch lists it again).
pub async fn close_tab(
    terminals: &Terminals,
    client: &DaemonClient,
    key: &str,
) -> Result<bool, String> {
    if !is_shell_tab(terminals, key) {
        return Err(format!("{key} is not a terminal tab this window opened"));
    }
    terminals.close(key);
    close(client, key).await
}

/// Re-attach the detached shell tab `key`. A shell has no session-list row
/// to re-attach through, so the host re-opens its tab directly. `Some` is
/// the failure report for the reducer; a key that is not a shell tab does
/// nothing.
pub fn reattach(terminals: &Terminals, key: &str) -> Option<Intent> {
    match terminals.target(key) {
        Some(target @ TabTarget::Shell { .. }) => terminals.open(target),
        _ => None,
    }
}

/// Open a tab, unfocused, for every shell the daemon still runs: the
/// relaunch half of Orca's model. Returns how many were listed and the
/// failure reports of those that could not attach.
///
/// # Errors
/// The daemon could not list its shells.
pub async fn restore(
    client: &DaemonClient,
    terminals: &Terminals,
) -> Result<(usize, Vec<Intent>), DaemonError> {
    let listed = client.shell_list().await?;
    let reports = listed
        .shells
        .iter()
        .filter_map(|shell| terminals.open_unfocused(target(shell)))
        .collect();
    Ok((listed.shells.len(), reports))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_app::config::AppConfig;
    use ainb_hangar_proto::spawn::{REPO_NOT_REGISTERED, SPAWN_STARTED};

    /// The JSON-RPC code for a method the daemon does not serve.
    const METHOD_NOT_FOUND: i32 = -32601;
    /// The JSON-RPC code for params the daemon refused.
    const INVALID_PARAMS: i32 = -32602;
    use ainb_app::models::{Session, Workspace};

    fn state_with(session: &Session) -> AppState {
        let mut state = AppState::with_config(AppConfig::default());
        let mut workspace = Workspace::new("app".into(), "/code/app".into());
        workspace.sessions.push(session.clone());
        state.sessions.workspaces.push(workspace);
        state
    }

    #[test]
    fn a_listed_session_names_its_worktree_and_nothing_else_does() {
        let session = Session::new("feat".into(), "/code/app/.worktrees/feat".into());
        let state = state_with(&session);
        assert_eq!(
            session_worktree(&state, session.id).as_deref(),
            Some("/code/app/.worktrees/feat")
        );
        assert_eq!(session_worktree(&state, Uuid::new_v4()), None);
    }

    #[test]
    fn every_press_carries_its_own_op_id() {
        let first = create_params("/w", mint_op_id(SpawnVerb::OpenTerminal)).mutation.op_id;
        let second = create_params("/w", mint_op_id(SpawnVerb::OpenTerminal)).mutation.op_id;
        assert!(first.is_some() && second.is_some());
        assert_ne!(first, second, "two presses are two shells");
        let close_a = close_params("ainb-dsh-0123abcd", mint_op_id(SpawnVerb::CloseTerminal))
            .mutation
            .op_id;
        let close_b = close_params("ainb-dsh-0123abcd", mint_op_id(SpawnVerb::CloseTerminal))
            .mutation
            .op_id;
        assert_ne!(close_a, close_b);
    }

    #[test]
    fn refusals_are_chosen_by_code_and_carry_the_daemon_detail() {
        let dark = DaemonError::Rpc {
            code: METHOD_NOT_FOUND,
            message: "unknown method: shell/create".into(),
        };
        assert!(spawn_refusal_text(&dark, SpawnVerb::OpenTerminal).contains("AINB_HANGAR_SPAWN=0"));
        let unregistered = DaemonError::Rpc {
            code: REPO_NOT_REGISTERED,
            message: "worktree_path is neither a registered repository".into(),
        };
        let text = spawn_refusal_text(&unregistered, SpawnVerb::OpenTerminal);
        assert!(text.contains("Add project"), "{text}");
        assert!(text.contains("neither a registered repository"), "{text}");
        let bad = DaemonError::Rpc {
            code: INVALID_PARAMS,
            message: "tmux_session_name is not a daemon shell".into(),
        };
        let text = spawn_refusal_text(&bad, SpawnVerb::CloseTerminal);
        assert!(text.starts_with("Closing the terminal failed"), "{text}");
        assert!(text.contains("not a daemon shell"), "{text}");
        // The same words under another code are not the unregistered hint:
        // the code decides, not the text.
        let worded = DaemonError::Rpc {
            code: INVALID_PARAMS,
            message: "not in a registered project folder".into(),
        };
        assert!(!spawn_refusal_text(&worded, SpawnVerb::OpenTerminal).contains("Add project"));
        let failed = DaemonError::Rpc {
            code: -32603,
            message: "tmux could not start the shell".into(),
        };
        assert!(
            spawn_refusal_text(&failed, SpawnVerb::OpenTerminal)
                .ends_with("tmux could not start the shell")
        );
        // tmux may have made the shell: not a failure, and not one to retry.
        let started = DaemonError::Rpc {
            code: SPAWN_STARTED,
            message: "tmux did not answer within 10s; the shell may still appear as \
                      ainb-dsh-0123abcd"
                .into(),
        };
        let text = spawn_refusal_text(&started, SpawnVerb::OpenTerminal);
        assert!(!text.contains("failed"), "{text}");
        assert!(text.starts_with(MAY_STILL_OPEN), "{text}");
        assert!(
            text.contains("may still appear as ainb-dsh-0123abcd"),
            "{text}"
        );
        let slow = DaemonError::Timeout(std::time::Duration::from_secs(30));
        let text = spawn_refusal_text(&slow, SpawnVerb::OpenTerminal);
        assert!(text.starts_with(MAY_STILL_OPEN), "{text}");
        // A close that timed out opened nothing.
        let text = spawn_refusal_text(&slow, SpawnVerb::CloseTerminal);
        assert!(text.starts_with("Closing the terminal failed"), "{text}");
    }
}
