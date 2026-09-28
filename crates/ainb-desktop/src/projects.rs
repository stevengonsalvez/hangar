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
    if projects.len() > defaults.max_repositories {
        tracing::info!(
            found = projects.len(),
            shown = defaults.max_repositories,
            "project list cut at workspace_defaults.max_repositories"
        );
        projects.truncate(defaults.max_repositories);
    }
    projects
}

/// The config key a registered folder is added to.
const SCAN_PATHS: &str = "workspace_defaults.workspace_scan_paths";

/// Register `folder`, a repository a person picked in the native folder
/// dialog, so the daemon creates from it: its canonical path is appended to
/// the user-level `workspace_scan_paths`, comments and every other key left
/// as they are. Only a repository's own top folder is accepted, never a
/// folder of many, and never the home folder or one above it: the picker
/// registers exactly one project, and cannot widen what the daemon trusts to
/// everything a person owns. A repository already inside a registered folder
/// is returned as it is, with nothing written.
///
/// # Errors
/// The sentence the composer shows: not a folder, not a repository's top, the
/// home folder, or a config file that cannot be read or written.
pub fn register(home: &Path, folder: &Path) -> Result<RegisteredProject, String> {
    let shown = folder.display();
    let canonical = std::fs::canonicalize(folder)
        .ok()
        .filter(|path| path.is_dir())
        .ok_or_else(|| format!("{shown} is not a folder on this machine."))?;
    if std::fs::canonicalize(home).is_ok_and(|home| home.starts_with(&canonical)) {
        return Err(format!(
            "{shown} holds your home folder: pick one repository's own folder."
        ));
    }
    if repository_top(&canonical).as_deref() != Some(canonical.as_path()) {
        return Err(format!(
            "{shown} is not the top folder of a git repository: pick the repository's own folder."
        ));
    }
    let project = RegisteredProject {
        name: canonical
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        path: canonical.display().to_string(),
    };
    if registered_roots(home).iter().any(|root| canonical.starts_with(root)) {
        return Ok(project);
    }
    append_scan_path(home, &project.path)
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
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| std::fs::canonicalize(String::from_utf8_lossy(&out.stdout).trim()).ok())
}

/// Append `path` to the user-level scan paths under the config file's lock,
/// read inside the lock so a concurrent settings save is not lost. A file
/// that exists but does not parse is refused, never replaced.
fn append_scan_path(home: &Path, path: &str) -> anyhow::Result<()> {
    use ainb_app::config::{lock, read_existing, write_keys_into_with_lock};

    let config = home.join(".agents-in-a-box").join("config").join("config.toml");
    if let Some(parent) = config.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock = lock::lock_for(&config)?;
    let existing = read_existing(&config)?;
    let table = if existing.trim().is_empty() {
        toml::Table::new()
    } else {
        existing.parse::<toml::Table>().map_err(|_| {
            anyhow::anyhow!(
                "{} does not parse; fix it with `ainb config edit`",
                config.display()
            )
        })?
    };
    let mut paths = table
        .get("workspace_defaults")
        .and_then(|defaults| defaults.get("workspace_scan_paths"))
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    paths.push(toml::Value::String(path.to_string()));
    write_keys_into_with_lock(
        &config,
        &[(SCAN_PATHS.to_string(), toml::Value::Array(paths))],
        &lock,
    )
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
    fn a_picked_repository_is_registered_and_then_listed() {
        let home = tempfile::tempdir().unwrap();
        write_config(
            home.path(),
            "config.toml",
            "# my notes\n[workspace_defaults]\n# keep me\nworkspace_scan_paths = [\"~/code\"]\nbranch_prefix = \"me/\"\n",
        );
        let repo = home.path().join("elsewhere/app");
        git_repo(&repo);

        let project = register(home.path(), &repo).expect("registered");
        let canonical = std::fs::canonicalize(&repo).unwrap().display().to_string();
        assert_eq!(
            project,
            RegisteredProject {
                name: "app".into(),
                path: canonical.clone()
            }
        );
        assert_eq!(list(home.path(), &WorkspaceDefaults::default()), [project]);

        let text = std::fs::read_to_string(home.path().join(".agents-in-a-box/config/config.toml"))
            .unwrap();
        assert!(
            text.contains("# my notes") && text.contains("# keep me"),
            "{text}"
        );
        assert!(text.contains("branch_prefix = \"me/\""), "{text}");
        assert!(
            text.contains(&format!("[\"~/code\", \"{canonical}\"]")),
            "{text}"
        );

        // Again: already registered, nothing appended.
        register(home.path(), &repo).expect("still registered");
        let again =
            std::fs::read_to_string(home.path().join(".agents-in-a-box/config/config.toml"))
                .unwrap();
        assert_eq!(again, text);
    }

    #[test]
    fn registering_with_no_config_file_creates_it() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("app");
        git_repo(&repo);
        register(home.path(), &repo).expect("registered");
        assert_eq!(
            registered_roots(home.path()),
            [std::fs::canonicalize(&repo).unwrap()]
        );
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
        assert!(
            !home.path().join(".agents-in-a-box/config/config.toml").exists(),
            "a refusal writes nothing"
        );
    }

    #[test]
    fn a_config_that_does_not_parse_is_refused_not_replaced() {
        let home = tempfile::tempdir().unwrap();
        write_config(home.path(), "config.toml", "[workspace_defaults\nbroken");
        let repo = home.path().join("app");
        git_repo(&repo);
        let err = register(home.path(), &repo).expect_err("refused");
        assert!(err.contains("does not parse"), "{err}");
        let text = std::fs::read_to_string(home.path().join(".agents-in-a-box/config/config.toml"))
            .unwrap();
        assert_eq!(text, "[workspace_defaults\nbroken");
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
