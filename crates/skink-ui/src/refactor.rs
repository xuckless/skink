//! Refactor preview: one screen for every change that touches several files
//! (rename symbol, multi-file code actions, edits an agent staged). A list of
//! files and their hunks with checkboxes on the left, the selected hunk side
//! by side on the right; Enter applies the checked hunks, one undo step per
//! file, and leaves the files unsaved for a last look.
//!
//! Diffs come from `skink_ai::prompter::Diff` (built on `similar`, Apache-2.0).
//! The layout idea (file tree of hunks, per-hunk checkboxes) is IntelliJ's
//! refactoring preview; nothing is copied from it.

use crate::App;
use skink_ai::prompter::{Diff, Segment};
use skink_core::{Buffer, Editor};
use skink_lsp::WorkspaceEdit;
use std::path::{Path, PathBuf};

/// Decision on one change block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hunk {
    /// Not decided yet (inline AI review); applies like `Accepted`.
    Pending,
    Accepted,
    Rejected,
}

impl Hunk {
    pub fn applies(self) -> bool {
        self != Hunk::Rejected
    }

    pub fn toggled(self) -> Hunk {
        match self {
            Hunk::Rejected => Hunk::Accepted,
            Hunk::Pending | Hunk::Accepted => Hunk::Rejected,
        }
    }
}

/// Where the changes came from (what to clean up afterwards).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A language server (rename, code action).
    Lsp,
    /// Writes an agent staged through `fs/write_text_file`.
    Agent,
    /// Replace across files after a project search.
    Search,
}

pub struct FilePatch {
    pub path: PathBuf,
    /// Text the diff was computed against (applying checks it is unchanged).
    pub base: String,
    pub diff: Diff,
    pub hunks: Vec<Hunk>,
}

impl FilePatch {
    pub fn new(path: PathBuf, base: String, new: &str) -> Self {
        let diff = Diff::new(&base, new);
        let hunks = vec![Hunk::Accepted; diff.block_count()];
        FilePatch {
            path,
            base,
            diff,
            hunks,
        }
    }
}

/// A row of the list on the left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    File(usize),
    Hunk(usize, usize),
}

pub struct Preview {
    pub title: String,
    pub source: Source,
    pub files: Vec<FilePatch>,
    /// Selected row.
    pub row: usize,
}

/// Whether the preview is on screen.
pub enum PreviewState {
    Closed,
    Open(Box<Preview>),
}

impl Preview {
    pub fn new(title: &str, source: Source, mut files: Vec<FilePatch>) -> Self {
        files.retain(|f| !f.hunks.is_empty());
        Preview {
            title: title.to_owned(),
            source,
            files,
            row: 0,
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut v = Vec::new();
        for (f, p) in self.files.iter().enumerate() {
            v.push(Row::File(f));
            v.extend((0..p.hunks.len()).map(|h| Row::Hunk(f, h)));
        }
        v
    }

    pub fn current(&self) -> Option<Row> {
        self.rows().get(self.row).copied()
    }

    pub fn move_by(&mut self, d: isize) {
        let n = self.rows().len();
        if n > 0 {
            self.row = self.row.saturating_add_signed(d).min(n - 1);
        }
    }

    /// Space: flip the hunk, or every hunk of a file.
    pub fn toggle(&mut self) {
        match self.current() {
            Some(Row::Hunk(f, h)) => {
                let x = &mut self.files[f].hunks[h];
                *x = x.toggled();
            }
            Some(Row::File(f)) => {
                let p = &mut self.files[f];
                let to = if p.hunks.iter().all(|h| h.applies()) {
                    Hunk::Rejected
                } else {
                    Hunk::Accepted
                };
                p.hunks.iter_mut().for_each(|h| *h = to);
            }
            None => {}
        }
    }

    pub fn set_all(&mut self, h: Hunk) {
        for p in &mut self.files {
            p.hunks.iter_mut().for_each(|x| *x = h);
        }
    }

    /// (files, hunks) that would change.
    pub fn counts(&self) -> (usize, usize) {
        let hunks = |p: &FilePatch| p.hunks.iter().filter(|h| h.applies()).count();
        (
            self.files.iter().filter(|p| hunks(p) > 0).count(),
            self.files.iter().map(hunks).sum(),
        )
    }

    /// The change block `h` of file `f` with up to `ctx` unchanged lines
    /// around it: (old lines, new lines, first old line number, 0-based).
    pub fn hunk_view(&self, f: usize, h: usize, ctx: usize) -> HunkView {
        let segs = &self.files[f].diff.segments;
        let mut line = 0;
        let mut seen = 0;
        for (i, s) in segs.iter().enumerate() {
            match s {
                Segment::Equal(l) => line += l.len(),
                Segment::Change { old, new } => {
                    if seen == h {
                        let before: Vec<String> = match i.checked_sub(1).map(|j| &segs[j]) {
                            Some(Segment::Equal(l)) => l[l.len().saturating_sub(ctx)..].to_vec(),
                            _ => Vec::new(),
                        };
                        let after: Vec<String> = match segs.get(i + 1) {
                            Some(Segment::Equal(l)) => l[..ctx.min(l.len())].to_vec(),
                            _ => Vec::new(),
                        };
                        return HunkView {
                            first_line: line - before.len(),
                            before,
                            old: old.clone(),
                            new: new.clone(),
                            after,
                        };
                    }
                    seen += 1;
                    line += old.len();
                }
            }
        }
        HunkView::default()
    }
}

#[derive(Default)]
pub struct HunkView {
    pub first_line: usize,
    pub before: Vec<String>,
    pub old: Vec<String>,
    pub new: Vec<String>,
    pub after: Vec<String>,
}

/// Text of a server edit applied to `base` (LSP positions, via a scratch editor).
fn apply_lsp_edits(base: &str, edits: &[skink_lsp::TextEdit]) -> String {
    let mut ed = Editor::new(Buffer::from_text(base));
    let e = crate::intel::edits_to_chars(&ed.buffer, edits);
    ed.apply_edits(e);
    ed.buffer.text()
}

/// A file-tree rename or move waiting for the language servers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileMove {
    Idle,
    /// `workspace/willRenameFiles` is out; the move happens on the answer.
    Asking {
        ticket: crate::lsp::Ticket,
        from: PathBuf,
        to_rel: String,
    },
}

/// `path` with the prefix `from` replaced by `to` (it was inside the move).
fn moved(path: &Path, from: &Path, to: &Path) -> PathBuf {
    match path.strip_prefix(from) {
        Ok(rest) if rest.as_os_str().is_empty() => to.to_path_buf(),
        Ok(rest) => to.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

impl App {
    /// Ask the servers what a move of `from` must change; true when the move
    /// now waits for them ([`FileMove::Asking`]).
    pub(crate) fn ask_before_move(&mut self, from: &Path, to_rel: &str) -> bool {
        let (Some(root), Some(link)) = (self.tree().map(|t| t.root.clone()), &mut self.lsp) else {
            return false;
        };
        let to = root.join(to_rel.trim());
        let ticket = link.ask(from, crate::lsp::Ask::WillRename(to));
        self.ai.file_move = FileMove::Asking {
            ticket,
            from: from.to_path_buf(),
            to_rel: to_rel.to_owned(),
        };
        self.doc.editor.message = Some("Asking the language servers about the move...".into());
        true
    }

    /// A language-server reply: the one the pending move waits for is
    /// consumed (the move runs, server edits go to the preview); others are
    /// handed back.
    pub(crate) fn file_move_reply(&mut self, r: crate::lsp::Reply) -> Option<crate::lsp::Reply> {
        let FileMove::Asking { ticket, .. } = &self.ai.file_move else {
            return Some(r);
        };
        if *ticket != r.ticket {
            return Some(r);
        }
        let FileMove::Asking { from, to_rel, .. } =
            std::mem::replace(&mut self.ai.file_move, FileMove::Idle)
        else {
            return None;
        };
        let from_c = crate::doc::canonical(&from); // before it is gone
        let to = self.move_path(&from, &to_rel)?;
        if let Some(link) = &mut self.lsp {
            link.ask(&from, crate::lsp::Ask::DidRename(to.clone()));
        }
        match r.result {
            Ok(crate::lsp::Answer::Workspace(Some(mut e))) if !e.is_empty() => {
                // Edits inside what moved now apply at the new place.
                for f in &mut e.files {
                    f.path = moved(&crate::doc::canonical(&f.path), &from_c, &to);
                }
                self.refactor_workspace_edit(&format!("Move to {to_rel}"), &e);
            }
            Ok(_) => {}
            Err(e) => {
                self.doc.editor.message = Some(format!(
                    "moved to {to_rel} (language server: {e}; references not updated)"
                ));
            }
        }
        None
    }

    /// Current text of `path`: the open buffer, else the file on disk.
    pub(crate) fn text_of(&self, path: &Path) -> Option<String> {
        let canon = crate::doc::canonical(path);
        match self.find_doc(|d| d.canon.as_ref() == Some(&canon)) {
            Some(i) => self.doc_at(i).map(|d| d.editor.buffer.text()),
            None => std::fs::read_to_string(path).ok(),
        }
    }

    /// Show a server's multi-file edit in the preview instead of applying it.
    pub(crate) fn refactor_workspace_edit(&mut self, title: &str, e: &WorkspaceEdit) {
        let files: Vec<FilePatch> = e
            .files
            .iter()
            .filter(|f| !f.edits.is_empty())
            .filter_map(|f| {
                let base = self.text_of(&f.path)?;
                let new = apply_lsp_edits(&base, &f.edits);
                Some(FilePatch::new(f.path.clone(), base, &new))
            })
            .collect();
        if !e.ops.is_empty() {
            self.doc.editor.message =
                Some("File create/rename/delete from the server is not supported yet".into());
        }
        self.open_refactor(Preview::new(title, Source::Lsp, files));
    }

    pub(crate) fn refactor_open(&self) -> bool {
        matches!(self.ai.preview, PreviewState::Open(_))
    }

    pub(crate) fn open_refactor(&mut self, p: Preview) {
        if p.files.is_empty() {
            self.doc.editor.message = Some(format!("{}: nothing to change", p.title));
            return;
        }
        self.ai.preview = PreviewState::Open(Box::new(p));
    }

    /// Keys while the preview is open. Always consumes them.
    pub(crate) fn refactor_key(&mut self, key: &skink_config::KeySpec) {
        use skink_config::Key;
        let PreviewState::Open(p) = &mut self.ai.preview else {
            return;
        };
        match key.key {
            Key::Up => p.move_by(-1),
            Key::Down => p.move_by(1),
            Key::PageUp => p.move_by(-10),
            Key::PageDown => p.move_by(10),
            Key::Home => p.row = 0,
            Key::End => p.move_by(isize::MAX / 2),
            Key::Char(' ') => p.toggle(),
            Key::Char('a') => p.set_all(Hunk::Accepted),
            Key::Char('n') => p.set_all(Hunk::Rejected),
            Key::Enter => self.apply_refactor(),
            Key::Escape | Key::Char('q') => self.close_refactor(false),
            _ => {}
        }
    }

    fn close_refactor(&mut self, applied: bool) {
        let state = std::mem::replace(&mut self.ai.preview, PreviewState::Closed);
        if let PreviewState::Open(p) = state
            && p.source == Source::Agent
        {
            self.ai.shared.clear_staged();
            if !applied {
                self.doc.editor.message = Some("Agent edits discarded".into());
            }
        }
    }

    /// Enter: write the checked hunks into the documents (opening files that
    /// are not open yet). Files stay unsaved.
    fn apply_refactor(&mut self) {
        let PreviewState::Open(p) = &self.ai.preview else {
            return;
        };
        let jobs: Vec<(PathBuf, String, String)> = p
            .files
            .iter()
            .filter(|f| f.hunks.iter().any(|h| h.applies()))
            .map(|f| {
                let hunks = &f.hunks;
                (
                    f.path.clone(),
                    f.base.clone(),
                    f.diff.apply(|i| hunks[i].applies()),
                )
            })
            .collect();
        let here = self.active;
        let (mut done, mut stale) = (0, 0);
        for (path, base, new) in jobs {
            let canon = crate::doc::canonical(&path);
            if path.exists() {
                self.open_path(&path);
            } else {
                // A file the agent creates: a new, unsaved buffer.
                self.add_doc(Editor::new(Buffer::new_file(&path)), false);
            }
            if self.doc.canon.as_deref() != Some(&canon)
                && self.doc.editor.buffer.path() != Some(path.as_path())
            {
                stale += 1;
                continue;
            }
            if self.doc.editor.buffer.text() != base {
                stale += 1;
                continue;
            }
            self.doc.editor.set_text(&new);
            done += 1;
        }
        if here < self.tabs.len() {
            self.switch_to(here);
        }
        self.close_refactor(true);
        self.doc.editor.message = Some(match stale {
            0 => format!("Changed {done} file(s) (unsaved)"),
            s => format!("Changed {done} file(s); {s} skipped because they changed meanwhile"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_toggle_and_hunk_views() {
        let f = FilePatch::new("a".into(), "1\n2\n3\n4\n5\n".into(), "1\nX\n3\n4\nY\n");
        let mut p = Preview::new("t", Source::Lsp, vec![f]);
        assert_eq!(p.rows().len(), 3);
        p.row = 2;
        p.toggle();
        assert_eq!(p.files[0].hunks, [Hunk::Accepted, Hunk::Rejected]);
        assert_eq!(p.counts(), (1, 1));
        let v = p.hunk_view(0, 1, 2);
        assert_eq!((v.first_line, v.before.len(), v.old.len()), (2, 2, 1));
        p.row = 0;
        p.toggle(); // not all applied -> all accepted
        assert_eq!(p.counts(), (1, 2));
    }
}
