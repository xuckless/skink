//! Soft wrap: how one buffer line splits into visual rows of a given
//! width, and the display-width rules the screen uses (tabs to the next
//! stop, wide characters two cells, control characters one).
//!
//! A row is a range of char columns. Column `c` is on the last row whose
//! start is `<= c`, so a cursor at the very start of a continuation row
//! shows there, and the end of the line is on the last row. Widths are
//! measured from the start of the line (a tab keeps its width whatever
//! row it lands on), so a row is drawn like the line scrolled sideways to
//! the row's first column.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Whether long lines wrap, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wrap {
    /// One row per line, scrolled sideways.
    #[default]
    Off,
    /// Break at spaces where possible, else between any two characters.
    Words(usize),
    /// Break between any two characters.
    Chars(usize),
}

impl Wrap {
    /// The row width, or `None` when lines do not wrap.
    pub fn width(self) -> Option<usize> {
        match self {
            Wrap::Off => None,
            Wrap::Words(w) | Wrap::Chars(w) => Some(w.max(1)),
        }
    }
}

/// Cells a grapheme cluster takes at display column `x`.
pub fn cell_width(g: &str, x: usize, tab: usize) -> usize {
    match g.chars().next() {
        Some('\t') => tab.max(1) - x % tab.max(1),
        // Drawn as their Control Pictures symbol (one cell).
        Some('\0'..='\x1f' | '\x7f' | '\u{80}'..='\u{9f}') => 1,
        _ => g.width(),
    }
}

/// Grapheme clusters of `line` as (first char index, char count, text).
fn clusters(line: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    let mut i = 0;
    line.graphemes(true).map(move |g| {
        let n = g.chars().count();
        let at = i;
        i += n;
        (at, n, g)
    })
}

/// Display column of char index `col` from the line start.
pub fn display_col(line: &str, col: usize, tab: usize) -> usize {
    let mut x = 0;
    for (i, _, g) in clusters(line) {
        if i >= col {
            break;
        }
        x += cell_width(g, x, tab);
    }
    x
}

/// Char columns where the rows of `line` start: always `[0, ...]`.
pub fn row_starts(line: &str, wrap: Wrap, tab: usize) -> Vec<usize> {
    let mut starts = vec![0];
    let Some(width) = wrap.width() else {
        return starts;
    };
    let words = matches!(wrap, Wrap::Words(_));
    // The current row's first column and display x; the last place to
    // break after a space in it, with its x.
    let (mut row, mut row_x) = (0usize, 0usize);
    let mut brk: Option<(usize, usize)> = None;
    let mut x = 0;
    for (i, n, g) in clusters(line) {
        let w = cell_width(g, x, tab);
        if x + w > row_x + width && i > row {
            match brk.filter(|_| words) {
                Some((at, at_x)) if at > row && x + w <= at_x + width => {
                    (row, row_x) = (at, at_x);
                }
                _ => (row, row_x) = (i, x),
            }
            starts.push(row);
            brk = None;
        }
        x += w;
        if g.chars().all(char::is_whitespace) {
            brk = Some((i + n, x));
        }
    }
    starts
}

/// The row of `starts` that column `col` is on.
pub fn row_of(starts: &[usize], col: usize) -> usize {
    starts.partition_point(|&s| s <= col).saturating_sub(1)
}

/// One screen row of the text: a buffer line, or one wrapped part of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VRow {
    pub line: usize,
    /// Which row of the line (0 = its first).
    pub row: usize,
    /// Char columns of the row, end exclusive (the line's length on the
    /// last row).
    pub start: usize,
    pub end: usize,
    /// Is this the line's last row?
    pub last: bool,
}

impl VRow {
    /// Does this row show column `col` (the line end is on the last row)?
    pub fn holds(&self, col: usize) -> bool {
        self.start <= col && (col < self.end || (self.last && col == self.end))
    }
}

/// The rows of one line.
pub fn rows_of_line(line_no: usize, text: &str, wrap: Wrap, tab: usize) -> Vec<VRow> {
    let len = text.chars().count();
    let starts = row_starts(text, wrap, tab);
    let n = starts.len();
    starts
        .iter()
        .enumerate()
        .map(|(k, &start)| VRow {
            line: line_no,
            row: k,
            start,
            end: starts.get(k + 1).copied().unwrap_or(len),
            last: k + 1 == n,
        })
        .collect()
}

/// Char column at display column `x` inside a row (x counted from the
/// row's first cell); never past the row (on a continued row, its last
/// character), always the start of a cluster.
pub fn col_in_row(text: &str, r: &VRow, x: usize, tab: usize) -> usize {
    let x0 = display_col(text, r.start, tab);
    let target = x0 + x;
    let mut pos = 0;
    let mut last_start = r.start;
    for (i, _, g) in clusters(text) {
        if i >= r.end {
            break;
        }
        let w = cell_width(g, pos, tab);
        // A zero-width cluster (a lone combining mark) shares its cell with
        // the next one: the click goes to the visible character.
        if i >= r.start && w > 0 {
            if target < pos + w {
                return i;
            }
            last_start = i;
        }
        pos += w;
    }
    if r.last { r.end } else { last_start }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn rows(s: &str, w: Wrap) -> Vec<String> {
        let starts = row_starts(s, w, 4);
        let chars: Vec<char> = s.chars().collect();
        starts
            .iter()
            .enumerate()
            .map(|(k, &a)| {
                let b = starts.get(k + 1).copied().unwrap_or(chars.len());
                chars[a..b].iter().collect()
            })
            .collect()
    }

    #[test]
    fn words_break_after_spaces_and_long_words_anywhere() {
        assert_eq!(
            rows("the quick brown fox", Wrap::Words(10)),
            ["the quick ", "brown fox"]
        );
        assert_eq!(
            rows("abcdefghijkl xy", Wrap::Words(5)),
            ["abcde", "fghij", "kl xy"]
        );
        assert_eq!(rows("short", Wrap::Words(10)), ["short"]);
        assert_eq!(rows("", Wrap::Words(10)), [""]);
        assert_eq!(rows("abcdef", Wrap::Chars(4)), ["abcd", "ef"]);
        assert_eq!(rows("ab cdef", Wrap::Chars(4)), ["ab c", "def"]);
        assert_eq!(rows("abcdef", Wrap::Off), ["abcdef"]);
    }

    #[test]
    fn wide_characters_and_tabs_never_overflow_a_row() {
        // Two-cell characters: three fit in 6 or 7 cells, not 3.5.
        assert_eq!(rows("日本語日本", Wrap::Chars(7)), ["日本語", "日本"]);
        // A tab takes the cells to the next stop of the line.
        assert_eq!(rows("ab\tcd", Wrap::Chars(4)), ["ab\t", "cd"]);
        // Width 1 still makes progress with a wide character.
        assert_eq!(rows("日x", Wrap::Chars(1)), ["日", "x"]);
    }

    #[test]
    fn columns_map_to_their_row_and_back() {
        let text = "the quick brown fox";
        let rs = rows_of_line(0, text, Wrap::Words(10), 4);
        assert_eq!(rs.len(), 2);
        let starts: Vec<usize> = rs.iter().map(|r| r.start).collect();
        assert_eq!(row_of(&starts, 0), 0);
        assert_eq!(row_of(&starts, 9), 0);
        assert_eq!(row_of(&starts, 10), 1); // start of the next row
        assert_eq!(row_of(&starts, 19), 1); // line end
        assert_eq!(col_in_row(text, &rs[1], 2, 4), 12);
        // Past the end of a continued row: its last character.
        assert_eq!(col_in_row(text, &rs[0], 50, 4), 9);
        // Past the end of the last row: the line end.
        assert_eq!(col_in_row(text, &rs[1], 50, 4), 19);
    }

    fn ed(text: &str, width: usize) -> crate::Editor {
        let mut e = crate::Editor::new(crate::Buffer::from_text(text));
        e.viewport_height = 3;
        e.set_wrap(Wrap::Words(width));
        e
    }

    #[test]
    fn up_and_down_move_by_screen_rows_keeping_the_column() {
        // Rows of 10: "aaaa bbbb ", "cccc dddd ", "eeee" then line 1.
        let mut e = ed("aaaa bbbb cccc dddd eeee\nshort", 10);
        e.goto(Some(1), Some(3));
        e.move_vertical(1, false);
        assert_eq!((e.cursor.line, e.cursor.col), (0, 12));
        e.move_vertical(1, false);
        assert_eq!((e.cursor.line, e.cursor.col), (0, 22));
        // Last row is shorter: the end, but the column is remembered.
        e.move_vertical(1, false);
        assert_eq!((e.cursor.line, e.cursor.col), (1, 2));
        e.move_vertical(-1, false);
        assert_eq!((e.cursor.line, e.cursor.col), (0, 22));
        e.move_vertical(-2, false);
        assert_eq!((e.cursor.line, e.cursor.col), (0, 2));
        e.move_vertical(-1, false);
        assert_eq!((e.cursor.line, e.cursor.col), (0, 0), "up from the top row");
    }

    #[test]
    fn home_and_end_stop_at_the_row_first() {
        let mut e = ed("aaaa bbbb cccc dddd", 10);
        e.goto(Some(1), Some(13)); // "cc|cc", second row
        e.move_line_end(false);
        assert_eq!(e.cursor.col, 19, "last row: the line end");
        e.goto(Some(1), Some(3));
        e.move_line_end(false);
        assert_eq!(e.cursor.col, 9, "first row: its last character");
        e.move_line_end(false);
        assert_eq!(e.cursor.col, 19, "again: the line end");
        e.goto(Some(1), Some(15));
        e.move_line_start_text(false);
        assert_eq!(e.cursor.col, 10, "the row's start");
        e.move_line_start_text(false);
        assert_eq!(e.cursor.col, 0, "then the line's");
    }

    #[test]
    fn the_view_scrolls_by_rows_to_keep_the_cursor_on_screen() {
        let mut e = ed("aaaa bbbb cccc dddd eeee ffff\nx\ny", 10);
        e.goto(Some(2), None);
        let rows = e.visual_rows(3);
        assert!(rows.iter().any(|r| r.line == 1), "{rows:?}");
        assert_eq!((e.scroll, e.scroll_row), (0, 1));
        e.move_file_start(false);
        assert_eq!((e.scroll, e.scroll_row), (0, 0));
        e.scroll_by(2);
        assert_eq!((e.scroll, e.scroll_row), (0, 2));
        e.scroll_by(10);
        assert_eq!(e.scroll, 2, "stops at the last row");
        // Turning wrap off goes back to whole lines.
        e.set_wrap(Wrap::Off);
        assert_eq!(e.scroll_row, 0);
    }

    proptest! {
        /// Every column is on exactly one row, rows tile the line in order,
        /// no row is wider than the width (a single over-wide cluster
        /// excepted), and a column's x in its row maps back to it.
        #[test]
        fn rows_tile_the_line_and_positions_round_trip(
            s in "[a-c \\t日e\u{301}]{0,40}",
            w in 1usize..12,
            words in any::<bool>(),
        ) {
            let wrap = if words { Wrap::Words(w) } else { Wrap::Chars(w) };
            let rs = rows_of_line(0, &s, wrap, 4);
            let len = s.chars().count();
            prop_assert_eq!(rs[0].start, 0);
            prop_assert_eq!(rs.last().unwrap().end, len);
            for pair in rs.windows(2) {
                prop_assert!(pair[0].end == pair[1].start && pair[0].start < pair[0].end);
            }
            for r in &rs {
                let wide = display_col(&s, r.end, 4) - display_col(&s, r.start, 4);
                let one = clusters(&s).filter(|(i, _, _)| *i >= r.start && *i < r.end).count() <= 1;
                prop_assert!(wide <= w || one, "row {:?} is {} wide", r, wide);
            }
            let visible = clusters(&s).filter(|(_, _, g)| cell_width(g, 0, 4) > 0 || g.starts_with('\t'));
            for (i, _, _) in visible.chain(std::iter::once((len, 0, ""))) {
                let holders: Vec<_> = rs.iter().filter(|r| r.holds(i)).collect();
                prop_assert_eq!(holders.len(), 1, "col {} in {:?}", i, rs);
                let r = holders[0];
                let x = display_col(&s, i, 4) - display_col(&s, r.start, 4);
                prop_assert_eq!(col_in_row(&s, r, x, 4), i);
            }
        }
    }
}

/// Where Up/Down aim on a wrapped line: the display column inside the row,
/// remembered while the cursor only moves up and down (any other move puts
/// the cursor elsewhere and the next Up/Down measures again).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum WantX {
    #[default]
    FromCursor,
    Kept {
        x: usize,
        at: crate::Cursor,
    },
}

impl crate::Editor {
    /// How lines wrap in this view.
    pub fn wrap(&self) -> Wrap {
        self.wrap
    }

    /// Wrap lines at a width, or stop wrapping (the front end calls this
    /// with the text area's width every frame; only a change does work).
    pub fn set_wrap(&mut self, wrap: Wrap) {
        if wrap == self.wrap {
            return;
        }
        self.wrap = wrap;
        self.scroll_row = 0;
        self.want_x = WantX::FromCursor;
        self.scroll_into_view();
    }

    /// The screen rows of buffer line `line`.
    pub fn line_rows(&self, line: usize) -> Vec<VRow> {
        let line = line.min(self.buffer.len_lines() - 1);
        rows_of_line(
            line,
            &self.buffer.line_text(line),
            self.wrap,
            self.tab_width,
        )
    }

    /// The row the cursor is on.
    pub fn cursor_row(&self) -> VRow {
        let rows = self.line_rows(self.cursor.line);
        let col = self.cursor.col;
        rows.iter()
            .copied()
            .find(|r| r.holds(col))
            .unwrap_or(rows[rows.len() - 1])
    }

    /// The first screen row: `scroll`, and the row of it shown at the top.
    fn top(&self) -> (usize, usize) {
        let last = self.buffer.len_lines() - 1;
        let line = self.scroll.min(last);
        let line = self.folds.hiding(line).map_or(line, |f| f.header);
        let n = self.line_rows(line).len();
        (line, self.scroll_row.min(n - 1))
    }

    fn row_below(&self, (line, row): (usize, usize)) -> Option<(usize, usize)> {
        if row + 1 < self.line_rows(line).len() {
            return Some((line, row + 1));
        }
        let last = self.buffer.len_lines() - 1;
        let next = if self.folds.is_empty() {
            (line < last).then_some(line + 1)
        } else {
            self.folds.next_visible(line, last)
        };
        next.map(|l| (l, 0))
    }

    fn row_above(&self, (line, row): (usize, usize)) -> Option<(usize, usize)> {
        if row > 0 {
            return Some((line, row - 1));
        }
        let prev = if self.folds.is_empty() {
            line.checked_sub(1)
        } else {
            self.folds.prev_visible(line)
        }?;
        Some((prev, self.line_rows(prev).len() - 1))
    }

    /// Up to `height` screen rows from the top of the view.
    pub fn visual_rows(&self, height: usize) -> Vec<VRow> {
        let mut out = Vec::with_capacity(height);
        let (mut line, row) = self.top();
        let mut rows = self.line_rows(line);
        out.extend(rows.iter().skip(row).take(height));
        let last = self.buffer.len_lines() - 1;
        while out.len() < height {
            let next = if self.folds.is_empty() {
                (line < last).then_some(line + 1)
            } else {
                self.folds.next_visible(line, last)
            };
            let Some(n) = next else { break };
            line = n;
            rows = self.line_rows(line);
            out.extend(rows.iter().take(height - out.len()));
        }
        out
    }

    /// Keep the cursor's row on screen (wrapped view).
    pub(crate) fn scroll_into_view_wrapped(&mut self) {
        let h = self.viewport_height.max(1);
        if !self.folds.is_empty() {
            self.sync_folds();
            while self.folds.open_hiding(self.cursor.line) {}
        }
        let target = (self.cursor.line, self.cursor_row().row);
        let top = self.top();
        (self.scroll, self.scroll_row) = top;
        if target < top {
            (self.scroll, self.scroll_row) = target;
            return;
        }
        // On screen already?
        let mut at = top;
        for _ in 1..h {
            if at == target {
                return;
            }
            match self.row_below(at) {
                Some(n) => at = n,
                None => break,
            }
        }
        if at == target {
            return;
        }
        // Below: the cursor's row becomes the bottom one.
        let mut top = target;
        for _ in 1..h {
            match self.row_above(top) {
                Some(p) => top = p,
                None => break,
            }
        }
        (self.scroll, self.scroll_row) = top;
    }

    /// Scroll by screen rows without moving the cursor (mouse wheel).
    pub(crate) fn scroll_rows(&mut self, delta: isize) {
        let mut at = self.top();
        for _ in 0..delta.unsigned_abs() {
            let next = if delta > 0 {
                self.row_below(at)
            } else {
                self.row_above(at)
            };
            match next {
                Some(n) => at = n,
                None => break,
            }
        }
        (self.scroll, self.scroll_row) = at;
    }

    /// The cursor's display column inside its row.
    fn cursor_x(&self) -> usize {
        let text = self.buffer.line_text(self.cursor.line);
        let r = self.cursor_row();
        display_col(&text, self.cursor.col, self.tab_width)
            - display_col(&text, r.start, self.tab_width)
    }

    /// Up/Down by screen rows.
    pub(crate) fn move_rows(&mut self, delta: isize, select: bool) {
        let x = match self.want_x {
            WantX::Kept { x, at } if at == self.cursor => x,
            _ => self.cursor_x(),
        };
        let mut at = (self.cursor.line, self.cursor_row().row);
        let mut moved = 0;
        for _ in 0..delta.unsigned_abs() {
            let next = if delta > 0 {
                self.row_below(at)
            } else {
                self.row_above(at)
            };
            match next {
                Some(n) => {
                    at = n;
                    moved += 1;
                }
                None => break,
            }
        }
        // Past the first row: line start; past the last: the end (Micro).
        let target = if moved == 0 && delta < 0 {
            crate::Cursor { line: 0, col: 0 }
        } else if moved == 0 && delta > 0 {
            crate::Cursor {
                line: at.0,
                col: usize::MAX,
            }
        } else {
            let text = self.buffer.line_text(at.0);
            let r = self.line_rows(at.0)[at.1];
            crate::Cursor {
                line: at.0,
                col: col_in_row(&text, &r, x, self.tab_width),
            }
        };
        self.move_to(target, select, true);
        self.want_x = WantX::Kept { x, at: self.cursor };
    }

    /// Home on a continued row: the row's start first; `None` when the
    /// cursor is on the first row or already at its row's start.
    pub(crate) fn row_home(&self) -> Option<usize> {
        let r = self.cursor_row();
        (r.row > 0 && self.cursor.col != r.start).then_some(r.start)
    }

    /// End on a row that continues: its last character first; `None` on
    /// the line's last row or when already there.
    pub(crate) fn row_end(&self) -> Option<usize> {
        let r = self.cursor_row();
        if r.last {
            return None;
        }
        let idx = self.buffer.line_to_char(r.line) + r.end;
        let end = self.cursor_at(self.prev_grapheme(idx)).col;
        (self.cursor.col != end).then_some(end)
    }
}
