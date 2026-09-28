//! The projects the composer offers: every git repository inside a folder the
//! daemon's `worktree/create` accepts, whether or not it has a session yet.
//!
//! ```text
//!  ~/.agents-in-a-box/config/config.toml      workspace_defaults.workspace_scan_paths ─┐
//!  ~/.agents-in-a-box/config/onboarding.toml  git_directories ─────────────────────────┤
//!                                                                    registered roots ◀┘
//!                          repositories found under them, canonical ──▶ Project select
//! ```
//!
//! The roots are the daemon's own `spawn::registered_roots`, called rather
//! than copied: the user-level files only, never a project's own
//! `.ainb/config.toml`, so the list names exactly the repositories a create is
//! accepted from. The daemon stays the gate; this is only what the window
//! offers. `tests/create_worktree.rs` creates from a listed project against
//! the real daemon.

use std::path::Path;

use ainb_app::config::WorkspaceDefaults;
use ainb_app::git::WorkspaceScanner;
use ainb_hangar_daemon::spawn::registered_roots;
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

/// The repositories under `home`'s registered roots, by name, at most
/// `defaults.max_repositories` of them. Scanned the way the TUI's repository
/// picker scans (depth and excludes from `defaults`), but never through its
/// cache, which is keyed on the TUI's own wider set of folders. A repository
/// whose canonical path leaves every root (a symlink out) is not offered:
/// the daemon would refuse it.
#[must_use]
pub fn list(home: &Path, defaults: &WorkspaceDefaults) -> Vec<RegisteredProject> {
    let roots = registered_roots(home);
    if roots.is_empty() {
        return Vec::new();
    }
    let scanned = WorkspaceScanner::new()
        .with_search_paths(roots.clone())
        .with_workspace_defaults(defaults)
        .scan_uncached()
        .map(|result| result.workspaces)
        .unwrap_or_default();
    let mut projects: Vec<RegisteredProject> = Vec::new();
    for workspace in scanned {
        let Ok(canonical) = std::fs::canonicalize(&workspace.path) else {
            continue;
        };
        if !roots.iter().any(|root| canonical.starts_with(root)) {
            continue;
        }
        let path = canonical.display().to_string();
        if projects.iter().any(|project| project.path == path) {
            continue;
        }
        projects.push(RegisteredProject {
            name: workspace.name,
            path,
        });
    }
    projects.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    projects.truncate(defaults.max_repositories);
    projects
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
