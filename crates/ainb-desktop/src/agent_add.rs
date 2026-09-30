//! Add one more agent to a worktree that already exists, from the window.
//!
//! The page names the worktree by ids the host holds
//! ([`WorktreeTarget`]: a listed session, or a shell tab the host opened);
//! the host resolves the folder itself and asks the daemon
//! (`worktree/agent_add`). A path never comes from the page, so a page cannot
//! point an agent at a folder the host did not list.
//!
//! ```text
//!  "+" menu ──worktree_agent_add{target, agent}──▶ host
//!      host: target ──own session list / shell tab──▶ worktree path
//!      host ──worktree/agent_add──▶ daemon ──▶ CreatedWorktree ──▶ new tab
//! ```

use ainb_hangar_client::DaemonClient;
use ainb_hangar_proto::mutation::{MutationEnvelope, OpId};
use ainb_hangar_proto::spawn::{SpawnAgent, WorktreeAgentAddParams};
use serde::Deserialize;

use crate::create::{CreatedWorktree, SpawnVerb, mint_op_id, spawn_refusal_text};
use crate::worktree_target::WorktreeTarget;

/// What the page sends: which worktree, by ids, and which agent. Unknown
/// fields are refused, so a `worktree_path` slipped in beside these fails at
/// the seam instead of being quietly ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct AddAgentArgs {
    /// The worktree the new agent joins.
    pub target: WorktreeTarget,
    /// The agent CLI, from the daemon's own list.
    pub agent: SpawnAgent,
}

/// The daemon params for one press. Permission prompts are never skipped from
/// the window, as for a create.
#[must_use]
pub fn params(worktree_path: String, agent: SpawnAgent, op_id: OpId) -> WorktreeAgentAddParams {
    WorktreeAgentAddParams {
        worktree_path,
        agent,
        model: None,
        prompt: None,
        skip_permissions: false,
        mutation: MutationEnvelope {
            op_id: Some(op_id),
            fence: None,
        },
    }
}

/// Ask the daemon for one more agent in `worktree_path`, which the caller
/// resolved from its own state ([`WorktreeTarget::resolve`]). Each call is a
/// fresh op id: two presses are two agents.
///
/// # Errors
/// The sentence [`spawn_refusal_text`] makes of the daemon's answer.
pub async fn request(
    client: &DaemonClient,
    worktree_path: String,
    agent: SpawnAgent,
) -> Result<CreatedWorktree, String> {
    client
        .worktree_agent_add(&params(
            worktree_path,
            agent,
            mint_op_id(SpawnVerb::AddAgent),
        ))
        .await
        .map(CreatedWorktree::from)
        .map_err(|e| spawn_refusal_text(&e, SpawnVerb::AddAgent))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "11111111-1111-4111-8111-111111111111";

    #[test]
    fn the_page_sends_a_target_and_an_agent_never_a_path() {
        let raw = serde_json::json!({
            "target": { "kind": "session", "id": ID }, "agent": "claude", "worktree_path": "/etc"
        });
        assert!(serde_json::from_value::<AddAgentArgs>(raw).is_err());
        let raw = serde_json::json!({ "target": { "kind": "session", "id": ID }, "agent": "vim" });
        assert!(serde_json::from_value::<AddAgentArgs>(raw).is_err());
        let raw = serde_json::json!({ "target": { "kind": "shell", "key": "ainb-dsh-1" }, "agent": "codex" });
        let args: AddAgentArgs = serde_json::from_value(raw).expect("ids only");
        assert_eq!(args.agent, SpawnAgent::Codex);
        assert_eq!(
            args.target,
            WorktreeTarget::Shell {
                key: "ainb-dsh-1".into()
            }
        );
    }

    #[test]
    fn params_carry_the_resolved_path_and_a_fresh_op_id_per_press() {
        let path = "/wt/app-fix-login".to_string();
        let first = params(
            path.clone(),
            SpawnAgent::Codex,
            mint_op_id(SpawnVerb::AddAgent),
        );
        let second = params(path, SpawnAgent::Codex, mint_op_id(SpawnVerb::AddAgent));
        assert_eq!(first.worktree_path, "/wt/app-fix-login");
        assert_eq!(first.agent, SpawnAgent::Codex);
        assert_eq!((first.model.as_ref(), first.prompt.as_ref()), (None, None));
        assert!(!first.skip_permissions, "the window never skips prompts");
        assert!(first.mutation.op_id.is_some());
        assert_ne!(first.mutation.op_id, second.mutation.op_id);
        assert_eq!(first.validate(), Ok(()));
    }
}
