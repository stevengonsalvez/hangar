//! The notifier's read of section 20 through the shell (`notify.rs`): every
//! card as a session with its board key, its name and its phase, handed over
//! only when the section moved, so a tick that changed nothing re-reads
//! nothing and can announce nothing.

use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{AppState, Keymap};
use ainb_desktop::executor::DesktopExecutor;
use ainb_desktop::host::DesktopHost;
use ainb_desktop::notify::{Gate, Notifier, Phase};
use ainb_desktop::shell::Shell;
use ainb_hangar_proto::agent_status::{RosterStatusResult, RosterStatusRow, WaitKind, status_row};
use ainb_hangar_proto::fleet::{
    AttentionState, FleetCapabilities, FleetConfidence, FleetProvenance, FleetProvider,
    FleetSession, LifecycleState, ManagementState, PaneBinding, TransportHealth,
};

fn session(key: &str, attention: AttentionState, request: Option<&str>) -> FleetSession {
    FleetSession {
        session_key: key.to_string(),
        provider: FleetProvider::Claude,
        provider_session_id: Some(key.to_string()),
        tmux_target: Some("dev:1.0".to_string()),
        pane_binding: PaneBinding::Bound,
        process_start_fingerprint: None,
        cwd: "/w/hangar".to_string(),
        display_name: None,
        lifecycle: LifecycleState::Running,
        active_work_count: 0,
        attention,
        current_request_fingerprint: request.map(str::to_string),
        current_request: None,
        management: ManagementState::Managed,
        transport_health: TransportHealth::Healthy,
        capabilities: FleetCapabilities::default(),
        provenance: FleetProvenance::Authoritative,
        confidence: FleetConfidence::High,
        discovered_at: 1,
        last_observed_at: 10,
        lifecycle_updated_at: 5,
        session_incarnation: None,
        attention_updated_at: 7,
        model: None,
        reasoning_effort: None,
        model_updated_at: 0,
        version: 1,
        updated_revision: 1,
    }
}

fn read(sessions: &[FleetSession]) -> RosterStatusResult {
    RosterStatusResult {
        rows: sessions
            .iter()
            .map(|session| RosterStatusRow {
                session: session.clone(),
                status: status_row(session, session.attention != AttentionState::None),
                read_revision: 1,
            })
            .collect(),
        read_revision: 1,
        unknown_events: Vec::new(),
        read_at_ms: 0,
    }
}

fn shell(state: AppState) -> Shell<impl ainb_desktop::host::FrameSink> {
    let host = DesktopHost::hosting(
        state,
        Keymap::defaults(),
        HostId::local(),
        Subscription::none(),
        |_batch: FrameBatch| {},
    )
    .without_attention_poll();
    Shell::new(host, DesktopExecutor::new(None))
}

#[test]
fn the_notifier_reads_each_card_once_per_move_of_the_section() {
    let mut state = AppState::with_config(AppConfig::default());
    let mut asking = session("claude:ask", AttentionState::Ask, Some("q-1"));
    asking.display_name = Some("fix login".to_string());
    assert!(state.apply_agent_status_read(
        read(&[asking, session("claude:busy", AttentionState::None, None)]),
        1,
    ));
    let shell = shell(state);

    let mut seen = 0;
    let mut sessions = shell
        .agent_sessions_since(&mut seen)
        .expect("the section moved since version 0");
    sessions.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].key, "claude:ask");
    assert_eq!(sessions[0].label, "fix login");
    assert_eq!(
        sessions[0].phase,
        Phase::Needs {
            kind: Some(WaitKind::Ask),
            request: Some("q-1".to_string()),
        }
    );
    assert_eq!(sessions[1].key, "claude:busy");
    assert_eq!(sessions[1].label, "hangar", "no name: the folder");
    assert_eq!(sessions[1].phase, Phase::Working);

    assert!(
        shell.agent_sessions_since(&mut seen).is_none(),
        "a tick that moved nothing hands over nothing"
    );

    // The launch's first read is a baseline, not a burst of notifications.
    let gate = Gate {
        enabled: true,
        ..Gate::default()
    };
    assert!(Notifier::default().observe(sessions, &gate, 0).is_empty());
}

#[test]
fn a_section_with_no_read_hands_over_nothing() {
    let shell = shell(AppState::with_config(AppConfig::default()));
    let mut seen = 0;
    assert!(shell.agent_sessions_since(&mut seen).is_none());
}
