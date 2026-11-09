//! Terminal polish: the history view (scrollback search and keyboard copy
//! mode are the editor's own find and selection on a read-only copy),
//! `file:line[:col]` links (Enter there, Ctrl-click in the live terminal),
//! and OSC 133 shell integration: previous / next prompt, select the last
//! command's output, and exact "last output" for Alt-Shift-E.

use crate::doc::{DocId, Jump};
use crate::{App, Tab, TermPane};
use skink_term::{History, Mark, MarkKind, Term};
use std::path::{Path, PathBuf};

/// The open history view of a terminal, if any.
#[derive(Debug, Default)]
pub enum HistoryView {
    #[default]
    Closed,
    Open {
        doc: DocId,
        /// Lines (0-based) where prompts start.
        prompts: Vec<usize>,
    },
}

/// What to select when the history view opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Cursor on the last line.
    End,
    /// The last finished command's output selected.
    LastOutput,
}

/// History rows joined into logical lines, and the line of each row.
fn logical(h: &History) -> (Vec<String>, Vec<usize>) {
    let mut lines = vec![String::new()];
    let mut line_of = Vec::with_capacity(h.rows.len());
    for (t, wrapped) in &h.rows {
        line_of.push(lines.len() - 1);
        let cur = lines.last_mut().expect("never empty");
        if *wrapped {
            cur.push_str(t);
        } else {
            cur.push_str(t.trim_end());
            lines.push(String::new());
        }
    }
    if lines.last().is_some_and(String::is_empty) && lines.len() > 1 {
        lines.pop();
    }
    (lines, line_of)
}

/// Absolute rows of the last command: (command line, output start, end).
/// Why a terminal has no prompt marks, from how its shell was started.
fn no_marks_why(p: &TermPane) -> String {
    use skink_term::Integration;
    match &p.integration {
        Integration::Injected(_) | Integration::Native(_) => {
            "the shell has not finished a prompt yet".into()
        }
        Integration::Unsupported(prog) => {
            format!("{prog} has no shell integration (bash, zsh, fish and PowerShell do)")
        }
        Integration::Off => {
            "shell integration is off for this terminal (\"terminal.shellintegration\")".into()
        }
    }
}

fn last_command_rows(marks: &[Mark], end_row: u64) -> Option<(u64, u64, u64)> {
    let c = marks.iter().rposition(|m| m.kind == MarkKind::Output)?;
    let done = marks[c..]
        .iter()
        .find(|m| matches!(m.kind, MarkKind::Done(_)))
        .map_or(end_row, |m| m.row);
    let input = marks[..c]
        .iter()
        .rev()
        .find(|m| matches!(m.kind, MarkKind::Input | MarkKind::Prompt))
        .map_or(marks[c].row, |m| m.row);
    Some((input, marks[c].row, done))
}

/// The last command line and its output, from shell-integration marks.
pub(crate) fn exact_last_output(term: &Term) -> Option<String> {
    let marks = term.marks();
    let h = term.history();
    let end = h.first + h.rows.len() as u64;
    let (input, _, done) = last_command_rows(&marks, end)?;
    let text = h.text_between(input, done);
    (!text.trim().is_empty()).then(|| text.trim_end().to_owned())
}

/// Strip what usually wraps a path in output: quotes, brackets, `-->`.
fn trim_token(t: &str) -> &str {
    t.trim_matches(|c: char| "\"'`()[]{}<>,;".contains(c))
}

/// A `file:line[:col]` link in `token` that exists under `cwd`.
pub(crate) fn link_in(token: &str, cwd: &Path) -> Option<Jump> {
    let t = trim_token(token);
    let (path, line, col, _) = crate::tasks::file_line_col(t)?;
    let p = cwd.join(path);
    if !p.is_file() || line == 0 {
        return None;
    }
    Some(Jump {
        path: p,
        line: line - 1,
        col: col.saturating_sub(1),
    })
}

/// The first link on a line of output.
fn first_link(line: &str, cwd: &Path) -> Option<Jump> {
    // Python: File "x.py", line 3
    if let Some(rest) = line.trim_start().strip_prefix("File \"")
        && let Some((p, rest)) = rest.split_once("\", line ")
    {
        let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let (Ok(n), true) = (n.parse::<usize>(), cwd.join(p).is_file()) {
            return Some(Jump {
                path: cwd.join(p),
                line: n.saturating_sub(1),
                col: 0,
            });
        }
    }
    line.split_whitespace().find_map(|t| link_in(t, cwd))
}

/// The whitespace-separated word of `line` at char column `col`.
fn word_at(line: &str, col: usize) -> Option<&str> {
    let mut start = 0;
    for (i, (b, c)) in line.char_indices().enumerate() {
        if c.is_whitespace() {
            if i == col {
                return None;
            }
            if i > col {
                return Some(&line[start..b]).filter(|w| !w.is_empty());
            }
            start = b + c.len_utf8();
        }
    }
    Some(&line[start..]).filter(|w| !w.is_empty())
}

impl App {
    /// The working folder of a terminal tab (for relative links).
    fn pane_cwd(&self, p: &TermPane) -> PathBuf {
        match (&p.purpose, &p.shell_cwd) {
            (crate::Purpose::Task(run), _) => run.task.cwd.clone(),
            (_, Some(start)) => crate::session::shell_cwd(p.term.process_id(), start),
            _ => p.cwd.clone(),
        }
    }

    /// The active terminal tab, else the most recent one.
    fn some_term(&self) -> Option<usize> {
        match self.tabs.get(self.active) {
            Some(Tab::Term(_)) => Some(self.active),
            _ => (0..self.tabs.len())
                .rev()
                .find(|&i| matches!(self.tabs[i], Tab::Term(_))),
        }
    }

    /// Shift-F11: the terminal's whole history in a read-only tab: Ctrl-F
    /// searches it, Shift+arrows select, Ctrl-C copies, Enter opens a
    /// `file:line` under the cursor.
    pub(crate) fn terminal_history(&mut self, focus: Focus) {
        let Some(i) = self.some_term() else {
            self.doc.editor.message = Some("No terminal tab".into());
            return;
        };
        let Tab::Term(p) = &self.tabs[i] else { return };
        let h = p.term.history();
        let marks = p.term.marks();
        let cwd = self.pane_cwd(p);
        let title = format!("Terminal history: {}", p.label);
        let (lines, line_of) = logical(&h);
        let line_at = |row: u64| {
            let r = row.saturating_sub(h.first) as usize;
            line_of
                .get(r)
                .copied()
                .unwrap_or(lines.len().saturating_sub(1))
        };
        let prompts: Vec<usize> = marks
            .iter()
            .filter(|m| m.kind == MarkKind::Prompt)
            .map(|m| line_at(m.row))
            .collect();
        let last = last_command_rows(&marks, h.first + h.rows.len() as u64)
            .map(|(_, out, done)| (line_at(out), line_at(done)));
        if focus == Focus::LastOutput && last.is_none() {
            self.doc.editor.message =
                Some(format!("No command output marked yet: {}", no_marks_why(p)));
            return;
        }
        let rows: Vec<(String, Option<Jump>)> = lines
            .into_iter()
            .map(|l| {
                let j = first_link(&l, &cwd);
                (l, j)
            })
            .collect();
        let n = rows.len();
        self.open_results(&title, rows);
        self.term_history = HistoryView::Open {
            doc: self.doc.id,
            prompts,
        };
        let ed = &mut self.doc.editor;
        match (focus, last) {
            (Focus::LastOutput, Some((from, to))) => {
                let to = to.max(from + 1).min(n);
                if to >= n {
                    ed.move_file_end(false);
                } else {
                    ed.goto(Some(to + 1), Some(1));
                }
                ed.anchor = Some(skink_core::Cursor { line: from, col: 0 });
                self.doc.editor.message = Some("Last output selected · Ctrl-C copies".into());
            }
            _ => {
                ed.move_file_end(false);
                self.doc.editor.message =
                    Some("Terminal history · Ctrl-F search · Shift+arrows select · Ctrl-C copy · Enter opens file:line".into());
            }
        }
    }

    /// Shift-F2 / Shift-F3: the previous / next shell prompt, in the live
    /// terminal (scrolls its view) or in its history view.
    pub(crate) fn jump_prompt(&mut self, forward: bool) {
        if let HistoryView::Open { doc, prompts } = &self.term_history
            && *doc == self.doc.id
            && !self.active_is_term()
        {
            let cur = self.doc.editor.cursor.line;
            let next = if forward {
                prompts.iter().find(|&&l| l > cur)
            } else {
                prompts.iter().rev().find(|&&l| l < cur)
            };
            match next {
                Some(&l) => self.doc.editor.goto(Some(l + 1), Some(1)),
                None => self.doc.editor.message = Some("No more prompts".into()),
            }
            return;
        }
        let Some(Tab::Term(p)) = self.tabs.get(self.active) else {
            self.doc.editor.message =
                Some("Prompt jumps work in a terminal or its history (Shift-F11)".into());
            return;
        };
        let prompts: Vec<u64> = p
            .term
            .marks()
            .iter()
            .filter(|m| m.kind == MarkKind::Prompt)
            .map(|m| m.row)
            .collect();
        if prompts.is_empty() {
            self.doc.editor.message = Some(format!("No prompt marks: {}", no_marks_why(p)));
            return;
        }
        let top = p.term.top_row();
        let target = if forward {
            prompts.iter().find(|&&r| r > top)
        } else {
            prompts.iter().rev().find(|&&r| r < top)
        };
        match target {
            Some(&r) => p.term.scroll_to_row(r),
            None if forward => p.term.scroll_to_bottom(),
            None => {}
        }
    }

    /// Ctrl-click in a terminal: open the `file:line[:col]` under the mouse.
    pub(crate) fn terminal_click_link(&mut self, col: u16, row: u16) -> bool {
        let Some(Tab::Term(p)) = self.tabs.get_mut(self.active) else {
            return false;
        };
        p.term.snapshot_into(&mut p.screen);
        let line = p.screen.row_text(row);
        let Some(word) = word_at(&line, usize::from(col)) else {
            return false;
        };
        let word = word.to_owned();
        let Tab::Term(p) = &self.tabs[self.active] else {
            return false;
        };
        let cwd = self.pane_cwd(p);
        let Some(j) = link_in(&word, &cwd) else {
            return false;
        };
        self.jump(&j.path, j.line, j.col);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_words_and_last_command_rows() {
        let d = std::env::temp_dir().join(format!("skink-links-{}", std::process::id()));
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/a.rs"), "x").unwrap();
        let j = first_link("  --> src/a.rs:4:18", &d).unwrap();
        assert_eq!((j.line, j.col), (3, 17));
        assert!(first_link("see (src/a.rs:2)", &d).is_some());
        assert!(first_link("nothing:3 here", &d).is_none());
        assert_eq!(word_at("ab cd:1 ef", 4), Some("cd:1"));
        assert_eq!(word_at("ab cd", 2), None);
        let m = |kind, row| Mark { kind, row };
        let marks = [
            m(MarkKind::Prompt, 0),
            m(MarkKind::Input, 0),
            m(MarkKind::Output, 1),
            m(MarkKind::Done(Some(0)), 5),
            m(MarkKind::Prompt, 5),
        ];
        assert_eq!(last_command_rows(&marks, 9), Some((0, 1, 5)));
        assert_eq!(last_command_rows(&marks[..3], 9), Some((0, 1, 9)));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
