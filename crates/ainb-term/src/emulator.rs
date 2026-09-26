//! One pane's headless emulator plus the mode tracker the snapshot needs.
//!
//! `wezterm-term` keeps its DEC modes, scroll region and keypad state in
//! private fields, and the fork pinned by the workspace has no accessors for
//! them. The snapshot (R2-plan S7) has to replay them, and the phone has to
//! know whether the pane wants mouse events at all (spike 2 section 3e), so
//! [`PaneEmulator`] parses the byte stream itself with the same escape parser
//! the emulator uses, records every mode change in [`Modes`], and then hands
//! the parsed actions to the emulator. One parse, two consumers, no drift
//! between what the grid did and what the tracker saw.
//!
//! ```text
//! bytes ──▶ escape parser ──▶ actions ──┬──▶ Modes (tracker)
//!                                       └──▶ wezterm Terminal (grid)
//! ```
//!
//! Mouse modes follow tmux, not xterm: `?1000h`, `?1002h` and `?1003h`
//! replace each other (one tracking mode at a time) and any of their resets
//! clears tracking, while `?1005` and `?1006` are independent encodings.
//! tmux's `#{mouse_any_flag}` means "any mouse mode is on" (`mouse_all_flag`
//! is `?1003`), which is why the tracker, not the tmux formats, is the source
//! of truth after the seed.
//!
//! The tracker mirrors what the emulator saves and restores: DECSC/DECRC and
//! `CSI s`/`CSI u` save and restore origin mode and the G0/G1 charsets per
//! screen, and `?1049h`/`?1049l` save on the primary screen and restore on
//! the way back, exactly as `TerminalState::dec_save_cursor` does. Not
//! tracked, because the emulator does not expose it: a pending autowrap
//! (the cursor sits on the last column after a full row); see `snapshot`.
//!
//! A panic inside the emulator is contained: the fork lacks upstream's fix for
//! a divide by zero in inline image placement, and crafted agent output must
//! not take the daemon down. After a panic the pane is [`Poisoned`] and every
//! further feed is refused, so the daemon replaces it and re-seeds.
//!
//! The last grapheme cluster of a feed is held back until the next feed or a
//! [`PaneEmulator::flush`]. Measured on the fork: `a` in one batch and
//! U+030A in the next leaves a bare `a`, and so does a split inside the
//! mark's UTF-8 bytes. tmux ends notifications mid-grapheme under load
//! (spike 2 section 1), so without the hold a phone would show accents,
//! ZWJ emoji and flags dropping at random. Only the last cluster is held
//! (at most [`MAX_HELD_BYTES`]); everything before it is performed at once,
//! so a feed with no control byte in it still costs linear time and shows
//! immediately. The cost is that the last glyph of a feed shows a few
//! milliseconds late.
//!
//! Known ceiling: a mark that arrives AFTER a flush (idle, snapshot or
//! resize) meets a base that was already performed, and the fork drops it,
//! so the daemon's grid keeps the bare base until the app repaints. Clients
//! are unaffected, they get the raw tail bytes; only a later snapshot shows
//! the bare base. tmux splits mid-grapheme under load, when the feed is not
//! idle, so this is rare; `a_mark_after_a_flush_is_the_documented_loss` pins
//! the behaviour.

use std::panic::{AssertUnwindSafe, catch_unwind};

use unicode_segmentation::UnicodeSegmentation;
use wezterm_escape_parser::csi::{
    CSI, Cursor, DecPrivateMode, DecPrivateModeCode, Device, Mode, TerminalMode, TerminalModeCode,
};
use wezterm_escape_parser::parser::Parser;
use wezterm_escape_parser::{Action, ControlCode, Esc, EscCode};
use wezterm_term::{CursorPosition, Terminal, TerminalSize};

use crate::{TermConfig, new_terminal};

/// Environment variable that sets the live window (scrollback rows kept per
/// pane). Clamped to [`MIN_LIVE_ROWS`]..=[`MAX_LIVE_ROWS`]; unset or
/// unparsable means [`crate::DEFAULT_LIVE_ROWS`].
pub const LIVE_ROWS_ENV: &str = "AINB_TERM_LIVE_ROWS";
/// Smallest live window the env var can ask for.
pub const MIN_LIVE_ROWS: usize = 100;
/// The most bytes [`PaneEmulator::feed`] holds back: one grapheme cluster.
/// A longer cluster (a pile of combining marks) is performed at once.
pub const MAX_HELD_BYTES: usize = 64;
/// Largest live window the env var can ask for.
pub const MAX_LIVE_ROWS: usize = 5_000;

/// The live window from [`LIVE_ROWS_ENV`], clamped.
pub fn live_rows_from_env() -> usize {
    live_rows_from(std::env::var(LIVE_ROWS_ENV).ok().as_deref())
}

/// The live window a raw env value means: unset or unparsable gives the
/// default, anything else is clamped to the allowed range.
pub fn live_rows_from(value: Option<&str>) -> usize {
    value
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map_or(crate::DEFAULT_LIVE_ROWS, |n| {
            n.clamp(MIN_LIVE_ROWS, MAX_LIVE_ROWS)
        })
}

/// Which mouse tracking mode the pane asked for. tmux keeps exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseTracking {
    /// No mouse reporting.
    #[default]
    Off,
    /// `?1000`: button press and release.
    Standard,
    /// `?1002`: presses, releases and drag motion.
    Button,
    /// `?1003`: every motion event.
    Any,
}

impl MouseTracking {
    /// The DEC private mode number that selects this tracking, if any.
    pub const fn code(self) -> Option<u16> {
        match self {
            Self::Off => None,
            Self::Standard => Some(1000),
            Self::Button => Some(1002),
            Self::Any => Some(1003),
        }
    }
}

/// A designated character set, as the emulator supports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Charset {
    /// `ESC ( B` / `ESC ) B`.
    #[default]
    Ascii,
    /// `ESC ( A` / `ESC ) A`.
    Uk,
    /// `ESC ( 0` / `ESC ) 0`: DEC special graphics, the line-drawing set.
    DecLineDrawing,
}

impl Charset {
    /// The final byte that designates this set.
    pub const fn designator(self) -> char {
        match self {
            Self::Ascii => 'B',
            Self::Uk => 'A',
            Self::DecLineDrawing => '0',
        }
    }
}

/// What DECSC saves and DECRC restores, beside the cursor: the emulator
/// keeps one slot per screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct SavedModes {
    origin: bool,
    g0: Charset,
    g1: Charset,
}

/// The terminal state the snapshot must replay and the grid does not carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modes {
    /// DECCKM (`?1`).
    pub application_cursor_keys: bool,
    /// DECKPAM (`ESC =`) against DECKPNM (`ESC >`).
    pub application_keypad: bool,
    /// DECOM (`?6`).
    pub origin: bool,
    /// DECAWM (`?7`), on by default.
    pub auto_wrap: bool,
    /// IRM (`CSI 4 h`).
    pub insert: bool,
    /// `?2004`.
    pub bracketed_paste: bool,
    /// `?1004`.
    pub focus_tracking: bool,
    /// Which of `?1000`, `?1002`, `?1003` is on.
    pub mouse: MouseTracking,
    /// `?1005`.
    pub mouse_utf8: bool,
    /// `?1006`.
    pub mouse_sgr: bool,
    /// DECSTBM as zero-based inclusive rows, or `None` for the whole screen.
    pub scroll_region: Option<(u16, u16)>,
    /// The set designated to G0.
    pub g0: Charset,
    /// The set designated to G1.
    pub g1: Charset,
    /// SO (`0x0e`) in effect: G1 is the active set. SI (`0x0f`) clears it.
    pub shift_out: bool,
    /// The alternate screen is active (`?1049`, `?1047`, `?47`).
    pub alt: bool,
    /// DECSC slots, primary screen then alternate screen.
    saved: [Option<SavedModes>; 2],
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            application_cursor_keys: false,
            application_keypad: false,
            origin: false,
            auto_wrap: true,
            insert: false,
            bracketed_paste: false,
            focus_tracking: false,
            mouse: MouseTracking::Off,
            mouse_utf8: false,
            mouse_sgr: false,
            scroll_region: None,
            g0: Charset::Ascii,
            g1: Charset::Ascii,
            shift_out: false,
            alt: false,
            saved: [None, None],
        }
    }
}

impl Modes {
    /// Whether the pane reports the mouse at all: what decides if a viewer
    /// should send mouse events.
    pub const fn wants_mouse(&self) -> bool {
        !matches!(self.mouse, MouseTracking::Off)
    }

    /// The set the next printed character is drawn from.
    pub const fn active_charset(&self) -> Charset {
        if self.shift_out { self.g1 } else { self.g0 }
    }

    fn save_cursor(&mut self) {
        self.saved[usize::from(self.alt)] = Some(SavedModes {
            origin: self.origin,
            g0: self.g0,
            g1: self.g1,
        });
    }

    /// DECRC: the emulator restores the slot of the ACTIVE screen, or the
    /// defaults when nothing was saved there.
    fn restore_cursor(&mut self) {
        let saved = self.saved[usize::from(self.alt)].unwrap_or_default();
        self.origin = saved.origin;
        self.g0 = saved.g0;
        self.g1 = saved.g1;
    }

    /// Apply one parsed action, mirroring what the emulator does with it.
    fn observe(&mut self, action: &Action, rows: u16) {
        match action {
            Action::CSI(CSI::Mode(mode)) => self.observe_mode(mode),
            Action::CSI(CSI::Cursor(Cursor::SaveCursor)) => self.save_cursor(),
            Action::CSI(CSI::Cursor(Cursor::RestoreCursor)) => self.restore_cursor(),
            Action::Control(ControlCode::ShiftOut) => self.shift_out = true,
            Action::Control(ControlCode::ShiftIn) => self.shift_out = false,
            Action::CSI(CSI::Cursor(Cursor::SetTopAndBottomMargins { top, bottom })) => {
                let top = top.as_zero_based();
                let bottom = bottom.as_zero_based();
                let last = u32::from(rows.saturating_sub(1));
                // The emulator ignores an inverted or one-row region; a region
                // covering the whole screen is the same as none.
                let bottom = bottom.min(last);
                self.scroll_region = if top < bottom && !(top == 0 && bottom == last) {
                    Some((
                        u16::try_from(top).unwrap_or(u16::MAX),
                        u16::try_from(bottom).unwrap_or(u16::MAX),
                    ))
                } else {
                    None
                };
            }
            Action::CSI(CSI::Device(device)) => {
                if matches!(**device, Device::SoftReset) {
                    self.soft_reset();
                }
            }
            Action::Esc(Esc::Code(code)) => match code {
                EscCode::DecApplicationKeyPad => self.application_keypad = true,
                EscCode::DecNormalKeyPad => self.application_keypad = false,
                EscCode::DecSaveCursorPosition => self.save_cursor(),
                EscCode::DecRestoreCursorPosition => self.restore_cursor(),
                EscCode::AsciiCharacterSetG0 => self.g0 = Charset::Ascii,
                EscCode::UkCharacterSetG0 => self.g0 = Charset::Uk,
                EscCode::DecLineDrawingG0 => self.g0 = Charset::DecLineDrawing,
                EscCode::AsciiCharacterSetG1 => self.g1 = Charset::Ascii,
                EscCode::UkCharacterSetG1 => self.g1 = Charset::Uk,
                EscCode::DecLineDrawingG1 => self.g1 = Charset::DecLineDrawing,
                EscCode::FullReset => *self = Self::default(),
                _ => {}
            },
            _ => {}
        }
    }

    /// DECSTR, as the emulator implements it (xterm's reading: autowrap on).
    fn soft_reset(&mut self) {
        self.application_cursor_keys = false;
        self.application_keypad = false;
        self.origin = false;
        self.auto_wrap = true;
        self.insert = false;
        self.scroll_region = None;
        self.g0 = Charset::Ascii;
        self.g1 = Charset::Ascii;
        self.saved = [None, None];
    }

    fn observe_mode(&mut self, mode: &Mode) {
        match mode {
            Mode::SetDecPrivateMode(DecPrivateMode::Code(code)) => self.set_dec(code, true),
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(code)) => self.set_dec(code, false),
            Mode::SetMode(TerminalMode::Code(TerminalModeCode::Insert)) => self.insert = true,
            Mode::ResetMode(TerminalMode::Code(TerminalModeCode::Insert)) => self.insert = false,
            _ => {}
        }
    }

    fn set_dec(&mut self, code: &DecPrivateModeCode, on: bool) {
        match code {
            DecPrivateModeCode::ApplicationCursorKeys => self.application_cursor_keys = on,
            DecPrivateModeCode::OriginMode => self.origin = on,
            DecPrivateModeCode::AutoWrap => self.auto_wrap = on,
            DecPrivateModeCode::BracketedPaste => self.bracketed_paste = on,
            DecPrivateModeCode::FocusTracking => self.focus_tracking = on,
            DecPrivateModeCode::Utf8Mouse => self.mouse_utf8 = on,
            DecPrivateModeCode::SGRMouse => self.mouse_sgr = on,
            // `?1049h` saves on the primary screen and `?1049l` restores
            // there, as the emulator does; `?47` and `?1047` only switch.
            DecPrivateModeCode::ClearAndEnableAlternateScreen => {
                if on && !self.alt {
                    self.save_cursor();
                    self.alt = true;
                } else if !on && self.alt {
                    self.alt = false;
                    self.restore_cursor();
                }
            }
            DecPrivateModeCode::EnableAlternateScreen
            | DecPrivateModeCode::OptEnableAlternateScreen => self.alt = on,
            DecPrivateModeCode::MouseTracking
            | DecPrivateModeCode::HighlightMouseTracking
            | DecPrivateModeCode::ButtonEventMouse
            | DecPrivateModeCode::AnyEventMouse => {
                self.mouse = match (code, on) {
                    (_, false) => MouseTracking::Off,
                    (DecPrivateModeCode::ButtonEventMouse, true) => MouseTracking::Button,
                    (DecPrivateModeCode::AnyEventMouse, true) => MouseTracking::Any,
                    (_, true) => MouseTracking::Standard,
                };
            }
            _ => {}
        }
    }
}

/// Split the last grapheme cluster off the trailing printable text of
/// `actions`, so it can be replayed at the head of the next batch. The rest
/// of that text stays in `actions` and is performed now, so the cost is
/// linear in the bytes fed. A cluster longer than [`MAX_HELD_BYTES`] is not
/// held at all.
fn take_trailing_cluster(actions: &mut Vec<Action>) -> Option<String> {
    // The parser emits one `Print` per character, so gather the whole
    // trailing run first.
    let mut parts: Vec<String> = Vec::new();
    while let Some(last) = actions.last() {
        match last {
            Action::Print(c) => parts.push(c.to_string()),
            Action::PrintString(s) => parts.push(s.clone()),
            _ => break,
        }
        actions.pop();
    }
    if parts.is_empty() {
        return None;
    }
    parts.reverse();
    let text = parts.concat();
    let last = text.graphemes(true).next_back()?;
    if last.len() > MAX_HELD_BYTES {
        actions.push(Action::PrintString(text));
        return None;
    }
    let head_len = text.len() - last.len();
    let held = text[head_len..].to_string();
    if head_len > 0 {
        let mut head = text;
        head.truncate(head_len);
        actions.push(Action::PrintString(head));
    }
    Some(held)
}

/// The emulator panicked on some input and its grid can no longer be
/// trusted. Replace the pane and re-seed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Poisoned;

impl std::fmt::Display for Poisoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("pane emulator poisoned by a panic; replace and re-seed it")
    }
}

impl std::error::Error for Poisoned {}

/// The cursor as a viewer needs it: zero-based column and row, and whether
/// it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    /// Zero-based column.
    pub col: u16,
    /// Zero-based visible row.
    pub row: u16,
    /// DECTCEM.
    pub visible: bool,
}

/// One pane: the emulator, its parser and the tracked modes.
pub struct PaneEmulator {
    term: Terminal,
    parser: Parser,
    modes: Modes,
    cols: u16,
    rows: u16,
    bytes_fed: u64,
    poisoned: bool,
    /// The last grapheme cluster of the last feed, not yet performed: a
    /// combining mark, ZWJ or variation selector in the next feed must land
    /// in the same batch as its base, or the emulator drops it. At most
    /// [`MAX_HELD_BYTES`].
    held: Option<String>,
    /// Test-only: make the next feed panic inside the guarded region, so
    /// the containment path is exercised without a crafted image.
    #[cfg(test)]
    panic_on_next_feed: bool,
}

impl std::fmt::Debug for PaneEmulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneEmulator")
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("modes", &self.modes)
            .field("bytes_fed", &self.bytes_fed)
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

impl PaneEmulator {
    /// A fresh `cols` x `rows` pane with `live_rows` of scrollback.
    pub fn new(cols: u16, rows: u16, live_rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            term: new_terminal(
                usize::from(cols),
                usize::from(rows),
                TermConfig { live_rows },
            ),
            parser: Parser::new(),
            modes: Modes::default(),
            cols,
            rows,
            bytes_fed: 0,
            poisoned: false,
            held: None,
            #[cfg(test)]
            panic_on_next_feed: false,
        }
    }

    /// Feed pane output. The slice may end anywhere, including inside an
    /// escape sequence or a multi-byte grapheme; the parser carries state
    /// across calls.
    ///
    /// The last grapheme cluster of the slice is HELD until the next feed
    /// or [`flush`](Self::flush): the emulator only joins a combining mark,
    /// ZWJ or variation selector to its base when both arrive in one batch,
    /// and a tmux feed splits graphemes wherever it likes. The daemon
    /// flushes after a few idle milliseconds; a snapshot flushes itself.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Poisoned> {
        if self.poisoned {
            return Err(Poisoned);
        }
        let rows = self.rows;
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            #[cfg(test)]
            assert!(!self.panic_on_next_feed, "injected emulator panic");
            let mut actions = self.parser.parse_as_vec(bytes);
            if let Some(held) = self.held.take() {
                actions.insert(0, Action::PrintString(held));
            }
            self.held = take_trailing_cluster(&mut actions);
            for action in &actions {
                self.modes.observe(action, rows);
            }
            self.term.perform_actions(actions);
        }));
        match outcome {
            Ok(()) => {
                self.bytes_fed += bytes.len() as u64;
                Ok(())
            }
            Err(_) => {
                self.poisoned = true;
                Err(Poisoned)
            }
        }
    }

    /// Perform the text held back by the last [`feed`](Self::feed). Call it
    /// when the feed goes idle and before reading the grid.
    pub fn flush(&mut self) -> Result<(), Poisoned> {
        if self.poisoned {
            return Err(Poisoned);
        }
        let Some(held) = self.held.take() else {
            return Ok(());
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            self.term.perform_actions(vec![Action::PrintString(held)]);
        }));
        if outcome.is_err() {
            self.poisoned = true;
            return Err(Poisoned);
        }
        Ok(())
    }

    /// Whether text is held back, waiting for a flush or the next feed.
    pub const fn has_held_text(&self) -> bool {
        self.held.is_some()
    }

    /// Bytes held back, at most [`MAX_HELD_BYTES`].
    pub fn held_bytes(&self) -> usize {
        self.held.as_ref().map_or(0, String::len)
    }

    /// Resize the pane. Like the emulator, this drops the scroll region.
    /// Held text is flushed first so it lands at the old width, in order.
    /// Refused on a poisoned pane, and a panic inside the resize poisons it.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Poisoned> {
        self.flush()?;
        let cols = cols.max(1);
        let rows = rows.max(1);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            self.term.resize(TerminalSize {
                rows: usize::from(rows),
                cols: usize::from(cols),
                pixel_width: 0,
                pixel_height: 0,
                dpi: 0,
            });
        }));
        if outcome.is_err() {
            self.poisoned = true;
            return Err(Poisoned);
        }
        self.cols = cols;
        self.rows = rows;
        self.modes.scroll_region = None;
        Ok(())
    }

    /// Columns and rows.
    pub const fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// The tracked modes.
    pub const fn modes(&self) -> &Modes {
        &self.modes
    }

    /// Bytes accepted so far, the pane-feed offset this emulator is at.
    pub const fn bytes_fed(&self) -> u64 {
        self.bytes_fed
    }

    /// Whether a panic has made the grid untrustworthy.
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// The window title (OSC 0 or 2).
    pub fn title(&self) -> &str {
        self.term.get_title()
    }

    /// Whether the alternate screen is active.
    pub fn is_alt_screen(&self) -> bool {
        self.term.is_alt_screen_active()
    }

    /// The cursor.
    pub fn cursor(&self) -> CursorState {
        let pos = self.term.cursor_pos();
        CursorState {
            col: u16::try_from(pos.x).unwrap_or(u16::MAX),
            row: u16::try_from(pos.y.max(0)).unwrap_or(u16::MAX),
            // `CursorVisibility` lives in a crate this one does not depend
            // on; the default position is the visible one.
            visible: pos.visibility == CursorPosition::default().visibility,
        }
    }

    /// The underlying emulator, for reading the grid.
    pub const fn terminal(&self) -> &Terminal {
        &self.term
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(cols: u16, rows: u16) -> PaneEmulator {
        PaneEmulator::new(cols, rows, crate::DEFAULT_LIVE_ROWS)
    }

    fn row_text(pane: &PaneEmulator, row: usize) -> String {
        let screen = pane.terminal().screen();
        let phys = screen.phys_range(&(0..screen.physical_rows as i64));
        let lines = screen.lines_in_phys_range(phys);
        lines[row].as_str().trim_end().to_string()
    }

    /// Spike 2 section 3b, fixture f7: DECSTBM homes the cursor, so MARK
    /// lands on row 1 and not on row 6.
    #[test]
    fn decstbm_homes_the_cursor() {
        let mut p = pane(40, 20);
        p.feed(b"\x1b[2J\x1b[H\x1b[6;1H\x1b[6;18rMARK\x1b[20;1H").unwrap();
        assert_eq!(row_text(&p, 0), "MARK");
        assert_eq!(row_text(&p, 5), "");
        assert_eq!(p.modes().scroll_region, Some((5, 17)));
    }

    /// Fixture f4: OSC 8 links survive as per-cell state, BEL and ST forms,
    /// with and without an id, two adjacent links, one across the margin.
    #[test]
    fn osc8_links_are_cell_state() {
        let mut p = pane(40, 20);
        p.feed(b"links:\r\n").unwrap();
        p.feed(b"\x1b]8;;https://example.invalid/a\x1b\\ALPHA\x1b]8;;\x1b\\ plain\r\n")
            .unwrap();
        p.feed(b"\x1b]8;id=x1;https://example.invalid/b\x07BRAVO\x1b]8;;\x07 plain\r\n")
            .unwrap();
        p.feed(b"\x1b]8;;https://example.invalid/c\x1b\\CHAR\x1b]8;;https://example.invalid/d\x1b\\DELTA\x1b]8;;\x1b\\\r\n")
            .unwrap();
        p.feed(b"\x1b]8;;https://example.invalid/long\x1b\\LONGLINK-abcdefghijklmnopqrstuvwxyz-0123456789\x1b]8;;\x1b\\ end\r\n")
            .unwrap();

        let screen = p.terminal().screen();
        let phys = screen.phys_range(&(0..screen.physical_rows as i64));
        let lines = screen.lines_in_phys_range(phys);
        let link_at = |row: usize, col: usize| -> String {
            lines[row]
                .get_cell(col)
                .and_then(|c| c.attrs().hyperlink().map(|h| h.uri().to_string()))
                .unwrap_or_default()
        };
        assert_eq!(link_at(1, 0), "https://example.invalid/a");
        assert_eq!(link_at(1, 4), "https://example.invalid/a");
        assert_eq!(link_at(1, 6), "");
        assert_eq!(link_at(2, 0), "https://example.invalid/b");
        assert_eq!(
            lines[2].get_cell(0).unwrap().attrs().hyperlink().unwrap().params().get("id"),
            Some(&"x1".to_string())
        );
        assert_eq!(link_at(3, 3), "https://example.invalid/c");
        assert_eq!(link_at(3, 4), "https://example.invalid/d");
        // The long link wraps at column 40 and continues on the next row.
        assert_eq!(link_at(4, 39), "https://example.invalid/long");
        assert_eq!(link_at(5, 0), "https://example.invalid/long");
        assert_eq!(row_text(&p, 5), "456789 end");
    }

    /// Fixture f2, with tmux's one-tracking-mode rule: after the walk, button
    /// tracking, SGR, focus, bracketed paste and DECCKM are on and UTF-8
    /// extension is off.
    #[test]
    fn decset_tracker_follows_the_mouse_fixture() {
        let mut p = pane(40, 20);
        p.feed(b"mouse fixture\r\n\x1b[?1000hX10+click on\r\n").unwrap();
        assert_eq!(p.modes().mouse, MouseTracking::Standard);
        p.feed(b"\x1b[?1005hutf8 ext on\r\n\x1b[?1005lutf8 ext off\r\n").unwrap();
        assert!(!p.modes().mouse_utf8);
        p.feed(b"\x1b[?1002hbtn-event on\r\n").unwrap();
        assert_eq!(p.modes().mouse, MouseTracking::Button);
        p.feed(b"\x1b[?1006hsgr ext on\r\n\x1b[?1004hfocus on\r\n\x1b[?2004hbracketed paste on\r\n\x1b[?1happlication cursor on\r\n")
            .unwrap();
        let m = *p.modes();
        assert!(m.mouse_sgr && m.focus_tracking && m.bracketed_paste && m.application_cursor_keys);
        assert!(m.wants_mouse());
        // Any tracking reset clears tracking, as tmux does.
        p.feed(b"\x1b[?1000l").unwrap();
        assert_eq!(p.modes().mouse, MouseTracking::Off);
        assert!(!p.modes().wants_mouse());
        assert!(p.modes().mouse_sgr, "encodings are independent of tracking");
    }

    #[test]
    fn tracker_sees_keypad_insert_origin_wrap_and_resets() {
        let mut p = pane(40, 20);
        p.feed(b"\x1b=\x1b[4h\x1b[?6h\x1b[?7l").unwrap();
        let m = *p.modes();
        assert!(m.application_keypad && m.insert && m.origin && !m.auto_wrap);
        p.feed(b"\x1b>").unwrap();
        assert!(!p.modes().application_keypad);
        p.feed(b"\x1b[?1000h\x1b[3;10r\x1b[?1h\x1b[!p").unwrap();
        let m = *p.modes();
        assert!(!m.insert && !m.origin && m.auto_wrap && !m.application_cursor_keys);
        assert_eq!(m.scroll_region, None, "DECSTR drops the region");
        assert_eq!(
            m.mouse,
            MouseTracking::Standard,
            "DECSTR leaves the mouse alone"
        );
        p.feed(b"\x1bc").unwrap();
        assert_eq!(*p.modes(), Modes::default(), "RIS resets everything");
    }

    #[test]
    fn a_split_escape_sequence_is_parsed_across_feeds() {
        let mut p = pane(40, 20);
        p.feed(b"\x1b[?10").unwrap();
        assert_eq!(p.modes().mouse, MouseTracking::Off);
        p.feed(b"02h\x1b[?200").unwrap();
        assert_eq!(p.modes().mouse, MouseTracking::Button);
        assert!(!p.modes().bracketed_paste);
        p.feed(b"4h").unwrap();
        assert!(p.modes().bracketed_paste);
        // A grapheme split mid-sequence lands as one cell.
        p.feed(b"\x1b[H\xe4\xb8").unwrap();
        p.feed(b"\xadX").unwrap();
        assert!(p.has_held_text(), "the last cluster waits for a flush");
        assert_eq!(row_text(&p, 0), "\u{4E2D}", "everything before it went in");
        p.flush().unwrap();
        assert!(!p.has_held_text());
        assert_eq!(row_text(&p, 0), "\u{4E2D}X");
        assert_eq!(p.cursor().col, 3);
        assert_eq!(p.bytes_fed(), 23);
    }

    /// Measured on the fork: a combining mark, a ZWJ sequence or a variation
    /// selector that arrives in a later batch than its base is dropped. The
    /// hold-back joins them.
    #[test]
    fn a_grapheme_split_across_feeds_joins_its_base() {
        let cases: [(&str, &[&[u8]], &str); 5] = [
            ("base|mark", &[b"xa", b"\xcc\x8a tail"], "xa\u{30a} tail"),
            ("mid-mark", &[b"xa\xcc", b"\x8a tail"], "xa\u{30a} tail"),
            (
                "base|mark|more",
                &[b"xa", b"\xcc\x8a", b" tail"],
                "xa\u{30a} tail",
            ),
            (
                "zwj",
                &[b"\xf0\x9f\x91\xa9", b"\xe2\x80\x8d\xf0\x9f\x92\xbb!"],
                "\u{1F469}\u{200D}\u{1F4BB}!",
            ),
            (
                "vs16",
                &[b"\xe2\x9d\xa4", b"\xef\xb8\x8f."],
                "\u{2764}\u{FE0F}.",
            ),
        ];
        for (label, parts, want) in cases {
            let mut p = pane(20, 3);
            for part in parts {
                p.feed(part).unwrap();
            }
            p.flush().unwrap();
            assert_eq!(row_text(&p, 0), want, "{label}");
        }
        // Held text is released by the next feed too, in order.
        let mut p = pane(20, 3);
        p.feed(b"xa").unwrap();
        p.feed(b"\xcc\x8a").unwrap();
        p.feed(b"\x1b[2;1Hy").unwrap();
        p.flush().unwrap();
        assert_eq!(row_text(&p, 0), "xa\u{30a}");
        assert_eq!(row_text(&p, 1), "y");
        assert_eq!(p.cursor().col, 1);
    }

    #[test]
    fn live_window_env_is_clamped() {
        assert_eq!(live_rows_from(None), crate::DEFAULT_LIVE_ROWS);
        assert_eq!(live_rows_from(Some("garbage")), crate::DEFAULT_LIVE_ROWS);
        assert_eq!(live_rows_from(Some("")), crate::DEFAULT_LIVE_ROWS);
        assert_eq!(live_rows_from(Some("10")), MIN_LIVE_ROWS);
        assert_eq!(live_rows_from(Some("100")), 100);
        assert_eq!(live_rows_from(Some(" 2500 ")), 2500);
        assert_eq!(live_rows_from(Some("5000")), 5000);
        assert_eq!(live_rows_from(Some("99999")), MAX_LIVE_ROWS);
    }

    #[test]
    fn live_window_caps_scrollback() {
        let mut p = PaneEmulator::new(40, 5, MIN_LIVE_ROWS);
        for i in 0..400 {
            p.feed(format!("line {i}\r\n").as_bytes()).unwrap();
        }
        // `scrollback_rows` counts every stored line, viewport included.
        assert_eq!(p.terminal().screen().scrollback_rows(), MIN_LIVE_ROWS + 5);
    }

    #[test]
    fn resize_changes_the_grid_and_drops_the_region() {
        let mut p = pane(120, 40);
        p.feed(b"\x1b[5;30r").unwrap();
        assert_eq!(p.modes().scroll_region, Some((4, 29)));
        p.resize(40, 20).unwrap();
        assert_eq!(p.size(), (40, 20));
        assert_eq!(p.modes().scroll_region, None);
        let size = p.terminal().get_size();
        assert_eq!((size.cols, size.rows), (40, 20));
        p.feed(b"\x1b[H\x1b[2J0123456789012345678901234567890123456789X").unwrap();
        p.flush().unwrap();
        assert_eq!(row_text(&p, 1), "X", "wraps at the new width");
        p.feed(b"held").unwrap();
        p.resize(80, 24).unwrap();
        assert!(!p.has_held_text(), "resize flushes first");
        // The widened pane reflows the wrapped row back onto row 0.
        assert!(row_text(&p, 0).ends_with("9Xheld"), "{}", row_text(&p, 0));
    }

    #[test]
    fn cursor_state_reports_position_and_visibility() {
        let mut p = pane(40, 20);
        p.feed(b"\x1b[10;12H\x1b[?25l").unwrap();
        assert_eq!(
            p.cursor(),
            CursorState {
                col: 11,
                row: 9,
                visible: false
            }
        );
        p.feed(b"\x1b[?25h").unwrap();
        assert!(p.cursor().visible);
        assert!(!p.is_alt_screen());
        p.feed(b"\x1b[?1049h\x1b]0;panel\x07").unwrap();
        assert!(p.is_alt_screen());
        assert_eq!(p.title(), "panel");
    }

    #[test]
    fn a_panic_poisons_the_pane_instead_of_unwinding() {
        let mut p = pane(40, 20);
        p.feed(b"before").unwrap();
        let before = p.bytes_fed();
        assert!(!p.is_poisoned());
        p.panic_on_next_feed = true;
        assert_eq!(p.feed(b"boom"), Err(Poisoned));
        assert!(p.is_poisoned());
        p.panic_on_next_feed = false;
        assert_eq!(p.feed(b"more"), Err(Poisoned), "stays poisoned");
        assert_eq!(p.bytes_fed(), before, "nothing counted after the poison");
        assert_eq!(
            format!("{Poisoned}"),
            "pane emulator poisoned by a panic; replace and re-seed it"
        );
        assert_eq!(p.resize(80, 24), Err(Poisoned), "resize refused too");
        assert_eq!(p.size(), (40, 20));
    }

    /// Only the last cluster is held, so a feed with no control byte in it
    /// (a minified bundle, a base64 blob) is performed as it arrives and the
    /// total cost stays linear in the bytes fed.
    #[test]
    fn only_the_last_cluster_is_held_and_time_stays_linear() {
        fn run(kib: usize) -> std::time::Duration {
            let mut p = PaneEmulator::new(80, 24, 100);
            let chunk = vec![b'a'; 4096];
            let start = std::time::Instant::now();
            for _ in 0..(kib / 4) {
                p.feed(&chunk).unwrap();
                assert!(p.held_bytes() <= MAX_HELD_BYTES);
                assert_eq!(p.held_bytes(), 1, "one `a` waits, the rest went in");
            }
            start.elapsed()
        }
        // Sizes kept small because the fork performs about 4 KiB of text
        // per 90 ms in a debug build. Quadratic re-concatenation made 4x
        // the bytes cost about 16x; linear is about 4x. Generous bound for
        // a shared CI runner.
        let quarter = run(256);
        let one = run(1024);
        assert!(
            one < quarter * 10 + std::time::Duration::from_millis(250),
            "256 KiB {quarter:?}, 1 MiB {one:?}: not linear"
        );
        // A trailing multi-byte cluster is the held one; a run of narrow
        // text before it is already on the grid.
        let mut p = pane(40, 3);
        p.feed("abc\u{1F469}\u{200D}\u{1F4BB}".as_bytes()).unwrap();
        assert_eq!(row_text(&p, 0), "abc");
        assert_eq!(p.held_bytes(), "\u{1F469}\u{200D}\u{1F4BB}".len());
        p.flush().unwrap();
        assert_eq!(row_text(&p, 0), "abc\u{1F469}\u{200D}\u{1F4BB}");
        // A cluster longer than the cap is not held at all.
        let mut p = pane(40, 3);
        let mut pile = String::from("x");
        for _ in 0..40 {
            pile.push('\u{0301}');
        }
        p.feed(pile.as_bytes()).unwrap();
        assert_eq!(p.held_bytes(), 0);
        assert_eq!(row_text(&p, 0), pile);
    }

    /// The documented residual: a mark that arrives after a flush meets a
    /// base already on the grid, and the fork drops it. Clients still get
    /// the raw bytes; only the daemon's grid, and so a later snapshot,
    /// shows the bare base.
    #[test]
    fn a_mark_after_a_flush_is_the_documented_loss() {
        let mut p = pane(20, 3);
        p.feed(b"xa").unwrap();
        p.flush().unwrap();
        p.feed(b"\xcc\x8a tail").unwrap();
        p.flush().unwrap();
        assert_eq!(
            row_text(&p, 0),
            "xa tail",
            "the mark after the flush is lost"
        );
        assert_eq!(p.cursor().col, 7);
    }

    #[test]
    fn tracker_mirrors_charsets_and_the_saved_cursor_per_screen() {
        let mut p = pane(40, 20);
        p.feed(b"\x1b(0\x1b)A\x0e").unwrap();
        let m = *p.modes();
        assert_eq!(
            (m.g0, m.g1, m.shift_out),
            (Charset::DecLineDrawing, Charset::Uk, true)
        );
        assert_eq!(m.active_charset(), Charset::Uk);
        assert_eq!(Charset::DecLineDrawing.designator(), '0');
        // DECSC saves origin and charsets; changes after it are undone by DECRC.
        p.feed(b"\x1b[?6h\x1b7\x1b[?6l\x1b(B\x0f").unwrap();
        assert!(!p.modes().origin && p.modes().g0 == Charset::Ascii);
        p.feed(b"\x1b8").unwrap();
        let m = *p.modes();
        assert!(m.origin, "DECRC restores origin mode");
        assert_eq!(m.g0, Charset::DecLineDrawing, "DECRC restores G0");
        assert!(!m.shift_out, "SO/SI is not part of the saved cursor");
        // `?1049h` saves on the primary screen; the alternate screen has its
        // own slot; `?1049l` restores the primary one.
        p.feed(b"\x1b[?1049h").unwrap();
        assert!(p.modes().alt);
        p.feed(b"\x1b[?6l\x1b(B\x1b[s\x1b(A\x1b[u").unwrap();
        assert_eq!(
            p.modes().g0,
            Charset::Ascii,
            "CSI s/u save and restore on alt"
        );
        p.feed(b"\x1b[?1049l").unwrap();
        let m = *p.modes();
        assert!(!m.alt && m.origin && m.g0 == Charset::DecLineDrawing);
        // DECRC with nothing saved on this screen resets to the defaults.
        p.feed(b"\x1b[!p\x1b(0\x1b[?6h\x1b8").unwrap();
        let m = *p.modes();
        assert!(!m.origin && m.g0 == Charset::Ascii);
        assert!(!m.shift_out);
        p.feed(b"\x1b[?47h").unwrap();
        assert!(p.modes().alt, "?47 only switches");
        p.feed(b"\x1bc").unwrap();
        assert_eq!(*p.modes(), Modes::default());
    }
}
