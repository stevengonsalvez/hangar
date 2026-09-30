//! The single source of truth for the Hangar home directory.
//!
//! Every Hangar consumer — the store's `hangar.db`, the daemon's per-task env
//! tree + `hangar.sock`, the structured-log dir, the daemon-token file, and the
//! plugin/runtime `state.toml` — roots under one directory. This module owns the
//! resolution so that contract lives in exactly one place; each resolver
//! delegates here and only appends its own leaf (`hangar.db`, `hangar/logs`,
//! `hangar/state.toml`, …).

use std::path::{Path, PathBuf};

/// The environment variable that overrides the Hangar home directory.
///
/// When set and non-empty, its value IS the home directory verbatim — the db
/// lives at `$AINB_HANGAR_HOME/hangar.db`, with NO `.agents-in-a-box` segment
/// appended. The `.agents-in-a-box` sub-directory is only used on the
/// real-home fallback path. This is the contract the P0.7 daemon tripwire
/// asserts (`docs/hangar/phases/P0.md:222`).
pub const HANGAR_HOME_ENV: &str = "AINB_HANGAR_HOME";

/// Sub-directory under the user's real home that holds the Hangar tree on the
/// default (no-override) path. Not appended when `$AINB_HANGAR_HOME` is set —
/// the override value is the home directory verbatim.
pub const HANGAR_DIR: &str = ".agents-in-a-box";

/// Resolve the Hangar home directory: `$AINB_HANGAR_HOME` (verbatim, when set
/// and non-empty) else `dirs::home_dir()?/.agents-in-a-box`.
///
/// Single source of truth — every resolver across the Hangar crates delegates
/// here so the home contract lives in one place. An EMPTY `$AINB_HANGAR_HOME`
/// is ignored (it would otherwise resolve to a relative path in the current
/// working directory), falling through to the real-home default.
///
/// Returns `None` only when `$AINB_HANGAR_HOME` is unset/empty AND the user's
/// home directory cannot be resolved.
#[must_use]
pub fn hangar_home() -> Option<PathBuf> {
    match std::env::var_os(HANGAR_HOME_ENV).filter(|p| !p.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => Some(dirs::home_dir()?.join(HANGAR_DIR)),
    }
}

/// The user config file under `hangar_home`: `<hangar home>/config/config.toml`.
///
/// The one file the daemon and the notifyd and session-reader plugins take
/// their own tables from (`[hangar]`, `[codex]`, `[acp]`, `[notifyd]`,
/// `[session_reader]`). A project's `.ainb/config.toml` never reaches them.
#[must_use]
pub fn config_path_in(hangar_home: &Path) -> PathBuf {
    hangar_home.join("config").join("config.toml")
}

/// [`config_path_in`] of [`hangar_home`]: `$AINB_HANGAR_HOME/config/config.toml`
/// when the override is set and non-empty, else
/// `~/.agents-in-a-box/config/config.toml`.
///
/// `None` exactly when [`hangar_home`] is.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    hangar_home().map(|home| config_path_in(&home))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Process-wide mutex serialising the `$AINB_HANGAR_HOME` mutations these
    /// tests perform — cargo runs tests in-process and in parallel, so an
    /// unguarded `set_var`/`remove_var` races with any sibling test that reads
    /// the same var.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// With `$AINB_HANGAR_HOME` set and non-empty, [`hangar_home`] returns it
    /// VERBATIM — no `.agents-in-a-box` segment appended. This is the override
    /// contract the daemon's socket path + db + state file all key off; if it
    /// broke, a non-default home would silently split the daemon (writing under
    /// the override) from its readers (reading under `~`).
    #[test]
    fn override_is_returned_verbatim() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prior = std::env::var_os(HANGAR_HOME_ENV);
        std::env::set_var(HANGAR_HOME_ENV, "/tmp/custom-hangar-home");

        let home = hangar_home().expect("override path resolves");
        assert_eq!(
            home,
            PathBuf::from("/tmp/custom-hangar-home"),
            "the override value must be used verbatim, with no `.agents-in-a-box` appended"
        );
        assert!(
            !home.ends_with(HANGAR_DIR),
            "the override path must NOT have the `.agents-in-a-box` segment appended"
        );

        match prior {
            Some(v) => std::env::set_var(HANGAR_HOME_ENV, v),
            None => std::env::remove_var(HANGAR_HOME_ENV),
        }
    }

    /// The config file follows the override like everything else under the
    /// home: a reader built on `$HOME` would miss it.
    #[test]
    fn config_path_follows_the_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prior = std::env::var_os(HANGAR_HOME_ENV);
        std::env::set_var(HANGAR_HOME_ENV, "/tmp/custom-hangar-home");

        assert_eq!(
            config_path(),
            Some(PathBuf::from("/tmp/custom-hangar-home/config/config.toml"))
        );

        match prior {
            Some(v) => std::env::set_var(HANGAR_HOME_ENV, v),
            None => std::env::remove_var(HANGAR_HOME_ENV),
        }
    }

    /// An empty `$AINB_HANGAR_HOME` is ignored and falls through to the
    /// real-home default (which carries the `.agents-in-a-box` segment) — an
    /// empty value must never resolve to a relative cwd path.
    #[test]
    fn empty_override_falls_through_to_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prior = std::env::var_os(HANGAR_HOME_ENV);
        std::env::set_var(HANGAR_HOME_ENV, "");

        let home = hangar_home();
        // Only assert the shape when a home dir is resolvable in the test env;
        // the point is the empty override did NOT win.
        if let Some(home) = home {
            assert!(
                home.ends_with(HANGAR_DIR),
                "empty override must fall through to `~/.agents-in-a-box`, got {home:?}"
            );
        }

        match prior {
            Some(v) => std::env::set_var(HANGAR_HOME_ENV, v),
            None => std::env::remove_var(HANGAR_HOME_ENV),
        }
    }
}
