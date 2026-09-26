//! Where an answer is typed: the agent's own pane, named stably.
//!
//! Discovery names a tmux SESSION, and keys sent to a session land in its
//! active pane, which is whichever pane the person last used once the session
//! is split. A fleet row records the pane the agent was observed in two
//! ways: an index target (`session:window.pane`), which tmux renumbers when a
//! lower pane closes, and a fingerprint carrying the pane's id (`%N`), the pid
//! of what ran in it and when its tmux session was created, which together
//! name one process in one pane for its life. The id is what an answer is
//! sent to, once the pid and the session say the same process is still
//! there: a pane ainb respawns keeps its id and gets a new pid, and a tmux
//! server restarted from scratch hands out `%0` again.

use tokio::process::Command;

/// What a fleet row knows about the agent's pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneHint {
    /// The index target the row was observed at (`session:window.pane`).
    /// Named in a refusal, never typed into: an index is renumbered, and
    /// tmux matches a bare session name by prefix.
    pub target: Option<String>,
    /// The row's process-start fingerprint (`pane=%N;pid=P;session_started=S`).
    pub fingerprint: Option<String>,
}

/// Whether `value` is a tmux pane id: `%` and digits, nothing else.
#[must_use]
pub fn is_pane_id(value: &str) -> bool {
    value
        .strip_prefix('%')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn field<'a>(fingerprint: &'a str, name: &str) -> Option<&'a str> {
    fingerprint
        .split(';')
        .find_map(|part| part.strip_prefix(name).and_then(|rest| rest.strip_prefix('=')))
}

/// The pane id (`%N`) a fingerprint carries, or `None` for a fingerprint of
/// another shape.
#[must_use]
pub fn pane_id_of(fingerprint: &str) -> Option<&str> {
    field(fingerprint, "pane").filter(|id| is_pane_id(id))
}

/// The pid a fingerprint carries (`pid=P`): what ran in the pane when the row
/// was raised.
#[must_use]
pub fn pane_pid_of(fingerprint: &str) -> Option<u32> {
    field(fingerprint, "pid").and_then(|pid| pid.parse().ok())
}

/// When the pane's tmux session was created, as the fingerprint carries it
/// (`session_started=S`, tmux's `#{session_created}`).
#[must_use]
pub fn session_started_of(fingerprint: &str) -> Option<&str> {
    field(fingerprint, "session_started").filter(|value| !value.is_empty())
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

/// One tmux format read off `target`, or `None` when tmux has no such target.
/// tmux answers a missing pane id with success and nothing, so an empty
/// answer is no answer.
async fn display(target: &str, format: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "-t", target, format])
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The session the pane `id` is in now, or `None` when the pane is gone.
pub async fn pane_session(id: &str) -> Option<String> {
    display(id, "#{session_name}").await
}

/// The pid of what runs in the pane `id` now, or `None` when the pane is gone.
pub async fn pane_pid(id: &str) -> Option<u32> {
    display(id, "#{pane_pid}").await?.parse().ok()
}

/// The pane an answer for `session` is typed into, as a pane id.
///
/// The fingerprint's pane id, when it is still in `session`, still runs the
/// pid the fingerprint carries, and its session was created when the
/// fingerprint says; else the session's only pane, across every window. An
/// index target is never typed into: tmux renumbers it and matches a bare
/// session name by prefix, so what it names now is not what the row meant.
///
/// `Ok(None)` when tmux has no session by that name at all: there is nothing
/// to narrow, and whether the send is refused or goes to a broker peer is
/// the route's call, not this one's.
///
/// # Errors
///
/// Why nothing may be typed: the agent's pane is gone, moved to another
/// session, or runs another process now; or nothing is recorded and the
/// session has no pane or more than one.
pub async fn resolve_send_pane(session: &str, hint: &PaneHint) -> Result<Option<String>, String> {
    let named = hint
        .fingerprint
        .as_deref()
        .and_then(|fingerprint| pane_id_of(fingerprint).map(|id| (fingerprint, id)));
    if let Some((fingerprint, id)) = named {
        let Some(owner) = pane_session(id).await else {
            return Err(format!("the agent's pane {id} is gone from {session}"));
        };
        if owner != session {
            return Err(format!(
                "the agent's pane {id} is now in {owner}, not {session}"
            ));
        }
        if let Some(started) = session_started_of(fingerprint) {
            match display(id, "#{session_created}").await {
                Some(now) if now == started => {}
                Some(_) => {
                    return Err(format!(
                        "the agent's pane {id} belongs to a {session} created since the row was raised: the agent is gone"
                    ));
                }
                None => return Err(format!("the agent's pane {id} is gone from {session}")),
            }
        }
        if let Some(raised_with) = pane_pid_of(fingerprint) {
            match pane_pid(id).await {
                Some(now) if now == raised_with => {}
                Some(now) => {
                    return Err(format!(
                        "the agent that asked is gone from {id} in {session}: another process (pid {now}) runs there now"
                    ));
                }
                None => return Err(format!("the agent's pane {id} is gone from {session}")),
            }
        }
        return Ok(Some(id.to_string()));
    }
    // Every pane of the session, in every window, the session matched
    // exactly: tmux would otherwise take `dev` for `devbox` and list the
    // current window alone.
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-s",
            "-t",
            &format!("={session}"),
            "-F",
            "#{pane_id}",
        ])
        .output()
        .await
        .map_err(|error| format!("tmux could not list {session}: {error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let panes: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    only_pane(session, &panes).map(|id| Some(id.to_string())).map_err(|reason| {
        match hint.target.as_deref() {
            Some(target) => format!(
                "{reason} (the row names {target}, an index tmux renumbers, which is not trusted)"
            ),
            None => reason,
        }
    })
}

/// [`resolve_send_pane`], with a session tmux has no name for refused here;
/// kept until every caller has moved.
///
/// # Errors
///
/// As [`resolve_send_pane`], plus the missing session.
pub async fn resolve_send_target(session: &str, hint: &PaneHint) -> Result<String, String> {
    resolve_send_pane(session, hint)
        .await?
        .ok_or_else(|| format!("no tmux session {session} to type into"))
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
    fn a_pane_id_is_a_percent_sign_and_digits() {
        assert!(is_pane_id("%0"));
        assert!(is_pane_id("%5853"));
        assert!(!is_pane_id("%"));
        assert!(!is_pane_id("5"));
        assert!(!is_pane_id("%5a"));
        assert!(!is_pane_id(" %5"));
        assert!(!is_pane_id(""));
    }

    #[test]
    fn the_pid_and_the_session_start_are_read_off_the_fingerprint_when_carried() {
        assert_eq!(pane_pid_of("pane=%12;pid=4;session_started=9"), Some(4));
        assert_eq!(
            session_started_of("pane=%12;pid=4;session_started=9"),
            Some("9")
        );
        assert_eq!(pane_pid_of("pane=%12"), None);
        assert_eq!(session_started_of("pane=%12;session_started="), None);
        assert_eq!(pane_pid_of("pane=%12;pid=x"), None);
        assert_eq!(pane_pid_of(""), None);
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
