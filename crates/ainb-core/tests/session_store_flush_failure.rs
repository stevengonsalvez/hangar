#![allow(missing_docs)]

// ABOUTME: P6e, the terminal host's quit, the other side of the bound. A queued
// session-store write that FAILS while the quit waits has no screen left to
// report on, so the quit has to count it as not written. Here the wait outlasts
// the write's own deadline, so the write always fails inside it: the case
// `session_store_flush_bound` only reaches when two equal timers race.
//
// Own binary: the process's session source is decided once, and `AINB_HOME` /
// `AINB_HANGAR_HOME` are process-wide.

use std::time::Duration;

use ainb::Effect;
use ainb::app::Persist;
use ainb::app::ui_state::UiState;
use ainb::cli::util::{self, SESSION_RPC_DEADLINE, SessionSource};
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use ainb::terminal_clients::TerminalClients;
use chrono::Utc;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use uuid::Uuid;

#[path = "support/fake_session_daemon.rs"]
mod fake_session_daemon;
#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;
use fake_session_daemon::{fake_daemon, ready_list};
use fleet_hangar::EnvGuard;

#[test]
fn a_write_that_fails_while_the_quit_waits_is_reported_as_not_written() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let hangar_home = root.path().join("hangar");
    std::fs::create_dir_all(home.join(".agents-in-a-box")).unwrap();
    std::fs::create_dir_all(hangar_home.join("hangar")).unwrap();
    let _env = [
        EnvGuard::set("HOME", &home),
        EnvGuard::set("AINB_HOME", &home),
        EnvGuard::set("AINB_HANGAR_HOME", &hangar_home),
    ];
    util::advertise_workspace_sessions_for_tests(true);

    let session = SessionMetadata {
        session_id: Uuid::new_v4(),
        tmux_session_name: "tmux_p6e-flush-failure".to_string(),
        worktree_path: home.join("work"),
        workspace_name: "ws".to_string(),
        created_at: Utc::now(),
        agent_type: SessionAgentType::Claude,
        headroom_enabled: true,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: ModelSource::LegacyTyped,
        codex_model: None,
        codex_thread_id: None,
        claude_session_id: None,
    };
    let mut store = SessionStore::default();
    store.upsert(session.clone());
    store.save().unwrap();

    // Hello and the resolve probe are answered; the write never is, so it
    // times out on its deadline while the quit is still waiting.
    fake_daemon(&rt, &hangar_home, |_, n| (n == 0).then(ready_list));
    let source = rt.block_on(util::session_source());
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    let mut terminal = Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        },
    )
    .expect("terminal");
    let effect = Effect::Persist(Persist::SessionHeadroom {
        tmux_session: session.tmux_session_name.clone(),
        expected: true,
        enabled: false,
    });
    let reports = rt
        .block_on(ainb::effect_host::execute(
            effect,
            &mut terminal,
            &UiState::default(),
            &mut TerminalClients::default(),
            None,
        ))
        .expect("the executor ran");
    assert!(reports.is_empty(), "the write reported on the tick");

    let dropped = ainb::effect_host::finish_session_store_writes(
        SESSION_RPC_DEADLINE + Duration::from_secs(3),
    );
    assert_eq!(
        dropped, 1,
        "a write that failed while the quit waited was reported as written"
    );
    rt.shutdown_background();
}
