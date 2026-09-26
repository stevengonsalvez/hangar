//! `worktree/create`: the daemon as the one owner of new work.
//!
//! A surface (the desktop first, a paired device later) asks the daemon for a
//! new worktree with an agent in it. The daemon runs the CLI's own create path,
//! `ainb --format json run --worktree`, rather than a second copy of it: that path
//! already cuts the branch, adds the worktree, trusts it for Claude, starts
//! tmux, launches the agent, submits the first prompt and writes the session
//! row, with rollback on failure. The daemon cannot link that code (`ainb-app`
//! depends on this crate), and a third hand-written mirror is how the TUI, the
//! CLI and board tasks drifted apart before.
//!
//! ```text
//!  surface ──worktree/create──▶ validate ──▶ `ainb --format json run …` ──▶ one JSON line
//!          ◀──── WorktreeCreateResult ◀──── parse ◀──────────────────┘
//! ```
//!
//! Dark by default: served only when [`SPAWN_ENV`] is `1` at boot. Off, the
//! method answers `METHOD_NOT_FOUND`, which a client cannot tell from an older
//! daemon. An environment variable, never a `daemon_config` key, so no
//! connected surface can switch it on.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use ainb_hangar_proto::spawn::{WorktreeCreateParams, WorktreeCreateResult};

/// The boot-time switch: `AINB_HANGAR_SPAWN=1` serves `worktree/create`.
pub const SPAWN_ENV: &str = "AINB_HANGAR_SPAWN";

/// Upper bound on one `ainb run`. It waits up to 30s for the agent's input
/// box before sending the prompt, and a first-run Codex thread claim can add
/// more; past this the create is reported failed rather than hanging a caller.
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Whether `worktree/create` is served, read once so a later `set_var` cannot
/// flip a running daemon.
#[must_use]
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var(SPAWN_ENV).is_ok_and(|v| v == "1"))
}

/// Why a create failed, split so the RPC layer can tell a bad request from a
/// host fault.
#[derive(Debug)]
pub enum SpawnError {
    /// The request was refused before anything ran.
    Invalid(String),
    /// `ainb run` ran and failed, or could not be run at all.
    Failed(String),
}

/// The `ainb run` argument vector for `params`. Pure, so the mapping is
/// testable without spawning anything.
///
/// Every value is its own argv element (no shell), and `validate` has already
/// refused any ref that starts with `-`.
#[must_use]
pub fn run_argv(params: &WorktreeCreateParams) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "--format".into(),
        "json".into(),
        "run".into(),
        "--worktree".into(),
        "--repo".into(),
        params.repo_path.clone(),
        "--tool".into(),
        params.agent.tool_arg().into(),
    ];
    if let Some(branch) = &params.branch {
        argv.extend(["--create-branch".into(), branch.clone()]);
    }
    if let Some(base) = &params.base {
        argv.extend(["--base".into(), base.clone()]);
    }
    if let Some(model) = &params.model {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(name) = &params.name {
        argv.extend(["--name".into(), name.clone()]);
    }
    if params.skip_permissions {
        argv.push("--dangerously-skip-permissions".into());
    }
    // Glued `--prompt=` form: a prompt starting with `-` must stay a value,
    // not be parsed by clap as the next flag.
    if let Some(prompt) = &params.prompt {
        argv.push(format!("--prompt={prompt}"));
    }
    argv
}

/// Parse `ainb --format json run`'s stdout: exactly one non-empty line holding the
/// created session. Anything else is a failure, never a guess.
///
/// # Errors
/// A message naming what was wrong with the output.
pub fn parse_run_output(stdout: &str) -> Result<WorktreeCreateResult, String> {
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    let [line] = lines.as_slice() else {
        return Err(format!(
            "`ainb run` printed {} lines, expected exactly one",
            lines.len()
        ));
    };
    serde_json::from_str(line)
        .map_err(|e| format!("`ainb run` output is not the created session: {e}"))
}

/// Create a worktree with an agent in it by running `ainb run`.
///
/// # Errors
/// [`SpawnError::Invalid`] for a request refused before anything ran,
/// [`SpawnError::Failed`] when `ainb run` failed or timed out.
pub async fn worktree_create(
    params: &WorktreeCreateParams,
) -> Result<WorktreeCreateResult, SpawnError> {
    params.validate().map_err(|e| SpawnError::Invalid(e.to_string()))?;
    if !Path::new(&params.repo_path).is_dir() {
        return Err(SpawnError::Invalid(format!(
            "repo_path is not a directory on this host: {}",
            params.repo_path
        )));
    }
    let bin = crate::atc::ainb_bin();
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(run_argv(params))
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        // The daemon may itself run inside a tmux pane; an inherited $TMUX
        // would point `ainb run`'s tmux calls at that client's server
        // context instead of the default one sessions live on.
        .env_remove("TMUX");
    let output = match tokio::time::timeout(RUN_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(SpawnError::Failed(format!("could not run {bin}: {e}"))),
        Err(_) => {
            return Err(SpawnError::Failed(format!(
                "`ainb run` did not finish within {}s",
                RUN_TIMEOUT.as_secs()
            )));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        let tail = lines[lines.len().saturating_sub(5)..].join("\n");
        return Err(SpawnError::Failed(format!("`ainb run` failed: {tail}")));
    }
    parse_run_output(&String::from_utf8_lossy(&output.stdout)).map_err(SpawnError::Failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::spawn::SpawnAgent;

    fn params() -> WorktreeCreateParams {
        WorktreeCreateParams {
            repo_path: "/repos/app".into(),
            branch: None,
            base: None,
            agent: SpawnAgent::Claude,
            model: None,
            prompt: None,
            skip_permissions: false,
            name: None,
            mutation: ainb_hangar_proto::mutation::MutationEnvelope::default(),
        }
    }

    #[test]
    fn a_minimal_request_asks_for_a_worktree_and_json() {
        assert_eq!(
            run_argv(&params()),
            [
                "--format",
                "json",
                "run",
                "--worktree",
                "--repo",
                "/repos/app",
                "--tool",
                "claude"
            ]
        );
    }

    #[test]
    fn every_field_maps_to_its_flag_and_the_prompt_stays_one_glued_value() {
        let p = WorktreeCreateParams {
            branch: Some("feat/x".into()),
            base: Some("origin/main".into()),
            agent: SpawnAgent::Codex,
            model: Some("gpt-5".into()),
            name: Some("x".into()),
            skip_permissions: true,
            prompt: Some("-y do it".into()),
            ..params()
        };
        let argv = run_argv(&p);
        let pairs: Vec<(&str, &str)> =
            argv.windows(2).map(|w| (w[0].as_str(), w[1].as_str())).collect();
        for pair in [
            ("--tool", "codex"),
            ("--create-branch", "feat/x"),
            ("--base", "origin/main"),
            ("--model", "gpt-5"),
            ("--name", "x"),
        ] {
            assert!(pairs.contains(&pair), "missing {pair:?} in {argv:?}");
        }
        assert!(argv.contains(&"--dangerously-skip-permissions".to_string()));
        assert_eq!(argv.last().map(String::as_str), Some("--prompt=-y do it"));
    }

    #[test]
    fn exactly_one_json_line_parses() {
        let out = "\n{\"session_id\":\"s\",\"tmux_session_name\":\"t\",\"worktree_path\":\"/w\",\"branch\":\"b\",\"model\":\"m\"}\n";
        let r = parse_run_output(out).expect("one line parses");
        assert_eq!(r.tmux_session_name, "t");
        assert_eq!(r.claude_session_id, None);
    }

    #[test]
    fn zero_or_two_lines_or_garbage_is_a_failure() {
        assert!(parse_run_output("").is_err());
        assert!(parse_run_output("{}\n{}\n").is_err());
        assert!(parse_run_output("Session created successfully!\n").is_err());
    }

    #[tokio::test]
    async fn a_missing_repo_dir_is_invalid_and_runs_nothing() {
        let p = WorktreeCreateParams {
            repo_path: "/definitely/not/here/ainb-spawn".into(),
            ..params()
        };
        match worktree_create(&p).await {
            Err(SpawnError::Invalid(msg)) => assert!(msg.contains("not a directory"), "{msg}"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_flag_shaped_branch_is_invalid_before_any_spawn() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = WorktreeCreateParams {
            repo_path: dir.path().display().to_string(),
            branch: Some("--help".into()),
            ..params()
        };
        assert!(matches!(
            worktree_create(&p).await,
            Err(SpawnError::Invalid(_))
        ));
    }
}
