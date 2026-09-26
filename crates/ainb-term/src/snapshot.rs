//! Snapshot serialiser: the ANSI repaint of an emulator (R2-plan S7, T16).
//!
//! A snapshot is a byte stream that, replayed into a fresh terminal, rebuilds
//! the grid, the cell attributes, the OSC 8 links, the title, the alt-screen
//! flag, the scroll region, the modes and the cursor. Every existing client
//! emulator (xterm.js on desktop, web and phone; vt100 in the TUI) consumes
//! it with no new model, and [`crate::viewer::ViewerQueue::push_snapshot`]
//! chunks it for the wire.
//!
//! Order, from spike 2 section 5 with one correction:
//!
//! 1. `?1049h` or `?1049l` BEFORE the grid, or the rows paint into the wrong
//!    buffer.
//! 2. the title.
//! 3. `ESC[r` and `?6l` before the grid, plus autowrap on, insert off, ASCII
//!    charsets, a clean pen and no open link, so the repaint is neither
//!    clipped by a region nor reinterpreted by origin mode.
//! 4. the history rows then the viewport rows, painted sequentially with
//!    CRLF so the history scrolls into the client's scrollback.
//! 5. the real scroll region, then every mode explicitly on or off, origin
//!    mode included.
//! 6. the cursor, AFTER origin mode. The spike put it before; both xterm
//!    and wezterm home the cursor on DECOM, so a cursor set first is lost.
//!    Under origin mode the cursor is addressed relative to the region.
//! 7. cursor visibility last.
//!
//! Clients send RIS before applying a snapshot; the head of the snapshot also
//! resets everything it depends on, so a client that did not is still
//! repainted correctly except for stale scrollback.
//!
//! Known ceiling: only the ACTIVE screen is painted. When the app is on the
//! alternate screen the primary screen is not carried, so a client sees a
//! blank primary screen when the app exits alt mode until the app repaints.
//! The emulator does not expose its inactive screen.

use std::fmt::Write as _;

use wezterm_term::color::ColorAttribute;
use wezterm_term::{CellAttributes, Intensity, Line, Underline};

use crate::canon::{lines, link_key};
use crate::emulator::{Modes, PaneEmulator};

const ESC: &str = "\x1b";

/// The repaint bytes for `pane`, with up to `scrollback_rows` rows of
/// history above the viewport.
pub fn snapshot(pane: &mut PaneEmulator, scrollback_rows: usize) -> Vec<u8> {
    let _ = pane.flush();
    let (cols, _rows) = pane.size();
    let cols = usize::from(cols);
    let modes = pane.modes();
    let mut out = String::new();

    // 1. the buffer, 2. the title, 3. a clean slate.
    out.push_str(if pane.is_alt_screen() {
        "\x1b[?1049h"
    } else {
        "\x1b[?1049l"
    });
    let _ = write!(out, "{ESC}]0;{}\x07", pane.title());
    out.push_str("\x1b[r\x1b[?6l\x1b[?7h\x1b[4l\x1b(B\x0f\x1b[0m\x1b]8;;\x1b\\\x1b[H\x1b[2J");

    // 4. the rows.
    let (history, viewport) = lines(pane, scrollback_rows);
    let mut pen = Pen::default();
    let mut first = true;
    for line in history.iter().chain(viewport.iter()) {
        if !first {
            out.push_str("\r\n");
        }
        first = false;
        emit_row(&mut out, &mut pen, line, cols);
    }
    out.push_str("\x1b[0m");
    if !pen.link.is_empty() {
        out.push_str("\x1b]8;;\x1b\\");
    }

    // 5. region and modes.
    if let Some((top, bottom)) = modes.scroll_region {
        let _ = write!(out, "{ESC}[{};{}r", top + 1, bottom + 1);
    }
    emit_modes(&mut out, modes);

    // 6. the cursor, relative to the region under origin mode. 7. visibility.
    let cursor = pane.cursor();
    let row = if modes.origin {
        let top = modes.scroll_region.map_or(0, |(t, _)| t);
        cursor.row.saturating_sub(top)
    } else {
        cursor.row
    };
    let _ = write!(
        out,
        "{ESC}[{};{}H",
        u32::from(row) + 1,
        u32::from(cursor.col) + 1
    );
    out.push_str(if cursor.visible {
        "\x1b[?25h"
    } else {
        "\x1b[?25l"
    });
    out.into_bytes()
}

fn emit_modes(out: &mut String, m: &Modes) {
    let dec = |out: &mut String, code: u16, on: bool| {
        let _ = write!(out, "{ESC}[?{code}{}", if on { 'h' } else { 'l' });
    };
    // tmux keeps one tracking mode; every other one is left off.
    for (mode, code) in [
        (crate::emulator::MouseTracking::Standard, 1000),
        (crate::emulator::MouseTracking::Button, 1002),
        (crate::emulator::MouseTracking::Any, 1003),
    ] {
        if m.mouse == mode {
            dec(out, code, true);
        }
    }
    if !m.wants_mouse() {
        dec(out, 1000, false);
    }
    dec(out, 1005, m.mouse_utf8);
    dec(out, 1006, m.mouse_sgr);
    dec(out, 1004, m.focus_tracking);
    dec(out, 2004, m.bracketed_paste);
    dec(out, 1, m.application_cursor_keys);
    dec(out, 7, m.auto_wrap);
    out.push_str(if m.application_keypad {
        "\x1b="
    } else {
        "\x1b>"
    });
    out.push_str(if m.insert { "\x1b[4h" } else { "\x1b[4l" });
    dec(out, 6, m.origin);
}

/// The style in effect on the output side.
#[derive(Debug, Default, PartialEq, Eq)]
struct Pen {
    sgr: String,
    link: String,
}

/// The SGR parameters (without `ESC[` and `m`) that set `a` from a reset
/// pen; empty for the default style.
fn sgr_params(a: &CellAttributes) -> String {
    let mut p: Vec<String> = Vec::new();
    match a.intensity() {
        Intensity::Bold => p.push("1".into()),
        Intensity::Half => p.push("2".into()),
        Intensity::Normal => {}
    }
    if a.italic() {
        p.push("3".into());
    }
    match a.underline() {
        Underline::None => {}
        Underline::Single => p.push("4".into()),
        Underline::Double => p.push("21".into()),
        Underline::Curly => p.push("4:3".into()),
        Underline::Dotted => p.push("4:4".into()),
        Underline::Dashed => p.push("4:5".into()),
    }
    match a.blink() {
        wezterm_term::Blink::None => {}
        wezterm_term::Blink::Slow => p.push("5".into()),
        wezterm_term::Blink::Rapid => p.push("6".into()),
    }
    if a.reverse() {
        p.push("7".into());
    }
    if a.invisible() {
        p.push("8".into());
    }
    if a.strikethrough() {
        p.push("9".into());
    }
    if a.overline() {
        p.push("53".into());
    }
    push_color(&mut p, a.foreground(), 30, 90, 38);
    push_color(&mut p, a.background(), 40, 100, 48);
    match a.underline_color() {
        ColorAttribute::Default => {}
        c => push_color(&mut p, c, 0, 0, 58),
    }
    p.join(";")
}

fn push_color(p: &mut Vec<String>, c: ColorAttribute, base: u8, bright: u8, ext: u8) {
    match c {
        ColorAttribute::Default => {}
        ColorAttribute::PaletteIndex(i) if base != 0 && i < 8 => p.push((base + i).to_string()),
        ColorAttribute::PaletteIndex(i) if base != 0 && i < 16 => {
            p.push((bright + i - 8).to_string());
        }
        ColorAttribute::PaletteIndex(i) => p.push(format!("{ext};5;{i}")),
        ColorAttribute::TrueColorWithPaletteFallback(t, _)
        | ColorAttribute::TrueColorWithDefaultFallback(t) => {
            let (r, g, b, _) = t.as_rgba_u8();
            p.push(format!("{ext};2;{r};{g};{b}"));
        }
    }
}

fn is_blank_default(text: &str, a: &CellAttributes) -> bool {
    (text == " " || text.is_empty())
        && a.attribute_bits_equal(&CellAttributes::default())
        && a.foreground() == ColorAttribute::Default
        && a.background() == ColorAttribute::Default
        && a.underline_color() == ColorAttribute::Default
        && a.hyperlink().is_none()
}

/// Paint one row from the cursor, up to its last non-default cell.
fn emit_row(out: &mut String, pen: &mut Pen, line: &Line, cols: usize) {
    let cells: Vec<_> = line.visible_cells().take_while(|c| c.cell_index() < cols).collect();
    let last = cells
        .iter()
        .rposition(|c| !is_blank_default(c.str(), c.attrs()))
        .map_or(0, |i| i + 1);
    for c in &cells[..last] {
        let a = c.attrs();
        let sgr = sgr_params(a);
        if sgr != pen.sgr {
            let _ = write!(out, "{ESC}[0m");
            if !sgr.is_empty() {
                let _ = write!(out, "{ESC}[{sgr}m");
            }
            pen.sgr = sgr;
        }
        let link = link_key(a);
        if link != pen.link {
            match a.hyperlink() {
                Some(h) => {
                    let id = h.params().get("id").map_or_else(String::new, |id| format!("id={id}"));
                    let _ = write!(out, "{ESC}]8;{id};{}{ESC}\\", h.uri());
                }
                None => out.push_str("\x1b]8;;\x1b\\"),
            }
            pen.link = link;
        }
        out.push_str(c.str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::canon;

    const FIXTURES: [(&str, &[u8]); 10] = [
        (
            "f1-altscreen",
            include_bytes!("../tests/fixtures/f1-altscreen.bin"),
        ),
        ("f2-mouse", include_bytes!("../tests/fixtures/f2-mouse.bin")),
        (
            "f3-widechars",
            include_bytes!("../tests/fixtures/f3-widechars.bin"),
        ),
        ("f4-osc8", include_bytes!("../tests/fixtures/f4-osc8.bin")),
        (
            "f5-oscstatus",
            include_bytes!("../tests/fixtures/f5-oscstatus.bin"),
        ),
        (
            "f6-scroll",
            include_bytes!("../tests/fixtures/f6-scroll.bin"),
        ),
        (
            "f7-decstbm",
            include_bytes!("../tests/fixtures/f7-decstbm.bin"),
        ),
        (
            "f8-widthprobe",
            include_bytes!("../tests/fixtures/f8-widthprobe.bin"),
        ),
        ("f9-live", include_bytes!("../tests/fixtures/f9-live.bin")),
        (
            "f11-altlink",
            include_bytes!("../tests/fixtures/f11-altlink.bin"),
        ),
    ];

    fn fed(bytes: &[u8], cols: u16, rows: u16) -> PaneEmulator {
        let mut p = PaneEmulator::new(cols, rows, 1000);
        // Real arrival boundaries are arbitrary: feed in odd chunks.
        for c in bytes.chunks(7) {
            p.feed(c).unwrap();
        }
        p
    }

    /// The round trip the plan names: `canon(emu) == canon(fresh.feed(snapshot(emu)))`.
    fn round_trip(name: &str, p: &mut PaneEmulator, scrollback_rows: usize) {
        let (cols, rows) = p.size();
        let snap = snapshot(p, scrollback_rows);
        let mut fresh = PaneEmulator::new(cols, rows, 1000);
        for c in snap.chunks(11) {
            fresh.feed(c).unwrap();
        }
        let want = canon(p, scrollback_rows).render();
        let got = canon(&mut fresh, scrollback_rows).render();
        assert!(
            want == got,
            "{name} at {cols}x{rows} with {scrollback_rows} history rows differs\n--- original\n{want}\n--- replayed\n{got}\n--- snapshot\n{}",
            String::from_utf8_lossy(&snap).replace('\x1b', "^[")
        );
    }

    #[test]
    fn every_fixture_round_trips_at_both_sizes() {
        for (name, bytes) in FIXTURES {
            for (cols, rows) in [(40, 20), (120, 40)] {
                let mut p = fed(bytes, cols, rows);
                round_trip(name, &mut p, 0);
            }
        }
    }

    #[test]
    fn history_rows_round_trip_into_the_client_scrollback() {
        let mut p = fed(FIXTURES[5].1, 40, 20);
        let c = canon(&mut p, 30);
        assert_eq!(
            c.scrollback.len(),
            30,
            "f6 at 40 wide wraps 60 lines past 20 rows"
        );
        round_trip("f6-scroll", &mut p, 30);
        round_trip("f6-scroll", &mut p, 1);
        round_trip("f6-scroll", &mut p, 5000);
        let mut p = fed(FIXTURES[5].1, 120, 40);
        round_trip("f6-scroll", &mut p, 10);
    }

    #[test]
    fn the_snapshot_head_is_ordered_as_the_spike_proved() {
        let mut p = fed(FIXTURES[0].1, 40, 20);
        let snap = String::from_utf8(snapshot(&mut p, 0)).unwrap();
        assert!(snap.starts_with("\x1b[?1049h\x1b]0;spike2 agent panel\x07\x1b[r\x1b[?6l\x1b[?7h\x1b[4l\x1b(B\x0f\x1b[0m\x1b]8;;\x1b\\\x1b[H\x1b[2J"));
        // f1 ends with `ESC[r`, so no region; the cursor at 10;12 shown.
        assert!(snap.ends_with("\x1b[?1000l\x1b[?1005l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1l\x1b[?7h\x1b>\x1b[4l\x1b[?6l\x1b[10;12H\x1b[?25h"), "{}", snap.replace('\x1b', "^["));
        let mut p = fed(FIXTURES[1].1, 40, 20);
        let snap = String::from_utf8(snapshot(&mut p, 0)).unwrap();
        assert!(
            snap.starts_with("\x1b[?1049l\x1b]0;wezterm\x07"),
            "primary screen, crate default title"
        );
        assert!(snap.contains("\x1b[?1002h\x1b[?1005l\x1b[?1006h\x1b[?1004h\x1b[?2004h\x1b[?1h"));
    }

    #[test]
    fn the_cursor_is_addressed_relative_to_the_region_under_origin_mode() {
        let mut p = PaneEmulator::new(40, 20, 100);
        p.feed(b"\x1b[6;18r\x1b[?6h\x1b[3;9Hx").unwrap();
        p.flush().unwrap();
        assert_eq!((p.cursor().row, p.cursor().col), (7, 9), "absolute row 7");
        let snap = String::from_utf8(snapshot(&mut p, 0)).unwrap();
        assert!(snap.ends_with("\x1b[6;18r\x1b[?1000l\x1b[?1005l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1l\x1b[?7h\x1b>\x1b[4l\x1b[?6h\x1b[3;10H\x1b[?25h"), "{}", snap.replace('\x1b', "^["));
        round_trip("origin", &mut p, 0);
        // Turning origin off afterwards must not move the replayed cursor
        // somewhere else than the original.
        p.feed(b"\x1b[?6l\x1b[2;3H").unwrap();
        round_trip("origin-off", &mut p, 0);
    }

    #[test]
    fn sgr_covers_every_attribute_and_colour_form() {
        let mut p = PaneEmulator::new(60, 4, 10);
        for sgr in [
            "1;3;4;5;7;9;53;38;5;200",
            "48;2;9;8;7",
            "58;5;3;4:3",
            "2;8;92;44;21",
            "31;102",
            "58;2;1;2;3",
            "4:4",
            "4:5;6",
        ] {
            p.feed(format!("\x1b[0m\x1b[{sgr}mX").as_bytes()).unwrap();
        }
        round_trip("sgr", &mut p, 0);
        let snap = String::from_utf8(snapshot(&mut p, 0)).unwrap();
        let shown = snap.replace('\x1b', "^[");
        for want in [
            "\x1b[1;3;4;5;7;9;53;38;5;200mX",
            "\x1b[48;2;9;8;7mX",
            "\x1b[4:3;58;5;3mX",
            "\x1b[2;21;8;92;44mX",
            "\x1b[31;102mX",
            "\x1b[58;2;1;2;3mX",
            "\x1b[4:4mX",
            "\x1b[4:5;6mX",
        ] {
            assert!(
                snap.contains(want),
                "missing {:?} in {shown}",
                want.replace('\x1b', "^[")
            );
        }
    }

    #[test]
    fn a_wide_glyph_at_the_margin_and_a_full_row_replay_exactly() {
        let mut p = PaneEmulator::new(10, 4, 10);
        // A full row of narrow cells, a row ending in a wide glyph, the
        // fork's margin case (one column free), and a wrapped continuation.
        p.feed("0123456789\r\n01234567\u{4E2D}\r\n012345678\u{4E2D}ZZ\r\n".as_bytes())
            .unwrap();
        round_trip("margins", &mut p, 0);
    }
}
