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

/// Byte length of the line break starting at `b[i]` (0 if none). Same set
/// as ropey's default features: LF, VT, FF, CR, CRLF, NEL, LS, PS.
fn line_break_len(b: &[u8], i: usize) -> usize {
    match b[i] {
        b'\n' | 0x0B | 0x0C => 1,
        b'\r' => 1 + usize::from(b.get(i + 1) == Some(&b'\n')),
        0xC2 if b.get(i + 1) == Some(&0x85) => 2,
        0xE2 if b.get(i + 1) == Some(&0x80) && matches!(b.get(i + 2), Some(0xA8 | 0xA9)) => 3,
        _ => 0,
    }
}

/// Line table over a borrowed text.
pub struct TextIndex<'a> {
    text: &'a str,
    /// Byte span `(start, content_end)` of every line, terminator excluded.
    /// Like ropey, text ending in a line break has a final empty line, and
    /// lone `\r` and the Unicode breaks count (so positions taken from the
    /// editor's rope address the same lines).
    lines: Vec<(usize, usize)>,
}

impl<'a> TextIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let b = text.as_bytes();
        let mut lines = Vec::new();
        let (mut start, mut i) = (0, 0);
        while i < b.len() {
            match line_break_len(b, i) {
                0 => i += 1,
                n => {
                    lines.push((start, i));
                    i += n;
                    start = i;
                }
            }
        }
        lines.push((start, b.len()));
        Self { text, lines }
    }

    pub fn text(&self) -> &'a str {
        self.text
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Byte span of the line content without its terminator.
    fn line_span(&self, line: usize) -> Option<(usize, usize)> {
        self.lines.get(line).copied()
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
        // Last line starting at or before `byte` (line 0 starts at 0).
        let line = self.lines.partition_point(|&(s, _)| s <= byte) - 1;
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
    fn line_breaks_match_ropey() {
        // Regression: only `\n` split lines, so a position from the editor's
        // rope (which also breaks on lone `\r`, VT, FF, NEL, LS, PS) pointed
        // at a different line here.
        let t = "a\rb\r\nc\u{2028}d\u{85}e\x0bf\x0cg\u{2029}";
        let ix = TextIndex::new(t);
        let lines: Vec<_> = (0..ix.line_count()).map(|l| ix.line(l).unwrap()).collect();
        assert_eq!(lines, ["a", "b", "c", "d", "e", "f", "g", ""]);
        let b = ix.pos_to_byte(p(3, 0));
        assert_eq!(&t[b..b + 1], "d");
        assert_eq!(ix.byte_to_pos(b), p(3, 0));
        // a byte inside a multi-byte terminator reports end of its line
        assert_eq!(ix.byte_to_pos(b - 1), p(2, 1));
        for b in (0..=t.len()).filter(|&b| t.is_char_boundary(b)) {
            let pos = ix.byte_to_pos(b);
            assert!(ix.pos_to_byte(pos) <= b, "byte {b}");
        }
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
