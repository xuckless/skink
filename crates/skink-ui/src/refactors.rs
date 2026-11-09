//! Refactors built from smaller pieces: safe delete (find-references, then
//! a preview of the removal), move file (`:mv`, through the tree's
//! `willRenameFiles` move) and extract variable / function (server code
//! actions, with a tree-sitter fallback for extract-variable).

use crate::App;
use crate::intel::Want;
use crate::lsp::Ask;
use crate::refactor::{FilePatch, Preview, Source};
use skink_lsp::{CodeAction, Location};
use std::path::PathBuf;

/// Whether references may stop a safe delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Refuse while the symbol is used outside its own body.
    Refs,
    /// `:safedelete!`: preview the removal whatever the references say.
    Force,
}

/// The item a safe delete would remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deletion {
    pub path: PathBuf,
    /// Byte range of the item in the text at the time of asking.
    pub start: usize,
    pub end: usize,
    /// 0-based lines the item covers (references inside don't count).
    pub lines: (usize, usize),
    pub name: String,
}

/// Which extraction `:extract` asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extract {
    Variable,
    Function,
}

impl Extract {
    fn noun(self) -> &'static str {
        match self {
            Extract::Variable => "variable",
            Extract::Function => "function",
        }
    }
    /// Does a code action's kind / title offer this extraction?
    fn offered_by(self, a: &CodeAction) -> bool {
        let kind = a.kind.as_deref().unwrap_or_default();
        let t = a.title.to_lowercase();
        let words: &[&str] = match self {
            Extract::Variable => &["variable", "constant", "local"],
            Extract::Function => &["function", "method"],
        };
        (kind.starts_with("refactor.extract") || t.contains("extract"))
            && words.iter().any(|w| t.contains(w))
    }
}

impl App {
    /// `:safedelete[!]`: remove the declaration at the cursor once find-
    /// references shows nothing else uses it.
    pub(crate) fn safe_delete(&mut self, check: Check) {
        let Some(path) = self.doc.canon.clone() else {
            self.doc.editor.message = Some("Safe delete needs a saved file".into());
            return;
        };
        let Some(lang) = skink_syntax::language_for_path(&path) else {
            self.doc.editor.message = Some("Safe delete: unknown language".into());
            return;
        };
        let b = &self.doc.editor.buffer;
        let text = b.text();
        let at = b.char_to_byte(self.doc.editor.cursor_idx());
        let Some((start, end)) = skink_syntax::enclosing_item(lang, &text, at) else {
            self.doc.editor.message = Some("No declaration at the cursor".into());
            return;
        };
        let lines = (
            b.char_to_line(b.byte_to_char(start)),
            b.char_to_line(b.byte_to_char(end.saturating_sub(1))),
        );
        let name = self.word_at_cursor();
        let d = Deletion {
            path,
            start,
            end,
            lines,
            name,
        };
        match check {
            Check::Force => self.preview_deletion(&d),
            Check::Refs => {
                let pos = self.doc.editor.cursor.into();
                if !self.ask(Want::SafeDelete(Box::new(d)), Ask::References(pos)) {
                    let why = self.doc.editor.message.take().unwrap_or_default();
                    self.doc.editor.message = Some(format!(
                        "Can't check references ({why}); :safedelete! deletes anyway"
                    ));
                }
            }
        }
    }

    /// The references answer: block on outside uses, else preview.
    pub(crate) fn on_safe_delete_refs(&mut self, d: Deletion, locs: Vec<Location>) {
        let outside: Vec<Location> = locs
            .into_iter()
            .filter(|l| {
                let inside = crate::doc::canonical(&l.path) == d.path
                    && (d.lines.0..=d.lines.1).contains(&l.line());
                !inside
            })
            .collect();
        if outside.is_empty() {
            return self.preview_deletion(&d);
        }
        let n = outside.len();
        let rows = outside
            .into_iter()
            .map(|l| {
                let line = std::fs::read_to_string(&l.path)
                    .ok()
                    .and_then(|t| t.lines().nth(l.line()).map(str::to_owned))
                    .unwrap_or_default();
                (
                    format!(
                        "{}:{}:{}  {}",
                        l.path.display(),
                        l.line() + 1,
                        l.col() + 1,
                        line.trim()
                    ),
                    Some(crate::doc::Jump {
                        path: l.path.clone(),
                        line: l.line(),
                        col: l.col(),
                    }),
                )
            })
            .collect();
        self.open_results(&format!("Still used: {} ({n})", d.name), rows);
        self.doc.editor.message = Some(format!(
            "{} is still used in {n} place{}; :safedelete! removes it anyway",
            d.name,
            if n == 1 { "" } else { "s" }
        ));
    }

    fn preview_deletion(&mut self, d: &Deletion) {
        let Some(base) = self.text_of(&d.path) else {
            return;
        };
        if d.end > base.len() || !base.is_char_boundary(d.start) || !base.is_char_boundary(d.end) {
            self.doc.editor.message = Some("The file changed; run safe delete again".into());
            return;
        }
        let new = format!("{}{}", &base[..d.start], &base[d.end..]);
        let title = format!("Safe delete {}", d.name);
        let p = FilePatch::new(d.path.clone(), base, &new);
        self.open_refactor(Preview::new(&title, Source::Lsp, vec![p]));
    }

    /// `:extract var|fn [NAME]`: the server's extract code action for the
    /// selection; extract-variable falls back to tree-sitter (Rust, TS/JS,
    /// Python) when no server offers it.
    pub(crate) fn extract(&mut self, args: &[&str]) {
        let kind = match args.first().copied() {
            Some("var" | "variable" | "v") => Extract::Variable,
            Some("fn" | "function" | "f" | "method") => Extract::Function,
            _ => {
                self.doc.editor.message = Some("Usage: extract var|fn [NAME]".into());
                return;
            }
        };
        let name = args.get(1).map(|s| (*s).to_owned());
        let ed = &self.doc.editor;
        let Some((s, e)) = ed.selection() else {
            self.doc.editor.message =
                Some(format!("Select the code to extract into a {}", kind.noun()));
            return;
        };
        let range = skink_lsp::Range::new(ed.cursor_at(s).into(), ed.cursor_at(e).into());
        let asked = self.lsp.is_some()
            && self.ask(
                Want::Extract {
                    kind,
                    name: name.clone(),
                },
                Ask::CodeActions(range, Vec::new()),
            );
        if !asked {
            self.doc.editor.message = None;
            self.extract_fallback(kind, name);
        }
    }

    /// The code actions answer for `:extract`.
    pub(crate) fn on_extract_actions(
        &mut self,
        kind: Extract,
        name: Option<String>,
        acts: Vec<CodeAction>,
    ) {
        let mut offered: Vec<CodeAction> =
            acts.into_iter().filter(|a| kind.offered_by(a)).collect();
        match offered.len() {
            0 => self.extract_fallback(kind, name),
            1 => {
                let a = offered.remove(0);
                self.doc.editor.message = Some(format!("{} (rename it with Alt-R)", a.title));
                self.run_code_action(a);
            }
            _ => {
                offered.sort_by_key(|a| !a.preferred);
                let items = offered
                    .into_iter()
                    .map(|a| crate::picker::Item {
                        label: a.title.clone(),
                        detail: a.kind.clone().unwrap_or_default(),
                        tag: if a.preferred { "preferred" } else { "" },
                        target: crate::picker::Target::Action(Box::new(a)),
                    })
                    .collect();
                self.open_choices(&format!("Extract {}", kind.noun()), items);
            }
        }
    }

    fn extract_fallback(&mut self, kind: Extract, name: Option<String>) {
        if kind == Extract::Function {
            self.doc.editor.message =
                Some("Extract function needs a language server that offers it".into());
            return;
        }
        let lang = self
            .doc
            .canon
            .as_deref()
            .and_then(skink_syntax::language_for_path);
        let ed = &self.doc.editor;
        let (Some(lang), Some((s, e))) = (lang, ed.selection()) else {
            self.doc.editor.message =
                Some("Extract variable: select an expression in a source file".into());
            return;
        };
        let text = ed.buffer.text();
        let (bs, be) = (ed.buffer.char_to_byte(s), ed.buffer.char_to_byte(e));
        let name = name.unwrap_or_else(|| "value".into());
        match skink_syntax::extract_variable(lang, &text, bs, be, &name) {
            Ok(new) => {
                self.doc.editor.set_text(&new);
                self.doc.editor.message = Some(format!("Extracted into {name} (Alt-R renames it)"));
            }
            Err(e) => self.doc.editor.message = Some(format!("Extract variable: {e}")),
        }
    }

    fn word_at_cursor(&self) -> String {
        let ed = &self.doc.editor;
        let w = |c: char| c.is_alphanumeric() || c == '_';
        let (mut s, mut e) = (ed.cursor_idx(), ed.cursor_idx());
        while s > 0 && ed.buffer.char_at(s - 1).is_some_and(w) {
            s -= 1;
        }
        while ed.buffer.char_at(e).is_some_and(w) {
            e += 1;
        }
        ed.buffer.slice(s, e)
    }
}
