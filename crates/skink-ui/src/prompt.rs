//! Status-line prompts: save as, find, open, the `:` command line, and the
//! "save before closing?" question.

use crate::{App, Overlay, commands, tree::NewEntry};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use skink_core::Request;
use skink_core::Within;
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PromptKind {
    /// `close_after`: the tab closes once the save succeeds (save on close).
    SaveAs {
        close_after: bool,
    },
    /// Live search; `origin` is where the cursor was when the bar opened
    /// (Esc goes back there). `selection` is what was selected then (Alt-S
    /// toggles `within` between it and the whole file).
    Find {
        origin: usize,
        within: Within,
        selection: Option<(usize, usize)>,
    },
    /// Replacement text for every match of `query` in `within` (Tab from
    /// the find bar).
    Replace {
        query: String,
        within: Within,
    },
    Command,
    Open,
    /// A question answered with one key.
    Choice(Choice),
    /// New path (relative to the tree root) for a file or folder.
    Rename {
        from: PathBuf,
    },
    /// Live filter of the file tree.
    TreeFilter,
    /// Text to search the whole project for (Alt-R/C/W toggle like Find).
    ProjectSearch,
    /// Name of a new file or folder inside `dir` (`a` / `A` in the tree).
    New {
        what: NewEntry,
        dir: PathBuf,
    },
}

/// One-key questions about the live document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Unsaved changes on close/quit: save / don't save.
    Close,
    /// The file changed on disk since it was read: overwrite it?
    Overwrite,
    /// Writing was refused: save through sudo?
    SudoSave,
    /// Changed on disk under unsaved edits: keep mine / take disk / diff.
    DiskChanged,
    /// A crash backup with unsaved edits exists: recover / discard.
    Recover { backup: PathBuf },
    /// Move a file or folder to the OS trash?
    Trash { path: PathBuf },
    /// A git operation that loses work.
    Git(crate::gitui::Danger),
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
        self.set_overlay(Overlay::Prompt(Prompt {
            kind,
            label: label.to_owned(),
            input: input.to_owned(),
            hist: None,
        }));
    }

    /// Save the live document; an untitled one asks for a name first.
    /// Returns true when it is saved now.
    fn save_live(&mut self, close_after: bool) -> bool {
        self.doc.editor.save();
        match self.doc.editor.request.take() {
            Some(Request::SaveAs) => {
                self.open_prompt(PromptKind::SaveAs { close_after }, "Save as: ", "");
                return false;
            }
            Some(r) => self.handle_request(r),
            None => self.git_after_save(),
        }
        !self.doc.editor.buffer.is_dirty()
    }

    pub(crate) fn open_choice(&mut self, choice: Choice) {
        let name = self.doc.title();
        let label = match &choice {
            Choice::Close => {
                let verb = if self.doc_count() > 1 {
                    "closing"
                } else {
                    "quitting"
                };
                format!("Save changes before {verb}? (y)es / (n)o / Esc ")
            }
            Choice::Overwrite => {
                format!("{name} changed on disk since it was read. Overwrite it? (y)es / (n)o ")
            }
            Choice::SudoSave => "Permission denied. Save with sudo? (y)es / (n)o ".to_owned(),
            Choice::DiskChanged => format!(
                "{name} changed on disk and has unsaved edits: (k)eep mine / (t)ake disk / (d)iff "
            ),
            Choice::Trash { path } => format!(
                "Move {} to the trash? (y)es / (n)o ",
                path.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            ),
            Choice::Git(d) => d.question(),
            Choice::Recover { .. } => format!(
                "{name} has unsaved changes from an earlier session: (r)ecover / (d)iscard / Esc later "
            ),
        };
        self.open_prompt(PromptKind::Choice(choice), &label, "");
    }

    fn choice_key(&mut self, choice: Choice, key: KeyCode) {
        self.set_overlay(Overlay::None);
        let k = match key {
            KeyCode::Char(c) => c.to_ascii_lowercase(),
            _ => '\u{1b}', // anything else cancels
        };
        match (choice, k) {
            (Choice::Close, 'y') => {
                if self.save_live(true) {
                    self.close_tab(self.cur_doc);
                }
            }
            (Choice::Close, 'n') => self.close_tab(self.cur_doc),
            (Choice::Overwrite, 'y') => {
                self.doc.editor.save_with(true);
                if let Some(r) = self.doc.editor.request.take() {
                    self.handle_request(r);
                }
            }
            (Choice::SudoSave, 'y') => self.sudo_save(),
            (Choice::DiskChanged, 'k') => self.resolve_disk_conflict(false),
            (Choice::DiskChanged, 't') => self.resolve_disk_conflict(true),
            (Choice::DiskChanged, 'd') => self.diff_with_disk(),
            (Choice::Trash { path }, 'y') => self.finish_trash(&path),
            (Choice::Git(d), 'y') => self.confirmed(d),
            (Choice::Recover { backup }, 'r') => self.recover(&backup, true),
            (Choice::Recover { backup }, 'd') => self.recover(&backup, false),
            _ => {}
        }
    }

    pub(crate) fn prompt_key(&mut self, ev: KeyEvent) {
        let Overlay::Prompt(p) = &mut self.overlay else {
            return;
        };
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        if let PromptKind::Choice(choice) = &p.kind {
            let choice = choice.clone();
            return self.choice_key(choice, ev.code);
        }
        let alt = ev.modifiers.contains(KeyModifiers::ALT);
        if let PromptKind::Find {
            origin,
            within,
            selection,
        } = p.kind
        {
            if ev.code == KeyCode::Tab && !p.input.is_empty() {
                let query = p.input.clone();
                let label = format!("Replace {query:?}{} with: ", within_note(within));
                self.doc.editor.last_search.clone_from(&query);
                return self.open_prompt(PromptKind::Replace { query, within }, &label, "");
            }
            if ev.code == KeyCode::Char('s') && alt {
                let within = match within {
                    Within::File => selection.map_or(Within::File, |(a, b)| Within::Range(a, b)),
                    Within::Range(..) => Within::File,
                };
                p.kind = PromptKind::Find {
                    origin,
                    within,
                    selection,
                };
                p.label = find_label(&self.doc.editor.search_opts, within);
                return self.find_live(origin);
            }
            let opts = &mut self.doc.editor.search_opts;
            let toggled = match ev.code {
                KeyCode::Char('r') if alt => {
                    opts.regex = !opts.regex;
                    true
                }
                KeyCode::Char('w') if alt => {
                    opts.whole_word = !opts.whole_word;
                    true
                }
                KeyCode::Char('c') if alt => {
                    opts.case = match opts.case {
                        skink_core::Case::Smart => skink_core::Case::Sensitive,
                        skink_core::Case::Sensitive => skink_core::Case::Smart,
                    };
                    true
                }
                _ => false,
            };
            if toggled {
                p.label = find_label(&self.doc.editor.search_opts, within);
                return self.find_live(origin);
            }
            if ev.code == KeyCode::Esc {
                self.set_overlay(Overlay::None);
                let c = self.doc.editor.cursor_at(origin);
                self.doc.editor.click(c.line, c.col, false);
                self.doc.editor.message = None;
                return;
            }
        }
        if p.kind == PromptKind::ProjectSearch && alt {
            let opts = &mut self.doc.editor.search_opts;
            let toggled = match ev.code {
                KeyCode::Char('r') => {
                    opts.regex = !opts.regex;
                    true
                }
                KeyCode::Char('w') => {
                    opts.whole_word = !opts.whole_word;
                    true
                }
                KeyCode::Char('c') => {
                    opts.case = match opts.case {
                        skink_core::Case::Smart => skink_core::Case::Sensitive,
                        skink_core::Case::Sensitive => skink_core::Case::Smart,
                    };
                    true
                }
                _ => false,
            };
            if toggled {
                p.label = crate::projsearch::search_label(&self.doc.editor.search_opts);
                return;
            }
        }
        if p.kind == PromptKind::TreeFilter && ev.code == KeyCode::Esc {
            self.set_overlay(Overlay::None);
            return self.set_tree_filter(None);
        }
        let before = p.input.clone();
        self.prompt_edit_key(ev, ctrl);
        if let Overlay::Prompt(Prompt {
            kind: PromptKind::TreeFilter,
            input,
            ..
        }) = &self.overlay
            && *input != before
        {
            let q = input.clone();
            self.set_tree_filter(Some(&q));
        }
        if let Overlay::Prompt(Prompt {
            kind: PromptKind::Find { origin, .. },
            input,
            ..
        }) = &self.overlay
            && *input != before
        {
            let origin = *origin;
            self.find_live(origin);
        }
    }

    /// Open the find bar, pre-filled with the last search, and search live.
    /// A selection over several lines starts it in "in selection" mode.
    pub(crate) fn open_find(&mut self) {
        let ed = &self.doc.editor;
        let selection = ed.selection();
        let within = match selection {
            Some((a, b)) if ed.buffer.char_to_line(a) != ed.buffer.char_to_line(b) => {
                Within::Range(a, b)
            }
            _ => Within::File,
        };
        let origin = match within {
            Within::Range(a, _) => a,
            Within::File => ed.cursor_idx(),
        };
        let q = ed.last_search.clone();
        let label = find_label(&ed.search_opts, within);
        self.open_prompt(
            PromptKind::Find {
                origin,
                within,
                selection,
            },
            &label,
            &q,
        );
        self.find_live(origin);
    }

    /// Select the first match at or after `origin` and show "N of M".
    fn find_live(&mut self, origin: usize) {
        let Overlay::Prompt(p) = &self.overlay else {
            return;
        };
        let within = match p.kind {
            PromptKind::Find { within, .. } => within,
            _ => Within::File,
        };
        let query = p.input.clone();
        let ed = &mut self.doc.editor;
        if query.is_empty() {
            let c = ed.cursor_at(origin);
            ed.click(c.line, c.col, false);
            ed.message = None;
            return;
        }
        ed.message = Some(match ed.search_opts.compile(&query) {
            Err(e) => format!("bad regex: {e}"),
            Ok(re) if ed.select_match_in(&re, origin, within) => {
                let c = ed.match_count_in(&re, within);
                let plus = if c.capped { "+" } else { "" };
                format!("{} of {}{plus}", c.current, c.total)
            }
            Ok(_) => "no matches".into(),
        });
    }

    fn prompt_edit_key(&mut self, ev: KeyEvent, ctrl: bool) {
        let Overlay::Prompt(p) = &mut self.overlay else {
            return;
        };
        match ev.code {
            KeyCode::Esc => self.set_overlay(Overlay::None),
            KeyCode::Enter => {
                if let Some(p) = self.take_prompt() {
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
        let Overlay::Prompt(p) = &mut self.overlay else {
            return;
        };
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
            PromptKind::Find { .. } => {
                // The live search already selected the match; remember it
                // for FindNext / FindPrevious.
                self.doc.editor.last_search.clone_from(&p.input);
                self.doc.editor.message = None;
            }
            PromptKind::Replace { query, within } => {
                let ed = &mut self.doc.editor;
                let opts = ed.search_opts;
                ed.message = Some(match opts.compile(&query) {
                    Err(e) => format!("bad regex: {e}"),
                    // Not `input`: the replacement may end in spaces.
                    Ok(re) => match ed.replace_matches(&re, &p.input, within, opts.regex) {
                        0 => "no matches".to_owned(),
                        1 => "replaced 1 match".to_owned(),
                        n => format!("replaced {n} matches"),
                    },
                });
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
            PromptKind::Choice(_) => {}
            PromptKind::Rename { from } => self.finish_rename(&from, input),
            PromptKind::ProjectSearch => self.start_project_search(&p.input),
            PromptKind::TreeFilter => {
                self.set_tree_filter(Some(input));
                self.set_sidebar_view(crate::SidebarView::Focused);
            }
            PromptKind::New { what, dir } => {
                if !input.is_empty() {
                    self.create_entry(&dir, input, what);
                }
            }
        }
    }
}

fn within_note(w: Within) -> &'static str {
    match w {
        Within::File => "",
        Within::Range(..) => " in selection",
    }
}

/// "Find: " plus the active toggles (Alt-R regex, Alt-C case, Alt-W word,
/// Alt-S in selection).
fn find_label(o: &skink_core::SearchOptions, within: Within) -> String {
    let mut l = String::from("Find");
    if within != Within::File {
        l.push_str(" [sel]");
    }
    if o.regex {
        l.push_str(" .*");
    }
    if o.case == skink_core::Case::Sensitive {
        l.push_str(" Aa");
    }
    if o.whole_word {
        l.push_str(" \\b");
    }
    l.push_str(": ");
    l
}
