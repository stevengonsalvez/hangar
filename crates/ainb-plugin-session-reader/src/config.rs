//! Plugin-side config: the `[session_reader]` table of the hangar home's
//! `config/config.toml`, where the hangar home is `$AINB_HANGAR_HOME` when set
//! and non-empty, else `~/.agents-in-a-box` (never `$HOME` directly; see
//! [`ainb_hangar_core::paths::config_path`]).
//!
//! Read-only — the host owns the file; this plugin only consumes its
//! own table (mirroring the burndown plugin's disk-read pattern, since
//! `PluginInitParams` carries no config channel). Any read or parse
//! failure falls back to defaults: config must never break a scan.
//!
//! A failure is logged without quoting the file, through the same sanitiser
//! the daemon uses ([`ainb_hangar_core::config_file`]): a parse error by line
//! and column, a malformed table by the names of its bad keys. The bad line or
//! value may be a token.
//!
//! ```toml
//! [session_reader]
//! incremental_window_days = 30
//! ```

use std::path::Path;

use serde::Deserialize;

/// Default trailing window (days) for the incremental refresh: files
/// whose mtime is older than `now - window` are served from the
/// persisted stable aggregate instead of being re-aggregated.
pub const DEFAULT_INCREMENTAL_WINDOW_DAYS: u32 = 30;

/// Parsed `[session_reader]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SessionReaderConfig {
    /// Trailing recent-window size in days. `0` is clamped to `1` so a
    /// misconfigured zero window cannot disable recency entirely.
    pub incremental_window_days: u32,
}

impl Default for SessionReaderConfig {
    fn default() -> Self {
        Self {
            incremental_window_days: DEFAULT_INCREMENTAL_WINDOW_DAYS,
        }
    }
}

impl SessionReaderConfig {
    /// The effective window, clamped to [1, 36500] days (a zero window
    /// cannot disable recency; an absurd one cannot overflow the
    /// nanosecond watermark arithmetic).
    #[must_use]
    pub fn window_days(self) -> u32 {
        self.incremental_window_days.clamp(1, 36_500)
    }
}

/// Load from the hangar home's config file (see the module docs). Missing
/// file, unreadable file, unparseable TOML, or a malformed `[session_reader]`
/// table all degrade to [`SessionReaderConfig::default`]; the last two with a
/// warn log that never quotes the file.
#[must_use]
pub fn load() -> SessionReaderConfig {
    ainb_hangar_core::paths::config_path()
        .map_or_else(SessionReaderConfig::default, |path| load_from(&path))
}

/// Load from an explicit path (testable entry point).
#[must_use]
pub fn load_from(path: &Path) -> SessionReaderConfig {
    let Ok(content) = std::fs::read_to_string(path) else {
        return SessionReaderConfig::default();
    };
    let root = match ainb_hangar_core::config_file::parse(&content) {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "session-reader: config parse failed; using defaults"
            );
            return SessionReaderConfig::default();
        }
    };
    match root.get("session_reader") {
        Some(table) => ainb_hangar_core::config_file::decode(table).unwrap_or_else(|malformed| {
            tracing::warn!(
                path = %path.display(),
                ?malformed,
                "session-reader: [session_reader] table malformed; using defaults"
            );
            SessionReaderConfig::default()
        }),
        None => SessionReaderConfig::default(),
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
    fn present_value_is_read() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "[session_reader]\nincremental_window_days = 7\n");
        assert_eq!(load_from(&path).incremental_window_days, 7);
    }

    #[test]
    fn missing_file_defaults() {
        let dir = TempDir::new().unwrap();
        let cfg = load_from(&dir.path().join("nope.toml"));
        assert_eq!(cfg, SessionReaderConfig::default());
        assert_eq!(cfg.incremental_window_days, DEFAULT_INCREMENTAL_WINDOW_DAYS);
    }

    #[test]
    fn missing_table_defaults() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "[usage]\nplan = \"max\"\n");
        assert_eq!(load_from(&path), SessionReaderConfig::default());
    }

    #[test]
    fn garbage_toml_defaults() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "not toml at {{{ all");
        assert_eq!(load_from(&path), SessionReaderConfig::default());
    }

    #[test]
    fn malformed_table_defaults() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "[session_reader]\nincremental_window_days = \"soon\"\n",
        );
        assert_eq!(load_from(&path), SessionReaderConfig::default());
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "[session_reader]\nincremental_window_days = 14\nfuture_knob = true\n",
        );
        assert_eq!(load_from(&path).incremental_window_days, 14);
    }

    #[test]
    fn zero_window_clamps_to_one_day() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "[session_reader]\nincremental_window_days = 0\n");
        assert_eq!(load_from(&path).window_days(), 1);
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
    /// message would echo the line into the plugin log.
    #[test]
    fn a_parse_error_is_logged_by_line_not_text() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "[session_reader]\nincremental_window_days = 7\ntoken = \"sk-SECRET-123\n",
        );
        let log = captured_log(|| {
            assert_eq!(load_from(&path), SessionReaderConfig::default());
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
            "[session_reader]\nincremental_window_days = \"sk-SECRET-456\"\n",
        );
        let log = captured_log(|| {
            assert_eq!(load_from(&path), SessionReaderConfig::default());
        });
        assert!(!log.contains("sk-SECRET"), "{log}");
        assert!(
            log.contains("malformed=[\"incremental_window_days\"]"),
            "{log}"
        );
    }

    /// Serializes the tests below: they mutate the process-wide `HOME` and
    /// `AINB_HANGAR_HOME`, which every other test in this binary that calls
    /// `load()` (none today, but `dirs::home_dir()` is process-global) would
    /// otherwise race.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Runs `body` with `HOME` and `AINB_HANGAR_HOME` set to `home` and
    /// `hangar_home` (`None` removes the variable), then restores whatever was
    /// there before, even if `body` panics.
    fn with_home_and_hangar_home<R>(
        home: Option<&std::path::Path>,
        hangar_home: Option<&str>,
        body: impl FnOnce() -> R,
    ) -> R {
        let _guard = ENV_LOCK.lock().unwrap();
        let prior_home = std::env::var_os("HOME");
        let prior_hangar = std::env::var_os("AINB_HANGAR_HOME");

        match home {
            Some(path) => std::env::set_var("HOME", path),
            None => std::env::remove_var("HOME"),
        }
        match hangar_home {
            Some(path) => std::env::set_var("AINB_HANGAR_HOME", path),
            None => std::env::remove_var("AINB_HANGAR_HOME"),
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        match prior_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prior_hangar {
            Some(v) => std::env::set_var("AINB_HANGAR_HOME", v),
            None => std::env::remove_var("AINB_HANGAR_HOME"),
        }

        result.unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    }

    /// `load()` reads `[session_reader]` from `$AINB_HANGAR_HOME`, never from
    /// `$HOME`. Two distinct homes, each with its OWN config carrying a
    /// distinct value ("7" under the hangar home the reader must use, "9"
    /// under `$HOME` it must not), so a reader that used `$HOME` would read a
    /// wrong value instead of silently passing.
    #[test]
    fn load_reads_the_hangar_home_not_home() {
        let hangar_home = TempDir::new().unwrap();
        let hangar_path = ainb_hangar_core::paths::config_path_in(hangar_home.path());
        std::fs::create_dir_all(hangar_path.parent().unwrap()).unwrap();
        std::fs::write(
            &hangar_path,
            "[session_reader]\nincremental_window_days = 7\n",
        )
        .unwrap();
        let wrong_home = TempDir::new().unwrap();
        let wrong_path =
            wrong_home.path().join(".agents-in-a-box").join("config").join("config.toml");
        std::fs::create_dir_all(wrong_path.parent().unwrap()).unwrap();
        std::fs::write(
            &wrong_path,
            "[session_reader]\nincremental_window_days = 9\n",
        )
        .unwrap();

        let window_days = with_home_and_hangar_home(
            Some(wrong_home.path()),
            Some(hangar_home.path().to_str().unwrap()),
            || load().incremental_window_days,
        );
        assert_eq!(window_days, 7, "must read $AINB_HANGAR_HOME, not $HOME");
    }

    /// With `$AINB_HANGAR_HOME` unset, `load()` falls back to
    /// `$HOME/.agents-in-a-box`.
    #[test]
    fn load_falls_back_to_home_when_hangar_home_is_unset() {
        let home = TempDir::new().unwrap();
        let path = home.path().join(".agents-in-a-box").join("config").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[session_reader]\nincremental_window_days = 11\n").unwrap();

        let window_days =
            with_home_and_hangar_home(Some(home.path()), None, || load().incremental_window_days);
        assert_eq!(window_days, 11);
    }

    /// An empty `$AINB_HANGAR_HOME` is not "set": `load()` must fall back to
    /// `$HOME/.agents-in-a-box`, the same as when the variable is absent.
    #[test]
    fn load_treats_an_empty_hangar_home_as_unset() {
        let home = TempDir::new().unwrap();
        let path = home.path().join(".agents-in-a-box").join("config").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[session_reader]\nincremental_window_days = 13\n").unwrap();

        let window_days = with_home_and_hangar_home(Some(home.path()), Some(""), || {
            load().incremental_window_days
        });
        assert_eq!(
            window_days, 13,
            "an empty override must fall through to $HOME"
        );
    }
}
