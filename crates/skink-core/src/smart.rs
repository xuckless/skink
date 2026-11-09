//! Typing helpers: indentation style, auto-pairs, smart newline / closer
//! outdent, and line-comment toggling.
//!
//! Behaviour follows what Micro's autoclose and comment plugins
//! (zyedidia/micro runtime/plugins, MIT) and most editors do; reimplemented
//! over the rope-based `Editor`.

use crate::{Buffer, Cursor, Editor};

/// What one level of indentation is in this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indent {
    Tabs,
    /// `tab_width` spaces.
    Spaces,
}

impl Indent {
    /// Guess from the file's indented lines (first 1000); `None` when there
    /// are none, so the caller's default applies.
    pub fn detect(b: &Buffer) -> Option<Indent> {
        let (mut tabs, mut spaces) = (0usize, 0usize);
        for i in 0..b.len_lines().min(1000) {
            if b.line_starts_with(i, "\t") {
                tabs += 1;
            } else if b.line_starts_with(i, "  ") {
                spaces += 1;
            }
        }
        match (tabs, spaces) {
            (0, 0) => None,
            (t, s) if s > t => Some(Indent::Spaces),
            _ => Some(Indent::Tabs),
        }
    }
}

const PAIRS: &[(char, char)] = &[
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('"', '"'),
    ('\'', '\''),
    ('`', '`'),
];

fn closer_of(open: char) -> Option<char> {
    PAIRS.iter().find(|p| p.0 == open).map(|p| p.1)
}

fn is_closer(c: char) -> bool {
    PAIRS.iter().any(|p| p.1 == c)
}

fn is_quote(c: char) -> bool {
    matches!(c, '"' | '\'' | '`')
}

impl Editor {
    /// One level of indentation as text.
    pub fn indent_unit(&self) -> String {
        match self.indent {
            Indent::Tabs => "\t".into(),
            Indent::Spaces => " ".repeat(self.tab_width),
        }
    }

    fn char_before_cursor(&self) -> Option<char> {
        self.cursor_idx()
            .checked_sub(1)
            .and_then(|i| self.buffer.char_at(i))
    }

    /// Cursor between an auto-pair like `(|)`: Backspace deletes both.
    pub(crate) fn empty_pair_at_cursor(&self) -> bool {
        self.auto_pairs
            && self.selection().is_none()
            && self
                .char_before_cursor()
                .and_then(closer_of)
                .is_some_and(|c| self.buffer.char_at(self.cursor_idx()) == Some(c))
    }

    /// One typed character at the primary cursor.
    pub(crate) fn type_char(&mut self, c: char) {
        let mut b = [0; 4];
        let s: &str = c.encode_utf8(&mut b);
        if !self.auto_pairs || !self.writable() {
            return self.insert_one(s);
        }
        let at = self.cursor_idx();
        let next = self.buffer.char_at(at);
        let prev = self.char_before_cursor();
        if let Some((start, end)) = self.selection() {
            // Wrap the selection, keeping it selected.
            if let Some(close) = closer_of(c) {
                let inner = self.buffer.slice(start, end);
                self.replace(start, end, &format!("{c}{inner}{close}"), false, false);
                self.anchor = Some(self.cursor_at(start + 1));
                self.set_cursor_idx(end + 1);
                return;
            }
            return self.insert_one(s);
        }
        // Typing the closer that is already next: step over it.
        if is_closer(c) && next == Some(c) {
            self.history.close();
            self.set_cursor_idx(at + 1);
            return;
        }
        // A closer typed on a blank line goes back one indent level.
        if is_closer(c) && !is_quote(c) {
            let line_start = self.buffer.line_to_char(self.cursor.line);
            let before = self.buffer.slice(line_start, at);
            if !before.is_empty() && before.chars().all(|x| x == ' ' || x == '\t') {
                let unit = self.indent_unit();
                let cut = if before.ends_with(&unit) {
                    unit.chars().count()
                } else {
                    1
                };
                self.replace(at - cut, at, s, false, false);
                return;
            }
        }
        if let Some(close) = closer_of(c) {
            let next_ok = next.is_none_or(|n| n.is_whitespace() || ")]},;:".contains(n));
            // Don't pair quotes inside words (don't, it's) or right after one.
            let prev_ok = !is_quote(c) || prev.is_none_or(|p| !p.is_alphanumeric() && p != c);
            if next_ok && prev_ok {
                self.replace(at, at, &format!("{c}{close}"), false, false);
                self.set_cursor_idx(at + 1);
                return;
            }
        }
        self.insert_one(s);
    }

    /// Enter: keep the indentation, add a level after an opening bracket,
    /// and put a closer right after the cursor on its own line.
    pub fn insert_newline(&mut self) {
        self.each_cursor(crate::multi::Scope::Cursors, |e, _| e.newline_one());
    }

    fn newline_one(&mut self) {
        let indent: String = self
            .buffer
            .line_text(self.cursor.line)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .take(self.cursor.col)
            .collect();
        let opener = self.selection().is_none()
            && self
                .char_before_cursor()
                .is_some_and(|c| matches!(c, '(' | '[' | '{'));
        if !opener {
            return self.insert_one(&format!("\n{indent}"));
        }
        let unit = self.indent_unit();
        let at = self.cursor_idx();
        let closer_next = self
            .char_before_cursor()
            .and_then(closer_of)
            .is_some_and(|c| self.buffer.char_at(at) == Some(c));
        let inner = format!("\n{indent}{unit}");
        if closer_next {
            self.replace(at, at, &format!("{inner}\n{indent}"), false, false);
            self.set_cursor_idx(at + inner.chars().count());
        } else {
            self.insert_one(&inner);
        }
    }

    /// Comment or uncomment the selected lines (or the cursor line) with
    /// the file's line-comment token. False when the file type has none.
    pub fn toggle_comment(&mut self) -> bool {
        let Some(tok) = self.comment else {
            self.message = Some("No line comments for this file type".into());
            return false;
        };
        if !self.writable() {
            return false;
        }
        let (first, last) = self.line_span();
        let lines: Vec<String> = (first..=last).map(|l| self.buffer.line_text(l)).collect();
        let indent_of = |l: &str| l.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        let blank = |l: &str| l.trim().is_empty();
        let all_commented = lines
            .iter()
            .filter(|l| !blank(l))
            .all(|l| l.trim_start().starts_with(tok));
        if lines.iter().all(|l| blank(l)) {
            return false;
        }
        let col = lines
            .iter()
            .filter(|l| !blank(l))
            .map(|l| indent_of(l))
            .min()
            .unwrap_or(0);
        let (cursor, anchor) = (self.cursor, self.anchor); // replace() moves them
        // (line, column of the change, chars added (+) or removed (-)).
        let mut changes: Vec<(usize, usize, isize)> = Vec::new();
        let mut started = false;
        for (k, l) in lines.iter().enumerate().rev() {
            if blank(l) {
                continue;
            }
            let line = first + k;
            let start = self.buffer.line_to_char(line);
            if all_commented {
                let at = indent_of(l);
                let rest: String = l.chars().skip(at + tok.chars().count()).collect();
                let n = tok.chars().count() + usize::from(rest.starts_with(' '));
                self.replace(start + at, start + at + n, "", false, started);
                changes.push((line, at, -(n as isize)));
            } else {
                let text = format!("{tok} ");
                self.replace(start + col, start + col, &text, false, started);
                changes.push((line, col, text.chars().count() as isize));
            }
            started = true;
        }
        let shift = |c: Cursor| -> Cursor {
            match changes.iter().find(|ch| ch.0 == c.line) {
                Some(&(_, at, d)) if c.col >= at => Cursor {
                    line: c.line,
                    col: c.col.saturating_add_signed(d).max(at),
                },
                _ => c,
            }
        };
        let (cursor, anchor) = (shift(cursor), anchor.map(shift));
        self.cursor = cursor;
        self.anchor = anchor;
        self.want_col = self.cursor.col;
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::{Buffer, CommandRegistry, Editor, Indent};

    fn ed(text: &str) -> Editor {
        Editor::new(Buffer::from_text(text))
    }

    fn typed(e: &mut Editor, s: &str) {
        for c in s.chars() {
            match c {
                '\n' => e.insert_newline(),
                c => e.insert_char(c),
            }
        }
    }

    #[test]
    fn pairs_skip_over_and_smart_newline() {
        let mut e = ed("");
        e.indent = Indent::Spaces;
        typed(&mut e, "fn f() {\nx");
        assert_eq!(e.buffer.text(), "fn f() {\n    x\n}");
        // A closer typed on a blank indented line goes back one level.
        let mut e = ed("if x {\n        ");
        e.indent = Indent::Spaces;
        e.move_file_end(false);
        typed(&mut e, "}");
        assert_eq!(e.buffer.text(), "if x {\n    }");
        // Quotes do not pair inside words; backspace removes an empty pair.
        let mut e = ed("");
        typed(&mut e, "don't (");
        assert_eq!(e.buffer.text(), "don't ()");
        e.backspace();
        assert_eq!(e.buffer.text(), "don't ");
    }

    #[test]
    fn indent_style_is_detected_from_the_file() {
        assert_eq!(
            Indent::detect(&Buffer::from_text("a\n    b\n    c\n\td")),
            Some(Indent::Spaces)
        );
        assert_eq!(Indent::detect(&Buffer::from_text("a\nb")), None);
        let mut e = ed("x\n  y");
        e.tab_width = 2;
        e.insert_tab();
        assert_eq!(e.buffer.text(), "  x\n  y");
    }

    #[test]
    fn toggle_comment_round_trips_at_the_common_indent() {
        let r = CommandRegistry::builtin();
        let mut e = ed("    a\n\n      b");
        e.comment = Some("//");
        e.select_all();
        assert!(r.run("ToggleComment", &mut e));
        assert_eq!(e.buffer.text(), "    // a\n\n    //   b");
        assert!(r.run("ToggleComment", &mut e));
        assert_eq!(e.buffer.text(), "    a\n\n      b");
        e.undo();
        assert_eq!(e.buffer.text(), "    // a\n\n    //   b");
    }
}
