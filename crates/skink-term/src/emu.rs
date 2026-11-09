//! Emulator state (no pty): vendored `vt100` screen model + query sniffer + extras.
//! Kept separate from `Term` so it is unit-testable by feeding bytes.

use crate::screen::{Attrs, Cell, Color, Cursor, CursorShape, MouseMode, Screen};
use crate::sniff::{Event, Sniffer};

/// Parser callbacks: capture the window title and OSC 52 clipboard requests.
#[derive(Default)]
pub(crate) struct Cb {
    pub title: String,
    pub clipboard: Vec<ClipboardRequest>,
    /// OSC 133 shell-integration marks, oldest first.
    pub marks: Vec<Mark>,
}

/// What an OSC 133 (FinalTerm / iTerm2 / VS Code shell integration) mark says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    /// `A`: a prompt starts.
    Prompt,
    /// `B`: the prompt ends, the command line starts.
    Input,
    /// `C`: the command runs; its output starts on this row.
    Output,
    /// `D[;code]`: the command finished.
    Done(Option<i32>),
}

/// A shell-integration mark at an absolute row (stable across scrolling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub kind: MarkKind,
    pub row: u64,
}

/// Marks kept per terminal (older ones are dropped).
const MAX_MARKS: usize = 4096;

/// All kept rows of a terminal's main screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    /// Absolute row number of `rows[0]`.
    pub first: u64,
    /// Text per screen row, and whether it soft-wraps into the next.
    pub rows: Vec<(String, bool)>,
}

impl History {
    /// Rows joined into logical lines (soft wraps undone).
    pub fn text(&self) -> String {
        self.text_between(self.first, self.first + self.rows.len() as u64)
    }

    /// Logical text of absolute rows `from..to`.
    pub fn text_between(&self, from: u64, to: u64) -> String {
        let s = from.saturating_sub(self.first) as usize;
        let e = (to.saturating_sub(self.first) as usize).min(self.rows.len());
        let mut out = String::new();
        for (t, wrapped) in self.rows.get(s..e).unwrap_or_default() {
            if *wrapped {
                out.push_str(t);
            } else {
                out.push_str(t.trim_end());
                out.push('\n');
            }
        }
        out
    }
}

/// An OSC 52 request from the child to set a clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardRequest {
    /// Selection letters as sent (`c`, `p`, `s`, ...; may be empty).
    pub selection: String,
    /// Base64 payload exactly as sent by the app.
    pub data_base64: String,
}

const MAX_CLIPBOARD_QUEUE: usize = 8;
const MAX_CLIPBOARD_BYTES: usize = 1 << 20;

impl crate::vt::Callbacks for Cb {
    fn set_window_title(&mut self, _: &mut crate::vt::Screen, title: &[u8]) {
        self.title = String::from_utf8_lossy(title).into_owned();
    }

    fn unhandled_osc(&mut self, screen: &mut crate::vt::Screen, params: &[&[u8]]) {
        let [b"133", what, rest @ ..] = params else {
            return;
        };
        let kind = match what.first() {
            Some(b'A') => MarkKind::Prompt,
            Some(b'B') => MarkKind::Input,
            Some(b'C') => MarkKind::Output,
            Some(b'D') => MarkKind::Done(
                rest.first()
                    .and_then(|c| std::str::from_utf8(c).ok())
                    .and_then(|c| c.parse().ok()),
            ),
            _ => return,
        };
        let g = screen.main_grid();
        let row = g.abs_row(g.pos().row);
        if self.marks.len() == MAX_MARKS {
            self.marks.remove(0);
        }
        self.marks.push(Mark { kind, row });
    }

    fn copy_to_clipboard(&mut self, _: &mut crate::vt::Screen, ty: &[u8], data: &[u8]) {
        if data != b"?"
            && data.len() <= MAX_CLIPBOARD_BYTES
            && self.clipboard.len() < MAX_CLIPBOARD_QUEUE
        {
            self.clipboard.push(ClipboardRequest {
                selection: String::from_utf8_lossy(ty).into_owned(),
                data_base64: String::from_utf8_lossy(data).into_owned(),
            });
        }
    }
}

pub(crate) struct Emu {
    parser: crate::vt::Parser<Cb>,
    sniffer: Sniffer,
    scratch: Vec<(usize, Event)>,
    shape: CursorShape,
    blink: bool,
    pub focus_reporting: bool,
    rows: u16,
    cols: u16,
}

impl Emu {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        let (rows, cols) = (rows.max(1), cols.max(1));
        Emu {
            parser: crate::vt::Parser::new_with_callbacks(rows, cols, scrollback, Cb::default()),
            sniffer: Sniffer::new(),
            scratch: Vec::new(),
            shape: CursorShape::Block,
            blink: true,
            focus_reporting: false,
            rows,
            cols,
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        self.rows = rows;
        self.cols = cols;
        self.parser.screen_mut().set_size(rows, cols);
    }

    /// Feed pty output. Any terminal-query answers the app is waiting for are
    /// appended to `replies` (to be written back to the pty).
    pub fn feed(&mut self, bytes: &[u8], replies: &mut Vec<u8>) {
        self.scratch.clear();
        self.sniffer.feed(bytes, &mut self.scratch);
        if self.scratch.is_empty() {
            self.parser.process(bytes);
            return;
        }
        // Process up to each query so e.g. the cursor position we report is
        // the one at that point in the stream.
        let events = std::mem::take(&mut self.scratch);
        let mut pos = 0;
        for &(end, ev) in &events {
            self.parser.process(&bytes[pos..end]);
            pos = end;
            self.on_event(ev, replies);
        }
        self.parser.process(&bytes[pos..]);
        self.scratch = events;
    }

    fn on_event(&mut self, ev: Event, replies: &mut Vec<u8>) {
        match ev {
            Event::DeviceStatus => replies.extend_from_slice(b"\x1b[0n"),
            Event::CursorPosition => {
                let (r, c) = self.parser.screen().cursor_position();
                replies.extend_from_slice(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes());
            }
            // VT220 with ANSI color (62;22), the common xterm-ish answer.
            Event::PrimaryDa => replies.extend_from_slice(b"\x1b[?62;22c"),
            Event::SecondaryDa => replies.extend_from_slice(b"\x1b[>0;0;0c"),
            Event::WindowChars => {
                replies
                    .extend_from_slice(format!("\x1b[8;{};{}t", self.rows, self.cols).as_bytes());
            }
            Event::CursorStyle(n) => {
                self.blink = n == 0 || n % 2 == 1;
                self.shape = match n {
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
            }
            Event::FocusReporting(on) => self.focus_reporting = on,
        }
    }

    pub fn take_clipboard(&mut self) -> Vec<ClipboardRequest> {
        std::mem::take(&mut self.parser.callbacks_mut().clipboard)
    }

    pub fn app_cursor(&self) -> bool {
        self.parser.screen().application_cursor()
    }
    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }
    pub fn alt_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }
    pub fn mouse(
        &self,
    ) -> (
        crate::vt::MouseProtocolMode,
        crate::vt::MouseProtocolEncoding,
    ) {
        let s = self.parser.screen();
        (s.mouse_protocol_mode(), s.mouse_protocol_encoding())
    }

    /// Lines of scrollback stored right now.
    pub fn scrollback_len(&mut self) -> usize {
        let cur = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let max = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(cur);
        max
    }

    pub fn scroll_offset(&self) -> usize {
        self.parser.screen().scrollback()
    }

    pub fn set_scroll_offset(&mut self, n: usize) {
        self.parser.screen_mut().set_scrollback(n);
    }

    /// Kept rows of the main screen.
    pub fn history(&self) -> History {
        let (first, rows) = self.parser.screen().main_grid().history();
        History { first, rows }
    }

    /// Shell-integration marks still inside the kept history.
    pub fn marks(&self) -> Vec<Mark> {
        let first = self.parser.screen().main_grid().history().0;
        self.parser
            .callbacks()
            .marks
            .iter()
            .filter(|m| m.row >= first)
            .copied()
            .collect()
    }

    /// Absolute row at the top of the view.
    pub fn top_row(&self) -> u64 {
        self.parser.screen().main_grid().top_abs_row()
    }

    /// Scroll the view so absolute row `abs` is at the top.
    pub fn scroll_to_row(&mut self, abs: u64) {
        self.parser.screen_mut().main_grid_mut().scroll_to_abs(abs);
    }

    pub fn snapshot_into(&mut self, out: &mut Screen) {
        let scrollback_len = self.scrollback_len();
        let s = self.parser.screen();
        let (rows, cols) = s.size();
        out.rows = rows;
        out.cols = cols;
        out.cells.clear();
        out.cells.reserve(usize::from(rows) * usize::from(cols));
        for r in 0..rows {
            for c in 0..cols {
                out.cells
                    .push(s.cell(r, c).map(convert_cell).unwrap_or_default());
            }
        }
        let (cr, cc) = s.cursor_position();
        let offset = s.scrollback();
        out.cursor = Cursor {
            row: cr,
            col: cc,
            visible: !s.hide_cursor() && offset == 0,
            shape: self.shape,
            blink: self.blink,
        };
        out.title.clear();
        out.title.push_str(&self.parser.callbacks().title);
        out.alt_screen = s.alternate_screen();
        out.scroll_offset = offset;
        out.scrollback_len = scrollback_len;
        out.mouse_mode = match s.mouse_protocol_mode() {
            crate::vt::MouseProtocolMode::None => MouseMode::Off,
            crate::vt::MouseProtocolMode::Press => MouseMode::Press,
            crate::vt::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
            crate::vt::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
            crate::vt::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
        };
        out.bracketed_paste = s.bracketed_paste();
        out.application_cursor = s.application_cursor();
    }
}

pub(crate) fn empty_screen() -> Screen {
    Screen {
        rows: 0,
        cols: 0,
        cells: Vec::new(),
        cursor: Cursor {
            row: 0,
            col: 0,
            visible: true,
            shape: CursorShape::Block,
            blink: true,
        },
        title: String::new(),
        alt_screen: false,
        scroll_offset: 0,
        scrollback_len: 0,
        mouse_mode: MouseMode::Off,
        bracketed_paste: false,
        application_cursor: false,
    }
}

fn convert_cell(c: &crate::vt::Cell) -> Cell {
    let width = if c.is_wide_continuation() {
        0
    } else if c.is_wide() {
        2
    } else {
        1
    };
    let mut attrs = 0;
    if c.bold() {
        attrs |= Attrs::BOLD;
    }
    if c.dim() {
        attrs |= Attrs::DIM;
    }
    if c.italic() {
        attrs |= Attrs::ITALIC;
    }
    if c.underline() {
        attrs |= Attrs::UNDERLINE;
    }
    if c.inverse() {
        attrs |= Attrs::REVERSE;
    }
    let (ch, combining) = if c.has_contents() && width != 0 {
        let s = c.contents();
        let mut it = s.chars();
        let ch = it.next().unwrap_or(' ');
        let rest = it.as_str();
        (ch, (!rest.is_empty()).then(|| Box::<str>::from(rest)))
    } else {
        (' ', None)
    };
    Cell {
        ch,
        combining,
        fg: Color::from(c.fgcolor()),
        bg: Color::from(c.bgcolor()),
        attrs: Attrs::from_bits(attrs),
        width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(e: &mut Emu, s: &str) -> Vec<u8> {
        let mut r = Vec::new();
        e.feed(s.as_bytes(), &mut r);
        r
    }
    fn snap(e: &mut Emu) -> Screen {
        let mut s = empty_screen();
        e.snapshot_into(&mut s);
        s
    }

    #[test]
    fn text_and_cursor_movement() {
        let mut e = Emu::new(5, 20, 100);
        feed(&mut e, "hello\r\nworld");
        let s = snap(&mut e);
        assert_eq!(s.row_text(0), "hello");
        assert_eq!(s.row_text(1), "world");
        assert_eq!((s.cursor.row, s.cursor.col), (1, 5));
        // CUP, CUU/CUF, then overwrite.
        feed(&mut e, "\x1b[3;4HX\x1b[2AY\x1b[2DZ");
        let s = snap(&mut e);
        assert_eq!(s.cell(2, 3).unwrap().ch, 'X');
        assert_eq!(s.cell(0, 4).unwrap().ch, 'Y'); // CUU 2 from row 2 -> row 0, col 4 after X
        assert_eq!(s.cell(0, 3).unwrap().ch, 'Z'); // CUB 2 from col 5 -> col 3
        assert_eq!(s.row_text(0).len(), 5);
    }

    #[test]
    fn sgr_colors_and_attrs() {
        let mut e = Emu::new(3, 40, 0);
        feed(
            &mut e,
            "\x1b[31mR\x1b[0m\x1b[38;5;200mI\x1b[38;2;1;2;3mT\x1b[0m\x1b[1;3;4;7;42mB\x1b[0mN",
        );
        let s = snap(&mut e);
        assert_eq!(s.cell(0, 0).unwrap().fg, Color::Indexed(1));
        assert_eq!(s.cell(0, 1).unwrap().fg, Color::Indexed(200));
        assert_eq!(s.cell(0, 2).unwrap().fg, Color::Rgb(1, 2, 3));
        let b = s.cell(0, 3).unwrap();
        assert!(b.attrs.bold() && b.attrs.italic() && b.attrs.underline() && b.attrs.reverse());
        assert_eq!(b.bg, Color::Indexed(2));
        let n = s.cell(0, 4).unwrap();
        assert_eq!(
            (n.fg, n.bg, n.attrs.bits()),
            (Color::Default, Color::Default, 0)
        );
    }

    #[test]
    fn alt_screen_roundtrip() {
        let mut e = Emu::new(3, 20, 100);
        feed(&mut e, "main");
        feed(&mut e, "\x1b[?1049h\x1b[2Jalt");
        let s = snap(&mut e);
        assert!(s.alt_screen);
        assert_eq!(s.text(), "alt");
        feed(&mut e, "\x1b[?1049l");
        let s = snap(&mut e);
        assert!(!s.alt_screen);
        assert_eq!(s.text(), "main");
    }

    #[test]
    fn modes_title_cursor() {
        let mut e = Emu::new(3, 20, 0);
        feed(
            &mut e,
            "\x1b[?1h\x1b[?2004h\x1b[?1006h\x1b[?1002h\x1b[?25l\x1b]0;my title\x07",
        );
        let s = snap(&mut e);
        assert!(s.application_cursor && s.bracketed_paste && !s.cursor.visible);
        assert_eq!(s.mouse_mode, MouseMode::ButtonMotion);
        assert_eq!(s.title, "my title");
        feed(&mut e, "\x1b[?25h\x1b[6 q");
        let s = snap(&mut e);
        assert!(s.cursor.visible);
        assert_eq!((s.cursor.shape, s.cursor.blink), (CursorShape::Bar, false));
    }

    #[test]
    fn wide_and_combining_chars() {
        let mut e = Emu::new(2, 10, 0);
        feed(&mut e, "a世e\u{301}b");
        let s = snap(&mut e);
        assert_eq!(s.cell(0, 1).unwrap().width, 2);
        assert_eq!(s.cell(0, 1).unwrap().ch, '世');
        assert!(s.cell(0, 2).unwrap().is_wide_continuation());
        let c = s.cell(0, 3).unwrap();
        assert_eq!((c.ch, c.combining.as_deref()), ('e', Some("\u{301}")));
        assert_eq!(s.cell(0, 4).unwrap().ch, 'b');
        assert_eq!(s.row_text(0), "a世e\u{301}b");
    }

    #[test]
    fn replies_to_queries_at_the_right_point() {
        let mut e = Emu::new(5, 20, 0);
        assert_eq!(feed(&mut e, "ab\x1b[6ncd"), b"\x1b[1;3R");
        assert_eq!(feed(&mut e, "\x1b[5n"), b"\x1b[0n");
        assert_eq!(feed(&mut e, "\x1b[c"), b"\x1b[?62;22c");
        assert_eq!(feed(&mut e, "\x1b[18t"), b"\x1b[8;5;20t");
        // Query split across two reads still answered once.
        let mut r = Vec::new();
        e.feed(b"\x1b[", &mut r);
        e.feed(b"6n", &mut r);
        assert_eq!(r, b"\x1b[1;5R");
    }

    #[test]
    fn osc133_marks_and_history_survive_scrolling() {
        let mut e = Emu::new(3, 20, 100);
        feed(
            &mut e,
            "\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07one\r\ntwo\r\nthree\r\n\x1b]133;D;1\x07\x1b]133;A\x07$ ",
        );
        let m = e.marks();
        let kinds: Vec<MarkKind> = m.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            [
                MarkKind::Prompt,
                MarkKind::Input,
                MarkKind::Output,
                MarkKind::Done(Some(1)),
                MarkKind::Prompt
            ]
        );
        let h = e.history();
        assert_eq!(h.first, 0);
        assert_eq!(h.text(), "$ ls\none\ntwo\nthree\n$\n");
        // Output of the command: from its C mark to the D mark.
        assert_eq!(h.text_between(m[2].row, m[3].row), "one\ntwo\nthree\n");
        e.scroll_to_row(1);
        assert_eq!(e.top_row(), 1);
    }

    #[test]
    fn scrollback_is_capped_and_scrollable() {
        let mut e = Emu::new(4, 10, 50);
        for i in 0..1000 {
            feed(&mut e, &format!("line {i}\r\n"));
        }
        assert_eq!(e.scrollback_len(), 50);
        e.set_scroll_offset(10);
        let s = snap(&mut e);
        assert_eq!(s.scroll_offset, 10);
        assert!(!s.cursor.visible);
        assert_eq!(s.row_text(0), "line 987");
        e.set_scroll_offset(1_000_000);
        assert_eq!(snap(&mut e).scroll_offset, 50);
        e.set_scroll_offset(0);
        assert_eq!(snap(&mut e).row_text(2), "line 999");
    }

    #[test]
    fn dim_title_and_osc52() {
        let mut e = Emu::new(2, 20, 0);
        feed(&mut e, "\x1b[2mD\x1b[0mN\x1b]52;c;aGk=\x07\x1b]52;c;?\x07");
        let s = snap(&mut e);
        assert!(s.cell(0, 0).unwrap().attrs.dim());
        assert!(!s.cell(0, 1).unwrap().attrs.dim());
        let reqs = e.take_clipboard();
        assert_eq!(reqs.len(), 1, "queries (`?`) are not forwarded");
        assert_eq!(
            (reqs[0].selection.as_str(), reqs[0].data_base64.as_str()),
            ("c", "aGk=")
        );
        assert!(e.take_clipboard().is_empty());
    }

    #[test]
    fn resize_keeps_content() {
        let mut e = Emu::new(3, 10, 0);
        feed(&mut e, "abc");
        e.resize(6, 30);
        let s = snap(&mut e);
        assert_eq!((s.rows, s.cols), (6, 30));
        assert_eq!(s.row_text(0), "abc");
        e.resize(0, 0); // clamped, must not panic
        assert_eq!(e.size(), (1, 1));
    }

    #[test]
    fn wrapping_on_a_one_row_screen_does_not_panic() {
        // Regression: vt grid `col_wrap` underflowed (`prev_pos.row -= 1`).
        let mut e = Emu::new(1, 3, 10);
        feed(&mut e, "abcdefgh");
        assert_eq!(snap(&mut e).row_text(0), "gh");
    }

    #[test]
    fn wide_char_on_a_one_column_screen_is_dropped() {
        // Regression: `size.cols - width` underflowed, then unwrap on None.
        let mut e = Emu::new(3, 1, 0);
        feed(&mut e, "世a");
        assert_eq!(snap(&mut e).row_text(0), "a");
    }

    #[test]
    fn shrinking_past_a_wide_char_then_writing_does_not_panic() {
        // Regression: the wide char was left in the last column without its
        // continuation cell; the next write over it indexed past the row.
        let mut e = Emu::new(2, 4, 0);
        feed(&mut e, "a世");
        e.resize(2, 2);
        feed(&mut e, "\x1b[1;2Hx\x1b[1;2H\x1b[P");
        assert_eq!(snap(&mut e).row_text(0), "a");
    }

    #[test]
    fn huge_insert_counts_are_cheap() {
        // Regression: `CSI 65535 @` was O(count^2) under the emulator lock
        // (~1 s each in release), freezing the pane and the UI's snapshot.
        let mut e = Emu::new(24, 80, 0);
        let start = std::time::Instant::now();
        feed(
            &mut e,
            &"x\x1b[1;1H\x1b[65535@\x1b[65535L\x1b[65535T".repeat(20),
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(snap(&mut e).text(), "");
    }

    #[test]
    fn hostile_byte_soup_never_panics() {
        // Escape-heavy pseudo-random input (huge params, scroll regions,
        // inserts/deletes, invalid UTF-8) interleaved with resizes and
        // scrollback views: the child's output is untrusted.
        const PIECES: &[&[u8]] = &[
            b"\x1b[",
            b"\x1b]",
            b"\x1bP",
            b"\x07",
            b"\x1b\\",
            b";",
            b"?",
            b">",
            b"65535",
            b"0",
            b"999999999999",
            b"H",
            b"r",
            b"L",
            b"M",
            b"@",
            b"P",
            b"J",
            b"K",
            b"S",
            b"T",
            b"X",
            b"m",
            b"h",
            b"l",
            b"n",
            b"c",
            b"t",
            b" q",
            b"\x1b[?1049h",
            b"\x1b[?1049l",
            b"\x1b7",
            b"\x1b8",
            b"\x1bM",
            b"\x1bD",
            b"\r\n",
            b"\t",
            b"\x08",
            b"\xff\xfe",
            b"\xe4\xb8",
            b"\xe4\xb8\x96",
            "\u{301}".as_bytes(),
            b"x",
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut e = Emu::new(5, 8, 20);
        let mut out = empty_screen();
        let mut buf = Vec::new();
        for round in 0..2000 {
            buf.clear();
            for _ in 0..64 {
                buf.extend_from_slice(PIECES[next() as usize % PIECES.len()]);
            }
            let mut r = Vec::new();
            e.feed(&buf, &mut r);
            if round % 7 == 0 {
                e.resize((next() % 12) as u16, (next() % 30) as u16);
            }
            e.set_scroll_offset((next() % 40) as usize);
            e.snapshot_into(&mut out);
            assert_eq!(
                out.cells.len(),
                usize::from(out.rows) * usize::from(out.cols)
            );
        }
    }
}
