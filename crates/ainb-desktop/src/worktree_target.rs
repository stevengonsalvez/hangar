//! Which worktree a press on the tab strip's "+" means, named by ids the host
//! already holds, and its folder as the host resolves it.
//!
//! The page names a listed session or an open shell tab, never a path. The
//! host reads the folder from its own session list or from the shell tab it
//! opened, so a page cannot aim a terminal or an agent anywhere else.
//!
//! ```text
//!  {kind: session, id}  ──host's session list──▶ that session's worktree
//!  {kind: shell,   key} ──host's shell tab─────▶ the folder its shell runs in
//! ```

use serde::Deserialize;

/// A worktree, as the page may name one.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub enum WorktreeTarget {
    /// The worktree of a session in the host's session list.
    Session {
        /// The session's id.
        id: String,
    },
    /// The folder of a shell tab the host opened (`shell_open`).
    Shell {
        /// The shell tab's key.
        key: String,
    },
}

impl WorktreeTarget {
    /// The folder this target names, read through the host's own lookups:
    /// `session_worktree` for a listed session's path, `shell_dir` for an
    /// open shell tab's folder. `None` from either is "not the host's".
    ///
    /// # Errors
    /// A sentence for a toast: an id or a key the host does not hold, or a
    /// row whose path is not a folder on this host (an orphaned row's is a
    /// note, not a path).
    pub fn resolve(
        &self,
        session_worktree: impl FnOnce(uuid::Uuid) -> Option<String>,
        shell_dir: impl FnOnce(&str) -> Option<String>,
    ) -> Result<String, String> {
        let path = match self {
            Self::Session { id } => uuid::Uuid::parse_str(id)
                .ok()
                .and_then(session_worktree)
                .ok_or_else(|| {
                    "That session is no longer in the list: pick a session in the sidebar, then try again."
                        .to_string()
                })?,
            Self::Shell { key } => shell_dir(key).ok_or_else(|| {
                "That terminal tab is no longer open: pick a tab or a session, then try again."
                    .to_string()
            })?,
        };
        let path = path.trim();
        if !path.starts_with('/') {
            return Err("That session has no worktree on this host.".into());
        }
        Ok(path.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "11111111-1111-4111-8111-111111111111";

    fn listed(id: uuid::Uuid) -> Option<String> {
        (id.to_string() == ID).then(|| "/wt/app-feat".to_string())
    }

    fn shells(key: &str) -> Option<String> {
        (key == "ainb-dsh-0123abcd").then(|| "/wt/app-shell".to_string())
    }

    fn session(id: &str) -> WorktreeTarget {
        WorktreeTarget::Session { id: id.into() }
    }

    #[test]
    fn a_listed_session_resolves_to_its_worktree() {
        assert_eq!(
            session(ID).resolve(listed, shells).as_deref(),
            Ok("/wt/app-feat")
        );
    }

    #[test]
    fn a_shell_tab_resolves_to_its_folder() {
        let tab = WorktreeTarget::Shell {
            key: "ainb-dsh-0123abcd".into(),
        };
        assert_eq!(tab.resolve(listed, shells).as_deref(), Ok("/wt/app-shell"));
        let gone = WorktreeTarget::Shell {
            key: "tmux_other".into(),
        };
        assert!(gone.resolve(listed, shells).unwrap_err().contains("no longer open"));
    }

    #[test]
    fn what_the_host_does_not_hold_is_refused() {
        let unknown = session("22222222-2222-4222-8222-222222222222");
        assert!(unknown.resolve(listed, shells).unwrap_err().contains("no longer in the list"));
        assert!(
            session("/etc").resolve(listed, shells).is_err(),
            "a path is not an id"
        );
        let orphan = session(ID).resolve(|_| Some("Missing worktree for session x".into()), shells);
        assert!(orphan.unwrap_err().contains("no worktree on this host"));
    }

    #[test]
    fn the_page_names_ids_only() {
        let raw = serde_json::json!({ "kind": "session", "id": ID, "worktree_path": "/etc" });
        assert!(serde_json::from_value::<WorktreeTarget>(raw).is_err());
        let raw = serde_json::json!({ "kind": "path", "path": "/etc" });
        assert!(serde_json::from_value::<WorktreeTarget>(raw).is_err());
        let raw = serde_json::json!({ "kind": "shell", "key": "ainb-dsh-0123abcd" });
        assert!(matches!(
            serde_json::from_value::<WorktreeTarget>(raw),
            Ok(WorktreeTarget::Shell { .. })
        ));
    }
}
