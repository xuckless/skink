//! Skink's coordinates vs plain text.
//!
//! The editor addresses text by *characters* (Unicode scalar values, like
//! `ropey`): either an absolute char index or a 0-based `(line, col)` where
//! `col` counts chars from the start of the line and never includes the line
//! terminator (`\n` or `\r\n`). Agents see plain `&str`/`String` (UTF-8 bytes).
//! [`TextIndex`] converts between the two, clamping like an editor would, and
//! never panics on out-of-range input.

/// 0-based line and char column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct CharPos {
    pub line: usize,
    pub col: usize,
}

impl CharPos {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// Half-open range `[start, end)` in char coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct CharRange {
    pub start: CharPos,
    pub end: CharPos,
}

impl CharRange {
    pub fn new(start: CharPos, end: CharPos) -> Self {
        Self { start, end }
    }
    /// Swap endpoints if the selection was made backwards.
    #[must_use]
    pub fn ordered(self) -> Self {
        if self.start <= self.end {
            self
        } else {
            Self {
                start: self.end,
                end: self.start,
            }
        }
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Line table over a borrowed text.
pub struct TextIndex<'a> {
    text: &'a str,
    /// Byte offset of the start of every line. Like ropey, text ending in
    /// `\n` has a final empty line.
    starts: Vec<usize>,
}

impl<'a> TextIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { text, starts }
    }

    pub fn text(&self) -> &'a str {
        self.text
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    fn line_end_incl(&self, line: usize) -> usize {
        self.starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text.len())
    }

    /// Byte span of the line content without its terminator.
    fn line_span(&self, line: usize) -> Option<(usize, usize)> {
        let s = *self.starts.get(line)?;
        let mut e = self.line_end_incl(line);
        let b = self.text.as_bytes();
        if e > s && b[e - 1] == b'\n' {
            e -= 1;
            if e > s && b[e - 1] == b'\r' {
                e -= 1;
            }
        }
        Some((s, e))
    }

    /// Line content without terminator, `None` if `line` is out of range.
    pub fn line(&self, line: usize) -> Option<&'a str> {
        self.line_span(line).map(|(s, e)| &self.text[s..e])
    }

    /// Char length of a line (without terminator).
    pub fn line_len(&self, line: usize) -> Option<usize> {
        self.line(line).map(|l| l.chars().count())
    }

    /// Byte offset of `pos`. A column past the end of the line clamps to the
    /// end of its content; a line past the end clamps to the end of the text.
    pub fn pos_to_byte(&self, pos: CharPos) -> usize {
        let Some((s, e)) = self.line_span(pos.line) else {
            return self.text.len();
        };
        let line = &self.text[s..e];
        match line.char_indices().nth(pos.col) {
            Some((i, _)) => s + i,
            None => e,
        }
    }

    /// Position of a byte offset (clamped to the text; offsets inside a
    /// multi-byte char round down to its start).
    pub fn byte_to_pos(&self, byte: usize) -> CharPos {
        let mut byte = byte.min(self.text.len());
        while !self.text.is_char_boundary(byte) {
            byte -= 1;
        }
        let line = match self.starts.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let (s, e) = self.line_span(line).unwrap_or((byte, byte));
        let upto = byte.min(e);
        CharPos {
            line,
            col: self.text[s..upto].chars().count(),
        }
    }

    /// Absolute char index (counting every char, including `\r` and `\n`).
    pub fn pos_to_char_idx(&self, pos: CharPos) -> usize {
        self.text[..self.pos_to_byte(pos)].chars().count()
    }

    /// Inverse of [`pos_to_char_idx`](Self::pos_to_char_idx); clamps to the text end.
    pub fn char_idx_to_pos(&self, idx: usize) -> CharPos {
        let byte = self
            .text
            .char_indices()
            .nth(idx)
            .map_or(self.text.len(), |(i, _)| i);
        self.byte_to_pos(byte)
    }

    /// Byte range of a char range (ordered, clamped).
    pub fn range_to_bytes(&self, r: CharRange) -> std::ops::Range<usize> {
        let r = r.ordered();
        self.pos_to_byte(r.start)..self.pos_to_byte(r.end)
    }

    /// The text covered by `r`.
    pub fn slice(&self, r: CharRange) -> &'a str {
        &self.text[self.range_to_bytes(r)]
    }

    /// Expand `r` to whole lines: from the start of the first line to the end
    /// of the last line *including* its terminator. A range ending at column 0
    /// of a later line does not include that line (editor convention).
    pub fn expand_to_lines(&self, r: CharRange) -> CharRange {
        let r = r.ordered();
        let first = r.start.line.min(self.line_count() - 1);
        let mut last = r.end.line.min(self.line_count() - 1);
        if r.end.col == 0 && r.end.line > r.start.line {
            last = last.saturating_sub(1).max(first);
        }
        let end = if last + 1 < self.line_count() {
            CharPos::new(last + 1, 0)
        } else {
            CharPos::new(last, self.line_len(last).unwrap_or(0))
        };
        CharRange::new(CharPos::new(first, 0), end)
    }

    /// New text with `r` replaced by `new`.
    pub fn replace(&self, r: CharRange, new: &str) -> String {
        let b = self.range_to_bytes(r);
        let mut out = String::with_capacity(self.text.len() - (b.end - b.start) + new.len());
        out.push_str(&self.text[..b.start]);
        out.push_str(new);
        out.push_str(&self.text[b.end..]);
        out
    }
}

/// Does the text predominantly use CRLF line endings?
pub fn uses_crlf(text: &str) -> bool {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count();
    crlf > 0 && crlf * 2 >= lf
}

/// Convert every line ending in `text` to `\n`.
pub fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Convert every line ending in `text` (assumed `\n` or `\r\n`) to `\r\n`.
pub fn to_crlf(text: &str) -> String {
    to_lf(text).replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(l: usize, c: usize) -> CharPos {
        CharPos::new(l, c)
    }

    #[test]
    fn ascii_roundtrip() {
        let t = "ab\ncde\n\nf";
        let ix = TextIndex::new(t);
        assert_eq!(ix.line_count(), 4);
        assert_eq!(ix.pos_to_byte(p(1, 2)), 5);
        assert_eq!(ix.byte_to_pos(5), p(1, 2));
        assert_eq!(ix.line(2), Some(""));
        assert_eq!(ix.line(9), None);
        for b in 0..=t.len() {
            assert_eq!(ix.pos_to_byte(ix.byte_to_pos(b)), b, "byte {b}");
        }
    }

    #[test]
    fn multibyte_chars_count_as_one() {
        let t = "héllo\n日本語x\n";
        let ix = TextIndex::new(t);
        // 'x' is col 3 on line 1 although it is byte offset 9 within the line
        let b = ix.pos_to_byte(p(1, 3));
        assert_eq!(&t[b..=b], "x");
        assert_eq!(ix.byte_to_pos(b), p(1, 3));
        assert_eq!(ix.pos_to_char_idx(p(1, 3)), 6 + 3);
        assert_eq!(ix.char_idx_to_pos(9), p(1, 3));
        assert_eq!(ix.slice(CharRange::new(p(0, 1), p(1, 1))), "éllo\n日");
        // mid-char byte rounds down
        assert_eq!(ix.byte_to_pos(2), p(0, 1));
    }

    #[test]
    fn emoji_surrogate_free() {
        let t = "a😀b";
        let ix = TextIndex::new(t);
        assert_eq!(ix.line_len(0), Some(3));
        assert_eq!(ix.slice(CharRange::new(p(0, 1), p(0, 2))), "😀");
    }

    #[test]
    fn crlf_columns_exclude_terminator() {
        let t = "ab\r\ncd\r\n";
        let ix = TextIndex::new(t);
        assert_eq!(ix.line(0), Some("ab"));
        assert_eq!(ix.line_len(0), Some(2));
        // column past the content clamps before the \r
        assert_eq!(ix.pos_to_byte(p(0, 99)), 2);
        // a byte pointing at the \n reports end of line
        assert_eq!(ix.byte_to_pos(3), p(0, 2));
        assert_eq!(ix.byte_to_pos(4), p(1, 0));
        assert_eq!(ix.pos_to_char_idx(p(1, 0)), 4);
        assert!(uses_crlf(t));
        assert!(!uses_crlf("a\nb\n"));
        assert_eq!(to_crlf("a\nb\r\nc"), "a\r\nb\r\nc");
        assert_eq!(to_lf("a\r\nb"), "a\nb");
    }

    #[test]
    fn clamping_never_panics() {
        let ix = TextIndex::new("abc");
        assert_eq!(ix.pos_to_byte(p(5, 5)), 3);
        assert_eq!(ix.byte_to_pos(100), p(0, 3));
        assert_eq!(ix.char_idx_to_pos(100), p(0, 3));
        let empty = TextIndex::new("");
        assert_eq!(empty.line_count(), 1);
        assert_eq!(empty.slice(CharRange::new(p(0, 0), p(3, 3))), "");
        assert_eq!(
            empty.expand_to_lines(CharRange::default()),
            CharRange::default()
        );
    }

    #[test]
    fn backwards_ranges_are_ordered() {
        let ix = TextIndex::new("hello world");
        let r = CharRange::new(p(0, 5), p(0, 0));
        assert_eq!(ix.slice(r), "hello");
        assert_eq!(ix.replace(r, "bye"), "bye world");
    }

    #[test]
    fn expand_to_lines_conventions() {
        let t = "one\ntwo\nthree\n";
        let ix = TextIndex::new(t);
        // mid-line selection on line 1
        let r = ix.expand_to_lines(CharRange::new(p(1, 1), p(1, 2)));
        assert_eq!(ix.slice(r), "two\n");
        // ends at column 0 of the next line: that line is not included
        let r = ix.expand_to_lines(CharRange::new(p(0, 0), p(2, 0)));
        assert_eq!(ix.slice(r), "one\ntwo\n");
        // multi-line
        let r = ix.expand_to_lines(CharRange::new(p(0, 2), p(2, 2)));
        assert_eq!(ix.slice(r), t);
        // last line without trailing newline
        let ix2 = TextIndex::new("a\nb");
        let r = ix2.expand_to_lines(CharRange::new(p(1, 0), p(1, 1)));
        assert_eq!(ix2.slice(r), "b");
    }

    #[test]
    fn replace_roundtrip() {
        let ix = TextIndex::new("fn a() {}\nfn b() {}\n");
        let r = ix.expand_to_lines(CharRange::new(p(1, 3), p(1, 4)));
        assert_eq!(ix.replace(r, "fn c() {}\n"), "fn a() {}\nfn c() {}\n");
    }
}
