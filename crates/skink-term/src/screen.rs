//! Owned, UI-toolkit-free snapshot of the visible terminal grid.

/// Terminal color. `Indexed(0..=15)` are the themeable ANSI colors,
/// `16..=255` the fixed xterm cube/greyscale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl From<crate::vt::Color> for Color {
    fn from(c: crate::vt::Color) -> Self {
        match c {
            crate::vt::Color::Default => Color::Default,
            crate::vt::Color::Idx(i) => Color::Indexed(i),
            crate::vt::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }
}

/// Text attribute bit set. (The emulator does not track strikethrough, blink
/// or underline styles/colors; see NOTICES.md.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs(u8);

impl Attrs {
    pub const BOLD: u8 = 1;
    pub const ITALIC: u8 = 2;
    pub const UNDERLINE: u8 = 4;
    pub const REVERSE: u8 = 8;
    pub const DIM: u8 = 16;

    pub(crate) const fn from_bits(bits: u8) -> Self {
        Attrs(bits)
    }

    pub fn bold(self) -> bool {
        self.0 & Self::BOLD != 0
    }
    pub fn italic(self) -> bool {
        self.0 & Self::ITALIC != 0
    }
    pub fn underline(self) -> bool {
        self.0 & Self::UNDERLINE != 0
    }
    /// Swap fg/bg when drawing (the grid stores the colors unswapped).
    pub fn reverse(self) -> bool {
        self.0 & Self::REVERSE != 0
    }
    pub fn dim(self) -> bool {
        self.0 & Self::DIM != 0
    }
    pub fn bits(self) -> u8 {
        self.0
    }
}

/// One grid cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// Base character (`' '` for an empty cell and for wide continuations).
    pub ch: char,
    /// Combining characters following `ch`, if any.
    pub combining: Option<Box<str>>,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
    /// Columns covered: 1 normal, 2 for the left half of a wide char, 0 for
    /// the right-half continuation cell (draw nothing there).
    pub width: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            combining: None,
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::default(),
            width: 1,
        }
    }
}

impl Cell {
    /// Append this cell's text (nothing for continuation cells).
    pub fn push_text(&self, out: &mut String) {
        if self.width == 0 {
            return;
        }
        out.push(self.ch);
        if let Some(c) = &self.combining {
            out.push_str(c);
        }
    }

    pub fn is_wide_continuation(&self) -> bool {
        self.width == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    /// False when the app hid it (DECTCEM) or the view is scrolled back.
    pub visible: bool,
    pub shape: CursorShape,
    pub blink: bool,
}

/// Which mouse events the application asked to receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    Off,
    /// X10 (DECSET 9): presses only.
    Press,
    /// DECSET 1000: press, release, wheel.
    PressRelease,
    /// DECSET 1002: plus drag.
    ButtonMotion,
    /// DECSET 1003: plus all motion.
    AnyMotion,
}

/// A snapshot of the pane as the user currently sees it (honours the
/// scrollback view offset).
#[derive(Debug, Clone)]
pub struct Screen {
    pub rows: u16,
    pub cols: u16,
    /// Row-major, `rows * cols` cells.
    pub cells: Vec<Cell>,
    pub cursor: Cursor,
    /// Window title set via OSC 0/2 (empty if never set).
    pub title: String,
    /// True while the alternate screen buffer is active (vim, htop, ...).
    pub alt_screen: bool,
    /// Lines the view is scrolled back from the live screen (0 = live).
    pub scroll_offset: usize,
    /// Lines of scrollback currently stored (always <= the configured cap).
    pub scrollback_len: usize,
    pub mouse_mode: MouseMode,
    pub bracketed_paste: bool,
    pub application_cursor: bool,
}

impl Screen {
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if row < self.rows && col < self.cols {
            self.cells
                .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
        } else {
            None
        }
    }

    pub fn row(&self, row: u16) -> &[Cell] {
        let w = usize::from(self.cols);
        let start = usize::from(row) * w;
        self.cells.get(start..start + w).unwrap_or(&[])
    }

    /// Row text with trailing blanks trimmed.
    pub fn row_text(&self, row: u16) -> String {
        let mut s = String::new();
        for c in self.row(row) {
            c.push_text(&mut s);
        }
        s.truncate(s.trim_end().len());
        s
    }

    /// All rows joined with `\n`, trailing blank rows trimmed.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for r in 0..self.rows {
            s.push_str(&self.row_text(r));
            s.push('\n');
        }
        s.truncate(s.trim_end().len());
        s
    }
}
