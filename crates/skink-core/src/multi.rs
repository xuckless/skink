//! Multiple cursors. The primary cursor stays in `Editor::cursor` /
//! `Editor::anchor`; extra cursors live in `Editor::secondary` as char indices.
//!
//! Every multi-cursor operation runs the ordinary single-cursor code once per
//! cursor ([`Editor::each_cursor`]): each cursor is loaded as the primary, the
//! operation runs, and the cursors positioned after it shift by the change in
//! length. All edits of one run form a single undo step.
//!
//! Action names and default keys follow Micro's multi-cursor actions
//! (zyedidia/micro internal/action/actions.go, MIT): SpawnMultiCursor,
//! SpawnMultiCursorUp/Down, RemoveMultiCursor, RemoveAllMultiCursors,
//! SkipMultiCursor. Behaviour is reimplemented over ropey char indices.

use crate::Editor;

/// One cursor of the set, as char indices (they shift with edits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sel {
    /// Other end of the selection, if any.
    pub anchor: Option<usize>,
    pub head: usize,
    pub(crate) want_col: usize,
}

impl Sel {
    pub fn start(&self) -> usize {
        self.anchor.map_or(self.head, |a| a.min(self.head))
    }

    pub fn end(&self) -> usize {
        self.anchor.map_or(self.head, |a| a.max(self.head))
    }

    /// Selected range, if non-empty.
    pub fn range(&self) -> Option<(usize, usize)> {
        (self.start() != self.end()).then(|| (self.start(), self.end()))
    }

    fn shifted(self, delta: isize, max: usize) -> Sel {
        let f = |p: usize| p.saturating_add_signed(delta).min(max);
        Sel {
            anchor: self.anchor.map(f),
            head: f(self.head),
            want_col: self.want_col,
        }
    }
}

/// How an operation visits the cursors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Once per cursor (typing, deleting, moving).
    Cursors,
    /// Once per distinct line span (indent, move lines, duplicate line), in
    /// the given order: moving lines up must run top-down, down bottom-up.
    Lines(Order),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    TopDown,
    BottomUp,
}

/// Undo grouping while one operation runs at every cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Batch {
    /// Ordinary single-cursor editing.
    Single,
    /// Inside [`Editor::each_cursor`]; `recorded` once the run's undo group exists.
    Multi { recorded: bool },
}

impl Editor {
    /// Number of cursors (1 when there is no multi-cursor).
    pub fn cursor_count(&self) -> usize {
        1 + self.secondary.len()
    }

    /// The extra cursors, sorted by position.
    pub fn secondary(&self) -> &[Sel] {
        &self.secondary
    }

    fn primary_sel(&self) -> Sel {
        Sel {
            anchor: self.anchor.map(|a| self.idx(a)),
            head: self.cursor_idx(),
            want_col: self.want_col,
        }
    }

    fn load(&mut self, s: Sel) {
        self.anchor = s.anchor.map(|a| self.cursor_at(a));
        self.cursor = self.cursor_at(s.head);
        self.want_col = s.want_col;
    }

    /// Run `f` at every cursor as one undo step. `f` gets the cursor's rank
    /// (0 = topmost). With a single cursor this is just `f(self, 0)`.
    pub fn each_cursor(&mut self, scope: Scope, mut f: impl FnMut(&mut Editor, usize)) {
        if self.secondary.is_empty() {
            return f(self, 0);
        }
        let primary = self.primary_sel();
        let mut sels: Vec<(Sel, bool)> = self.secondary.drain(..).map(|s| (s, false)).collect();
        sels.push((primary, true));
        sels.sort_by_key(|(s, _)| (s.start(), s.end()));
        let n = sels.len();
        // Line-scoped operations run once per line span.
        let runs: Vec<bool> = match scope {
            Scope::Cursors => vec![true; n],
            Scope::Lines(_) => {
                let mut last_line = None;
                sels.iter()
                    .map(|(s, _)| {
                        let (a, b) = (
                            self.buffer.char_to_line(s.start()),
                            self.buffer.char_to_line(s.end()),
                        );
                        let fresh = last_line.is_none_or(|l| a > l);
                        last_line = Some(last_line.map_or(b, |l: usize| l.max(b)));
                        fresh
                    })
                    .collect()
            }
        };
        let order: Vec<usize> = match scope {
            Scope::Lines(Order::TopDown) => (0..n).collect(),
            Scope::Cursors | Scope::Lines(Order::BottomUp) => (0..n).rev().collect(),
        };
        let primary_before = primary.head;
        self.history.close();
        self.batch = Batch::Multi { recorded: false };
        for i in order {
            if !runs[i] {
                continue;
            }
            self.load(sels[i].0);
            let len = self.buffer.len_chars();
            f(self, i);
            sels[i].0 = self.primary_sel();
            let delta = self.buffer.len_chars() as isize - len as isize;
            if delta != 0 {
                let max = self.buffer.len_chars();
                // Cursors after this one move with the text.
                for (s, _) in &mut sels[i + 1..] {
                    *s = s.shifted(delta, max);
                }
            }
        }
        let recorded = self.batch == Batch::Multi { recorded: true };
        self.batch = Batch::Single;
        let p = sels.iter().position(|(_, p)| *p).unwrap_or(0);
        if recorded {
            self.history.retarget_last(primary_before, sels[p].0.head);
            self.history.close();
        }
        self.set_cursors(sels);
    }

    /// Install a cursor set (one entry flagged primary), merging overlaps.
    fn set_cursors(&mut self, mut sels: Vec<(Sel, bool)>) {
        sels.sort_by_key(|(s, _)| (s.start(), s.end()));
        let mut merged: Vec<(Sel, bool)> = Vec::with_capacity(sels.len());
        for (s, p) in sels {
            match merged.last_mut() {
                Some((m, mp))
                    if s.start() < m.end() || s.start() == m.start() || s.head == m.head =>
                {
                    let (lo, hi) = (m.start().min(s.start()), m.end().max(s.end()));
                    // The merged selection keeps the primary's direction.
                    let src = if p { s } else { *m };
                    let forward = src.anchor.is_none_or(|a| a <= src.head);
                    *m = Sel {
                        anchor: (lo != hi).then_some(if forward { lo } else { hi }),
                        head: if forward { hi } else { lo },
                        want_col: if p { s.want_col } else { m.want_col },
                    };
                    *mp |= p;
                }
                _ => merged.push((s, p)),
            }
        }
        let p = merged.iter().position(|(_, p)| *p).unwrap_or(0);
        let (primary, _) = merged.remove(p);
        self.secondary = merged.into_iter().map(|(s, _)| s).collect();
        self.load(primary);
        self.scroll_into_view();
    }

    /// Make `new` the primary cursor, keeping the current one as an extra.
    fn spawn(&mut self, new: Sel) {
        let mut all: Vec<(Sel, bool)> = self.secondary.drain(..).map(|s| (s, false)).collect();
        all.push((self.primary_sel(), false));
        all.push((new, true));
        self.history.close();
        self.set_cursors(all);
    }

    /// Word under (or just before) the cursor as a char range.
    fn word_at_cursor(&self) -> Option<(usize, usize)> {
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let i = self.cursor_idx();
        let at = |j: usize| self.buffer.char_at(j).is_some_and(is_word);
        let i = if at(i) {
            i
        } else if i > 0 && at(i - 1) {
            i - 1
        } else {
            return None;
        };
        let mut s = i;
        while s > 0 && at(s - 1) {
            s -= 1;
        }
        let mut e = i;
        while at(e) {
            e += 1;
        }
        Some((s, e))
    }

    /// Next occurrence of the primary selection's text after it (wrapping),
    /// skipping occurrences that are already cursors.
    fn next_match(&self) -> Option<(usize, usize)> {
        let (s, e) = self.selection()?;
        let needle = self.buffer.slice(s, e);
        let text = self.buffer.text();
        let len = e - s;
        let taken = |a: usize| {
            a == s
                || self
                    .secondary
                    .iter()
                    .any(|x| x.range() == Some((a, a + len)))
        };
        let from = self.buffer.char_to_byte(e);
        text[from..]
            .match_indices(&needle)
            .map(|(b, _)| from + b)
            .chain(text[..from].match_indices(&needle).map(|(b, _)| b))
            .map(|b| self.buffer.byte_to_char(b))
            .find(|&a| !taken(a))
            .map(|a| (a, a + len))
    }

    /// Ctrl-D: select the word at the cursor, or add a cursor at the next
    /// occurrence of the selection (Micro's SpawnMultiCursor).
    pub fn spawn_cursor_next_match(&mut self) -> bool {
        if self.selection().is_none() {
            let Some((s, e)) = self.word_at_cursor() else {
                return false;
            };
            self.history.close();
            self.anchor = Some(self.cursor_at(s));
            self.cursor = self.cursor_at(e);
            self.want_col = self.cursor.col;
            return true;
        }
        match self.next_match() {
            Some((a, b)) => {
                self.spawn(Sel {
                    anchor: Some(a),
                    head: b,
                    want_col: self.cursor_at(b).col,
                });
                true
            }
            None => {
                self.message = Some("No more matches".into());
                false
            }
        }
    }

    /// Drop the primary selection and move on to the next match (Micro's
    /// SkipMultiCursor).
    pub fn skip_cursor_match(&mut self) -> bool {
        let Some((a, b)) = self.next_match() else {
            return false;
        };
        let mut all: Vec<(Sel, bool)> = self.secondary.drain(..).map(|s| (s, false)).collect();
        all.push((
            Sel {
                anchor: Some(a),
                head: b,
                want_col: self.cursor_at(b).col,
            },
            true,
        ));
        self.history.close();
        self.set_cursors(all);
        true
    }

    /// Add a cursor on the line above the topmost (or below the bottommost)
    /// cursor, at the primary's goal column.
    pub fn spawn_cursor_vertical(&mut self, delta: isize) -> bool {
        let lines = self
            .secondary
            .iter()
            .map(|s| self.buffer.char_to_line(s.head))
            .chain([self.cursor.line]);
        let edge = if delta < 0 { lines.min() } else { lines.max() };
        let Some(line) = edge
            .and_then(|l| l.checked_add_signed(delta))
            .filter(|&l| l < self.buffer.len_lines())
        else {
            return false;
        };
        let col = self.want_col.min(self.buffer.line_len(line));
        let head = self.buffer.line_to_char(line) + col;
        let want_col = self.want_col;
        self.spawn(Sel {
            anchor: None,
            head,
            want_col,
        });
        true
    }

    /// Alt-click: add a cursor, or remove the one already there.
    pub fn toggle_cursor_at(&mut self, line: usize, col: usize) {
        let line = line.min(self.buffer.len_lines() - 1);
        let col = col.min(self.buffer.line_len(line));
        let head = self.buffer.line_to_char(line) + col;
        if let Some(i) = self.secondary.iter().position(|s| s.head == head) {
            self.secondary.remove(i);
            return;
        }
        if head == self.cursor_idx() {
            self.remove_primary_cursor();
            return;
        }
        self.spawn(Sel {
            anchor: None,
            head,
            want_col: col,
        });
    }

    /// Remove the primary cursor; the nearest one before it becomes primary
    /// (Micro's RemoveMultiCursor). False when there is only one cursor.
    pub fn remove_primary_cursor(&mut self) -> bool {
        if self.secondary.is_empty() {
            return false;
        }
        let at = self.cursor_idx();
        let i = self
            .secondary
            .iter()
            .rposition(|s| s.head < at)
            .unwrap_or(self.secondary.len() - 1);
        let s = self.secondary.remove(i);
        self.load(s);
        self.scroll_into_view();
        true
    }

    /// Back to one cursor, the primary. False when there was only one.
    pub fn remove_all_cursors(&mut self) -> bool {
        let had = !self.secondary.is_empty();
        self.secondary.clear();
        had
    }

    /// Copy every cursor's selection, one per line (top to bottom).
    pub(crate) fn copy_all(&mut self) -> bool {
        let mut all: Vec<Sel> = self.secondary.clone();
        all.push(self.primary_sel());
        all.sort_by_key(Sel::start);
        if all.iter().all(|s| s.range().is_none()) {
            return false;
        }
        let parts: Vec<String> = all
            .iter()
            .map(|s| {
                s.range()
                    .map_or_else(String::new, |(a, b)| self.buffer.slice(a, b))
            })
            .collect();
        self.clipboard = parts.join("\n");
        self.clipboard_changed = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::{Buffer, CommandRegistry, Editor};

    fn ed(text: &str) -> Editor {
        Editor::new(Buffer::from_text(text))
    }

    #[test]
    fn ctrl_d_selects_word_then_adds_matches_and_typing_edits_all_as_one_undo() {
        let r = CommandRegistry::builtin();
        let mut e = ed("foo bar foo\nfoo");
        r.run("SpawnMultiCursor", &mut e); // selects "foo"
        assert_eq!(e.selected_text().as_deref(), Some("foo"));
        r.run("SpawnMultiCursor", &mut e);
        r.run("SpawnMultiCursor", &mut e);
        assert_eq!(e.cursor_count(), 3);
        assert!(!r.run("SpawnMultiCursor", &mut e), "all taken");
        e.insert_text("x");
        e.insert_text("y");
        assert_eq!(e.buffer.text(), "xy bar xy\nxy");
        e.undo();
        e.undo();
        assert_eq!(e.buffer.text(), "foo bar foo\nfoo");
        assert_eq!(e.cursor_count(), 1);
    }

    #[test]
    fn edits_apply_bottom_up_and_backspace_merges_touching_cursors() {
        let r = CommandRegistry::builtin();
        let mut e = ed("ab\ncd\nef");
        e.goto(Some(1), Some(3));
        r.run("SpawnMultiCursorDown", &mut e);
        r.run("SpawnMultiCursorDown", &mut e);
        assert_eq!(e.cursor_count(), 3);
        r.run("InsertNewline", &mut e);
        assert_eq!(e.buffer.text(), "ab\n\ncd\n\nef\n");
        r.run("Backspace", &mut e);
        assert_eq!(e.buffer.text(), "ab\ncd\nef");
        // Deleting everything collapses all cursors into one.
        for _ in 0..3 {
            r.run("Backspace", &mut e);
        }
        r.run("Backspace", &mut e);
        assert_eq!(e.cursor_count(), 1);
    }

    #[test]
    fn copy_and_paste_distribute_one_line_per_cursor() {
        let r = CommandRegistry::builtin();
        let mut e = ed("a1\nb2\nc3");
        r.run("SpawnMultiCursorDown", &mut e);
        r.run("SpawnMultiCursorDown", &mut e);
        r.run("SelectToEndOfLine", &mut e);
        assert!(r.run("Copy", &mut e));
        assert_eq!(e.clipboard, "a1\nb2\nc3");
        r.run("CursorEnd", &mut e); // collapses: all go to the end
        r.run("Escape,RemoveAllMultiCursors|Deselect", &mut e);
        assert_eq!(e.cursor_count(), 1);
        let mut e2 = ed("x\ny\nz");
        r.run("SpawnMultiCursorDown", &mut e2);
        r.run("SpawnMultiCursorDown", &mut e2);
        e2.clipboard = e.clipboard.clone();
        r.run("Paste", &mut e2);
        assert_eq!(e2.buffer.text(), "a1x\nb2y\nc3z");
    }

    #[test]
    fn move_lines_with_cursors_on_adjacent_lines_moves_the_block() {
        let r = CommandRegistry::builtin();
        let mut e = ed("1\n2\n3\n4");
        e.goto(Some(2), Some(1));
        r.run("SpawnMultiCursorDown", &mut e);
        r.run("MoveLinesUp", &mut e);
        assert_eq!(e.buffer.text(), "2\n3\n1\n4");
        r.run("MoveLinesDown", &mut e);
        r.run("MoveLinesDown", &mut e);
        assert_eq!(e.buffer.text(), "1\n4\n2\n3");
        // Two cursors on one line outdent it once.
        let mut e = ed("\t\tab");
        e.toggle_cursor_at(0, 3);
        assert_eq!(e.cursor_count(), 2);
        r.run("OutdentLine", &mut e);
        assert_eq!(e.buffer.text(), "\tab");
    }
}
