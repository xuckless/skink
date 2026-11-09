//! One open document: an `Editor` plus everything the UI remembers per file
//! (scroll, branch, LSP sync state, syntax tree, cached conflict blocks).
//! Keeping it in one struct means switching tabs is a single swap and no
//! per-file state can leak from one tab into another.

use crate::disk::{Backup, DiskWatch, SizeClass};
use skink_core::{Buffer, Conflict, Editor};
use skink_term::WakeFn;
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

/// Per-document syntax highlighting state. What is on screen (`shown`) and
/// whether a background parse is running (`job`) are independent: while a
/// big file re-parses, the previous colours stay up.
pub struct Syntax {
    /// Holds the tree for `shown`.
    pub hl: skink_syntax::Highlighter,
    pub shown: Shown,
    pub job: ParseJob,
}

/// The tree `Syntax::hl` holds.
pub enum Shown {
    /// No tree (new, or dropped while parked).
    Nothing,
    /// Parsed at buffer revision `rev`; `text` is what highlight queries read.
    Tree {
        rev: u64,
        text: String,
        cover: Cover,
    },
}

/// Which lines a tree was parsed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cover {
    Whole,
    /// Big files parse a window around the view (`first..last`, 0-based,
    /// half-open): the tree then costs the window, not the file.
    Lines {
        first: usize,
        last: usize,
    },
}

impl Cover {
    fn covers(self, first: usize, last: usize) -> bool {
        match self {
            Cover::Whole => true,
            Cover::Lines { first: a, last: b } => a <= first && last <= b,
        }
    }
}

impl Shown {
    /// The tree's text when it is at `rev` and covers `line`.
    pub fn at(&self, rev: u64, line: usize) -> Option<&str> {
        match self {
            Shown::Tree {
                rev: r,
                text,
                cover,
            } if *r == rev && cover.covers(line, line + 1) => Some(text),
            _ => None,
        }
    }
}

/// A parse running on a worker thread.
pub enum ParseJob {
    Idle,
    /// Parsing the text of revision `rev`; the result arrives on `rx`.
    Running {
        rev: u64,
        rx: std::sync::mpsc::Receiver<(skink_syntax::Highlighter, String, Cover)>,
    },
}

/// Texts this large parse only a window of lines around the view.
const WINDOW_FROM: usize = 256 << 10;
/// Lines parsed above and below the view in a window.
const WINDOW_MARGIN: usize = 1500;

fn to_syntax_edit(e: &skink_core::ByteEdit) -> skink_syntax::Edit {
    let p = |p: skink_core::TextPoint| skink_syntax::Pos {
        row: p.row,
        col: p.col,
    };
    skink_syntax::Edit {
        start_byte: e.start_byte,
        old_end_byte: e.old_end_byte,
        new_end_byte: e.new_end_byte,
        start_pos: p(e.start),
        old_end_pos: p(e.old_end),
        new_end_pos: p(e.new_end),
    }
}

/// Texts this large parse on a worker; smaller ones parse in well under a
/// frame, so inline is simpler and never shows stale colours.
const PARSE_ASYNC_FROM: usize = 64 << 10;

impl Syntax {
    fn new(lang: skink_syntax::Lang) -> Self {
        Syntax {
            hl: skink_syntax::Highlighter::new(lang),
            shown: Shown::Nothing,
            job: ParseJob::Idle,
        }
    }

    #[cfg(test)]
    pub fn for_test(lang: skink_syntax::Lang) -> Self {
        Self::new(lang)
    }

    /// The tree's text when it matches buffer revision `rev` and covers
    /// lines `view` (`first..last`).
    pub fn current(&self, rev: u64, view: (usize, usize)) -> Option<&str> {
        match &self.shown {
            Shown::Tree {
                rev: r,
                text,
                cover,
            } if *r == rev && cover.covers(view.0, view.1) => Some(text),
            _ => None,
        }
    }

    /// Bring the tree up to the buffer's revision: take a finished worker
    /// result, then parse again if the buffer moved on. When the shown tree's
    /// revision is still in the buffer's edit journal the parse is
    /// incremental (edits applied to a copy of the tree, which shares every
    /// unchanged subtree), so a background re-parse costs the changed part,
    /// not a second full tree. Big texts parse on a worker when `wake` can
    /// tell the event loop the result is ready; without one (tests,
    /// snapshots) everything parses inline.
    pub fn refresh(&mut self, buf: &Buffer, wake: Option<&WakeFn>, view: (usize, usize)) {
        let rev = buf.revision();
        if let ParseJob::Running { rev: job_rev, rx } = &self.job {
            match rx.try_recv() {
                Ok((hl, text, cover)) => {
                    // Even a stale result beats no colours; re-parsed below.
                    self.shown = Shown::Tree {
                        rev: *job_rev,
                        text,
                        cover,
                    };
                    self.hl = hl;
                    self.job = ParseJob::Idle;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.job = ParseJob::Idle,
            }
        }
        if self.current(rev, view).is_some() {
            return;
        }
        let text = buf.text();
        // A big file parses a window around the view (never incremental:
        // the window moves); others the whole text, incrementally when the
        // shown tree's revision is still in the edit journal.
        let cover = if text.len() >= WINDOW_FROM {
            let lines = buf.len_lines();
            Cover::Lines {
                first: view.0.saturating_sub(WINDOW_MARGIN),
                last: (view.1 + WINDOW_MARGIN).min(lines),
            }
        } else {
            Cover::Whole
        };
        let window = match cover {
            Cover::Whole => None,
            Cover::Lines { first, last } => {
                let byte = |line: usize| match line < buf.len_lines() {
                    true => buf.char_to_byte(buf.line_to_char(line)),
                    false => text.len(),
                };
                Some(byte(first)..byte(last))
            }
        };
        let edits: Option<Vec<skink_syntax::Edit>> = match (&self.shown, &window) {
            (
                Shown::Tree {
                    rev: r,
                    cover: Cover::Whole,
                    ..
                },
                None,
            ) => buf
                .edits_since(*r)
                .map(|es| es.iter().map(to_syntax_edit).collect()),
            _ => None,
        };
        // The first parse of a document also runs on the worker: opening a
        // file then draws at once (uncoloured for a moment) instead of
        // waiting for the whole parse (Markdown: ~3 ms per 20 KB).
        let first = matches!(self.shown, Shown::Nothing);
        match wake {
            Some(wake) if first || text.len() >= PARSE_ASYNC_FROM => {
                let (tx, rx) = std::sync::mpsc::channel();
                let mut hl = match edits {
                    Some(_) => self.hl.snapshot(),
                    None => skink_syntax::Highlighter::new(self.hl.lang()),
                };
                let wake = wake.clone();
                std::thread::spawn(move || {
                    match (edits, window) {
                        (_, Some(w)) => hl.update_window(&text, w),
                        (Some(e), None) => hl.update_with_edits(&text, &e),
                        (None, None) => hl.update(&text),
                    }
                    if tx.send((hl, text, cover)).is_ok() {
                        wake();
                    }
                });
                self.job = ParseJob::Running { rev, rx };
            }
            _ => {
                match (edits, window) {
                    (_, Some(w)) => self.hl.update_window(&text, w),
                    (Some(e), None) => self.hl.update_with_edits(&text, &e),
                    (None, None) => self.hl.update(&text),
                }
                self.shown = Shown::Tree { rev, text, cover };
            }
        }
    }

    /// Drop the tree and any running parse (memory for a parked tab).
    fn forget(&mut self) {
        self.hl.clear();
        self.shown = Shown::Nothing;
        self.job = ParseJob::Idle;
    }
}

/// Lines of a results view (references, problems) that lead somewhere:
/// Enter on such a line jumps there.
#[derive(Debug, Default)]
pub enum Links {
    #[default]
    None,
    /// One entry per line of the view.
    Lines(Vec<Option<Jump>>),
}

/// A place to jump to: 0-based line and char column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jump {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
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
    pub size: SizeClass,
    pub disk: DiskWatch,
    pub backup: Backup,
    pub growth: Growth,
    pub links: Links,
    pub kind: DocKind,
    /// Change marks and blame against git.
    pub git: crate::gitui::DocGit,
    /// Opened for another tool (`$EDITOR`): lean, and someone waits.
    pub role: DocRole,
}

/// Whether a document is an ordinary one or was opened for another tool.
#[derive(Default)]
pub enum DocRole {
    #[default]
    Normal,
    Lean {
        lean: crate::lean::Lean,
        waiter: crate::handoff::Waiter,
    },
}

impl DocRole {
    pub fn lean(&self) -> Option<&crate::lean::Lean> {
        match self {
            DocRole::Normal => None,
            DocRole::Lean { lean, .. } => Some(lean),
        }
    }
}

/// What a document tab shows: editable/viewable text, or a rendered preview
/// of another file (its text is the rendered output, kept read-only).
#[derive(Default)]
pub enum DocKind {
    #[default]
    Text,
    Preview(Box<crate::preview::Preview>),
    /// A git view (status panel, blame, branches, stashes, side-by-side
    /// diff): read-only text plus what each row stands for.
    Git(Box<crate::gitui::GitView>),
    /// The debug panel (stack, variables, watch, breakpoints, console).
    Debug(Box<crate::debugview::DebugView>),
}

/// Selections grown by "expand selection", so "shrink" can walk back.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Growth {
    #[default]
    None,
    /// `stack` holds earlier selections as char ranges, innermost first;
    /// valid while the buffer is at `rev` and the selection is `current`.
    Grown {
        rev: u64,
        current: (usize, usize),
        stack: Vec<(usize, usize)>,
    },
}

impl Doc {
    pub fn new(id: DocId, editor: Editor, syntax: bool) -> Self {
        let canon = editor.buffer.path().map(canonical);
        let git_branch = editor.buffer.path().and_then(git_branch);
        let editor_len = editor.buffer.len_chars();
        let mut doc = Doc {
            id,
            editor,
            canon,
            hview: HView::START,
            git_branch,
            lsp: LspSync::default(),
            syntax: None,
            conflicts: None,
            size: SizeClass::of(editor_len),
            disk: DiskWatch::InSync,
            backup: Backup::None,
            growth: Growth::None,
            links: Links::None,
            kind: DocKind::Text,
            git: crate::gitui::DocGit::default(),
            role: DocRole::Normal,
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
            && let Shown::Tree { text, .. } = &syn.shown
            && text.len() >= KEEP_BELOW
        {
            syn.forget();
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
            && let Some(text) = syn.shown.at(rev, ed.cursor.line)
        {
            let b = &ed.buffer;
            return syn
                .hl
                .bracket_pair(text, b.char_to_byte(at))
                .map(|(o, c)| (b.byte_to_char(o), b.byte_to_char(c)));
        }
        ed.bracket_pair_plain()
    }

    /// The selection as a char range (empty at the cursor when none).
    pub(crate) fn sel_range(&self) -> (usize, usize) {
        self.editor
            .selection()
            .unwrap_or_else(|| (self.editor.cursor_idx(), self.editor.cursor_idx()))
    }

    pub(crate) fn select_range(&mut self, (start, end): (usize, usize)) {
        let ed = &mut self.editor;
        ed.remove_all_cursors();
        ed.anchor = (start != end).then(|| ed.cursor_at(start));
        ed.cursor = ed.cursor_at(end);
        ed.scroll_into_view();
    }

    /// Definitions around the cursor (`impl Foo`, `fn bar`), from the
    /// current syntax tree only (never parses for this).
    pub fn scopes(&mut self) -> Vec<String> {
        let ed = &self.editor;
        let rev = ed.buffer.revision();
        let at = ed.buffer.char_to_byte(ed.cursor_idx());
        let line = ed.cursor.line;
        match &mut self.syntax {
            Some(syn) => match syn.shown.at(rev, line) {
                Some(text) => syn.hl.scopes(text, at),
                None => Vec::new(),
            },
            None => Vec::new(),
        }
    }

    /// Alt-O: grow the selection to the enclosing syntax node.
    pub fn expand_selection(&mut self) -> bool {
        let rev = self.editor.buffer.revision();
        let cur = self.sel_range();
        let mut stack = match std::mem::take(&mut self.growth) {
            Growth::Grown {
                rev: r,
                current,
                stack,
            } if r == rev && current == cur => stack,
            _ => Vec::new(),
        };
        let Some(syn) = &mut self.syntax else {
            return false;
        };
        let b = &self.editor.buffer;
        let line = self.editor.cursor.line;
        syn.refresh(b, None, (line, line + 1)); // needs a current tree now
        let Shown::Tree { text, .. } = &syn.shown else {
            return false;
        };
        let Some((s, e)) =
            syn.hl
                .enclosing_node(text, b.char_to_byte(cur.0), b.char_to_byte(cur.1))
        else {
            return false;
        };
        let next = (b.byte_to_char(s), b.byte_to_char(e));
        stack.push(cur);
        self.select_range(next);
        self.growth = Growth::Grown {
            rev,
            current: next,
            stack,
        };
        true
    }

    /// Alt-I: back to the selection before the last expand.
    pub fn shrink_selection(&mut self) -> bool {
        let rev = self.editor.buffer.revision();
        let cur = self.sel_range();
        let Growth::Grown {
            rev: r,
            current,
            mut stack,
        } = std::mem::take(&mut self.growth)
        else {
            return false;
        };
        if r != rev || current != cur {
            return false;
        }
        let Some(prev) = stack.pop() else {
            return false;
        };
        self.select_range(prev);
        if !stack.is_empty() {
            self.growth = Growth::Grown {
                rev,
                current: prev,
                stack,
            };
        }
        true
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
        self.canon.is_some()
            && !self.editor.buffer.is_read_only()
            && self.size == SizeClass::Normal
            && matches!(self.role, DocRole::Normal)
    }

    /// Colour with `lang` whatever the file name says (scratch notes with
    /// no extension read as Markdown).
    pub fn force_syntax(&mut self, lang: skink_syntax::Lang) {
        if self.size == SizeClass::Normal {
            self.syntax = Some(Syntax::new(lang));
        }
    }

    /// Re-resolve the path after "save as".
    pub fn path_changed(&mut self, syntax: bool) {
        self.canon = self.editor.buffer.path().map(canonical);
        self.git_branch = self.editor.buffer.path().and_then(git_branch);
        self.editor.comment = self.editor.buffer.path().and_then(comment_token);
        self.editor.blocks = self.editor.buffer.path().map_or(
            skink_core::BlockStart::Brackets,
            skink_core::BlockStart::for_path,
        );
        self.reset_syntax(syntax);
    }

    /// (Re)create the highlighter for this file's language; parsing is lazy.
    pub fn reset_syntax(&mut self, enabled: bool) {
        self.syntax = self
            .editor
            .buffer
            .path()
            .filter(|_| enabled && self.size == SizeClass::Normal)
            .and_then(skink_syntax::language_for_path)
            .map(Syntax::new);
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
        self.links = Links::None;
    }
}

/// Apply the editing settings to an editor: tab size, indent style (the
/// file's own when it has one), auto-pairs, comment token for its file type.
pub fn apply_edit_settings(ed: &mut Editor, s: &skink_config::Settings) {
    ed.tab_width = s.tabsize;
    let fallback = if s.tabs_to_spaces {
        skink_core::Indent::Spaces
    } else {
        skink_core::Indent::Tabs
    };
    ed.indent = skink_core::Indent::detect(&ed.buffer).unwrap_or(fallback);
    ed.auto_pairs = s.autoclose;
    ed.comment = ed.buffer.path().and_then(comment_token);
    ed.blocks = ed.buffer.path().map_or(
        skink_core::BlockStart::Brackets,
        skink_core::BlockStart::for_path,
    );
}

/// Line-comment token by file name / extension.
pub fn comment_token(p: &Path) -> Option<&'static str> {
    let name = p.file_name()?.to_str()?;
    if matches!(
        name,
        "Makefile" | "makefile" | "Dockerfile" | ".gitignore" | ".env"
    ) {
        return Some("#");
    }
    Some(match p.extension()?.to_str()? {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "go" | "zig" | "c" | "h" | "cc"
        | "cpp" | "hpp" | "java" | "kt" | "kts" | "cs" | "swift" | "dart" | "scala" | "scss"
        | "jsonc" | "proto" | "php" | "glsl" | "wgsl" => "//",
        "py" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yaml" | "yml" | "rb" | "nix" | "pl"
        | "r" | "conf" | "cmake" | "ps1" | "tf" | "ex" | "exs" | "jl" | "nu" => "#",
        "lua" | "sql" | "hs" | "elm" | "ada" => "--",
        "ini" | "asm" | "s" | "clj" | "lisp" | "el" | "scm" => ";",
        "tex" | "erl" | "m" => "%",
        "vim" => "\"",
        _ => return None,
    })
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
