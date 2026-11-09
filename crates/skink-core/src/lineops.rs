//! Line and case operations: join, sort, upper/lower/title case, trim
//! trailing whitespace. Each is one undo step and works on the selection's
//! lines (or the cursor line); the command table runs them per cursor.

use crate::{Cursor, Editor};

/// Target of a case conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextCase {
    Upper,
    Lower,
    /// First letter of each word upper, the rest lower.
    Title,
}

impl TextCase {
    pub fn apply(self, s: &str) -> String {
        match self {
            TextCase::Upper => s.to_uppercase(),
            TextCase::Lower => s.to_lowercase(),
            TextCase::Title => {
                let mut out = String::with_capacity(s.len());
                let mut at_word_start = true;
                for c in s.chars() {
                    if c.is_alphanumeric() {
                        if at_word_start {
                            out.extend(c.to_uppercase());
                        } else {
                            out.extend(c.to_lowercase());
                        }
                        at_word_start = false;
                    } else {
                        out.push(c);
                        at_word_start = true;
                    }
                }
                out
            }
        }
    }
}

impl Editor {
    /// Char range of whole lines `first..=last` (without the last newline).
    fn lines_range(&self, first: usize, last: usize) -> (usize, usize) {
        let start = self.buffer.line_to_char(first);
        let end = self.buffer.line_to_char(last) + self.buffer.line_len(last);
        (start, end)
    }

    /// Replace lines `first..=last` with `text` and select the result.
    fn replace_lines(&mut self, first: usize, last: usize, text: &str) {
        let (start, end) = self.lines_range(first, last);
        if self.buffer.slice(start, end) == text {
            return;
        }
        self.replace(start, end, text, false, false);
        self.anchor = Some(self.cursor_at(start));
        self.cursor = self.cursor_at(start + text.chars().count());
        self.want_col = self.cursor.col;
    }

    /// Join the selected lines (or the cursor line and the next) with single
    /// spaces, dropping the joined lines' indentation.
    pub fn join_lines(&mut self) {
        if !self.writable() {
            return;
        }
        let (first, last) = self.line_span();
        let last = if first == last { last + 1 } else { last };
        if last >= self.buffer.len_lines() {
            return;
        }
        let mut joined = self.buffer.line_text(first).trim_end().to_owned();
        for l in first + 1..=last {
            let t = self.buffer.line_text(l);
            let t = t.trim();
            if !t.is_empty() {
                if !joined.is_empty() {
                    joined.push(' ');
                }
                joined.push_str(t);
            }
        }
        let col = self.buffer.line_text(first).trim_end().chars().count();
        self.replace_lines(first, last, &joined);
        self.anchor = None;
        self.cursor = Cursor { line: first, col };
        self.want_col = col;
    }

    /// Sort the selected lines (needs at least two).
    pub fn sort_lines(&mut self) -> bool {
        let (first, last) = self.line_span();
        if first == last || !self.writable() {
            return false;
        }
        let mut lines: Vec<String> = (first..=last).map(|l| self.buffer.line_text(l)).collect();
        lines.sort();
        self.replace_lines(first, last, &lines.join("\n"));
        true
    }

    /// Convert the selection (or the word at the cursor) to `case`.
    pub fn change_case(&mut self, case: TextCase) -> bool {
        if !self.writable() {
            return false;
        }
        let (start, end) = match self.selection() {
            Some(r) => r,
            None => {
                let at = self.cursor_idx();
                (self.word_left_bound(at), self.word_right_bound(at))
            }
        };
        if start == end {
            return false;
        }
        let (anchor, cursor) = (self.anchor, self.cursor);
        let text = case.apply(&self.buffer.slice(start, end));
        if text.chars().count() != end - start {
            // Some letters change length (ß -> SS): select the new text.
            self.replace(start, end, &text, false, false);
            self.anchor = Some(self.cursor_at(start));
            return true;
        }
        self.replace(start, end, &text, false, false);
        self.anchor = anchor;
        self.cursor = cursor;
        true
    }

    /// Remove trailing spaces and tabs on every line (one undo step).
    pub fn trim_trailing_whitespace(&mut self) -> usize {
        if !self.writable() {
            return 0;
        }
        let cursor = self.cursor;
        let mut n = 0;
        for line in (0..self.buffer.len_lines()).rev() {
            let t = self.buffer.line_text(line);
            let keep = t.trim_end_matches([' ', '\t']).chars().count();
            let len = t.chars().count();
            if keep < len {
                let start = self.buffer.line_to_char(line);
                self.replace(start + keep, start + len, "", false, n > 0);
                n += 1;
            }
        }
        self.anchor = None;
        self.cursor = Cursor {
            line: cursor.line,
            col: cursor.col.min(self.buffer.line_len(cursor.line)),
        };
        self.want_col = self.cursor.col;
        n
    }

    fn word_left_bound(&self, mut i: usize) -> usize {
        while i > 0 && self.buffer.char_at(i - 1).is_some_and(is_word) {
            i -= 1;
        }
        i
    }

    fn word_right_bound(&self, mut i: usize) -> usize {
        while self.buffer.char_at(i).is_some_and(is_word) {
            i += 1;
        }
        i
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Buffer;

    #[test]
    fn join_sort_case_trim() {
        let mut e = Editor::new(Buffer::from_text("a  \n    b\nc\n"));
        e.join_lines();
        assert_eq!(e.buffer.text(), "a b\nc\n");
        assert_eq!((e.cursor.line, e.cursor.col), (0, 1));

        let mut e = Editor::new(Buffer::from_text("pear\napple\nfig\n"));
        e.select_all();
        assert!(e.sort_lines());
        assert_eq!(e.buffer.text(), "apple\nfig\npear\n");
        e.undo();
        assert_eq!(e.buffer.text(), "pear\napple\nfig\n", "one undo step");

        let mut e = Editor::new(Buffer::from_text("hello wORLD-x"));
        e.goto(Some(1), Some(3));
        e.change_case(TextCase::Upper); // word at the cursor
        assert_eq!(e.buffer.text(), "HELLO wORLD-x");
        e.select_all();
        e.change_case(TextCase::Title);
        assert_eq!(e.buffer.text(), "Hello World-X");

        let mut e = Editor::new(Buffer::from_text("a \t\nb\nc  "));
        assert_eq!(e.trim_trailing_whitespace(), 2);
        assert_eq!(e.buffer.text(), "a\nb\nc");
        e.undo();
        assert_eq!(e.buffer.text(), "a \t\nb\nc  ", "one undo step");
    }
}
