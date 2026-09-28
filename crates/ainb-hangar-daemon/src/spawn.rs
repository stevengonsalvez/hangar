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
//! `worktree/agent_add` is the same run with `--existing-worktree`: one more
//! agent in a worktree a create already made, on the branch it already has.
//!
//! `shell/create` needs no CLI hop: a plain shell is one `tmux new-session`,
//! which the daemon runs itself (there is no `ainb shell` to call).
//!
//! Dark by default: served only when [`SPAWN_ENV`] is `1` at boot. Off, the
//! methods answer `METHOD_NOT_FOUND`, which a client cannot tell from an older
//! daemon. An environment variable, never a `daemon_config` key, so no
//! connected surface can switch it on.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use ainb_hangar_proto::spawn::{
    ShellCreateParams, ShellCreateResult, WorktreeAgentAddParams, WorktreeCreateParams,
    WorktreeCreateResult,
};

/// The boot-time switch: `AINB_HANGAR_SPAWN=1` serves the spawn verbs.
pub const SPAWN_ENV: &str = "AINB_HANGAR_SPAWN";

/// Upper bound on one `ainb run`. It waits up to 30s for the agent's input
/// box before sending the prompt, and a first-run Codex thread claim can add
/// more; past this the create is reported failed rather than hanging a caller.
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Whether the spawn verbs are served, read once so a later `set_var` cannot
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

/// The `ainb run` argument vector for `params`, whose `worktree_path` the
/// daemon has already resolved to a canonical linked worktree. Pure, like
/// [`run_argv`], and glued the same way.
#[must_use]
pub fn agent_add_argv(params: &WorktreeAgentAddParams) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "--format".into(),
        "json".into(),
        "run".into(),
        "--existing-worktree".into(),
        params.worktree_path.clone(),
        "--tool".into(),
        params.agent.tool_arg().into(),
    ];
    if let Some(model) = &params.model {
        argv.push(format!("--model={model}"));
    }
    if params.skip_permissions {
        argv.push("--dangerously-skip-permissions".into());
    }
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
    if !no_dot_components(path) {
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

/// The directory every managed worktree lives in:
/// `<home>/.agents-in-a-box/worktrees/by-name`, with `$AINB_HOME` standing in
/// for the home as it does for the CLI's own worktree manager, so the daemon
/// and the `ainb run` it spawns agree on it. `ainb-core`'s
/// `managed_worktrees_layout` test pins it to the manager's own path.
#[must_use]
pub fn managed_worktrees(home: &Path) -> PathBuf {
    std::env::var_os("AINB_HOME")
        .map_or_else(|| home.to_path_buf(), PathBuf::from)
        .join(".agents-in-a-box")
        .join("worktrees")
        .join("by-name")
}

/// Whether `path` has no `.` or `..` component. Checked before a path is
/// resolved, so it cannot walk out of the directory it names.
#[must_use]
pub fn no_dot_components(path: &Path) -> bool {
    !path.components().any(|c| matches!(c, Component::CurDir | Component::ParentDir))
}

/// Whether `canonical`, already resolved, is a folder directly in `managed`
/// ([`managed_worktrees`]) once `managed` is resolved too: the only place
/// `ainb run --worktree` puts a tree. A nested folder, or a link out to a tree
/// elsewhere, is not one.
#[must_use]
pub fn is_managed_tree(canonical: &Path, managed: &Path) -> bool {
    std::fs::canonicalize(managed).is_ok_and(|dir| canonical.parent() == Some(dir.as_path()))
}

/// The source repository of the linked worktree whose top is `canonical`, or
/// why `canonical` is not one: not in a git checkout, not the top of it, or
/// the repository's main checkout (its git dir is the common one). Git's own
/// answer, so a submodule or a stray `.git` file is not mistaken for a tree.
///
/// Shared with `ainb run --existing-worktree`, which makes the same check
/// before it starts anything, so the two cannot disagree on what a tree is.
///
/// # Errors
/// The reason, phrased to follow the name of the path being checked.
pub fn linked_worktree_source(canonical: &Path) -> Result<PathBuf, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(canonical)
        .args([
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .ok_or_else(|| "is not a git worktree".to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // `--git-common-dir` may be relative to the directory git ran in.
    let resolved: Vec<Option<PathBuf>> = stdout
        .lines()
        .map(|line| std::fs::canonicalize(canonical.join(line.trim())).ok())
        .collect();
    let [Some(top), Some(git_dir), Some(common_dir)] = resolved.as_slice() else {
        return Err("is not a git worktree".into());
    };
    if top != canonical {
        return Err("is not the top of a git worktree".into());
    }
    if git_dir == common_dir {
        return Err("is a repository's main checkout, not a linked worktree".into());
    }
    common_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "has no source repository".into())
}

/// `worktree_path` as the canonical top of a linked worktree that ainb
/// manages, cut from a repository inside a registered root, or why not.
///
/// A second agent shares the tree with the first, so every check is against
/// the disk, not the request: no `.` or `..` components, a direct child of the
/// managed worktree directory once symlinks are resolved (the only place
/// `ainb run --worktree` puts a tree, so a nested worktree or a link out to
/// one elsewhere is refused), a linked worktree's top
/// ([`linked_worktree_source`]; the main checkout is what `worktree/create`
/// exists to keep agents out of), and a source repository that
/// [`resolve_repo`] accepts. `managed` is [`managed_worktrees`], passed in so
/// a test does not depend on the process's `$AINB_HOME`.
fn resolve_worktree(
    worktree_path: &str,
    home: &Path,
    managed: &Path,
) -> Result<PathBuf, SpawnError> {
    let path = Path::new(worktree_path);
    if !no_dot_components(path) {
        return Err(SpawnError::Invalid(
            "worktree_path must not contain . or .. components".into(),
        ));
    }
    let canonical = std::fs::canonicalize(path).map_err(|_| {
        SpawnError::Invalid(format!(
            "worktree_path is not a directory on this host: {worktree_path}"
        ))
    })?;
    if !is_managed_tree(&canonical, managed) {
        return Err(SpawnError::Invalid(format!(
            "worktree_path is not a worktree ainb created: it must be a folder directly in {}",
            managed.display()
        )));
    }
    let source = linked_worktree_source(&canonical)
        .map_err(|why| SpawnError::Invalid(format!("worktree_path {why}")))?;
    resolve_repo(&source.to_string_lossy(), home).map_err(|error| match error {
        SpawnError::Invalid(why) => SpawnError::Invalid(format!(
            "the worktree's source repository is refused: {why}"
        )),
        failed @ SpawnError::Failed(_) => failed,
    })?;
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
    run_ainb(run_argv(&resolved), "the worktree may still be creating").await
}

/// Add an agent to an existing worktree by running `ainb run
/// --existing-worktree`. Nothing is created but the tmux session and its
/// session row: `ainb run` never deletes a tree or a branch it did not make.
///
/// # Errors
/// As [`worktree_create`].
pub async fn worktree_agent_add(
    params: &WorktreeAgentAddParams,
) -> Result<WorktreeCreateResult, SpawnError> {
    params.validate().map_err(|e| SpawnError::Invalid(e.to_string()))?;
    let home = dirs::home_dir()
        .ok_or_else(|| SpawnError::Failed("the daemon has no home directory".into()))?;
    // git and the filesystem, off the async workers.
    let path = params.worktree_path.clone();
    let tree = tokio::task::spawn_blocking(move || {
        resolve_worktree(&path, &home, &managed_worktrees(&home))
    })
    .await
    .map_err(|e| SpawnError::Failed(format!("checking the worktree: {e}")))??;
    let mut resolved = params.clone();
    resolved.worktree_path = tree.to_string_lossy().into_owned();
    run_ainb(agent_add_argv(&resolved), "the agent may still be starting").await
}

/// How many fresh names a shell create tries before giving up on a run of
/// `duplicate session` answers. Eight hex digits collide about once in four
/// billion, so a second failure means something else is wrong.
const SHELL_NAME_TRIES: usize = 3;

/// Upper bound on one `tmux new-session`. It returns as soon as the session
/// exists; a tmux server that does not answer in this long is wedged.
const SHELL_TMUX_TIMEOUT: Duration = Duration::from_secs(10);

/// `worktree_path` as a directory a shell may open in: the top of a
/// registered repository ([`resolve_repo`]) or a worktree ainb created
/// ([`resolve_worktree`]), canonical, or why not.
fn resolve_shell_dir(
    worktree_path: &str,
    home: &Path,
    managed: &Path,
) -> Result<PathBuf, SpawnError> {
    if !no_dot_components(Path::new(worktree_path)) {
        return Err(SpawnError::Invalid(
            "worktree_path must not contain . or .. components".into(),
        ));
    }
    let repo_why = match resolve_repo(worktree_path, home) {
        Ok(dir) => return Ok(dir),
        Err(SpawnError::Invalid(why)) => why,
        Err(failed) => return Err(failed),
    };
    resolve_worktree(worktree_path, home, managed).map_err(|error| match error {
        SpawnError::Invalid(tree_why) => SpawnError::Invalid(format!(
            "worktree_path is neither a registered repository ({repo_why}) nor a worktree ainb created ({tree_why})"
        )),
        failed @ SpawnError::Failed(_) => failed,
    })
}

/// Open a plain shell in a registered repository or an ainb worktree: a new
/// detached tmux session named `ainb-sh-<id8>`, started in that directory.
///
/// The `ainb-sh-` prefix is the TUI's own shell prefix, so the TUI leaves the
/// session alone: `AppState::load_other_tmux_sessions` skips it from the
/// other-tmux list, `auto_detect_workspace_shells` adopts only `ainb-ws-` as
/// a workspace's shell, and `cleanup_orphaned_tmux_shells` sweeps only
/// `ainb-ws-` and `ainb-shell-` (all in `ainb-app/src/app/state.rs`).
///
/// Never `-A` (attach to a same-named session) and never a kill: a name that
/// is taken is retried with a fresh id, so no live session is touched.
///
/// # Errors
/// [`SpawnError::Invalid`] for a directory refused before tmux ran,
/// [`SpawnError::Failed`] when tmux could not make the session.
pub async fn shell_create(params: &ShellCreateParams) -> Result<ShellCreateResult, SpawnError> {
    params.validate().map_err(|e| SpawnError::Invalid(e.to_string()))?;
    let home = dirs::home_dir()
        .ok_or_else(|| SpawnError::Failed("the daemon has no home directory".into()))?;
    // git and the filesystem, off the async workers.
    let path = params.worktree_path.clone();
    let dir = tokio::task::spawn_blocking(move || {
        resolve_shell_dir(&path, &home, &managed_worktrees(&home))
    })
    .await
    .map_err(|e| SpawnError::Failed(format!("checking the shell's folder: {e}")))??;
    // The helper escapes the folder for tmux and keeps the daemon's secrets
    // out of the server and the shell.
    let start_dir = dir
        .to_str()
        .ok_or_else(|| SpawnError::Invalid("worktree_path is not valid UTF-8".into()))?;
    for _ in 0..SHELL_NAME_TRIES {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let name = format!("ainb-sh-{}", &id[..8]);
        let mut tmux = crate::tmux_session::tmux_new_session(&name, start_dir, &[], &[]);
        // An inherited $TMUX would aim this at the daemon's own client
        // context, as for `ainb run` below.
        tmux.env_remove("TMUX")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            // A wedged tmux is abandoned at the timeout, not left running.
            .kill_on_drop(true);
        let out = match tokio::time::timeout(SHELL_TMUX_TIMEOUT, tmux.output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(SpawnError::Failed(format!("could not run tmux: {e}"))),
            Err(_) => {
                // The name is fresh, so a session under it is this create's:
                // try to take it back. The server may still make it after its
                // client is gone, so name it too: nothing else would find it.
                kill_own_shell(&name).await;
                return Err(SpawnError::Failed(format!(
                    "tmux did not answer within {}s; the shell may still appear as {name}",
                    SHELL_TMUX_TIMEOUT.as_secs()
                )));
            }
        };
        if out.status.success() {
            return Ok(ShellCreateResult {
                tmux_session_name: name,
                worktree_path: dir.to_string_lossy().into_owned(),
            });
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.contains("duplicate session") {
            // The name was free (tmux says so as `duplicate session`), so a
            // session under it now is the one this command made before the
            // secret scrub after it failed: take it back.
            kill_own_shell(&name).await;
            return Err(SpawnError::Failed(format!(
                "tmux could not start the shell {name}: {}",
                stderr_tail(&out.stderr)
            )));
        }
    }
    Err(SpawnError::Failed(format!(
        "tmux refused {SHELL_NAME_TRIES} fresh shell names as duplicates"
    )))
}

/// Best-effort, bounded removal of a shell this create just made under a
/// fresh name, by its exact name (`=name`, never a prefix match).
async fn kill_own_shell(name: &str) {
    let mut kill = tokio::process::Command::new("tmux");
    kill.env_remove("TMUX")
        .args(["kill-session", "-t", &format!("={name}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let _ = tokio::time::timeout(SHELL_TMUX_TIMEOUT, kill.status()).await;
}

/// Run `ainb` with `argv` and read its one JSON line, bounded by the run
/// timeout. `pending` says what a timed-out run may still be doing.
async fn run_ainb(argv: Vec<String>, pending: &str) -> Result<WorktreeCreateResult, SpawnError> {
    let bin = crate::atc::ainb_bin();
    // `ainb run` writes to files, never to pipes the daemon holds. With pipes,
    // anything that stopped the daemon reading them (a create that timed out,
    // a caller that went away, a daemon restart) would turn `ainb run`'s next
    // write into EPIPE, killing it before its own rollback ran and orphaning
    // the tmux session, the worktree and the branch. A file takes every write
    // whatever the daemon is doing.
    let logs = RunLogs::create()?;
    let mut child = tokio::process::Command::new(&bin);
    child
        .args(argv)
        .stdin(std::process::Stdio::null())
        .stdout(logs.stdout()?)
        .stderr(logs.stderr()?)
        // The daemon may itself run inside a tmux pane; an inherited $TMUX
        // would point `ainb run`'s tmux calls at that client's server
        // context instead of the default one sessions live on.
        .env_remove("TMUX");
    // `ainb run` starts tmux itself; a server it starts keeps this
    // environment for every pane after, so the daemon's secrets stay out.
    crate::tmux_session::strip_daemon_secrets(&mut child);
    let child = child
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
                "`ainb run` is still running after {}s: {pending}; check the sidebar",
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
    fn an_agent_add_runs_ainb_run_in_the_existing_worktree() {
        let p = WorktreeAgentAddParams {
            worktree_path: "/w/app--feat--1a2b3c4d".into(),
            agent: SpawnAgent::Gemini,
            model: Some("-flash".into()),
            prompt: Some("-y go".into()),
            skip_permissions: true,
            mutation: ainb_hangar_proto::mutation::MutationEnvelope::default(),
        };
        let argv = agent_add_argv(&p);
        assert_eq!(
            &argv[..7],
            [
                "--format",
                "json",
                "run",
                "--existing-worktree",
                "/w/app--feat--1a2b3c4d",
                "--tool",
                "gemini"
            ]
        );
        assert!(argv.contains(&"--model=-flash".to_string()), "{argv:?}");
        assert!(argv.contains(&"--dangerously-skip-permissions".to_string()));
        assert_eq!(argv.last().map(String::as_str), Some("--prompt=-y go"));
        for create_only in [
            "--worktree",
            "--repo",
            "--create-branch",
            "--base",
            "--name",
        ] {
            assert!(
                !argv.iter().any(|a| a == create_only),
                "{create_only} in {argv:?}"
            );
        }
    }

    /// `world()` plus a linked worktree of its repo under the managed
    /// directory, as `ainb run --worktree` would have left it.
    fn world_with_worktree() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let (home, repo) = world();
        let managed = home.path().join(".agents-in-a-box/worktrees/by-name");
        std::fs::create_dir_all(&managed).unwrap();
        let tree = managed.join("app--feat--1a2b3c4d");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                &tree.display().to_string(),
            ],
        );
        (home, repo, managed, tree)
    }

    #[test]
    fn a_managed_linked_worktree_of_a_registered_repo_resolves() {
        let (home, _repo, managed, tree) = world_with_worktree();
        let resolved =
            resolve_worktree(&tree.display().to_string(), home.path(), &managed).expect("resolves");
        assert_eq!(resolved, std::fs::canonicalize(&tree).unwrap());
    }

    #[test]
    fn a_main_checkout_a_subdir_dots_and_unmanaged_trees_are_refused() {
        let (home, repo, managed, tree) = world_with_worktree();
        let refused = |path: &Path, managed: &Path, why: &str| match resolve_worktree(
            &path.display().to_string(),
            home.path(),
            managed,
        ) {
            Err(SpawnError::Invalid(message)) => {
                assert!(message.contains(why), "{}: {message}", path.display());
            }
            other => panic!("{} must be refused, got {other:?}", path.display()),
        };

        refused(&tree.join("../app--feat--1a2b3c4d"), &managed, "..");
        let sub = tree.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        refused(&sub, &managed, "directly in");
        // In the managed place but inside a checkout, not the top of one.
        let inside = repo.join("src");
        std::fs::create_dir_all(&inside).unwrap();
        refused(&inside, &repo, "top of a git worktree");
        // The main checkout, even when it sits where a managed tree would.
        refused(&repo, &managed, "directly in");
        refused(&repo, repo.parent().unwrap(), "main checkout");
        refused(&managed, &managed, "directly in");
        // A linked worktree nested inside a managed one is not a tree ainb made.
        let nested = tree.join("inner");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "inner",
                &nested.display().to_string(),
            ],
        );
        refused(&nested, &managed, "directly in");
        // A link in the managed place out to a tree elsewhere.
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("outside");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "outside",
                &outside.display().to_string(),
            ],
        );
        let link = managed.join("app--link--00000000");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        refused(&link, &managed, "directly in");
    }

    #[test]
    fn a_worktree_of_an_unregistered_repo_is_refused() {
        let (home, _repo, managed, _tree) = world_with_worktree();
        let elsewhere = tempfile::tempdir().unwrap();
        git(elsewhere.path(), &["init", "-q", "-b", "main"]);
        git(
            elsewhere.path(),
            &["commit", "-q", "--allow-empty", "-m", "init"],
        );
        let stray = managed.join("stray--x--00000000");
        git(
            elsewhere.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "x",
                &stray.display().to_string(),
            ],
        );
        match resolve_worktree(&stray.display().to_string(), home.path(), &managed) {
            Err(SpawnError::Invalid(message)) => {
                assert!(message.contains("registered"), "{message}")
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_shell_opens_in_a_registered_repo_or_a_managed_worktree_only() {
        let (home, repo, managed, tree) = world_with_worktree();
        let ok = |path: &Path| {
            resolve_shell_dir(&path.display().to_string(), home.path(), &managed)
                .unwrap_or_else(|e| panic!("{} must resolve: {e:?}", path.display()))
        };
        assert_eq!(ok(&repo), std::fs::canonicalize(&repo).unwrap());
        assert_eq!(ok(&tree), std::fs::canonicalize(&tree).unwrap());

        let refused = |path: &Path| {
            matches!(
                resolve_shell_dir(&path.display().to_string(), home.path(), &managed),
                Err(SpawnError::Invalid(_))
            )
        };
        assert!(refused(&repo.join("../app")), "dot components");
        let sub = repo.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(refused(&sub), "a subdirectory");
        let elsewhere = tempfile::tempdir().unwrap();
        git(elsewhere.path(), &["init", "-q"]);
        assert!(refused(elsewhere.path()), "an unregistered repository");
    }

    #[test]
    fn an_existing_branch_is_seen_before_any_spawn() {
        let (_home, repo) = world();
        git(&repo, &["branch", "taken"]);
        assert!(branch_exists(&repo, "taken"));
        assert!(!branch_exists(&repo, "fresh"));
    }
}
