// ABOUTME: CLI status and kill commands for session management
//
// status: Show detailed session information (text/JSON output)
// kill: Terminate a session and its tmux, with confirmation prompt

use anyhow::{Context, Result};
use serde::Serialize;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

use super::util::{find_session, load_session_store, mutate_session_store};
use super::{KillArgs, OutputFormat, StatusArgs};
use crate::tmux::ClaudeProcessDetector;

/// JSON output structure for status command
#[derive(Debug, Serialize)]
pub struct StatusOutput {
    pub session_id: String,
    pub workspace_name: String,
    pub tmux_session_name: String,
    pub worktree_path: String,
    pub created_at: String,
    pub is_running: bool,
    pub claude_active: bool,
}

/// Check if a tmux session exists
fn tmux_session_exists(tmux_session_name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", &format!("={tmux_session_name}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Execute the status command
///
/// Shows detailed information about a session including:
/// - Session ID, workspace name
/// - Tmux session status
/// - Worktree path
/// - Creation time
/// - Whether Claude CLI is active
#[allow(clippy::unused_async)]
pub async fn execute(args: StatusArgs, format: OutputFormat) -> Result<()> {
    let session = find_session(&args.session)?;

    // Check tmux session status
    let is_running = tmux_session_exists(&session.tmux_session_name);

    // Check if Claude is active in the session
    let claude_active = if is_running {
        let detector = ClaudeProcessDetector::new();
        detector.is_claude_running(&session.tmux_session_name).unwrap_or(false)
    } else {
        false
    };

    // Re-derived from the worktree path, exactly as the TUI and `ainb list`
    // do — never the persisted `workspace_name`, which is the `--repo`
    // basename recorded at creation time. See
    // `SessionMetadata::display_workspace_name`.
    let workspace_name = session.display_workspace_name();

    match format {
        OutputFormat::Json => {
            let output = StatusOutput {
                session_id: session.session_id.to_string(),
                workspace_name: workspace_name.clone(),
                tmux_session_name: session.tmux_session_name.clone(),
                worktree_path: session.worktree_path.display().to_string(),
                created_at: session.created_at.to_rfc3339(),
                is_running,
                claude_active,
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&output).context("Failed to serialize status")?
            );
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            let status_text = if is_running {
                if claude_active {
                    "\x1b[32m●\x1b[0m Running (Claude active)"
                } else {
                    "\x1b[33m●\x1b[0m Running (shell)"
                }
            } else {
                "\x1b[31m●\x1b[0m Stopped"
            };

            let short_id = &session.session_id.to_string()[..8];

            println!("Session: {}", session.session_id);
            println!("{}", "━".repeat(44));
            println!("Workspace:    {workspace_name}");
            println!("Status:       {status_text}");
            println!("Tmux:         {}", session.tmux_session_name);
            println!("Worktree:     {}", session.worktree_path.display());
            println!(
                "Created:      {}",
                session.created_at.format("%Y-%m-%d %H:%M:%S UTC")
            );
            println!();
            println!("Commands:");
            println!("  Attach:     ainb attach {short_id}");
            println!("  Logs:       ainb logs {short_id} --follow");
            println!("  Kill:       ainb kill {short_id}");
        }
    }

    Ok(())
}

/// Execute the kill command
///
/// Terminates a session by:
/// 1. Finding the session by ID or name
/// 2. Prompting for confirmation (unless --force)
/// 3. Killing the tmux session
/// 4. Removing the session from `SessionStore`
#[allow(clippy::unused_async, clippy::if_not_else)]
pub async fn kill(args: KillArgs) -> Result<()> {
    let session = find_session(&args.session)?;
    // Same re-derived name the user just saw in `ainb list` / the TUI, so the
    // confirmation prompt names the session the way they addressed it.
    let workspace_name = session.display_workspace_name();

    // Check if session is running
    let is_running = tmux_session_exists(&session.tmux_session_name);

    if !is_running {
        println!("Session '{workspace_name}' is not running (tmux session not found).");
        println!("Removing from session store...");

        // Still remove from store (locked RMW: pu4)
        mutate_session_store(|store| store.remove_by_session_id(session.session_id))
            .context("Failed to save session store")?;

        println!("Session removed.");
        return Ok(());
    }

    // Prompt for confirmation unless --force
    if !args.force {
        print!("Kill session '{workspace_name}'? [y/N] ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Kill tmux session
    println!("Killing tmux session '{}'...", session.tmux_session_name);

    let output = Command::new("tmux")
        // Exact target: a bare `-t` resolves exact, then prefix, so this could
        // otherwise kill a different, live session whose name starts the same.
        .args([
            "kill-session",
            "-t",
            &format!("={}", session.tmux_session_name),
        ])
        .output()
        .context("Failed to execute tmux kill-session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Warning: tmux kill-session failed: {}", stderr.trim());
        // Continue anyway - might already be dead
    } else {
        println!("Tmux session killed.");
    }

    // Remove from session store (locked RMW: pu4)
    mutate_session_store(|store| store.remove_by_session_id(session.session_id))
        .context("Failed to save session store")?;

    println!("Session '{workspace_name}' removed.");

    // The worktree is never removed here. The removal hint is only for a
    // tree ainb made that no other session is in: `rm -rf` on a shared tree
    // pulls it out from under that session, and on a session run in the
    // repository itself it names the user's own checkout.
    let kept = match (crate::git::WorktreeManager::new(), load_session_store()) {
        (Ok(manager), Ok(store)) => {
            kept_tree(&manager, &store, &session.worktree_path, session.session_id)
        }
        _ => KeptTree::Other,
    };
    println!("\n{}", kept_tree_note(&session.worktree_path, &kept));

    Ok(())
}

/// What `ainb kill` may say about the worktree it leaves behind.
#[derive(Debug, PartialEq, Eq)]
enum KeptTree {
    /// A tree ainb made that no other session uses: safe to remove.
    Removable,
    /// A tree ainb made that another session still works in.
    Shared,
    /// Not a tree ainb made (a session run in the repository itself), or
    /// one whose users could not be read: never offered for removal.
    Other,
}

/// Which [`KeptTree`] `tree` is once `killed`'s row is gone, by the one
/// check every path that may remove a session's tree asks
/// ([`crate::interactive::session_manager::tree_in_use_by_another`]).
fn kept_tree(
    manager: &crate::git::WorktreeManager,
    store: &crate::interactive::session_manager::SessionStore,
    tree: &Path,
    killed: uuid::Uuid,
) -> KeptTree {
    if manager.managed_tree(tree).is_none() {
        return KeptTree::Other;
    }
    if crate::interactive::session_manager::tree_in_use_by_another(
        manager,
        store,
        tree,
        Some(killed),
    ) {
        KeptTree::Shared
    } else {
        KeptTree::Removable
    }
}

/// The note `ainb kill` prints about the worktree it leaves behind.
fn kept_tree_note(tree: &Path, kept: &KeptTree) -> String {
    let tree = tree.display();
    match kept {
        KeptTree::Removable => {
            format!("Note: Worktree at '{tree}' was not removed.\nTo clean up, run: rm -rf {tree}")
        }
        KeptTree::Shared => {
            format!("Note: Worktree at '{tree}' was not removed: another session still uses it.")
        }
        KeptTree::Other => format!("Note: Worktree at '{tree}' was not removed."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interactive::session_manager::SessionMetadata;
    use crate::models::session::SessionAgentType;
    use chrono::Utc;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn create_test_session(id: Uuid, workspace: &str, tmux_name: &str) -> SessionMetadata {
        SessionMetadata {
            session_id: id,
            tmux_session_name: tmux_name.to_string(),
            worktree_path: PathBuf::from(format!("/tmp/test-worktree-{}", id)),
            workspace_name: workspace.to_string(),
            created_at: Utc::now(),
            agent_type: SessionAgentType::default(),
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
            claude_session_id: None,
        }
    }

    /// Only a tree ainb made that no other session uses gets the `rm -rf`
    /// hint.
    #[test]
    fn the_removal_hint_is_only_for_a_removable_tree() {
        let tree = Path::new("/w/app--feat--1a2b3c4d");
        let removable = kept_tree_note(tree, &KeptTree::Removable);
        assert!(
            removable.ends_with("To clean up, run: rm -rf /w/app--feat--1a2b3c4d"),
            "{removable}"
        );
        let shared = kept_tree_note(tree, &KeptTree::Shared);
        assert!(!shared.contains("rm -rf"), "{shared}");
        assert!(shared.contains("another session still uses it"), "{shared}");
        let other = kept_tree_note(tree, &KeptTree::Other);
        assert!(!other.contains("rm -rf"), "{other}");
    }

    /// The killed session's own `by-session` link does not count as a user;
    /// another session's row or link does; a folder ainb did not make is
    /// never offered for removal.
    #[cfg(unix)]
    #[test]
    fn a_tree_is_removable_only_when_ainb_made_it_and_no_one_else_uses_it() {
        use crate::interactive::session_manager::SessionStore;
        let root = tempfile::tempdir().unwrap();
        let manager =
            crate::git::WorktreeManager::with_base_dir(root.path().join("worktrees")).unwrap();
        let by_name = root.path().join("worktrees/by-name");
        let by_session = root.path().join("worktrees/by-session");
        std::fs::create_dir_all(&by_session).unwrap();
        let tree = by_name.join("app--feat--1a2b3c4d");
        std::fs::create_dir_all(&tree).unwrap();
        let (killed, other) = (Uuid::new_v4(), Uuid::new_v4());
        std::os::unix::fs::symlink(&tree, by_session.join(killed.to_string())).unwrap();

        let empty = SessionStore::default();
        assert_eq!(
            kept_tree(&manager, &empty, &tree, killed),
            KeptTree::Removable
        );

        let mut joined = SessionStore::default();
        let mut row = create_test_session(other, "app", "ainb-joined");
        row.worktree_path = tree.clone();
        joined.upsert(row);
        assert_eq!(
            kept_tree(&manager, &joined, &tree, killed),
            KeptTree::Shared
        );

        std::os::unix::fs::symlink(&tree, by_session.join(other.to_string())).unwrap();
        assert_eq!(kept_tree(&manager, &empty, &tree, killed), KeptTree::Shared);

        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert_eq!(kept_tree(&manager, &empty, &repo, killed), KeptTree::Other);
    }

    #[test]
    fn test_find_session_by_full_uuid() {
        // This test would require mocking SessionStore::load()
        // For now, we verify the logic flow with a unit test of the search
        let id = Uuid::new_v4();
        let id_str = id.to_string();

        // Verify UUID parsing works
        let parsed = Uuid::parse_str(&id_str);
        assert!(parsed.is_ok());
        assert_eq!(parsed.unwrap(), id);
    }

    #[test]
    fn test_find_session_by_prefix() {
        let id = Uuid::new_v4();
        let full_id = id.to_string();
        let prefix = &full_id[..8];

        // Verify prefix matching logic
        assert!(full_id.to_lowercase().starts_with(&prefix.to_lowercase()));
    }

    #[test]
    fn test_status_output_json_serialization() {
        let output = StatusOutput {
            session_id: "f79e07da-774d-415c-aedf-a2acd0bee0d3".to_string(),
            workspace_name: "my-workspace".to_string(),
            tmux_session_name: "tmux_my-session".to_string(),
            worktree_path: "/path/to/worktree".to_string(),
            created_at: "2026-01-17T18:25:46Z".to_string(),
            is_running: true,
            claude_active: true,
        };

        let json = serde_json::to_string_pretty(&output);
        assert!(json.is_ok());

        let json_str = json.unwrap();
        assert!(json_str.contains("session_id"));
        assert!(json_str.contains("workspace_name"));
        assert!(json_str.contains("is_running"));
        assert!(json_str.contains("claude_active"));
    }

    #[test]
    fn test_session_metadata_clone() {
        let id = Uuid::new_v4();
        let session = create_test_session(id, "test-workspace", "tmux_test");

        let cloned = session.clone();
        assert_eq!(cloned.session_id, id);
        assert_eq!(cloned.workspace_name, "test-workspace");
        assert_eq!(cloned.tmux_session_name, "tmux_test");
    }
}
