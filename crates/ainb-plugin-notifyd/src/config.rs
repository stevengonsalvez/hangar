// ABOUTME: Plugin-side reader for the `[notifyd]` table of the host config.

//! Plugin-side config: the `[notifyd]` table of the hangar home's
//! `config/config.toml`, where the hangar home is `$AINB_HANGAR_HOME` when set
//! and non-empty, else `~/.agents-in-a-box` (never `$HOME` directly; see
//! [`ainb_hangar_core::paths::config_path`]).
//!
//! Read-only, and read the same way `ainb-plugin-session-reader` reads its own
//! table: the host owns the file, `PluginInitParams` carries no config channel,
//! and every failure degrades to the coded defaults. A malformed config must
//! never stop notifications, it may only fail to tune them.
//!
//! A failure is logged without quoting the file, through the same sanitiser
//! the daemon uses ([`ainb_hangar_core::config_file`]): a parse error by line
//! and column, a malformed table by the names of its bad keys. The bad line or
//! value may be a token.
//!
//! ```toml
//! [notifyd]
//! os_debounce_secs = 30
//! approval_timeout_secs = 900
//! ```

use std::path::Path;

use serde::Deserialize;

/// Coded default OS-notification debounce, in seconds.
pub const DEFAULT_OS_DEBOUNCE_SECS: u64 = 60;

/// Coded default AWAIT ceiling for a permission request, in seconds.
pub const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 600;

/// The ceiling on `approval_timeout_secs`.
///
/// The broker sits at the BOTTOM of a timeout ladder: broker AWAIT < the
/// client's re-dial deadline (640s) < Claude's registered `PermissionRequest`
/// hook timeout (660s). A configured value above the client deadline means the
/// hook is hard-killed before the broker ever answers, which turns a
/// deliberate deny into a silent hang, so it is clamped rather than trusted.
pub const MAX_APPROVAL_TIMEOUT_SECS: u64 = 630;

/// Parsed `[notifyd]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NotifydConfig {
    /// Per-`(session, event)` debounce window for OS notifications, in seconds.
    pub os_debounce_secs: u64,
    /// Seconds an unanswered permission request waits before it is auto-denied.
    pub approval_timeout_secs: u64,
}

impl Default for NotifydConfig {
    fn default() -> Self {
        Self {
            os_debounce_secs: DEFAULT_OS_DEBOUNCE_SECS,
            approval_timeout_secs: DEFAULT_APPROVAL_TIMEOUT_SECS,
        }
    }
}

impl NotifydConfig {
    /// The effective debounce window.
    #[must_use]
    pub fn os_debounce(self) -> std::time::Duration {
        std::time::Duration::from_secs(self.os_debounce_secs)
    }

    /// The effective AWAIT ceiling, clamped to [1, [`MAX_APPROVAL_TIMEOUT_SECS`]].
    ///
    /// `0` would auto-deny every request the instant it arrived; anything above
    /// the ceiling breaks the timeout ladder (see [`MAX_APPROVAL_TIMEOUT_SECS`]).
    #[must_use]
    pub fn approval_timeout(self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.approval_timeout_secs.clamp(1, MAX_APPROVAL_TIMEOUT_SECS),
        )
    }
}

/// Load from the hangar home's config file (see the module docs). Missing
/// file, unreadable file, unparseable TOML, or a malformed `[notifyd]` table
/// all degrade to [`NotifydConfig::default`]; the last two with a warn log that
/// never quotes the file.
#[must_use]
pub fn load() -> NotifydConfig {
    ainb_hangar_core::paths::config_path()
        .map_or_else(NotifydConfig::default, |path| load_from(&path))
}

/// Load from an explicit path (testable entry point).
#[must_use]
pub fn load_from(path: &Path) -> NotifydConfig {
    let Ok(content) = std::fs::read_to_string(path) else {
        return NotifydConfig::default();
    };
    let root = match ainb_hangar_core::config_file::parse(&content) {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "notifyd: config parse failed; using defaults"
            );
            return NotifydConfig::default();
        }
    };
    match root.get("notifyd") {
        Some(table) => ainb_hangar_core::config_file::decode(table).unwrap_or_else(|malformed| {
            tracing::warn!(
                path = %path.display(),
                ?malformed,
                "notifyd: [notifyd] table malformed; using defaults"
            );
            NotifydConfig::default()
        }),
        None => NotifydConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write_config(dir: &TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("config.toml");
        std::fs::write(&path, body).expect("write config");
        path
    }

    #[test]
    fn present_values_are_read() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "[notifyd]\nos_debounce_secs = 15\napproval_timeout_secs = 120\n",
        );
        let cfg = load_from(&path);
        assert_eq!(cfg.os_debounce().as_secs(), 15);
        assert_eq!(cfg.approval_timeout().as_secs(), 120);
    }

    #[test]
    fn a_partial_table_keeps_the_other_default() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "[notifyd]\nos_debounce_secs = 5\n");
        let cfg = load_from(&path);
        assert_eq!(cfg.os_debounce().as_secs(), 5);
        assert_eq!(
            cfg.approval_timeout().as_secs(),
            DEFAULT_APPROVAL_TIMEOUT_SECS
        );
    }

    #[test]
    fn missing_file_defaults() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            load_from(&dir.path().join("nope.toml")),
            NotifydConfig::default()
        );
    }

    #[test]
    fn a_broken_table_defaults_rather_than_failing() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "[notifyd]\nos_debounce_secs = \"soon\"\n");
        assert_eq!(load_from(&path), NotifydConfig::default());
    }

    /// The AWAIT ceiling has to stay under the client's re-dial deadline, or a
    /// configured value silently turns a deny into a hung hook.
    #[test]
    fn an_absurd_approval_timeout_is_clamped_to_the_ladder() {
        let cfg = NotifydConfig {
            os_debounce_secs: 60,
            approval_timeout_secs: 86_400,
        };
        assert_eq!(cfg.approval_timeout().as_secs(), MAX_APPROVAL_TIMEOUT_SECS);

        let instant = NotifydConfig {
            os_debounce_secs: 60,
            approval_timeout_secs: 0,
        };
        assert_eq!(instant.approval_timeout().as_secs(), 1);
    }

    /// Everything logged on this thread while `run` runs, as a fmt subscriber
    /// writes it.
    ///
    /// A process-wide subscriber, not `with_default`: tracing caches a
    /// callsite's interest at its first hit, so a sibling test reaching the same
    /// `warn!` first, on a thread with no subscriber, would cache "never" and
    /// silence the capture. The global one is interested in every callsite; it
    /// keeps only the bytes of a thread that is capturing.
    fn captured_log(run: impl FnOnce()) -> String {
        thread_local! {
            static CAPTURE: std::cell::RefCell<Option<Vec<u8>>> =
                const { std::cell::RefCell::new(None) };
        }
        struct ThisThread;
        impl std::io::Write for ThisThread {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                CAPTURE.with_borrow_mut(|capture| {
                    if let Some(buffer) = capture {
                        buffer.extend_from_slice(bytes);
                    }
                });
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            let subscriber =
                tracing_subscriber::fmt().with_ansi(false).with_writer(|| ThisThread).finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("the only global subscriber in this test binary");
            // A callsite first hit while the subscriber was being installed
            // may have cached "never"; recompute every callsite against it.
            tracing::callsite::rebuild_interest_cache();
        });

        CAPTURE.set(Some(Vec::new()));
        run();
        String::from_utf8(CAPTURE.take().unwrap_or_default()).unwrap()
    }

    /// A token on a malformed line is located, never quoted: toml's own
    /// message would echo the line into the notifyd log.
    #[test]
    fn a_parse_error_is_logged_by_line_not_text() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "[notifyd]\nos_debounce_secs = 5\ntoken = \"sk-SECRET-123\n",
        );
        let log = captured_log(|| {
            assert_eq!(load_from(&path), NotifydConfig::default());
        });
        assert!(!log.contains("sk-SECRET"), "{log}");
        assert!(log.contains("at line 3, column 23"), "{log}");
    }

    /// A wrong-typed value is named by its key, never quoted: serde's message
    /// would echo the value, and it may be a token.
    #[test]
    fn a_malformed_table_is_logged_by_key_not_value() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "[notifyd]\nos_debounce_secs = 5\napproval_timeout_secs = \"sk-SECRET-456\"\n",
        );
        let log = captured_log(|| {
            assert_eq!(load_from(&path), NotifydConfig::default());
        });
        assert!(!log.contains("sk-SECRET"), "{log}");
        assert!(
            log.contains("malformed=[\"approval_timeout_secs\"]"),
            "{log}"
        );
    }
}
