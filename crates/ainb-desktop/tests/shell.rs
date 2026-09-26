//! The shell's host and executor are one lock: a dispatch arriving while ticks
//! run returns instead of deadlocking.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Chord, CommandId, Intent, Keymap, SectionId};
use ainb_desktop::executor::DesktopExecutor;
use ainb_desktop::host::DesktopHost;
use ainb_desktop::shell::Shell;

mod support;

#[test]
fn a_dispatch_during_ticks_returns() {
    support::isolated_home();

    // Frames are sent across threads in the window, so the sink here is Send.
    let (frames, received) = mpsc::channel::<FrameBatch>();
    let host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Shell]),
        move |batch: FrameBatch| {
            let _ = frames.send(batch);
        },
    );
    let shell = Arc::new(Shell::new(host, DesktopExecutor::new(None)));

    let ticking = {
        let shell = Arc::clone(&shell);
        std::thread::spawn(move || {
            for _ in 0..500 {
                shell.tick();
            }
        })
    };
    let (done, finished) = mpsc::channel();
    let dispatching = {
        let shell = Arc::clone(&shell);
        std::thread::spawn(move || {
            for spelling in ["s", "q"].iter().cycle().take(200) {
                shell.dispatch(Intent::Key(Chord::parse(spelling).expect("valid chord")));
            }
            let _ = done.send(());
        })
    };

    finished
        .recv_timeout(Duration::from_secs(30))
        .expect("dispatch returned while the shell was ticking");
    dispatching.join().expect("dispatch thread");
    ticking.join().expect("tick thread");
    assert!(
        received.try_iter().count() > 0,
        "the shell framed its moves"
    );
}

/// P6e: the flush the app's exit paths call is bounded by its own argument,
/// the wait for the shell's lock included. A tick stuck holding that lock
/// would otherwise hold the whole exit, which is the hang the bound exists to
/// stop; the exit hears that the queue was never reached and goes.
#[test]
fn a_flush_gives_up_on_a_shell_that_stays_busy() {
    support::isolated_home();

    let host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Shell]),
        |_batch: FrameBatch| {},
    );
    let shell = Shell::new(host, DesktopExecutor::new(None));

    let bound = Duration::from_millis(300);
    let busy = shell.hold_for_tests();
    let started = std::time::Instant::now();
    let outcome = shell.flush_session_store_writes(bound);
    let waited = started.elapsed();
    drop(busy);

    assert!(
        outcome.is_none(),
        "the flush claimed a queue it never reached"
    );
    assert!(
        waited < bound * 4,
        "the flush waited past its bound on the shell's lock: {waited:?}"
    );

    // The shell is free again, so the same call reaches the queue and finds
    // nothing in it.
    assert_eq!(shell.flush_session_store_writes(bound), Some(0));
}

/// The answer banner's sequence, as the window sends it (`answer.ts`):
/// `answer_home` first, then `session_list.select_row`, `session_list.select_tab`
/// and `session_list.ask.pick`, each through `dispatch_renderer`. From the inbox,
/// the settings and the git view the reducer is on the session list by the
/// time the rows arrive, and none is refused (#121). Without the ask first,
/// the Ask tab row is refused as off screen, which is what the pick over the
/// inbox page hit.
#[test]
fn the_banners_sequence_lands_from_any_page() {
    use ainb_app::AppState;
    use ainb_app::app::screens::ids as screen_ids;
    use ainb_app::fleet::attention::{AttentionKind, AttentionOption, SessionAttention};
    use ainb_app::models::{Session, Workspace};
    use ainb_app::wire::frame::{HostId, Subscription};

    support::isolated_home();
    // One session, blocked on a daemon question with an option to pick: what
    // the banner is drawn for.
    let mut state = AppState::new();
    let mut session = Session::new("api".to_string(), "/work/repo".to_string());
    session.live_attention = vec![
        SessionAttention::daemon(AttentionKind::Ask, 1, "att-1".to_string())
            .with_detail("Which environment?")
            .with_options(vec![AttentionOption {
                label: "staging".to_string(),
                description: String::new(),
            }]),
    ];
    let session_id = session.id;
    let mut workspace = Workspace::new("repo".to_string(), std::path::PathBuf::from("/work/repo"));
    workspace.sessions.push(session);
    state.sessions.workspaces.push(workspace);
    state.sessions.selected_workspace_index = Some(0);
    state.sessions.selected_session_index = Some(0);
    let host = DesktopHost::hosting(
        state,
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Shell]),
        |_: FrameBatch| {},
    )
    .without_attention_poll();
    let shell = Shell::new(host, DesktopExecutor::new(None));
    let name = |id: &str, args: serde_json::Value| Intent::Command(CommandId::new(id), args);
    let sequence = || {
        [
            name(
                "session_list.select_row",
                serde_json::json!({ "target": { "session": session_id }, "open": false }),
            ),
            name(
                "session_list.select_tab",
                serde_json::json!({ "tab": "Ask" }),
            ),
            name(
                "session_list.ask.pick",
                serde_json::json!({ "request": "att-1", "index": 0, "label": "staging" }),
            ),
        ]
    };

    // OPEN_INBOX / OPEN_SETTINGS in the window, and the git view from a row.
    let pages: [&[&str]; 3] = [
        &["global.go_home", "home.inbox"],
        &["global.go_home", "home.config"],
        &["global.go_home", "home.sessions", "session_list.git"],
    ];
    for page in pages {
        for step in page {
            shell.dispatch(name(step, serde_json::Value::Null));
        }
        let screen = shell.current_screen();
        if screen == screen_ids::SESSION_LIST {
            // No session to open the git view from: the leg is the home walk.
            shell.dispatch(name("global.go_home", serde_json::Value::Null));
        }
        let screen = shell.current_screen();
        assert_ne!(
            screen,
            screen_ids::SESSION_LIST,
            "{page:?} left the session list"
        );
        let [_, select_tab, _] = sequence();
        assert!(
            shell.dispatch_renderer(select_tab).is_some(),
            "the Ask tab row is off screen on {screen}"
        );

        shell.answer_home();
        assert_eq!(
            shell.current_screen(),
            screen_ids::SESSION_LIST,
            "home from {screen}"
        );
        for intent in sequence() {
            assert_eq!(
                shell.dispatch_renderer(intent),
                None,
                "refused after answer_home from {screen}"
            );
        }
    }
}
