use crate::{
    Buffer,
    history::{Edit, History},
};

/// Cursor position: 0-based line and char column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    pub line: usize,
    pub col: usize,
}

/// Which side of a merge conflict to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Ours,
    Theirs,
    Both,
}

/// A `<<<<<<<` / `=======` / `>>>>>>>` block, as 0-based line numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conflict {
    pub start: usize,
    /// First line of the `|||||||` base section (diff3), if any.
    pub base: Option<usize>,
    pub mid: usize,
    pub end: usize,
}

/// Things the editor asks the front end to do (prompts, exit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Quit,
    SaveAs,
    Find,
}

/// Editing state for one buffer. UI-free.
#[derive(Debug)]
pub struct Editor {
    pub buffer: Buffer,
    pub cursor: Cursor,
    /// Other end of the selection, if any.
    pub anchor: Option<Cursor>,
    /// First visible line.
    pub scroll: usize,
    pub viewport_height: usize,
    pub request: Option<Request>,
    /// One-line status message ("Saved", errors).
    pub message: Option<String>,
    /// Internal register; the front end mirrors it to the system clipboard.
    pub clipboard: String,
    /// Set when `clipboard` changed and should be pushed to the system.
    pub clipboard_changed: bool,
    pub tab_width: usize,
    pub last_search: String,
    want_col: usize,
    history: History,
}

impl Editor {
    pub fn new(buffer: Buffer) -> Self {
        Self {
            buffer,
            cursor: Cursor::default(),
            anchor: None,
            scroll: 0,
            viewport_height: 24,
            request: None,
            message: None,
            clipboard: String::new(),
            clipboard_changed: false,
            tab_width: 4,
            last_search: String::new(),
            want_col: 0,
            history: History::default(),
        }
    }

    // ---- positions -------------------------------------------------------

    fn last_line(&self) -> usize {
        self.buffer.len_lines() - 1
    }

    /// Char index of a cursor position (clamped).
    pub fn idx(&self, c: Cursor) -> usize {
        let line = c.line.min(self.last_line());
        self.buffer.line_to_char(line) + c.col.min(self.buffer.line_len(line))
    }

    pub fn cursor_at(&self, idx: usize) -> Cursor {
        let idx = idx.min(self.buffer.len_chars());
        let line = self.buffer.char_to_line(idx);
        Cursor {
            line,
            col: idx - self.buffer.line_to_char(line),
        }
    }

    pub fn cursor_idx(&self) -> usize {
        self.idx(self.cursor)
    }

    fn set_cursor_idx(&mut self, idx: usize) {
        self.cursor = self.cursor_at(idx);
        self.want_col = self.cursor.col;
        self.scroll_into_view();
    }

    /// Selection as an ordered char range, if non-empty.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.idx(self.anchor?);
        let c = self.cursor_idx();
        (a != c).then(|| (a.min(c), a.max(c)))
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|(s, e)| self.buffer.slice(s, e))
    }

    // ---- view ------------------------------------------------------------

    pub fn set_viewport_height(&mut self, h: usize) {
        let h = h.max(1);
        if h != self.viewport_height {
            self.viewport_height = h;
            self.scroll_into_view();
        }
    }

    pub fn scroll_into_view(&mut self) {
        let h = self.viewport_height.max(1);
        if self.cursor.line < self.scroll {
            self.scroll = self.cursor.line;
        } else if self.cursor.line >= self.scroll + h {
            self.scroll = self.cursor.line + 1 - h;
        }
    }

    /// Scroll the view (mouse wheel) without moving the cursor.
    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.last_line();
        self.scroll = self.scroll.saturating_add_signed(delta).min(max);
    }

    /// Jump to a 1-based line/column, clamped to the buffer.
    pub fn goto(&mut self, line: Option<usize>, col: Option<usize>) {
        let line = line
            .map_or(0, |l| l.saturating_sub(1))
            .min(self.last_line());
        let col = col
            .map_or(0, |c| c.saturating_sub(1))
            .min(self.buffer.line_len(line));
        self.history.close();
        self.anchor = None;
        self.cursor = Cursor { line, col };
        self.want_col = col;
        self.scroll_into_view();
    }

    // ---- movement --------------------------------------------------------

    /// Move the cursor, extending the selection if `select`.
    fn move_to(&mut self, target: Cursor, select: bool, keep_want: bool) {
        self.history.close();
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        let line = target.line.min(self.last_line());
        self.cursor = Cursor {
            line,
            col: target.col.min(self.buffer.line_len(line)),
        };
        if !keep_want {
            self.want_col = self.cursor.col;
        }
        self.scroll_into_view();
    }

    pub fn move_vertical(&mut self, delta: isize, select: bool) {
        let line = self.cursor.line.saturating_add_signed(delta);
        let line = line.min(self.last_line());
        // Moving up from the first line goes to column 0, down from the
        // last line to its end (Micro behaviour).
        let target = if delta < 0 && self.cursor.line == 0 {
            Cursor { line: 0, col: 0 }
        } else if delta > 0 && self.cursor.line == self.last_line() {
            Cursor {
                line,
                col: usize::MAX,
            }
        } else {
            Cursor {
                line,
                col: self.want_col,
            }
        };
        let keep = target.col == self.want_col;
        self.move_to(target, select, keep);
    }

    pub fn move_left(&mut self, select: bool) {
        if !select && let Some((s, _)) = self.selection() {
            let c = self.cursor_at(s);
            return self.move_to(c, false, false);
        }
        let i = self.cursor_idx().saturating_sub(1);
        let c = self.cursor_at(i);
        self.move_to(c, select, false);
    }

    pub fn move_right(&mut self, select: bool) {
        if !select && let Some((_, e)) = self.selection() {
            let c = self.cursor_at(e);
            return self.move_to(c, false, false);
        }
        let i = (self.cursor_idx() + 1).min(self.buffer.len_chars());
        let c = self.cursor_at(i);
        self.move_to(c, select, false);
    }

    pub fn move_word_left(&mut self, select: bool) {
        let i = self.word_left(self.cursor_idx());
        let c = self.cursor_at(i);
        self.move_to(c, select, false);
    }

    pub fn move_word_right(&mut self, select: bool) {
        let i = self.word_right(self.cursor_idx());
        let c = self.cursor_at(i);
        self.move_to(c, select, false);
    }

    pub fn move_line_start(&mut self, select: bool) {
        let c = Cursor {
            line: self.cursor.line,
            col: 0,
        };
        self.move_to(c, select, false);
    }

    /// Home: first non-blank, toggling to column 0 (Micro's StartOfTextToggle).
    pub fn move_line_start_text(&mut self, select: bool) {
        let text = self.buffer.line_text(self.cursor.line);
        let first = text.chars().take_while(|c| c.is_whitespace()).count();
        let col = if self.cursor.col == first { 0 } else { first };
        self.move_to(
            Cursor {
                line: self.cursor.line,
                col,
            },
            select,
            false,
        );
    }

    pub fn move_line_end(&mut self, select: bool) {
        let c = Cursor {
            line: self.cursor.line,
            col: usize::MAX,
        };
        self.move_to(c, select, false);
    }

    pub fn move_file_start(&mut self, select: bool) {
        self.move_to(Cursor::default(), select, false);
    }

    pub fn move_file_end(&mut self, select: bool) {
        let c = Cursor {
            line: usize::MAX,
            col: usize::MAX,
        };
        self.move_to(c, select, false);
    }

    pub fn select_all(&mut self) {
        self.history.close();
        self.anchor = Some(Cursor::default());
        self.cursor = Cursor {
            line: self.last_line(),
            col: usize::MAX,
        };
        self.cursor.col = self.buffer.line_len(self.cursor.line);
        self.scroll_into_view();
    }

    pub fn deselect(&mut self) {
        self.anchor = None;
        self.message = None;
    }

    /// Place the cursor from a mouse position (0-based line/col).
    pub fn click(&mut self, line: usize, col: usize, extend: bool) {
        self.move_to(Cursor { line, col }, extend, false);
    }

    fn class(c: char) -> u8 {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    }

    fn word_left(&self, mut i: usize) -> usize {
        while i > 0
            && self
                .buffer
                .char_at(i - 1)
                .is_some_and(|c| Self::class(c) == 1)
        {
            i -= 1;
        }
        if let Some(k) = i
            .checked_sub(1)
            .and_then(|j| self.buffer.char_at(j))
            .map(Self::class)
        {
            while i > 0
                && self
                    .buffer
                    .char_at(i - 1)
                    .is_some_and(|c| Self::class(c) == k)
            {
                i -= 1;
            }
        }
        i
    }

    fn word_right(&self, mut i: usize) -> usize {
        let n = self.buffer.len_chars();
        while i < n && self.buffer.char_at(i).is_some_and(|c| Self::class(c) == 1) {
            i += 1;
        }
        if let Some(k) = self.buffer.char_at(i).map(Self::class) {
            while i < n && self.buffer.char_at(i).is_some_and(|c| Self::class(c) == k) {
                i += 1;
            }
        }
        i
    }

    // ---- editing ---------------------------------------------------------

    fn writable(&mut self) -> bool {
        if self.buffer.is_read_only() {
            self.message = Some("Buffer is read-only".into());
            false
        } else {
            true
        }
    }

    /// Replace `[start, end)` with `text` and record it for undo.
    fn replace(&mut self, start: usize, end: usize, text: &str, typing: bool, into_open: bool) {
        let edit = Edit {
            at: start,
            removed: self.buffer.slice(start, end),
            inserted: text.to_owned(),
        };
        let before = self.cursor_idx();
        self.buffer.remove(start, end);
        self.buffer.insert(start, text);
        let after = start + text.chars().count();
        if into_open {
            self.history.record_into_open(edit, after);
        } else {
            self.history.record(edit, before, after, typing);
        }
        self.anchor = None;
        self.set_cursor_idx(after);
    }

    /// Insert text at the cursor, replacing any selection.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() || !self.writable() {
            return;
        }
        let text = if text.contains('\r') {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.to_owned()
        };
        match self.selection() {
            Some((s, e)) => self.replace(s, e, &text, false, false),
            None => {
                let at = self.cursor_idx();
                let typing = text.chars().count() == 1 && text != "\n";
                self.replace(at, at, &text, typing, false);
            }
        }
    }

    pub fn insert_char(&mut self, c: char) {
        let mut b = [0; 4];
        self.insert_text(c.encode_utf8(&mut b));
    }

    /// Newline, keeping the current line's indentation.
    pub fn insert_newline(&mut self) {
        let indent: String = self
            .buffer
            .line_text(self.cursor.line)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .take(self.cursor.col)
            .collect();
        self.insert_text(&format!("\n{indent}"));
    }

    pub fn insert_tab(&mut self) {
        if self.selection().is_some() {
            return self.indent_lines(true);
        }
        self.insert_text("\t");
    }

    pub fn delete_selection(&mut self) -> bool {
        if !self.writable() {
            return false;
        }
        match self.selection() {
            Some((s, e)) => {
                self.replace(s, e, "", false, false);
                true
            }
            None => false,
        }
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() || !self.writable() {
            return;
        }
        let i = self.cursor_idx();
        if i > 0 {
            self.replace(i - 1, i, "", false, false);
        }
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection() || !self.writable() {
            return;
        }
        let i = self.cursor_idx();
        if i < self.buffer.len_chars() {
            self.replace(i, i + 1, "", false, false);
        }
    }

    pub fn delete_word_left(&mut self) {
        if self.delete_selection() || !self.writable() {
            return;
        }
        let i = self.cursor_idx();
        let w = self.word_left(i);
        if w < i {
            self.replace(w, i, "", false, false);
        }
    }

    pub fn delete_word_right(&mut self) {
        if self.delete_selection() || !self.writable() {
            return;
        }
        let i = self.cursor_idx();
        let w = self.word_right(i);
        if w > i {
            self.replace(i, w, "", false, false);
        }
    }

    /// First and last line touched by the selection (or the cursor line).
    fn line_span(&self) -> (usize, usize) {
        match self.selection() {
            Some((s, e)) => {
                let (a, b) = (self.cursor_at(s), self.cursor_at(e));
                // A selection ending at column 0 does not include that line.
                let last = if b.col == 0 && b.line > a.line {
                    b.line - 1
                } else {
                    b.line
                };
                (a.line, last)
            }
            None => (self.cursor.line, self.cursor.line),
        }
    }

    /// Indent (or outdent) every line in the selection / the cursor line.
    pub fn indent_lines(&mut self, indent: bool) {
        if !self.writable() {
            return;
        }
        let (first, last) = self.line_span();
        let (anchor, cursor) = (self.anchor, self.cursor);
        let mut started = false;
        for line in (first..=last).rev() {
            let start = self.buffer.line_to_char(line);
            let (end, text) = if indent {
                (start, "\t".to_owned())
            } else {
                let l = self.buffer.line_text(line);
                let n = if l.starts_with('\t') {
                    1
                } else {
                    l.chars()
                        .take(self.tab_width)
                        .take_while(|c| *c == ' ')
                        .count()
                };
                (start + n, String::new())
            };
            if !indent && end == start {
                continue;
            }
            self.replace(start, end, &text, false, started);
            started = true;
        }
        // Restore the selection, shifted by the change on its own line.
        let shift = |c: Cursor| -> Cursor {
            if c.line < first || c.line > last {
                return c;
            }
            let col = if indent {
                c.col + 1
            } else {
                c.col.saturating_sub(1)
            };
            Cursor { line: c.line, col }
        };
        let (new_cursor, new_anchor) = (shift(cursor), anchor.map(shift));
        self.cursor = new_cursor;
        self.anchor = new_anchor;
        self.want_col = self.cursor.col;
    }

    /// Swap the selected lines with their neighbour above (`up`) or below.
    pub fn move_lines(&mut self, up: bool) {
        if !self.writable() {
            return;
        }
        let (first, last) = self.line_span();
        if (up && first == 0) || (!up && last >= self.last_line()) {
            return;
        }
        let (lo, hi) = if up {
            (first - 1, last)
        } else {
            (first, last + 1)
        };
        let start = self.buffer.line_to_char(lo);
        let end = if hi < self.last_line() {
            self.buffer.line_to_char(hi + 1)
        } else {
            self.buffer.len_chars()
        };
        let trailing_nl = self.buffer.char_at(end.wrapping_sub(1)) == Some('\n') && end > start;
        let mut lines: Vec<String> = (lo..=hi).map(|l| self.buffer.line_text(l)).collect();
        if up {
            lines.rotate_left(1); // moved-from line goes below the block
        // block was [prev, sel...] -> [sel..., prev]
        } else {
            lines.rotate_right(1); // [sel..., next] -> [next, sel...]
        }
        let mut text = lines.join("\n");
        if trailing_nl {
            text.push('\n');
        }
        let (anchor, cursor) = (self.anchor, self.cursor);
        self.replace(start, end, &text, false, false);
        let d = if up { -1isize } else { 1 };
        let moved = |c: Cursor| Cursor {
            line: c.line.saturating_add_signed(d),
            col: c.col,
        };
        let (new_cursor, new_anchor) = (moved(cursor), anchor.map(moved));
        self.cursor = new_cursor;
        self.anchor = new_anchor;
        self.scroll_into_view();
    }

    pub fn duplicate_line(&mut self) {
        if !self.writable() {
            return;
        }
        let (first, last) = self.line_span();
        let start = self.buffer.line_to_char(first);
        let end = if last < self.last_line() {
            self.buffer.line_to_char(last + 1)
        } else {
            self.buffer.len_chars()
        };
        let mut block = self.buffer.slice(start, end);
        if !block.ends_with('\n') {
            block.insert(0, '\n');
        }
        let cursor = self.cursor;
        self.replace(end, end, &block, false, false);
        self.cursor = Cursor {
            line: cursor.line + (last - first + 1),
            col: cursor.col,
        };
        self.scroll_into_view();
    }

    // ---- clipboard -------------------------------------------------------

    fn set_clipboard(&mut self, text: String) {
        self.clipboard = text;
        self.clipboard_changed = true;
    }

    /// Copy the selection; returns false if there was none.
    pub fn copy(&mut self) -> bool {
        match self.selected_text() {
            Some(t) => {
                self.set_clipboard(t);
                true
            }
            None => false,
        }
    }

    pub fn copy_line(&mut self) {
        let (first, last) = self.line_span();
        let start = self.buffer.line_to_char(first);
        let end = if last < self.last_line() {
            self.buffer.line_to_char(last + 1)
        } else {
            self.buffer.len_chars()
        };
        let mut t = self.buffer.slice(start, end);
        if !t.ends_with('\n') {
            t.push('\n');
        }
        self.set_clipboard(t);
    }

    pub fn cut(&mut self) -> bool {
        if self.copy() {
            self.delete_selection();
            true
        } else {
            false
        }
    }

    pub fn cut_line(&mut self) {
        if !self.writable() {
            return;
        }
        self.copy_line();
        let (first, last) = self.line_span();
        let start = self.buffer.line_to_char(first);
        let end = if last < self.last_line() {
            self.buffer.line_to_char(last + 1)
        } else {
            self.buffer.len_chars()
        };
        // Cutting the final line (no trailing newline) also eats the newline before it.
        let start = if end == self.buffer.len_chars()
            && first > 0
            && !self.buffer.slice(start, end).ends_with('\n')
        {
            start - 1
        } else {
            start
        };
        self.replace(start, end, "", false, false);
    }

    pub fn paste(&mut self) {
        let t = self.clipboard.clone();
        self.insert_text(&t);
    }

    // ---- undo ------------------------------------------------------------

    pub fn undo(&mut self) {
        let Some(g) = self.history.pop_undo() else {
            self.message = Some("Nothing to undo".into());
            return;
        };
        for e in g.edits.iter().rev() {
            self.buffer.remove(e.at, e.at + e.inserted.chars().count());
            self.buffer.insert(e.at, &e.removed);
        }
        self.anchor = None;
        self.set_cursor_idx(g.before);
        self.history.push_redo(g);
    }

    pub fn redo(&mut self) {
        let Some(g) = self.history.pop_redo() else {
            self.message = Some("Nothing to redo".into());
            return;
        };
        for e in &g.edits {
            self.buffer.remove(e.at, e.at + e.removed.chars().count());
            self.buffer.insert(e.at, &e.inserted);
        }
        self.anchor = None;
        self.set_cursor_idx(g.after);
        self.history.push_undo_keep_redo(g);
    }

    // ---- merge conflicts -------------------------------------------------

    /// All conflict blocks in the buffer, in order.
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut out = Vec::new();
        let (mut start, mut base, mut mid) = (None, None, None);
        for i in 0..self.buffer.len_lines() {
            let l = self.buffer.line_text(i);
            if l.starts_with("<<<<<<<") {
                (start, base, mid) = (Some(i), None, None);
            } else if l.starts_with("|||||||") && start.is_some() && mid.is_none() {
                base = Some(i);
            } else if l == "=======" && start.is_some() && mid.is_none() {
                mid = Some(i);
            } else if l.starts_with(">>>>>>>") {
                if let (Some(s), Some(m)) = (start, mid) {
                    out.push(Conflict {
                        start: s,
                        base,
                        mid: m,
                        end: i,
                    });
                }
                (start, base, mid) = (None, None, None);
            }
        }
        out
    }

    /// Move to the next/previous conflict (wrapping); false if there is none.
    pub fn goto_conflict(&mut self, forward: bool) -> bool {
        let all = self.conflicts();
        let cur = self.cursor.line;
        let pick = if forward {
            all.iter().find(|c| c.start > cur).or(all.first())
        } else {
            all.iter().rev().find(|c| c.start < cur).or(all.last())
        };
        match pick {
            Some(c) => {
                let line = c.start;
                self.history.close();
                self.anchor = None;
                self.cursor = Cursor { line, col: 0 };
                self.want_col = 0;
                self.scroll_into_view();
                true
            }
            None => {
                self.message = Some("No merge conflicts".into());
                false
            }
        }
    }

    /// Replace the conflict under the cursor with one side (or both).
    pub fn resolve_conflict(&mut self, choice: Resolution) -> bool {
        if !self.writable() {
            return false;
        }
        let line = self.cursor.line;
        let Some(c) = self
            .conflicts()
            .into_iter()
            .find(|c| c.start <= line && line <= c.end)
        else {
            self.message = Some("Cursor is not inside a merge conflict".into());
            return false;
        };
        let lines = |a: usize, b: usize| -> String {
            (a..b)
                .map(|i| format!("{}\n", self.buffer.line_text(i)))
                .collect()
        };
        let ours = lines(c.start + 1, c.base.unwrap_or(c.mid));
        let theirs = lines(c.mid + 1, c.end);
        let text = match choice {
            Resolution::Ours => ours,
            Resolution::Theirs => theirs,
            Resolution::Both => ours + &theirs,
        };
        let from = self.buffer.line_to_char(c.start);
        let to = if c.end < self.last_line() {
            self.buffer.line_to_char(c.end + 1)
        } else {
            self.buffer.len_chars()
        };
        // The last line of the buffer has no newline to keep.
        let text = if c.end >= self.last_line() {
            text.trim_end_matches('\n').to_owned()
        } else {
            text
        };
        self.replace(from, to, &text, false, false);
        self.cursor = self.cursor_at(from);
        self.want_col = 0;
        true
    }

    // ---- files & search --------------------------------------------------

    pub fn save(&mut self) {
        if self.buffer.path().is_none() {
            self.request = Some(Request::SaveAs);
            return;
        }
        match self.buffer.save() {
            Ok(()) => {
                let name = self
                    .buffer
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.message = Some(format!("Saved {name}"));
            }
            Err(e) => self.message = Some(format!("Save failed: {e}")),
        }
        self.history.close();
    }

    /// Find `query` (literal, case-sensitive) after/before the cursor with
    /// wrap-around, and select it. Matches do not span lines.
    pub fn find(&mut self, query: &str, forward: bool) -> bool {
        if query.is_empty() {
            return false;
        }
        self.last_search = query.to_owned();
        let n = self.buffer.len_lines();
        let (sel_start, sel_end) = self
            .selection()
            .unwrap_or((self.cursor_idx(), self.cursor_idx()));
        let from = if forward { sel_end } else { sel_start };
        let from_c = self.cursor_at(from);
        let qlen = query.chars().count();
        for step in 0..=n {
            let line = if forward {
                (from_c.line + step) % n
            } else {
                (from_c.line + n - step % n) % n
            };
            let text = self.buffer.line_text(line);
            let chars: Vec<(usize, char)> = text.char_indices().collect();
            let char_col = |byte: usize| {
                chars
                    .iter()
                    .position(|(b, _)| *b == byte)
                    .unwrap_or(chars.len())
            };
            let hit = if forward {
                let skip = if step == 0 { from_c.col } else { 0 };
                let start_byte = chars.get(skip).map_or(text.len(), |(b, _)| *b);
                text[start_byte..]
                    .find(query)
                    .map(|b| char_col(b + start_byte))
            } else {
                let limit = if step == 0 { from_c.col } else { usize::MAX };
                let end_byte = chars.get(limit).map_or(text.len(), |(b, _)| *b);
                text[..end_byte]
                    .rmatch_indices(query)
                    .map(|(b, _)| char_col(b))
                    .find(|c| c + qlen <= limit)
            };
            if let Some(col) = hit {
                let start = self.buffer.line_to_char(line) + col;
                self.history.close();
                self.anchor = Some(self.cursor_at(start));
                self.set_cursor_idx(start + qlen);
                return true;
            }
        }
        self.message = Some(format!("Not found: {query}"));
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        Editor::new(Buffer::from_text(text))
    }

    fn at(e: &mut Editor, line: usize, col: usize) {
        e.goto(Some(line + 1), Some(col + 1));
    }

    #[test]
    fn vertical_move_remembers_column() {
        let mut e = ed("long line\nx\nlong line");
        e.goto(Some(1), Some(6));
        e.move_vertical(1, false);
        assert_eq!(e.cursor, Cursor { line: 1, col: 1 });
        e.move_vertical(1, false);
        assert_eq!(e.cursor, Cursor { line: 2, col: 5 });
    }

    #[test]
    fn left_right_wrap_lines() {
        let mut e = ed("ab\ncd");
        e.goto(Some(1), Some(3));
        e.move_right(false);
        assert_eq!(e.cursor, Cursor { line: 1, col: 0 });
        e.move_left(false);
        assert_eq!(e.cursor, Cursor { line: 0, col: 2 });
    }

    #[test]
    fn scroll_follows_cursor() {
        let mut e = ed(&"x\n".repeat(50));
        e.set_viewport_height(10);
        e.move_vertical(20, false);
        assert_eq!(e.scroll, 11);
        e.move_vertical(-20, false);
        assert_eq!(e.scroll, 0);
    }

    #[test]
    fn goto_clamps() {
        let mut e = ed("ab\ncd");
        e.goto(Some(99), Some(99));
        assert_eq!(e.cursor, Cursor { line: 1, col: 2 });
    }

    #[test]
    fn typing_backspace_and_undo_redo() {
        let mut e = ed("");
        for c in "hello".chars() {
            e.insert_char(c);
        }
        assert_eq!(e.buffer.text(), "hello");
        e.backspace();
        assert_eq!(e.buffer.text(), "hell");
        e.undo(); // undo the backspace
        assert_eq!(e.buffer.text(), "hello");
        e.undo(); // typing run is one step
        assert_eq!(e.buffer.text(), "");
        e.redo();
        assert_eq!(e.buffer.text(), "hello");
        assert_eq!(e.cursor, Cursor { line: 0, col: 5 });
        assert!(e.buffer.is_dirty());
    }

    #[test]
    fn selection_replace_and_delete() {
        let mut e = ed("hello world");
        e.goto(Some(1), Some(1));
        e.move_word_right(true);
        assert_eq!(e.selected_text().as_deref(), Some("hello"));
        e.insert_char('X');
        assert_eq!(e.buffer.text(), "X world");
        e.select_all();
        e.backspace();
        assert_eq!(e.buffer.text(), "");
        e.undo();
        assert_eq!(e.buffer.text(), "X world");
    }

    #[test]
    fn newline_keeps_indent() {
        let mut e = ed("\tfoo");
        e.move_file_end(false);
        e.insert_newline();
        assert_eq!(e.buffer.text(), "\tfoo\n\t");
    }

    #[test]
    fn indent_and_outdent_selection() {
        let mut e = ed("a\nb\nc");
        e.select_all();
        e.indent_lines(true);
        assert_eq!(e.buffer.text(), "\ta\n\tb\n\tc");
        e.indent_lines(false);
        assert_eq!(e.buffer.text(), "a\nb\nc");
        e.undo();
        assert_eq!(e.buffer.text(), "\ta\n\tb\n\tc");
        e.undo();
        assert_eq!(e.buffer.text(), "a\nb\nc");
    }

    #[test]
    fn move_lines_up_and_down() {
        let mut e = ed("a\nb\nc\n");
        at(&mut e, 1, 0);
        e.move_lines(true);
        assert_eq!(e.buffer.text(), "b\na\nc\n");
        assert_eq!(e.cursor.line, 0);
        e.move_lines(false);
        e.move_lines(false);
        assert_eq!(e.buffer.text(), "a\nc\nb\n");
        assert_eq!(e.cursor.line, 2);
    }

    #[test]
    fn duplicate_and_cut_line() {
        let mut e = ed("a\nb");
        e.duplicate_line();
        assert_eq!(e.buffer.text(), "a\na\nb");
        at(&mut e, 2, 0);
        e.cut_line();
        assert_eq!(e.buffer.text(), "a\na");
        assert_eq!(e.clipboard, "b\n");
        e.move_file_start(false);
        e.paste();
        assert_eq!(e.buffer.text(), "b\na\na");
    }

    #[test]
    fn cut_copy_paste_selection() {
        let mut e = ed("abc def");
        e.move_word_right(true);
        assert!(e.cut());
        assert_eq!(e.buffer.text(), " def");
        assert!(e.clipboard_changed);
        e.move_file_end(false);
        e.paste();
        assert_eq!(e.buffer.text(), " defabc");
    }

    #[test]
    fn word_motions() {
        let mut e = ed("foo.bar baz");
        e.move_word_right(false);
        assert_eq!(e.cursor.col, 3);
        e.move_word_right(false);
        assert_eq!(e.cursor.col, 4);
        e.move_word_right(false);
        assert_eq!(e.cursor.col, 7);
        e.move_word_left(false);
        assert_eq!(e.cursor.col, 4);
    }

    #[test]
    fn find_wraps_and_selects() {
        let mut e = ed("one two\nthree two");
        assert!(e.find("two", true));
        assert_eq!(e.selected_text().as_deref(), Some("two"));
        assert_eq!(e.cursor, Cursor { line: 0, col: 7 });
        assert!(e.find("two", true));
        assert_eq!(e.cursor, Cursor { line: 1, col: 9 });
        assert!(e.find("two", true)); // wraps
        assert_eq!(e.cursor, Cursor { line: 0, col: 7 });
        assert!(e.find("two", false));
        assert_eq!(e.cursor, Cursor { line: 1, col: 9 });
        assert!(!e.find("zzz", true));
    }

    const MERGED: &str = "a\n<<<<<<< HEAD\nours1\nours2\n=======\ntheirs1\n>>>>>>> branch\nz\n<<<<<<< HEAD\nx\n||||||| base\nb\n=======\ny\n>>>>>>> branch\n";

    #[test]
    fn finds_conflicts_including_diff3() {
        let e = ed(MERGED);
        let c = e.conflicts();
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].start, c[0].mid, c[0].end, c[0].base), (1, 4, 6, None));
        assert_eq!(
            (c[1].start, c[1].base, c[1].mid, c[1].end),
            (8, Some(10), 12, 14)
        );
    }

    #[test]
    fn conflict_navigation_wraps() {
        let mut e = ed(MERGED);
        assert!(e.goto_conflict(true));
        assert_eq!(e.cursor.line, 1);
        e.goto_conflict(true);
        assert_eq!(e.cursor.line, 8);
        e.goto_conflict(true); // wraps
        assert_eq!(e.cursor.line, 1);
        e.goto_conflict(false); // wraps backwards
        assert_eq!(e.cursor.line, 8);
        assert!(!ed("clean").goto_conflict(true));
    }

    #[test]
    fn resolve_ours_theirs_both_with_undo() {
        let mut e = ed(MERGED);
        e.goto_conflict(true);
        assert!(e.resolve_conflict(Resolution::Ours));
        assert!(e.buffer.text().starts_with("a\nours1\nours2\nz\n<<<<<<<"));
        e.goto_conflict(true);
        assert!(e.resolve_conflict(Resolution::Theirs));
        assert_eq!(e.buffer.text(), "a\nours1\nours2\nz\ny\n");
        e.undo();
        e.undo();
        assert_eq!(e.buffer.text(), MERGED);
        e.move_file_start(false);
        e.goto_conflict(true);
        e.resolve_conflict(Resolution::Both);
        assert!(e.buffer.text().starts_with("a\nours1\nours2\ntheirs1\nz\n"));
        assert!(!ed("plain").resolve_conflict(Resolution::Ours));
    }

    #[test]
    fn read_only_blocks_edits() {
        let b = Buffer::open(&std::env::temp_dir()).unwrap(); // directory listing
        assert!(b.is_read_only());
        let before = b.text();
        let mut e = Editor::new(b);
        e.insert_char('a');
        assert_eq!(e.buffer.text(), before);
        assert!(e.message.is_some());
    }
}
