//! Where an answer is typed: the agent's own pane, named stably.
//!
//! Discovery names a tmux SESSION, and keys sent to a session land in its
//! active pane, which is whichever pane the person last used once the session
//! is split. A fleet row records the pane the agent was observed in two
//! ways: an index target (`session:window.pane`), which tmux renumbers when a
//! lower pane closes, and a fingerprint carrying the pane's id (`%N`), which
//! never changes for the pane's life. The id is what an answer is sent to.

use tokio::process::Command;

/// What a fleet row knows about the agent's pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneHint {
    /// The index target the row was observed at (`session:window.pane`).
    pub target: Option<String>,
    /// The row's process-start fingerprint (`pane=%N;pid=P;...`).
    pub fingerprint: Option<String>,
}

/// The pane id (`%N`) a fingerprint carries, or `None` for a fingerprint of
/// another shape.
#[must_use]
pub fn pane_id_of(fingerprint: &str) -> Option<&str> {
    fingerprint.split(';').find_map(|part| part.strip_prefix("pane=")).filter(|id| {
        id.starts_with('%') && id.len() > 1 && id[1..].bytes().all(|b| b.is_ascii_digit())
    })
}

/// The one pane a session with no recorded pane may still be typed into:
/// exactly one listed. Two or more, and the answer would go to whichever is
/// active, which is refused.
///
/// # Errors
///
/// Why nothing may be typed: no panes, or more than one.
pub fn only_pane<'a>(session: &str, panes: &'a [String]) -> Result<&'a str, String> {
    match panes {
        [] => Err(format!("no pane is open in {session}")),
        [one] => Ok(one.as_str()),
        many => Err(format!(
            "the row names no pane and {session} has {} panes: refusing to type into whichever is active",
            many.len()
        )),
    }
}

async fn display(target: &str, format: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "-t", target, format])
        .output()
        .await
        .ok()?;
    // tmux answers a pane id nothing runs under with an empty line and
    // success: empty is absent.
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The pane `answer` is sent to in `session`, as a stable id (`%N`):
///
/// 1. the fingerprint's pane id, checked to be alive and in `session`;
/// 2. else the index target's pane id, read now;
/// 3. else the session's only pane.
///
/// # Errors
///
/// Why nothing may be typed: the agent's pane is gone, or the session has
/// several panes and the row names none.
pub async fn resolve_send_target(session: &str, hint: &PaneHint) -> Result<String, String> {
    if let Some(id) = hint.fingerprint.as_deref().and_then(pane_id_of) {
        return match display(id, "#{session_name}").await {
            Some(owner) if owner == session => Ok(id.to_string()),
            Some(owner) => Err(format!(
                "the agent's pane {id} is now in {owner}, not {session}"
            )),
            None => Err(format!("the agent's pane {id} is gone from {session}")),
        };
    }
    if let Some(target) = hint.target.as_deref() {
        return match display(target, "#{pane_id}").await {
            Some(id) => Ok(id),
            None => Err(format!("the pane {target} the row names is gone")),
        };
    }
    let output = Command::new("tmux")
        .args(["list-panes", "-t", session, "-F", "#{pane_id}"])
        .output()
        .await
        .map_err(|error| format!("tmux could not list {session}: {error}"))?;
    if !output.status.success() {
        return Err(format!("no tmux session {session} to type into"));
    }
    let panes: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    only_pane(session, &panes).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pane_id_is_read_off_the_fingerprint_and_nothing_else() {
        assert_eq!(pane_id_of("pane=%12;pid=4;session_started=9"), Some("%12"));
        assert_eq!(pane_id_of("pid=4;pane=%0"), Some("%0"));
        assert_eq!(pane_id_of("pane=12;pid=4"), None, "no percent sign");
        assert_eq!(pane_id_of("pane=%;pid=4"), None, "no number");
        assert_eq!(pane_id_of("legacy:claude:dev:1.0:x"), None);
        assert_eq!(pane_id_of(""), None);
    }

    #[test]
    fn a_session_with_no_recorded_pane_is_typed_into_only_when_it_has_one() {
        let one = vec!["%3".to_string()];
        assert_eq!(only_pane("dev", &one), Ok("%3"));
        let two = vec!["%3".to_string(), "%4".to_string()];
        let refused = only_pane("dev", &two).unwrap_err();
        assert!(
            refused.contains("2 panes") && refused.contains("refusing"),
            "{refused}"
        );
        assert!(only_pane("dev", &[]).unwrap_err().contains("no pane"));
    }
}
