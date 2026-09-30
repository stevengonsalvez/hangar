//! The terminal link wiring in `main.rs` and the window's capability, which no
//! test can open a window from, checked at the source (as `notify_wiring.rs`
//! checks the notifier's): the host opens a link only through `links::open`,
//! and the webview is given no other way to open one. The rule, and that a
//! refused URL never reaches the opener, are `links.rs`'s own unit tests.

use std::path::Path;

fn read(relative: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)).unwrap()
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
fn the_command_opens_through_the_rule_with_the_default_program() {
    let source = read("src/main.rs");
    let command = body(&source, "open_url");
    let rule = at(
        command,
        "ainb_desktop::links::open(&url,",
        "the rule is asked",
    );
    let opener = at(
        command,
        "tauri_plugin_opener::open_url(url, None::<&str>)",
        "the default program opens what the rule handed over",
    );
    assert!(
        rule < opener,
        "the opener is the rule's closure:\n{command}"
    );
    assert_eq!(
        command.matches("tauri_plugin_opener::").count(),
        1,
        "the opener is called once, inside the rule:\n{command}"
    );
    at(
        body(&source, "main"),
        "open_url,",
        "the command is in the invoke handler",
    );
}

#[test]
fn the_webview_has_no_other_way_to_open_a_url() {
    let source = read("src/main.rs");
    // Registered, the opener plugin would put its own `open_url` and its link
    // click script in the page; as a library, it adds neither.
    assert!(
        !source.contains("tauri_plugin_opener::init")
            && !source.contains("tauri_plugin_opener::Builder"),
        "main.rs registers the opener plugin"
    );
    // A new-window handler is what would let `window.open` reach the OS; the
    // webview's default refuses every new window.
    assert!(
        !source.contains("on_new_window"),
        "main.rs handles new windows"
    );
    let capability = read("capabilities/default.json");
    let capability: serde_json::Value = serde_json::from_str(&capability).unwrap();
    let permissions = capability["permissions"].as_array().expect("a permission list");
    for permission in permissions {
        let name = permission
            .as_str()
            .or_else(|| permission["identifier"].as_str())
            .unwrap_or_default();
        assert!(
            !name.starts_with("opener:") && !name.starts_with("shell:"),
            "the window is granted `{name}`"
        );
    }
}
