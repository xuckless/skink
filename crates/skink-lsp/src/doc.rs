//! A text document as the client sees it: the full text plus a line index so
//! char <-> UTF-16 column conversion is O(line length).

use std::path::Path;

use crate::{
    pos::{char_to_byte, char_to_utf16, utf16_to_char},
    types::{Position, Range},
};

/// Files larger than this are not read from disk just to convert columns.
const MAX_DISK_DOC: u64 = 16 * 1024 * 1024;

/// An open document (text, line index and the client's version counter).
/// Shared with `skink-host`, which mirrors buffers the same way.
#[derive(Debug)]
pub struct Doc {
    pub version: i64,
    text: String,
    /// Byte offset of the start of each line (always starts with 0).
    starts: Vec<usize>,
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(
        text.bytes()
            .enumerate()
            .filter(|&(_, b)| b == b'\n')
            .map(|(i, _)| i + 1),
    );
    v
}

impl Doc {
    pub fn new(version: i64, text: String) -> Self {
        let starts = line_starts(&text);
        Self {
            version,
            text,
            starts,
        }
    }

    pub fn from_disk(path: &Path) -> Option<Self> {
        if std::fs::metadata(path).ok()?.len() > MAX_DISK_DOC {
            return None;
        }
        Some(Self::new(0, std::fs::read_to_string(path).ok()?))
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: String) {
        self.starts = line_starts(&text);
        self.text = text;
    }

    /// Line `i` without its `\n` / `\r\n`.
    pub fn line(&self, i: usize) -> Option<&str> {
        let start = *self.starts.get(i)?;
        let end = self.starts.get(i + 1).map_or(self.text.len(), |&n| n - 1);
        let l = &self.text[start..end];
        Some(l.strip_suffix('\r').unwrap_or(l))
    }

    /// Char column -> UTF-16 column on `line` (identity if the line is unknown).
    pub fn utf16_col(&self, line: usize, col: usize) -> u32 {
        self.line(line)
            .map_or(col as u32, |l| char_to_utf16(l, col))
    }

    /// UTF-16 column -> char column on `line` (identity if the line is unknown).
    pub fn char_col(&self, line: u32, col16: u32) -> usize {
        self.line(line as usize)
            .map_or(col16 as usize, |l| utf16_to_char(l, col16))
    }

    /// Byte offset of `p`, clamped to the document / line end.
    pub fn byte_offset(&self, p: Position) -> usize {
        match self.line(p.line) {
            Some(l) => self.starts[p.line] + char_to_byte(l, p.col),
            None => self.text.len(),
        }
    }

    /// Replace `range` (char coordinates) with `new`.
    pub fn replace(&mut self, range: Range, new: &str) {
        let s = self.byte_offset(range.start);
        let e = self.byte_offset(range.end).max(s);
        self.text.replace_range(s..e, new);
        self.starts = line_starts(&self.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(l: usize, c: usize) -> Position {
        Position::new(l, c)
    }

    #[test]
    fn lines_with_crlf_and_trailing_newline() {
        let d = Doc::new(1, "ab\r\ncd\n".into());
        assert_eq!(d.line(0), Some("ab"));
        assert_eq!(d.line(1), Some("cd"));
        assert_eq!(d.line(2), Some(""));
        assert_eq!(d.line(3), None);
    }

    #[test]
    fn column_conversion() {
        let d = Doc::new(1, "a😀b\nz".into());
        assert_eq!(d.utf16_col(0, 2), 3);
        assert_eq!(d.char_col(0, 3), 2);
        assert_eq!(d.utf16_col(9, 4), 4); // unknown line: identity
    }

    #[test]
    fn replace_ranges() {
        let mut d = Doc::new(1, "héllo\nwörld".into());
        d.replace(Range::new(p(0, 1), p(1, 2)), "X\nY");
        assert_eq!(d.text(), "hX\nYrld");
        assert_eq!(d.line(1), Some("Yrld"));
        d.replace(Range::new(p(5, 0), p(5, 0)), "!"); // past end appends
        assert_eq!(d.text(), "hX\nYrld!");
        d.replace(Range::new(p(0, 0), p(0, 99)), ""); // col clamps to line end
        assert_eq!(d.text(), "\nYrld!");
    }

    #[test]
    fn insert_at_crlf_line_end_keeps_cr() {
        let mut d = Doc::new(1, "ab\r\ncd".into());
        d.replace(Range::new(p(0, 2), p(0, 2)), "!");
        assert_eq!(d.text(), "ab!\r\ncd");
    }
}
