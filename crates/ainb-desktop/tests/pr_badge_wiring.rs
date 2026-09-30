//! The PR badge wiring in `main.rs`, which no test can open a window from,
//! checked at the source (as `open_url_wiring.rs` checks the link opener's):
//! the command resolves the webview's session id to the session list's own
//! worktree and branch before anything reaches `gh`, answers through the
//! cache, and is in the invoke handler. The lookup, the cache and every
//! fail-quiet path are `pr_badge.rs`'s own unit tests.

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
fn the_command_resolves_the_session_then_asks_the_cache() {
    let source = read("src/main.rs");
    let command = body(&source, "pr_badge");
    let resolve = at(
        command,
        "window.shell.worktree_of(&session_id)",
        "the session id is resolved by the host",
    );
    let ask = at(
        command,
        "badges.badge(&worktree, &branch).await",
        "the cache answers",
    );
    assert!(resolve < ask, "resolved before asked:\n{command}");
    assert!(
        !command.contains("Command::new") && !command.contains("lookup("),
        "the command runs no gh of its own, only through the cache:\n{command}"
    );
    let main = body(&source, "main");
    at(main, "pr_badge,", "the command is in the invoke handler");
    at(
        main,
        "app.manage(PrBadges::new(ainb_desktop::pr_badge::find_gh()))",
        "one cache for the window",
    );
}
