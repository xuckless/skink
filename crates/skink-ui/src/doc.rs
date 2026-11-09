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
pub enum LspSync {
    /// Not announced to a server (no `didOpen` yet).
    #[default]
    Closed,
    /// `didOpen` was sent, so `didClose` must be sent too.
    Open(OpenSync),
}

#[derive(Debug)]
pub struct OpenSync {
    /// Revision of the last edit noted.
    pub rev: u64,
    /// Edits not sent yet (flushed after the typing pause).
    pub pending: bool,
    /// Dirty flag at the last sync; a dirty -> clean edge is a save.
    pub was_dirty: bool,
}

impl LspSync {
    pub fn is_open(&self) -> bool {
        matches!(self, LspSync::Open(_))
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, LspSync::Open(s) if s.pending)
    }
}

/// Per-document syntax highlighting state.
pub struct Syntax {
    pub hl: skink_syntax::Highlighter,
    pub parse: ParseState,
}

/// Whether `Syntax::hl` holds a tree, and for which text.
pub enum ParseState {
    /// No tree (new, or dropped while parked); the next frame parses.
    Unparsed,
    /// Parsed at buffer revision `rev`; `text` is what highlight queries read.
    Parsed { rev: u64, text: String },
}

/// Horizontal view position of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HView {
    /// First visible display column.
    pub offset: usize,
    pub mode: HMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HMode {
    /// Keep the cursor on screen (typing, moving).
    FollowCursor,
    /// Scrolled by wheel/trackpad: stay put until the cursor leaves `anchor`.
    Manual { anchor: skink_core::Cursor },
}

impl HView {
    pub const START: HView = HView {
        offset: 0,
        mode: HMode::FollowCursor,
    };
}

/// Columns one horizontal wheel notch moves.
pub const HWHEEL_COLS: usize = 6;

pub struct Doc {
    pub id: DocId,
    pub editor: Editor,
    /// Canonical path, resolved once: the key for LSP, de-duplication and
    /// the file tree highlight (no `canonicalize` syscall per frame).
    pub canon: Option<PathBuf>,
    pub hview: HView,
    pub git_branch: Option<String>,
    pub lsp: LspSync,
    pub syntax: Option<Syntax>,
    /// Conflict blocks and the revision they were computed at.
    conflicts: Option<(u64, Vec<Conflict>)>,
}

impl Doc {
    pub fn new(id: DocId, editor: Editor, syntax: bool) -> Self {
        let canon = editor.buffer.path().map(canonical);
        let git_branch = editor.buffer.path().and_then(git_branch);
        let mut doc = Doc {
            id,
            editor,
            canon,
            hview: HView::START,
            git_branch,
            lsp: LspSync::default(),
            syntax: None,
            conflicts: None,
        };
        doc.reset_syntax(syntax);
        doc
    }

    /// Called when the doc leaves the screen. A parse tree costs ~35x the file
    /// size (10 MB for a 300 KB Rust file), so big files drop it and the text
    /// copy it was built from; switching back re-parses (~25 ms at 300 KB).
    /// Small files keep theirs so ordinary tab switches stay instant.
    pub fn park(&mut self) {
        /// Below this a tree is < ~2 MB and re-parsing would only cost latency.
        const KEEP_BELOW: usize = 64 << 10;
        if let Some(syn) = &mut self.syntax
            && let ParseState::Parsed { text, .. } = &syn.parse
            && text.len() >= KEEP_BELOW
        {
            syn.hl.clear();
            syn.parse = ParseState::Unparsed;
        }
    }

    /// The bracket pair at (or just before) the cursor as `(open, close)`
    /// char indices: from the syntax tree when it is current (brackets in
    /// strings and comments do not count), else by plain counting.
    pub fn bracket_pair(&mut self) -> Option<(usize, usize)> {
        let ed = &self.editor;
        let at = ed.idx(ed.cursor);
        let rev = ed.buffer.revision();
        if let Some(syn) = &mut self.syntax
            && let ParseState::Parsed { rev: r, text } = &syn.parse
            && *r == rev
        {
            let b = &ed.buffer;
            return syn
                .hl
                .bracket_pair(text, b.char_to_byte(at))
                .map(|(o, c)| (b.byte_to_char(o), b.byte_to_char(c)));
        }
        ed.bracket_pair_plain()
    }

    /// Jump to the other bracket of the pair at the cursor.
    pub fn jump_to_matching_bracket(&mut self) {
        let Some((open, close)) = self.bracket_pair() else {
            return;
        };
        let at = self.editor.idx(self.editor.cursor);
        let to = if at == open || at == open + 1 {
            close
        } else {
            open
        };
        let c = self.editor.cursor_at(to);
        self.editor.click(c.line, c.col, false);
    }

    /// Scroll the view sideways without moving the cursor.
    pub fn scroll_horizontally(&mut self, delta: isize) {
        self.hview = HView {
            offset: self.hview.offset.saturating_add_signed(delta),
            mode: HMode::Manual {
                anchor: self.editor.cursor,
            },
        };
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
                parse: ParseState::Unparsed,
            });
    }

    /// Conflict blocks, recomputed only when the text changed.
    pub fn conflicts(&mut self) -> &[Conflict] {
        let rev = self.editor.buffer.revision();
        let fresh = matches!(&self.conflicts, Some((r, _)) if *r == rev);
        if !fresh {
            self.conflicts = Some((rev, self.editor.conflicts()));
        }
        self.conflicts.as_ref().map_or(&[], |(_, c)| c)
    }

    /// Replace the contents of a read-only view (command output arriving).
    pub fn replace_view(&mut self, title: &str, text: &str) {
        let tab = self.editor.tab_width;
        self.editor = Editor::new(Buffer::view(title, text));
        self.editor.tab_width = tab;
        self.hview = HView::START;
    }
}

/// `canonicalize`, falling back to the path itself (new, unsaved files).
/// On Windows the `\\?\` verbatim prefix is dropped (`\\?\C:\x` -> `C:\x`,
/// `\\?\UNC\srv\x` -> `\\srv\x`) so paths match what users type, what
/// language servers send back and what agents ask for.
pub fn canonical(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    match c.to_str().and_then(strip_verbatim) {
        Some(plain) => PathBuf::from(plain),
        None => c,
    }
}

fn strip_verbatim(p: &str) -> Option<String> {
    if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    let rest = p.strip_prefix(r"\\?\")?;
    // Only drive paths (`C:\...`); other verbatim forms have no plain form.
    let b = rest.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':').then(|| rest.to_owned())
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
    fn verbatim_prefix_is_dropped() {
        assert_eq!(strip_verbatim(r"\\?\C:\x\y").as_deref(), Some(r"C:\x\y"));
        assert_eq!(
            strip_verbatim(r"\\?\UNC\srv\s").as_deref(),
            Some(r"\\srv\s")
        );
        assert_eq!(strip_verbatim(r"\\?\Volume{1}\x"), None);
        assert_eq!(strip_verbatim("/home/x"), None);
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
