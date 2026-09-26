//! Seed replay: rebuild an emulator from `capture-pane -p -e` rows plus the
//! tmux format flags (R2-plan S3, spike 2 section 5).
//!
//! A control client receives nothing produced before it attached, and after
//! `%continue` the stream is not a continuation, so every attach and every
//! resume seeds the emulator from tmux's own grid before applying deltas.
//! `capture-pane -e` carries SGR and OSC 8 but nothing outside the character
//! grid; the alt-screen flag, title, cursor, scroll region and modes come
//! from one `display-message` on the same ordered control stream:
//!
//! ```text
//! display-message -p -t %N "<SEED_FLAGS_FORMAT>"   ─▶ SeedState::parse
//! display-message -p -t %N "<SEED_TITLE_FORMAT>"   ─▶ title
//! capture-pane -p -e -t %N                        ─▶ rows
//!                                     seed_repaint ─▶ PaneEmulator::feed
//! ```
//!
//! The daemon (lane D, WP8) issues the three commands and hands the reply
//! bodies here; this module only knows the format strings and the replay.
//!
//! tmux's `mouse_any_flag` means "any mouse mode is on"; the flag for
//! `?1003` is `mouse_all_flag` (tmux 3.4 with only `?1002h` reports
//! `std=0 btn=1 any=1 all=0`). Spike 2's "over-report" was this misreading.
//! The seed uses `mouse_all_flag`, and the tracker in
//! [`crate::emulator::Modes`] takes over from the first DECSET it sees.
//!
//! Known ceilings, all because tmux has no format for the state:
//!
//! * Bracketed paste (`?2004`) and focus reporting (`?1004`) are unknown at
//!   seed time, so after every seed (every attach, every `%continue`) the
//!   model says both are off until the app re-sends them, and Claude Code
//!   sends `?2004h` once at startup. A client that pastes by keystrokes
//!   would submit a multi-line paste line by line. WP9b therefore sends
//!   pastes through `set-buffer` plus `paste-buffer -p`, so tmux applies
//!   the pane's real mode.
//! * The charset in use (`ESC ( 0`, SO) is unknown; `capture-pane -e`
//!   returns the drawn glyphs, not the designation. A pane mid-way through
//!   DEC line drawing prints `q` for a line until it re-designates.

use std::fmt::Write as _;

/// The `display-message -p` format whose reply [`SeedState::parse`] reads.
pub const SEED_FLAGS_FORMAT: &str = "#{alternate_on} #{cursor_flag} #{cursor_y} #{cursor_x} #{scroll_region_upper} #{scroll_region_lower} #{keypad_cursor_flag} #{keypad_flag} #{wrap_flag} #{origin_flag} #{insert_flag} #{mouse_standard_flag} #{mouse_button_flag} #{mouse_all_flag} #{mouse_sgr_flag} #{mouse_utf8_flag}";

/// The `display-message -p` format for the title.
pub const SEED_TITLE_FORMAT: &str = "#{pane_title}";

/// The terminal state tmux keeps outside the character grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SeedState {
    /// `#{alternate_on}`.
    pub alt: bool,
    /// `#{cursor_flag}`: DECTCEM.
    pub cursor_visible: bool,
    /// `#{cursor_y}`, absolute, zero-based.
    pub cursor_y: u16,
    /// `#{cursor_x}`, zero-based.
    pub cursor_x: u16,
    /// `#{scroll_region_upper}`, zero-based.
    pub region_upper: u16,
    /// `#{scroll_region_lower}`, zero-based, inclusive.
    pub region_lower: u16,
    /// `#{keypad_cursor_flag}`: DECCKM.
    pub app_cursor: bool,
    /// `#{keypad_flag}`: DECKPAM.
    pub app_keypad: bool,
    /// `#{wrap_flag}`: DECAWM.
    pub wrap: bool,
    /// `#{origin_flag}`: DECOM.
    pub origin: bool,
    /// `#{insert_flag}`: IRM.
    pub insert: bool,
    /// `#{mouse_standard_flag}`: `?1000`.
    pub mouse_standard: bool,
    /// `#{mouse_button_flag}`: `?1002`.
    pub mouse_button: bool,
    /// `#{mouse_all_flag}`: `?1003`.
    pub mouse_all: bool,
    /// `#{mouse_sgr_flag}`: `?1006`.
    pub mouse_sgr: bool,
    /// `#{mouse_utf8_flag}`: `?1005`.
    pub mouse_utf8: bool,
}

impl SeedState {
    /// Parse the reply line to [`SEED_FLAGS_FORMAT`]: sixteen
    /// space-separated integers.
    pub fn parse(line: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(line).ok()?;
        let f: Vec<u32> = text.split_whitespace().map(str::parse).collect::<Result<_, _>>().ok()?;
        if f.len() != 16 {
            return None;
        }
        let flag = |i: usize| f[i] != 0;
        let n = |i: usize| u16::try_from(f[i]).ok();
        Some(Self {
            alt: flag(0),
            cursor_visible: flag(1),
            cursor_y: n(2)?,
            cursor_x: n(3)?,
            region_upper: n(4)?,
            region_lower: n(5)?,
            app_cursor: flag(6),
            app_keypad: flag(7),
            wrap: flag(8),
            origin: flag(9),
            insert: flag(10),
            mouse_standard: flag(11),
            mouse_button: flag(12),
            mouse_all: flag(13),
            mouse_sgr: flag(14),
            mouse_utf8: flag(15),
        })
    }
}

/// The bytes that rebuild a `rows`-tall pane from `capture` (the
/// `capture-pane -p -e` reply lines, raw), `title` and `state`.
///
/// Order: the buffer, the title (C0 and C1 controls stripped), a clean
/// slate, one absolutely positioned row per line, then the region, the
/// modes, origin mode, the cursor (relative to the region under origin
/// mode, because DECOM homes it) and cursor visibility last.
pub fn seed_repaint<R: AsRef<[u8]>>(
    capture: &[R],
    title: &[u8],
    state: &SeedState,
    rows: u16,
) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(if state.alt {
        b"\x1b[?1049h"
    } else {
        b"\x1b[?1049l"
    });
    out.extend_from_slice(b"\x1b]0;");
    out.extend_from_slice(
        crate::snapshot::sanitize_title(&String::from_utf8_lossy(title)).as_bytes(),
    );
    out.extend_from_slice(b"\x07");
    out.extend_from_slice(
        b"\x1b[r\x1b[?6l\x1b[?7h\x1b[4l\x1b(B\x0f\x1b[0m\x1b]8;;\x1b\\\x1b[H\x1b[2J",
    );
    for (i, row) in capture.iter().enumerate().take(usize::from(rows)) {
        let mut s = String::new();
        let _ = write!(s, "\x1b[{};1H", i + 1);
        out.extend_from_slice(s.as_bytes());
        out.extend_from_slice(row.as_ref());
        out.extend_from_slice(b"\x1b[0m\x1b]8;;\x1b\\");
    }
    let mut tail = String::new();
    let _ = write!(
        tail,
        "\x1b[{};{}r",
        u32::from(state.region_upper) + 1,
        u32::from(state.region_lower) + 1
    );
    // tmux keeps one tracking mode and clears all of them on any reset, so
    // only the set flags are turned on (the last one wins) and a single
    // reset stands for none.
    let tracking = [
        (state.mouse_standard, 1000),
        (state.mouse_button, 1002),
        (state.mouse_all, 1003),
    ];
    if tracking.iter().any(|(on, _)| *on) {
        for (on, code) in tracking {
            if on {
                let _ = write!(tail, "\x1b[?{code}h");
            }
        }
    } else {
        tail.push_str("\x1b[?1000l");
    }
    for (on, code) in [
        (state.mouse_utf8, 1005),
        (state.mouse_sgr, 1006),
        (state.app_cursor, 1),
        (state.wrap, 7),
    ] {
        let _ = write!(tail, "\x1b[?{code}{}", if on { 'h' } else { 'l' });
    }
    tail.push_str(if state.app_keypad { "\x1b=" } else { "\x1b>" });
    tail.push_str(if state.insert { "\x1b[4h" } else { "\x1b[4l" });
    let row = if state.origin {
        tail.push_str("\x1b[?6h");
        state.cursor_y.saturating_sub(state.region_upper)
    } else {
        state.cursor_y
    };
    let _ = write!(
        tail,
        "\x1b[{};{}H",
        u32::from(row) + 1,
        u32::from(state.cursor_x) + 1
    );
    tail.push_str(if state.cursor_visible {
        "\x1b[?25h"
    } else {
        "\x1b[?25l"
    });
    out.extend_from_slice(tail.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::canon;
    use crate::emulator::{MouseTracking, PaneEmulator};
    use crate::snapshot::snapshot;

    /// Recorded on tmux 3.6a, 120x40, from a pane that entered the alt
    /// screen, set a title, hid the cursor, turned on `?1000 ?1002 ?1006`
    /// (tmux reports `mouse_button_flag=1`, `mouse_all_flag=0`),
    /// painted an OSC 8 link, SGR runs, truecolor, a coloured background and
    /// wide glyphs, set a scroll region with origin mode, DECCKM and DECKPAM,
    /// and parked the cursor at region-relative 3;9.
    const FLAGS: &[u8] = include_bytes!("../tests/fixtures/f11-seed.flags");
    const TITLE: &[u8] = include_bytes!("../tests/fixtures/f11-seed.title");
    const CAPTURE: &[u8] = include_bytes!("../tests/fixtures/f11-seed.capture");

    fn reply_lines(body: &[u8]) -> Vec<Vec<u8>> {
        body.split(|b| *b == b'\n').map(<[u8]>::to_vec).collect()
    }

    fn seeded() -> PaneEmulator {
        let state = SeedState::parse(FLAGS.strip_suffix(b"\n").unwrap()).unwrap();
        let mut rows = reply_lines(CAPTURE);
        if rows.last().is_some_and(Vec::is_empty) {
            rows.pop();
        }
        assert_eq!(rows.len(), 40);
        let bytes = seed_repaint(&rows, TITLE.strip_suffix(b"\n").unwrap(), &state, 40);
        let mut p = PaneEmulator::new(120, 40, 1000);
        p.feed(&bytes).unwrap();
        p
    }

    #[test]
    fn the_flags_line_parses_as_recorded() {
        let s = SeedState::parse(b"1 0 7 8 5 17 1 1 1 1 0 0 1 0 1 0\n").unwrap();
        assert_eq!(
            s,
            SeedState {
                alt: true,
                cursor_visible: false,
                cursor_y: 7,
                cursor_x: 8,
                region_upper: 5,
                region_lower: 17,
                app_cursor: true,
                app_keypad: true,
                wrap: true,
                origin: true,
                insert: false,
                mouse_standard: false,
                mouse_button: true,
                mouse_all: false,
                mouse_sgr: true,
                mouse_utf8: false,
            }
        );
        assert_eq!(SeedState::parse(b"1 0 7"), None);
        assert_eq!(SeedState::parse(b"x 0 7 8 5 17 1 1 1 1 0 0 1 1 1 0"), None);
        assert_eq!(SEED_FLAGS_FORMAT.matches("#{").count(), 16);
    }

    /// The six rows of the spike 2 section 5 state table, plus the region,
    /// origin, DECCKM and DECKPAM this recording adds.
    #[test]
    fn a_complete_seed_restores_what_a_grid_only_seed_drops() {
        let mut p = seeded();
        let c = canon(&mut p, 0);
        assert!(c.alt, "alternate screen");
        assert_eq!(c.title, "spike2 altlink", "window title");
        assert!(!c.cursor_visible, "cursor visibility");
        assert_eq!(
            (c.cursor_row, c.cursor_col),
            (7, 8),
            "cursor position, absolute"
        );
        let m = p.modes();
        assert_eq!(
            m.mouse,
            MouseTracking::Button,
            "button tracking, not all motion"
        );
        assert!(m.mouse_sgr && !m.mouse_utf8, "mouse reporting");
        assert_eq!(m.scroll_region, Some((5, 17)));
        assert!(m.origin && m.application_cursor_keys && m.application_keypad && m.auto_wrap);
        assert!(!m.insert);
        // OSC 8 hyperlink and SGR attributes come from capture-pane -e itself.
        assert_eq!(c.grid[1][0].link, "https://example.invalid/seed");
        assert_eq!(c.grid[1][7].link, "https://example.invalid/seed");
        assert_eq!(c.grid[1][8].link, "");
        assert_eq!(c.row_text(1), "SEEDLINK plain");
        assert_eq!(c.grid[0][0].fg, "i39");
        assert_eq!(c.grid[2][0].attrs, "b,u,rev");
        assert_eq!(c.grid[2][15].fg, "#ff8000");
        assert_eq!(c.grid[2][25].bg, "i236");
        assert_eq!(
            c.row_text(2),
            "bold-under-rev truecolor  dimbg  \u{4E2D}\u{6587} \u{1F680}"
        );
        assert!(c.render().contains("r002 w 33,35,38\n"), "{}", c.render());
        assert_eq!(
            c.row_text(8),
            "tick 0003",
            "CUP 4;1 under origin mode, region top 5"
        );
        assert_eq!(c.grid[8][0].attrs, "b");
    }

    #[test]
    fn a_seeded_pane_snapshots_and_round_trips() {
        let mut p = seeded();
        let snap = snapshot(&mut p, 0).unwrap();
        let mut fresh = PaneEmulator::new(120, 40, 1000);
        fresh.feed(&snap).unwrap();
        assert_eq!(canon(&mut fresh, 0).render(), canon(&mut p, 0).render());
    }

    #[test]
    fn the_seed_bytes_are_ordered_as_the_spike_proved() {
        let state = SeedState::parse(b"0 1 2 3 0 9 0 0 1 0 0 0 0 0 0 0").unwrap();
        let bytes = seed_repaint(
            &[b"one".as_slice(), b"two"],
            "T\u{9c}\x07".as_bytes(),
            &state,
            10,
        );
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(
            s,
            "\x1b[?1049l\x1b]0;T\x07\x1b[r\x1b[?6l\x1b[?7h\x1b[4l\x1b(B\x0f\x1b[0m\x1b]8;;\x1b\\\x1b[H\x1b[2J\
             \x1b[1;1Hone\x1b[0m\x1b]8;;\x1b\\\x1b[2;1Htwo\x1b[0m\x1b]8;;\x1b\\\
             \x1b[1;10r\x1b[?1000l\x1b[?1005l\x1b[?1006l\x1b[?1l\x1b[?7h\x1b>\x1b[4l\x1b[3;4H\x1b[?25h"
        );
        // More capture rows than the pane is tall are cut, not painted past it.
        let bytes = seed_repaint(&[b"a".as_slice(), b"b", b"c"], b"", &state, 2);
        assert!(!String::from_utf8(bytes).unwrap().contains("\x1b[3;1H"));
    }
}
