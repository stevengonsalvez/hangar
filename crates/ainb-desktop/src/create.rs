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

/// Which daemon request a sentence or an op id is for: the window's spawn
/// verbs (a new worktree, a new agent, a new terminal) and a terminal's close,
/// which share one reading of the daemon's codes, so each code has one
/// sentence here and nowhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnVerb {
    /// `worktree/create`: a new worktree with an agent in it.
    CreateWorktree,
    /// `worktree/agent_add`: one more agent in a worktree that exists.
    AddAgent,
    /// `shell/create`: a plain terminal in a worktree.
    OpenTerminal,
    /// `shell/close`: end a terminal the daemon opened.
    CloseTerminal,
}

impl SpawnVerb {
    /// What was being done, to lead a failure: "Opening the terminal failed".
    const fn doing(self) -> &'static str {
        match self {
            Self::CreateWorktree => "Creating the worktree",
            Self::AddAgent => "Adding the agent",
            Self::OpenTerminal => "Opening the terminal",
            Self::CloseTerminal => "Closing the terminal",
        }
    }

    /// What a daemon without the verb cannot do.
    const fn missing(self) -> &'static str {
        match self {
            Self::CreateWorktree => "create worktrees",
            Self::AddAgent => "add agents to a worktree",
            Self::OpenTerminal | Self::CloseTerminal => "run shells",
        }
    }

    /// The repository an unregistered refusal is about: a create names its
    /// source, the others the worktree they act in.
    const fn repository(self) -> &'static str {
        match self {
            Self::CreateWorktree => "this repository is",
            _ => "this worktree's repository is",
        }
    }

    /// What a request that may already have happened says, with the
    /// daemon's `detail`: a second press could start a second one. `None`
    /// for a close, which starts nothing.
    fn may_have_started(self, detail: &str) -> Option<String> {
        match self {
            Self::CreateWorktree => Some(format!(
                "The create started but did not finish: {detail}. Check the sidebar before creating again."
            )),
            Self::AddAgent => Some(format!(
                "The agent was started but has not reported back yet: {detail}. Check the sidebar before adding another."
            )),
            Self::OpenTerminal => Some(format!("{MAY_STILL_OPEN} ({detail})")),
            Self::CloseTerminal => None,
        }
    }

    /// What a request the daemon did not answer in time says. `None` for a
    /// close: nothing can have started, so it simply failed.
    fn unanswered(self) -> Option<String> {
        match self {
            Self::CreateWorktree => Some(
                "The daemon did not answer in time: the worktree may still be creating; check the sidebar."
                    .into(),
            ),
            Self::AddAgent => Some(
                "The daemon did not answer in time: the agent may still be starting; check the sidebar."
                    .into(),
            ),
            Self::OpenTerminal => Some(format!("{MAY_STILL_OPEN} (the daemon did not answer in time)")),
            Self::CloseTerminal => None,
        }
    }
}

/// A fresh op id for one request. Every request mints its own: nothing in the
/// window retries a press, so two presses (Create twice, an agent picked
/// twice, New terminal twice) are two ops the daemon keeps apart, never one
/// it would fold.
#[must_use]
pub fn mint_op_id(verb: SpawnVerb) -> OpId {
    let prefix = match verb {
        SpawnVerb::CreateWorktree => "desktop-create",
        SpawnVerb::AddAgent => "desktop-agent-add",
        SpawnVerb::OpenTerminal | SpawnVerb::CloseTerminal => "desktop-shell",
    };
    OpId::parse(format!("{prefix}-{}", uuid::Uuid::new_v4().simple()))
        .expect("a uuid op id is always well formed")
}

/// What an open that may have made its shell says: the shell is not known to
/// be gone, so another press could make a second one.
pub const MAY_STILL_OPEN: &str = "The terminal may still open; check before opening another.";

/// A daemon error as the sentence the window shows for `verb`: the one place
/// a code becomes words. Chosen by the error's code, never its words; the
/// daemon's detail is shown where it is the answer (a refused field, a
/// failure, a run that started, an unregistered repository).
///
/// The spawn opt-out is named in the file the daemon reads it from, by the
/// daemon's own rule: `$AINB_HANGAR_HOME` when set, else `~/.agents-in-a-box`.
#[must_use]
pub fn spawn_refusal_text(error: &DaemonError, verb: SpawnVerb) -> String {
    let spawn_config = ainb_hangar_daemon::hangar_dir().map_or_else(
        |_| PathBuf::from("$AINB_HANGAR_HOME/config/config.toml"),
        |home| ainb_hangar_daemon::spawn::config_path_in(&home),
    );
    refusal_sentence(error, verb, &spawn_config)
}

/// [`spawn_refusal_text`] with the daemon's config file given, so the
/// sentence is testable without touching the environment.
#[must_use]
pub fn refusal_sentence(error: &DaemonError, verb: SpawnVerb, spawn_config: &Path) -> String {
    let doing = verb.doing();
    match error {
        DaemonError::Rpc { code, .. } if *code == METHOD_NOT_FOUND => format!(
            "{doing} failed: this daemon does not {}. It is older than this app, or was started with AINB_HANGAR_SPAWN=0 or with [hangar] spawn = false in {} (or with that file unreadable). Update it, or restart it without that setting.",
            verb.missing(),
            spawn_config.display()
        ),
        DaemonError::Rpc { code, message } if *code == REPO_NOT_REGISTERED => format!(
            "{doing} failed: {} not in a registered project folder. Use Add project to pick its folder, then try again. ({message})",
            verb.repository()
        ),
        DaemonError::Rpc { code, message } if *code == INVALID_PARAMS => {
            format!("{doing} failed: the daemon refused it: {message}")
        }
        // `ainb run` or tmux ran: it is still going, or it failed after
        // starting. Either way this is not a request that never happened.
        DaemonError::Rpc { code, message } if *code == SPAWN_STARTED => verb
            .may_have_started(message)
            .unwrap_or_else(|| format!("{doing} failed: {message}")),
        DaemonError::Rpc { message, .. } => format!("{doing} failed: {message}"),
        DaemonError::Timeout(_) => verb
            .unanswered()
            .unwrap_or_else(|| format!("{doing} failed: the daemon did not answer in time.")),
        other => format!("{doing} failed: the daemon is not reachable: {other}"),
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
    let params = params(args, mint_op_id(SpawnVerb::CreateWorktree));
    client
        .worktree_create(&params)
        .await
        .map(CreatedWorktree::from)
        .map_err(|e| spawn_refusal_text(&e, SpawnVerb::CreateWorktree))
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
        let p = params(args(), mint_op_id(SpawnVerb::CreateWorktree));
        assert_eq!(p.repo_path, "/repos/app");
        assert_eq!(p.branch.as_deref(), Some("feat/x"));
        assert_eq!(p.base, None);
        assert_eq!(p.model, None);
        assert_eq!(p.agent, SpawnAgent::Codex);
        assert_eq!(p.prompt.as_deref(), Some("fix it"));
    }

    #[test]
    fn the_window_never_skips_permission_prompts() {
        assert!(!params(args(), mint_op_id(SpawnVerb::CreateWorktree)).skip_permissions);
    }

    #[test]
    fn every_submit_carries_its_own_op_id() {
        let first = params(args(), mint_op_id(SpawnVerb::CreateWorktree)).mutation.op_id;
        let second = params(args(), mint_op_id(SpawnVerb::CreateWorktree)).mutation.op_id;
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
        use SpawnVerb::{AddAgent, CloseTerminal, CreateWorktree, OpenTerminal};
        let text = |code: i32, message: &str, verb| {
            refusal_sentence(
                &DaemonError::Rpc {
                    code,
                    message: message.into(),
                },
                verb,
                Path::new("/srv/hangar-home/config/config.toml"),
            )
        };
        for verb in [CreateWorktree, AddAgent, OpenTerminal, CloseTerminal] {
            // The verbs are on by default: a daemon that does not serve one
            // is older than this app, or was started with an opt-out.
            let dark = text(METHOD_NOT_FOUND, "unknown method", verb);
            assert!(dark.contains("AINB_HANGAR_SPAWN=0"), "{dark}");
            assert!(dark.contains("[hangar] spawn = false"), "{dark}");
            // The file the daemon reads, not a fixed `~/.agents-in-a-box`.
            assert!(
                dark.contains("in /srv/hangar-home/config/config.toml "),
                "{dark}"
            );
            assert!(!dark.contains(".agents-in-a-box"), "{dark}");
            assert!(!dark.contains("AINB_HANGAR_SPAWN=1"), "{dark}");
            let unregistered = text(REPO_NOT_REGISTERED, "not under a registered folder", verb);
            assert!(unregistered.contains("Add project"), "{unregistered}");
            assert!(
                unregistered.contains("(not under a registered folder)"),
                "{unregistered}"
            );
            // By code, not by sentence: the same words under INVALID_PARAMS
            // are just a refusal, with the daemon's detail.
            let worded = text(INVALID_PARAMS, "not in a registered project folder", verb);
            assert!(!worded.contains("Add project"), "{worded}");
            assert!(
                worded.ends_with("the daemon refused it: not in a registered project folder"),
                "{worded}"
            );
            assert!(
                text(-32603, "tmux could not start", verb)
                    .ends_with("failed: tmux could not start")
            );
        }
        // Started, not failed: a second press could start a second one.
        for verb in [CreateWorktree, AddAgent, OpenTerminal] {
            let started = text(
                SPAWN_STARTED,
                "`ainb run` is still running after 120s",
                verb,
            );
            assert!(
                started.contains("`ainb run` is still running after 120s"),
                "{started}"
            );
            assert!(!started.contains("failed"), "{started}");
            let slow = spawn_refusal_text(
                &DaemonError::Timeout(std::time::Duration::from_secs(150)),
                verb,
            );
            assert!(!slow.contains("failed"), "{slow}");
        }
        assert!(
            text(SPAWN_STARTED, "m", CreateWorktree)
                .starts_with("The create started but did not finish: m")
        );
        assert!(
            text(SPAWN_STARTED, "m", AddAgent)
                .starts_with("The agent was started but has not reported back yet: m")
        );
        assert!(text(SPAWN_STARTED, "m", OpenTerminal).starts_with(MAY_STILL_OPEN));
        assert!(
            text(SPAWN_STARTED, "m", CreateWorktree)
                .contains("Check the sidebar before creating again")
        );
        assert!(
            text(SPAWN_STARTED, "m", AddAgent).contains("Check the sidebar before adding another")
        );
        // A close starts nothing: a late answer or a started code is a failure.
        let slow = DaemonError::Timeout(std::time::Duration::from_secs(30));
        assert!(
            spawn_refusal_text(&slow, CloseTerminal).starts_with("Closing the terminal failed")
        );
        assert!(text(SPAWN_STARTED, "m", CloseTerminal).starts_with("Closing the terminal failed"));
        assert!(
            spawn_refusal_text(&slow, CreateWorktree)
                .contains("may still be creating; check the sidebar")
        );
        assert!(
            text(INVALID_PARAMS, "bad base", CreateWorktree)
                .starts_with("Creating the worktree failed")
        );
        assert!(
            text(INVALID_PARAMS, "not a tree", AddAgent).starts_with("Adding the agent failed")
        );
    }

    #[test]
    fn op_ids_are_fresh_and_name_their_verb() {
        let create = mint_op_id(SpawnVerb::CreateWorktree);
        let add = mint_op_id(SpawnVerb::AddAgent);
        assert!(create.as_str().starts_with("desktop-create-"), "{create:?}");
        assert!(add.as_str().starts_with("desktop-agent-add-"), "{add:?}");
        let shell = mint_op_id(SpawnVerb::OpenTerminal);
        assert!(shell.as_str().starts_with("desktop-shell-"), "{shell:?}");
        assert_ne!(add, mint_op_id(SpawnVerb::AddAgent));
    }
}
