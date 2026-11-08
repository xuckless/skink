use crate::Buffer;

/// Cursor position: 0-based line and char column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    pub line: usize,
    pub col: usize,
}

/// Editing state for one buffer. UI-free.
#[derive(Debug)]
pub struct Editor {
    pub buffer: Buffer,
    pub cursor: Cursor,
    /// First visible line.
    pub scroll: usize,
    pub viewport_height: usize,
    pub quit: bool,
    /// Column the cursor wants when moving vertically across short lines.
    want_col: usize,
}

impl Editor {
    pub fn new(buffer: Buffer) -> Self {
        Self {
            buffer,
            cursor: Cursor::default(),
            scroll: 0,
            viewport_height: 24,
            quit: false,
            want_col: 0,
        }
    }

    /// Jump to a 1-based line/column, clamped to the buffer.
    pub fn goto(&mut self, line: Option<usize>, col: Option<usize>) {
        let line = line
            .map_or(0, |l| l.saturating_sub(1))
            .min(self.last_line());
        let col = col
            .map_or(0, |c| c.saturating_sub(1))
            .min(self.buffer.line_len(line));
        self.cursor = Cursor { line, col };
        self.want_col = col;
        self.scroll_into_view();
    }

    pub fn set_viewport_height(&mut self, h: usize) {
        self.viewport_height = h.max(1);
        self.scroll_into_view();
    }

    fn last_line(&self) -> usize {
        self.buffer.len_lines() - 1
    }

    pub fn move_to_line(&mut self, line: usize) {
        self.cursor.line = line.min(self.last_line());
        self.cursor.col = self.want_col.min(self.buffer.line_len(self.cursor.line));
        self.scroll_into_view();
    }

    pub fn move_vertical(&mut self, delta: isize) {
        let line = self.cursor.line.saturating_add_signed(delta);
        self.move_to_line(line);
    }

    pub fn move_left(&mut self) {
        if self.cursor.col > 0 {
            self.cursor.col -= 1;
        } else if self.cursor.line > 0 {
            self.cursor.line -= 1;
            self.cursor.col = self.buffer.line_len(self.cursor.line);
        }
        self.want_col = self.cursor.col;
        self.scroll_into_view();
    }

    pub fn move_right(&mut self) {
        if self.cursor.col < self.buffer.line_len(self.cursor.line) {
            self.cursor.col += 1;
        } else if self.cursor.line < self.last_line() {
            self.cursor.line += 1;
            self.cursor.col = 0;
        }
        self.want_col = self.cursor.col;
        self.scroll_into_view();
    }

    pub fn move_to_col(&mut self, col: usize) {
        self.cursor.col = col.min(self.buffer.line_len(self.cursor.line));
        self.want_col = self.cursor.col;
    }

    pub fn move_line_end(&mut self) {
        self.move_to_col(self.buffer.line_len(self.cursor.line));
    }

    pub fn scroll_into_view(&mut self) {
        let h = self.viewport_height.max(1);
        if self.cursor.line < self.scroll {
            self.scroll = self.cursor.line;
        } else if self.cursor.line >= self.scroll + h {
            self.scroll = self.cursor.line + 1 - h;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        Editor::new(Buffer::from_text(text))
    }

    #[test]
    fn vertical_move_remembers_column() {
        let mut e = ed("long line\nx\nlong line");
        e.goto(Some(1), Some(6));
        e.move_vertical(1);
        assert_eq!(e.cursor, Cursor { line: 1, col: 1 });
        e.move_vertical(1);
        assert_eq!(e.cursor, Cursor { line: 2, col: 5 });
    }

    #[test]
    fn left_right_wrap_lines() {
        let mut e = ed("ab\ncd");
        e.goto(Some(1), Some(3));
        e.move_right();
        assert_eq!(e.cursor, Cursor { line: 1, col: 0 });
        e.move_left();
        assert_eq!(e.cursor, Cursor { line: 0, col: 2 });
    }

    #[test]
    fn scroll_follows_cursor() {
        let mut e = ed(&"x\n".repeat(50));
        e.set_viewport_height(10);
        e.move_vertical(20);
        assert_eq!(e.scroll, 11);
        e.move_vertical(-20);
        assert_eq!(e.scroll, 0);
    }

    #[test]
    fn goto_clamps() {
        let mut e = ed("ab\ncd");
        e.goto(Some(99), Some(99));
        assert_eq!(e.cursor, Cursor { line: 1, col: 2 });
    }
}
