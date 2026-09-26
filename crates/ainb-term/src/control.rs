//! tmux control-mode feed parser.
//!
//! The daemon attaches one `tmux -C attach-session` client per watched
//! session and reads its stdout through this parser. Everything here follows
//! what spike 2 (section 1 and 5) and spike 7 measured on tmux 3.4, and the
//! fixture in `tests/fixtures/control-mode-tmux-3.6a.ctl` re-checks it on
//! tmux 3.6a:
//!
//! * The stream is BYTES, never `str`: tmux ends a notification in the middle
//!   of a multi-byte grapheme under load (98 of 14,349 lines in one capture),
//!   so a UTF-8 line reader fails only under load. Payloads come out as raw
//!   bytes and the emulator's parser reassembles the grapheme.
//! * Pane output is `vis(3)`-escaped: any byte tmux considers unprintable is a
//!   backslash and exactly three octal digits, the backslash itself included
//!   (`\134`). UTF-8 passes through verbatim. [`unvis`] reverses it.
//! * `refresh-client -f pause-after=N` switches the whole client from
//!   `%output` to `%extended-output`, which carries the buffering age; both
//!   forms decode to the same [`Event::Output`].
//! * `%pause` and `%continue` arrive INSIDE the `%begin`/`%end` block of the
//!   command that caused them, so they are surfaced wherever they appear and
//!   never buried in a reply body. They are the ONLY notifications lifted
//!   out of a block, and they are unauthenticated there: a reply body is raw
//!   pane text (`capture-pane` rows, `#{pane_title}`), so a pane can print
//!   `%continue %0` and it lands inside the next reply. The feed actor (WP8)
//!   must act on [`Event::Continue`] only for a pane it recorded as paused
//!   from a top-level `%pause`, and treat a `%pause` seen inside a block as
//!   advisory.
//! * A block closes only on the `%end` or `%error` whose `time number flags`
//!   equal the open `%begin`'s, the one guard tmux gives. A pane printing
//!   `%end 1 2 1` therefore stays a body line, and so do `%output`, `%exit`
//!   and `%layout-change` lines in a body: they are never lifted.
//! * `%error` closes a block like `%end`; a daemon that does not see it
//!   watches a silently dead pane (spike 7's syntax trap).
//! * A pane id as its own argument is a tmux parse error (`%` starts a
//!   conditional directive), so [`refresh_client_pane`] glues it to `-A` in
//!   quotes.
//!
//! ```text
//! tmux -C ──bytes──▶ ControlParser::feed ──▶ Event::Output {pane, age_ms, data}
//!                                         ├─▶ Event::Pause / Continue
//!                                         ├─▶ Event::Reply {ok, number, lines}
//!                                         └─▶ Event::LayoutChange / Exit / Other
//! ```

use std::fmt;

/// A tmux pane id, the number after `%`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u32);

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

impl PaneId {
    /// Parse `%12`.
    fn parse(token: &[u8]) -> Option<Self> {
        let digits = token.strip_prefix(b"%")?;
        parse_u32(digits).map(Self)
    }
}

/// A tmux window id, the number after `@`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u32);

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.0)
    }
}

impl WindowId {
    fn parse(token: &[u8]) -> Option<Self> {
        let digits = token.strip_prefix(b"@")?;
        parse_u32(digits).map(Self)
    }
}

fn parse_u32(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

fn parse_u64(digits: &[u8]) -> Option<u64> {
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// Split the first space-delimited field off a line.
fn field(b: &[u8]) -> (&[u8], &[u8]) {
    b.iter()
        .position(|c| *c == b' ')
        .map_or((b, &b[..0]), |i| (&b[..i], &b[i + 1..]))
}

/// One decoded control-mode line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Pane output, decoded. `age_ms` is present only under `pause-after`
    /// (`%extended-output`): how long tmux buffered the bytes before sending.
    Output {
        /// The pane.
        pane: PaneId,
        /// Buffering age in milliseconds, when tmux reports it.
        age_ms: Option<u64>,
        /// The raw pane bytes, exactly as the program wrote them.
        data: Vec<u8>,
    },
    /// tmux paused this pane's output for this client. Everything the pane
    /// writes until `%continue` is discarded for this client.
    Pause(PaneId),
    /// Output resumes at the pane's CURRENT position: re-snapshot first.
    Continue(PaneId),
    /// A command reply: the lines between `%begin` and `%end` (`ok`) or
    /// `%error` (`!ok`). Reply bodies are raw, not `vis`-escaped.
    Reply {
        /// `%end` rather than `%error`.
        ok: bool,
        /// The command number tmux assigned, matching `%begin`.
        number: u64,
        /// The body lines, without their newline.
        lines: Vec<Vec<u8>>,
    },
    /// `%layout-change window layout visible-layout flags`. Every
    /// `refresh-client -C` emits one even when nothing changed, so a resize
    /// count must dedupe on `layout`.
    LayoutChange {
        /// The window.
        window: WindowId,
        /// The rest of the line after the window id, raw.
        layout: Vec<u8>,
    },
    /// The window was closed.
    WindowClose(WindowId),
    /// tmux is closing this client. The feed is gone after this.
    Exit {
        /// The reason, when tmux gives one.
        reason: Option<Vec<u8>>,
    },
    /// Any other notification, raw: `%sessions-changed`, `%window-add`, ...
    Other {
        /// The notification name including the `%`.
        name: Vec<u8>,
        /// The rest of the line.
        rest: Vec<u8>,
    },
}

/// Reverse tmux's `vis(3)` octal escaping. Works on bytes: the input may end
/// mid-grapheme. A backslash not followed by three octal digits is kept as
/// is, which never happens on a well-formed feed because tmux escapes the
/// backslash itself.
pub fn unvis(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = u32::from(b[i + 1] - b'0') * 64
                + u32::from(b[i + 2] - b'0') * 8
                + u32::from(b[i + 3] - b'0');
            // Three octal digits reach 0o777; tmux never emits above 0o377.
            out.push(u8::try_from(v & 0xff).unwrap_or(0));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Escape bytes the way tmux does before it writes pane output on the
/// control stream: every byte outside printable ASCII except UTF-8 lead and
/// continuation bytes becomes `\ooo`, and so does the backslash. Used by the
/// tests to round-trip, and by anything that wants to fake a feed.
pub fn vis(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    for &c in b {
        let printable = (0x20..0x7f).contains(&c) && c != b'\\';
        if printable || c >= 0x80 {
            out.push(c);
        } else {
            out.extend_from_slice(format!("\\{c:03o}").as_bytes());
        }
    }
    out
}

/// What `refresh-client -A` can do to a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneAction {
    /// Start delivering this pane's output to this client.
    On,
    /// Stop delivering, and let tmux stop reading the pane when nobody
    /// wants it (the spike 7 Run C throttle: use only for unwatched panes).
    Off,
    /// Resume after `%pause`; re-snapshot before applying more output.
    Continue,
    /// Pause delivery for this client.
    Pause,
}

impl PaneAction {
    const fn word(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
            Self::Continue => "continue",
            Self::Pause => "pause",
        }
    }
}

/// The `refresh-client -A` command line for one pane, newline-terminated.
///
/// The pane id is glued to `-A` inside double quotes: as its own argument a
/// bare `%0` is a tmux `parse error: syntax error` (spike 7), and the
/// symptom is a pane that looks alive and never resumes.
pub fn refresh_client_pane(pane: PaneId, action: PaneAction) -> String {
    format!("refresh-client -A\"{pane}:{}\"\n", action.word())
}

/// The `refresh-client -f pause-after=N` command line, newline-terminated.
/// Whole seconds, minimum 1; tmux has no sub-second option.
pub fn refresh_client_pause_after(seconds: u32) -> String {
    format!("refresh-client -f pause-after={}\n", seconds.max(1))
}

/// An open reply block.
#[derive(Debug)]
struct Block {
    /// The `%begin` line's `time number flags`, raw: the closing `%end` or
    /// `%error` must carry exactly the same three fields.
    key: Vec<u8>,
    /// The command number, parsed from the key.
    number: u64,
    /// The body so far.
    lines: Vec<Vec<u8>>,
}

/// Byte-oriented parser over the control client's stdout.
#[derive(Debug, Default)]
pub struct ControlParser {
    /// Bytes of an incomplete line.
    partial: Vec<u8>,
    /// The open `%begin` block, if any.
    block: Option<Block>,
}

impl ControlParser {
    /// A parser at the start of the stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes as they arrive. Every complete line yields at most one
    /// event; a trailing partial line waits for the next feed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        let mut rest = bytes;
        while let Some(nl) = rest.iter().position(|b| *b == b'\n') {
            let line = if self.partial.is_empty() {
                rest[..nl].to_vec()
            } else {
                let mut l = std::mem::take(&mut self.partial);
                l.extend_from_slice(&rest[..nl]);
                l
            };
            rest = &rest[nl + 1..];
            if let Some(ev) = self.line(line) {
                events.push(ev);
            }
        }
        // ponytail: unbounded partial-line buffer. A control line is bounded
        // by tmux's per-read chunk (tens of KiB seen); add a cap if a hostile
        // pane ever matters.
        self.partial.extend_from_slice(rest);
        events
    }

    /// Whether a reply block is open.
    pub const fn in_block(&self) -> bool {
        self.block.is_some()
    }

    fn line(&mut self, mut line: Vec<u8>) -> Option<Event> {
        // `\r` before `\n` can only be a line-ending artefact: a pane's own
        // carriage return is escaped as `\015`.
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if let Some(block) = self.block.as_mut() {
            let closes = |rest: &[u8]| rest == block.key.as_slice();
            let ok = match (line.strip_prefix(b"%end "), line.strip_prefix(b"%error ")) {
                (Some(rest), _) if closes(rest) => Some(true),
                (_, Some(rest)) if closes(rest) => Some(false),
                _ => None,
            };
            if let Some(ok) = ok {
                let block = self.block.take().expect("block is open");
                return Some(Event::Reply {
                    ok,
                    number: block.number,
                    lines: block.lines,
                });
            }
            // tmux delivers the %pause or %continue a command caused inside
            // that command's reply. Those two are lifted out; nothing else
            // is, because a reply body is raw pane text and can contain any
            // line a pane cares to print, `%output` and `%end` included.
            if let Some(ev) = pause_or_continue(&line) {
                return Some(ev);
            }
            block.lines.push(line);
            return None;
        }
        if let Some(rest) = line.strip_prefix(b"%begin ") {
            self.block = Some(Block {
                key: rest.to_vec(),
                number: block_number(rest).unwrap_or(0),
                lines: Vec::new(),
            });
            return None;
        }
        notification(&line).or_else(|| {
            line.starts_with(b"%").then(|| {
                let (name, rest) = field(&line);
                Event::Other {
                    name: name.to_vec(),
                    rest: rest.to_vec(),
                }
            })
        })
    }
}

/// `%begin time number flags`: the number.
fn block_number(rest: &[u8]) -> Option<u64> {
    let (_time, rest) = field(rest);
    let (number, _flags) = field(rest);
    parse_u64(number)
}

/// The two notifications tmux emits inside a reply block.
fn pause_or_continue(line: &[u8]) -> Option<Event> {
    if let Some(rest) = line.strip_prefix(b"%pause ") {
        return PaneId::parse(field(rest).0).map(Event::Pause);
    }
    if let Some(rest) = line.strip_prefix(b"%continue ") {
        return PaneId::parse(field(rest).0).map(Event::Continue);
    }
    None
}

/// Decode a top-level notification. `None` means the line is not one.
fn notification(line: &[u8]) -> Option<Event> {
    if let Some(rest) = line.strip_prefix(b"%extended-output ") {
        // %extended-output %<pane> <age> [more fields] : <data>. Today the
        // colon follows the age directly, and the payload may itself
        // contain ` : `, so the normal `: ` start is checked FIRST. The man
        // page leaves room for fields between the age and the colon; only
        // when the field after the age is not the colon is the payload
        // taken from after the first ` : `.
        let (pane, rest) = field(rest);
        let (age, rest) = field(rest);
        let data = if let Some(data) = rest.strip_prefix(b": ") {
            data
        } else if rest == b":" {
            &rest[1..]
        } else {
            rest.windows(3).position(|w| w == b" : ").map_or(rest, |i| &rest[i + 3..])
        };
        return Some(Event::Output {
            pane: PaneId::parse(pane)?,
            age_ms: Some(parse_u64(age)?),
            data: unvis(data),
        });
    }
    if let Some(rest) = line.strip_prefix(b"%output ") {
        let (pane, data) = field(rest);
        return Some(Event::Output {
            pane: PaneId::parse(pane)?,
            age_ms: None,
            data: unvis(data),
        });
    }
    if let Some(ev) = pause_or_continue(line) {
        return Some(ev);
    }
    if let Some(rest) = line.strip_prefix(b"%layout-change ") {
        let (window, layout) = field(rest);
        return Some(Event::LayoutChange {
            window: WindowId::parse(window)?,
            layout: layout.to_vec(),
        });
    }
    if let Some(rest) = line.strip_prefix(b"%window-close ") {
        return WindowId::parse(field(rest).0).map(Event::WindowClose);
    }
    if line == b"%exit" {
        return Some(Event::Exit { reason: None });
    }
    if let Some(rest) = line.strip_prefix(b"%exit ") {
        return Some(Event::Exit {
            reason: Some(rest.to_vec()),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tmux 3.6a on a private socket: `pause-after=2`, `-A"%0:on"`, a pane
    /// printing a backslash, a tab, SGR, CJK, an emoji, CRLF and an OSC 8
    /// link whose text contains `%0`; then `capture-pane -p -e`, the quoted
    /// and the bare `-A %0:continue` (the latter is the parse error), and
    /// `kill-server`.
    const TMUX_3_6A: &[u8] = include_bytes!("../tests/fixtures/control-mode-tmux-3.6a.ctl");

    /// tmux 3.6a on a private socket: a pane whose title is `%end 1 2 1` and
    /// which prints `%end 1 2 1`, `%continue %0`, `%exit`, `%output %0
    /// INJECT`, `%error 1 2 1` and `%pause %0`; then `display-message -p
    /// "#{pane_title}"` and `capture-pane -p -e`, so every forged line comes
    /// back inside a reply body.
    const INJECT_3_6A: &[u8] =
        include_bytes!("../tests/fixtures/control-mode-inject-tmux-3.6a.ctl");

    fn parse_all(bytes: &[u8], chunk: usize) -> Vec<Event> {
        let mut p = ControlParser::new();
        let mut out = Vec::new();
        for c in bytes.chunks(chunk) {
            out.extend(p.feed(c));
        }
        assert!(p.partial.is_empty(), "fixture ends on a newline");
        out
    }

    fn outputs(events: &[Event]) -> Vec<u8> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Output { data, .. } => Some(data.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect()
    }

    #[test]
    fn the_recorded_stream_decodes_the_same_at_every_chunk_size() {
        let whole = parse_all(TMUX_3_6A, TMUX_3_6A.len());
        for chunk in [1, 2, 3, 7, 64, 4096] {
            assert_eq!(parse_all(TMUX_3_6A, chunk), whole, "chunk {chunk}");
        }
    }

    #[test]
    fn the_recorded_stream_has_the_shape_the_spikes_measured() {
        let events = parse_all(TMUX_3_6A, 4096);
        // The pane bytes come back exactly as the program wrote them: the
        // pty turned `\r\n` into `\r\r\n`, and the backslash and tab were
        // octal on the wire.
        let bytes = outputs(&events);
        assert_eq!(
            bytes,
            b"A\\B\tC\x1b[1mD\x1b[0m \xe4\xb8\xad \xf0\x9f\x9a\x80 \r\r\n\x1b]8;;https://example.invalid/x\x1b\\LINK\x1b]8;;\x1b\\ %0 not a notification\r\r\n".to_vec()
        );
        // Under pause-after every output line is %extended-output with an age.
        assert!(events.iter().all(|e| !matches!(e, Event::Output { age_ms: None, .. })));
        assert!(matches!(
            events.iter().find(|e| matches!(e, Event::Output { .. })),
            Some(Event::Output {
                pane: PaneId(0),
                age_ms: Some(2),
                ..
            })
        ));
        // Replies, in order: the attach (279), pause-after, -A on, display
        // (body `%0`, which is NOT a notification), capture-pane, the quoted
        // continue, the bare continue (error), kill-server.
        let replies: Vec<(bool, u64, usize)> = events
            .iter()
            .filter_map(|e| match e {
                Event::Reply { ok, number, lines } => Some((*ok, *number, lines.len())),
                _ => None,
            })
            .collect();
        assert_eq!(
            replies,
            vec![
                (true, 279, 0),
                (true, 284, 0),
                (true, 285, 0),
                (true, 286, 1),
                (true, 288, 10),
                (true, 289, 0),
                (false, 290, 1),
                (true, 291, 0),
            ]
        );
        let display =
            events.iter().find(|e| matches!(e, Event::Reply { number: 286, .. })).unwrap();
        assert!(matches!(display, Event::Reply { lines, .. } if lines[0] == b"%0"));
        let err = events.iter().find(|e| matches!(e, Event::Reply { ok: false, .. })).unwrap();
        assert!(
            matches!(err, Event::Reply { lines, .. } if lines[0] == b"parse error: syntax error")
        );
        // capture-pane -e bodies are raw: the ESC survives unescaped.
        let capture =
            events.iter().find(|e| matches!(e, Event::Reply { number: 288, .. })).unwrap();
        assert!(matches!(capture, Event::Reply { lines, .. } if lines[1].starts_with(b"\x1b]8;;")));
        assert_eq!(events.last(), Some(&Event::Exit { reason: None }));
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Other { name, .. } if name == b"%session-changed"
        )));
    }

    #[test]
    fn a_grapheme_split_across_two_notifications_is_returned_as_raw_bytes() {
        // The first two bytes of U+1F680 end one line; the rest opens the next.
        let wire =
            b"%extended-output %3 0 : tail \xf0\x9f\n%extended-output %3 1 : \x9a\x80 more\n";
        let mut p = ControlParser::new();
        let events = p.feed(wire);
        assert_eq!(events.len(), 2);
        let bytes = outputs(&events);
        assert_eq!(bytes, b"tail \xf0\x9f\x9a\x80 more");
        assert_eq!(std::str::from_utf8(&bytes).unwrap(), "tail \u{1F680} more");
        assert!(matches!(
            events[0],
            Event::Output {
                pane: PaneId(3),
                age_ms: Some(0),
                ..
            }
        ));
    }

    #[test]
    fn pause_and_continue_inside_a_reply_block_are_surfaced() {
        let wire = b"%pause %0\n%begin 1 288 1\n%continue %0\n%end 1 288 1\n%begin 1 289 1\n%pause %1\nbody line\n%error 1 289 1\n";
        let events = parse_all(wire, 5);
        assert_eq!(
            events,
            vec![
                Event::Pause(PaneId(0)),
                Event::Continue(PaneId(0)),
                Event::Reply {
                    ok: true,
                    number: 288,
                    lines: vec![]
                },
                Event::Pause(PaneId(1)),
                Event::Reply {
                    ok: false,
                    number: 289,
                    lines: vec![b"body line".to_vec()]
                },
            ]
        );
    }

    #[test]
    fn plain_output_and_layout_change_and_exit_reason_decode() {
        let wire = b"%output %7 hi\\033[m\n%layout-change @2 4a8b,40x20,0,0,7 4a8b,40x20,0,0,7 *\n%window-close @2\n%exit detached\n";
        let events = parse_all(wire, 9);
        assert_eq!(
            events,
            vec![
                Event::Output {
                    pane: PaneId(7),
                    age_ms: None,
                    data: b"hi\x1b[m".to_vec()
                },
                Event::LayoutChange {
                    window: WindowId(2),
                    layout: b"4a8b,40x20,0,0,7 4a8b,40x20,0,0,7 *".to_vec()
                },
                Event::WindowClose(WindowId(2)),
                Event::Exit {
                    reason: Some(b"detached".to_vec())
                },
            ]
        );
    }

    #[test]
    fn unvis_round_trips_every_byte_and_leaves_utf8_alone() {
        let all: Vec<u8> = (0..=255u8).collect();
        let escaped = vis(&all);
        assert_eq!(unvis(&escaped), all);
        assert_eq!(vis(b"\\"), b"\\134");
        assert_eq!(vis(b"\t\x1b\r\n"), b"\\011\\033\\015\\012");
        assert_eq!(vis("中".as_bytes()), "中".as_bytes());
        // A lone backslash that is not an escape is kept.
        assert_eq!(unvis(b"a\\9b"), b"a\\9b");
        assert_eq!(unvis(b"a\\"), b"a\\");
        assert_eq!(unvis(b"\\13"), b"\\13");
    }

    #[test]
    fn refresh_client_commands_avoid_the_syntax_trap() {
        assert_eq!(
            refresh_client_pane(PaneId(0), PaneAction::Continue),
            "refresh-client -A\"%0:continue\"\n"
        );
        assert_eq!(
            refresh_client_pane(PaneId(12), PaneAction::On),
            "refresh-client -A\"%12:on\"\n"
        );
        assert_eq!(
            refresh_client_pause_after(2),
            "refresh-client -f pause-after=2\n"
        );
        assert_eq!(
            refresh_client_pause_after(0),
            "refresh-client -f pause-after=1\n"
        );
        assert_eq!(PaneId(3).to_string(), "%3");
        assert_eq!(WindowId(4).to_string(), "@4");
    }

    #[test]
    fn forged_notifications_in_a_reply_body_stay_body_lines() {
        let whole = parse_all(INJECT_3_6A, INJECT_3_6A.len());
        for chunk in [1, 5, 64] {
            assert_eq!(parse_all(INJECT_3_6A, chunk), whole, "chunk {chunk}");
        }
        // The pane's own bytes come through as output, and nothing in them
        // becomes an event.
        let bytes = outputs(&whole);
        assert!(bytes.starts_with(b"\x1b]0;%end 1 2 1\x07%end 1 2 1\r\r\n%continue %0"));
        // The only Exit is tmux's own, last. The forged `%pause` and
        // `%continue` DO surface, by design and documented as
        // unauthenticated: WP8 acts on a Continue only for a pane it
        // recorded as paused at top level. Nothing else was lifted.
        assert_eq!(whole.last(), Some(&Event::Exit { reason: None }));
        assert_eq!(
            whole.iter().filter(|e| matches!(e, Event::Exit { .. })).count(),
            1
        );
        assert_eq!(
            whole
                .iter()
                .filter(|e| matches!(e, Event::Pause(PaneId(0)) | Event::Continue(PaneId(0))))
                .count(),
            2,
            "the two in-block lines lifted, unauthenticated"
        );
        assert_eq!(
            whole.iter().filter(|e| matches!(e, Event::Output { .. })).count(),
            2
        );
        assert!(whole.iter().all(|e| !matches!(
            e,
            Event::Output { data, .. } if data == b"INJECT"
        )));
        // The title reply is one body line, `%end 1 2 1`, and the capture
        // reply holds all six forged lines plus the two blank rows; both
        // close on the real `%end` with the matching fields.
        let replies: Vec<(bool, u64, Vec<Vec<u8>>)> = whole
            .iter()
            .filter_map(|e| match e {
                Event::Reply { ok, number, lines } => Some((*ok, *number, lines.clone())),
                _ => None,
            })
            .collect();
        let numbers: Vec<u64> = replies.iter().map(|r| r.1).collect();
        assert_eq!(numbers, vec![279, 284, 285, 288, 289, 290]);
        assert!(
            replies.iter().all(|r| r.0),
            "no reply closed on the forged %error"
        );
        assert_eq!(replies[3].2, vec![b"%end 1 2 1".to_vec()]);
        // Eight rows captured: the two lifted lines are gone, the other
        // four forged lines and the two blank rows stay.
        assert_eq!(replies[4].2.len(), 6);
        assert_eq!(
            replies[4].2[..4],
            [
                b"%end 1 2 1".to_vec(),
                b"%exit".to_vec(),
                b"%output %0 INJECT".to_vec(),
                b"%error 1 2 1".to_vec(),
            ]
        );
        // The real %end never leaked as Other.
        assert!(whole.iter().all(|e| !matches!(
            e,
            Event::Other { name, .. } if name == b"%end" || name == b"%error"
        )));
    }

    #[test]
    fn a_block_closes_only_on_its_own_fields() {
        let wire = b"%begin 10 5 1\n%end 10 5 0\n%end 11 5 1\n%error 10 6 1\n%end 10 5 1\n";
        let events = parse_all(wire, 4);
        assert_eq!(
            events,
            vec![Event::Reply {
                ok: true,
                number: 5,
                lines: vec![
                    b"%end 10 5 0".to_vec(),
                    b"%end 11 5 1".to_vec(),
                    b"%error 10 6 1".to_vec(),
                ]
            }]
        );
        let wire = b"%begin 10 7 1\nparse error\n%error 10 7 1\n";
        assert!(matches!(
            parse_all(wire, 3).as_slice(),
            [Event::Reply {
                ok: false,
                number: 7,
                ..
            }]
        ));
    }

    #[test]
    fn extended_output_payload_keeps_its_own_colon_fields() {
        // The lead's live line: a pane printing `key : value : more`.
        let wire = b"%extended-output %0 5 : key : value : more\n%extended-output %0 0 : \n%extended-output %0 0 :\n";
        let events = parse_all(wire, 6);
        assert_eq!(
            events,
            vec![
                Event::Output {
                    pane: PaneId(0),
                    age_ms: Some(5),
                    data: b"key : value : more".to_vec()
                },
                Event::Output {
                    pane: PaneId(0),
                    age_ms: Some(0),
                    data: Vec::new()
                },
                Event::Output {
                    pane: PaneId(0),
                    age_ms: Some(0),
                    data: Vec::new()
                },
            ]
        );
        // Only when a field sits between the age and the colon does the
        // payload start after the first ` : `.
        let wire = b"%extended-output %2 5 future : a : b\n";
        assert_eq!(
            parse_all(wire, 6),
            vec![Event::Output {
                pane: PaneId(2),
                age_ms: Some(5),
                data: b"a : b".to_vec()
            }]
        );
    }

    #[test]
    fn a_partial_line_waits_for_the_next_feed() {
        let mut p = ControlParser::new();
        assert!(p.feed(b"%output %1 ab").is_empty());
        assert!(!p.in_block());
        let events = p.feed(b"c\n%begin 1 2 1\n");
        assert_eq!(
            events,
            vec![Event::Output {
                pane: PaneId(1),
                age_ms: None,
                data: b"abc".to_vec()
            }]
        );
        assert!(p.in_block());
        assert!(p.feed(b"%0\n").is_empty(), "a pane id is a body line");
        assert_eq!(
            p.feed(b"%end 1 2 1\n"),
            vec![Event::Reply {
                ok: true,
                number: 2,
                lines: vec![b"%0".to_vec()]
            }]
        );
    }
}
