//! Rename a sidebar row: the operator's own label for a session, and nothing
//! else. The branch, the worktree folder and the tmux session keep their names.
//!
//! The label's owner is the one the terminal's label popup writes
//! (`AppState::confirm_session_label_rename`): the durable label store, keyed
//! by tmux session name and persisted to `session-labels.json`, which a
//! relaunch loads. The window names a session and the new name; the name is
//! checked here, never trusted from the page.
//!
//! The store is read from disk for each rename, not taken from the one this
//! window loaded at launch: the terminal writes the same file, so the names
//! other rows show are the ones on disk. The write is the store's one write,
//! [`SessionLabelStore::set_label`]: this session's label, set on the file as
//! it stands under the file's lock. It lands before the rename returns, so a
//! write the store refuses (a label file that does not parse, left as it is)
//! is the field's refusal and nothing is framed.
//!
//! ```text
//!  field ──session_rename(id, name)──▶ checked_name ──▶ set_label ──▶ row
//!                                                         └─▶ refusal: field
//! ```

use ainb_app::AppState;
use ainb_app::config::{SessionLabelStore, normalize_session_label};
use ainb_app::models::Session;
use uuid::Uuid;

const EMPTY: &str = "A name cannot be empty.";
const NOT_LISTED: &str = "This session is not in the session list, so it cannot be renamed.";
const NO_TMUX: &str = "This session has no tmux session to keep a name under.";
const INVISIBLE: &str = "A name cannot contain invisible formatting characters.";
const UNREADABLE: &str = "The saved names file (session-labels.json) cannot be read, so this \
     name was not saved and the file was left as it is.";
const NOT_SAVED: &str = "This name could not be saved to the saved names file.";

/// The id the window sent, or the sentence the field shows when that is not
/// one. The sentence does not repeat what was sent: it came from the page.
///
/// # Errors
/// `raw` is not a UUID.
pub fn session_id(raw: &str) -> Result<Uuid, String> {
    Uuid::parse_str(raw)
        .map_err(|_| "The window sent something that is not a session id.".to_string())
}

/// `raw` as the name to keep, or why it is refused. `taken` are the names the
/// other rows in the same project show.
///
/// Trimmed; empty, a control character or more than 64 characters is refused
/// by the label store's own rule ([`normalize_session_label`]), so the window
/// and the terminal keep the same names. The terminal reads an empty label as
/// "clear it"; the window refuses it instead, so a slip of the keyboard never
/// wipes a name. A format character (a zero-width space, a bidi override) is
/// refused too: the sidebar draws a name without them, so `api\u{200B}` would
/// pass the duplicate check and still read as a second `api`.
///
/// # Errors
/// The sentence the rename field shows.
pub fn checked_name<'a>(
    raw: &str,
    mut taken: impl Iterator<Item = &'a str>,
) -> Result<String, String> {
    let name = normalize_session_label(raw)?.ok_or_else(|| EMPTY.to_string())?;
    if name.chars().any(crate::intent::is_format) {
        return Err(INVISIBLE.to_string());
    }
    if taken.any(|shown| shown == name) {
        return Err(format!(
            "Another worktree in this project is already named \"{name}\"."
        ));
    }
    Ok(name)
}

/// The name a row shows: its label, else the name it was loaded with, else
/// its own name. The same order the sidebar draws a card title in.
fn shown<'a>(labels: &'a SessionLabelStore, session: &'a Session) -> &'a str {
    session
        .tmux_session_name
        .as_deref()
        .and_then(|tmux| labels.get(tmux))
        .or(session.display_name.as_ref())
        .unwrap_or(&session.name)
}

/// Give `session` the display name `raw`, if [`checked_name`] allows it
/// against the other rows of its project, and write it to the label store.
/// Returns the name kept.
///
/// # Errors
/// The sentence the rename field shows: the session is not listed, it has no
/// tmux session to key the label by, the name is refused, or the store
/// refused the write. Nothing is written or framed then.
pub fn rename(state: &mut AppState, session: Uuid, raw: &str) -> Result<String, String> {
    let labels = SessionLabelStore::load();
    // Read through `Deref`: a write through the section bumps its version, and
    // a refused rename must not reframe anything.
    let (at, tmux, name) = {
        let workspaces = &state.sessions.workspaces;
        let (w, s) = workspaces
            .iter()
            .enumerate()
            .find_map(|(w, workspace)| {
                let s = workspace.sessions.iter().position(|row| row.id == session)?;
                Some((w, s))
            })
            .ok_or_else(|| NOT_LISTED.to_string())?;
        let project = &workspaces[w].sessions;
        let tmux = project[s].tmux_session_name.clone().ok_or_else(|| NO_TMUX.to_string())?;
        let others = project.iter().filter(|row| row.id != session).map(|row| shown(&labels, row));
        ((w, s), tmux, checked_name(raw, others)?)
    };
    let labels = SessionLabelStore::set_label(&tmux, Some(name.clone())).map_err(|error| {
        // The error may quote a label out of the file, and neither the log
        // nor the field repeats a label: the field gets the host's sentence.
        tracing::warn!(%session, kind = ?error.kind(), "session label not saved");
        if error.kind() == std::io::ErrorKind::InvalidData {
            UNREADABLE.to_string()
        } else {
            NOT_SAVED.to_string()
        }
    })?;
    state.sessions.workspaces[at.0].sessions[at.1].display_name = Some(name.clone());
    state.session_labels.session_label_store = labels;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::checked_name;

    fn check(raw: &str, taken: &[&str]) -> Result<String, String> {
        checked_name(raw, taken.iter().copied())
    }

    #[test]
    fn a_name_is_kept_trimmed() {
        assert_eq!(check("  Fix login  ", &[]), Ok("Fix login".to_string()));
    }

    #[test]
    fn an_empty_or_blank_name_is_refused() {
        assert_eq!(check("", &[]), Err("A name cannot be empty.".to_string()));
        assert_eq!(
            check(" \t ", &[]),
            Err("A name cannot be empty.".to_string())
        );
    }

    #[test]
    fn a_control_character_is_refused() {
        for raw in ["a\nb", "a\u{1b}[31mb", "a\u{7}b"] {
            let refusal = check(raw, &[]).expect_err(raw);
            assert!(refusal.contains("control characters"), "{refusal}");
        }
    }

    #[test]
    fn a_name_is_capped_at_64_characters() {
        let longest = "é".repeat(64);
        assert_eq!(check(&longest, &[]), Ok(longest.clone()));
        let refusal = check(&format!("{longest}x"), &[]).expect_err("65 characters");
        assert!(refusal.contains("64 characters"), "{refusal}");
    }

    #[test]
    fn an_invisible_formatting_character_is_refused() {
        for raw in ["api\u{200B}", "\u{202E}ipa", "a\u{FEFF}b"] {
            assert_eq!(
                check(raw, &[]),
                Err("A name cannot contain invisible formatting characters.".to_string()),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn a_name_another_row_shows_is_refused() {
        assert_eq!(
            check(" api ", &["web", "api"]),
            Err("Another worktree in this project is already named \"api\".".to_string())
        );
        // Only an exact match: a name that differs is another name.
        assert_eq!(check("API", &["api"]), Ok("API".to_string()));
    }
}
