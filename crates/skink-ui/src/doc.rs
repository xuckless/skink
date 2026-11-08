//! One open document: an `Editor` plus everything the UI remembers per file
//! (scroll, branch, LSP sync state, syntax tree, cached conflict blocks).
//! Keeping it in one struct means switching tabs is a single swap and no
//! per-file state can leak from one tab into another.

use skink_core::{Buffer, Conflict, Editor};
use std::path::{Path, PathBuf};

/// Stable identity of a document, independent of its tab position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocId(pub u64);

/// What the language server has seen of this document.
#[derive(Debug, Default)]
pub struct LspSync {
    /// `didOpen` was sent (so `didClose` must be sent too).
    pub opened: bool,
    /// Revision last sent to the server.
    pub rev: u64,
    /// Edits not sent yet (flushed after the typing pause).
    pub pending: bool,
    /// Dirty flag at the last sync; a dirty -> clean edge is a save.
    pub was_dirty: bool,
}

/// Per-document syntax highlighting state.
pub struct Syntax {
    pub hl: skink_syntax::Highlighter,
    /// Revision the tree was parsed at (`u64::MAX` = never).
    pub rev: u64,
    /// The text that was parsed (highlight queries read from it).
    pub text: String,
}

pub struct Doc {
    pub id: DocId,
    pub editor: Editor,
    /// Canonical path, resolved once: the key for LSP, de-duplication and
    /// the file tree highlight (no `canonicalize` syscall per frame).
    pub canon: Option<PathBuf>,
    pub hscroll: usize,
    pub git_branch: Option<String>,
    pub lsp: LspSync,
    pub syntax: Option<Syntax>,
    conflicts: (u64, Vec<Conflict>),
}

impl Doc {
    pub fn new(id: DocId, editor: Editor, syntax: bool) -> Self {
        let canon = editor.buffer.path().map(canonical);
        let git_branch = editor.buffer.path().and_then(git_branch);
        let mut doc = Doc {
            id,
            editor,
            canon,
            hscroll: 0,
            git_branch,
            lsp: LspSync::default(),
            syntax: None,
            conflicts: (u64::MAX, Vec::new()),
        };
        doc.reset_syntax(syntax);
        doc
    }

    /// Display name: the view title, the file name, or `[scratch]`.
    pub fn title(&self) -> String {
        self.editor
            .buffer
            .title()
            .unwrap_or_else(|| "[scratch]".to_owned())
    }

    /// A file that a language server may care about.
    pub fn is_lsp_file(&self) -> bool {
        self.canon.is_some() && !self.editor.buffer.is_read_only()
    }

    /// Re-resolve the path after "save as".
    pub fn path_changed(&mut self, syntax: bool) {
        self.canon = self.editor.buffer.path().map(canonical);
        self.git_branch = self.editor.buffer.path().and_then(git_branch);
        self.reset_syntax(syntax);
    }

    /// (Re)create the highlighter for this file's language; parsing is lazy.
    pub fn reset_syntax(&mut self, enabled: bool) {
        self.syntax = self
            .editor
            .buffer
            .path()
            .filter(|_| enabled)
            .and_then(skink_syntax::language_for_path)
            .map(|lang| Syntax {
                hl: skink_syntax::Highlighter::new(lang),
                rev: u64::MAX,
                text: String::new(),
            });
    }

    /// Conflict blocks, recomputed only when the text changed.
    pub fn conflicts(&mut self) -> &[Conflict] {
        let rev = self.editor.buffer.revision();
        if self.conflicts.0 != rev {
            self.conflicts = (rev, self.editor.conflicts());
        }
        &self.conflicts.1
    }

    /// Replace the contents of a read-only view (command output arriving).
    pub fn replace_view(&mut self, title: &str, text: &str) {
        let tab = self.editor.tab_width;
        self.editor = Editor::new(Buffer::view(title, text));
        self.editor.tab_width = tab;
        self.hscroll = 0;
    }
}

/// `canonicalize`, falling back to the path itself (new, unsaved files).
pub fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Current branch, read from `HEAD` without running git. Handles linked
/// worktrees and submodules, where `.git` is a file holding `gitdir: <path>`.
pub fn git_branch(path: &Path) -> Option<String> {
    let start = canonical(path);
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        let git_dir = if dot_git.is_dir() {
            dot_git
        } else if dot_git.is_file() {
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            dir.join(target)
        } else {
            continue;
        };
        let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
        let head = head.trim();
        return Some(match head.strip_prefix("ref: refs/heads/") {
            Some(b) => b.to_owned(),
            None => head.chars().take(7).collect(), // detached: short hash
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_from_worktree_gitdir_file() {
        let d = std::env::temp_dir().join(format!("skink-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let real = d.join("repo/.git/worktrees/wt");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        std::fs::create_dir_all(d.join("wt/src")).unwrap();
        std::fs::write(d.join("wt/.git"), format!("gitdir: {}\n", real.display())).unwrap();
        std::fs::write(d.join("wt/src/a.rs"), "").unwrap();
        assert_eq!(
            git_branch(&d.join("wt/src/a.rs")).as_deref(),
            Some("feature/x")
        );
        std::fs::write(real.join("HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(
            git_branch(&d.join("wt/src/a.rs")).as_deref(),
            Some("0123456")
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn conflicts_are_cached_per_revision() {
        let mut doc = Doc::new(
            DocId(1),
            Editor::new(Buffer::from_text("<<<<<<< a\nx\n=======\ny\n>>>>>>> b\n")),
            false,
        );
        assert_eq!(doc.conflicts().len(), 1);
        doc.editor.select_all();
        doc.editor.insert_text("clean");
        assert!(doc.conflicts().is_empty());
    }
}
