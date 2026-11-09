//! Status-line prompts: save as, find, open, the `:` command line, and the
//! "save before closing?" question.

use crate::{App, commands, tree::NewEntry};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use skink_core::Request;
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PromptKind {
    /// `close_after`: the tab closes once the save succeeds (save on close).
    SaveAs {
        close_after: bool,
    },
    Find,
    Command,
    Open,
    CloseConfirm,
    /// Name of a new file or folder inside `dir` (`a` / `A` in the tree).
    New {
        what: NewEntry,
        dir: PathBuf,
    },
}

pub struct Prompt {
    pub kind: PromptKind,
    pub label: String,
    pub input: String,
    /// Position while browsing command history (None = editing a new line).
    hist: Option<usize>,
}

impl App {
    pub(crate) fn open_prompt(&mut self, kind: PromptKind, label: &str, input: &str) {
        self.prompt = Some(Prompt {
            kind,
            label: label.to_owned(),
            input: input.to_owned(),
            hist: None,
        });
    }

    /// Save the live document; an untitled one asks for a name first.
    /// Returns true when it is saved now.
    fn save_live(&mut self, close_after: bool) -> bool {
        self.doc.editor.save();
        if self.doc.editor.request.take() == Some(Request::SaveAs) {
            self.open_prompt(PromptKind::SaveAs { close_after }, "Save as: ", "");
            return false;
        }
        !self.doc.editor.buffer.is_dirty()
    }

    pub(crate) fn prompt_key(&mut self, ev: KeyEvent) {
        let Some(p) = &mut self.prompt else { return };
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        if p.kind == PromptKind::CloseConfirm {
            self.prompt = None;
            match ev.code {
                KeyCode::Char('y' | 'Y') => {
                    if self.save_live(true) {
                        self.close_tab(self.cur_doc);
                    }
                }
                KeyCode::Char('n' | 'N') => self.close_tab(self.cur_doc),
                _ => {} // anything else cancels
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
                p.hist = None;
            }
            KeyCode::Up | KeyCode::Down if p.kind == PromptKind::Command => {
                let n = self.cmd_history.len();
                if n == 0 {
                    return;
                }
                p.hist = match (p.hist, ev.code == KeyCode::Up) {
                    (None, true) => Some(n - 1),
                    (Some(i), true) => Some(i.saturating_sub(1)),
                    (Some(i), false) if i + 1 < n => Some(i + 1),
                    _ => None,
                };
                p.input = p
                    .hist
                    .map_or_else(String::new, |i| self.cmd_history[i].clone());
            }
            KeyCode::Tab if matches!(p.kind, PromptKind::Command | PromptKind::Open) => {
                let (kind, input) = (p.kind.clone(), p.input.clone());
                self.complete(kind, &input);
            }
            KeyCode::Char('u') if ctrl => p.input.clear(),
            KeyCode::Char(c) if !ctrl => {
                p.input.push(c);
                p.hist = None;
            }
            _ => {}
        }
    }

    /// Tab completion: a unique match replaces the input, several extend it
    /// to their common prefix and are listed in the status line.
    fn complete(&mut self, kind: PromptKind, input: &str) {
        let options = if kind == PromptKind::Command {
            let trimmed = input.trim_start();
            match trimmed.split_once(' ') {
                None => commands::complete_name(trimmed, &self.registry),
                Some((cmd, arg)) if commands::takes_path(cmd) => {
                    commands::complete_path(arg.trim_start())
                        .into_iter()
                        .map(|p| format!("{cmd} {p}"))
                        .collect()
                }
                Some(_) => Vec::new(),
            }
        } else {
            commands::complete_path(input)
        };
        let Some(p) = &mut self.prompt else { return };
        match options.as_slice() {
            [] => {}
            [one] => p.input.clone_from(one),
            many => {
                let common = commands::common_prefix(many);
                if common.len() > input.len() {
                    p.input = common;
                }
                let shown: Vec<&str> = many.iter().take(8).map(String::as_str).collect();
                let more = if many.len() > 8 { " …" } else { "" };
                self.doc.editor.message = Some(format!("{}{more}", shown.join("  ")));
            }
        }
    }

    fn finish_prompt(&mut self, p: Prompt) {
        let input = p.input.trim();
        match p.kind {
            PromptKind::SaveAs { close_after } => {
                if input.is_empty() {
                    return;
                }
                self.doc.editor.buffer.set_path(input.into());
                self.doc.path_changed(self.settings.syntax);
                self.doc.editor.save();
                self.lsp_attach_live();
                if close_after && !self.doc.editor.buffer.is_dirty() {
                    self.close_tab(self.cur_doc);
                }
            }
            PromptKind::Find => {
                self.doc.editor.find(&p.input, true);
            }
            PromptKind::Command => {
                self.push_history(input);
                self.run_command(input);
            }
            PromptKind::Open => {
                if !input.is_empty() {
                    self.open_location(input);
                }
            }
            PromptKind::CloseConfirm => {}
            PromptKind::New { what, dir } => {
                if !input.is_empty() {
                    self.create_entry(&dir, input, what);
                }
            }
        }
    }
}
