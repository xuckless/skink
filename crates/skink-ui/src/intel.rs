//! Code intelligence in the UI: completion, hover, signature help, go to
//! definition / references, formatting, code actions, symbols and problems.
//!
//! Every question to the language server is a [`Waiting`] entry until its
//! reply arrives (`poll_intel`); what is on screen is one [`Popup`]; an
//! inserted snippet with tabstops is a [`SnippetSession`]. Replies for a
//! document that is no longer live, or that were superseded, are dropped.

use crate::{
    App, Overlay,
    doc::{DocId, Jump, Links},
    lsp::{Answer, Ask, Ticket},
    picker::{self, Item, Target},
};
use skink_core::{ByteEdit, Snippet};
use skink_lsp::{
    CodeAction, CompletionItem, DocumentSymbol, FormattingOptions, GotoKind, Location, Position,
    Range, Severity, TextEdit, WorkspaceEdit,
};
use std::path::{Path, PathBuf};

/// What is drawn next to the cursor.
pub enum Popup {
    Closed,
    Completion(Completion),
    Info(Info),
}

pub struct Completion {
    pub doc: DocId,
    /// Char index where the word being completed starts.
    pub start: usize,
    pub items: Vec<CompletionItem>,
    /// Indices into `items` matching the typed prefix, best first.
    pub ranked: Vec<usize>,
    pub selected: usize,
    pub resolving: Resolving,
}

/// Lazy documentation for the selected completion item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolving {
    Idle,
    /// Asked for `items[index]`.
    Asked(usize),
}

pub struct Info {
    pub kind: InfoKind,
    pub lines: Vec<String>,
}

pub enum InfoKind {
    Hover,
    /// Signature help; the active parameter's char range in `lines[0]`.
    Signature {
        active: Option<(usize, usize)>,
    },
    /// A few lines around a definition.
    Peek {
        title: String,
    },
}

/// The question a ticket belongs to, and what to do with the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    Completion(Trigger),
    Hover,
    Signature,
    Goto {
        kind: GotoKind,
        peek: bool,
    },
    References,
    Format(AfterFormat),
    CodeActions,
    /// Quietly check whether the cursor line has code actions (lightbulb).
    Bulb(BulbAt),
    /// Documentation for completion item `index` (lazy resolve).
    Resolve(usize),
    Execute,
    Outline,
    WorkspaceSymbols,
    Inlay {
        rev: u64,
    },
    /// The server's foldable spans, asked at this buffer revision.
    Folding {
        rev: u64,
    },
    Rename,
    /// References for a safe delete of this item.
    SafeDelete(Box<crate::refactors::Deletion>),
    /// Code actions for `:extract`.
    Extract {
        kind: crate::refactors::Extract,
        name: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Ctrl-Space: say so when there is nothing.
    Manual,
    /// While typing: stay quiet.
    Typed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFormat {
    Nothing,
    Save,
}

pub struct Waiting {
    pub ticket: Ticket,
    pub want: Want,
    pub doc: DocId,
}

/// Tabstops of an inserted snippet that Tab still visits.
pub enum SnippetSession {
    Inactive,
    Active(Stops),
}

pub struct Stops {
    doc: DocId,
    /// Buffer revision `ranges` refer to.
    rev: u64,
    /// Remaining stops, each with its occurrences as byte ranges.
    ranges: Vec<Vec<(usize, usize)>>,
}

/// Where the lightbulb question was asked: document, revision, line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulbAt {
    pub doc: DocId,
    pub rev: u64,
    pub line: usize,
}

/// The code-action lightbulb in the sign column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bulb {
    /// Not asked for the current place.
    Unknown,
    /// Asked; the answer is pending.
    Asked(BulbAt),
    /// Answered: actions exist (`lit`) or not at that place.
    Known { at: BulbAt, lit: bool },
}

/// Inline hints for the live document.
pub enum Inlays {
    None,
    /// Hints for buffer revision `rev`, by line.
    At {
        doc: DocId,
        rev: u64,
        hints: Vec<skink_lsp::InlayHint>,
    },
}

pub struct Intel {
    pub popup: Popup,
    pub waiting: Vec<Waiting>,
    pub snippet: SnippetSession,
    pub inlays: Inlays,
    pub bulb: Bulb,
}

impl Default for Intel {
    fn default() -> Self {
        Intel {
            popup: Popup::Closed,
            waiting: Vec::new(),
            snippet: SnippetSession::Inactive,
            inlays: Inlays::None,
            bulb: Bulb::Unknown,
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// LSP char range -> buffer char indices (clamped).
fn char_range(b: &skink_core::Buffer, r: Range) -> (usize, usize) {
    let at = |p: Position| {
        let line = p.line.min(b.len_lines().saturating_sub(1));
        b.line_to_char(line) + p.col.min(b.line_len(line))
    };
    let (s, e) = (at(r.start), at(r.end));
    (s.min(e), s.max(e))
}

pub(crate) fn edits_to_chars(
    b: &skink_core::Buffer,
    edits: &[TextEdit],
) -> Vec<(usize, usize, String)> {
    edits
        .iter()
        .map(|e| {
            let (s, t) = char_range(b, e.range);
            (s, t, e.new_text.clone())
        })
        .collect()
}

/// Map a byte position through an edit (`start` side stays before text
/// inserted exactly at it, the `end` side grows over it).
fn map_byte(p: usize, e: &ByteEdit, is_end: bool) -> usize {
    if p < e.start_byte || (!is_end && p == e.start_byte) {
        p
    } else if p >= e.old_end_byte {
        p + e.new_end_byte - e.old_end_byte
    } else if is_end {
        e.new_end_byte
    } else {
        e.start_byte
    }
}

/// Markdown from a server, as display lines: fences dropped (their content
/// kept), headings and emphasis markers stripped.
pub fn markdown_lines(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in md.lines() {
        let t = line.trim_end();
        if t.trim_start().starts_with("```") {
            continue;
        }
        let t = t.trim_start_matches('#').trim_start();
        out.push(t.replace("**", "").replace('`', ""));
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    while out.first().is_some_and(String::is_empty) {
        out.remove(0);
    }
    out
}

impl App {
    // ---- asking ------------------------------------------------------------

    /// Path of the live document when a language server has it open.
    fn lsp_path(&self) -> Option<PathBuf> {
        if self.lsp.is_none() || !self.doc.lsp.is_open() {
            return None;
        }
        self.doc.canon.clone()
    }

    /// Send `ask` for the live document (after flushing unsent edits). A
    /// newer question of the same kind replaces an older one.
    pub(crate) fn ask(&mut self, want: Want, ask: Ask) -> bool {
        let Some(path) = self.lsp_path() else {
            if !matches!(
                want,
                Want::Completion(Trigger::Typed)
                    | Want::Signature
                    | Want::Inlay { .. }
                    | Want::Folding { .. }
                    | Want::Bulb(_)
                    | Want::Resolve(_)
            ) {
                self.doc.editor.message = Some(self.no_server_reason());
            }
            return false;
        };
        self.flush_lsp();
        let Some(link) = &mut self.lsp else {
            return false;
        };
        let ticket = link.ask(&path, ask);
        let same = std::mem::discriminant(&want);
        self.intel
            .waiting
            .retain(|w| std::mem::discriminant(&w.want) != same);
        self.intel.waiting.push(Waiting {
            ticket,
            want,
            doc: self.doc.id,
        });
        true
    }

    fn no_server_reason(&self) -> String {
        if !self.lsp_enabled {
            return "Language servers are off".into();
        }
        match (&self.lsp, &self.doc.canon) {
            (Some(link), Some(p)) => match link.status(p) {
                Some(skink_lsp::ServerStatus::NotInstalled(n)) => {
                    format!("No language server installed ({n})")
                }
                Some(skink_lsp::ServerStatus::Failed(e)) => format!("Language server failed: {e}"),
                Some(skink_lsp::ServerStatus::Unsupported) | None => {
                    "No language server for this file".into()
                }
                _ => "Language server is starting".into(),
            },
            _ => "No language server for this file".into(),
        }
    }

    fn cursor_pos(&self) -> Position {
        self.doc.editor.cursor.into()
    }

    // ---- replies -------------------------------------------------------------

    /// Take replies, server-requested edits and crashes (event loop).
    pub(crate) fn poll_intel(&mut self) {
        let Some(link) = &mut self.lsp else {
            return;
        };
        let replies = link.replies();
        let edits = link.drain();
        let crashed = std::mem::take(&mut link.activity.crashed);
        for e in edits {
            self.apply_workspace_edit(&e);
        }
        for server in crashed {
            self.restart_after_crash(&server);
        }
        for r in replies {
            let Some(r) = self.file_move_reply(r) else {
                continue;
            };
            let Some(i) = self.intel.waiting.iter().position(|w| w.ticket == r.ticket) else {
                continue; // superseded
            };
            let w = self.intel.waiting.remove(i);
            if w.doc != self.doc.id {
                continue;
            }
            match r.result {
                Ok(a) => self.on_answer(w.want, a),
                Err(e) if e == "superseded" => {}
                Err(e) => match w.want {
                    Want::Completion(Trigger::Typed) | Want::Signature | Want::Inlay { .. } => {}
                    // No spans from this server (or it failed): indentation,
                    // and no more asking for this document.
                    Want::Folding { .. } => self.doc.editor.fold_spans_unavailable(),
                    Want::Bulb(at) => self.intel.bulb = Bulb::Known { at, lit: false },
                    Want::Resolve(i) => self.resolved(i, None),
                    Want::Format(AfterFormat::Save) => self.save_now(),
                    _ => self.doc.editor.message = Some(format!("Language server: {e}")),
                },
            }
        }
    }

    fn on_answer(&mut self, want: Want, a: Answer) {
        match (want, a) {
            (Want::Completion(t), Answer::Completion(items)) => self.show_completion(items, t),
            (Want::Hover, Answer::Hover(h)) => match h {
                Some(h) if !markdown_lines(&h.text).is_empty() => {
                    self.intel.popup = Popup::Info(Info {
                        kind: InfoKind::Hover,
                        lines: markdown_lines(&h.text),
                    });
                }
                _ => self.doc.editor.message = Some("No information here".into()),
            },
            (Want::Signature, Answer::Signature(s)) => self.show_signature(s),
            (Want::Goto { kind, peek }, Answer::Locations(locs)) => {
                self.on_locations(kind, peek, locs)
            }
            (Want::References, Answer::Locations(locs)) => self.show_references(locs),
            (Want::Format(after), Answer::Edits(edits)) => {
                let e = edits_to_chars(&self.doc.editor.buffer, &edits);
                self.doc.editor.apply_edits(e);
                if after == AfterFormat::Save {
                    self.save_now();
                }
            }
            (Want::CodeActions, Answer::Actions(acts)) => self.show_code_actions(acts),
            (Want::Resolve(i), Answer::Resolved(doc)) => self.resolved(i, doc),
            (Want::Bulb(at), Answer::Actions(acts)) => {
                self.intel.bulb = Bulb::Known {
                    at,
                    lit: !acts.is_empty(),
                };
            }
            (Want::Execute, Answer::Done) => {}
            (Want::Outline, Answer::DocumentSymbols(syms)) => self.fill_outline(syms),
            (Want::WorkspaceSymbols, Answer::Symbols(syms)) => self.fill_symbols(syms),
            (Want::Rename, Answer::Workspace(Some(e))) if !e.is_empty() => {
                self.refactor_workspace_edit("Rename", &e);
            }
            (Want::SafeDelete(d), Answer::Locations(locs)) => self.on_safe_delete_refs(*d, locs),
            (Want::Extract { kind, name }, Answer::Actions(acts)) => {
                self.on_extract_actions(kind, name, acts)
            }
            (Want::Rename, Answer::Workspace(_)) => {
                self.doc.editor.message = Some("Nothing to rename here".into());
            }
            (Want::Folding { rev }, Answer::Folds(spans)) => {
                let spans: Vec<(usize, usize)> =
                    spans.iter().map(|f| (f.start_line, f.end_line)).collect();
                self.doc.editor.set_fold_spans(&spans, rev);
            }
            (Want::Inlay { rev }, Answer::Inlay(hints)) => {
                self.intel.inlays = Inlays::At {
                    doc: self.doc.id,
                    rev,
                    hints,
                };
            }
            _ => {}
        }
    }

    // ---- completion -----------------------------------------------------------

    /// Ctrl-Space (or typing): ask for completions at the cursor.
    pub(crate) fn request_completion(&mut self, t: Trigger) {
        if let Some(items) = self.config_completion() {
            return self.show_completion(items, t);
        }
        let pos = self.cursor_pos();
        self.ask(Want::Completion(t), Ask::Completion(pos));
    }

    fn word_start(&self) -> usize {
        let b = &self.doc.editor.buffer;
        let mut i = self.doc.editor.cursor_idx();
        while i > 0 && b.char_at(i - 1).is_some_and(is_word) {
            i -= 1;
        }
        i
    }

    fn show_completion(&mut self, items: Vec<CompletionItem>, t: Trigger) {
        if items.is_empty() {
            if t == Trigger::Manual {
                self.doc.editor.message = Some("No completions".into());
            }
            return;
        }
        self.intel.popup = Popup::Completion(Completion {
            doc: self.doc.id,
            start: self.word_start(),
            items,
            ranked: Vec::new(),
            selected: 0,
            resolving: Resolving::Idle,
        });
        self.refilter_completion();
    }

    /// Re-rank against the text typed since the list opened; close when the
    /// cursor left the word or nothing matches.
    fn refilter_completion(&mut self) {
        let ed = &self.doc.editor;
        let cur = ed.cursor_idx();
        let Popup::Completion(c) = &mut self.intel.popup else {
            return;
        };
        if c.doc != self.doc.id || cur < c.start || ed.cursor_count() > 1 {
            self.intel.popup = Popup::Closed;
            return;
        }
        let typed = ed.buffer.slice(c.start, cur);
        if typed.chars().any(|ch| !is_word(ch)) {
            self.intel.popup = Popup::Closed;
            return;
        }
        let q = picker::prepare_query(&typed);
        let mut scored: Vec<(i32, &str, usize)> = c
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                picker::score_prepared(&q, &it.label.to_lowercase())
                    .map(|s| (s, it.sort_text.as_deref().unwrap_or(&it.label), i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        c.ranked = scored.into_iter().map(|(.., i)| i).collect();
        c.selected = 0;
        if c.ranked.is_empty() {
            self.intel.popup = Popup::Closed;
        }
    }

    /// Insert the selected completion (Enter / Tab).
    pub(crate) fn accept_completion(&mut self) -> bool {
        let popup = std::mem::replace(&mut self.intel.popup, Popup::Closed);
        let Popup::Completion(c) = popup else {
            self.intel.popup = popup;
            return false;
        };
        let Some(item) = c.ranked.get(c.selected).map(|&i| c.items[i].clone()) else {
            return false;
        };
        let ed = &mut self.doc.editor;
        // Imports and the like first; they are usually above the cursor.
        let mut start = match &item.edit {
            Some(e) => char_range(&ed.buffer, e.range).0.min(c.start),
            None => c.start,
        };
        if !item.additional_edits.is_empty() {
            let extra = edits_to_chars(&ed.buffer, &item.additional_edits);
            for (s, e, t) in &extra {
                if *e <= start {
                    start = (start + t.chars().count()).saturating_sub(e - s);
                }
            }
            ed.apply_edits(extra);
        }
        let text = item
            .edit
            .as_ref()
            .map_or(item.insert_text.as_str(), |e| e.new_text.as_str());
        let cur = ed.cursor_idx();
        ed.anchor = Some(ed.cursor_at(start.min(cur)));
        if item.snippet {
            let snip = Snippet::parse(text);
            ed.insert_text(&snip.text);
            self.start_snippet(start.min(cur), snip);
        } else {
            ed.insert_text(text);
        }
        true
    }

    fn start_snippet(&mut self, at: usize, snip: Snippet) {
        let b = &self.doc.editor.buffer;
        let base = b.char_to_byte(at);
        let to_bytes = |r: &(usize, usize)| {
            let s = b.char_to_byte(at + r.0) - base;
            let e = b.char_to_byte(at + r.1) - base;
            (base + s, base + e)
        };
        let mut ranges: Vec<Vec<(usize, usize)>> = snip
            .stops
            .iter()
            .map(|occ| occ.iter().map(to_bytes).collect())
            .collect();
        if ranges.len() <= 1 {
            // Only the final position: just put the cursor there.
            if let Some(&(s, _)) = ranges.first().and_then(|r| r.first()) {
                let c = b.byte_to_char(s);
                self.doc.editor.place_cursors(&[(c, c)]);
            }
            return;
        }
        let first = ranges.remove(0);
        let chars: Vec<(usize, usize)> = first
            .iter()
            .map(|&(s, e)| (b.byte_to_char(s), b.byte_to_char(e)))
            .collect();
        self.doc.editor.place_cursors(&chars);
        self.intel.snippet = SnippetSession::Active(Stops {
            doc: self.doc.id,
            rev: self.doc.editor.buffer.revision(),
            ranges,
        });
    }

    /// Tab inside a snippet: select the next tabstop(s). False when no
    /// snippet is active (Tab then indents as usual).
    pub(crate) fn next_snippet_stop(&mut self) -> bool {
        let session = std::mem::replace(&mut self.intel.snippet, SnippetSession::Inactive);
        let SnippetSession::Active(mut stops) = session else {
            return false;
        };
        let b = &self.doc.editor.buffer;
        if stops.doc != self.doc.id {
            return false;
        }
        let Some(edits) = b.edits_since(stops.rev) else {
            return false;
        };
        for occ in &mut stops.ranges {
            for r in occ.iter_mut() {
                for e in &edits {
                    *r = (map_byte(r.0, e, false), map_byte(r.1, e, true));
                }
            }
        }
        let next = stops.ranges.remove(0);
        let len = b.len_bytes();
        let chars: Vec<(usize, usize)> = next
            .iter()
            .map(|&(s, e)| (b.byte_to_char(s.min(len)), b.byte_to_char(e.min(len))))
            .collect();
        self.doc.editor.place_cursors(&chars);
        if !stops.ranges.is_empty() {
            stops.rev = self.doc.editor.buffer.revision();
            self.intel.snippet = SnippetSession::Active(stops);
        }
        true
    }

    /// Keys for an open popup. True when consumed.
    pub(crate) fn popup_key(&mut self, spec: &skink_config::KeySpec) -> bool {
        use skink_config::Key;
        let plain = !spec.ctrl && !spec.alt && !spec.shift;
        match &mut self.intel.popup {
            Popup::Closed => false,
            Popup::Completion(c) => {
                let n = c.ranked.len().max(1);
                match spec.key {
                    Key::Up if plain => c.selected = (c.selected + n - 1) % n,
                    Key::Down if plain => c.selected = (c.selected + 1) % n,
                    Key::PageUp if plain => c.selected = c.selected.saturating_sub(8),
                    Key::PageDown if plain => c.selected = (c.selected + 8).min(n - 1),
                    Key::Enter | Key::Tab if plain => return self.accept_completion(),
                    Key::Escape => self.intel.popup = Popup::Closed,
                    _ => return false,
                }
                true
            }
            Popup::Info(info) => {
                let keep = matches!(info.kind, InfoKind::Signature { .. })
                    && !matches!(spec.key, Key::Escape | Key::Up | Key::Down | Key::Enter);
                if !keep {
                    self.intel.popup = Popup::Closed;
                }
                spec.key == Key::Escape
            }
        }
    }

    /// After a character was typed: refilter, or open completion / signature
    /// help when the language asks for it.
    pub(crate) fn after_typed(&mut self, c: char) {
        if self.lsp_path().is_none() {
            return;
        }
        match c {
            '(' | ',' => {
                let pos = self.cursor_pos();
                self.ask(Want::Signature, Ask::Signature(pos));
            }
            ')' if matches!(&self.intel.popup, Popup::Info(i) if matches!(i.kind, InfoKind::Signature { .. })) => {
                self.intel.popup = Popup::Closed
            }
            _ => {}
        }
        if matches!(self.intel.popup, Popup::Completion(_)) {
            self.refilter_completion();
            if matches!(self.intel.popup, Popup::Completion(_)) || is_word(c) {
                return;
            }
        }
        if !self.doc_settings().autocomplete || self.doc.editor.cursor_count() > 1 {
            return;
        }
        let prev = {
            let ed = &self.doc.editor;
            let i = ed.cursor_idx();
            (i >= 2).then(|| ed.buffer.char_at(i - 2)).flatten()
        };
        let trigger =
            c == '.' || (c == ':' && prev == Some(':')) || (c == '>' && prev == Some('-'));
        let word_len = self.doc.editor.cursor_idx() - self.word_start();
        if trigger || (is_word(c) && word_len == 2) {
            self.request_completion(Trigger::Typed);
        }
    }

    /// After Backspace / cursor moves: keep the completion list honest.
    pub(crate) fn after_edit_key(&mut self) {
        if matches!(self.intel.popup, Popup::Completion(_)) {
            self.refilter_completion();
        }
    }

    // ---- hover & signature ------------------------------------------------------

    pub(crate) fn hover(&mut self) {
        let pos = self.cursor_pos();
        self.ask(Want::Hover, Ask::Hover(pos));
    }

    fn show_signature(&mut self, s: Option<skink_lsp::SignatureHelp>) {
        let Some(s) = s else {
            if matches!(&self.intel.popup, Popup::Info(i) if matches!(i.kind, InfoKind::Signature { .. }))
            {
                self.intel.popup = Popup::Closed;
            }
            return;
        };
        let Some(sig) = s
            .signatures
            .get(s.active_signature)
            .or(s.signatures.first())
        else {
            return;
        };
        let active = sig
            .active_parameter
            .or(s.active_parameter)
            .and_then(|i| sig.parameters.get(i))
            .and_then(|p| {
                let b = sig.label.find(&p.label)?;
                let s = sig.label[..b].chars().count();
                Some((s, s + p.label.chars().count()))
            });
        let mut lines = vec![sig.label.clone()];
        if let Some(d) = &sig.documentation {
            let d = markdown_lines(d);
            if !d.is_empty() {
                lines.push(String::new());
                lines.extend(d.into_iter().take(6));
            }
        }
        if matches!(self.intel.popup, Popup::Completion(_)) {
            return; // the completion list wins the space
        }
        self.intel.popup = Popup::Info(Info {
            kind: InfoKind::Signature { active },
            lines,
        });
    }

    // ---- go to -------------------------------------------------------------------

    pub(crate) fn goto(&mut self, kind: GotoKind, peek: bool) {
        let pos = self.cursor_pos();
        self.ask(Want::Goto { kind, peek }, Ask::Goto(kind, pos));
    }

    fn on_locations(&mut self, kind: GotoKind, peek: bool, locs: Vec<Location>) {
        match (locs.len(), peek) {
            (0, _) => {
                self.doc.editor.message = Some(
                    match kind {
                        GotoKind::Definition => "No definition found",
                        GotoKind::Declaration => "No declaration found",
                        GotoKind::TypeDefinition => "No type definition found",
                        GotoKind::Implementation => "No implementation found",
                    }
                    .into(),
                )
            }
            (_, true) => self.peek(&locs[0]),
            (1, false) => self.jump(&locs[0].path, locs[0].line(), locs[0].col()),
            _ => {
                let items = locs.iter().map(|l| self.location_item(l, "")).collect();
                self.open_choices("Go to", items);
            }
        }
    }

    fn location_item(&self, l: &Location, tag: &'static str) -> Item {
        let root = self.project_root();
        let rel = l.path.strip_prefix(&root).unwrap_or(&l.path);
        Item {
            label: format!("{}:{}", rel.display(), l.line() + 1),
            detail: line_of(&l.path, l.line()).trim().to_owned(),
            target: Target::Location {
                path: l.path.clone(),
                line: l.line(),
                col: l.col(),
            },
            tag,
        }
    }

    fn peek(&mut self, l: &Location) {
        let text = std::fs::read_to_string(&l.path).unwrap_or_default();
        let start = l.line().saturating_sub(2);
        let lines: Vec<String> = text
            .lines()
            .skip(start)
            .take(12)
            .map(|s| s.replace('\t', "    "))
            .collect();
        let root = self.project_root();
        let rel = l.path.strip_prefix(&root).unwrap_or(&l.path);
        self.intel.popup = Popup::Info(Info {
            kind: InfoKind::Peek {
                title: format!("{}:{}", rel.display(), l.line() + 1),
            },
            lines,
        });
    }

    /// Open `path` and put the cursor at 0-based `line`, `col`.
    pub(crate) fn jump(&mut self, path: &Path, line: usize, col: usize) {
        self.record_jump();
        let same = self.doc.canon.as_deref() == Some(&crate::doc::canonical(path));
        if !same {
            self.open_path(path);
        }
        self.doc.editor.goto(Some(line + 1), Some(col + 1));
        self.doc.editor.scroll_into_view();
    }

    // ---- references & problems (results tabs) ----------------------------------------

    /// Alt-R: ask for the new name on the command line, pre-filled.
    pub(crate) fn prompt_rename_symbol(&mut self) {
        let ed = &self.doc.editor;
        let (s, mut e) = (self.word_start(), ed.cursor_idx());
        while ed.buffer.char_at(e).is_some_and(is_word) {
            e += 1;
        }
        let word: String = (s..e).filter_map(|i| ed.buffer.char_at(i)).collect();
        self.open_prompt(
            crate::prompt::PromptKind::Command,
            ":",
            &format!("renamesymbol {word}"),
        );
    }

    /// `:renamesymbol NEW`: the server's edit goes to the refactor preview.
    pub(crate) fn rename_symbol(&mut self, new_name: &str) {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            self.doc.editor.message = Some("usage: renamesymbol NEW_NAME".into());
            return;
        }
        let pos = self.cursor_pos();
        self.ask(Want::Rename, Ask::Rename(pos, new_name.to_owned()));
    }

    pub(crate) fn find_references(&mut self) {
        let pos = self.cursor_pos();
        self.ask(Want::References, Ask::References(pos));
    }

    fn show_references(&mut self, locs: Vec<Location>) {
        if locs.is_empty() {
            self.doc.editor.message = Some("No references found".into());
            return;
        }
        let word = {
            let ed = &self.doc.editor;
            let (s, mut e) = (self.word_start(), ed.cursor_idx());
            while ed.buffer.char_at(e).is_some_and(is_word) {
                e += 1;
            }
            ed.buffer.slice(s, e)
        };
        let n = locs.len();
        let rows = locs
            .into_iter()
            .map(|l| {
                let line = line_of(&l.path, l.line());
                (
                    format!(
                        "{}:{}:{}  {}",
                        self.rel(&l.path),
                        l.line() + 1,
                        l.col() + 1,
                        line.trim()
                    ),
                    Some(Jump {
                        path: l.path.clone(),
                        line: l.line(),
                        col: l.col(),
                    }),
                )
            })
            .collect();
        self.open_results(&format!("References: {word} ({n})"), rows);
    }

    fn rel(&self, p: &Path) -> String {
        let root = self.project_root();
        p.strip_prefix(&root).unwrap_or(p).display().to_string()
    }

    /// A read-only results tab; Enter on a line with a jump goes there.
    pub(crate) fn open_results(&mut self, title: &str, rows: Vec<(String, Option<Jump>)>) {
        let text: Vec<&str> = rows.iter().map(|(t, _)| t.as_str()).collect();
        self.open_view(title, &text.join("\n"));
        self.doc.links = Links::Lines(rows.into_iter().map(|(_, j)| j).collect());
    }

    /// Enter in a results tab. True when it jumped.
    pub(crate) fn follow_link(&mut self) -> bool {
        let Links::Lines(lines) = &self.doc.links else {
            return false;
        };
        let Some(Some(j)) = lines.get(self.doc.editor.cursor.line).cloned() else {
            return false;
        };
        self.jump(&j.path, j.line, j.col);
        true
    }

    /// Shift-F8: every diagnostic of the open files, errors first.
    pub(crate) fn show_problems(&mut self) {
        if self.lsp.is_none() && matches!(self.task_problems, crate::tasks::TaskProblems::None) {
            self.doc.editor.message =
                Some("No problems (no language server running, no task run yet)".into());
            return;
        }
        let mut all: Vec<(PathBuf, skink_lsp::Diagnostic)> = Vec::new();
        for p in self.diag_paths() {
            all.extend(self.all_diags(&p).into_iter().map(|d| (p.clone(), d)));
        }
        all.sort_by(|a, b| {
            (a.1.severity, &a.0, a.1.range.start.line).cmp(&(
                b.1.severity,
                &b.0,
                b.1.range.start.line,
            ))
        });
        let (e, w) = all.iter().fold((0, 0), |(e, w), (_, d)| match d.severity {
            Severity::Error => (e + 1, w),
            Severity::Warning => (e, w + 1),
            _ => (e, w),
        });
        let rows = all
            .into_iter()
            .map(|(p, d)| {
                let sev = match d.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                    Severity::Info => "info",
                    Severity::Hint => "hint",
                };
                let first = d.message.lines().next().unwrap_or_default().to_owned();
                (
                    format!(
                        "{sev:<7} {}:{}:{}  {first}",
                        self.rel(&p),
                        d.range.start.line + 1,
                        d.range.start.col + 1
                    ),
                    Some(Jump {
                        path: p,
                        line: d.range.start.line,
                        col: d.range.start.col,
                    }),
                )
            })
            .collect::<Vec<_>>();
        let task = match &self.task_problems {
            crate::tasks::TaskProblems::Found { label, .. } => format!(" + task {label}"),
            crate::tasks::TaskProblems::None => String::new(),
        };
        let title = format!("Problems ({e} errors, {w} warnings{task})");
        if rows.is_empty() {
            self.open_results(&title, vec![("No problems in open files".into(), None)]);
        } else {
            self.open_results(&title, rows);
        }
    }

    /// F6 / Shift-F6: next / previous diagnostic in this file.
    pub(crate) fn goto_diagnostic(&mut self, forward: bool) {
        let Some(p) = &self.doc.canon else {
            return;
        };
        let mut lines: Vec<(usize, usize)> = self
            .all_diags(p)
            .iter()
            .map(|d| (d.range.start.line, d.range.start.col))
            .collect();
        lines.sort_unstable();
        lines.dedup();
        let c = self.doc.editor.cursor;
        let here = (c.line, c.col);
        let next = if forward {
            lines.iter().find(|&&l| l > here).or(lines.first())
        } else {
            lines.iter().rev().find(|&&l| l < here).or(lines.last())
        };
        match next {
            Some(&(l, col)) => self.doc.editor.goto(Some(l + 1), Some(col + 1)),
            None => self.doc.editor.message = Some("No problems in this file".into()),
        }
    }

    // ---- formatting -----------------------------------------------------------------

    fn format_options(&self) -> FormattingOptions {
        FormattingOptions {
            tab_size: self.doc_settings().tabsize as u32,
            insert_spaces: self.doc.editor.indent == skink_core::Indent::Spaces,
        }
    }

    /// Alt-Shift-F: the selection, or the whole file.
    pub(crate) fn format(&mut self, after: AfterFormat) -> bool {
        let opts = self.format_options();
        let ask = match self.doc.editor.selection() {
            Some((s, e)) if after == AfterFormat::Nothing => {
                let (a, b) = (self.doc.editor.cursor_at(s), self.doc.editor.cursor_at(e));
                Ask::FormatRange(Range::new(a.into(), b.into()), opts)
            }
            _ => Ask::Format(opts),
        };
        self.ask(Want::Format(after), ask)
    }

    /// Save, formatting first when `formatonsave` is on and a server has
    /// the file. True when the save was deferred until the format reply.
    pub(crate) fn format_before_save(&mut self) -> bool {
        self.doc_settings().format_on_save
            && self.doc.editor.buffer.is_dirty()
            && self.lsp_path().is_some()
            && self.format(AfterFormat::Save)
    }

    pub(crate) fn save_now(&mut self) {
        self.doc.editor.save();
        if let Some(r) = self.doc.editor.request.take() {
            self.handle_request(r);
        }
    }

    // ---- code actions & workspace edits ---------------------------------------------------

    /// Fetch documentation for the selected completion item when the server
    /// left it out (one question per item; the answer fills the docs panel).
    pub(crate) fn resolve_selected_completion(&mut self) {
        let Popup::Completion(c) = &self.intel.popup else {
            return;
        };
        let Some(&i) = c.ranked.get(c.selected) else {
            return;
        };
        let item = &c.items[i];
        let wanted = item.documentation.is_none()
            && matches!(item.resolve, skink_lsp::Resolve::Available(_))
            && c.resolving != Resolving::Asked(i);
        if !wanted {
            return;
        }
        let item = Box::new(item.clone());
        if self.ask(Want::Resolve(i), Ask::Resolve(item))
            && let Popup::Completion(c) = &mut self.intel.popup
        {
            c.resolving = Resolving::Asked(i);
        }
    }

    fn resolved(&mut self, index: usize, doc: Option<String>) {
        if let Popup::Completion(c) = &mut self.intel.popup
            && let Some(item) = c.items.get_mut(index)
        {
            item.resolve = skink_lsp::Resolve::Complete;
            item.documentation = doc;
            c.resolving = Resolving::Idle;
        }
    }

    /// Ask (quietly) whether the cursor line has code actions, once per
    /// place, after edits have been sent. Drives the sign-column lightbulb.
    pub(crate) fn update_bulb(&mut self) {
        if self.lsp_path().is_none() || self.doc.lsp.is_pending() {
            return;
        }
        let ed = &self.doc.editor;
        let at = BulbAt {
            doc: self.doc.id,
            rev: ed.buffer.revision(),
            line: ed.cursor.line,
        };
        let current = match self.intel.bulb {
            Bulb::Unknown => None,
            Bulb::Asked(a) | Bulb::Known { at: a, .. } => Some(a),
        };
        if current == Some(at) {
            return;
        }
        let len = ed.buffer.line_len(at.line);
        let range = Range::new(Position::new(at.line, 0), Position::new(at.line, len));
        let diags = match (&self.lsp, &self.doc.canon) {
            (Some(link), Some(p)) => link
                .diagnostics(p)
                .into_iter()
                .filter(|d| d.range.start.line <= at.line && d.range.end.line >= at.line)
                .collect(),
            _ => Vec::new(),
        };
        if self.ask(Want::Bulb(at), Ask::CodeActions(range, diags)) {
            self.intel.bulb = Bulb::Asked(at);
        }
    }

    /// The line showing the lightbulb, if the answer for the current place
    /// says there are actions.
    pub(crate) fn bulb_line(&self) -> Option<usize> {
        match self.intel.bulb {
            Bulb::Known { at, lit: true }
                if at.doc == self.doc.id
                    && at.rev == self.doc.editor.buffer.revision()
                    && at.line == self.doc.editor.cursor.line =>
            {
                Some(at.line)
            }
            _ => None,
        }
    }

    pub(crate) fn code_actions(&mut self) {
        let ed = &self.doc.editor;
        let (s, e) = ed
            .selection()
            .map(|(s, e)| (ed.cursor_at(s), ed.cursor_at(e)))
            .unwrap_or((ed.cursor, ed.cursor));
        let range = Range::new(s.into(), e.into());
        let diags = match (&self.lsp, &self.doc.canon) {
            (Some(link), Some(p)) => link
                .diagnostics(p)
                .into_iter()
                .filter(|d| d.range.start.line <= e.line && d.range.end.line >= s.line)
                .collect(),
            _ => Vec::new(),
        };
        self.ask(Want::CodeActions, Ask::CodeActions(range, diags));
    }

    fn show_code_actions(&mut self, mut acts: Vec<CodeAction>) {
        if acts.is_empty() {
            self.doc.editor.message = Some("No code actions here".into());
            return;
        }
        acts.sort_by_key(|a| !a.preferred);
        let items = acts
            .into_iter()
            .map(|a| Item {
                label: a.title.clone(),
                detail: a.kind.clone().unwrap_or_default(),
                tag: if a.preferred { "preferred" } else { "" },
                target: Target::Action(Box::new(a)),
            })
            .collect();
        self.open_choices("Code actions", items);
    }

    pub(crate) fn run_code_action(&mut self, a: CodeAction) {
        if let Some(e) = &a.edit {
            self.apply_workspace_edit(e);
        }
        if let Some(c) = a.command {
            self.ask(Want::Execute, Ask::Execute(c));
        }
    }

    /// Apply a server's multi-file edit to the open documents (opening the
    /// others); the edited files stay unsaved for review.
    pub(crate) fn apply_workspace_edit(&mut self, e: &WorkspaceEdit) {
        // Several files: review them in the refactor preview first.
        if e.files.iter().filter(|f| !f.edits.is_empty()).count() > 1 {
            return self.refactor_workspace_edit("Code action", e);
        }
        let here = self.active;
        let mut touched = 0;
        for f in &e.files {
            if f.edits.is_empty() {
                continue;
            }
            let canon = crate::doc::canonical(&f.path);
            if self.doc.canon.as_deref() != Some(&canon) {
                self.open_path(&f.path);
            }
            if self.doc.canon.as_deref() == Some(&canon) {
                let edits = edits_to_chars(&self.doc.editor.buffer, &f.edits);
                self.doc.editor.apply_edits(edits);
                touched += 1;
            }
        }
        if here < self.tabs.len() {
            self.switch_to(here);
        }
        if !e.ops.is_empty() {
            self.doc.editor.message =
                Some("File create/rename/delete from the server is not supported yet".into());
        } else if touched > 1 {
            self.doc.editor.message = Some(format!("{touched} files changed (unsaved)"));
        }
    }

    // ---- symbols ------------------------------------------------------------------------

    /// Ctrl-R: symbols in this file (quick open in `@` mode).
    pub(crate) fn open_outline(&mut self) {
        self.open_picker();
        if let Overlay::Picker(p) = &mut self.overlay {
            p.input = "@".into();
        }
        self.picker_input_changed();
    }

    /// F1: the command palette (quick open in `>` mode).
    pub(crate) fn open_palette(&mut self) {
        self.open_picker();
        if let Overlay::Picker(p) = &mut self.overlay {
            p.input = ">".into();
        }
        self.picker_input_changed();
    }

    /// Alt-W: symbols in the project (quick open in `#` mode).
    pub(crate) fn open_workspace_symbols(&mut self) {
        self.open_picker();
        if let Overlay::Picker(p) = &mut self.overlay {
            p.input = "#".into();
        }
        self.picker_input_changed();
    }

    /// The quick-open input changed: switch lists on `>`, `@`, `#`, and keep
    /// project symbol results in step with the query.
    pub(crate) fn picker_input_changed(&mut self) {
        let Overlay::Picker(p) = &self.overlay else {
            return;
        };
        if matches!(p.mode, picker::Mode::Choices(_)) {
            return;
        }
        let want = picker::Mode::for_input(&p.input);
        let query = p.input.get(1..).unwrap_or_default().to_owned();
        let changed = want != p.mode;
        match (want, changed) {
            (picker::Mode::Files, true) => {
                let input = p.input.clone();
                self.open_picker();
                if let Overlay::Picker(p) = &mut self.overlay {
                    for c in input.chars() {
                        p.type_char(c);
                    }
                }
            }
            (picker::Mode::Commands, true) => {
                let items = crate::commands::palette_items(self);
                if let Overlay::Picker(p) = &mut self.overlay {
                    p.set_mode(picker::Mode::Commands, items);
                }
            }
            (picker::Mode::Outline, true) => {
                if let Overlay::Picker(p) = &mut self.overlay {
                    p.set_mode(picker::Mode::Outline, Vec::new());
                }
                self.ask(Want::Outline, Ask::DocumentSymbols);
            }
            (picker::Mode::Symbols, _) => {
                if changed && let Overlay::Picker(p) = &mut self.overlay {
                    p.set_mode(picker::Mode::Symbols, Vec::new());
                }
                if !query.is_empty() {
                    self.ask(Want::WorkspaceSymbols, Ask::WorkspaceSymbols(query));
                }
            }
            _ => {}
        }
    }

    fn fill_outline(&mut self, syms: Vec<DocumentSymbol>) {
        fn walk(out: &mut Vec<Item>, s: &DocumentSymbol, depth: usize, path: &Path) {
            out.push(Item {
                label: format!("{}{}", "  ".repeat(depth), s.name),
                detail: s.detail.clone().unwrap_or_default(),
                tag: kind_tag(s.kind),
                target: Target::Location {
                    path: path.to_path_buf(),
                    line: s.selection_range.start.line,
                    col: s.selection_range.start.col,
                },
            });
            for c in &s.children {
                walk(out, c, depth + 1, path);
            }
        }
        let Some(path) = self.doc.canon.clone() else {
            return;
        };
        let mut items = Vec::new();
        for s in &syms {
            walk(&mut items, s, 0, &path);
        }
        if let Overlay::Picker(p) = &mut self.overlay
            && p.mode == picker::Mode::Outline
        {
            p.set_items(items);
        }
    }

    fn fill_symbols(&mut self, syms: Vec<skink_lsp::SymbolInfo>) {
        let items = syms
            .into_iter()
            .take(500)
            .map(|s| Item {
                label: s.name.clone(),
                detail: match &s.container {
                    Some(c) => format!("{c}  {}", self.rel(&s.location.path)),
                    None => self.rel(&s.location.path),
                },
                tag: kind_tag(s.kind),
                target: Target::Location {
                    path: s.location.path.clone(),
                    line: s.location.line(),
                    col: s.location.col(),
                },
            })
            .collect();
        if let Overlay::Picker(p) = &mut self.overlay
            && p.mode == picker::Mode::Symbols
        {
            p.set_items(items);
        }
    }

    pub(crate) fn open_choices(&mut self, title: &str, items: Vec<Item>) {
        self.set_overlay(Overlay::Picker(Box::new(picker::Picker::choices(
            title, items,
        ))));
    }

    // ---- inlay hints ------------------------------------------------------------------

    /// Ask for hints in the visible lines when they are stale.
    pub(crate) fn refresh_inlays(&mut self, first: usize, last: usize) {
        if !self.doc_settings().inlay_hints
            || self.lsp_path().is_none()
            || self.doc.lsp.is_pending()
        {
            return;
        }
        let rev = self.doc.editor.buffer.revision();
        let fresh = matches!(&self.intel.inlays, Inlays::At { doc, rev: r, .. } if *doc == self.doc.id && *r == rev);
        let asked = self
            .intel
            .waiting
            .iter()
            .any(|w| w.want == Want::Inlay { rev } && w.doc == self.doc.id);
        if fresh || asked {
            return;
        }
        let range = Range::new(Position::new(first, 0), Position::new(last + 1, 0));
        self.ask(Want::Inlay { rev }, Ask::InlayHints(range));
    }

    /// Ask the server for foldable spans when the ones held are older than
    /// the text (never while it is starting, never again once it has none).
    pub(crate) fn refresh_folds(&mut self) {
        if self.lsp_path().is_none() || self.doc.lsp.is_pending() {
            return;
        }
        use skink_core::fold::FoldSource;
        let rev = self.doc.editor.buffer.revision();
        match self.doc.editor.folds().source() {
            FoldSource::IndentOnly => return,
            FoldSource::Server { from, .. } if *from == rev => return,
            FoldSource::Indent | FoldSource::Server { .. } => {}
        }
        let asked = self
            .intel
            .waiting
            .iter()
            .any(|w| matches!(w.want, Want::Folding { .. }) && w.doc == self.doc.id);
        if !asked {
            self.ask(Want::Folding { rev }, Ask::Folding);
        }
    }

    /// Hints for display line `line` of the live document: (char column, text).
    pub(crate) fn inlays_on(&self, line: usize) -> Vec<(usize, String)> {
        match &self.intel.inlays {
            Inlays::At { doc, rev, hints }
                if *doc == self.doc.id && *rev == self.doc.editor.buffer.revision() =>
            {
                hints
                    .iter()
                    .filter(|h| h.position.line == line)
                    .map(|h| {
                        let mut t = h.label.clone();
                        if h.pad_left {
                            t.insert(0, ' ');
                        }
                        if h.pad_right {
                            t.push(' ');
                        }
                        (h.position.col, t)
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    // ---- servers ---------------------------------------------------------------------

    /// A server died unexpectedly: start it again once per minute at most,
    /// re-announcing the live document.
    fn restart_after_crash(&mut self, server: &str) {
        let now = std::time::Instant::now();
        let recent = self
            .lsp_restarts
            .iter()
            .filter(|(s, t)| s == server && now.duration_since(*t).as_secs() < 60)
            .count();
        if recent > 0 {
            self.doc.editor.message = Some(format!(
                "{server} crashed again; :lsp restart to try once more"
            ));
            return;
        }
        self.lsp_restarts.retain(|(s, _)| s != server);
        self.lsp_restarts.push((server.to_owned(), now));
        self.doc.editor.message = Some(format!("{server} crashed; restarting it"));
        self.restart_lsp();
    }

    /// `:lsp restart`: fresh server for the live file; every open document
    /// of the project is announced again as it becomes live.
    pub(crate) fn restart_lsp(&mut self) {
        let (Some(link), Some(p)) = (&self.lsp, &self.doc.canon) else {
            return;
        };
        link.restart(p);
        for i in 0..self.tabs.len() {
            if let Some(d) = self.doc_at_mut(i) {
                d.lsp = crate::doc::LspSync::Closed;
            }
        }
        self.doc.lsp = crate::doc::LspSync::Closed;
        self.intel.waiting.clear();
        self.lsp_attach_live();
    }

    /// `:lsp`: status of the live file's server, progress, recent log.
    pub(crate) fn show_lsp_status(&mut self) {
        let Some(link) = &self.lsp else {
            let mut text = String::from("No language server has been started.");
            if let Some(p) = &self.doc.canon {
                text.push_str(&format!("\n{}", self.lsp_pick(p)));
            }
            self.open_view("Language servers", &text);
            return;
        };
        let mut lines = Vec::new();
        if let Some(p) = &self.doc.canon {
            use skink_lsp::ServerStatus as St;
            let say = |s: Option<St>| match s {
                Some(St::Ready(name)) => format!("running ({name})"),
                Some(St::NotStarted) => "not started yet (starts on first use)".into(),
                Some(St::Failed(e)) => format!("failed: {e} (:lsp restart)"),
                Some(St::NotInstalled(n)) => format!("{n} is not installed"),
                Some(St::Disabled) => "turned off in settings".into(),
                Some(St::Unsupported) => "no language server for this file type".into(),
                None => "starting".into(),
            };
            match link.statuses(p) {
                Some(all) if all.len() > 1 => {
                    lines.push(format!("{}: {} servers", self.rel(p), all.len()));
                    for (i, s) in all.into_iter().enumerate() {
                        let role = if i == 0 { "primary" } else { "also" };
                        lines.push(format!("  {role}: {}", say(Some(s))));
                    }
                }
                _ => lines.push(format!("{}: {}", self.rel(p), say(link.status(p)))),
            }
            lines.push(self.lsp_pick(p));
        }
        lines.push(match link.backend() {
            Some(skink_core::Backend::Delegated) => "servers: run by the EveryAI host".into(),
            Some(skink_core::Backend::Local) | None => "servers: run by Skink".into(),
        });
        for pid in link.server_pids() {
            let rss = crate::lsp::rss_mb(pid).map_or_else(|| "?".into(), |m| format!("{m:.1} MB"));
            lines.push(format!("server pid {pid}: {rss}"));
        }
        for (s, p) in &link.activity.progress {
            lines.push(format!("{s}: {p}"));
        }
        lines.push(String::new());
        lines.push("Recent log (:lsp log for all):".into());
        lines.extend(link.activity.log.iter().rev().take(20).rev().cloned());
        self.open_view("Language servers", &lines.join("\n"));
    }

    /// Which server a file gets and why: from settings or built in, and
    /// where it was found on PATH.
    pub(crate) fn lsp_pick(&self, path: &std::path::Path) -> String {
        let langs = self.lsp_languages();
        let Some((lang, id)) = skink_lsp::language_in(&langs, path) else {
            return "no language server is configured for this file type (add \"lsp.<lang>\" in settings)".into();
        };
        let from = match lang.source {
            skink_lsp::Source::BuiltIn => "built in",
            skink_lsp::Source::Settings => "from settings.json",
            skink_lsp::Source::Disabled => {
                return format!(
                    "{} ({id}): turned off by \"lsp.{}\": false",
                    lang.id, lang.id
                );
            }
        };
        let tried = |servers: &[skink_lsp::ServerSpec]| -> Vec<String> {
            servers
                .iter()
                .map(|s| {
                    let cmd = std::iter::once(s.program.as_str())
                        .chain(s.args.iter().map(String::as_str))
                        .collect::<Vec<_>>()
                        .join(" ");
                    match skink_lsp::find_in_path(&s.program) {
                        Some(full) => format!("{cmd}  -> {}", full.display()),
                        None => format!("{cmd}  -> not found on PATH"),
                    }
                })
                .collect()
        };
        let mut out = format!(
            "{} ({id}), {from}:\n  {}",
            lang.id,
            tried(&lang.servers).join("\n  ")
        );
        for extra in &lang.also {
            out.push_str(&format!("\n  also: {}", tried(extra).join("\n        ")));
        }
        out
    }

    pub(crate) fn show_lsp_log(&mut self) {
        let text = self.lsp.as_ref().map_or_else(String::new, |l| {
            l.activity
                .log
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        });
        self.open_view("Language server log", &text);
    }

    /// Short status-line note: progress, or nothing.
    pub(crate) fn lsp_note(&self) -> Option<String> {
        let link = self.lsp.as_ref()?;
        link.activity
            .progress
            .last()
            .map(|(s, p)| format!("{s}: {p}"))
    }
}

/// Short tag for a symbol kind in lists.
fn kind_tag(k: skink_lsp::ItemKind) -> &'static str {
    use skink_lsp::ItemKind as K;
    match k {
        K::Function | K::Method | K::Constructor => "fn",
        K::Struct | K::Class => "type",
        K::Interface => "trait",
        K::Enum => "enum",
        K::EnumMember => "variant",
        K::Module | K::Namespace | K::Package => "mod",
        K::Field | K::Property => "field",
        K::Constant => "const",
        K::Variable => "var",
        _ => "",
    }
}

/// Line `n` (0-based) of a file, read on demand (results lists).
fn line_of(path: &Path, n: usize) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.lines().nth(n).map(str::to_owned))
        .unwrap_or_default()
}

/// The text a completion item shows next to its label.
pub fn item_detail(it: &CompletionItem) -> &str {
    it.detail.as_deref().unwrap_or_default()
}
