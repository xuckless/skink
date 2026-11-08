//! ratatui front end. `App` is pure state + `render`, so tests drive it with
//! synthetic key events against ratatui's `TestBackend` and compare the screen.

use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Position},
    style::{Modifier, Style},
    text::Line,
    widgets::Paragraph,
};
use skink_config::{Key, KeySpec, Keymap};
use skink_core::{Backend, CommandRegistry, Editor};
use std::io;
use unicode_width::UnicodeWidthChar;

const TAB_WIDTH: usize = 4;

pub struct App {
    pub editor: Editor,
    keymap: Keymap,
    registry: CommandRegistry,
    backend: Backend,
    hscroll: usize,
}

impl App {
    pub fn new(editor: Editor, keymap: Keymap, backend: Backend) -> Self {
        Self {
            editor,
            keymap,
            registry: CommandRegistry::builtin(),
            backend,
            hscroll: 0,
        }
    }

    pub fn should_quit(&self) -> bool {
        self.editor.quit
    }

    pub fn handle_key(&mut self, ev: KeyEvent) {
        // Windows reports key releases too; only act on presses/repeats.
        if ev.kind == KeyEventKind::Release {
            return;
        }
        if let Some(spec) = key_spec(ev) {
            if let Some(cmd) = self.keymap.command(&spec) {
                self.registry.run(cmd, &mut self.editor);
            }
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let [text_area, status_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
        self.editor.set_viewport_height(text_area.height as usize);
        let width = text_area.width as usize;

        let cur_line = self.editor.buffer.line_text(self.editor.cursor.line);
        let cursor_x = display_col(&cur_line, self.editor.cursor.col);
        if cursor_x < self.hscroll {
            self.hscroll = cursor_x;
        } else if width > 0 && cursor_x >= self.hscroll + width {
            self.hscroll = cursor_x + 1 - width;
        }

        let first = self.editor.scroll;
        let last = (first + text_area.height as usize).min(self.editor.buffer.len_lines());
        let lines: Vec<Line> = (first..last)
            .map(|i| Line::raw(clip(&self.editor.buffer.line_text(i), self.hscroll, width)))
            .collect();
        frame.render_widget(Paragraph::new(lines), text_area);

        let b = &self.editor.buffer;
        let name = b
            .path()
            .map_or_else(|| "[scratch]".to_owned(), |p| p.display().to_string());
        let ro = if b.is_read_only() { " [ro]" } else { "" };
        let host = match self.backend {
            Backend::Local => "",
            Backend::Delegated => "  EveryAI",
        };
        let c = self.editor.cursor;
        let status = format!(
            " {name}{ro}  Ln {}, Col {}  {}{host}",
            c.line + 1,
            c.col + 1,
            b.line_ending().label()
        );
        frame.render_widget(
            Paragraph::new(status).style(Style::new().add_modifier(Modifier::REVERSED)),
            status_area,
        );

        let x = text_area.x + (cursor_x - self.hscroll) as u16;
        let y = text_area.y + (c.line - self.editor.scroll) as u16;
        frame.set_cursor_position(Position::new(x, y));
    }
}

fn key_spec(ev: KeyEvent) -> Option<KeySpec> {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    let key = match ev.code {
        KeyCode::Char(c) => Key::Char(if ctrl || alt {
            c.to_ascii_lowercase()
        } else {
            c
        }),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Esc => Key::Escape,
        _ => return None,
    };
    // Shift is already folded into the char for printable keys.
    let shift = shift && !matches!(key, Key::Char(_));
    Some(KeySpec {
        ctrl,
        alt,
        shift,
        key,
    })
}

/// Display column of char index `col`, expanding tabs to tab stops.
fn display_col(line: &str, col: usize) -> usize {
    let mut x = 0;
    for c in line.chars().take(col) {
        x += char_width(c, x);
    }
    x
}

fn char_width(c: char, x: usize) -> usize {
    if c == '\t' {
        TAB_WIDTH - x % TAB_WIDTH
    } else {
        c.width().unwrap_or(0)
    }
}

/// The part of `line` visible in columns `[hscroll, hscroll + width)`.
fn clip(line: &str, hscroll: usize, width: usize) -> String {
    let mut out = String::new();
    let mut x = 0;
    for c in line.chars() {
        let w = char_width(c, x);
        let (start, end) = (x, x + w);
        x = end;
        if end <= hscroll {
            continue;
        }
        if start >= hscroll + width {
            break;
        }
        if start < hscroll || end > hscroll + width || c == '\t' {
            // Partially visible wide char or a tab: pad with spaces.
            let from = start.max(hscroll);
            let to = end.min(hscroll + width);
            out.extend(std::iter::repeat_n(' ', to - from));
        } else {
            out.push(c);
        }
    }
    out
}

/// Run the TUI. With `exit_after_first_frame` it draws once and returns
/// (used by the startup/RSS measurement script).
pub fn run(mut app: App, exit_after_first_frame: bool) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = (|| {
        loop {
            terminal.draw(|f| app.render(f))?;
            if exit_after_first_frame || app.should_quit() {
                return Ok(());
            }
            if let Event::Key(k) = event::read()? {
                app.handle_key(k);
            }
        }
    })();
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, crossterm::event::KeyEvent};
    use skink_core::Buffer;

    fn app(text: &str) -> App {
        App::new(
            Editor::new(Buffer::from_text(text)),
            Keymap::defaults(),
            Backend::Local,
        )
    }

    fn screen(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| app.render(f)).unwrap();
        let buf = t.backend().buffer();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) {
        app.handle_key(KeyEvent::new(code, mods));
    }

    #[test]
    fn renders_text_and_status() {
        let mut a = app("hello\nworld");
        let s = screen(&mut a, 30, 4);
        assert_eq!(s[0], "hello");
        assert_eq!(s[1], "world");
        assert_eq!(s[3], " [scratch]  Ln 1, Col 1  LF");
    }

    #[test]
    fn keys_move_cursor_and_update_status() {
        let mut a = app("hello\nworld");
        press(&mut a, KeyCode::Down, KeyModifiers::NONE);
        press(&mut a, KeyCode::Right, KeyModifiers::NONE);
        let s = screen(&mut a, 30, 4);
        assert_eq!(s[3], " [scratch]  Ln 2, Col 2  LF");
    }

    #[test]
    fn page_down_scrolls() {
        let text: String = (1..=50).map(|i| format!("line{i}\n")).collect();
        let mut a = app(&text);
        let _ = screen(&mut a, 20, 11); // 10 text rows
        press(&mut a, KeyCode::PageDown, KeyModifiers::NONE);
        press(&mut a, KeyCode::PageDown, KeyModifiers::NONE);
        let s = screen(&mut a, 20, 11);
        assert_eq!(s[9], "line21");
        assert!(s[10].contains("Ln 21"));
    }

    #[test]
    fn ctrl_q_quits() {
        let mut a = app("x");
        assert!(!a.should_quit());
        press(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert!(a.should_quit());
    }

    #[test]
    fn release_events_are_ignored() {
        let mut a = app("x");
        let mut ev = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        ev.kind = KeyEventKind::Release;
        a.handle_key(ev);
        assert!(!a.should_quit());
    }

    #[test]
    fn horizontal_scroll_keeps_cursor_visible() {
        let mut a = app("0123456789abcdefghij");
        press(&mut a, KeyCode::End, KeyModifiers::NONE);
        let s = screen(&mut a, 10, 2);
        assert_eq!(s[0], "bcdefghij");
    }

    #[test]
    fn tabs_expand_to_stops() {
        assert_eq!(display_col("\tx", 1), 4);
        assert_eq!(display_col("a\tx", 2), 4);
        assert_eq!(clip("a\tb", 0, 10), "a   b");
    }
}
