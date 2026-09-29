//! The projects the composer offers, and the one way the window adds one.
//!
//! ```text
//!  config.toml      workspace_scan_paths ─┐                 repositories under
//!  onboarding.toml  git_directories ──────┴─▶ roots ───────▶ them (scanned) ─┐
//!  projects.toml    projects (Add project) ─▶ exact repository tops ─────────┴─▶ Project select
//! ```
//!
//! Both sets are the daemon's own readers (`spawn::registered_roots`,
//! `spawn::registered_projects`), called rather than copied, so the list names
//! exactly the repositories a create is accepted from. The daemon stays the
//! gate. `tests/create_worktree.rs` checks both against the real daemon.

use std::path::{Path, PathBuf};

use ainb_app::config::WorkspaceDefaults;
use ainb_app::git::WorkspaceScanner;
use ainb_hangar_daemon::spawn::{
    PROJECTS_FILE, expand_home, registered_projects, registered_roots,
};
use serde::Serialize;

/// One repository the composer may create into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct RegisteredProject {
    /// The repository folder's own name.
    pub name: String,
    /// Canonical absolute path: the form the daemon compares against.
    pub path: String,
}

impl RegisteredProject {
    fn at(canonical: &Path) -> Self {
        Self {
            name: canonical
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            path: canonical.display().to_string(),
        }
    }
}

/// The repositories under `home`'s registered roots, by name, at most
/// `defaults.max_repositories` of them, then every added project. Roots are scanned the
/// way the TUI's repository picker scans (depth and excludes from
/// `defaults`), but never through its cache, which is keyed on the TUI's own
/// wider set of folders. A scanned repository whose canonical path leaves
/// every root (a symlink out) is not offered: the daemon would refuse it.
#[must_use]
pub fn list(home: &Path, defaults: &WorkspaceDefaults) -> Vec<RegisteredProject> {
    let roots = registered_roots(home);
    let scanned: Vec<PathBuf> = if roots.is_empty() {
        Vec::new()
    } else {
        WorkspaceScanner::new()
            .with_search_paths(roots.clone())
            .with_workspace_defaults(defaults)
            .scan_uncached()
            .map(|result| result.workspaces)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|workspace| std::fs::canonicalize(workspace.path).ok())
            .filter(|canonical| roots.iter().any(|root| canonical.starts_with(root)))
            .collect()
    };
    let mut projects: Vec<RegisteredProject> = Vec::new();
    for canonical in scanned {
        push_new(&mut projects, RegisteredProject::at(&canonical));
    }
    sort_by_name(&mut projects);
    if projects.len() > defaults.max_repositories {
        tracing::info!(
            found = projects.len(),
            shown = defaults.max_repositories,
            "scanned projects cut at workspace_defaults.max_repositories"
        );
        projects.truncate(defaults.max_repositories);
    }
    // Added projects after the scan and never cut: each is one a person
    // picked, and a pick the list dropped would leave the Project select
    // showing another repository than the one a create sends.
    let mut added: Vec<RegisteredProject> = registered_projects(home)
        .iter()
        .map(|path| RegisteredProject::at(path))
        .collect();
    sort_by_name(&mut added);
    for project in added {
        push_new(&mut projects, project);
    }
    projects
}

fn push_new(projects: &mut Vec<RegisteredProject>, project: RegisteredProject) {
    if !projects.iter().any(|known| known.path == project.path) {
        projects.push(project);
    }
}

fn sort_by_name(projects: &mut [RegisteredProject]) {
    projects.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
}

/// Register `folder`, a repository a person picked in the native folder
/// dialog, so the daemon creates from it: its canonical path is appended to
/// `projects` in the daemon's [`PROJECTS_FILE`]. The daemon matches that entry
/// exactly, never as a root, so no repository nested inside it is admitted.
/// Only a repository's own top folder is accepted, never the home folder or
/// one above it. Every pick is recorded, even one inside a registered root:
/// the scan that lists root repositories stops at `scan_max_depth` and
/// `exclude_paths`, and a pick it cannot see would be accepted but never
/// listed. A pick already in the file is not added twice.
///
/// # Errors
/// The sentence the composer shows: no home, not a folder, not a repository's
/// top, the home folder, or a projects file that cannot be read or written.
pub fn register(home: &Path, folder: &Path) -> Result<RegisteredProject, String> {
    let shown = folder.display();
    let home = std::fs::canonicalize(home).map_err(|_| {
        "The home folder cannot be resolved, so no project can be registered.".to_string()
    })?;
    let canonical = std::fs::canonicalize(folder)
        .ok()
        .filter(|path| path.is_dir())
        .ok_or_else(|| format!("{shown} is not a folder on this machine."))?;
    if home.starts_with(&canonical) {
        return Err(format!(
            "{shown} holds your home folder: pick one repository's own folder."
        ));
    }
    if repository_top(&canonical).as_deref() != Some(canonical.as_path()) {
        return Err(format!(
            "{shown} is not the top folder of a git repository: pick the repository's own folder."
        ));
    }
    let project = RegisteredProject::at(&canonical);
    append_project(&home, &canonical)
        .map_err(|error| format!("Could not register {shown}: {error:#}"))?;
    Ok(project)
}

/// The canonical top of the git checkout `dir` is in, if any: git's own
/// answer, the same question the daemon asks before it creates.
fn repository_top(dir: &Path) -> Option<PathBuf> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        // An inherited GIT_DIR or GIT_WORK_TREE would answer for that
        // repository instead of the one at `dir`.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| std::fs::canonicalize(String::from_utf8_lossy(&out.stdout).trim()).ok())
}

/// What heads the projects file, so a person who opens it knows what it is.
const PROJECTS_HEADER: &str = "# Repositories added with Add project in the ainb desktop.\n\
# The daemon creates worktrees from exactly these folders (never from a\n\
# repository nested inside one). Remove a line to unregister it.\n";

/// Append `canonical` to the projects file unless an entry already names
/// it, deciding under the lock from what is read inside it, so two adds can
/// neither lose one nor record one twice. A file that exists but does not
/// parse is refused, never replaced.
fn append_project(home: &Path, canonical: &Path) -> anyhow::Result<()> {
    use ainb_app::config::{lock, read_existing, write_atomic};

    let file = home.join(".agents-in-a-box").join("config").join(PROJECTS_FILE);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = lock::lock_for(&file)?;
    let existing = read_existing(&file)?;
    let mut table = if existing.trim().is_empty() {
        toml::Table::new()
    } else {
        existing
            .parse::<toml::Table>()
            .map_err(|_| anyhow::anyhow!("{} does not parse; fix or remove it", file.display()))?
    };
    let mut projects = table
        .get("projects")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let listed = projects
        .iter()
        .filter_map(toml::Value::as_str)
        .filter_map(|entry| std::fs::canonicalize(expand_home(home, entry)).ok())
        .any(|entry| entry == canonical);
    if listed {
        return Ok(());
    }
    projects.push(toml::Value::String(canonical.display().to_string()));
    table.insert("projects".into(), toml::Value::Array(projects));
    write_atomic(
        &file,
        &format!("{PROJECTS_HEADER}{}", toml::to_string(&table)?),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        let ok = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .is_ok_and(|out| out.status.success());
        assert!(ok, "git init {}", path.display());
    }

    fn write_config(home: &Path, file: &str, body: &str) {
        let config = home.join(".agents-in-a-box/config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join(file), body).unwrap();
    }

    #[test]
    fn no_registered_folder_lists_nothing() {
        let home = tempfile::tempdir().unwrap();
        git_repo(&home.path().join("code/app"));
        assert!(list(home.path(), &WorkspaceDefaults::default()).is_empty());
    }

    #[test]
    fn repositories_under_both_kinds_of_root_are_listed_without_a_session() {
        let home = tempfile::tempdir().unwrap();
        git_repo(&home.path().join("code/web"));
        git_repo(&home.path().join("code/group/api"));
        git_repo(&home.path().join("elsewhere/cli"));
        git_repo(&home.path().join("unregistered/secret"));
        std::fs::create_dir_all(home.path().join("code/not-a-repo")).unwrap();
        write_config(
            home.path(),
            "config.toml",
            "[workspace_defaults]\nworkspace_scan_paths = [\"~/code\", \"/does/not/exist\"]\n",
        );
        write_config(
            home.path(),
            "onboarding.toml",
            &format!(
                "git_directories = [\"{}\"]\n",
                home.path().join("elsewhere").display()
            ),
        );

        let listed = list(home.path(), &WorkspaceDefaults::default());
        let canonical =
            |rel: &str| std::fs::canonicalize(home.path().join(rel)).unwrap().display().to_string();
        assert_eq!(
            listed,
            vec![
                RegisteredProject {
                    name: "api".into(),
                    path: canonical("code/group/api")
                },
                RegisteredProject {
                    name: "cli".into(),
                    path: canonical("elsewhere/cli")
                },
                RegisteredProject {
                    name: "web".into(),
                    path: canonical("code/web")
                },
            ]
        );
    }

    #[test]
    fn a_registered_repository_lists_itself() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("app");
        git_repo(&repo);
        write_config(
            home.path(),
            "config.toml",
            &format!(
                "[workspace_defaults]\nworkspace_scan_paths = [\"{}\"]\n",
                repo.display()
            ),
        );
        let listed = list(home.path(), &WorkspaceDefaults::default());
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].name, "app");
    }

    #[test]
    fn a_symlink_out_of_every_root_is_not_offered() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        git_repo(&outside.path().join("escaped"));
        std::fs::create_dir_all(home.path().join("code")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("escaped"),
            home.path().join("code/escaped"),
        )
        .unwrap();
        write_config(
            home.path(),
            "config.toml",
            "[workspace_defaults]\nworkspace_scan_paths = [\"~/code\"]\n",
        );
        assert!(list(home.path(), &WorkspaceDefaults::default()).is_empty());
    }

    fn projects_file(home: &Path) -> PathBuf {
        home.join(".agents-in-a-box/config").join(PROJECTS_FILE)
    }

    #[test]
    fn a_picked_repository_is_registered_exactly_and_then_listed() {
        let home = tempfile::tempdir().unwrap();
        let config = "# my notes\n[workspace_defaults]\nworkspace_scan_paths = [\"~/code\"]\n";
        write_config(home.path(), "config.toml", config);
        let repo = home.path().join("elsewhere/app");
        git_repo(&repo);
        git_repo(&repo.join("vendor/lib"));

        let project = register(home.path(), &repo).expect("registered");
        let canonical = std::fs::canonicalize(&repo).unwrap();
        assert_eq!(project, RegisteredProject::at(&canonical));
        assert_eq!(
            list(home.path(), &WorkspaceDefaults::default()),
            [project],
            "the project itself, not the repository nested in it"
        );
        assert_eq!(registered_projects(home.path()), [canonical]);
        let config_after =
            std::fs::read_to_string(home.path().join(".agents-in-a-box/config/config.toml"))
                .unwrap();
        assert_eq!(config_after, config, "config.toml is not touched");

        // Again: already registered, nothing appended.
        let text = std::fs::read_to_string(projects_file(home.path())).unwrap();
        register(home.path(), &repo).expect("still registered");
        assert_eq!(
            std::fs::read_to_string(projects_file(home.path())).unwrap(),
            text
        );
    }

    /// A pick inside a root is recorded too: the scan that lists root
    /// repositories stops at `scan_max_depth`, so a deep one would otherwise
    /// be accepted and never listed.
    #[test]
    fn a_deep_repository_inside_a_root_is_recorded_and_listed() {
        let home = tempfile::tempdir().unwrap();
        let deep = home.path().join("code/a/b/c/d/deep");
        git_repo(&deep);
        write_config(
            home.path(),
            "onboarding.toml",
            &format!(
                "git_directories = [\"{}\"]\n",
                home.path().join("code").display()
            ),
        );
        assert!(
            list(home.path(), &WorkspaceDefaults::default()).is_empty(),
            "past the scan depth"
        );

        let project = register(home.path(), &deep).expect("registered");
        assert_eq!(list(home.path(), &WorkspaceDefaults::default()), [project]);
        let canonical = std::fs::canonicalize(&deep).unwrap();
        assert_eq!(registered_projects(home.path()), [canonical]);
    }

    #[test]
    fn added_projects_are_never_cut_by_max_repositories() {
        let home = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            git_repo(&home.path().join("code").join(name));
        }
        let added = home.path().join("elsewhere/aa");
        git_repo(&added);
        write_config(
            home.path(),
            "config.toml",
            "[workspace_defaults]\nworkspace_scan_paths = [\"~/code\"]\n",
        );
        register(home.path(), &added).expect("registered");
        let defaults = WorkspaceDefaults {
            max_repositories: 1,
            ..WorkspaceDefaults::default()
        };
        let names: Vec<String> =
            list(home.path(), &defaults).into_iter().map(|project| project.name).collect();
        assert_eq!(
            names,
            ["a", "aa"],
            "the scan is cut, the added project is not"
        );
    }

    #[test]
    fn a_project_added_twice_is_recorded_once() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("app");
        git_repo(&repo);
        register(home.path(), &repo).expect("registered");
        // Spelled differently, same repository.
        register(home.path(), &home.path().join("app/")).expect("again");
        assert_eq!(registered_projects(home.path()).len(), 1);
    }

    #[test]
    fn only_a_repository_top_may_be_registered() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("code/app");
        git_repo(&repo);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(home.path().join("plain")).unwrap();

        for (folder, why) in [
            (
                home.path().join("code"),
                "not the top folder of a git repository",
            ),
            (repo.join("src"), "not the top folder of a git repository"),
            (
                home.path().join("plain"),
                "not the top folder of a git repository",
            ),
            (home.path().join("missing"), "not a folder"),
            (home.path().to_path_buf(), "holds your home folder"),
            (PathBuf::from("/"), "holds your home folder"),
        ] {
            let err = register(home.path(), &folder).expect_err("refused");
            assert!(err.contains(why), "{}: {err}", folder.display());
        }
        let missing_home = home.path().join("no-such-home");
        let err = register(&missing_home, &repo).expect_err("no home");
        assert!(err.contains("home folder cannot be resolved"), "{err}");
        assert!(
            !projects_file(home.path()).exists(),
            "a refusal writes nothing"
        );
    }

    #[test]
    fn a_projects_file_that_does_not_parse_is_refused_not_replaced() {
        let home = tempfile::tempdir().unwrap();
        write_config(home.path(), PROJECTS_FILE, "projects = [\"/a\"");
        let repo = home.path().join("app");
        git_repo(&repo);
        let err = register(home.path(), &repo).expect_err("refused");
        assert!(err.contains("does not parse"), "{err}");
        assert_eq!(
            std::fs::read_to_string(projects_file(home.path())).unwrap(),
            "projects = [\"/a\""
        );
    }

    #[test]
    fn the_list_is_capped_at_max_repositories() {
        let home = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            git_repo(&home.path().join("code").join(name));
        }
        write_config(
            home.path(),
            "config.toml",
            "[workspace_defaults]\nworkspace_scan_paths = [\"~/code\"]\n",
        );
        let defaults = WorkspaceDefaults {
            max_repositories: 2,
            ..WorkspaceDefaults::default()
        };
        let names: Vec<String> =
            list(home.path(), &defaults).into_iter().map(|project| project.name).collect();
        assert_eq!(names, ["a", "b"]);
    }
}
