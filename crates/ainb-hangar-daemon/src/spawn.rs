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

use std::path::{Component, Path, PathBuf};
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
    // Glued `--flag=value`: a value starting with `-` stays a value.
    if let Some(model) = &params.model {
        argv.push(format!("--model={model}"));
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

/// The folders a repository may be created from: the user's configured
/// workspace scan paths and onboarding git directories, the same roots the
/// desktop's project list comes from, canonicalized. Read fresh per call (a
/// person may add one at any time); a missing or unreadable file adds none.
fn registered_roots(home: &Path) -> Vec<PathBuf> {
    let config = home.join(".agents-in-a-box").join("config");
    let read = |file: &str| {
        std::fs::read_to_string(config.join(file))
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
    };
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push_all = |value: Option<&toml::Value>| {
        for entry in value.and_then(toml::Value::as_array).into_iter().flatten() {
            if let Some(path) = entry.as_str() {
                let expanded = path
                    .strip_prefix("~/")
                    .map_or_else(|| PathBuf::from(path), |rest| home.join(rest));
                if let Ok(canonical) = std::fs::canonicalize(expanded) {
                    roots.push(canonical);
                }
            }
        }
    };
    if let Some(table) = read("config.toml") {
        push_all(
            table
                .get("workspace_defaults")
                .and_then(|defaults| defaults.get("workspace_scan_paths")),
        );
    }
    if let Some(table) = read("onboarding.toml") {
        push_all(table.get("git_directories"));
    }
    roots
}

/// `repo_path` as the canonical top of a git repository inside a registered
/// root, or why not. Refuses `.` and `..` components before resolving
/// anything, so a path cannot walk out of the root it names.
fn resolve_repo(repo_path: &str, home: &Path) -> Result<PathBuf, SpawnError> {
    let path = Path::new(repo_path);
    if path.components().any(|c| matches!(c, Component::CurDir | Component::ParentDir)) {
        return Err(SpawnError::Invalid(
            "repo_path must not contain . or .. components".into(),
        ));
    }
    let canonical = std::fs::canonicalize(path).map_err(|_| {
        SpawnError::Invalid(format!(
            "repo_path is not a directory on this host: {repo_path}"
        ))
    })?;
    let roots = registered_roots(home);
    if !roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(SpawnError::Invalid(
            "repo_path is not under a registered workspace folder: add its folder to \
             workspace_defaults.workspace_scan_paths"
                .into(),
        ));
    }
    let top = std::process::Command::new("git")
        .arg("-C")
        .arg(&canonical)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| std::fs::canonicalize(String::from_utf8_lossy(&out.stdout).trim()).ok());
    if top.as_deref() != Some(canonical.as_path()) {
        return Err(SpawnError::Invalid(
            "repo_path is not the top of a git repository".into(),
        ));
    }
    Ok(canonical)
}

/// Whether `branch` already exists in the repository at `repo`. Checked
/// before spawning: an existing branch is checked out as it is, which would
/// silently ignore `base`, and a second agent on a live branch is not a new
/// worktree.
fn branch_exists(repo: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .stdin(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Create a worktree with an agent in it by running `ainb run`.
///
/// The child is never killed: on timeout, or if the daemon stops, `ainb run`
/// runs to its own end, including its rollback, and the session (or its
/// cleanup) lands on its own. A timeout is reported as "may still be
/// creating", not as a failure that already undid anything.
///
/// # Errors
/// [`SpawnError::Invalid`] for a request refused before anything ran,
/// [`SpawnError::Failed`] when `ainb run` failed or outlived the bound.
pub async fn worktree_create(
    params: &WorktreeCreateParams,
) -> Result<WorktreeCreateResult, SpawnError> {
    params.validate().map_err(|e| SpawnError::Invalid(e.to_string()))?;
    let home = dirs::home_dir()
        .ok_or_else(|| SpawnError::Failed("the daemon has no home directory".into()))?;
    let repo = resolve_repo(&params.repo_path, &home)?;
    if let Some(branch) = &params.branch {
        if branch_exists(&repo, branch) {
            return Err(SpawnError::Invalid(format!(
                "branch '{branch}' already exists: pick a new name"
            )));
        }
    }
    let mut resolved = params.clone();
    resolved.repo_path = repo.to_string_lossy().into_owned();
    let bin = crate::atc::ainb_bin();
    // `ainb run` writes to files, never to pipes the daemon holds. With pipes,
    // anything that stopped the daemon reading them (a create that timed out,
    // a caller that went away, a daemon restart) would turn `ainb run`'s next
    // write into EPIPE, killing it before its own rollback ran and orphaning
    // the tmux session, the worktree and the branch. A file takes every write
    // whatever the daemon is doing.
    let logs = RunLogs::create()?;
    let child = tokio::process::Command::new(&bin)
        .args(run_argv(&resolved))
        .stdin(std::process::Stdio::null())
        .stdout(logs.stdout()?)
        .stderr(logs.stderr()?)
        // The daemon may itself run inside a tmux pane; an inherited $TMUX
        // would point `ainb run`'s tmux calls at that client's server
        // context instead of the default one sessions live on.
        .env_remove("TMUX")
        .spawn()
        .map_err(|e| SpawnError::Failed(format!("could not run {bin}: {e}")))?;
    // The wait runs in its own task, so it outlives both a timeout here and a
    // caller that stops waiting (a closed connection drops this future), and
    // the run's outcome is still recorded.
    let mut child = child;
    let mut wait = tokio::spawn(async move { child.wait().await });
    let timeout = run_timeout();
    let status = match tokio::time::timeout(timeout, &mut wait).await {
        Ok(Ok(Ok(status))) => status,
        Ok(Ok(Err(e))) => return Err(SpawnError::Failed(format!("waiting on {bin}: {e}"))),
        Ok(Err(e)) => return Err(SpawnError::Failed(format!("waiting on {bin}: {e}"))),
        Err(_) => {
            // Nobody is waiting on the answer any more, so the run's own
            // outcome goes to the log, where an operator can find it.
            tokio::spawn(async move {
                match wait.await {
                    Ok(Ok(status)) if status.success() => {
                        tracing::info!("`ainb run` finished after its create timed out");
                    }
                    Ok(Ok(status)) => tracing::warn!(
                        %status,
                        stderr = %stderr_tail(&logs.read_stderr()),
                        "`ainb run` failed after its create timed out"
                    ),
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "waiting on a timed-out `ainb run` failed");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "the wait on a timed-out `ainb run` ended");
                    }
                }
                logs.remove();
            });
            return Err(SpawnError::Failed(format!(
                "`ainb run` is still running after {}s: the worktree may still be creating; check the sidebar",
                timeout.as_secs()
            )));
        }
    };
    let stdout = logs.read_stdout();
    let stderr = logs.read_stderr();
    logs.remove();
    if !status.success() {
        return Err(SpawnError::Failed(format!(
            "`ainb run` failed: {}",
            stderr_tail(&stderr)
        )));
    }
    parse_run_output(&String::from_utf8_lossy(&stdout)).map_err(SpawnError::Failed)
}

/// Where one `ainb run` writes its stdout and stderr: two files under the
/// daemon's log dir (`<hangar>/hangar/logs/spawn/`), readable by the daemon's
/// user only, removed once the run's outcome is read.
struct RunLogs {
    stdout: std::path::PathBuf,
    stderr: std::path::PathBuf,
}

impl RunLogs {
    fn create() -> Result<Self, SpawnError> {
        let dir = crate::log_dir()
            .map_err(|e| SpawnError::Failed(format!("no log directory for `ainb run`: {e}")))?
            .join("spawn");
        std::fs::create_dir_all(&dir)
            .map_err(|e| SpawnError::Failed(format!("creating {}: {e}", dir.display())))?;
        let name = uuid::Uuid::new_v4().simple().to_string();
        Ok(Self {
            stdout: dir.join(format!("{name}.stdout")),
            stderr: dir.join(format!("{name}.stderr")),
        })
    }

    fn open(path: &std::path::Path) -> Result<std::process::Stdio, SpawnError> {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map(std::process::Stdio::from)
            .map_err(|e| SpawnError::Failed(format!("creating {}: {e}", path.display())))
    }

    fn stdout(&self) -> Result<std::process::Stdio, SpawnError> {
        Self::open(&self.stdout)
    }

    fn stderr(&self) -> Result<std::process::Stdio, SpawnError> {
        Self::open(&self.stderr)
    }

    fn read_stdout(&self) -> Vec<u8> {
        std::fs::read(&self.stdout).unwrap_or_default()
    }

    fn read_stderr(&self) -> Vec<u8> {
        std::fs::read(&self.stderr).unwrap_or_default()
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.stdout);
        let _ = std::fs::remove_file(&self.stderr);
    }
}

/// The last five lines of a run's stderr: the CLI's own last words.
fn stderr_tail(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    lines[lines.len().saturating_sub(5)..].join("\n")
}

/// Test seam: a shorter wait than [`RUN_TIMEOUT`], in milliseconds, or 0 for
/// the real one. A test of what happens after the timeout should not sit
/// through two minutes of it.
#[cfg(any(test, feature = "test-support"))]
static RUN_TIMEOUT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Shorten the wait on `ainb run`, or restore it with `None`. Test-only.
#[cfg(any(test, feature = "test-support"))]
pub fn set_run_timeout_for_test(timeout: Option<Duration>) {
    let ms = timeout.map_or(0, |t| u64::try_from(t.as_millis()).unwrap_or(u64::MAX));
    RUN_TIMEOUT_MS.store(ms, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(any(test, feature = "test-support"))]
fn run_timeout() -> Duration {
    match RUN_TIMEOUT_MS.load(std::sync::atomic::Ordering::SeqCst) {
        0 => RUN_TIMEOUT,
        ms => Duration::from_millis(ms),
    }
}

/// Compiled out of a shipped daemon: always the real wait.
#[cfg(not(any(test, feature = "test-support")))]
const fn run_timeout() -> Duration {
    RUN_TIMEOUT
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
            model: Some("-gpt-5".into()),
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
        ] {
            assert!(pairs.contains(&pair), "missing {pair:?} in {argv:?}");
        }
        assert!(
            argv.contains(&"--model=-gpt-5".to_string()),
            "a dash-led model stays one glued value: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a.starts_with("--name")),
            "no tmux name is ever sent"
        );
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

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "git {args:?}");
    }

    /// A home whose config registers `root`, and a repo inside it.
    fn world() -> (tempfile::TempDir, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("code");
        let repo = root.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let config = home.path().join(".agents-in-a-box/config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            format!(
                "[workspace_defaults]\nworkspace_scan_paths = [\"{}\"]\n",
                root.display()
            ),
        )
        .unwrap();
        (home, repo)
    }

    #[test]
    fn a_repo_under_a_registered_root_resolves_to_its_canonical_top() {
        let (home, repo) = world();
        let resolved = resolve_repo(&repo.display().to_string(), home.path()).expect("resolves");
        assert_eq!(resolved, std::fs::canonicalize(&repo).unwrap());
    }

    #[test]
    fn dot_components_unregistered_roots_and_subdirs_are_refused() {
        let (home, repo) = world();
        let dotted = format!("{}/../app", repo.display());
        assert!(
            matches!(resolve_repo(&dotted, home.path()), Err(SpawnError::Invalid(m)) if m.contains(".."))
        );

        let outside = tempfile::tempdir().unwrap();
        git(outside.path(), &["init", "-q"]);
        assert!(matches!(
            resolve_repo(&outside.path().display().to_string(), home.path()),
            Err(SpawnError::Invalid(m)) if m.contains("registered")
        ));

        let sub = repo.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(matches!(
            resolve_repo(&sub.display().to_string(), home.path()),
            Err(SpawnError::Invalid(m)) if m.contains("top of a git repository")
        ));
    }

    #[test]
    fn an_existing_branch_is_seen_before_any_spawn() {
        let (_home, repo) = world();
        git(&repo, &["branch", "taken"]);
        assert!(branch_exists(&repo, "taken"));
        assert!(!branch_exists(&repo, "fresh"));
    }
}
