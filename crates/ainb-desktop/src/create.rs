//! Create a worktree with an agent in it, from the window.
//!
//! The window asks and attaches; the daemon does the work (`worktree/create`).
//! Nothing here runs git or tmux: the daemon is the one owner of new work, so
//! a paired device later creates work through the same verb.
//!
//! ```text
//!  composer ──worktree_create──▶ host ──worktree/create──▶ daemon
//!           ◀── CreatedWorktree ──┘ then attaches the new tmux session as a tab
//! ```

use std::path::{Path, PathBuf};

use ainb_hangar_client::{DaemonClient, DaemonError};
use ainb_hangar_proto::mutation::{MutationEnvelope, OpId};
use ainb_hangar_proto::spawn::{
    REPO_NOT_REGISTERED, SPAWN_STARTED, SpawnAgent, WorktreeCreateParams, WorktreeCreateResult,
};
use serde::{Deserialize, Serialize};

/// The JSON-RPC code for a method the daemon does not serve.
const METHOD_NOT_FOUND: i32 = -32601;
/// The JSON-RPC code for params the daemon refused.
const INVALID_PARAMS: i32 = -32602;

/// What the composer sends: the fields of the new worktree session.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct CreateWorktreeArgs {
    /// Absolute path of the source repository.
    pub repo_path: String,
    /// New branch name; the daemon picks one when absent.
    pub branch: Option<String>,
    /// Ref the branch starts from; the repository's default branch when absent.
    pub base: Option<String>,
    /// The agent CLI: the daemon's own `SpawnAgent`, so the list of agents is
    /// written once, in the proto, and generated into TypeScript.
    pub agent: SpawnAgent,
    /// Provider model id, passed through unchanged.
    pub model: Option<String>,
    /// First prompt for the agent.
    pub prompt: Option<String>,
}

/// What the window needs back: the session to attach and to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct CreatedWorktree {
    /// The ainb session id.
    pub session_id: String,
    /// The tmux session running the agent; also the new tab's key.
    pub tmux_session_name: String,
    /// The worktree directory.
    pub worktree_path: String,
    /// The branch the worktree is on.
    pub branch: String,
}

impl From<WorktreeCreateResult> for CreatedWorktree {
    fn from(result: WorktreeCreateResult) -> Self {
        Self {
            session_id: result.session_id,
            tmux_session_name: result.tmux_session_name,
            worktree_path: result.worktree_path,
            branch: result.branch,
        }
    }
}

/// Blank optional text is "not given", so an empty field in the composer
/// never reaches the daemon as an empty branch or model.
fn given(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// The daemon params for `args`, deduplicated by `op_id`. Permission
/// prompts are never skipped from the window: that stays a deliberate CLI
/// flag.
#[must_use]
pub fn params(args: CreateWorktreeArgs, op_id: OpId) -> WorktreeCreateParams {
    WorktreeCreateParams {
        repo_path: args.repo_path.trim().to_string(),
        branch: given(args.branch),
        base: given(args.base),
        agent: args.agent,
        model: given(args.model),
        prompt: args.prompt.filter(|p| !p.trim().is_empty()),
        skip_permissions: false,
        mutation: MutationEnvelope {
            op_id: Some(op_id),
            fence: None,
        },
    }
}

/// A fresh op id for one request. Every call to [`request`] mints its own:
/// nothing in the window retries a submit, so two submits (a person pressing
/// Create twice) are two ops the daemon keeps apart, never one it would fold.
#[must_use]
pub fn mint_op_id() -> OpId {
    OpId::parse(format!("desktop-create-{}", uuid::Uuid::new_v4().simple()))
        .expect("a uuid op id is always well formed")
}

/// A daemon error as the sentence the composer shows.
///
/// The spawn opt-out is named in the file the daemon reads it from, by the
/// daemon's own rule: `$AINB_HANGAR_HOME` when set, else `~/.agents-in-a-box`.
#[must_use]
pub fn refusal_text(error: &DaemonError) -> String {
    let spawn_config = ainb_hangar_daemon::hangar_dir().map_or_else(
        |_| PathBuf::from("$AINB_HANGAR_HOME/config/config.toml"),
        |home| ainb_hangar_daemon::spawn::config_path_in(&home),
    );
    refusal_text_in(error, &spawn_config)
}

/// [`refusal_text`] with the daemon's config file given, so the sentence is
/// testable without touching the environment.
#[must_use]
pub fn refusal_text_in(error: &DaemonError, spawn_config: &Path) -> String {
    match error {
        DaemonError::Rpc { code, .. } if *code == METHOD_NOT_FOUND => format!(
            "This daemon does not create worktrees: it is older than this app, or was started with AINB_HANGAR_SPAWN=0 or with [hangar] spawn = false in {} (or with that file unreadable). Update it, or restart it without that setting.",
            spawn_config.display()
        ),
        DaemonError::Rpc { code, .. } if *code == REPO_NOT_REGISTERED => {
            "This repository is not in a registered project folder: use Add project to pick its folder, then create again."
                .into()
        }
        DaemonError::Rpc { code, message } if *code == INVALID_PARAMS => {
            format!("The daemon refused the request: {message}")
        }
        // `ainb run` ran: it is still going, or it failed after starting.
        // Either way this is not a create that never happened.
        DaemonError::Rpc { code, message } if *code == SPAWN_STARTED => {
            format!("The create started: {message}")
        }
        DaemonError::Rpc { message, .. } => format!("Creating the worktree failed: {message}"),
        DaemonError::Timeout(_) => {
            "The daemon did not answer in time: the worktree may still be creating; check the sidebar."
                .into()
        }
        other => format!("The daemon is not reachable: {other}"),
    }
}

/// Ask the daemon for the worktree session.
///
/// # Errors
/// A sentence for the composer: a bad field, a daemon without the verb, or a
/// create that failed on the host.
pub async fn request(
    client: &DaemonClient,
    args: CreateWorktreeArgs,
) -> Result<CreatedWorktree, String> {
    let params = params(args, mint_op_id());
    client
        .worktree_create(&params)
        .await
        .map(CreatedWorktree::from)
        .map_err(|e| refusal_text(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> CreateWorktreeArgs {
        CreateWorktreeArgs {
            repo_path: " /repos/app ".into(),
            branch: Some(" feat/x ".into()),
            base: Some(String::new()),
            agent: SpawnAgent::Codex,
            model: Some("   ".into()),
            prompt: Some("fix it".into()),
        }
    }

    #[test]
    fn blank_fields_are_not_given_and_values_are_trimmed() {
        let p = params(args(), mint_op_id());
        assert_eq!(p.repo_path, "/repos/app");
        assert_eq!(p.branch.as_deref(), Some("feat/x"));
        assert_eq!(p.base, None);
        assert_eq!(p.model, None);
        assert_eq!(p.agent, SpawnAgent::Codex);
        assert_eq!(p.prompt.as_deref(), Some("fix it"));
    }

    #[test]
    fn the_window_never_skips_permission_prompts() {
        assert!(!params(args(), mint_op_id()).skip_permissions);
    }

    #[test]
    fn every_submit_carries_its_own_op_id() {
        let first = params(args(), mint_op_id()).mutation.op_id;
        let second = params(args(), mint_op_id()).mutation.op_id;
        assert!(first.is_some() && second.is_some());
        assert_ne!(first, second, "two submits never share an op id");
    }

    #[test]
    fn an_unknown_agent_is_refused_at_the_seam() {
        let raw = serde_json::json!({
            "repo_path": "/r", "branch": null, "base": null,
            "agent": "vim", "model": null, "prompt": null
        });
        assert!(serde_json::from_value::<CreateWorktreeArgs>(raw).is_err());
    }

    #[test]
    fn refusals_read_as_sentences() {
        let dark = DaemonError::Rpc {
            code: METHOD_NOT_FOUND,
            message: "unknown method: worktree/create".into(),
        };
        // The verbs are on by default: a daemon that does not serve one is
        // older than this app, or was started with the opt-out.
        let dark = refusal_text_in(&dark, Path::new("/srv/hangar-home/config/config.toml"));
        assert!(dark.contains("AINB_HANGAR_SPAWN=0"), "{dark}");
        assert!(dark.contains("[hangar] spawn = false"), "{dark}");
        assert!(!dark.contains("AINB_HANGAR_SPAWN=1"), "{dark}");
        // The file the daemon reads, not a fixed `~/.agents-in-a-box`.
        assert!(
            dark.contains("in /srv/hangar-home/config/config.toml "),
            "{dark}"
        );
        assert!(!dark.contains(".agents-in-a-box"), "{dark}");
        let bad = DaemonError::Rpc {
            code: INVALID_PARAMS,
            message: "base is not a valid git ref name".into(),
        };
        assert!(refusal_text(&bad).contains("base is not a valid git ref name"));
        let unregistered = DaemonError::Rpc {
            code: REPO_NOT_REGISTERED,
            message: "repo_path is not under a registered workspace folder: add its folder to \
                      workspace_defaults.workspace_scan_paths"
                .into(),
        };
        assert!(refusal_text(&unregistered).contains("use Add project"));
        // By code, not by sentence: the same words under INVALID_PARAMS are
        // just a refusal.
        let worded = DaemonError::Rpc {
            code: INVALID_PARAMS,
            message: "repo_path is not under a registered workspace folder".into(),
        };
        assert!(refusal_text(&worded).starts_with("The daemon refused the request"));
        let started = DaemonError::Rpc {
            code: SPAWN_STARTED,
            message: "`ainb run` is still running after 120s: the worktree may still be \
                      creating; check the sidebar"
                .into(),
        };
        let started = refusal_text(&started);
        assert!(started.starts_with("The create started: "), "{started}");
        assert!(!started.contains("failed"), "{started}");
        let unstarted = DaemonError::Rpc {
            code: -32603,
            message: "could not run ainb: No such file or directory".into(),
        };
        assert!(refusal_text(&unstarted).starts_with("Creating the worktree failed"));
        let slow = DaemonError::Timeout(std::time::Duration::from_secs(150));
        assert!(refusal_text(&slow).contains("may still be creating; check the sidebar"));
    }
}
