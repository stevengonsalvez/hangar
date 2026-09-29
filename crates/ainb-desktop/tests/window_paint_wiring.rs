//! The native window's theme paint is applied in `main.rs`, which no test can
//! open a window from, so its wiring is checked at the source (as
//! `updater_gate.rs` checks the updater's): the colour comes from
//! `theme::window_paint` and reaches `set_background_color`, the appearance is
//! set before the theme it leaves is read, and an OS theme change repaints
//! the background. The decision itself is `theme::window_paint`'s own unit
//! test; this holds the two call sites to it.

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
fn the_background_is_window_paints_colour_set_on_the_window() {
    let source = main_rs();
    let paint = body(&source, "paint_window_background");
    let decided = at(
        paint,
        "theme::window_paint(",
        "the colour is decided in theme.rs",
    );
    let applied = at(
        paint,
        ".set_background_color(",
        "the colour reaches the window",
    );
    assert!(
        decided < applied,
        "the colour is decided before it is set:\n{paint}"
    );
}

#[test]
fn a_pick_sets_the_appearance_then_paints_the_background_it_leaves() {
    let source = main_rs();
    let paint = body(&source, "paint_window_theme");
    let held = at(paint, ".set_theme(", "the appearance is held or released");
    let painted = at(
        paint,
        "paint_window_background(",
        "the background is painted",
    );
    assert!(
        held < painted,
        "the background is chosen for the theme the appearance leaves:\n{paint}"
    );
}

#[test]
fn an_os_theme_change_repaints_the_background() {
    let source = main_rs();
    let window = body(&source, "open_main_window");
    let event = at(
        window,
        "WindowEvent::ThemeChanged(",
        "the OS switching theme is heard",
    );
    at(
        &window[event..],
        "paint_window_background(",
        "the OS switching theme repaints the background",
    );
}
