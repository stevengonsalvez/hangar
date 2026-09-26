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

use ainb_hangar_client::{DaemonClient, DaemonError};
use ainb_hangar_proto::mutation::{MutationEnvelope, OpId};
use ainb_hangar_proto::spawn::{SpawnAgent, WorktreeCreateParams, WorktreeCreateResult};
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
    /// `claude`, `codex`, `gemini`, `copilot` or `antigravity`.
    pub agent: String,
    /// Provider model id, passed through unchanged.
    pub model: Option<String>,
    /// First prompt for the agent.
    pub prompt: Option<String>,
    /// Tmux session name; the daemon picks one when absent.
    pub name: Option<String>,
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

fn agent(name: &str) -> Result<SpawnAgent, String> {
    match name {
        "claude" => Ok(SpawnAgent::Claude),
        "codex" => Ok(SpawnAgent::Codex),
        "gemini" => Ok(SpawnAgent::Gemini),
        "copilot" => Ok(SpawnAgent::Copilot),
        "antigravity" => Ok(SpawnAgent::Antigravity),
        other => Err(format!("unknown agent: {other}")),
    }
}

/// Blank optional text is "not given", so an empty field in the composer
/// never reaches the daemon as an empty branch or model.
fn given(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// The daemon params for `args`. Permission prompts are never skipped from
/// the window: that stays a deliberate CLI flag.
///
/// # Errors
/// An unknown agent name, as text for the composer.
pub fn params(
    args: CreateWorktreeArgs,
    op_id: Option<OpId>,
) -> Result<WorktreeCreateParams, String> {
    Ok(WorktreeCreateParams {
        repo_path: args.repo_path.trim().to_string(),
        branch: given(args.branch),
        base: given(args.base),
        agent: agent(args.agent.trim())?,
        model: given(args.model),
        prompt: args.prompt.filter(|p| !p.trim().is_empty()),
        skip_permissions: false,
        name: given(args.name),
        mutation: MutationEnvelope { op_id, fence: None },
    })
}

/// A daemon error as the sentence the composer shows.
#[must_use]
pub fn refusal_text(error: &DaemonError) -> String {
    match error {
        DaemonError::Rpc { code, .. } if *code == METHOD_NOT_FOUND => {
            "This daemon does not create worktrees yet: start it with AINB_HANGAR_SPAWN=1.".into()
        }
        DaemonError::Rpc { code, message } if *code == INVALID_PARAMS => {
            format!("The daemon refused the request: {message}")
        }
        DaemonError::Rpc { message, .. } => format!("Creating the worktree failed: {message}"),
        DaemonError::Timeout(_) => {
            "Creating the worktree took too long; check the daemon log.".into()
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
    let params = params(args, None)?;
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
            agent: "codex".into(),
            model: Some("   ".into()),
            prompt: Some("fix it".into()),
            name: None,
        }
    }

    #[test]
    fn blank_fields_are_not_given_and_values_are_trimmed() {
        let p = params(args(), None).expect("valid");
        assert_eq!(p.repo_path, "/repos/app");
        assert_eq!(p.branch.as_deref(), Some("feat/x"));
        assert_eq!(p.base, None);
        assert_eq!(p.model, None);
        assert_eq!(p.agent, SpawnAgent::Codex);
        assert_eq!(p.prompt.as_deref(), Some("fix it"));
    }

    #[test]
    fn the_window_never_skips_permission_prompts() {
        assert!(!params(args(), None).expect("valid").skip_permissions);
    }

    #[test]
    fn an_unknown_agent_is_refused_before_the_daemon() {
        let bad = CreateWorktreeArgs {
            agent: "vim".into(),
            ..args()
        };
        assert_eq!(params(bad, None).unwrap_err(), "unknown agent: vim");
    }

    #[test]
    fn refusals_read_as_sentences() {
        let dark = DaemonError::Rpc {
            code: METHOD_NOT_FOUND,
            message: "unknown method: worktree/create".into(),
        };
        assert!(refusal_text(&dark).contains("AINB_HANGAR_SPAWN=1"));
        let bad = DaemonError::Rpc {
            code: INVALID_PARAMS,
            message: "base is not a valid git ref name".into(),
        };
        assert!(refusal_text(&bad).contains("base is not a valid git ref name"));
        let failed = DaemonError::Rpc {
            code: -32603,
            message: "`ainb run` failed: branch exists".into(),
        };
        assert!(refusal_text(&failed).starts_with("Creating the worktree failed"));
    }
}
