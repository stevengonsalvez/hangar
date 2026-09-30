//! The window's rename, end to end through the shell: a name the host accepts
//! is the label the sidebar reads off the `session_labels` frame, it is on disk
//! before the command returns, and a relaunch loads it. A refused name writes
//! and frames nothing. The branch, the folder and the tmux session keep their
//! names either way.
//!
//! Its own process: it points `AINB_HOME` at a scratch home, where the label
//! store lives.

use std::sync::{Arc, Mutex};

use ainb_app::AppState;
use ainb_app::config::{AppConfig, SessionLabelStore};
use ainb_app::models::{Session, Workspace};
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{CommandId, Intent, Keymap, SectionId};
use ainb_desktop::executor::DesktopExecutor;
use ainb_desktop::host::DesktopHost;
use ainb_desktop::shell::Shell;
use uuid::Uuid;

mod support;

/// The label store is one file in the one scratch home: one test at a time.
static STORE: Mutex<()> = Mutex::new(());

fn row(name: &str, tmux: Option<&str>) -> Session {
    let mut session = Session::new(name.to_string(), format!("/work/repo/{name}"));
    session.branch_name = format!("ainb/{name}");
    session.tmux_session_name = tmux.map(str::to_string);
    session
}

/// Two projects: `api` and `web` in `repo`, `api` again in `other`, and a row
/// with no tmux session. Every tmux name is fresh, so tests never share a label.
struct Fixture {
    shell: Shell<Box<dyn FnMut(FrameBatch) + Send>>,
    frames: Arc<Mutex<Vec<String>>>,
    api: Uuid,
    api_tmux: String,
    web: Uuid,
    web_tmux: String,
    other_api: Uuid,
    other_tmux: String,
    no_tmux: Uuid,
}

fn fixture() -> Fixture {
    support::isolated_home();
    let fresh = |name: &str| format!("ainb-{name}-{}", Uuid::new_v4().simple());
    let api_tmux = fresh("api");
    let api = row("api", Some(&api_tmux));
    let web_tmux = fresh("web");
    let web = row("web", Some(&web_tmux));
    let no_tmux = row("boss", None);
    let other_tmux = fresh("other");
    let other_api = row("api", Some(&other_tmux));
    let ids = (api.id, web.id, other_api.id, no_tmux.id);

    let mut state = AppState::with_config(AppConfig::default());
    let mut repo = Workspace::new("repo".to_string(), "/work/repo".into());
    repo.sessions = vec![api, web, no_tmux];
    let mut other = Workspace::new("other".to_string(), "/work/other".into());
    other.sessions = vec![other_api];
    state.sessions.workspaces = vec![repo, other];

    let frames = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let frames = Arc::clone(&frames);
        Box::new(move |batch: FrameBatch| {
            frames.lock().unwrap().push(serde_json::to_string(&batch).unwrap());
        }) as Box<dyn FnMut(FrameBatch) + Send>
    };
    let host = DesktopHost::hosting(
        state,
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::SessionLabels, SectionId::Sessions]),
        sink,
    )
    .without_attention_poll();
    let shell = Shell::new(host, DesktopExecutor::new(None));
    shell.subscribe(Subscription::only(&[
        SectionId::SessionLabels,
        SectionId::Sessions,
    ]));
    frames.lock().unwrap().clear();
    Fixture {
        shell,
        frames,
        api: ids.0,
        api_tmux,
        web: ids.1,
        web_tmux,
        other_api: ids.2,
        other_tmux,
        no_tmux: ids.3,
    }
}

/// The label store as the next launch reads it: a state built from disk.
fn relaunched_label(tmux: &str) -> Option<String> {
    AppState::with_config(AppConfig::default())
        .session_labels
        .session_label_store
        .get(tmux)
        .cloned()
}

#[test]
fn an_accepted_name_is_framed_written_and_survives_a_relaunch() {
    let _one = STORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let f = fixture();

    f.shell.rename_session(f.api, "  Fix login  ").expect("accepted");

    let framed = f.frames.lock().unwrap().join("\n");
    assert!(
        framed.contains(&format!("\"{}\":\"Fix login\"", f.api_tmux)),
        "the session_labels frame carries the trimmed name under the tmux name: {framed}"
    );
    assert_eq!(relaunched_label(&f.api_tmux).as_deref(), Some("Fix login"));
}

/// Session `id`'s row in the last `sessions` frame sent.
fn framed_row(frames: &[String], id: Uuid) -> serde_json::Value {
    let id = id.to_string();
    frames
        .iter()
        .filter_map(|batch| serde_json::from_str::<serde_json::Value>(batch).ok())
        .flat_map(|batch| batch["frames"].as_array().cloned().unwrap_or_default())
        .rfind(|frame| frame["section"] == "sessions")
        .and_then(|frame| {
            frame["body"]["workspaces"]
                .as_array()?
                .iter()
                .flat_map(|workspace| workspace["sessions"].as_array().cloned().unwrap_or_default())
                .find(|row| row["id"] == id.as_str())
        })
        .expect("a sessions frame with the row")
}

#[test]
fn the_rename_touches_no_branch_folder_or_tmux_name() {
    let _one = STORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let f = fixture();
    f.shell.rename_session(f.api, "Fix login").expect("accepted");
    let api = framed_row(&f.frames.lock().unwrap(), f.api);
    assert_eq!(api["name"], "api");
    assert_eq!(api["branch_name"], "ainb/api");
    assert_eq!(api["workspace_path"], "/work/repo/api");
    assert_eq!(api["tmux_session_name"], f.api_tmux.as_str());
}

#[test]
fn a_refused_name_writes_and_frames_nothing() {
    let _one = STORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let f = fixture();
    let refusals = [
        (f.api, "   ", "A name cannot be empty."),
        (f.api, "a\nb", "control characters"),
        (f.api, &"x".repeat(65), "64 characters"),
        (f.api, "api\u{200B}", "invisible formatting"),
        // `web` is another row in the same project.
        (f.api, " web ", "already named \"web\""),
        (f.no_tmux, "Boss", "no tmux session"),
        (Uuid::new_v4(), "Gone", "not in the session list"),
    ];
    for (session, raw, why) in refusals {
        let refusal =
            f.shell.rename_session(session, raw).expect_err(&format!("{raw:?} is refused"));
        assert!(refusal.contains(why), "{raw:?}: {refusal}");
    }
    // A dispatch frames whatever moved since the last frame: a refused rename
    // moved nothing, so nothing goes out even then.
    f.shell.dispatch(Intent::Command(
        CommandId::new("global.go_home"),
        serde_json::Value::Null,
    ));
    assert_eq!(f.frames.lock().unwrap().len(), 0, "nothing was framed");
    assert_eq!(relaunched_label(&f.api_tmux), None, "nothing was written");
}

#[test]
fn a_name_only_counts_as_taken_within_its_project() {
    let _one = STORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let f = fixture();
    // `api` is a row in `repo`, not in `other`: the other project may use it.
    f.shell
        .rename_session(f.other_api, "web")
        .expect("another project's names are not taken");
    // A row's own name is not taken from itself.
    f.shell.rename_session(f.web, "web").expect("its own name");
    // A label counts as the name a row shows: `web` is now `Checkout`, so
    // `Checkout` is taken in `repo` and `web` is free again.
    f.shell.rename_session(f.web, "Checkout").expect("accepted");
    let refusal = f.shell.rename_session(f.api, "Checkout").expect_err("taken");
    assert!(refusal.contains("already named"), "{refusal}");
    f.shell.rename_session(f.api, "web").expect("web is free now");
}

/// What the terminal does when a person names a session there while the
/// window is open: the label store on disk gains a label.
fn terminal_labels(tmux: &str, label: &str) {
    let mut store = SessionLabelStore::load();
    store.set(tmux.to_string(), Some(label.to_string()));
    store.save().expect("the terminal's write");
}

#[test]
fn a_label_the_terminal_wrote_since_launch_is_kept_and_counts_as_taken() {
    let _one = STORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let f = fixture();
    // After the window loaded its store: the terminal names `web` and the
    // other project's `api`.
    terminal_labels(&f.web_tmux, "Checkout");
    terminal_labels(&f.other_tmux, "Billing");

    let refusal = f.shell.rename_session(f.api, "Checkout").expect_err("taken");
    assert!(refusal.contains("already named \"Checkout\""), "{refusal}");

    f.shell.rename_session(f.api, "Fix login").expect("accepted");
    assert_eq!(relaunched_label(&f.api_tmux).as_deref(), Some("Fix login"));
    assert_eq!(
        relaunched_label(&f.web_tmux).as_deref(),
        Some("Checkout"),
        "the window's write kept the terminal's label"
    );
    assert_eq!(relaunched_label(&f.other_tmux).as_deref(), Some("Billing"));
}
