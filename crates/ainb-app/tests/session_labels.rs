#![allow(missing_docs)]

// ABOUTME: The session label store, `session-labels.json`, has three writers:
// the terminal's label popup, the desktop window's rename and `ainb label`.
// Each one sets a single label on the file as it is on disk, under the lock
// every reader and writer takes, so no writer puts back a label another one
// changed, and a file that does not parse is refused rather than replaced.

use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use ainb_app::app::state::NotificationType;
use ainb_app::app::{NoRenderer, reports};
use ainb_app::config::SessionLabelStore;
use ainb_app::config::lock::lock_for;
use ainb_app::models::{Session, Workspace};
use ainb_app::{AppState, Effect, Keymap, dispatch};

#[path = "support/home.rs"]
mod home;

use home::ScopedHome;

fn label_file(home: &ScopedHome) -> std::path::PathBuf {
    home.path().join(".agents-in-a-box").join("session-labels.json")
}

/// A state with one listed session, `tmux`, selected, as the terminal has it
/// once the workspace load has run.
fn terminal_with_one_session(tmux: &str) -> AppState {
    let mut state = AppState::new();
    let mut session = Session::new("api".to_string(), "/work/repo/api".to_string());
    session.tmux_session_name = Some(tmux.to_string());
    let mut workspace = Workspace::new("repo".to_string(), "/work/repo".into());
    workspace.sessions = vec![session];
    state.sessions.workspaces = vec![workspace];
    state.sessions.selected_workspace_index = Some(0);
    state.sessions.selected_session_index = Some(0);
    state
}

/// The terminal's label popup: open it on the selected session, type `label`,
/// confirm, and run the writes it queued as the terminal host does. Returns
/// each write's outcome.
fn terminal_popup_saves(state: &mut AppState, label: &str) -> Vec<Result<(), String>> {
    state.start_session_label_rename();
    state.session_labels.session_label_rename_buffer.clear();
    for c in label.chars() {
        state.session_label_rename_char(c);
    }
    state.confirm_session_label_rename();
    state
        .take_effects()
        .iter()
        .filter_map(|effect| match effect {
            Effect::Persist(store) => Some(ainb_app::config::persist::write(store)),
            _ => None,
        })
        .collect()
}

/// What the desktop window's rename writes (`ainb_desktop::rename`).
fn desktop_renames(tmux: &str, label: &str) -> std::io::Result<SessionLabelStore> {
    SessionLabelStore::set_label(tmux, Some(label.to_string()))
}

#[test]
fn a_terminal_save_keeps_a_desktop_rename_made_after_the_terminal_launched() {
    let home = ScopedHome::new();
    // The terminal launches and loads the store: empty.
    let mut terminal = terminal_with_one_session("tmux-term");
    // The window renames another session after that.
    desktop_renames("tmux-desk", "Checkout").expect("the window's write");

    for outcome in terminal_popup_saves(&mut terminal, "Fix login") {
        outcome.expect("the terminal's write");
    }

    let on_disk = SessionLabelStore::load();
    assert_eq!(
        on_disk.get("tmux-term").map(String::as_str),
        Some("Fix login")
    );
    assert_eq!(
        on_disk.get("tmux-desk").map(String::as_str),
        Some("Checkout"),
        "the terminal put back the store it loaded at launch: {}",
        std::fs::read_to_string(label_file(&home)).unwrap_or_default()
    );
}

#[test]
fn an_unparseable_label_file_is_refused_left_as_it_is_and_reported() {
    let home = ScopedHome::new();
    let file = label_file(&home);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let damaged = b"{\"tmux-a\": \"Keep me\", \"tmux-b\": ";
    std::fs::write(&file, damaged).unwrap();

    // The window and `ainb label` get the refusal back.
    let refusal = desktop_renames("tmux-c", "New").expect_err("a damaged file is refused");
    assert_eq!(refusal.kind(), std::io::ErrorKind::InvalidData, "{refusal}");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        damaged,
        "the file was rewritten"
    );

    // The terminal's popup: its write fails, and the report is a notice.
    let mut terminal = terminal_with_one_session("tmux-c");
    let outcomes = terminal_popup_saves(&mut terminal, "New");
    let [Err(error)] = outcomes.as_slice() else {
        panic!("one refused write, got {outcomes:?}");
    };
    assert_eq!(
        std::fs::read(&file).unwrap(),
        damaged,
        "the file was rewritten"
    );
    let _ = dispatch(
        &mut terminal,
        &Keymap::defaults(),
        &mut NoRenderer,
        reports::persist_failed("session_labels", error),
    );
    assert!(
        terminal.shell.notifications.iter().any(|note| {
            note.notification_type == NotificationType::Error
                && note.message.contains("Could not save session labels")
                && note.message.contains("was not saved")
        }),
        "{:?}",
        terminal.shell.notifications
    );

    // A reader still gets a store, empty, rather than a failure.
    assert_eq!(SessionLabelStore::load().get("tmux-a"), None);
}

#[test]
fn a_legacy_file_that_does_not_parse_is_left_alone_and_does_not_block_a_write() {
    let home = ScopedHome::new();
    let legacy = home.path().join(".agents-in-a-box").join("ssh_display_names.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let damaged = b"{\"ssh-a-22\": ";
    std::fs::write(&legacy, damaged).unwrap();

    desktop_renames("tmux-a", "New").expect("the current file is written");

    assert_eq!(
        std::fs::read(&legacy).unwrap(),
        damaged,
        "the legacy file was rewritten"
    );
    assert_eq!(
        SessionLabelStore::load().get("tmux-a").map(String::as_str),
        Some("New")
    );
}

#[test]
fn a_load_waits_for_a_writer_that_holds_the_lock() {
    let home = ScopedHome::new();
    let file = label_file(&home);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, r#"{"tmux-a": "Before"}"#).unwrap();

    let writer = lock_for(&file).expect("the writer's lock");
    let (loaded_tx, loaded_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let store = SessionLabelStore::load();
        loaded_tx.send(store.get("tmux-a").cloned()).unwrap();
    });
    assert!(
        loaded_rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "the load read the file while a writer held its lock"
    );
    std::fs::write(&file, r#"{"tmux-a": "After"}"#).unwrap();
    drop(writer);

    let loaded = loaded_rx.recv_timeout(Duration::from_secs(10)).expect("the load finishes");
    reader.join().unwrap();
    assert_eq!(loaded.as_deref(), Some("After"));
}

#[test]
fn a_label_write_waits_for_a_writer_that_holds_the_lock_and_keeps_its_label() {
    let home = ScopedHome::new();
    let file = label_file(&home);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, r#"{"tmux-a": "Before"}"#).unwrap();

    let writer = lock_for(&file).expect("the other writer's lock");
    let (written_tx, written_rx) = mpsc::channel();
    let renamer = std::thread::spawn(move || {
        written_tx.send(desktop_renames("tmux-b", "New")).unwrap();
    });
    assert!(
        written_rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "the label was written while another writer held the lock"
    );
    // The other writer's change lands before it lets go, so a write that read
    // the file before taking the lock puts "Before" back.
    std::fs::write(&file, r#"{"tmux-a": "After"}"#).unwrap();
    drop(writer);

    written_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the label write finishes")
        .expect("the label write succeeds");
    renamer.join().unwrap();
    let on_disk = SessionLabelStore::load();
    assert_eq!(on_disk.get("tmux-a").map(String::as_str), Some("After"));
    assert_eq!(on_disk.get("tmux-b").map(String::as_str), Some("New"));
}

#[test]
fn a_desktop_rename_racing_a_terminal_rename_loses_neither() {
    let _home = ScopedHome::new();
    const ROUNDS: usize = 200;

    for round in 0..ROUNDS {
        let start = Arc::new(Barrier::new(2));
        let desktop = {
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                desktop_renames(&format!("tmux-desk-{round}"), "Desk").expect("the window's write");
            })
        };
        // A fresh terminal each round, so each one saves from a store loaded
        // before the window's write it races.
        let mut terminal = terminal_with_one_session(&format!("tmux-term-{round}"));
        start.wait();
        for outcome in terminal_popup_saves(&mut terminal, "Term") {
            outcome.expect("the terminal's write");
        }
        desktop.join().unwrap();
    }

    let on_disk = SessionLabelStore::load();
    let lost: Vec<String> = (0..ROUNDS)
        .flat_map(|round| [format!("tmux-desk-{round}"), format!("tmux-term-{round}")])
        .filter(|key| on_disk.get(key).is_none())
        .collect();
    assert!(lost.is_empty(), "{} labels lost: {lost:?}", lost.len());
}
