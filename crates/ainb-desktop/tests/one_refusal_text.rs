//! Each daemon error code has one sentence in this window: the spawn and shell
//! verbs share `create::spawn_refusal_text`. A second `refusal_text` beside it
//! is how two sentences for the same code (and two readings of the same
//! code) came back before, so the source is checked for exactly one.

use std::path::Path;

/// Every `.rs` file under `dir`, recursively.
fn sources(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn one_refusal_text_function_in_the_desktop_source() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut defined = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read source");
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            let is_fn = code.starts_with("fn ")
                || code.starts_with("pub fn ")
                || code.starts_with("pub(crate) fn ");
            if is_fn && code.split('(').next().is_some_and(|head| head.ends_with("refusal_text")) {
                defined.push(format!("{}:{}", file.display(), number + 1));
            }
        }
    }
    assert_eq!(
        defined.len(),
        1,
        "exactly one fn .*refusal_text in crates/ainb-desktop/src, found: {defined:#?}"
    );
}
