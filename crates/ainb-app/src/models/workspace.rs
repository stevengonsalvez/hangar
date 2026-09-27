// ABOUTME: Workspace data model representing a git repository that can contain multiple sessions

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{Session, ShellSession};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct Workspace {
    pub name: String,
    pub path: PathBuf,
    pub sessions: Vec<Session>,
    /// Single shell session per workspace (used for quick directory switching)
    #[serde(default)]
    pub shell_session: Option<ShellSession>,
    // Legacy field for migration - will be removed in future
    #[serde(default, skip_serializing)]
    shell_sessions: Vec<ShellSession>,
}

impl Workspace {
    pub fn new(name: String, path: PathBuf) -> Self {
        Self {
            name,
            path,
            sessions: Vec::new(),
            shell_session: None,
            shell_sessions: Vec::new(),
        }
    }

    pub fn add_session(&mut self, session: Session) {
        self.sessions.push(session);
    }

    pub fn remove_session(&mut self, session_id: &uuid::Uuid) -> bool {
        let initial_len = self.sessions.len();
        self.sessions.retain(|s| &s.id != session_id);
        self.sessions.len() != initial_len
    }

    pub fn get_session_mut(&mut self, session_id: &uuid::Uuid) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| &s.id == session_id)
    }

    pub fn get_session(&self, session_id: &uuid::Uuid) -> Option<&Session> {
        self.sessions.iter().find(|s| &s.id == session_id)
    }

    pub fn running_sessions(&self) -> Vec<&Session> {
        self.sessions.iter().filter(|s| s.status.is_running()).collect()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    // Shell session methods (single shell per workspace)

    /// Set the workspace shell session
    pub fn set_shell_session(&mut self, shell_session: ShellSession) {
        self.shell_session = Some(shell_session);
    }

    /// Clear the workspace shell session
    pub fn clear_shell_session(&mut self) {
        self.shell_session = None;
    }

    /// Get mutable reference to shell session
    pub fn get_shell_session_mut(&mut self) -> Option<&mut ShellSession> {
        self.shell_session.as_mut()
    }

    /// Get reference to shell session
    pub fn get_shell_session(&self) -> Option<&ShellSession> {
        self.shell_session.as_ref()
    }

    /// Check if shell session exists and is running
    pub fn has_running_shell(&self) -> bool {
        self.shell_session.as_ref().map(|s| s.status.is_running()).unwrap_or(false)
    }

    /// Total count of all sessions (AI + shell)
    pub fn total_session_count(&self) -> usize {
        self.sessions.len() + if self.shell_session.is_some() { 1 } else { 0 }
    }

    /// Migrate legacy shell_sessions to single shell_session (if any)
    pub fn migrate_legacy_shells(&mut self) {
        if self.shell_session.is_none() && !self.shell_sessions.is_empty() {
            // Take the most recently accessed shell session
            if let Some(shell) = self.shell_sessions.iter().max_by_key(|s| s.last_accessed).cloned()
            {
                self.shell_session = Some(shell);
            }
        }
        self.shell_sessions.clear();
    }
}

impl Workspace {
    /// Whether a scan that rebuilt this workspace would have found nothing
    /// new: the same shell row and the same sessions, by
    /// [`Session::same_scan_fields`].
    #[must_use]
    pub fn same_scan_fields(&self, other: &Self) -> bool {
        self.name == other.name
            && self.path == other.path
            && self.shell_session == other.shell_session
            && super::session::same_scan_rows(&self.sessions, &other.sessions)
    }
}

/// Whether a scan found the same workspaces it is holding, in the same order.
#[must_use]
pub fn same_scan_workspaces(held: &[Workspace], found: &[Workspace]) -> bool {
    held.len() == found.len()
        && held.iter().zip(found).all(|(held, found)| held.same_scan_fields(found))
}

/// Carry the host's fields from the rows of `held` onto the rows of `found`
/// with the same session id, wherever the scan now lists them
/// ([`super::session::carry_host_rows`]).
pub fn carry_host_rows(held: &[Workspace], found: &mut [Workspace]) {
    let previous: Vec<Session> =
        held.iter().flat_map(|workspace| workspace.sessions.iter().cloned()).collect();
    for workspace in found {
        super::session::carry_host_rows(&previous, &mut workspace.sessions);
    }
}

/// Put every workspace's sessions in the one order every surface shows and
/// steps through: worktrees newest first, a worktree's sessions kept together
/// (newest first within it), and the session id to break a tie, so the order
/// never depends on how the scan happened to find the rows.
///
/// The scan finds rows in `tmux list-sessions` order, which is alphabetical by
/// session name. The desktop drew that order while the palette's "next
/// session" walked it; drawing anything else made "next" jump around the
/// sidebar or go nowhere. Sorting here, where the rows are found, gives the
/// TUI, the desktop and every step command the same list (Orca's recent-first
/// default).
///
/// A worktree is placed by its NEWEST session, so starting a second agent in
/// an old worktree brings the whole worktree up, not just the new row.
/// Creation, not access time: opening a session must never move its row out
/// from under the pointer that opened it.
pub fn sort_sessions_recent_first(workspaces: &mut [Workspace]) {
    use std::cmp::Reverse;
    use std::collections::HashMap;

    for workspace in workspaces {
        let mut newest: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
        for session in &workspace.sessions {
            newest
                .entry(session.workspace_path.clone())
                .and_modify(|at| *at = (*at).max(session.created_at))
                .or_insert(session.created_at);
        }
        workspace.sessions.sort_by_key(|session| {
            (
                Reverse(newest[&session.workspace_path]),
                session.workspace_path.clone(),
                Reverse(session.created_at),
                session.id,
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn session(name: &str, worktree: &str, created_s: i64) -> Session {
        let mut session = Session::new(name.to_string(), worktree.to_string());
        session.created_at = Utc.timestamp_opt(created_s, 0).unwrap();
        session
    }

    fn names(workspace: &Workspace) -> Vec<&str> {
        workspace.sessions.iter().map(|session| session.name.as_str()).collect()
    }

    #[test]
    fn worktrees_come_newest_first_whatever_order_the_scan_found() {
        // Found alphabetically, as `tmux list-sessions` lists them.
        let mut workspace = Workspace::new("repo".into(), "/repo".into());
        workspace.sessions = vec![
            session("a-oldest", "/wt/a", 100),
            session("b-newest", "/wt/b", 300),
            session("c-middle", "/wt/c", 200),
        ];
        let mut workspaces = vec![workspace];
        sort_sessions_recent_first(&mut workspaces);
        assert_eq!(names(&workspaces[0]), ["b-newest", "c-middle", "a-oldest"]);
    }

    #[test]
    fn a_worktrees_sessions_stay_together_placed_by_its_newest() {
        // `/wt/old` was made first, but a second agent started in it last:
        // the whole worktree comes up, its two rows adjacent, newest first.
        let mut workspace = Workspace::new("repo".into(), "/repo".into());
        workspace.sessions = vec![
            session("old-first-agent", "/wt/old", 100),
            session("mid", "/wt/mid", 200),
            session("old-second-agent", "/wt/old", 300),
        ];
        let mut workspaces = vec![workspace];
        sort_sessions_recent_first(&mut workspaces);
        assert_eq!(
            names(&workspaces[0]),
            ["old-second-agent", "old-first-agent", "mid"]
        );
    }

    #[test]
    fn the_order_is_the_same_however_the_rows_arrive() {
        // Two worktrees created in the same second: the tie falls to the
        // worktree path, then the id, never to the scan's own order.
        let one = session("one", "/wt/x", 100);
        let two = session("two", "/wt/y", 100);
        let mut forward = vec![Workspace::new("repo".into(), "/repo".into())];
        forward[0].sessions = vec![one.clone(), two.clone()];
        let mut backward = vec![Workspace::new("repo".into(), "/repo".into())];
        backward[0].sessions = vec![two, one];
        sort_sessions_recent_first(&mut forward);
        sort_sessions_recent_first(&mut backward);
        assert_eq!(names(&forward[0]), names(&backward[0]));
    }
}
