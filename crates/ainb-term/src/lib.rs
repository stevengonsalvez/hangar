//! Headless terminal emulation for the Hangar terminal stream (R2).
//!
//! The daemon feeds each watched pane's tmux control-mode output into one
//! [`Terminal`] per pane and serves snapshots from it. This crate is the pure
//! half: no daemon, tmux or IO dependencies.
//!
//! The emulator is `wezterm-term`, taken from the crates.io fork
//! `tattoy-wezterm-term` (renamed back to `wezterm_term` by the workspace
//! dependency). Spike 2 chose it because it is the only candidate whose cell
//! advance matches tmux 3.4 on every glyph tested, and only with
//! [`UNICODE_VERSION`] set to 14; the unit test below pins that table.

pub mod canon;
pub mod control;
pub mod emulator;
pub mod floor;
pub mod seed;
pub mod snapshot;
pub mod viewer;

use std::sync::Arc;

use wezterm_term::color::ColorPalette;
pub use wezterm_term::{Terminal, TerminalSize};
use wezterm_term::{TerminalConfiguration, UnicodeVersion};

/// Unicode width tables the emulator uses. At the crate default (9) an
/// emoji-presentation sequence such as U+2764 U+FE0F is one cell wide, where
/// tmux 3.4 draws it two wide.
pub const UNICODE_VERSION: u8 = 14;

/// Default live window: scrollback rows the emulator keeps per pane.
pub const DEFAULT_LIVE_ROWS: usize = 1_000;

/// Emulator configuration shared by every pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermConfig {
    /// Scrollback rows kept beyond the visible screen.
    pub live_rows: usize,
}

impl Default for TermConfig {
    fn default() -> Self {
        Self {
            live_rows: DEFAULT_LIVE_ROWS,
        }
    }
}

impl TerminalConfiguration for TermConfig {
    fn scrollback_size(&self) -> usize {
        self.live_rows
    }

    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }

    fn unicode_version(&self) -> UnicodeVersion {
        UnicodeVersion {
            version: UNICODE_VERSION,
            ambiguous_are_wide: false,
            cell_widths: None,
        }
    }
}

/// A headless emulator of `cols` x `rows` cells.
///
/// Replies the emulator would send to the application (device attributes,
/// cursor reports) are discarded: tmux is the terminal the application talks
/// to, and it answers those itself.
pub fn new_terminal(cols: usize, rows: usize, config: TermConfig) -> Terminal {
    let size = TerminalSize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    };
    Terminal::new(
        size,
        Arc::new(config),
        "ainb",
        env!("CARGO_PKG_VERSION"),
        Box::new(std::io::sink()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cell advance per glyph, from spike 2 section 3c: each glyph is written
    /// at column 0 and the cursor column read back. The expected column is
    /// what tmux 3.4 reported for the same bytes (`#{cursor_x}`), so a
    /// mismatch here means the daemon's grid and a `tmux attach` user's grid
    /// disagree on that row.
    const CELL_ADVANCE: [(&str, &str, usize); 13] = [
        ("ASCII A", "A", 1),
        ("CJK U+4E2D", "\u{4E2D}", 2),
        ("emoji U+1F600", "\u{1F600}", 2),
        ("U+2764 bare", "\u{2764}", 1),
        ("U+2764 + VS16", "\u{2764}\u{FE0F}", 2),
        ("U+2764 + VS15", "\u{2764}\u{FE0E}", 1),
        ("e + U+0301", "e\u{0301}", 1),
        (
            "ZWJ U+1F469 U+200D U+1F4BB",
            "\u{1F469}\u{200D}\u{1F4BB}",
            2,
        ),
        (
            "tag flag U+1F3F4",
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
            2,
        ),
        ("halfwidth U+FF71", "\u{FF71}", 1),
        ("block U+2588", "\u{2588}", 1),
        ("box U+2500", "\u{2500}", 1),
        ("U+00E9 precomposed", "\u{00E9}", 1),
    ];

    fn advance(glyph: &str) -> usize {
        let mut term = new_terminal(60, 20, TermConfig::default());
        term.advance_bytes(b"\x1b[2J\x1b[H");
        term.advance_bytes(glyph.as_bytes());
        term.cursor_pos().x
    }

    #[test]
    fn cell_advance_matches_tmux_3_4() {
        let wrong: Vec<String> = CELL_ADVANCE
            .iter()
            .filter_map(|&(label, glyph, want)| {
                let got = advance(glyph);
                (got != want).then(|| format!("{label}: tmux {want}, emulator {got}"))
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "cell advance differs from tmux 3.4:\n{}",
            wrong.join("\n")
        );
    }

    #[test]
    fn default_unicode_version_would_break_vs16() {
        // Guards the reason UNICODE_VERSION exists: at the crate default the
        // VS16 row is one cell narrower than tmux.
        #[derive(Debug)]
        struct Unicode9;
        impl TerminalConfiguration for Unicode9 {
            fn color_palette(&self) -> ColorPalette {
                ColorPalette::default()
            }
        }
        let size = TerminalSize {
            rows: 20,
            cols: 60,
            pixel_width: 0,
            pixel_height: 0,
            dpi: 0,
        };
        let mut term = Terminal::new(
            size,
            Arc::new(Unicode9),
            "ainb",
            "0",
            Box::new(std::io::sink()),
        );
        term.advance_bytes("\u{2764}\u{FE0F}".as_bytes());
        assert_eq!(term.cursor_pos().x, 1);
    }
}
