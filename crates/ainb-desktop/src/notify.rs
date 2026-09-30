//! The OS notification a session's move into Needs or Done raises, and when it
//! is held back, as Orca's agent notifications behave.
//!
//! The host reads section 20 (the board's cards) on every tick that moved it
//! and hands each card's phase to a [`Notifier`], which remembers the last one
//! per session. [`decide`] is the whole rule, pure so it is tested without a
//! window:
//!
//! ```text
//!  card ──Phase::of──▶ previous vs next ──moved?──▶ enabled? ──▶ focused? ──▶ Notice
//!                         (dedupe)                  (Settings)   (shown session
//!                                                                 in a focused window)
//! ```
//!
//! - **Needs** fires when a session starts waiting on a person, or, still
//!   waiting, is asked a new request (two known fingerprints that differ).
//! - **Done** fires when a session seen working completes its turn: idle with
//!   `turn_complete`, the board's own Done. Not from Needs: an answer closes
//!   the request, and the row falls back to the turn that asked, still
//!   complete, before the agent works again (`agent_status::state_of`).
//! - The same phase read again never fires, and neither does the first read
//!   of a session: a launch does not announce everything already waiting. A
//!   read that knows nothing (`unverifiable`) keeps the phase before it.
//! - Off in Settings, nothing fires. The session whose terminal (or ACP
//!   transcript) the work area shows is not announced while the window has
//!   focus: the person is already looking at it. An unfocused window does
//!   not hold anything back.
//! - A held-back move is still the move: it does not fire later when the
//!   window loses focus or the setting comes back on.
//! - One session fires at most once per [`COOLDOWN_MS`], Orca's burst
//!   cooldown, so a phase that flaps is one banner and not a stream.
//! - [`BURST`] or more moves in one read (a wake from sleep, a reconnect) are
//!   one summary banner, "N sessions need you", not a stack of them.

use std::collections::HashMap;
use std::path::Path;

use ainb_hangar_proto::agent_status::{AgentState, WaitKind};
use ainb_hangar_proto::status_view::AgentCard;

/// The file under the hangar home that holds the host's copy of the Settings
/// toggle, as `theme.rs` keeps the theme's.
pub const NOTIFICATIONS_FILE: &str = "desktop-notifications";

/// The shortest gap between two notifications for one session: Orca's
/// `NOTIFICATION_COOLDOWN_MS`.
pub const COOLDOWN_MS: i64 = 5_000;

/// How many notices from one read become one summary notice.
pub const BURST: usize = 3;

/// What a session is doing, as far as notifications care.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Running a turn.
    Working,
    /// Blocked on a person: on what kind of input, and the request's
    /// fingerprint, so a second question reads as a new move.
    Needs {
        kind: Option<WaitKind>,
        request: Option<String>,
    },
    /// Free: with its last turn completed, or not (just started, interrupted).
    Idle { turn_complete: bool },
    /// The process is gone.
    Exited,
    /// Nothing has said: the phase before it stands.
    Unknown,
}

impl Phase {
    /// The phase a card's state reads as.
    #[must_use]
    pub fn of(
        state: AgentState,
        kind: Option<WaitKind>,
        request: Option<&str>,
        turn_complete: bool,
    ) -> Self {
        match state {
            AgentState::Working => Self::Working,
            AgentState::Waiting => Self::Needs {
                kind,
                request: request.map(str::to_string),
            },
            AgentState::Idle => Self::Idle { turn_complete },
            AgentState::Exited => Self::Exited,
            AgentState::Unverifiable => Self::Unknown,
        }
    }
}

/// One session as a read shows it: its board key, its name, its phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// `provider:session-id`, the card's key, which the page's focus names.
    pub key: String,
    /// The name a notification shows (`label`).
    pub label: String,
    pub phase: Phase,
}

impl Session {
    /// The session `card` describes.
    #[must_use]
    pub fn of(card: &AgentCard) -> Self {
        let status = &card.status;
        Self {
            key: status.session_key.clone(),
            label: label(
                status.display_name.as_deref(),
                &status.cwd,
                &status.session_key,
            ),
            phase: Phase::of(
                status.state,
                status.wait_kind,
                card.session.current_request_fingerprint.as_deref(),
                status.turn_complete,
            ),
        }
    }
}

/// The session's display name, else the last part of its working directory
/// (the repository, or the worktree named for its branch), else its key.
#[must_use]
pub fn label(display_name: Option<&str>, cwd: &str, key: &str) -> String {
    let named = display_name.map(str::trim).filter(|name| !name.is_empty());
    let folder = Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty());
    named.or(folder).unwrap_or(key).to_string()
}

/// What holds a notification back, as the page and the window last said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gate {
    /// The Settings toggle.
    pub enabled: bool,
    /// The card key of the session the work area shows, if any.
    pub focused_session: Option<String>,
    /// Whether the window has the OS's focus.
    pub window_focused: bool,
}

/// What a notification announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Needs,
    Done,
    /// Several sessions moved in one read ([`BURST`]).
    Summary,
}

/// One notification to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    /// The session a click opens; `None` for a summary, whose click only
    /// brings the window forward.
    pub session_key: Option<String>,
    pub title: String,
    pub body: String,
}

/// The move between two phases that is worth a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Move {
    Needs(Option<WaitKind>),
    Done,
}

fn moved(previous: &Phase, next: &Phase) -> Option<Move> {
    match (previous, next) {
        (
            Phase::Needs {
                request: Some(was), ..
            },
            Phase::Needs {
                request: Some(now),
                kind,
            },
        ) if was != now => Some(Move::Needs(*kind)),
        // The same request as its evidence fills in (a fingerprint, a kind).
        (Phase::Needs { .. }, Phase::Needs { .. }) => None,
        (_, Phase::Needs { kind, .. }) => Some(Move::Needs(*kind)),
        (
            Phase::Working
            | Phase::Idle {
                turn_complete: false,
            },
            Phase::Idle {
                turn_complete: true,
            },
        ) => Some(Move::Done),
        _ => None,
    }
}

/// Whether `next` is worth a notification after `previous` (`None`: the first
/// read of this session), under `gate`, and what it says.
#[must_use]
pub fn decide(previous: Option<&Phase>, next: &Session, gate: &Gate) -> Option<Notice> {
    let movement = moved(previous?, &next.phase)?;
    if !gate.enabled {
        return None;
    }
    if gate.window_focused && gate.focused_session.as_deref() == Some(next.key.as_str()) {
        return None;
    }
    let (kind, body) = match movement {
        Move::Needs(wait) => (
            NoticeKind::Needs,
            format!("Needs you: {}", needs_what(wait)),
        ),
        Move::Done => (NoticeKind::Done, "Done: finished its turn".to_string()),
    };
    Some(Notice {
        kind,
        session_key: Some(next.key.clone()),
        title: next.label.clone(),
        body,
    })
}

/// `notices` as they are shown: as they are, below [`BURST`]; one summary
/// naming how many need you and how many are done, and who, at or above it.
#[must_use]
pub fn coalesce(notices: Vec<Notice>) -> Vec<Notice> {
    if notices.len() < BURST {
        return notices;
    }
    let needs = notices.iter().filter(|notice| notice.kind == NoticeKind::Needs).count();
    let done = notices.len() - needs;
    let title = match (needs, done) {
        (_, 0) => format!("{needs} sessions need you"),
        (0, _) => format!("{done} sessions are done"),
        _ => format!(
            "{needs} {} you, {done} done",
            if needs == 1 {
                "session needs"
            } else {
                "sessions need"
            }
        ),
    };
    const NAMED: usize = 3;
    let mut body = notices
        .iter()
        .take(NAMED)
        .map(|notice| notice.title.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if notices.len() > NAMED {
        body.push_str(&format!(" and {} more", notices.len() - NAMED));
    }
    vec![Notice {
        kind: NoticeKind::Summary,
        session_key: None,
        title,
        body,
    }]
}

fn needs_what(kind: Option<WaitKind>) -> &'static str {
    match kind {
        Some(WaitKind::Ask) => "it asked a question",
        Some(WaitKind::Approval) => "it wants an approval",
        Some(WaitKind::Error) => "it stopped on an error",
        Some(WaitKind::Waiting) | None => "it is waiting for input",
    }
}

/// Whether a session that last fired at `fired_at` is still cooling at
/// `now_ms`. A clock stepped back past it is not: a session is never held
/// silent until the wall clock catches up.
fn cooling(fired_at: i64, now_ms: i64) -> bool {
    (0..COOLDOWN_MS).contains(&(now_ms - fired_at))
}

/// The last phase of every session a read showed, and when each last fired.
#[derive(Debug, Default)]
pub struct Notifier {
    phases: HashMap<String, Phase>,
    fired_at: HashMap<String, i64>,
}

impl Notifier {
    /// Fold one read of every session at `now_ms` and return what to show,
    /// a burst as one summary ([`coalesce`]). Each session in a summary
    /// still starts its own cooldown. A session the read leaves out is forgotten: back later, it is a first
    /// read again.
    pub fn observe(
        &mut self,
        sessions: impl IntoIterator<Item = Session>,
        gate: &Gate,
        now_ms: i64,
    ) -> Vec<Notice> {
        let mut notices = Vec::new();
        let mut phases = HashMap::with_capacity(self.phases.len());
        for session in sessions {
            let previous = self.phases.get(&session.key);
            if let Some(notice) = decide(previous, &session, gate) {
                let held = self.fired_at.get(&session.key).is_some_and(|at| cooling(*at, now_ms));
                if !held {
                    self.fired_at.insert(session.key.clone(), now_ms);
                    notices.push(notice);
                }
            }
            let phase = match (session.phase, previous) {
                (Phase::Unknown, Some(previous)) => previous.clone(),
                (phase, _) => phase,
            };
            phases.insert(session.key, phase);
        }
        self.phases = phases;
        self.fired_at.retain(|_, at| cooling(*at, now_ms));
        coalesce(notices)
    }
}

/// The stored toggle: on unless the file says `off`, the page's own default.
#[must_use]
pub fn load(path: &Path) -> bool {
    std::fs::read_to_string(path).map_or(true, |word| word.trim() != "off")
}

/// Store the toggle at `path`, whole or not at all (`theme::store_word`).
///
/// # Errors
///
/// When the directory cannot be created or the file cannot be written.
pub fn store(path: &Path, enabled: bool) -> std::io::Result<()> {
    crate::theme::store_word(path, if enabled { "on" } else { "off" })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "claude:abc";

    const DONE: Phase = Phase::Idle {
        turn_complete: true,
    };
    const FREE: Phase = Phase::Idle {
        turn_complete: false,
    };

    fn session(phase: Phase) -> Session {
        Session {
            key: KEY.to_string(),
            label: "hangar".to_string(),
            phase,
        }
    }

    fn needs(kind: WaitKind, request: &str) -> Phase {
        Phase::Needs {
            kind: Some(kind),
            request: Some(request.to_string()),
        }
    }

    fn on() -> Gate {
        Gate {
            enabled: true,
            focused_session: None,
            window_focused: false,
        }
    }

    #[test]
    fn a_session_that_starts_waiting_fires_needs_naming_it() {
        let notice = decide(
            Some(&Phase::Working),
            &session(needs(WaitKind::Ask, "q1")),
            &on(),
        )
        .expect("Working to Needs fires");
        assert_eq!(notice.kind, NoticeKind::Needs);
        assert_eq!(notice.session_key.as_deref(), Some(KEY));
        assert_eq!(notice.title, "hangar");
        assert_eq!(notice.body, "Needs you: it asked a question");
        for previous in [DONE, FREE, Phase::Exited, Phase::Unknown] {
            assert!(
                decide(
                    Some(&previous),
                    &session(needs(WaitKind::Approval, "a1")),
                    &on()
                )
                .is_some(),
                "{previous:?} to Needs fires too"
            );
        }
    }

    #[test]
    fn a_session_seen_working_that_completes_its_turn_fires_done() {
        let notice =
            decide(Some(&Phase::Working), &session(DONE), &on()).expect("Working to Done fires");
        assert_eq!(notice.title, "hangar");
        assert_eq!(notice.body, "Done: finished its turn");
        assert!(
            decide(Some(&FREE), &session(DONE), &on()).is_some(),
            "a completed turn read after the idle that led to it"
        );
        assert_eq!(
            decide(Some(&Phase::Working), &session(FREE), &on()),
            None,
            "idle with no completed turn (interrupted) is not the board's Done"
        );
        assert_eq!(
            decide(Some(&needs(WaitKind::Ask, "q1")), &session(DONE), &on()),
            None,
            "an answer falls back to the complete turn that asked: no Done"
        );
    }

    #[test]
    fn the_same_phase_read_again_does_not_fire() {
        let waiting = needs(WaitKind::Ask, "q1");
        for phase in [
            Phase::Working,
            waiting,
            DONE,
            FREE,
            Phase::Exited,
            Phase::Unknown,
        ] {
            assert_eq!(
                decide(Some(&phase), &session(phase.clone()), &on()),
                None,
                "{phase:?} read again"
            );
        }
        for previous in [Phase::Exited, Phase::Unknown, DONE] {
            assert_eq!(
                decide(Some(&previous), &session(DONE), &on()),
                None,
                "{previous:?} to Done: no turn seen running"
            );
        }
    }

    #[test]
    fn a_new_request_while_still_waiting_is_a_new_move_and_its_evidence_filling_in_is_not() {
        assert!(
            decide(
                Some(&needs(WaitKind::Approval, "a1")),
                &session(needs(WaitKind::Approval, "a2")),
                &on()
            )
            .is_some(),
            "another request"
        );
        let unfingerprinted = Phase::Needs {
            kind: Some(WaitKind::Waiting),
            request: None,
        };
        assert_eq!(
            decide(
                Some(&unfingerprinted),
                &session(needs(WaitKind::Ask, "q1")),
                &on()
            ),
            None,
            "the same request's fingerprint and kind arriving"
        );
        assert_eq!(
            decide(
                Some(&needs(WaitKind::Waiting, "q1")),
                &session(needs(WaitKind::Ask, "q1")),
                &on()
            ),
            None,
            "the same request, its kind refined"
        );
    }

    #[test]
    fn the_first_read_of_a_session_never_fires() {
        assert_eq!(
            decide(None, &session(needs(WaitKind::Ask, "q1")), &on()),
            None
        );
        assert_eq!(decide(None, &session(DONE), &on()), None);
    }

    #[test]
    fn the_toggle_off_holds_every_move_back() {
        let off = Gate {
            enabled: false,
            ..on()
        };
        assert_eq!(
            decide(
                Some(&Phase::Working),
                &session(needs(WaitKind::Ask, "q1")),
                &off
            ),
            None
        );
        assert_eq!(decide(Some(&Phase::Working), &session(DONE), &off), None);
    }

    #[test]
    fn the_session_shown_in_a_focused_window_is_not_announced() {
        let looking = Gate {
            enabled: true,
            focused_session: Some(KEY.to_string()),
            window_focused: true,
        };
        assert_eq!(
            decide(Some(&Phase::Working), &session(DONE), &looking),
            None
        );
        let other = Gate {
            focused_session: Some("claude:other".to_string()),
            ..looking
        };
        assert!(
            decide(Some(&Phase::Working), &session(DONE), &other).is_some(),
            "another session shown does not hold this one back"
        );
    }

    #[test]
    fn an_unfocused_window_does_not_hold_the_shown_session_back() {
        let away = Gate {
            enabled: true,
            focused_session: Some(KEY.to_string()),
            window_focused: false,
        };
        assert!(decide(Some(&Phase::Working), &session(DONE), &away).is_some());
    }

    #[test]
    fn the_notifier_fires_once_per_move_across_repeated_reads() {
        let mut notifier = Notifier::default();
        let read = |phase: Phase| vec![session(phase)];
        assert!(notifier.observe(read(Phase::Working), &on(), 0).is_empty());
        assert_eq!(notifier.observe(read(DONE), &on(), 10_000).len(), 1, "Done");
        for poll in 1..=5 {
            assert!(
                notifier.observe(read(DONE), &on(), 10_000 + poll * 500).is_empty(),
                "re-poll {poll} of the same phase"
            );
        }
        assert!(notifier.observe(read(Phase::Working), &on(), 20_000).is_empty());
        assert_eq!(
            notifier.observe(read(needs(WaitKind::Ask, "q1")), &on(), 30_000).len(),
            1,
            "a new move fires again"
        );
    }

    #[test]
    fn a_read_that_knows_nothing_keeps_the_phase_before_it() {
        let mut notifier = Notifier::default();
        notifier.observe(vec![session(Phase::Working)], &on(), 0);
        assert_eq!(
            notifier.observe(vec![session(needs(WaitKind::Ask, "q1"))], &on(), 10_000).len(),
            1
        );
        notifier.observe(vec![session(Phase::Unknown)], &on(), 20_000);
        assert!(
            notifier
                .observe(vec![session(needs(WaitKind::Ask, "q1"))], &on(), 30_000)
                .is_empty(),
            "the same question, back from silence, is not asked again"
        );

        notifier.observe(vec![session(Phase::Working)], &on(), 40_000);
        notifier.observe(vec![session(Phase::Unknown)], &on(), 50_000);
        assert_eq!(
            notifier.observe(vec![session(DONE)], &on(), 60_000).len(),
            1,
            "a turn seen running, then silence, then complete, is Done"
        );
    }

    #[test]
    fn a_move_held_back_is_spent_not_deferred() {
        let mut notifier = Notifier::default();
        let looking = Gate {
            enabled: true,
            focused_session: Some(KEY.to_string()),
            window_focused: true,
        };
        notifier.observe(vec![session(Phase::Working)], &looking, 0);
        assert!(notifier.observe(vec![session(DONE)], &looking, 10_000).is_empty());
        assert!(
            notifier.observe(vec![session(DONE)], &on(), 11_000).is_empty(),
            "leaving the window does not announce what was already seen"
        );
    }

    #[test]
    fn one_session_fires_at_most_once_per_cooldown() {
        let mut notifier = Notifier::default();
        notifier.observe(vec![session(Phase::Working)], &on(), 0);
        assert_eq!(
            notifier.observe(vec![session(needs(WaitKind::Ask, "q1"))], &on(), 1_000).len(),
            1
        );
        notifier.observe(vec![session(Phase::Working)], &on(), 2_000);
        assert!(
            notifier.observe(vec![session(DONE)], &on(), 1_000 + COOLDOWN_MS - 1).is_empty(),
            "a flap inside the cooldown"
        );
        notifier.observe(vec![session(Phase::Working)], &on(), 20_000);
        assert_eq!(
            notifier.observe(vec![session(DONE)], &on(), 21_000).len(),
            1,
            "past it, the next move fires"
        );
    }

    #[test]
    fn a_clock_stepped_back_does_not_silence_a_session() {
        let mut notifier = Notifier::default();
        notifier.observe(vec![session(Phase::Working)], &on(), 100_000);
        assert_eq!(
            notifier.observe(vec![session(DONE)], &on(), 100_000).len(),
            1
        );
        notifier.observe(vec![session(Phase::Working)], &on(), 40_000);
        assert_eq!(
            notifier.observe(vec![session(DONE)], &on(), 40_000).len(),
            1,
            "the wall clock went back a minute"
        );
    }

    fn named(key: &str, phase: Phase) -> Session {
        Session {
            key: key.to_string(),
            label: key.to_string(),
            phase,
        }
    }

    #[test]
    fn ten_sessions_moving_in_one_read_are_one_summary() {
        let mut notifier = Notifier::default();
        let keys: Vec<String> = (0..10).map(|n| format!("s{n}")).collect();
        let read =
            |phase: &Phase| keys.iter().map(|key| named(key, phase.clone())).collect::<Vec<_>>();
        assert!(notifier.observe(read(&Phase::Working), &on(), 0).is_empty());
        let notices = notifier.observe(read(&needs(WaitKind::Ask, "q1")), &on(), 10_000);
        assert_eq!(notices.len(), 1, "one banner for the burst: {notices:?}");
        let summary = &notices[0];
        assert_eq!(summary.kind, NoticeKind::Summary);
        assert_eq!(
            summary.session_key, None,
            "a click only brings the window forward"
        );
        assert_eq!(summary.title, "10 sessions need you");
        assert_eq!(summary.body, "s0, s1, s2 and 7 more");

        // Each session in the summary started its own cooldown.
        notifier.observe(read(&Phase::Working), &on(), 11_000);
        assert!(
            notifier.observe(read(&DONE), &on(), 12_000).is_empty(),
            "inside every session's cooldown"
        );
    }

    #[test]
    fn two_moves_in_one_read_stay_two_notices() {
        let mut notifier = Notifier::default();
        let read = |a: Phase, b: Phase| vec![named("a", a), named("b", b)];
        notifier.observe(read(Phase::Working, Phase::Working), &on(), 0);
        let notices = notifier.observe(read(needs(WaitKind::Ask, "q1"), DONE), &on(), 10_000);
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].session_key.as_deref(), Some("a"));
        assert_eq!(notices[0].kind, NoticeKind::Needs);
        assert_eq!(notices[1].session_key.as_deref(), Some("b"));
        assert_eq!(notices[1].kind, NoticeKind::Done);
    }

    #[test]
    fn a_mixed_burst_says_how_many_need_you_and_how_many_are_done() {
        let notice = |kind, title: &str| Notice {
            kind,
            session_key: Some(title.to_string()),
            title: title.to_string(),
            body: String::new(),
        };
        let summary = coalesce(vec![
            notice(NoticeKind::Needs, "api"),
            notice(NoticeKind::Done, "web"),
            notice(NoticeKind::Done, "cli"),
        ]);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].title, "1 session needs you, 2 done");
        assert_eq!(summary[0].body, "api, web, cli");
        let done = coalesce(vec![
            notice(NoticeKind::Done, "a"),
            notice(NoticeKind::Done, "b"),
            notice(NoticeKind::Done, "c"),
        ]);
        assert_eq!(done[0].title, "3 sessions are done");
    }

    #[test]
    fn a_session_the_read_leaves_out_is_a_first_read_when_it_returns() {
        let mut notifier = Notifier::default();
        notifier.observe(vec![session(Phase::Working)], &on(), 0);
        notifier.observe(Vec::new(), &on(), 10_000);
        assert!(notifier.observe(vec![session(DONE)], &on(), 20_000).is_empty());
    }

    #[test]
    fn a_card_reads_as_its_phase_and_name() {
        assert_eq!(
            Phase::of(AgentState::Waiting, Some(WaitKind::Ask), Some("q1"), false),
            needs(WaitKind::Ask, "q1")
        );
        assert_eq!(
            Phase::of(AgentState::Working, None, None, false),
            Phase::Working
        );
        assert_eq!(Phase::of(AgentState::Idle, None, None, true), DONE);
        assert_eq!(Phase::of(AgentState::Idle, None, None, false), FREE);
        assert_eq!(
            Phase::of(AgentState::Exited, None, None, false),
            Phase::Exited
        );
        assert_eq!(
            Phase::of(AgentState::Unverifiable, None, None, false),
            Phase::Unknown
        );

        assert_eq!(label(Some("fix login"), "/src/hangar", KEY), "fix login");
        assert_eq!(label(Some("  "), "/src/hangar", KEY), "hangar");
        assert_eq!(label(None, "/w/agents-fix-login", KEY), "agents-fix-login");
        assert_eq!(label(None, "", KEY), KEY);
    }

    #[test]
    fn the_stored_toggle_is_on_unless_it_says_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home").join(NOTIFICATIONS_FILE);
        assert!(load(&path), "nothing stored yet");
        store(&path, false).unwrap();
        assert!(!load(&path));
        store(&path, true).unwrap();
        assert!(load(&path));
        std::fs::write(&path, "neon").unwrap();
        assert!(load(&path), "an unknown word is the default");
    }
}
