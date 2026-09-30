//! The notification wiring in `main.rs`, which no test can open a window
//! from, checked at the source (as `window_paint_wiring.rs` checks the theme
//! paint): the tick hands every move of section 20 to the notifier under the
//! gate the page and the window keep current, and only then shows what it
//! returns, and a click on one brings the window forward and names the
//! session to the page. The rule itself is `notify::decide`'s own unit tests,
//! and the OS round trip is `notify_click_dbus.rs`.

use std::path::Path;

fn main_rs() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")).unwrap()
}

/// The body of `fn name`, from its opening brace to the matching close.
fn body<'a>(source: &'a str, name: &str) -> &'a str {
    let signature = format!("fn {name}(");
    let start = source
        .find(&signature)
        .unwrap_or_else(|| panic!("main.rs has no `{signature}`"));
    let open = start + source[start..].find('{').expect("the fn has a body");
    let mut depth = 0usize;
    for (at, character) in source[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[open..=open + at];
                }
            }
            _ => {}
        }
    }
    panic!("`{signature}` never closes");
}

/// Where `needle` first appears in `haystack`, failing with `what` if nowhere.
fn at(haystack: &str, needle: &str, what: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("{what}: no `{needle}` in\n{haystack}"))
}

#[test]
fn the_tick_shows_only_what_the_notifier_decides_under_the_live_gate() {
    let source = main_rs();
    let tick = at(&source, "window.shell.tick();", "the tick loop");
    let read = at(
        &source,
        "window.shell.agent_sessions_since(&mut seen)",
        "the tick reads section 20's moves",
    );
    let gate = at(
        &source,
        "handle.state::<Notifications>().gate()",
        "the gate is read when there is something to decide",
    );
    let observe = at(
        &source,
        "notifier.observe(sessions, &gate, now)",
        "the notifier decides",
    );
    let show = at(
        &source,
        "notify_delivery::announce(&os, notices, &open)",
        "what it returns is handed to the OS",
    );
    assert!(
        tick < read && read < gate && gate < observe && observe < show,
        "tick, read, gate, decide, show: in that order"
    );
    at(
        &source,
        "let os = OsDelivery::default();",
        "the OS delivery is notify-rust's",
    );
    at(
        &source,
        "Arc::new(move |session| open_from_notice(&handle, session))",
        "a click runs open_from_notice",
    );
}

#[test]
fn a_click_brings_the_window_forward_then_tells_the_page_the_session() {
    let source = main_rs();
    let open = body(&source, "open_from_notice");
    let focus = at(open, ".set_focus()", "the window takes focus");
    let emit = at(
        open,
        "handle.emit(\"notify:open\", NotifyOpen { session_key })",
        "the page hears the session",
    );
    assert!(focus < emit, "forward first, then the page selects");
    assert!(
        !source.contains("tauri_plugin_notification"),
        "no plugin: it drops the click"
    );
}

#[test]
fn the_gate_follows_the_toggle_the_shown_session_and_window_focus() {
    let source = main_rs();
    let set = body(&source, "notifications_set");
    at(set, "enabled.store(enabled", "the toggle is obeyed at once");
    at(set, "notify::store(", "and kept for the next launch");
    at(
        body(&source, "notify_focus"),
        "= session;",
        "the shown session is the page's",
    );
    let focused = at(
        &source,
        "tauri::WindowEvent::Focused(focused)",
        "window focus is followed",
    );
    assert!(
        source[focused..].contains("window_focused.store(*focused"),
        "and lands in the gate"
    );
    for command in ["notifications_set", "notify_focus"] {
        let handlers = &source[at(&source, "generate_handler![", "the command list")..];
        assert!(
            handlers.contains(command),
            "`{command}` is not in the handler list"
        );
    }
}
