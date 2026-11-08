//! ratatui front end. `App` is state + `render`; the event loop in [`run`]
//! feeds it terminal events, and tests feed it synthetic ones against
//! ratatui's `TestBackend`.

mod clip;
mod input;
mod lsp;
mod termview;
mod text;

use clip::Clipboard;
use ratatui::{
    Frame,
    crossterm::{
        event::{
            self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
            EnableMouseCapture, Event, KeyEvent, KeyEventKind, MouseButton, MouseEvent,
            MouseEventKind,
        },
        execute,
    },
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use skink_config::Keymap;
use skink_core::{Backend, CommandRegistry, Cursor, Editor, Request};
use skink_term::{CommandSpec, MouseKind, Screen, Term, TermEvent, WakeFn};
use std::{
    io,
    sync::{Arc, mpsc},
};
use text::{char_col_at, display_col};

const SCROLLBACK: usize = 3000;
const WHEEL_LINES: isize = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Editor,
    Term,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PromptKind {
    SaveAs,
    Find,
    Command,
    QuitConfirm,
}

struct Prompt {
    kind: PromptKind,
    label: String,
    input: String,
}

struct TermPane {
    term: Term,
    screen: Screen,
    exited: bool,
}

#[derive(Default, Clone, Copy)]
struct Areas {
    editor: Rect,
    term: Rect,
}

pub struct App {
    pub editor: Editor,
    keymap: Keymap,
    registry: CommandRegistry,
    backend: Backend,
    hscroll: usize,
    prompt: Option<Prompt>,
    pane: Option<TermPane>,
    focus: Focus,
    areas: Areas,
    clip: Clipboard,
    wake: Option<WakeFn>,
    quit: bool,
    osc52: bool,
    lsp: Option<lsp::LspLink>,
    lsp_requested: bool,
    lsp_rev: u64,
    lsp_dirty: bool,
    was_dirty: bool,
    diag_status: (usize, usize),
    diag_here: Option<String>,
}

impl App {
    pub fn new(editor: Editor, keymap: Keymap, backend: Backend) -> Self {
        Self {
            editor,
            keymap,
            registry: CommandRegistry::builtin(),
            backend,
            hscroll: 0,
            prompt: None,
            pane: None,
            focus: Focus::Editor,
            areas: Areas::default(),
            clip: Clipboard::new(),
            wake: None,
            quit: false,
            osc52: false,
            lsp: None,
            lsp_requested: false,
            lsp_rev: 0,
            lsp_dirty: false,
            was_dirty: false,
            diag_status: (0, 0),
            diag_here: None,
        }
    }

    /// Called when terminal output arrives, to wake the event loop.
    pub fn set_wake(&mut self, wake: WakeFn) {
        self.wake = Some(wake);
    }

    /// Start language-server support for the open file (servers spawn
    /// lazily and only if installed). Needs the wake callback set first.
    pub fn request_lsp(&mut self) {
        self.lsp_requested = true;
    }

    fn enable_lsp(&mut self) {
        let Some(path) = self.editor.buffer.path().map(Into::into) else {
            return;
        };
        let path: std::path::PathBuf = path;
        if path.is_dir() || self.wake.is_none() {
            return;
        }
        let root = self.cwd().unwrap_or_else(|| std::path::PathBuf::from("."));
        let wake = self.wake.clone();
        let link = lsp::LspLink::start(root, move || {
            if let Some(w) = &wake {
                w();
            }
        });
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        link.open(&path, self.editor.buffer.text());
        self.lsp_rev = self.editor.buffer.revision();
        self.lsp = Some(link);
    }

    /// True while an edit has not been sent to the language server yet.
    pub fn lsp_pending(&self) -> bool {
        self.lsp.is_some() && self.lsp_dirty
    }

    /// Send the latest text (called after the typing pause).
    pub fn flush_lsp(&mut self) {
        let (Some(link), Some(path)) = (&self.lsp, self.editor.buffer.path()) else {
            return;
        };
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if self.lsp_dirty {
            link.change(&path, self.editor.buffer.text());
            self.lsp_dirty = false;
        }
        let dirty = self.editor.buffer.is_dirty();
        if self.was_dirty && !dirty {
            link.save(&path);
        }
        self.was_dirty = dirty;
        link.drain();
    }

    fn track_changes(&mut self) {
        let rev = self.editor.buffer.revision();
        if rev != self.lsp_rev {
            self.lsp_rev = rev;
            self.lsp_dirty = true;
        }
        if self.was_dirty && !self.editor.buffer.is_dirty() {
            self.lsp_dirty = true; // a save: flush the text, then notify
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    // ---- terminal pane ----------------------------------------------------

    /// Open (or replace) the terminal pane running `spec`, and focus it.
    pub fn open_terminal(&mut self, spec: CommandSpec) -> io::Result<()> {
        self.close_terminal();
        let spec = match self.cwd() {
            Some(dir) => spec.cwd(dir),
            None => spec,
        };
        let (rows, cols) = (
            self.areas.term.height.saturating_sub(1).max(8),
            self.areas.term.width.max(20),
        );
        let term = Term::spawn_with_wake(spec, rows, cols, SCROLLBACK, self.wake.clone())?;
        let screen = term.screen();
        self.pane = Some(TermPane {
            term,
            screen,
            exited: false,
        });
        self.focus = Focus::Term;
        Ok(())
    }

    pub fn close_terminal(&mut self) {
        self.pane = None; // Term's Drop reaps the child
        self.focus = Focus::Editor;
    }

    fn cwd(&self) -> Option<std::path::PathBuf> {
        let p = self.editor.buffer.path()?;
        if p.is_dir() {
            Some(p.to_path_buf())
        } else {
            p.parent().filter(|d| d.is_dir()).map(Into::into)
        }
    }

    /// Drain terminal events; returns true if something changed on screen.
    pub fn poll_terminal(&mut self) -> bool {
        let Some(pane) = &mut self.pane else {
            return false;
        };
        let mut changed = false;
        while let Ok(ev) = pane.term.events().try_recv() {
            changed = true;
            if matches!(ev, TermEvent::Exited) {
                pane.exited = true;
            }
        }
        // Apps inside the pane (e.g. an AI CLI) may ask to set the clipboard.
        for req in pane.term.take_clipboard_requests() {
            if req.data_base64 != "?" {
                clip::write_osc52(&req.data_base64);
            }
        }
        changed
    }

    // ---- input -------------------------------------------------------------

    pub fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) => self.handle_key(k),
            Event::Mouse(m) => self.handle_mouse(m),
            Event::Paste(s) => self.handle_paste(&s),
            _ => {}
        }
        self.track_changes();
    }

    pub fn handle_paste(&mut self, s: &str) {
        if let Some(p) = &mut self.prompt {
            p.input.push_str(&s.replace(['\n', '\r'], " "));
        } else if self.focus == Focus::Term {
            if let Some(pane) = &self.pane {
                let _ = pane.term.paste(s);
            }
        } else {
            self.editor.insert_text(s);
        }
    }

    pub fn handle_key(&mut self, ev: KeyEvent) {
        // Windows reports key releases too; only act on presses/repeats.
        if ev.kind == KeyEventKind::Release {
            return;
        }
        if self.prompt.is_some() {
            return self.prompt_key(ev);
        }
        let Some(spec) = input::key_spec(ev) else {
            return;
        };

        if spec.key == skink_config::Key::F(12) {
            return self.toggle_terminal_focus();
        }
        if spec.key == skink_config::Key::F(11) {
            return self.close_terminal();
        }

        if self.focus == Focus::Term {
            if let Some(pane) = &mut self.pane {
                if pane.exited {
                    return self.close_terminal();
                }
                let _ = pane.term.send_key(spec);
                return;
            }
        }

        match self.keymap.command(&spec).map(str::to_owned) {
            Some(chain) => self.run_chain(&chain),
            None => {
                if let skink_config::Key::Char(c) = spec.key {
                    if !spec.ctrl && !spec.alt {
                        self.editor.insert_char(c);
                    }
                }
            }
        }
    }

    fn toggle_terminal_focus(&mut self) {
        if self.pane.is_none() {
            let _ = self.open_terminal(CommandSpec::default_shell());
            return;
        }
        self.focus = if self.focus == Focus::Term {
            Focus::Editor
        } else {
            Focus::Term
        };
    }

    /// Run a Micro action string, then act on whatever the editor asked for.
    pub fn run_chain(&mut self, chain: &str) {
        if let Some(rest) = chain.strip_prefix("command-edit:") {
            return self.open_prompt(PromptKind::Command, ":", rest);
        }
        if chain.contains("Paste") {
            if let Some(t) = self.clip.get() {
                self.editor.clipboard = t;
            }
        }
        self.registry.run_chain(chain, &mut self.editor);
        if self.editor.clipboard_changed {
            self.editor.clipboard_changed = false;
            self.clip.set(&self.editor.clipboard, self.osc52);
        }
        match self.editor.request.take() {
            Some(Request::Quit) => self.request_quit(),
            Some(Request::SaveAs) => self.open_prompt(PromptKind::SaveAs, "Save as: ", ""),
            Some(Request::Find) => {
                let q = self.editor.last_search.clone();
                self.open_prompt(PromptKind::Find, "Find: ", &q);
            }
            None => {}
        }
    }

    fn request_quit(&mut self) {
        if self.editor.buffer.is_dirty() {
            self.open_prompt(
                PromptKind::QuitConfirm,
                "Save changes before quitting? (y)es / (n)o / Esc ",
                "",
            );
        } else {
            self.quit = true;
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, label: &str, input: &str) {
        self.prompt = Some(Prompt {
            kind,
            label: label.to_owned(),
            input: input.to_owned(),
        });
    }

    fn prompt_key(&mut self, ev: KeyEvent) {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};
        let Some(p) = &mut self.prompt else { return };
        if p.kind == PromptKind::QuitConfirm {
            let answer = match ev.code {
                KeyCode::Char('y' | 'Y') => Some(true),
                KeyCode::Char('n' | 'N') => Some(false),
                _ => None,
            };
            self.prompt = None;
            match answer {
                Some(true) => {
                    self.editor.save();
                    if self.editor.request.take() == Some(Request::SaveAs) {
                        self.open_prompt(PromptKind::SaveAs, "Save as: ", "");
                    } else if !self.editor.buffer.is_dirty() {
                        self.quit = true;
                    }
                }
                Some(false) => self.quit = true,
                None => {}
            }
            return;
        }
        match ev.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                if let Some(p) = self.prompt.take() {
                    self.finish_prompt(p);
                }
            }
            KeyCode::Backspace => {
                p.input.pop();
            }
            KeyCode::Char('u') if ev.modifiers.contains(KeyModifiers::CONTROL) => p.input.clear(),
            KeyCode::Char(c) if !ev.modifiers.contains(KeyModifiers::CONTROL) => p.input.push(c),
            _ => {}
        }
    }

    fn finish_prompt(&mut self, p: Prompt) {
        match p.kind {
            PromptKind::SaveAs => {
                if !p.input.trim().is_empty() {
                    self.editor.buffer.set_path(p.input.trim().into());
                    self.editor.save();
                }
            }
            PromptKind::Find => {
                self.editor.find(&p.input, true);
            }
            PromptKind::Command => self.run_command(p.input.trim()),
            PromptKind::QuitConfirm => {}
        }
    }

    /// Micro-style command bar: `goto 12[:3]`, `save`, `quit`, `term [cmd..]`.
    pub fn run_command(&mut self, line: &str) {
        let mut parts = line.split_whitespace();
        let Some(cmd) = parts.next() else { return };
        let args: Vec<&str> = parts.collect();
        match cmd {
            "goto" => {
                let mut it = args.first().copied().unwrap_or("").split(':');
                let l = it.next().and_then(|s| s.parse().ok());
                let c = it.next().and_then(|s| s.parse().ok());
                if l.is_some() {
                    self.editor.goto(l, c);
                } else {
                    self.editor.message = Some("usage: goto LINE[:COL]".into());
                }
            }
            "save" => self.run_chain("Save"),
            "quit" | "q" => self.run_chain("Quit"),
            "term" | "terminal" | "agent" => {
                let spec = match args.split_first() {
                    Some((prog, rest)) => CommandSpec::new(*prog).args(rest.iter().copied()),
                    None => CommandSpec::default_shell(),
                };
                if let Err(e) = self.open_terminal(spec) {
                    self.editor.message = Some(format!("terminal: {e}"));
                }
            }
            other => self.editor.message = Some(format!("Unknown command: {other}")),
        }
    }

    pub fn handle_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        if self.pane.is_some() && self.areas.term.contains(pos) {
            self.focus = Focus::Term;
            let local_row = m.row - self.areas.term.y.saturating_sub(0);
            // Row 0 of the pane area is the title bar.
            let (col, row) = (m.column - self.areas.term.x, local_row.saturating_sub(1));
            let kind = match m.kind {
                MouseEventKind::Down(b) => Some(MouseKind::Down(map_button(b))),
                MouseEventKind::Up(b) => Some(MouseKind::Up(map_button(b))),
                MouseEventKind::Drag(b) => Some(MouseKind::Drag(map_button(b))),
                MouseEventKind::Moved => Some(MouseKind::Move),
                MouseEventKind::ScrollUp => Some(MouseKind::ScrollUp),
                MouseEventKind::ScrollDown => Some(MouseKind::ScrollDown),
                _ => None,
            };
            if let (Some(kind), Some(pane)) = (kind, &self.pane) {
                let ev = skink_term::MouseEvent::new(kind, col, row);
                let handled = pane.term.send_mouse(ev).unwrap_or(false);
                if !handled {
                    match kind {
                        MouseKind::ScrollUp => pane.term.scroll_up(WHEEL_LINES as usize),
                        MouseKind::ScrollDown => pane.term.scroll_down(WHEEL_LINES as usize),
                        _ => {}
                    }
                }
            }
            return;
        }
        if !self.areas.editor.contains(pos) {
            return;
        }
        match m.kind {
            MouseEventKind::ScrollUp => self.editor.scroll_by(-WHEEL_LINES),
            MouseEventKind::ScrollDown => self.editor.scroll_by(WHEEL_LINES),
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
                self.focus = Focus::Editor;
                let gutter = self.gutter_width() as u16;
                let row = (m.row - self.areas.editor.y) as usize + self.editor.scroll;
                let line = row.min(self.editor.buffer.len_lines() - 1);
                let x =
                    (m.column.saturating_sub(self.areas.editor.x + gutter)) as usize + self.hscroll;
                let col = char_col_at(
                    &self.editor.buffer.line_text(line),
                    x,
                    self.editor.tab_width,
                );
                let extend = matches!(m.kind, MouseEventKind::Drag(_));
                self.editor.click(line, col, extend);
            }
            _ => {}
        }
    }

    // ---- render --------------------------------------------------------------

    fn gutter_width(&self) -> usize {
        self.editor.buffer.len_lines().to_string().len() + 2
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let full = frame.area();
        let [body, status_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(full);
        let (editor_area, term_area) = if self.pane.is_some() {
            let th = (body.height * 2 / 5)
                .max(6)
                .min(body.height.saturating_sub(3));
            let [e, t] = Layout::vertical([Constraint::Min(2), Constraint::Length(th)]).areas(body);
            (e, t)
        } else {
            (body, Rect::default())
        };
        self.areas = Areas {
            editor: editor_area,
            term: term_area,
        };

        self.render_editor(frame, editor_area);
        if self.pane.is_some() {
            self.render_terminal(frame, term_area);
        }
        self.render_status(frame, status_area);
    }

    fn render_editor(&mut self, frame: &mut Frame, area: Rect) {
        self.editor.set_viewport_height(area.height as usize);
        let gutter = self.gutter_width();
        let width = (area.width as usize).saturating_sub(gutter);
        let tab = self.editor.tab_width;

        let cur_line = self.editor.buffer.line_text(self.editor.cursor.line);
        let cursor_x = display_col(&cur_line, self.editor.cursor.col, tab);
        if cursor_x < self.hscroll {
            self.hscroll = cursor_x;
        } else if width > 0 && cursor_x >= self.hscroll + width {
            self.hscroll = cursor_x + 1 - width;
        }

        let diags = match (&self.lsp, self.editor.buffer.path()) {
            (Some(l), Some(p)) => {
                l.diagnostics(&std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
            }
            _ => Vec::new(),
        };
        let (diag_lines, n_err, n_warn) = lsp::summarize(&diags);
        self.diag_status = (n_err, n_warn);
        self.diag_here = diags
            .iter()
            .find(|d| d.range.start.line == self.editor.cursor.line)
            .map(|d| d.message.lines().next().unwrap_or("").to_owned());
        let sel = self
            .editor
            .selection()
            .map(|(s, e)| (self.editor.cursor_at(s), self.editor.cursor_at(e)));
        let first = self.editor.scroll;
        let last = (first + area.height as usize).min(self.editor.buffer.len_lines());
        let mut lines = Vec::with_capacity(last - first);
        for i in first..last {
            let text = self.editor.buffer.line_text(i);
            let range = sel.and_then(|(s, e)| line_selection(i, s, e, text.chars().count()));
            let (sign, sign_style) = match diag_lines.get(&i) {
                Some(skink_lsp::Severity::Error) => ("E", Style::new().fg(Color::Red)),
                Some(skink_lsp::Severity::Warning) => ("W", Style::new().fg(Color::Yellow)),
                Some(_) => ("i", Style::new().fg(Color::Cyan)),
                None => (" ", Style::new()),
            };
            let mut spans = vec![
                Span::styled(sign, sign_style),
                Span::styled(
                    format!("{:>w$} ", i + 1, w = gutter - 2),
                    Style::new().fg(Color::DarkGray),
                ),
            ];
            spans.extend(text::spans(&text, self.hscroll, width, tab, range));
            lines.push(Line::from(spans));
        }
        frame.render_widget(Paragraph::new(lines), area);

        if self.focus == Focus::Editor && self.prompt.is_none() {
            let x = area.x + (gutter + cursor_x - self.hscroll) as u16;
            let y = area.y + (self.editor.cursor.line - self.editor.scroll) as u16;
            frame.set_cursor_position(Position::new(x, y));
        }
    }

    fn render_terminal(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Term;
        let Some(pane) = &mut self.pane else { return };
        let inner = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height.saturating_sub(1),
        };
        if pane.term.size() != (inner.height, inner.width) && inner.height > 0 && inner.width > 0 {
            let _ = pane.term.resize(inner.height, inner.width);
        }
        pane.term.snapshot_into(&mut pane.screen);
        let title = if pane.exited {
            " terminal [exited - press a key to close] ".to_owned()
        } else if pane.screen.title.is_empty() {
            " terminal ".to_owned()
        } else {
            format!(" terminal: {} ", pane.screen.title)
        };
        let bar_style = if focused {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().fg(Color::DarkGray)
        };
        let bar = format!("{title:─<w$}", w = area.width as usize);
        frame.render_widget(
            Paragraph::new(bar).style(bar_style),
            Rect { height: 1, ..area },
        );
        termview::render(frame.buffer_mut(), inner, &pane.screen);
        if focused && self.prompt.is_none() && pane.screen.cursor.visible && !pane.exited {
            let c = pane.screen.cursor;
            if c.col < inner.width && c.row < inner.height {
                frame.set_cursor_position(Position::new(inner.x + c.col, inner.y + c.row));
            }
        }
    }

    fn render_status(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(p) = &self.prompt {
            let text = format!("{}{}", p.label, p.input);
            frame.render_widget(Paragraph::new(text.clone()), area);
            if p.kind != PromptKind::QuitConfirm {
                let x = area.x + text.chars().count() as u16;
                frame.set_cursor_position(Position::new(
                    x.min(area.right().saturating_sub(1)),
                    area.y,
                ));
            }
            return;
        }
        let b = &self.editor.buffer;
        let name = b
            .path()
            .map_or_else(|| "[scratch]".to_owned(), |p| p.display().to_string());
        let flag = match (b.is_read_only(), b.is_dirty()) {
            (true, _) => " [ro]",
            (_, true) => " *",
            _ => "",
        };
        let host = match self.backend {
            Backend::Local => "",
            Backend::Delegated => "  EveryAI",
        };
        let c: Cursor = self.editor.cursor;
        let hint = if self.pane.is_some() {
            "  F12 switch  F11 close"
        } else {
            "  F12 terminal"
        };
        let status = match &self.editor.message {
            Some(m) => format!(" {m}"),
            None => format!(
                " {name}{flag}  Ln {}, Col {}  {}{host}{hint}",
                c.line + 1,
                c.col + 1,
                b.line_ending().label()
            ),
        };
        frame.render_widget(
            Paragraph::new(status).style(Style::new().add_modifier(Modifier::REVERSED)),
            area,
        );
    }
}

fn map_button(b: MouseButton) -> skink_term::MouseButton {
    match b {
        MouseButton::Left => skink_term::MouseButton::Left,
        MouseButton::Right => skink_term::MouseButton::Right,
        MouseButton::Middle => skink_term::MouseButton::Middle,
    }
}

/// Selected char-column range on `line`, if any (end is exclusive).
fn line_selection(line: usize, s: Cursor, e: Cursor, len: usize) -> Option<(usize, usize)> {
    if line < s.line || line > e.line {
        return None;
    }
    let from = if line == s.line { s.col } else { 0 };
    let to = if line == e.line { e.col } else { len + 1 };
    (from < to).then_some((from, to))
}

enum Msg {
    Input(Event),
    Wake,
    InputClosed,
}

/// Run the TUI. With `exit_after_first_frame` it draws once and returns
/// (used by the startup/RSS measurement script).
pub fn run(mut app: App, exit_after_first_frame: bool) -> io::Result<()> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let wake_tx = tx.clone();
    app.set_wake(Arc::new(move || {
        let _ = wake_tx.send(Msg::Wake);
    }));
    app.osc52 = true;
    if app.lsp_requested {
        app.enable_lsp();
    }

    let mut terminal = ratatui::init();
    let _ = execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    if !exit_after_first_frame {
        std::thread::spawn(move || {
            while let Ok(ev) = event::read() {
                if tx.send(Msg::Input(ev)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Msg::InputClosed);
        });
    }
    let result = (|| {
        loop {
            terminal.draw(|f| app.render(f))?;
            if exit_after_first_frame || app.should_quit() {
                return Ok(());
            }
            // Block until something happens (zero idle CPU), then batch
            // everything queued so a burst costs one redraw.
            let mut msg = if app.lsp_pending() {
                match rx.recv_timeout(std::time::Duration::from_millis(250)) {
                    Ok(m) => Some(m),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        app.flush_lsp();
                        continue;
                    }
                    Err(_) => None,
                }
            } else {
                rx.recv().ok()
            };
            while let Some(m) = msg {
                match m {
                    Msg::Input(ev) => app.handle_event(ev),
                    Msg::Wake => {}
                    Msg::InputClosed => app.quit = true,
                }
                msg = rx.try_recv().ok();
            }
            app.poll_terminal();
            if app.lsp.is_some() && !app.lsp_dirty {
                app.flush_lsp();
            }
        }
    })();
    let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests;
