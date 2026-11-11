//! Jump list (back / forward over big moves), toasts, scrollbar marks.

use crate::App;
use crate::view::Diags;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

/// A place in a file (0-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
}

const JUMPS_KEPT: usize = 100;

/// Where you were before jumps (definitions, results, quick open, goto).
#[derive(Debug, Default)]
pub struct JumpList {
    back: Vec<Mark>,
    forward: Vec<Mark>,
}

impl JumpList {
    /// Remember `here` before a jump; a new jump drops the forward list.
    pub fn record(&mut self, here: Mark) {
        if self
            .back
            .last()
            .is_some_and(|m| m.path == here.path && m.line == here.line)
        {
            return;
        }
        self.back.push(here);
        if self.back.len() > JUMPS_KEPT {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    pub fn back(&mut self, here: Option<Mark>) -> Option<Mark> {
        let to = self.back.pop()?;
        self.forward.extend(here);
        Some(to)
    }

    pub fn forward(&mut self, here: Option<Mark>) -> Option<Mark> {
        let to = self.forward.pop()?;
        self.back.extend(here);
        Some(to)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub level: Level,
    pub until: Instant,
}

const TOAST_FOR: Duration = Duration::from_secs(4);
const TOASTS_SHOWN: usize = 3;
const MESSAGES_KEPT: usize = 100;

impl App {
    fn here(&self) -> Option<Mark> {
        let c = self.doc.editor.cursor;
        self.doc.canon.clone().map(|path| Mark {
            path,
            line: c.line,
            col: c.col,
        })
    }

    /// Call before a jump so Alt-U can come back.
    pub(crate) fn record_jump(&mut self) {
        if let Some(m) = self.here() {
            self.jumps.record(m);
        }
    }

    fn go_to_mark(&mut self, m: &Mark) {
        if self.doc.canon.as_ref() != Some(&m.path) {
            self.open_path(&m.path);
        }
        self.doc.editor.goto(Some(m.line + 1), Some(m.col + 1));
        self.doc.editor.scroll_into_view();
    }

    /// Alt-U: back to where you were before the last jump.
    pub fn jump_back(&mut self) {
        let here = self.here();
        loop {
            match self.jumps.back(here.clone()) {
                Some(m) if m.path.is_file() => return self.go_to_mark(&m),
                Some(_) => continue, // deleted since
                None => {
                    self.doc.editor.message = Some("No earlier position".into());
                    return;
                }
            }
        }
    }

    /// Alt-Shift-U: forward again.
    pub fn jump_forward(&mut self) {
        let here = self.here();
        loop {
            match self.jumps.forward(here.clone()) {
                Some(m) if m.path.is_file() => return self.go_to_mark(&m),
                Some(_) => continue,
                None => {
                    self.doc.editor.message = Some("No later position".into());
                    return;
                }
            }
        }
    }

    // ---- toasts ------------------------------------------------------------

    /// A short note in the top-right corner that goes away by itself.
    pub(crate) fn toast(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        if self.messages.len() == MESSAGES_KEPT {
            self.messages.pop_front();
        }
        self.messages.push_back(text.clone());
        self.toasts.push(Toast {
            text,
            level,
            until: Instant::now() + TOAST_FOR,
        });
        if self.toasts.len() > TOASTS_SHOWN {
            self.toasts.remove(0);
        }
    }

    /// `:messages`
    pub(crate) fn show_messages(&mut self) {
        let mut text = String::from("Messages, newest last\n\n");
        if self.messages.is_empty() {
            text.push_str("  None yet.\n");
        }
        for m in &self.messages {
            text.push_str(m);
            text.push('\n');
        }
        self.open_view("Messages", &text);
    }

    /// Time until the next toast expires.
    pub(crate) fn toast_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        self.toasts
            .iter()
            .map(|t| t.until.saturating_duration_since(now))
            .min()
    }

    pub(crate) fn expire_toasts(&mut self) {
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);
    }

    pub(crate) fn render_toasts(&self, frame: &mut Frame) {
        let t = &self.theme;
        let area = self.areas.main;
        // At most two thirds of the width and one line, so a long message
        // (a script's, a server's) never hides the text under it.
        let max = (area.width.saturating_mul(2) / 3).max(40.min(area.width));
        for (i, toast) in self.toasts.iter().rev().enumerate() {
            // Display width (CJK, emoji), cut with `…` to fit; `:messages`
            // has the whole text.
            let first = toast.text.lines().next().unwrap_or("");
            let text = crate::view::truncate(first, usize::from(max).saturating_sub(4));
            let width = (unicode_width::UnicodeWidthStr::width(text.as_str()) as u16 + 4).min(max);
            let y = area.y + i as u16;
            if width < 6 || y >= area.bottom() {
                break;
            }
            let r = Rect {
                x: area.right() - width,
                y,
                width,
                height: 1,
            };
            let bar = match toast.level {
                Level::Info => t.blue,
                Level::Warn => t.yellow,
                Level::Error => t.red,
            };
            frame.render_widget(Clear, r);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("\u{258c} ", Style::new().fg(t.c(bar)).bg(t.c(t.bg_popup))),
                    Span::styled(
                        format!("{text} "),
                        Style::new().fg(t.c(t.fg)).bg(t.c(t.bg_popup)),
                    ),
                ])),
                r,
            );
        }
    }

    // ---- scrollbar marks ---------------------------------------------------

    /// Marks in the editor's last column for where diagnostics, find matches
    /// and the cursor are in the whole file.
    pub(crate) fn render_scroll_marks(
        &self,
        frame: &mut Frame,
        area: Rect,
        diags: &Diags,
        find_lines: &[usize],
    ) {
        let ed = &self.doc.editor;
        let total = ed.buffer.len_lines();
        let h = area.height as usize;
        if total <= h || area.width < 10 || h == 0 {
            return;
        }
        let t = &self.theme;
        let row = |line: usize| (line * h / total).min(h - 1);
        let mut marks: Vec<(usize, ratatui::style::Color)> = Vec::new();
        for &line in find_lines {
            marks.push((row(line), t.c(t.orange)));
        }
        for (&line, sev) in diags.severities() {
            let c = match sev {
                skink_lsp::Severity::Error => t.red,
                skink_lsp::Severity::Warning => t.yellow,
                _ => continue,
            };
            marks.push((row(line), t.c(c)));
        }
        for (line, c) in self.git_scroll_marks() {
            marks.push((row(line), t.c(c)));
        }
        marks.push((row(ed.cursor.line), t.c(t.blue)));
        let x = area.right() - 1;
        let buf = frame.buffer_mut();
        for (r, color) in marks {
            if let Some(cell) = buf.cell_mut(Position::new(x, area.y + r as u16)) {
                cell.set_symbol("\u{2590}").set_fg(color);
            }
        }
    }
}
