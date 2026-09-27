//! Wire types for the spawn verbs: the daemon creates worktrees and the agent
//! sessions in them, so every surface (desktop, TUI, a paired device later)
//! creates work through one owner.
//!
//! `worktree/create` and `worktree/agent_add` are dark until their flip PR:
//! the daemon answers `METHOD_NOT_FOUND` unless `AINB_HANGAR_SPAWN` is set at
//! boot, so a daemon built from main behaves exactly as v1.29.0 does. Neither
//! method is in the mutation registry while dark; their params already carry
//! the D18 envelope so the flip only registers them.
//!
//! ```text
//!  desktop ──worktree/create──▶ daemon ──`ainb --format json run --worktree`──▶ git + tmux + agent
//!          ◀── WorktreeCreateResult ──┘
//!  desktop ──worktree/agent_add──▶ daemon ──`ainb --format json run --existing-worktree`──▶ tmux + agent
//!          ◀── WorktreeCreateResult ──┘
//! ```

use serde::{Deserialize, Serialize};

/// The agent CLI a new session runs. Mirrors `ainb run --tool`. The one
/// list of agents: the desktop's command takes it and its TypeScript is
/// generated from it, so no surface keeps a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SpawnAgent {
    /// Claude Code.
    Claude,
    /// The `OpenAI` Codex CLI.
    Codex,
    /// Gemini CLI.
    Gemini,
    /// GitHub Copilot CLI.
    Copilot,
    /// Antigravity CLI.
    Antigravity,
}

impl SpawnAgent {
    /// The `ainb run --tool` value for this agent.
    #[must_use]
    pub const fn tool_arg(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Copilot => "copilot",
            Self::Antigravity => "antigravity",
        }
    }
}

/// Longest accepted `model`, `branch` or `base` value, in bytes.
pub const SPAWN_FIELD_MAX: usize = 200;

/// Longest accepted first prompt, in bytes.
pub const SPAWN_PROMPT_MAX: usize = 64 * 1024;

/// Parameters for `worktree/create`: a new git worktree on a new branch, with
/// one agent session running in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCreateParams {
    /// Absolute path of the source repository on the daemon's host.
    pub repo_path: String,
    /// New branch name. Absent: `ainb/session-<id8>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Ref the new branch starts from. Absent: the repository's default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Agent CLI to launch in the worktree.
    pub agent: SpawnAgent,
    /// Provider model id, passed through unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// First prompt, submitted once the agent's input box is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Start the agent with its permission prompts skipped.
    #[serde(default)]
    pub skip_permissions: bool,
    /// The D18 mutation envelope, flattened so the wire object stays
    /// `{ ..fields.., op_id?, fence? }`. Unused while the method is dark.
    #[serde(flatten)]
    pub mutation: crate::mutation::MutationEnvelope,
}

/// Parameters for `worktree/agent_add`: one more agent session in a worktree
/// that already exists, on the branch it already has.
///
/// A verb of its own, not a field on [`WorktreeCreateParams`]: that struct
/// flattens the mutation envelope, so it cannot deny unknown fields, and an
/// older spawn-enabled daemon would drop an `existing_worktree` field and
/// create a new worktree instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeAgentAddParams {
    /// Absolute path of the worktree checkout on the daemon's host: the
    /// `worktree_path` a create returned.
    pub worktree_path: String,
    /// Agent CLI to launch in the worktree.
    pub agent: SpawnAgent,
    /// Provider model id, passed through unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// First prompt, submitted once the agent's input box is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Start the agent with its permission prompts skipped.
    #[serde(default)]
    pub skip_permissions: bool,
    /// The D18 mutation envelope, flattened as on [`WorktreeCreateParams`].
    /// Unused while the method is dark.
    #[serde(flatten)]
    pub mutation: crate::mutation::MutationEnvelope,
}

/// Why a spawn request was refused before anything was created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnParamsError {
    /// `repo_path` is not an absolute path.
    RepoPathNotAbsolute,
    /// `worktree_path` is not an absolute path.
    WorktreePathNotAbsolute,
    /// A text field is empty, too long, or holds a control character.
    BadField(&'static str),
    /// A ref-like field (`branch`, `base`) would be read as a flag or is not a
    /// valid git ref name.
    BadRef(&'static str),
}

impl std::fmt::Display for SpawnParamsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RepoPathNotAbsolute => f.write_str("repo_path must be an absolute path"),
            Self::WorktreePathNotAbsolute => f.write_str("worktree_path must be an absolute path"),
            Self::BadField(field) => {
                write!(f, "{field} is empty, too long, or has control characters")
            }
            Self::BadRef(field) => write!(f, "{field} is not a valid git ref name"),
        }
    }
}

impl std::error::Error for SpawnParamsError {}

impl WorktreeCreateParams {
    /// Shape checks that need no filesystem: absolute repo path, bounded
    /// fields without control characters, and ref names that cannot be
    /// mistaken for a command-line flag.
    ///
    /// # Errors
    /// The first field that fails, as a [`SpawnParamsError`].
    pub fn validate(&self) -> Result<(), SpawnParamsError> {
        if !self.repo_path.starts_with('/') {
            return Err(SpawnParamsError::RepoPathNotAbsolute);
        }
        text_ok("repo_path", &self.repo_path, 4096)?;
        if let Some(branch) = &self.branch {
            ref_ok("branch", branch)?;
        }
        if let Some(base) = &self.base {
            ref_ok("base", base)?;
        }
        agent_fields_ok(self.model.as_deref(), self.prompt.as_deref())
    }
}

impl WorktreeAgentAddParams {
    /// Shape checks that need no filesystem: an absolute worktree path, and
    /// the same `model` and `prompt` rules as [`WorktreeCreateParams::validate`].
    /// Whether the path really is a worktree the daemon may use is the
    /// daemon's own check, against the disk.
    ///
    /// # Errors
    /// The first field that fails, as a [`SpawnParamsError`].
    pub fn validate(&self) -> Result<(), SpawnParamsError> {
        if !self.worktree_path.starts_with('/') {
            return Err(SpawnParamsError::WorktreePathNotAbsolute);
        }
        text_ok("worktree_path", &self.worktree_path, 4096)?;
        agent_fields_ok(self.model.as_deref(), self.prompt.as_deref())
    }
}

/// The `model` and `prompt` rules every spawn verb shares. A prompt may be
/// long and multi-line, so it is held only to a size bound and no NUL (argv
/// cannot carry one).
fn agent_fields_ok(model: Option<&str>, prompt: Option<&str>) -> Result<(), SpawnParamsError> {
    if let Some(model) = model {
        text_ok("model", model, SPAWN_FIELD_MAX)?;
    }
    if let Some(prompt) = prompt {
        if prompt.len() > SPAWN_PROMPT_MAX || prompt.contains('\0') {
            return Err(SpawnParamsError::BadField("prompt"));
        }
    }
    Ok(())
}

fn text_ok(field: &'static str, value: &str, max: usize) -> Result<(), SpawnParamsError> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(SpawnParamsError::BadField(field));
    }
    Ok(())
}

/// A conservative subset of `git check-ref-format`, whole name and per
/// `/`-separated component.
///
/// Whole name: no leading `-` (it would reach `ainb run` and git as a flag),
/// no whitespace or control characters, none of `~^:?*[\`, no `..`, no `@{`,
/// not `@` alone, no trailing `/` or `.`. Each component: not empty (so no
/// leading `/` or `//`), no leading `.`, no `.lock` suffix.
///
/// The cases are pinned by `tests/fixtures/spawn_validation.json`, which the
/// desktop composer's validator is tested against too, so the two cannot
/// drift apart.
fn ref_ok(field: &'static str, value: &str) -> Result<(), SpawnParamsError> {
    text_ok(field, value, SPAWN_FIELD_MAX)?;
    let whole_bad = value.starts_with('-')
        || value == "@"
        || value.ends_with('/')
        || value.ends_with('.')
        || value.contains("..")
        || value.contains("@{")
        || value.chars().any(|c| c.is_whitespace() || "~^:?*[\\".contains(c));
    let part_bad = value.split('/').any(|part| {
        part.is_empty() || part.starts_with('.') || part.to_ascii_lowercase().ends_with(".lock")
    });
    if whole_bad || part_bad {
        return Err(SpawnParamsError::BadRef(field));
    }
    Ok(())
}

/// Result for `worktree/create`: the session the daemon made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCreateResult {
    /// The ainb session id (UUID string), the `sessions` table key.
    pub session_id: String,
    /// The tmux session running the agent; attach with `=<name>`.
    pub tmux_session_name: String,
    /// The worktree checkout directory.
    pub worktree_path: String,
    /// The branch the worktree is on.
    pub branch: String,
    /// The id ainb minted for a Claude launch (`claude --session-id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    /// The model the agent was launched with, when one was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> WorktreeCreateParams {
        WorktreeCreateParams {
            repo_path: "/repos/app".into(),
            branch: Some("feat/login".into()),
            base: Some("origin/main".into()),
            agent: SpawnAgent::Claude,
            model: None,
            prompt: Some("fix the login bug".into()),
            skip_permissions: false,
            mutation: crate::mutation::MutationEnvelope::default(),
        }
    }

    #[test]
    fn a_plain_request_is_valid() {
        assert_eq!(params().validate(), Ok(()));
    }

    #[test]
    fn a_relative_repo_path_is_refused() {
        let p = WorktreeCreateParams {
            repo_path: "repos/app".into(),
            ..params()
        };
        assert_eq!(p.validate(), Err(SpawnParamsError::RepoPathNotAbsolute));
    }

    /// A branch or base starting with `-` would reach `ainb run` and git as a
    /// flag. Refused at the edge, before any process is spawned.
    #[test]
    fn refs_that_read_as_flags_or_bad_refs_are_refused() {
        for bad in [
            "-rf", "--help", "a..b", "a b", "a~1", "x@{1}", "/a", "a/", "a.lock", ".a",
        ] {
            let p = WorktreeCreateParams {
                branch: Some(bad.into()),
                ..params()
            };
            assert_eq!(
                p.validate(),
                Err(SpawnParamsError::BadRef("branch")),
                "{bad}"
            );
            let p = WorktreeCreateParams {
                base: Some(bad.into()),
                ..params()
            };
            assert_eq!(p.validate(), Err(SpawnParamsError::BadRef("base")), "{bad}");
        }
    }

    #[test]
    fn control_characters_and_oversize_fields_are_refused() {
        let p = WorktreeCreateParams {
            model: Some("m".repeat(SPAWN_FIELD_MAX + 1)),
            ..params()
        };
        assert_eq!(p.validate(), Err(SpawnParamsError::BadField("model")));
        let p = WorktreeCreateParams {
            prompt: Some("x".repeat(SPAWN_PROMPT_MAX + 1)),
            ..params()
        };
        assert_eq!(p.validate(), Err(SpawnParamsError::BadField("prompt")));
    }

    /// Absent optionals stay absent on the wire, and the envelope flattens,
    /// so an older client's frame decodes unchanged.
    #[test]
    fn the_wire_shape_is_flat_and_sparse() {
        let minimal = serde_json::json!({"repo_path": "/r", "agent": "codex"});
        let p: WorktreeCreateParams = serde_json::from_value(minimal).expect("decode minimal");
        assert_eq!(p.agent, SpawnAgent::Codex);
        assert!(!p.skip_permissions);
        let back = serde_json::to_value(&p).expect("encode");
        assert_eq!(
            back,
            serde_json::json!({"repo_path": "/r", "agent": "codex", "skip_permissions": false})
        );
        let with_op: WorktreeCreateParams = serde_json::from_value(
            serde_json::json!({"repo_path": "/r", "agent": "claude", "op_id": "op-1"}),
        )
        .expect("decode with op id");
        assert!(with_op.mutation.op_id.is_some());
    }

    fn agent_add() -> WorktreeAgentAddParams {
        WorktreeAgentAddParams {
            worktree_path: "/home/u/.agents-in-a-box/worktrees/by-name/app--feat--1a2b3c4d".into(),
            agent: SpawnAgent::Codex,
            model: None,
            prompt: Some("-y review it".into()),
            skip_permissions: false,
            mutation: crate::mutation::MutationEnvelope::default(),
        }
    }

    #[test]
    fn an_agent_add_needs_an_absolute_worktree_path_and_sane_fields() {
        assert_eq!(agent_add().validate(), Ok(()));
        let relative = WorktreeAgentAddParams {
            worktree_path: "by-name/app".into(),
            ..agent_add()
        };
        assert_eq!(
            relative.validate(),
            Err(SpawnParamsError::WorktreePathNotAbsolute)
        );
        let control = WorktreeAgentAddParams {
            worktree_path: "/w/app\n".into(),
            ..agent_add()
        };
        assert_eq!(
            control.validate(),
            Err(SpawnParamsError::BadField("worktree_path"))
        );
        let nul = WorktreeAgentAddParams {
            prompt: Some("a\0b".into()),
            ..agent_add()
        };
        assert_eq!(nul.validate(), Err(SpawnParamsError::BadField("prompt")));
    }

    /// Same sparse, flat wire shape as `worktree/create`.
    #[test]
    fn the_agent_add_wire_shape_is_flat_and_sparse() {
        let minimal =
            serde_json::json!({"worktree_path": "/w", "agent": "claude", "op_id": "op-1"});
        let p: WorktreeAgentAddParams = serde_json::from_value(minimal).expect("decode minimal");
        assert!(p.mutation.op_id.is_some());
        assert_eq!(
            serde_json::to_value(WorktreeAgentAddParams {
                mutation: crate::mutation::MutationEnvelope::default(),
                ..p
            })
            .expect("encode"),
            serde_json::json!({"worktree_path": "/w", "agent": "claude", "skip_permissions": false})
        );
    }

    #[test]
    fn tool_args_match_ainb_run() {
        assert_eq!(SpawnAgent::Claude.tool_arg(), "claude");
        assert_eq!(SpawnAgent::Antigravity.tool_arg(), "antigravity");
    }
}
