//! The window's theme as the host knows it before any page has loaded.
//!
//! The page keeps the person's pick in its own storage and paints it before
//! its first frame (`ui/public/theme-boot.js`). The native window cannot wait
//! for the page: it is created, and painted, first. So the page tells the host
//! every pick (and the stored one at start, `theme_set`), the host keeps a copy
//! in a one-word file, and at the next launch it creates the window already in
//! that theme: the window's appearance forced to the pick, and its background
//! the page's own `--background`. Without it a light window launched on a dark
//! system showed a dark frame until the page painted.
//!
//! ```text
//!  launch ──load──▶ ThemePreference ──▶ window theme + background ──▶ page paints
//!  pick   ──theme_set──▶ store ─────────▶ window theme + background
//! ```

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

/// The file under the hangar home that holds the host's copy of the pick.
pub const THEME_FILE: &str = "desktop-theme";

/// What a person can pick. Generated into `bindings/Desktop.ts`, which the
/// page's theme module takes its own type from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    /// Follow the OS.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

/// What is actually painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    /// The light palette (`:root.light` in `tokens.css`).
    Light,
    /// The dark palette, the page's default (`:root` in `tokens.css`).
    Dark,
}

impl ThemePreference {
    /// The word the file and the page use.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// The preference `word` names, or `None` for anything else.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim() {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    /// The appearance the native window is held to: `None` follows the OS.
    ///
    /// Forced for an explicit pick, because the webview paints its own default
    /// background from the window's appearance until the page has painted, and
    /// the page's `prefers-color-scheme` reads the same appearance; left to the
    /// OS for System, so both keep following it live.
    #[must_use]
    pub fn forced(self) -> Option<Theme> {
        match self {
            Self::System => None,
            Self::Light => Some(Theme::Light),
            Self::Dark => Some(Theme::Dark),
        }
    }

    /// The theme painted for this pick while the OS paints `system`: the same
    /// rule as `resolveTheme` in `theme.ts`.
    #[must_use]
    pub fn resolve(self, system: Theme) -> Theme {
        self.forced().unwrap_or(system)
    }
}

impl Theme {
    /// The page's `--background` for this theme, as RGB. `tokens.css` is the
    /// source; the parity test below holds these to it.
    #[must_use]
    pub fn background(self) -> [u8; 3] {
        match self {
            Self::Light => [0xff, 0xff, 0xff],
            Self::Dark => [0x0b, 0x0e, 0x14],
        }
    }
}

/// The native window's background for `preference` while the window shows
/// `shown` (under a System pick, the OS's own theme): the page's
/// `--background` for the theme that resolves to. Every native paint takes its
/// colour from here, whether a pick, the launch, or the OS switching theme
/// under a System pick.
#[must_use]
pub fn window_paint(preference: ThemePreference, shown: Theme) -> [u8; 3] {
    preference.resolve(shown).background()
}

/// The stored pick, or System when the file is missing, unreadable, or holds
/// anything else: the page's own fallback (`readPreference`).
#[must_use]
pub fn load(path: &Path) -> ThemePreference {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|word| ThemePreference::parse(&word))
        .unwrap_or_default()
}

/// Store `preference` at `path`, whole or not at all: written beside it and
/// renamed over it, so a crash mid-write leaves the previous pick. Each write
/// stages under its own name, so two picks stored at once never rename a
/// file the other is still writing or has already moved.
///
/// # Errors
///
/// When the directory cannot be created or the file cannot be written.
pub fn store(path: &Path, preference: ThemePreference) -> io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let staged = path.with_file_name(name);
    let written =
        std::fs::write(&staged, preference.as_str()).and_then(|()| std::fs::rename(&staged, path));
    if written.is_err() {
        // Best effort: the error being returned is the one that matters.
        let _ = std::fs::remove_file(&staged);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ThemePreference; 3] = [
        ThemePreference::System,
        ThemePreference::Light,
        ThemePreference::Dark,
    ];

    #[test]
    fn a_pick_resolves_against_the_os_only_when_it_is_system() {
        let cases = [
            (
                ThemePreference::System,
                Theme::Dark,
                Theme::Dark,
                [0x0b, 0x0e, 0x14],
            ),
            (
                ThemePreference::System,
                Theme::Light,
                Theme::Light,
                [0xff, 0xff, 0xff],
            ),
            (
                ThemePreference::Light,
                Theme::Dark,
                Theme::Light,
                [0xff, 0xff, 0xff],
            ),
            (
                ThemePreference::Light,
                Theme::Light,
                Theme::Light,
                [0xff, 0xff, 0xff],
            ),
            (
                ThemePreference::Dark,
                Theme::Dark,
                Theme::Dark,
                [0x0b, 0x0e, 0x14],
            ),
            (
                ThemePreference::Dark,
                Theme::Light,
                Theme::Dark,
                [0x0b, 0x0e, 0x14],
            ),
        ];
        for (preference, system, painted, background) in cases {
            let resolved = preference.resolve(system);
            assert_eq!(resolved, painted, "{preference:?} on a {system:?} OS");
            assert_eq!(
                window_paint(preference, system),
                background,
                "{preference:?} on a {system:?} OS"
            );
        }
        assert_eq!(
            ThemePreference::System.forced(),
            None,
            "System follows the OS live"
        );
    }

    #[test]
    fn a_stored_pick_survives_a_relaunch_and_anything_else_is_system() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home").join(THEME_FILE);
        assert_eq!(load(&path), ThemePreference::System, "nothing stored yet");
        for preference in ALL {
            store(&path, preference).unwrap();
            assert_eq!(load(&path), preference);
        }
        std::fs::write(&path, "neon").unwrap();
        assert_eq!(load(&path), ThemePreference::System, "an unknown word");
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert_eq!(load(&path), ThemePreference::System, "not UTF-8");
    }

    /// Picks written at once (the page tells every change, and the start)
    /// each land whole: no write renames a staged file another is still
    /// writing or has already moved, and none is left behind.
    #[test]
    fn picks_stored_at_once_each_land_whole_and_leave_no_staged_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(THEME_FILE);
        std::thread::scope(|scope| {
            for writer in 0..8 {
                let path = &path;
                scope.spawn(move || {
                    for round in 0..50 {
                        let preference = ALL[(writer + round) % ALL.len()];
                        store(path, preference).unwrap_or_else(|error| {
                            panic!("writer {writer} round {round}: {error}")
                        });
                    }
                });
            }
        });
        let word = std::fs::read_to_string(&path).unwrap();
        assert!(
            ThemePreference::parse(&word).is_some(),
            "the file holds one whole pick, not {word:?}"
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [THEME_FILE], "no staged file is left behind");
    }

    #[test]
    fn the_wire_names_are_the_pages() {
        for preference in ALL {
            let word = format!("\"{}\"", preference.as_str());
            assert_eq!(
                serde_json::from_str::<ThemePreference>(&word).unwrap(),
                preference
            );
            assert_eq!(
                ThemePreference::parse(preference.as_str()),
                Some(preference)
            );
        }
        assert!(serde_json::from_str::<ThemePreference>("\"neon\"").is_err());
    }

    /// The `--background` a `tokens.css` rule sets, as RGB.
    fn css_background(css: &str, selector: &str) -> [u8; 3] {
        let open = format!("{selector} {{");
        let start = css.find(&open).unwrap_or_else(|| panic!("no `{open}` rule"));
        let body = &css[start + open.len()..];
        let body = &body[..body.find('}').expect("the rule closes")];
        let value = body
            .lines()
            .find_map(|line| line.trim().strip_prefix("--background:"))
            .unwrap_or_else(|| panic!("`{selector}` sets no --background"));
        let hex = value.trim().trim_end_matches(';').trim();
        let hex = hex
            .strip_prefix('#')
            .unwrap_or_else(|| panic!("--background {hex} is not #rrggbb"));
        assert_eq!(hex.len(), 6, "--background #{hex} is not #rrggbb");
        let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).unwrap();
        [channel(0), channel(2), channel(4)]
    }

    /// The window's background is the page's own, for each theme, so the
    /// frame before the page paints and the page itself are one colour.
    #[test]
    fn the_window_background_is_the_pages_background_token() {
        let css = include_str!("../ui/src/theme/tokens.css");
        assert_eq!(Theme::Dark.background(), css_background(css, ":root"));
        assert_eq!(
            Theme::Light.background(),
            css_background(css, ":root.light")
        );
    }
}
