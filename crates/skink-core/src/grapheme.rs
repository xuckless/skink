//! Grapheme clusters: what a user sees as one character (an emoji with
//! modifiers or ZWJ joins, a letter with combining accents, a flag). The
//! cursor moves over, and Backspace/Delete remove, whole clusters.
//! Boundaries come from `unicode-segmentation` (MIT/Apache-2.0) over a small
//! window of the line, so very long lines stay cheap.

use crate::Editor;
use unicode_segmentation::GraphemeCursor;

/// Chars of context on each side; clusters longer than this are split.
const WINDOW: usize = 64;

impl Editor {
    /// A window of the buffer around char `i`, inside `i`'s line (plus its
    /// newline): (start char, text).
    fn window(&self, i: usize) -> (usize, String) {
        let b = &self.buffer;
        let line = b.char_to_line(i);
        let ls = b.line_to_char(line);
        let le = if line + 1 < b.len_lines() {
            b.line_to_char(line + 1)
        } else {
            b.len_chars()
        };
        let start = i.saturating_sub(WINDOW).max(ls);
        let end = (i + WINDOW).min(le);
        (start, b.slice(start, end))
    }

    fn byte_in(text: &str, chars: usize) -> usize {
        text.char_indices()
            .nth(chars)
            .map_or(text.len(), |(b, _)| b)
    }

    /// Start of the cluster before char `i` (at line starts: `i - 1`).
    pub(crate) fn prev_grapheme(&self, i: usize) -> usize {
        if i == 0 {
            return 0;
        }
        let (start, text) = self.window(i);
        if start == i {
            return i - 1; // across the line break
        }
        let at = Self::byte_in(&text, i - start);
        let mut c = GraphemeCursor::new(at, text.len(), true);
        match c.prev_boundary(&text, 0) {
            Ok(Some(b)) => start + text[..b].chars().count(),
            _ => i - 1,
        }
    }

    /// End of the cluster starting at char `i`.
    pub(crate) fn next_grapheme(&self, i: usize) -> usize {
        let n = self.buffer.len_chars();
        if i >= n {
            return n;
        }
        let (start, text) = self.window(i);
        let at = Self::byte_in(&text, i - start);
        let mut c = GraphemeCursor::new(at, text.len(), true);
        match c.next_boundary(&text, 0) {
            Ok(Some(b)) if b > at => start + text[..b].chars().count(),
            _ => i + 1,
        }
    }

    /// `i`, moved back to the start of its cluster if it points inside one.
    pub(crate) fn snap_grapheme(&self, i: usize) -> usize {
        if i == 0 || i >= self.buffer.len_chars() {
            return i;
        }
        let (start, text) = self.window(i);
        let at = Self::byte_in(&text, i - start);
        let mut c = GraphemeCursor::new(at, text.len(), true);
        match c.is_boundary(&text, 0) {
            Ok(false) => self.prev_grapheme(i),
            _ => i,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Buffer, Editor};

    #[test]
    fn cursor_and_deletes_step_over_whole_clusters() {
        // family emoji (ZWJ sequence), e + combining acute, a flag.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let text = format!("a{family}e\u{301}\u{1F1E9}\u{1F1EA}b");
        let mut e = Editor::new(Buffer::from_text(&text));
        e.move_right(false); // after a
        e.move_right(false); // over the family
        assert_eq!(e.cursor.col, 1 + family.chars().count());
        e.move_right(false); // over e + accent
        e.move_right(false); // over the flag
        assert_eq!(e.buffer.char_at(e.cursor_idx()), Some('b'));
        e.backspace(); // removes the whole flag
        assert_eq!(e.buffer.text(), format!("a{family}e\u{301}b"));
        e.move_left(false);
        e.backspace(); // the family goes as one
        assert_eq!(e.buffer.text(), "ae\u{301}b");
        e.delete_forward(); // e + accent as one
        assert_eq!(e.buffer.text(), "ab");
    }
}
