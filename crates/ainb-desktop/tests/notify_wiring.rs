//! The notification wiring in `main.rs`, which no test can open a window
//! from, checked at the source (as `window_paint_wiring.rs` checks the theme
//! paint): the tick hands every move of section 20 to the notifier under the
//! gate the page and the window keep current, and only then shows what it
//! returns. The rule itself is `notify::decide`'s own unit tests.

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
        "show_notice(&handle, &notice)",
        "what it returns is shown",
    );
    assert!(
        tick < read && read < gate && gate < observe && observe < show,
        "tick, read, gate, decide, show: in that order"
    );
    assert!(
        body(&source, "show_notice").contains(".notification()"),
        "the host sends it through the plugin"
    );
    at(
        &source,
        ".plugin(tauri_plugin_notification::init())",
        "the plugin is registered",
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
