//! Canonical screen dump for byte-equality tests, ported from spike 2's
//! `canon.rs` and extended with the tracked modes.
//!
//! [`Canon::render`] lowers a [`PaneEmulator`] into text that two emulators
//! can be compared on: grid text, per-cell colours, attributes, OSC 8
//! target and double-width flags, the cursor, the alt-screen flag, the title,
//! and the modes the snapshot must carry. Trailing default blank cells are
//! dropped per row ("ANSI normalisation"), so a repaint that stops at the
//! last written cell compares equal to the original grid.
//!
//! This is the fidelity oracle: WP4 proves `canon(emu) == canon(fresh <-
//! snapshot(emu))`, and lane D's G1 compares the daemon's wire snapshot
//! against a direct pty the same way.

use std::fmt::Write as _;

use wezterm_term::color::ColorAttribute;
use wezterm_term::{CellAttributes, Intensity, Line, Underline};

use crate::emulator::{Modes, PaneEmulator};

/// One cell as the comparison sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cell {
    /// Grapheme content. Empty means an unwritten or blank cell.
    pub text: String,
    /// Columns this cell occupies (1 or 2; 0 for a spacer).
    pub width: u8,
    /// The trailing half of a double-width grapheme.
    pub spacer: bool,
    /// Foreground: `-`, `i<n>` or `#rrggbb`.
    pub fg: String,
    /// Background, same forms.
    pub bg: String,
    /// Sorted attribute tags such as `b`, `b,u`, `rev`.
    pub attrs: String,
    /// OSC 8 target, with `id=` prefixed when the link has one.
    pub link: String,
}

impl Cell {
    fn blank() -> Self {
        Self {
            fg: "-".into(),
            bg: "-".into(),
            width: 1,
            ..Self::default()
        }
    }

    fn is_default(&self) -> bool {
        (self.text.is_empty() || self.text == " ") && !self.spacer && self.style_is_default()
    }

    fn style_key(&self) -> String {
        format!(
            "fg={} bg={} at={} link={}",
            self.fg, self.bg, self.attrs, self.link
        )
    }

    fn style_is_default(&self) -> bool {
        self.fg == "-" && self.bg == "-" && self.attrs.is_empty() && self.link.is_empty()
    }
}

/// The whole comparable state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Canon {
    /// Columns.
    pub cols: usize,
    /// Rows.
    pub rows: usize,
    /// Alternate screen active.
    pub alt: bool,
    /// Cursor row, zero-based.
    pub cursor_row: usize,
    /// Cursor column, zero-based.
    pub cursor_col: usize,
    /// DECTCEM.
    pub cursor_visible: bool,
    /// Window title.
    pub title: String,
    /// The tracked modes, rendered by `Debug`.
    pub modes: String,
    /// Scrollback rows above the viewport, oldest first.
    pub scrollback: Vec<Vec<Cell>>,
    /// The viewport.
    pub grid: Vec<Vec<Cell>>,
    /// Per scrollback row: the row wrapped into the next.
    pub scrollback_wrapped: Vec<bool>,
    /// Per viewport row: the row wrapped into the next.
    pub grid_wrapped: Vec<bool>,
}

/// `-`, `i<n>` or `#rrggbb`.
pub fn color_key(c: ColorAttribute) -> String {
    match c {
        ColorAttribute::Default => "-".into(),
        ColorAttribute::PaletteIndex(i) => format!("i{i}"),
        ColorAttribute::TrueColorWithPaletteFallback(t, _)
        | ColorAttribute::TrueColorWithDefaultFallback(t) => {
            let (r, g, b, _) = t.as_rgba_u8();
            format!("#{r:02x}{g:02x}{b:02x}")
        }
    }
}

fn attr_tags(a: &CellAttributes) -> String {
    let mut tags = Vec::new();
    match a.intensity() {
        Intensity::Bold => tags.push("b"),
        Intensity::Half => tags.push("dim"),
        Intensity::Normal => {}
    }
    if a.italic() {
        tags.push("i");
    }
    match a.underline() {
        Underline::None => {}
        Underline::Single => tags.push("u"),
        Underline::Double => tags.push("uu"),
        Underline::Curly => tags.push("u~"),
        Underline::Dotted => tags.push("u:"),
        Underline::Dashed => tags.push("u-"),
    }
    match a.blink() {
        wezterm_term::Blink::None => {}
        wezterm_term::Blink::Slow => tags.push("blink"),
        wezterm_term::Blink::Rapid => tags.push("blink!"),
    }
    if a.reverse() {
        tags.push("rev");
    }
    if a.strikethrough() {
        tags.push("strike");
    }
    if a.invisible() {
        tags.push("inv");
    }
    if a.overline() {
        tags.push("over");
    }
    tags.join(",")
}

/// The OSC 8 key of a cell: `id=<id> <uri>` or `<uri>`, empty when none.
pub fn link_key(a: &CellAttributes) -> String {
    a.hyperlink().map_or_else(String::new, |h| {
        h.params()
            .get("id")
            .map_or_else(|| h.uri().to_string(), |id| format!("id={id} {}", h.uri()))
    })
}

/// Lower one emulator line into cells, `cols` wide.
pub fn canon_line(line: &Line, cols: usize) -> Vec<Cell> {
    let mut row = vec![Cell::blank(); cols];
    for cr in line.visible_cells() {
        let idx = cr.cell_index();
        if idx >= cols {
            break;
        }
        let a = cr.attrs();
        let s = cr.str();
        let width = u8::try_from(cr.width().max(1)).unwrap_or(u8::MAX);
        row[idx] = Cell {
            text: if s == " " {
                String::new()
            } else {
                s.to_string()
            },
            width,
            spacer: false,
            fg: color_key(a.foreground()),
            bg: color_key(a.background()),
            attrs: attr_tags(a),
            link: link_key(a),
        };
        if width == 2 && idx + 1 < cols {
            row[idx + 1] = Cell {
                text: String::new(),
                width: 0,
                spacer: true,
                fg: row[idx].fg.clone(),
                bg: row[idx].bg.clone(),
                attrs: row[idx].attrs.clone(),
                link: row[idx].link.clone(),
            };
        }
    }
    row
}

/// The viewport lines and, above them, the last `scrollback_rows` lines of
/// history (fewer when the pane holds fewer).
pub fn lines(pane: &PaneEmulator, scrollback_rows: usize) -> (Vec<Line>, Vec<Line>) {
    let screen = pane.terminal().screen();
    let total = screen.scrollback_rows();
    let rows = screen.physical_rows.min(total);
    let top = total - rows;
    let sb = scrollback_rows.min(top);
    let history = screen.lines_in_phys_range(top - sb..top);
    let viewport = screen.lines_in_phys_range(top..total);
    (history, viewport)
}

/// Lower the emulator, with `scrollback_rows` rows of history.
pub fn canon(pane: &mut PaneEmulator, scrollback_rows: usize) -> Canon {
    let _ = pane.flush();
    let (cols, rows) = pane.size();
    let cols = usize::from(cols);
    let (history, viewport) = lines(pane, scrollback_rows);
    let cursor = pane.cursor();
    Canon {
        cols,
        rows: usize::from(rows),
        alt: pane.is_alt_screen(),
        cursor_row: usize::from(cursor.row),
        cursor_col: usize::from(cursor.col),
        cursor_visible: cursor.visible,
        title: pane.title().to_string(),
        modes: modes_key(pane.modes()),
        scrollback: history.iter().map(|l| canon_line(l, cols)).collect(),
        grid: viewport.iter().map(|l| canon_line(l, cols)).collect(),
        scrollback_wrapped: history.iter().map(Line::last_cell_was_wrapped).collect(),
        grid_wrapped: viewport.iter().map(Line::last_cell_was_wrapped).collect(),
    }
}

/// The tracked modes as one comparable string.
pub fn modes_key(m: &Modes) -> String {
    format!("{m:?}")
}

impl Canon {
    /// The comparable text.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "size {}x{}", self.cols, self.rows);
        let _ = writeln!(out, "alt {}", u8::from(self.alt));
        let _ = writeln!(
            out,
            "cursor r={} c={} vis={}",
            self.cursor_row,
            self.cursor_col,
            u8::from(self.cursor_visible)
        );
        let _ = writeln!(out, "title {:?}", self.title);
        let _ = writeln!(out, "modes {}", self.modes);
        for (r, row) in self.scrollback.iter().enumerate() {
            let label = format!("s{r:03}");
            render_row(&mut out, &label, row);
            if self.scrollback_wrapped.get(r).copied().unwrap_or(false) {
                let _ = writeln!(out, "{label} wrap");
            }
        }
        for (r, row) in self.grid.iter().enumerate() {
            let label = format!("r{r:03}");
            render_row(&mut out, &label, row);
            if self.grid_wrapped.get(r).copied().unwrap_or(false) {
                let _ = writeln!(out, "{label} wrap");
            }
        }
        out
    }

    /// The text of viewport row `r`, trailing blanks trimmed.
    pub fn row_text(&self, r: usize) -> String {
        row_text(&self.grid[r])
    }
}

fn row_text(row: &[Cell]) -> String {
    let mut last = row.len();
    while last > 0 && row[last - 1].is_default() {
        last -= 1;
    }
    let mut text = String::new();
    for c in &row[..last] {
        if c.spacer {
            continue;
        }
        if c.text.is_empty() {
            text.push(' ');
        } else {
            text.push_str(&c.text);
        }
    }
    text
}

fn render_row(out: &mut String, label: &str, row: &[Cell]) {
    let mut last = row.len();
    while last > 0 && row[last - 1].is_default() {
        last -= 1;
    }
    let live = &row[..last];
    let _ = writeln!(out, "{label} t |{}|", row_text(row));
    // Style runs, only where a run differs from the default style.
    let mut i = 0usize;
    while i < live.len() {
        let key = live[i].style_key();
        let mut j = i + 1;
        while j < live.len() && live[j].style_key() == key {
            j += 1;
        }
        if !live[i].style_is_default() {
            let _ = writeln!(out, "{label} a {}..{} {}", i, j - 1, key);
        }
        i = j;
    }
    // Double-width cells are load-bearing for layout, so record them.
    let wides: Vec<String> = live
        .iter()
        .enumerate()
        .filter(|(_, c)| c.width == 2 && !c.spacer)
        .map(|(i, _)| i.to_string())
        .collect();
    if !wides.is_empty() {
        let _ = writeln!(out, "{label} w {}", wides.join(","));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_records_text_styles_links_wides_and_modes() {
        let mut p = PaneEmulator::new(20, 3, 100);
        p.feed(b"\x1b]0;t\x07\x1b[1;31mab\x1b[0m \x1b]8;id=q;https://x/\x1b\\L\x1b]8;;\x1b\\\r\n\xe4\xb8\xad\x1b[48;2;1;2;3m \x1b[0m\r\n\x1b[?1002h\x1b[?25l")
            .unwrap();
        let c = canon(&mut p, 0);
        let text = c.render();
        assert!(
            text.starts_with("size 20x3\nalt 0\ncursor r=2 c=0 vis=0\ntitle \"t\"\nmodes Modes {")
        );
        assert!(text.contains("mouse: Button"), "{text}");
        assert!(text.contains("r000 t |ab L|\n"), "{text}");
        assert!(
            text.contains("r000 a 0..1 fg=i1 bg=- at=b link=\n"),
            "{text}"
        );
        assert!(
            text.contains("r000 a 3..3 fg=- bg=- at= link=id=q https://x/\n"),
            "{text}"
        );
        assert!(text.contains("r001 t |\u{4E2D} |\n"), "{text}");
        assert!(
            text.contains("r001 a 2..2 fg=- bg=#010203 at= link=\n"),
            "{text}"
        );
        assert!(text.contains("r001 w 0\n"), "{text}");
        assert!(text.contains("r002 t ||\n"), "{text}");
        assert_eq!(c.row_text(0), "ab L");
    }

    #[test]
    fn scrollback_rows_come_out_oldest_first_and_are_clamped() {
        let mut p = PaneEmulator::new(10, 2, 100);
        for i in 0..6 {
            p.feed(format!("l{i}\r\n").as_bytes()).unwrap();
        }
        // Six lines plus the cursor line: the viewport holds l5 and an
        // empty line, four lines are history.
        let c = canon(&mut p, 2);
        assert_eq!(c.scrollback.len(), 2);
        assert_eq!(row_text(&c.scrollback[0]), "l3");
        assert_eq!(row_text(&c.scrollback[1]), "l4");
        assert_eq!(c.row_text(0), "l5");
        assert_eq!(c.row_text(1), "");
        assert_eq!(
            canon(&mut p, 50).scrollback.len(),
            5,
            "clamped to what exists"
        );
        assert_eq!(canon(&mut p, 0).scrollback.len(), 0);
        assert!(canon(&mut p, 2).render().contains("s000 t |l3|\ns001 t |l4|\nr000 t |l5|\n"));
    }
}
