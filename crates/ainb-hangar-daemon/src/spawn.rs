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
//! `shell/list` and `shell/close` find and end those shells by their own
//! prefix, [`DAEMON_SHELL_PREFIX`], and the owner option every one of them
//! carries, [`SHELL_OWNER_OPTION`].
//!
//! Served by default. Two boot-time switches turn them off, and they then
//! answer `METHOD_NOT_FOUND`, which a client cannot tell from an older
//! daemon: [`SPAWN_ENV`] set to anything but `1` (`AINB_HANGAR_SPAWN=0`), or
//! [`SPAWN_CONFIG_KEY`] (`[hangar] spawn = false`) in the hangar home's
//! `config/config.toml`. Either one saying off wins. An environment variable
//! and a file the daemon only reads, never a `daemon_config` key, so no
//! connected surface can switch them either way.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use ainb_hangar_proto::spawn::{
    DAEMON_SHELL_PREFIX, ShellCloseParams, ShellCloseResult, ShellCreateParams, ShellCreateResult,
    ShellListResult, WorktreeAgentAddParams, WorktreeCreateParams, WorktreeCreateResult,
    is_daemon_shell_name,
};

/// The boot-time switch for the spawn verbs: unset or `1` serves them,
/// `0` (or any other value) keeps them off. See [`served_by`].
pub const SPAWN_ENV: &str = "AINB_HANGAR_SPAWN";

/// The same switch as a file key: `spawn` under `[hangar]` in
/// [`config_path_in`]. Absent or `true` serves them, `false` keeps them off.
/// See [`served_by_config`].
pub const SPAWN_CONFIG_KEY: &str = "hangar.spawn";

/// Upper bound on one `ainb run`. It waits up to 30s for the agent's input
/// box before sending the prompt, and a first-run Codex thread claim can add
/// more; past this the create is reported failed rather than hanging a caller.
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Whether the spawn verbs are served, read once so a later `set_var` or
/// config edit cannot flip a running daemon.
///
/// Served only when both switches serve them: an opt-out in either one wins,
/// so `AINB_HANGAR_SPAWN=1` does not undo `[hangar] spawn = false`, and an
/// app launched without the variable still honours the file.
#[must_use]
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        // No home, no file to read: the variable alone decides.
        let file =
            crate::hangar_dir().map_or(true, |home| served_by_config(&config_path_in(&home)));
        served(std::env::var_os(SPAWN_ENV).as_deref(), file)
    })
}

/// [`enabled`]'s decision: [`SPAWN_ENV`] is `env`, and the config file
/// serves the verbs when `file` (see [`served_by_config`]). Either switch
/// saying off wins.
#[must_use]
pub fn served(env: Option<&std::ffi::OsStr>, file: bool) -> bool {
    served_by(env) && file
}

/// The file [`SPAWN_CONFIG_KEY`] is read from: `<hangar home>/config/config.toml`.
///
/// The hangar home is `$AINB_HANGAR_HOME` when set and non-empty, else
/// `~/.agents-in-a-box` (see [`crate::hangar_dir`]); never `$HOME` directly.
///
/// The user config, which the daemon also reads `[codex] app_server` and
/// `[acp.adapters]` from, all through [`crate::hangar_config`], and the notifyd
/// and session-reader plugins read their own tables from. A project's
/// `.ainb/config.toml` never reaches the daemon.
///
/// Delegates to [`ainb_hangar_core::paths::config_path_in`], the one place the
/// layout lives.
#[must_use]
pub fn config_path_in(hangar_home: &Path) -> PathBuf {
    ainb_hangar_core::paths::config_path_in(hangar_home)
}

/// Whether a daemon whose [`SPAWN_ENV`] is `value` serves the spawn verbs.
///
/// Only an unset variable changes meaning with the flip: unset now serves
/// them, and every value keeps the answer it had while they were opt-in
/// (`1` on, anything else off). So `AINB_HANGAR_SPAWN=0`, `=false` or `=`
/// is an opt-out, and a typo in one fails closed rather than silently
/// turning the verbs on.
#[must_use]
pub fn served_by(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_none_or(|value| value == "1")
}

/// Whether the config file at `path` lets the spawn verbs be served.
///
/// The file twin of [`served_by`]: only a missing file, no `[hangar]`
/// section, an empty one, or `spawn = true` serves them. Everything else
/// keeps them off, with a warning, so a mistyped opt-out fails closed:
/// `false` or any other value (`"0"`, `0`, `"off"`), any other key under
/// `[hangar]` (it holds this one key, so another is a typo of it), a
/// `hangar` that is not a table, `spawn` under `[hangar_daemon]` (a section
/// this file does not feed), a link to nowhere, and a file that cannot be
/// read or is not valid TOML.
#[must_use]
pub fn served_by_config(path: &Path) -> bool {
    let off = |why: &str| {
        tracing::warn!(path = %path.display(), "{why}; spawn verbs kept off");
        false
    };
    let table = match crate::hangar_config(path) {
        Ok(Some(table)) => table,
        Ok(None) => return true,
        // A dangling link (a dotfiles checkout that is not there) is not
        // "no file": the opt-out may be behind it.
        Err(crate::HangarConfigError::Read(error)) => {
            return off(&format!("cannot read hangar config: {error}"));
        }
        Err(crate::HangarConfigError::Parse(error)) => {
            return off(&format!("hangar config is not valid TOML: {error}"));
        }
    };
    if table.get("hangar_daemon").and_then(|section| section.get("spawn")).is_some() {
        return off(&format!(
            "`hangar_daemon.spawn` is not read from this file; the key is `{SPAWN_CONFIG_KEY}`"
        ));
    }
    let Some(section) = table.get("hangar") else {
        return true;
    };
    let Some(section) = section.as_table() else {
        return off("`hangar` must be a table");
    };
    if let Some(other) = section.keys().find(|key| *key != "spawn") {
        return off(&format!(
            "`hangar.{other}` is not a key; the spawn opt-out is `{SPAWN_CONFIG_KEY}`"
        ));
    }
    match section.get("spawn") {
        None => true,
        Some(toml::Value::Boolean(on)) => *on,
        Some(_) => off(&format!("`{SPAWN_CONFIG_KEY}` must be true or false")),
    }
}

/// Why a create failed, split so the RPC layer can tell a bad request from a
/// host fault.
#[derive(Debug)]
pub enum SpawnError {
    /// The request was refused before anything ran.
    Invalid(String),
    /// The repository is in no registered folder and is not an added
    /// project: refused before anything ran, with its own variant so the RPC
    /// layer answers [`ainb_hangar_proto::spawn::REPO_NOT_REGISTERED`] by
    /// type, whatever the message says.
    Unregistered(String),
    /// Nothing was started: `ainb run` or tmux could not be run at all, or
    /// every fresh shell name was taken.
    Failed(String),
    /// `ainb run`, or a shell's `tmux new-session`, was started and did not
    /// hand back a session: it outlived the bound, its wait was lost, it
    /// failed, or its output was not a session. What it made is its own
    /// (a slow run keeps going, a shell may still appear), so the RPC layer
    /// answers [`ainb_hangar_proto::spawn::SPAWN_STARTED`], which the ledger
    /// records: a retry under the same op id must never start a second one.
    Started(String),
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
/// workspace scan paths and onboarding git directories, canonicalized. Read
/// fresh per call (a person may add one at any time); a missing or unreadable
/// file adds none. Public so the desktop's project list
/// (`ainb-desktop/src/projects.rs`) offers exactly what this accepts, from
/// this one definition.
#[must_use]
pub fn registered_roots(home: &Path) -> Vec<PathBuf> {
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
                if let Ok(canonical) = std::fs::canonicalize(expand_home(home, path)) {
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

/// The message of [`SpawnError::Unregistered`], the refusal for a repository
/// in no registered folder and not an added project. The variant, not this
/// text, is what every spawn verb answers
/// [`ainb_hangar_proto::spawn::REPO_NOT_REGISTERED`] for.
pub const UNREGISTERED: &str = "repo_path is not under a registered workspace folder: add its \
                                folder to workspace_defaults.workspace_scan_paths";

/// `path` with a leading `~/` meaning `home`, as the registered folder
/// files are read.
#[must_use]
pub fn expand_home(home: &Path, path: &str) -> PathBuf {
    path.strip_prefix("~/")
        .map_or_else(|| PathBuf::from(path), |rest| home.join(rest))
}

/// The file [`registered_projects`] reads, under `~/.agents-in-a-box/config`.
/// Its own file, not a `config.toml` key: a settings save prunes keys it does
/// not model under `[workspace_defaults]`, and onboarding replaces
/// `workspace_scan_paths` wholesale; neither touches this.
pub const PROJECTS_FILE: &str = "projects.toml";

/// Repositories a person added one at a time (the desktop's Add project),
/// canonicalized: `projects = [...]` in [`PROJECTS_FILE`]. Unlike a
/// [`registered_roots`] entry each is matched EXACTLY, never as a root, so
/// adding one repository admits no repository nested inside it. A missing or
/// unreadable file adds none.
#[must_use]
pub fn registered_projects(home: &Path) -> Vec<PathBuf> {
    let file = home.join(".agents-in-a-box").join("config").join(PROJECTS_FILE);
    let table = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .unwrap_or_default();
    table
        .get("projects")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .filter_map(|path| std::fs::canonicalize(expand_home(home, path)).ok())
        .collect()
}

/// `repo_path` as the canonical top of a git repository inside a registered
/// root, or exactly a registered project, or why not. Refuses `.` and `..` components before resolving
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
    if !roots.iter().any(|root| canonical.starts_with(root))
        && !registered_projects(home).contains(&canonical)
    {
        return Err(SpawnError::Unregistered(UNREGISTERED.into()));
    }
    let top = std::process::Command::new("git")
        .arg("-C")
        .arg(&canonical)
        .args(["rev-parse", "--show-toplevel"])
        // An inherited GIT_DIR or GIT_WORK_TREE would answer for that
        // repository instead of the one at `canonical`.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
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
    // An unregistered source stays `Unregistered`: the fix (add the project)
    // is the same as for `worktree/create`, so the code is too.
    resolve_repo(&source.to_string_lossy(), home).map_err(|error| match error {
        SpawnError::Invalid(why) => SpawnError::Invalid(format!(
            "the worktree's source repository is refused: {why}"
        )),
        SpawnError::Unregistered(why) => SpawnError::Unregistered(format!(
            "the worktree's source repository is refused: {why}"
        )),
        fault @ (SpawnError::Failed(_) | SpawnError::Started(_)) => fault,
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
/// [`SpawnError::Unregistered`] for a repository no registered folder or
/// project admits. [`SpawnError::Invalid`] for any other request refused:
/// params that fail `validate`, a `repo_path` with `.` or `..` components,
/// that is not a directory or not the top of a git repository, or a `branch`
/// that already exists. Both are answered before `ainb run` ran.
/// [`SpawnError::Failed`] when the daemon has no home directory, or
/// `ainb run` could not be started. [`SpawnError::Started`] when it was
/// started and failed, outlived the bound, or printed no result it could
/// read.
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
/// [`SpawnError::Unregistered`] for a worktree whose source repository no
/// registered folder or project admits. [`SpawnError::Invalid`] for any other
/// request refused: params that fail `validate`, or a `worktree_path` with
/// `.` or `..` components, that is not a directory, not a folder directly in
/// [`managed_worktrees`], not the top of a linked worktree, or whose source
/// repository is otherwise refused. Both are answered before `ainb run` ran.
/// [`SpawnError::Failed`] when the daemon has no home directory, the
/// worktree check could not finish, or `ainb run` could not be started;
/// [`SpawnError::Started`] as for [`worktree_create`].
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

/// The tmux user option a shell carries the op id of the create that made
/// it: a label for whoever reads the session, not a lookup. Retries are the
/// D18 ledger's (see [`shell_create`]).
const SHELL_OP_OPTION: &str = "@ainb_op_id";

/// The tmux user option every shell the daemon opens carries, set to
/// [`SHELL_OWNER`] as the session is made. `shell/list` and `shell/close`
/// act only on a session that has it: a user's session that happens to use
/// [`DAEMON_SHELL_PREFIX`] is never listed or closed.
const SHELL_OWNER_OPTION: &str = "@ainb_owner";

/// The value of [`SHELL_OWNER_OPTION`] on a daemon shell.
const SHELL_OWNER: &str = "daemon";

/// `worktree_path` as a directory a shell may open in: the top of a
/// registered repository ([`resolve_repo`]) or a worktree ainb created
/// ([`resolve_worktree`]), canonical, or why not.
///
/// The refusal is [`SpawnError::Unregistered`] when registering a project is
/// the fix: a managed tree whose source repository is unregistered, or a
/// folder outside the managed directory that no registered root or project
/// admits. Anything else, a managed folder that is not a usable tree
/// included, is [`SpawnError::Invalid`].
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
    let (repo_why, repo_unregistered) = match resolve_repo(worktree_path, home) {
        Ok(dir) => return Ok(dir),
        Err(SpawnError::Invalid(why)) => (why, false),
        Err(SpawnError::Unregistered(why)) => (why, true),
        Err(failed) => return Err(failed),
    };
    let in_managed = std::fs::canonicalize(worktree_path)
        .is_ok_and(|canonical| is_managed_tree(&canonical, managed));
    resolve_worktree(worktree_path, home, managed).map_err(|error| {
        let (tree_why, tree_unregistered) = match error {
            SpawnError::Invalid(why) => (why, false),
            SpawnError::Unregistered(why) => (why, true),
            fault @ (SpawnError::Failed(_) | SpawnError::Started(_)) => return fault,
        };
        let why = format!(
            "worktree_path is neither a registered repository ({repo_why}) nor a worktree ainb created ({tree_why})"
        );
        if tree_unregistered || (repo_unregistered && !in_managed) {
            SpawnError::Unregistered(why)
        } else {
            SpawnError::Invalid(why)
        }
    })
}

/// Open a plain shell in a registered repository or an ainb worktree: a new
/// detached tmux session named `ainb-dsh-<id8>` ([`DAEMON_SHELL_PREFIX`]),
/// started in that directory.
///
/// The prefix is the daemon's own. The TUI's shells are `ainb-sh-`, which it
/// filters out of its tmux list, so a daemon shell under that prefix could
/// not be told from the TUI's and nothing listed or closed it.
///
/// The session is made by [`crate::tmux_session::tmux_new_session`], so the
/// shell never holds the daemon's OAuth token, whoever started the server.
///
/// A create that carries an `op_id` is made idempotent by the D18 ledger at
/// dispatch (`shell/create` is in `MUTATING_METHODS`): a retry with the same
/// op id and body replays the first reply, the same shell, and runs nothing.
/// The op id is also set on the session as [`SHELL_OP_OPTION`], a label only.
/// Every shell is marked [`SHELL_OWNER_OPTION`] in the same tmux command, so
/// no daemon shell exists without it.
///
/// Never `-A` (attach to a same-named session), and the only kill is of a
/// session this call just made under a fresh name and failed to finish: a
/// name that is taken is retried with a fresh id, so no live session is
/// touched.
///
/// # Errors
/// [`SpawnError::Unregistered`] for a folder that registering a project
/// would admit (see `resolve_shell_dir`) and [`SpawnError::Invalid`] for any
/// other directory or op id refused, both before tmux ran;
/// [`SpawnError::Failed`] when tmux could not be run or every fresh name was
/// taken; [`SpawnError::Started`] when `new-session` ran and may have made
/// the shell (it did not answer in time, or failed after running).
pub async fn shell_create(params: &ShellCreateParams) -> Result<ShellCreateResult, SpawnError> {
    shell_create_named(params, || {
        uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
    })
    .await
}

/// [`shell_create`] with the id each name attempt uses drawn from `next_id`
/// (eight lowercase hex digits), so a test can hand it a name already taken.
///
/// # Errors
/// As [`shell_create`].
pub async fn shell_create_named(
    params: &ShellCreateParams,
    mut next_id: impl FnMut() -> String,
) -> Result<ShellCreateResult, SpawnError> {
    params.validate().map_err(|e| SpawnError::Invalid(e.to_string()))?;
    let op_id = match &params.mutation.op_id {
        Some(op_id) => Some(
            ainb_hangar_proto::mutation::OpId::parse(op_id.as_str())
                .map_err(|why| SpawnError::Invalid(format!("op_id: {why}")))?,
        ),
        None => None,
    };
    let home = dirs::home_dir()
        .ok_or_else(|| SpawnError::Failed("the daemon has no home directory".into()))?;
    // git and the filesystem, off the async workers.
    let path = params.worktree_path.clone();
    let dir = tokio::task::spawn_blocking(move || {
        resolve_shell_dir(&path, &home, &managed_worktrees(&home))
    })
    .await
    .map_err(|e| SpawnError::Failed(format!("checking the shell's folder: {e}")))??;
    let worktree_path = dir.to_string_lossy().into_owned();
    // The helper escapes the folder for tmux and keeps the daemon's secrets
    // out of the server and the shell.
    let start_dir = dir
        .to_str()
        .ok_or_else(|| SpawnError::Invalid("worktree_path is not valid UTF-8".into()))?;
    for _ in 0..SHELL_NAME_TRIES {
        let name = format!("{DAEMON_SHELL_PREFIX}{}", next_id());
        if !is_daemon_shell_name(&name) {
            return Err(SpawnError::Failed(format!("not a shell name: {name}")));
        }
        // The one helper makes the session: `tmux -u`, the daemon's secrets
        // kept out, the folder escaped, `$TMUX` followed as every other
        // daemon tmux call follows it.
        let mut tmux = crate::tmux_session::tmux_new_session(&name, start_dir, &[], &[]);
        crate::tmux_session::set_session_option(&mut tmux, &name, SHELL_OWNER_OPTION, SHELL_OWNER);
        if let Some(op_id) = &op_id {
            crate::tmux_session::set_session_option(
                &mut tmux,
                &name,
                SHELL_OP_OPTION,
                op_id.as_str(),
            );
        }
        tmux.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            // A wedged tmux is abandoned at the timeout, not left running.
            .kill_on_drop(true);
        let out = match tokio::time::timeout(shell_tmux_timeout(), tmux.output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(SpawnError::Failed(format!("could not run tmux: {e}"))),
            Err(_) => {
                // The name is fresh, so a session under it is this create's:
                // try to take it back. The server may still make it after its
                // client is gone, so name it too: nothing else would find it.
                kill_own_shell(&name).await;
                return Err(SpawnError::Started(format!(
                    "tmux did not answer within {}s; the shell may still appear as {name}",
                    shell_tmux_timeout().as_secs()
                )));
            }
        };
        if out.status.success() {
            return Ok(ShellCreateResult {
                tmux_session_name: name,
                worktree_path,
            });
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.contains("duplicate session") {
            // The name was free (tmux says so as `duplicate session`), so a
            // session under it now is the one this command made before the
            // steps after it (secret scrub, owner and op id labels) failed:
            // take it back.
            // The take-back is best-effort, so the session may outlive it.
            kill_own_shell(&name).await;
            return Err(SpawnError::Started(format!(
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
    kill.args(["kill-session", "-t", &format!("={name}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let _ = tokio::time::timeout(shell_tmux_timeout(), kill.status()).await;
}

/// Every shell the daemon opened that is still running, by name. Only
/// [`DAEMON_SHELL_PREFIX`] sessions marked [`SHELL_OWNER_OPTION`]: the TUI's
/// shells, agents' sessions and the user's own, whatever they are named, are
/// never listed. No tmux server running is no shells.
///
/// # Errors
/// [`SpawnError::Failed`] when tmux could not be asked.
pub async fn shell_list() -> Result<ShellListResult, SpawnError> {
    let mut shells = daemon_shells().await?;
    shells.sort_by(|a, b| a.tmux_session_name.cmp(&b.tmux_session_name));
    Ok(ShellListResult { shells })
}

/// End one shell the daemon opened, by its exact name (`=<name>`, never a
/// prefix match). Any name that is not a daemon shell's is refused before
/// tmux runs. A shell already gone answers `closed: false`, and so does a
/// session under a daemon shell's name that [`shell_list`] would not list
/// (no [`SHELL_OWNER_OPTION`]): it is not one of the daemon's, and stays.
///
/// # Errors
/// [`SpawnError::Invalid`] for a name that is not a daemon shell's,
/// [`SpawnError::Failed`] when tmux could not end a running one.
pub async fn shell_close(params: &ShellCloseParams) -> Result<ShellCloseResult, SpawnError> {
    params.validate().map_err(|e| SpawnError::Invalid(e.to_string()))?;
    if !owned_by_the_daemon(&params.tmux_session_name).await? {
        return Ok(ShellCloseResult { closed: false });
    }
    let exact = format!("={}", params.tmux_session_name);
    let killed = run_tmux(&["kill-session", "-t", &exact]).await.map_err(|e| e.failed())?;
    if killed.status.success() {
        return Ok(ShellCloseResult { closed: true });
    }
    if no_such_session(&killed.stderr) {
        return Ok(ShellCloseResult { closed: false });
    }
    Err(SpawnError::Failed(format!(
        "tmux could not close {}: {}",
        params.tmux_session_name,
        stderr_tail(&killed.stderr)
    )))
}

/// Every running [`DAEMON_SHELL_PREFIX`] session marked
/// [`SHELL_OWNER_OPTION`], with its start folder.
///
/// The owner is read per candidate by [`owned_by_the_daemon`], not with a
/// `#{@ainb_owner}` column here: a format falls back to the global option,
/// so one `set -g @ainb_owner daemon` would claim every prefixed session.
/// One more tmux call per prefixed session, a handful at most.
async fn daemon_shells() -> Result<Vec<ShellCreateResult>, SpawnError> {
    // The path goes last: it is the one field that could hold a tab.
    let out = run_tmux(&["list-sessions", "-F", "#{session_name}\t#{session_path}"])
        .await
        .map_err(|e| e.failed())?;
    if !out.status.success() {
        if no_such_session(&out.stderr) {
            return Ok(Vec::new());
        }
        return Err(SpawnError::Failed(format!(
            "tmux could not list sessions: {}",
            stderr_tail(&out.stderr)
        )));
    }
    let mut shells = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((name, path)) = line.split_once('\t') else {
            continue;
        };
        if is_daemon_shell_name(name) && owned_by_the_daemon(name).await? {
            shells.push(ShellCreateResult {
                tmux_session_name: name.to_string(),
                worktree_path: path.to_string(),
            });
        }
    }
    Ok(shells)
}

/// Whether the session named exactly `name` carries [`SHELL_OWNER_OPTION`]
/// set to [`SHELL_OWNER`] as its OWN option. `show-options` without `-g` or
/// `-A` reads the session's options only, never the global value a format
/// or an inherited lookup would fall back to. A session that is gone, or
/// no server at all, is not the daemon's.
async fn owned_by_the_daemon(name: &str) -> Result<bool, SpawnError> {
    let exact = format!("={name}:");
    let out = run_tmux(&["show-options", "-qv", "-t", &exact, SHELL_OWNER_OPTION])
        .await
        .map_err(|e| e.failed())?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim_end() == SHELL_OWNER);
    }
    if no_such_session(&out.stderr) {
        return Ok(false);
    }
    Err(SpawnError::Failed(format!(
        "tmux could not read {name}'s owner: {}",
        stderr_tail(&out.stderr)
    )))
}

/// Whether tmux's `stderr` says the session, or the whole server, is not
/// there: an answer, unlike a socket it may not open (which is a fault).
fn no_such_session(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr);
    stderr.contains("can't find session")
        || stderr.contains("no server running")
        || (stderr.contains("error connecting") && stderr.contains("No such file or directory"))
}

/// Why a tmux call did not return an answer.
enum TmuxError {
    Spawn(std::io::Error),
    Timeout,
}

impl TmuxError {
    fn failed(self) -> SpawnError {
        match self {
            Self::Spawn(e) => SpawnError::Failed(format!("could not run tmux: {e}")),
            Self::Timeout => SpawnError::Failed(format!(
                "tmux did not answer within {}s",
                shell_tmux_timeout().as_secs()
            )),
        }
    }
}

/// Run one tmux command for the shell verbs, bounded by
/// [`SHELL_TMUX_TIMEOUT`]. Any of them may start the tmux server, which keeps
/// the environment it started with for every pane after, so the daemon's
/// secrets are dropped from all of them.
async fn run_tmux(args: &[&str]) -> Result<std::process::Output, TmuxError> {
    let mut tmux = tokio::process::Command::new("tmux");
    // `-u`: a daemon started without a UTF-8 locale (launchd sets none)
    // would otherwise get every tab and non-ASCII byte in a format back as
    // `_`, and no listed path would match the folder it was opened in.
    tmux.arg("-u")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // A wedged tmux is abandoned at the timeout, not left running.
        .kill_on_drop(true);
    // `$TMUX` is inherited, so list and close reach the server the create
    // made its shells on.
    crate::tmux_session::strip_daemon_secrets(&mut tmux);
    match tokio::time::timeout(shell_tmux_timeout(), tmux.output()).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(TmuxError::Spawn(e)),
        Err(_) => Err(TmuxError::Timeout),
    }
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
        Ok(Ok(Err(e))) => return Err(SpawnError::Started(format!("waiting on {bin}: {e}"))),
        Ok(Err(e)) => return Err(SpawnError::Started(format!("waiting on {bin}: {e}"))),
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
                drop(logs);
            });
            return Err(SpawnError::Started(format!(
                "`ainb run` is still running after {}s: {pending}; check the sidebar",
                timeout.as_secs()
            )));
        }
    };
    let stdout = logs.read_stdout();
    let stderr = logs.read_stderr();
    drop(logs);
    if !status.success() {
        return Err(SpawnError::Started(format!(
            "`ainb run` failed: {}",
            stderr_tail(&stderr)
        )));
    }
    parse_run_output(&String::from_utf8_lossy(&stdout)).map_err(SpawnError::Started)
}

/// Where one `ainb run` writes its stdout and stderr: two files under the
/// daemon's log dir (`<hangar>/hangar/logs/spawn/`), readable by the daemon's
/// user only. Dropping it removes them, so every arm of a run clears its
/// output: the outcome read, an open or a spawn that failed, a wait that
/// errored, and a timed-out run once it ends.
struct RunLogs {
    stdout: std::path::PathBuf,
    stderr: std::path::PathBuf,
}

impl RunLogs {
    fn create() -> Result<Self, SpawnError> {
        let dir = own_run_logs_dir()
            .map_err(|e| SpawnError::Failed(format!("no place for `ainb run` output: {e}")))?;
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
}

impl Drop for RunLogs {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.stdout);
        let _ = std::fs::remove_file(&self.stderr);
    }
}

/// `<hangar>/hangar/logs/spawn/`, where every `ainb run` writes its output:
/// created `0700` when missing and made `0700` when it is not (`DirBuilder`'s
/// mode only applies to a directory it creates, and an older daemon's
/// `create_dir_all` left it `0755`, or `0775` under umask 002).
///
/// `Err` unless it is a real directory (not a symlink) this user owns, so
/// nothing is ever written into, or removed from, a directory that is not the
/// daemon's. Ownership is checked BEFORE the mode is tightened, so an open
/// directory of ours is repaired rather than refused forever; the private
/// check runs after, as a post-condition.
///
/// Accepted: the `logs/` parent is followed if it is a symlink (it is the
/// daemon's log dir, trusted like the rest of the home), and a process of the
/// same user could swap `spawn` between the metadata check and
/// `set_permissions`. Both need this user's own access already.
fn own_run_logs_dir() -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    let dir = crate::log_dir().map_err(|e| format!("no log directory: {e}"))?.join("spawn");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let meta =
        std::fs::symlink_metadata(&dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
    if !meta.file_type().is_dir() || meta.uid() != nix::unistd::geteuid().as_raw() {
        return Err(format!(
            "{} is not a private directory of this user (a symlink, or another user's)",
            dir.display()
        ));
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("making {} private: {e}", dir.display()))?;
    if !crate::hook_ingress::dir_is_ours(&dir) {
        return Err(format!(
            "{} is not a private directory of this user after making it 0700",
            dir.display()
        ));
    }
    Ok(dir)
}

/// Whether `name` is one of a run's own files: [`RunLogs`] names them by a
/// simple UUID, 32 lowercase hex digits, then `.stdout` or `.stderr`.
fn is_run_log_name(name: &str) -> bool {
    let Some((stem, stream)) = name.split_once('.') else {
        return false;
    };
    stem.len() == 32
        && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && matches!(stream, "stdout" | "stderr")
}

/// Remove the `ainb run` output a previous daemon left behind. A run's files
/// go when its outcome is read, but a daemon killed mid-run never reads it.
///
/// Only a run's own files go: regular files named as [`RunLogs`] names them,
/// directly in [`own_run_logs_dir`]. A symlink is never followed and nothing
/// is recursed into; a directory that is not the daemon's is not swept.
///
/// Called at boot once this process owns the home, so no run of this
/// daemon's is writing yet. A run a dead daemon started may still be; its
/// writes go on landing in the unlinked file, so removing it never fails the
/// run.
pub fn remove_stale_run_logs() {
    let dir = match own_run_logs_dir() {
        Ok(dir) => dir,
        Err(error) => {
            tracing::warn!(%error, "not sweeping stale `ainb run` output");
            return;
        }
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(dir = %dir.display(), %error, "could not list stale `ainb run` output");
            return;
        }
    };
    let mut removed = 0_usize;
    for entry in entries.filter_map(Result::ok) {
        // `DirEntry::file_type` does not follow a symlink.
        let ours = entry.file_name().to_str().is_some_and(is_run_log_name)
            && entry.file_type().is_ok_and(|kind| kind.is_file());
        if !ours {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(error) => tracing::warn!(
                file = %entry.path().display(),
                %error,
                "could not remove stale `ainb run` output"
            ),
        }
    }
    if removed > 0 {
        tracing::info!(removed, "removed stale `ainb run` output");
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

/// Test seam: a shorter wait than [`SHELL_TMUX_TIMEOUT`], in milliseconds,
/// or 0 for the real one, so a test of a wedged tmux does not sit through
/// ten seconds of it.
#[cfg(any(test, feature = "test-support"))]
static SHELL_TMUX_TIMEOUT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Shorten the wait on the shell verbs' tmux, or restore it with `None`.
/// Test-only.
#[cfg(any(test, feature = "test-support"))]
pub fn set_shell_tmux_timeout_for_test(timeout: Option<Duration>) {
    let ms = timeout.map_or(0, |t| u64::try_from(t.as_millis()).unwrap_or(u64::MAX));
    SHELL_TMUX_TIMEOUT_MS.store(ms, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(any(test, feature = "test-support"))]
fn shell_tmux_timeout() -> Duration {
    match SHELL_TMUX_TIMEOUT_MS.load(std::sync::atomic::Ordering::SeqCst) {
        0 => SHELL_TMUX_TIMEOUT,
        ms => Duration::from_millis(ms),
    }
}

/// Compiled out of a shipped daemon: always the real wait.
#[cfg(not(any(test, feature = "test-support")))]
const fn shell_tmux_timeout() -> Duration {
    SHELL_TMUX_TIMEOUT
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
            Err(SpawnError::Unregistered(_))
        ));

        let sub = repo.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(matches!(
            resolve_repo(&sub.display().to_string(), home.path()),
            Err(SpawnError::Invalid(m)) if m.contains("top of a git repository")
        ));
    }

    /// An added project is matched exactly: it resolves, a repository nested
    /// inside it (vendored, a submodule) does not.
    #[test]
    fn a_registered_project_admits_itself_and_no_nested_repository() {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join("elsewhere/app");
        let nested = project.join("vendor/lib");
        std::fs::create_dir_all(&nested).unwrap();
        git(&project, &["init", "-q", "-b", "main"]);
        git(&nested, &["init", "-q", "-b", "main"]);
        let config = home.path().join(".agents-in-a-box/config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join(PROJECTS_FILE),
            format!("projects = [\"{}\"]\n", project.display()),
        )
        .unwrap();

        let resolved = resolve_repo(&project.display().to_string(), home.path()).expect("resolves");
        assert_eq!(resolved, std::fs::canonicalize(&project).unwrap());
        assert!(matches!(
            resolve_repo(&nested.display().to_string(), home.path()),
            Err(SpawnError::Unregistered(_))
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

    /// A managed tree cut from a repository outside every registered folder,
    /// left in the managed directory next to `world_with_worktree`'s tree.
    fn stray_tree(managed: &Path) -> (tempfile::TempDir, PathBuf) {
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
        (elsewhere, stray)
    }

    /// Unregistered, not Invalid: the fix is to add the project, as it is
    /// for `worktree/create`, so every spawn verb answers the same code.
    #[test]
    fn a_worktree_of_an_unregistered_repo_is_refused_as_unregistered() {
        let (home, _repo, managed, _tree) = world_with_worktree();
        let (_elsewhere, stray) = stray_tree(&managed);
        match resolve_worktree(&stray.display().to_string(), home.path(), &managed) {
            Err(SpawnError::Unregistered(message)) => {
                assert!(message.contains("registered"), "{message}")
            }
            other => panic!("expected Unregistered, got {other:?}"),
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
        let unregistered = |path: &Path| {
            matches!(
                resolve_shell_dir(&path.display().to_string(), home.path(), &managed),
                Err(SpawnError::Unregistered(_))
            )
        };
        assert!(refused(&repo.join("../app")), "dot components");
        let sub = repo.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(refused(&sub), "a subdirectory");
        // A whole repository planted where a managed tree would be: adding a
        // project would not make it a tree ainb made.
        let planted = managed.join("planted--main--00000000");
        std::fs::create_dir_all(&planted).unwrap();
        git(&planted, &["init", "-q", "-b", "main"]);
        assert!(refused(&planted), "a main checkout in the managed place");

        let elsewhere = tempfile::tempdir().unwrap();
        git(elsewhere.path(), &["init", "-q"]);
        assert!(unregistered(elsewhere.path()), "an unregistered repository");
        let (_source, stray) = stray_tree(&managed);
        assert!(unregistered(&stray), "a tree of an unregistered repository");
    }

    /// On unless the operator set the variable to something other than `1`.
    #[test]
    fn the_spawn_verbs_are_served_unless_opted_out() {
        use std::ffi::OsStr;
        assert!(served_by(None), "unset serves them: the default");
        assert!(served_by(Some(OsStr::new("1"))));
        for off in ["0", "", "false", "off", "no", "true", " 1"] {
            assert!(!served_by(Some(OsStr::new(off))), "{off:?} keeps them off");
        }
    }

    /// Served only when neither switch opts out: `=1` does not undo the file.
    #[test]
    fn either_switch_saying_off_keeps_the_spawn_verbs_off() {
        use std::ffi::OsStr;
        for (env, file, want) in [
            (None, true, true),
            (Some("1"), true, true),
            (None, false, false),
            (Some("1"), false, false),
            (Some("0"), true, false),
            (Some("0"), false, false),
        ] {
            assert_eq!(
                served(env.map(OsStr::new), file),
                want,
                "env {env:?}, file serves {file}"
            );
        }
    }

    /// On unless the file opts out; anything it cannot read as `true` is off.
    #[test]
    fn the_config_file_serves_the_spawn_verbs_unless_opted_out() {
        let home = tempfile::tempdir().unwrap();
        let path = config_path_in(home.path());
        assert!(served_by_config(&path), "no file serves them: the default");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for on in [
            "",
            "[other]\nkey = 1\n",
            "[hangar_daemon]\nautostandup = 1\n",
            "[hangar]\n",
            "[hangar]\nspawn = true\n",
        ] {
            std::fs::write(&path, on).unwrap();
            assert!(served_by_config(&path), "{on:?} serves them");
        }
        for off in [
            "[hangar]\nspawn = false\n",
            "hangar.spawn = false\n",
            "[hangar]\nspawn = \"0\"\n",
            "[hangar]\nspawn = \"true\"\n",
            "[hangar]\nspawn = 1\n",
            "hangar = \"off\"\n",
            "[hangar\nspawn = true\n",
            "[hangar]\nspawn_verbs = false\n",
            "[hangar]\nspawn = true\nspwan = false\n",
            "[hangar_daemon]\nspawn = false\n",
        ] {
            std::fs::write(&path, off).unwrap();
            assert!(!served_by_config(&path), "{off:?} keeps them off");
        }
    }

    /// A config file the daemon cannot read is not taken as "no opt-out".
    #[test]
    fn an_unreadable_config_file_keeps_the_spawn_verbs_off() {
        let home = tempfile::tempdir().unwrap();
        let path = config_path_in(home.path());
        // A directory where the file should be: present, but never readable.
        std::fs::create_dir_all(&path).unwrap();
        assert!(!served_by_config(&path));

        let linked = home.path().join("linked.toml");
        std::os::unix::fs::symlink(home.path().join("not-there.toml"), &linked).unwrap();
        assert!(
            !served_by_config(&linked),
            "a link to nowhere is not a missing file"
        );
    }

    #[test]
    fn an_existing_branch_is_seen_before_any_spawn() {
        let (_home, repo) = world();
        git(&repo, &["branch", "taken"]);
        assert!(branch_exists(&repo, "taken"));
        assert!(!branch_exists(&repo, "fresh"));
    }
}
