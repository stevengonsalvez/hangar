//! A broken bridge-token line in `config.toml` never reaches an error or a log.
//!
//! `toml`'s own error `Display` quotes the offending source line under a caret.
//! The user config holds `[fleet.bridge.*]` tokens, so an unterminated
//! `token = "...` line used to print the token twice at every startup: in
//! `AppConfig::migrate_legacy_paths`' warning and in `AppConfig::load`'s error.
//!
//! Home comes from the scoped guard, like every test that moves it.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use ainb_app::config::AppConfig;

#[path = "support/home.rs"]
mod home;
use home::ScopedHome;

const SECRET: &str = "s3cr3tBridgeTokenValue";

/// A tracing subscriber that writes every event's fields into a string.
struct Capture(Arc<Mutex<String>>);

struct FieldWriter<'a>(&'a mut String);

impl tracing::field::Visit for FieldWriter<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut out = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        event.record(&mut FieldWriter(&mut out));
        out.push('\n');
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
// The home guard is held to the end on purpose: `load` reads `HOME` last.
#[allow(clippy::significant_drop_tightening)]
fn a_broken_bridge_token_line_leaks_into_neither_the_error_nor_the_log() {
    let mut home = ScopedHome::new();
    home.unset("AINB_CONFIG_PATH");
    let root = home.path().join(".agents-in-a-box");
    let canonical = root.join("config").join("config.toml");
    std::fs::create_dir_all(canonical.parent().unwrap()).expect("mkdir");
    // Line 2 is the broken one: the string never closes.
    std::fs::write(
        &canonical,
        format!("[fleet.bridge.telegram]\ntoken = \"123:{SECRET}\nuser_id = 42\n"),
    )
    .expect("write canonical");
    // A parseable stray file, so the migration reaches the canonical parse
    // and logs its warning.
    std::fs::write(
        root.join("config.toml"),
        "[ui_preferences]\ntheme = \"dark\"\n",
    )
    .expect("write stray");

    // The project layers read `./.ainb` and `./.agents-box` under the working
    // directory, which is this crate's and holds neither.

    let log = Arc::new(Mutex::new(String::new()));
    let error = tracing::subscriber::with_default(Capture(Arc::clone(&log)), || {
        AppConfig::migrate_legacy_paths();
        AppConfig::load().expect_err("an unterminated string must not load")
    });

    let log = log.lock().unwrap().clone();
    assert!(
        log.contains("user config does not parse"),
        "the migration did not reach the canonical parse: {log}"
    );
    assert!(!log.contains(SECRET), "token in the startup log: {log}");
    assert!(
        log.contains("line 2"),
        "the log lost the line number: {log}"
    );

    for rendered in [
        format!("{error}"),
        format!("{error:#}"),
        format!("{error:?}"),
    ] {
        assert!(!rendered.contains(SECRET), "token in the error: {rendered}");
    }
    let chain = format!("{error:#}");
    assert!(chain.contains("config does not parse"), "{chain}");
    assert!(chain.contains("line 2"), "{chain}");
}
