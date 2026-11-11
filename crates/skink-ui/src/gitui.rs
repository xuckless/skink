//! Git in the editor: change marks in the gutter, hunk stage / unstage /
//! revert with an inline peek, blame (inline and as a tab), the git panel
//! (status, commit, branches, stashes, push / pull in a terminal), a
//! side-by-side diff and status colours for the file tree.
//!
//! All git work runs on worker threads ([`crate::git`] is the blocking
//! half) and reports back through one channel; the loop is woken, never
//! polled. Every piece of state is an explicit enum.

use crate::{
    App, Overlay,
    doc::{DocId, DocKind, Jump},
    git::{self, Base, Blame, FileRef, FileState, Hunk, HunkKind, Hunks, Status},
    prompt::Choice,
};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use skink_config::{Key, KeySpec};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

/// A gutter glyph and its colour.
pub type GutterMark = (&'static str, (u8, u8, u8));

/// Change marks of one document.
#[derive(Default)]
pub enum Marks {
    /// Not asked yet (the document has not been shown).
    #[default]
    Unknown,
    /// The index and HEAD versions are being read; `prev` stays on screen.
    Loading {
        prev: Option<Box<Tracked>>,
    },
    /// Known, but git changed underneath (commit, stage, checkout): shown
    /// until the reload lands.
    Stale(Box<Tracked>),
    Ready(Box<Tracked>),
    /// Not a file in a git work tree, too large, or not a text tab.
    Off,
}

pub struct Tracked {
    pub file: FileRef,
    pub base: Base,
    pub hunks: Hunks,
    /// Buffer revision `hunks` were computed at.
    pub rev: u64,
}

/// Blame of one document, valid for one buffer revision.
#[derive(Default)]
pub enum BlameState {
    #[default]
    None,
    Loading,
    Ready {
        rev: u64,
        blame: Box<Blame>,
    },
    Failed {
        rev: u64,
    },
}

#[derive(Default)]
pub struct DocGit {
    pub marks: Marks,
    pub blame: BlameState,
}

impl DocGit {
    fn tracked(&self) -> Option<&Tracked> {
        match &self.marks {
            Marks::Ready(t) | Marks::Stale(t) | Marks::Loading { prev: Some(t) } => Some(t),
            Marks::Unknown | Marks::Loading { prev: None } | Marks::Off => None,
        }
    }

    fn tracked_mut(&mut self) -> Option<&mut Tracked> {
        match &mut self.marks {
            Marks::Ready(t) | Marks::Stale(t) | Marks::Loading { prev: Some(t) } => Some(t),
            Marks::Unknown | Marks::Loading { prev: None } | Marks::Off => None,
        }
    }

    /// Mark for a reload (git changed underneath).
    fn invalidate(&mut self) {
        self.marks = match std::mem::take(&mut self.marks) {
            Marks::Ready(t) | Marks::Stale(t) => Marks::Stale(t),
            Marks::Off => Marks::Off,
            m @ (Marks::Unknown | Marks::Loading { .. }) => m,
        };
        self.blame = BlameState::None;
    }
}

/// Git status for the file tree.
#[derive(Default)]
pub enum TreeStatus {
    #[default]
    Unknown,
    Loading(Option<Arc<git::TreeMarks>>),
    Stale(Arc<git::TreeMarks>),
    Ready(Arc<git::TreeMarks>),
    /// The tree is not inside a work tree.
    Off,
}

impl TreeStatus {
    fn marks(&self) -> Option<Arc<git::TreeMarks>> {
        match self {
            TreeStatus::Loading(Some(m)) | TreeStatus::Stale(m) | TreeStatus::Ready(m) => {
                Some(m.clone())
            }
            TreeStatus::Unknown | TreeStatus::Loading(None) | TreeStatus::Off => None,
        }
    }
}

/// The inline diff box under a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peek {
    Closed,
    Open { doc: DocId, line: usize },
}

/// The commit message tab.
pub enum Draft {
    None,
    Editing {
        doc: DocId,
        root: PathBuf,
        path: PathBuf,
        amend: bool,
        signoff: bool,
    },
    /// `git commit` is running for this draft.
    Committing {
        doc: DocId,
    },
}

/// Whether the cursor line shows its blame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineBlame {
    /// Follow `"inlineblame"` in settings.json.
    Setting,
    On,
    Off,
}

/// A clickable "Accept ours | theirs | both" label, in screen cells.
#[derive(Debug, Clone, Copy)]
pub struct Button {
    doc: DocId,
    x: Range16,
    y: u16,
    line: usize,
    action: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct Range16 {
    start: u16,
    end: u16,
}

/// Git operations that lose work: asked about first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Danger {
    /// Throw away the unstaged changes of a tracked file.
    Discard {
        root: PathBuf,
        path: String,
    },
    DropStash {
        root: PathBuf,
        name: String,
    },
    DeleteBranch {
        root: PathBuf,
        name: String,
    },
}

impl Danger {
    pub fn question(&self) -> String {
        match self {
            Danger::Discard { path, .. } => {
                format!(
                    "Discard all unstaged changes to {path}? This cannot be undone. (y)es / (n)o "
                )
            }
            Danger::DropStash { name, .. } => {
                format!("Drop {name}? Its changes are lost. (y)es / (n)o ")
            }
            Danger::DeleteBranch { name, .. } => {
                format!("Delete branch {name}? (git refuses if it is not merged) (y)es / (n)o ")
            }
        }
    }
}

enum Msg {
    Base {
        doc: DocId,
        got: Option<(FileRef, Base)>,
    },
    Blame {
        doc: DocId,
        rev: u64,
        got: Result<Blame, String>,
        open_view: bool,
    },
    Status {
        root: PathBuf,
        got: Result<Status, String>,
    },
    Done {
        what: String,
        got: Result<String, String>,
        then: Then,
    },
}

/// What to do after a git command finished.
enum Then {
    Refresh,
    /// The commit of this draft ran.
    Committed(DocId),
}

pub struct GitState {
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    pub tree: TreeStatus,
    pub peek: Peek,
    pub draft: Draft,
    pub inline_blame: InlineBlame,
    buttons: Vec<Button>,
    /// Inline blame re-runs at most this often while typing.
    blame_clock: Instant,
}

impl GitState {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        GitState {
            tx,
            rx,
            tree: TreeStatus::Unknown,
            peek: Peek::Closed,
            draft: Draft::None,
            inline_blame: InlineBlame::Setting,
            buttons: Vec::new(),
            blame_clock: Instant::now() - Duration::from_secs(10),
        }
    }
}

/// What a git view tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewWhat {
    Status {
        root: PathBuf,
    },
    Blame,
    Branches {
        root: PathBuf,
    },
    Stashes {
        root: PathBuf,
    },
    Diff,
    /// Two files side by side (`skink --diff`, `:diff OTHER`).
    Compare(Box<crate::compare::Compare>),
}

/// What one row of a git view stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    None,
    File(git::Entry),
    Commit(String),
    Branch(String),
    Stash(String),
    Jump(Jump),
    /// A row of a comparison: the change it belongs to and its 0-based
    /// line on each side.
    Pair {
        hunk: Option<usize>,
        left: Option<usize>,
        right: Option<usize>,
    },
}

/// Colour roles in git views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tint {
    Dim,
    Head,
    Accent,
    Add,
    Mod,
    Del,
    Warn,
    AddBg,
    DelBg,
    AddNum,
    DelNum,
}

pub struct GitView {
    pub what: ViewWhat,
    pub rows: Vec<Row>,
    /// Per line: char-column ranges and their colour.
    pub tints: Vec<Vec<(usize, usize, Tint)>>,
}

/// Builds a git view line by line.
struct ViewText {
    text: String,
    rows: Vec<Row>,
    tints: Vec<Vec<(usize, usize, Tint)>>,
}

impl ViewText {
    fn new() -> Self {
        ViewText {
            text: String::new(),
            rows: Vec::new(),
            tints: Vec::new(),
        }
    }

    fn line(&mut self, parts: &[(&str, Option<Tint>)], row: Row) {
        let mut col = 0;
        let mut tints = Vec::new();
        for (s, t) in parts {
            let n = s.chars().count();
            if let Some(t) = t
                && n > 0
            {
                tints.push((col, col + n, *t));
            }
            col += n;
            self.text.push_str(s);
        }
        self.text.push('\n');
        self.rows.push(row);
        self.tints.push(tints);
    }

    fn finish(self, what: ViewWhat) -> (String, GitView) {
        (
            self.text,
            GitView {
                what,
                rows: self.rows,
                tints: self.tints,
            },
        )
    }
}

fn first_line(s: &str) -> &str {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

/// `text` cut or padded to exactly `w` chars, tabs as 4 spaces.
fn cell(text: &str, w: usize) -> String {
    let mut out: String = text.replace('\t', "    ").chars().take(w).collect();
    let n = out.chars().count();
    out.extend(std::iter::repeat_n(' ', w - n));
    out
}

impl App {
    // ---- plumbing ------------------------------------------------------

    fn git_spawn(&self, job: impl FnOnce() -> Msg + Send + 'static) {
        let tx = self.git.tx.clone();
        match self.wake.clone() {
            Some(wake) => {
                std::thread::spawn(move || {
                    let _ = tx.send(job());
                    wake();
                });
            }
            // Tests: no event loop, answer right away.
            None => {
                let _ = tx.send(job());
            }
        }
    }

    fn doc_by_id_mut(&mut self, id: DocId) -> Option<&mut crate::doc::Doc> {
        let i = self.find_doc(|d| d.id == id)?;
        self.doc_at_mut(i)
    }

    /// The work tree of the project (sync: one quick `git rev-parse`).
    fn git_root(&mut self) -> Option<PathBuf> {
        if let DocKind::Git(v) = &self.doc.kind
            && let ViewWhat::Status { root }
            | ViewWhat::Branches { root }
            | ViewWhat::Stashes { root } = &v.what
        {
            return Some(root.clone());
        }
        // From the file or the file tree, never the process's cwd.
        let start = self
            .doc
            .canon
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.tree().map(|t| t.root.clone()));
        let root = start
            .and_then(|s| git::toplevel(&s))
            .map(|p| crate::doc::canonical(&p));
        if root.is_none() {
            self.editor_mut().message =
                Some("not inside a git repository (open a file or folder in one)".into());
        }
        root
    }

    /// Run git in `root` on a worker; the result is reported and every git
    /// view refreshed.
    fn git_op(&mut self, what: &str, root: PathBuf, args: Vec<String>) {
        let what = what.to_owned();
        self.editor_mut().message = Some(format!("{what}…"));
        self.git_spawn(move || {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            let got = git::run(&root, &a).map(|o| String::from_utf8_lossy(&o).into_owned());
            Msg::Done {
                what,
                got,
                then: Then::Refresh,
            }
        });
    }

    /// Between events: apply answers, then ask what the screen needs.
    pub(crate) fn poll_git(&mut self) {
        while let Ok(m) = self.git.rx.try_recv() {
            self.git_apply(m);
        }
        self.git_request_marks();
        self.git_request_blame();
        self.git_request_tree();
    }

    fn git_apply(&mut self, m: Msg) {
        match m {
            Msg::Base { doc, got } => {
                let Some(d) = self.doc_by_id_mut(doc) else {
                    return;
                };
                d.git.marks = match got {
                    None => Marks::Off,
                    Some((file, base)) => {
                        let rev = d.editor.buffer.revision();
                        let hunks = git::compute(&base, &d.editor.buffer.text());
                        Marks::Ready(Box::new(Tracked {
                            file,
                            base,
                            hunks,
                            rev,
                        }))
                    }
                };
            }
            Msg::Blame {
                doc,
                rev,
                got,
                open_view,
            } => {
                let Some(d) = self.doc_by_id_mut(doc) else {
                    return;
                };
                match got {
                    Ok(b) => {
                        if open_view {
                            let title = format!("blame {}", d.title());
                            let path = d.canon.clone();
                            let text = d.editor.buffer.text();
                            self.open_blame_view(&title, &b, &text, path.as_deref());
                        }
                        if let Some(d) = self.doc_by_id_mut(doc) {
                            d.git.blame = BlameState::Ready {
                                rev,
                                blame: Box::new(b),
                            };
                        }
                    }
                    Err(e) => {
                        d.git.blame = BlameState::Failed { rev };
                        if open_view {
                            self.editor_mut().message = Some(format!("blame: {}", first_line(&e)));
                        }
                    }
                }
            }
            Msg::Status { root, got } => match got {
                Ok(st) => {
                    self.git.tree = TreeStatus::Ready(Arc::new(git::TreeMarks::new(&root, &st)));
                    self.refresh_status_views(&root, &st);
                }
                Err(e) => {
                    self.git.tree = TreeStatus::Off;
                    self.status_view_failed(&root, &e);
                }
            },
            Msg::Done { what, got, then } => {
                let note = match &got {
                    Ok(out) if first_line(out).is_empty() => format!("{what}: done"),
                    Ok(out) => format!("{what}: {}", first_line(out)),
                    Err(e) => format!("{what} failed: {}", first_line(e)),
                };
                self.editor_mut().message = Some(note);
                if let Then::Committed(doc) = then {
                    match got {
                        Ok(_) => {
                            self.git.draft = Draft::None;
                            if let Some(i) = self.find_doc(|d| d.id == doc) {
                                self.close_tab(i);
                            }
                        }
                        Err(_) => {
                            // Keep the tab so the message is not lost.
                            if let Draft::Committing { doc: d } = self.git.draft {
                                self.git.draft = Draft::None;
                                let _ = d;
                            }
                        }
                    }
                }
                self.check_disk(true);
                self.git_refresh_all();
            }
        }
    }

    /// Git changed (an operation finished, focus came back): reload marks,
    /// status, blame and the open git views.
    pub(crate) fn git_refresh_all(&mut self) {
        for i in 0..self.tabs.len() {
            if let Some(d) = self.doc_at_mut(i) {
                d.git.invalidate();
            }
        }
        self.git.tree = match std::mem::take(&mut self.git.tree) {
            TreeStatus::Ready(m) | TreeStatus::Stale(m) => TreeStatus::Stale(m),
            TreeStatus::Off => TreeStatus::Unknown,
            s @ (TreeStatus::Unknown | TreeStatus::Loading(_)) => s,
        };
        let views: Vec<ViewWhat> = (0..self.tabs.len())
            .filter_map(|i| match &self.doc_at(i)?.kind {
                DocKind::Git(v) => Some(v.what.clone()),
                _ => None,
            })
            .collect();
        for what in views {
            match what {
                ViewWhat::Status { root } => self.request_status(root),
                ViewWhat::Branches { root } => self.show_branches(root, false),
                ViewWhat::Stashes { root } => self.show_stashes(root, false),
                ViewWhat::Blame | ViewWhat::Diff | ViewWhat::Compare(_) => {}
            }
        }
    }

    /// A save happened in the live document.
    pub(crate) fn git_after_save(&mut self) {
        if self.doc.editor.buffer.is_dirty() {
            return;
        }
        if let Draft::Editing {
            doc,
            root,
            path,
            amend,
            signoff,
        } = &self.git.draft
            && *doc == self.doc.id
        {
            let (doc, root, path, amend, signoff) =
                (*doc, root.clone(), path.clone(), *amend, *signoff);
            self.git.draft = Draft::Committing { doc };
            self.editor_mut().message = Some("committing…".into());
            self.git_spawn(move || {
                let p = path.to_string_lossy().into_owned();
                let mut args = vec!["commit", "--cleanup=strip", "-F", p.as_str()];
                if amend {
                    args.push("--amend");
                }
                if signoff {
                    args.push("--signoff");
                }
                let got = git::run(&root, &args).map(|o| String::from_utf8_lossy(&o).into_owned());
                Msg::Done {
                    what: "commit".into(),
                    got,
                    then: Then::Committed(doc),
                }
            });
            return;
        }
        // The work tree changed: the tree colours and the panel follow.
        self.git.tree = match std::mem::take(&mut self.git.tree) {
            TreeStatus::Ready(m) => TreeStatus::Stale(m),
            s => s,
        };
        let roots: Vec<PathBuf> = (0..self.tabs.len())
            .filter_map(|i| match &self.doc_at(i)?.kind {
                DocKind::Git(v) => match &v.what {
                    ViewWhat::Status { root } => Some(root.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        for r in roots {
            self.request_status(r);
        }
    }

    /// Why git line features (blame, diff, hunks) are off for the live
    /// document, when it is not about tracking.
    fn git_off_reason(&self) -> Option<&'static str> {
        let d = &self.doc;
        if !matches!(d.kind, DocKind::Text) {
            Some("git line features are off for this view")
        } else if d.size != crate::disk::SizeClass::Normal {
            Some("git line features are off for very large files")
        } else if d.editor.buffer.is_read_only() {
            Some("git line features are off for read-only (binary or locked) files")
        } else {
            None
        }
    }

    fn git_request_marks(&mut self) {
        let d = &mut self.doc;
        if !matches!(d.git.marks, Marks::Unknown | Marks::Stale(_)) {
            return;
        }
        let eligible = matches!(d.kind, DocKind::Text)
            && d.size == crate::disk::SizeClass::Normal
            && !d.editor.buffer.is_read_only();
        let (Some(path), true) = (d.canon.clone(), eligible) else {
            d.git.marks = Marks::Off;
            return;
        };
        let prev = match std::mem::take(&mut d.git.marks) {
            Marks::Stale(t) => Some(t),
            _ => None,
        };
        d.git.marks = Marks::Loading { prev };
        let id = d.id;
        self.git_spawn(move || {
            let got = git::locate(&path).map(|f| {
                let b = git::load_base(&f);
                (f, b)
            });
            Msg::Base { doc: id, got }
        });
    }

    pub(crate) fn inline_blame_on(&self) -> bool {
        match self.git.inline_blame {
            InlineBlame::Setting => self.settings.inline_blame,
            InlineBlame::On => true,
            InlineBlame::Off => false,
        }
    }

    fn git_request_blame(&mut self) {
        const EVERY: Duration = Duration::from_millis(700);
        if !self.inline_blame_on() || self.git.blame_clock.elapsed() < EVERY {
            return;
        }
        let rev = self.doc.editor.buffer.revision();
        let busy = match &self.doc.git.blame {
            BlameState::Loading => true,
            BlameState::Ready { rev: r, .. } | BlameState::Failed { rev: r } => *r == rev,
            BlameState::None => false,
        };
        if busy {
            return;
        }
        self.start_blame(false);
    }

    /// Blame the live document's buffer on a worker.
    fn start_blame(&mut self, open_view: bool) -> bool {
        let Some(t) = self.doc.git.tracked() else {
            return false;
        };
        if !matches!(t.base, Base::Tracked { .. }) {
            return false;
        }
        let file = t.file.clone();
        let rev = self.doc.editor.buffer.revision();
        let mut text = self.doc.editor.buffer.text();
        // The buffer holds LF; git compares against the CRLF blob, where
        // every line would differ ("not committed").
        if self.doc.editor.buffer.line_ending() == skink_core::LineEnding::CrLf {
            text = text.replace('\n', "\r\n");
        }
        let doc = self.doc.id;
        self.doc.git.blame = BlameState::Loading;
        self.git.blame_clock = Instant::now();
        self.git_spawn(move || Msg::Blame {
            doc,
            rev,
            got: git::blame(&file, &text),
            open_view,
        });
        true
    }

    fn git_request_tree(&mut self) {
        if !matches!(self.git.tree, TreeStatus::Unknown | TreeStatus::Stale(_)) {
            return;
        }
        let Some(root) = self.tree().map(|t| t.root.clone()) else {
            return;
        };
        let prev = match std::mem::take(&mut self.git.tree) {
            TreeStatus::Stale(m) => Some(m),
            _ => None,
        };
        self.git.tree = TreeStatus::Loading(prev);
        self.git_spawn(move || match git::toplevel(&root) {
            Some(top) => {
                let top = crate::doc::canonical(&top);
                let got = git::status(&top, true);
                Msg::Status { root: top, got }
            }
            None => Msg::Status {
                root,
                got: Err("not a repository".into()),
            },
        });
    }

    fn request_status(&mut self, root: PathBuf) {
        self.git_spawn(move || {
            let got = git::status(&root, true);
            Msg::Status { root, got }
        });
    }

    /// Status colours for the file tree (cheap clone of the shared map).
    pub(crate) fn tree_git_marks(&self) -> Option<Arc<git::TreeMarks>> {
        self.git.tree.marks()
    }

    // ---- change marks ----------------------------------------------------

    /// Bring the live document's hunks up to date with its buffer.
    pub(crate) fn fresh_hunks(&mut self) -> Option<&Hunks> {
        let d = &mut self.doc;
        let rev = d.editor.buffer.revision();
        let t = d.git.tracked_mut()?;
        if t.rev != rev {
            t.hunks = git::compute(&t.base, &d.editor.buffer.text());
            t.rev = rev;
        }
        Some(&t.hunks)
    }

    /// Gutter marks for buffer lines `rows`: glyph and colour.
    pub(crate) fn git_gutter(&mut self, rows: &[usize]) -> Vec<Option<GutterMark>> {
        let t = self.theme;
        let last = self.doc.editor.buffer.len_lines().saturating_sub(1);
        let enabled = self.settings.git_gutter;
        let Some(h) = self.fresh_hunks().filter(|_| enabled) else {
            return vec![None; rows.len()];
        };
        let color = |k: HunkKind| match k {
            HunkKind::Added => t.green,
            HunkKind::Modified => t.blue,
            HunkKind::Deleted => t.red,
        };
        let glyph = |h: &Hunk, line: usize, staged: bool| -> Option<&'static str> {
            match h.kind {
                HunkKind::Deleted if h.new.start == line => Some("\u{2594}"),
                // Removed at the very end: mark the last line from below.
                HunkKind::Deleted if h.new.start > last && line == last => Some("\u{2581}"),
                HunkKind::Deleted => None,
                // Shape tells added from modified without colour: a thin
                // bar is new, a thick one changed; dotted when staged.
                _ if h.new.contains(&line) => Some(match (h.kind, staged) {
                    (HunkKind::Added, false) => "\u{258e}",
                    (_, false) => "\u{258c}",
                    (HunkKind::Added, true) => "\u{250a}",
                    (_, true) => "\u{250b}",
                }),
                _ => None,
            }
        };
        rows.iter()
            .map(|&line| {
                let find = |list: &[Hunk], staged: bool| {
                    list.iter()
                        .find_map(|h| glyph(h, line, staged).map(|g| (g, color(h.kind))))
                };
                find(&h.unstaged, false).or_else(|| find(&h.staged, true))
            })
            .collect()
    }

    /// Scrollbar marks: (buffer line, colour) for every unstaged hunk.
    pub(crate) fn git_scroll_marks(&self) -> Vec<(usize, (u8, u8, u8))> {
        let t = &self.theme;
        if !self.settings.git_gutter {
            return Vec::new();
        }
        self.doc.git.tracked().map_or_else(Vec::new, |tr| {
            tr.hunks
                .unstaged
                .iter()
                .map(|h| {
                    let c = match h.kind {
                        HunkKind::Added => t.green,
                        HunkKind::Modified => t.blue,
                        HunkKind::Deleted => t.red,
                    };
                    (h.anchor(), c)
                })
                .collect()
        })
    }

    /// All hunks (unstaged and staged) by line.
    fn all_hunks(&mut self) -> Vec<(Hunk, bool)> {
        let Some(h) = self.fresh_hunks() else {
            return Vec::new();
        };
        let mut all: Vec<(Hunk, bool)> = h
            .unstaged
            .iter()
            .map(|h| (h.clone(), false))
            .chain(h.staged.iter().map(|h| (h.clone(), true)))
            .collect();
        all.sort_by_key(|(h, _)| h.anchor());
        all
    }

    /// `NextHunk` / `PreviousHunk`: move to the next change, wrapping.
    pub(crate) fn goto_hunk(&mut self, forward: bool) {
        let all = self.all_hunks();
        if all.is_empty() {
            let off = self.git_off_reason();
            self.editor_mut().message = Some(match (off, &self.doc.git.marks) {
                (Some(why), Marks::Off) => why.into(),
                (None, Marks::Off) => "no git changes (not in a repository)".into(),
                _ => "no changes against git".into(),
            });
            return;
        }
        let cur = self.doc.editor.cursor.line;
        let mut anchors: Vec<usize> = all.iter().map(|(h, _)| h.anchor()).collect();
        anchors.dedup();
        let target = if forward {
            anchors.iter().find(|&&a| a > cur).or(anchors.first())
        } else {
            anchors.iter().rev().find(|&&a| a < cur).or(anchors.last())
        };
        let Some(&line) = target else { return };
        let n = anchors.iter().position(|&a| a == line).unwrap_or(0) + 1;
        self.record_jump();
        let last = self.doc.editor.buffer.len_lines().saturating_sub(1);
        self.doc.editor.goto(Some(line.min(last) + 1), None);
        self.editor_mut().message = Some(format!("change {n} of {}", anchors.len()));
    }

    pub(crate) fn next_hunk(&mut self) {
        self.goto_hunk(true);
    }

    pub(crate) fn previous_hunk(&mut self) {
        self.goto_hunk(false);
    }

    /// The hunk at the cursor: unstaged first, then staged.
    fn hunk_here(&mut self) -> Option<(Hunk, bool)> {
        let line = self.doc.editor.cursor.line;
        let h = self.fresh_hunks()?;
        h.unstaged
            .iter()
            .find(|h| h.touches(line))
            .map(|h| (h.clone(), false))
            .or_else(|| {
                h.staged
                    .iter()
                    .find(|h| h.touches(line))
                    .map(|h| (h.clone(), true))
            })
    }

    fn no_hunk(&mut self) {
        self.editor_mut().message = Some("no change at the cursor".into());
    }

    pub(crate) fn stage_hunk(&mut self) {
        self.git.peek = Peek::Closed;
        let Some((h, false)) = self.hunk_here() else {
            return self.no_hunk();
        };
        let Some(t) = self.doc.git.tracked() else {
            return;
        };
        let (file, base) = (t.file.clone(), t.base.clone());
        let text = self.doc.editor.buffer.text();
        self.editor_mut().message = Some("staging…".into());
        self.git_spawn(move || {
            let got = match &base {
                Base::Tracked { index, .. } => git::stage_hunk(&file, index, &text, &h),
                // A new file: an empty index entry first, then the hunk.
                Base::Untracked => git::run(&file.root, &["add", "-N", "--", &file.rel])
                    .and_then(|_| git::stage_hunk(&file, "", &text, &h)),
                Base::Unmarked => Err("this file is not tracked".into()),
            };
            Msg::Done {
                what: "stage hunk".into(),
                got: got.map(|()| String::new()),
                then: Then::Refresh,
            }
        });
    }

    pub(crate) fn unstage_hunk(&mut self) {
        self.git.peek = Peek::Closed;
        let line = self.doc.editor.cursor.line;
        let staged = self
            .fresh_hunks()
            .and_then(|h| h.staged.iter().find(|h| h.touches(line)).cloned());
        let Some(h) = staged else {
            self.editor_mut().message = Some("no staged change at the cursor".into());
            return;
        };
        let Some(Tracked {
            file,
            base: Base::Tracked { index, head },
            ..
        }) = self.doc.git.tracked()
        else {
            return;
        };
        let (file, index, head) = (file.clone(), index.clone(), head.clone());
        self.git_spawn(move || {
            let got = match &head {
                Some(head) => git::unstage_hunk(&file, head, &index, &h),
                // Added since HEAD: unstaging a part means unstaging it all.
                None => {
                    git::run(&file.root, &["rm", "--cached", "-q", "--", &file.rel]).map(|_| ())
                }
            };
            Msg::Done {
                what: "unstage hunk".into(),
                got: got.map(|()| String::new()),
                then: Then::Refresh,
            }
        });
    }

    /// Put the index version of the hunk back into the buffer (one undo
    /// step, so Ctrl-Z brings the change back).
    pub(crate) fn revert_hunk(&mut self) {
        self.git.peek = Peek::Closed;
        let Some((h, false)) = self.hunk_here() else {
            return self.no_hunk();
        };
        let old_text = match self.doc.git.tracked().map(|t| &t.base) {
            Some(Base::Tracked { index, .. }) => index.clone(),
            Some(Base::Untracked) => String::new(),
            _ => return,
        };
        let old = git::lines(&old_text);
        let b = &self.doc.editor.buffer;
        let rope_lines = b.len_lines();
        let len = b.len_chars();
        let ends_nl = b.char_at(len.wrapping_sub(1)) == Some('\n');
        let at = |line: usize| {
            if line < rope_lines {
                b.line_to_char(line)
            } else {
                len
            }
        };
        let (start, end) = (at(h.new.start), at(h.new.end));
        let mut text: String = old[h.old.clone()]
            .iter()
            .map(|l| format!("{l}\n"))
            .collect();
        if end == len && !text.is_empty() {
            let keep_nl = if h.old.end == old.len() {
                old_text.ends_with('\n')
            } else {
                true
            };
            if !keep_nl {
                text.pop();
            }
            if start == len && !ends_nl && len > 0 {
                text.insert(0, '\n');
            }
        }
        self.doc.editor.apply_edits(vec![(start, end, text)]);
        self.doc.editor.goto(Some(h.new.start + 1), None);
        self.editor_mut().message = Some("change reverted (Ctrl-Z brings it back)".into());
    }

    // ---- peek ------------------------------------------------------------

    pub(crate) fn peek_hunk(&mut self) {
        match self.hunk_here() {
            Some((h, _)) => {
                self.git.peek = Peek::Open {
                    doc: self.doc.id,
                    line: h.anchor(),
                }
            }
            None => self.no_hunk(),
        }
    }

    /// Keys of the open peek; true when consumed. Any other key closes it.
    pub(crate) fn git_peek_key(&mut self, k: &KeySpec) -> bool {
        let Peek::Open { doc, .. } = self.git.peek else {
            return false;
        };
        self.git.peek = Peek::Closed;
        if doc != self.doc.id || k.ctrl || k.alt {
            return false;
        }
        match k.key {
            Key::Char('s') => self.stage_hunk(),
            Key::Char('u') => self.unstage_hunk(),
            Key::Char('r') => self.revert_hunk(),
            Key::Escape => {}
            _ => return false,
        }
        true
    }

    pub(crate) fn render_git_peek(&mut self, frame: &mut Frame, area: Rect, (cx, cy): (u16, u16)) {
        let Peek::Open { doc, .. } = self.git.peek else {
            return;
        };
        if doc != self.doc.id || !matches!(self.overlay, Overlay::None) {
            return;
        }
        let Some((h, staged)) = self.hunk_here() else {
            self.git.peek = Peek::Closed;
            return;
        };
        let t = self.theme;
        let old_text = match self.doc.git.tracked().map(|t| &t.base) {
            Some(Base::Tracked { head, .. }) if staged => head.clone().unwrap_or_default(),
            Some(Base::Tracked { index, .. }) => index.clone(),
            _ => String::new(),
        };
        let old = git::lines(&old_text);
        const MAX: usize = 10;
        let mut lines: Vec<Line> = Vec::new();
        let max_w = (area.width as usize).saturating_sub(6).clamp(20, 100);
        let push = |lines: &mut Vec<Line>, sign: &str, text: &str, fg, bg| {
            lines.push(Line::from(Span::styled(
                cell(&format!("{sign} {text}"), max_w),
                Style::new().fg(t.c(fg)).bg(t.c(bg)),
            )));
        };
        let old_range = h.old.clone();
        for (n, i) in old_range.clone().enumerate() {
            if n == MAX {
                push(
                    &mut lines,
                    " ",
                    &format!("… {} more", old_range.len() - MAX),
                    t.comment,
                    t.bg_popup,
                );
                break;
            }
            push(
                &mut lines,
                "-",
                old.get(i).copied().unwrap_or(""),
                t.red,
                t.diff_del,
            );
        }
        if !staged {
            let ed = &self.doc.editor;
            for (n, i) in h.new.clone().enumerate() {
                if n == MAX {
                    push(
                        &mut lines,
                        " ",
                        &format!("… {} more", h.new.len() - MAX),
                        t.comment,
                        t.bg_popup,
                    );
                    break;
                }
                push(
                    &mut lines,
                    "+",
                    &ed.buffer.line_text(i),
                    t.green,
                    t.diff_add,
                );
            }
        } else {
            push(
                &mut lines,
                " ",
                "(staged: the index has the new lines)",
                t.comment,
                t.bg_popup,
            );
        }
        let keys = if staged {
            " u unstage \u{b7} Esc "
        } else {
            " s stage \u{b7} r revert \u{b7} Esc "
        };
        let w = (max_w as u16 + 2).min(area.width);
        let h_rows = (lines.len() as u16 + 2).min(area.height);
        let y = if cy + 1 + h_rows <= area.bottom() {
            cy + 1
        } else {
            cy.saturating_sub(h_rows)
        };
        let rect = Rect {
            x: cx.min(area.right().saturating_sub(w)).max(area.x),
            y: y.max(area.y),
            width: w,
            height: h_rows,
        };
        frame.render_widget(Clear, rect);
        let title = if staged {
            " staged change "
        } else {
            " change "
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.c(t.blue)))
            .style(Style::new().bg(t.c(t.bg_popup)).fg(t.c(t.fg)))
            .title(Span::styled(title, Style::new().fg(t.c(t.blue))))
            .title_bottom(
                Line::styled(keys, Style::new().fg(t.c(t.comment))).alignment(Alignment::Right),
            );
        frame.render_widget(Paragraph::new(lines).block(block), rect);
    }

    // ---- blame -----------------------------------------------------------

    pub(crate) fn toggle_inline_blame(&mut self) {
        let on = !self.inline_blame_on();
        self.git.inline_blame = if on {
            InlineBlame::On
        } else {
            InlineBlame::Off
        };
        self.git.blame_clock = Instant::now() - Duration::from_secs(10);
        self.editor_mut().message = Some(format!("inline blame {}", if on { "on" } else { "off" }));
    }

    /// Text for the cursor line's blame, when it is known for this revision.
    pub(crate) fn inline_blame_text(&self) -> Option<String> {
        if !self.inline_blame_on() {
            return None;
        }
        let BlameState::Ready { rev, blame } = &self.doc.git.blame else {
            return None;
        };
        if *rev != self.doc.editor.buffer.revision() {
            return None;
        }
        let c = blame.at(self.doc.editor.cursor.line)?;
        Some(if c.uncommitted() {
            "    You \u{b7} not committed yet".to_owned()
        } else {
            let mut s = format!(
                "    {}, {} \u{b7} {}",
                c.author,
                git::relative(git::now() - c.time),
                c.summary
            );
            if s.chars().count() > 80 {
                s = s.chars().take(79).collect::<String>() + "\u{2026}";
            }
            s
        })
    }

    /// `:blame`: the whole file's blame in a tab.
    pub(crate) fn blame_view(&mut self) {
        if !self.start_blame(true) {
            let off = self.git_off_reason();
            self.editor_mut().message = Some(match (off, &self.doc.git.marks) {
                (Some(why), _) => why.into(),
                (None, Marks::Unknown | Marks::Loading { prev: None }) => {
                    "git is still loading; try again".into()
                }
                (None, _) => "blame needs a file tracked by git".into(),
            });
        }
    }

    fn open_blame_view(&mut self, title: &str, b: &Blame, text: &str, path: Option<&Path>) {
        let now = git::now();
        let mut v = ViewText::new();
        for (i, line) in git::lines(text).iter().enumerate() {
            let jump = path.map(|p| {
                Row::Jump(Jump {
                    path: p.to_path_buf(),
                    line: i,
                    col: 0,
                })
            });
            match b.at(i) {
                Some(c) if !c.uncommitted() => {
                    let hash = &c.hash[..8.min(c.hash.len())];
                    v.line(
                        &[
                            (hash, Some(Tint::Accent)),
                            (" ", None),
                            (&cell(&c.author, 14), None),
                            (" ", None),
                            (&cell(&git::relative(now - c.time), 14), Some(Tint::Dim)),
                            (" \u{2502} ", Some(Tint::Dim)),
                            (line, None),
                        ],
                        Row::Commit(c.hash.clone()),
                    );
                }
                _ => v.line(
                    &[
                        (
                            "\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}\u{b7} ",
                            Some(Tint::Dim),
                        ),
                        (&cell("You", 14), Some(Tint::Mod)),
                        (" ", None),
                        (&cell("not committed", 14), Some(Tint::Dim)),
                        (" \u{2502} ", Some(Tint::Dim)),
                        (line, None),
                    ],
                    jump.unwrap_or(Row::None),
                ),
            }
        }
        let (text, view) = v.finish(ViewWhat::Blame);
        self.open_git_view(title, &text, view);
    }

    pub(crate) fn replace_git_view_at(&mut self, i: usize, title: &str, text: &str, view: GitView) {
        self.replace_git_view(i, title, text, view);
    }

    pub(crate) fn open_git_view(&mut self, title: &str, text: &str, view: GitView) {
        self.open_view(title, text);
        self.doc.kind = DocKind::Git(Box::new(view));
    }

    /// Rebuild an open git view in place, keeping the cursor line.
    fn replace_git_view(&mut self, i: usize, title: &str, text: &str, view: GitView) {
        if let Some(d) = self.doc_at_mut(i) {
            let line = d.editor.cursor.line;
            d.replace_view(title, text);
            d.editor.goto(Some(line + 1), None);
            d.kind = DocKind::Git(Box::new(view));
        }
    }

    // ---- git views: syntax and keys ---------------------------------------

    /// Styles of git view lines `first..last` (instead of syntax colours).
    pub(crate) fn git_view_styles(
        &self,
        first: usize,
        last: usize,
    ) -> Option<Vec<Vec<(usize, usize, Style)>>> {
        let tints = match &self.doc.kind {
            DocKind::Git(v) => &v.tints,
            DocKind::Debug(_) => self.debug_view_tints()?,
            _ => return None,
        };
        let t = &self.theme;
        let style = |tint: Tint| match tint {
            Tint::Dim => Style::new().fg(t.c(t.comment)),
            Tint::Head => Style::new().fg(t.c(t.blue)).add_modifier(Modifier::BOLD),
            Tint::Accent => Style::new().fg(t.c(t.purple)),
            Tint::Add => Style::new().fg(t.c(t.green)),
            Tint::Mod => Style::new().fg(t.c(t.yellow)),
            Tint::Del => Style::new().fg(t.c(t.red)),
            Tint::Warn => Style::new().fg(t.c(t.orange)).add_modifier(Modifier::BOLD),
            Tint::AddBg => Style::new().bg(t.c(t.diff_add)),
            Tint::DelBg => Style::new().bg(t.c(t.diff_del)),
            Tint::AddNum => Style::new().fg(t.c(t.green)).bg(t.c(t.diff_add)),
            Tint::DelNum => Style::new().fg(t.c(t.red)).bg(t.c(t.diff_del)),
        };
        Some(
            (first..last)
                .map(|i| {
                    tints
                        .get(i)
                        .into_iter()
                        .flatten()
                        .map(|&(s, e, tint)| (s, e, style(tint)))
                        .collect()
                })
                .collect(),
        )
    }

    /// Plain keys in a git view tab. True when consumed.
    pub(crate) fn git_view_key(&mut self, k: &KeySpec) -> bool {
        let DocKind::Git(v) = &self.doc.kind else {
            return false;
        };
        if k.ctrl || k.alt {
            return false;
        }
        let row = v
            .rows
            .get(self.doc.editor.cursor.line)
            .cloned()
            .unwrap_or(Row::None);
        let what = v.what.clone();
        let c = match k.key {
            Key::Enter => '\n',
            Key::Char(c) => c,
            _ => return false,
        };
        if let ViewWhat::Compare(_) = &what {
            return self.compare_key(c, &row);
        }
        match (&what, c, row) {
            (_, '\n', Row::Jump(j)) => self.jump(&j.path, j.line, j.col),
            (_, '\n', Row::Commit(h)) => {
                let short = &h[..8.min(h.len())];
                self.run_to_view(
                    &format!("git --no-pager show --stat --patch {h}"),
                    &format!("commit {short}"),
                );
            }
            (ViewWhat::Status { root }, c, row) => self.status_key(root.clone(), c, row),
            (ViewWhat::Branches { root }, '\n', Row::Branch(b)) => {
                self.git_op(
                    &format!("switch to {b}"),
                    root.clone(),
                    vec!["switch".into(), b],
                );
            }
            (ViewWhat::Branches { root }, 'D', Row::Branch(b)) => {
                self.ask_git(Danger::DeleteBranch {
                    root: root.clone(),
                    name: b,
                });
            }
            (ViewWhat::Branches { .. }, 'n', _) => {
                self.open_prompt(crate::prompt::PromptKind::Command, ":", "branch ");
            }
            (ViewWhat::Stashes { root }, '\n' | 'a', Row::Stash(s)) => {
                self.git_op(
                    &format!("apply {s}"),
                    root.clone(),
                    vec!["stash".into(), "apply".into(), s],
                );
            }
            (ViewWhat::Stashes { root }, 'p', Row::Stash(s)) => {
                self.git_op(
                    &format!("pop {s}"),
                    root.clone(),
                    vec!["stash".into(), "pop".into(), s],
                );
            }
            (ViewWhat::Stashes { root }, 'D', Row::Stash(s)) => {
                self.ask_git(Danger::DropStash {
                    root: root.clone(),
                    name: s,
                });
            }
            (ViewWhat::Branches { root } | ViewWhat::Stashes { root }, 'g', _) => {
                let root = root.clone();
                match what {
                    ViewWhat::Branches { .. } => self.show_branches(root, false),
                    _ => self.show_stashes(root, false),
                }
            }
            _ => return false,
        }
        true
    }

    fn status_key(&mut self, root: PathBuf, c: char, row: Row) {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>();
        match (c, row) {
            ('\n', Row::File(e)) => self.open_path(&git::abs(&root, &e.path)),
            ('s', Row::File(e)) => self.git_op(
                &format!("stage {}", e.path),
                root,
                s(&["add", "--", &e.path]),
            ),
            ('u', Row::File(e)) if e.staged() => self.git_op(
                &format!("unstage {}", e.path),
                root,
                s(&["restore", "--staged", "--", &e.path]),
            ),
            ('d', Row::File(e)) if e.state == FileState::Untracked => {
                // Untracked work goes to the OS trash, never deleted.
                self.finish_trash_prompt(&git::abs(&root, &e.path));
            }
            ('d', Row::File(e)) if e.y != '.' => {
                self.ask_git(Danger::Discard { root, path: e.path })
            }
            ('S', _) => self.git_op("stage all", root, s(&["add", "-A"])),
            ('U', _) => self.git_op("unstage all", root, s(&["reset", "-q"])),
            ('c', _) => self.open_commit(false),
            ('C', _) => self.open_commit(true),
            ('b', _) => self.show_branches(root, true),
            ('z', _) => self.git_op("stash", root, s(&["stash", "push"])),
            ('Z', _) => self.show_stashes(root, true),
            ('P', _) => self.git_terminal(root, &["push"]),
            ('p', _) => self.git_terminal(root, &["pull"]),
            ('f', _) => self.git_terminal(root, &["fetch"]),
            ('g', _) => self.request_status(root),
            _ => {}
        }
    }

    fn finish_trash_prompt(&mut self, path: &Path) {
        self.open_choice(Choice::Trash {
            path: path.to_path_buf(),
        });
    }

    fn ask_git(&mut self, d: Danger) {
        self.open_choice(Choice::Git(d));
    }

    /// The user said yes to `d`.
    pub(crate) fn confirmed(&mut self, d: Danger) {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>();
        match d {
            Danger::Discard { root, path } => self.git_op(
                &format!("discard {path}"),
                root,
                s(&["restore", "--", &path]),
            ),
            Danger::DropStash { root, name } => {
                self.git_op(&format!("drop {name}"), root, s(&["stash", "drop", &name]))
            }
            Danger::DeleteBranch { root, name } => {
                self.git_op(&format!("delete {name}"), root, s(&["branch", "-d", &name]))
            }
        }
    }

    /// Push, pull and fetch run in a terminal tab, so prompts for
    /// passwords, passphrases and 2FA stay between the user and git.
    fn git_terminal(&mut self, root: PathBuf, args: &[&str]) {
        if let Err(e) = git::test_guard(&root) {
            self.editor_mut().message = Some(e);
            return;
        }
        let spec = skink_term::CommandSpec::new("git")
            .args(args.iter().copied())
            .cwd(root);
        self.open_terminal_or_report(spec);
    }

    pub(crate) fn git_command_terminal(&mut self, args: &[&str]) {
        if let Some(root) = self.git_root() {
            self.git_terminal(root, args);
        }
    }

    // ---- the git panel -----------------------------------------------------

    pub(crate) fn git_panel(&mut self) {
        let Some(root) = self.git_root() else { return };
        let what = ViewWhat::Status { root: root.clone() };
        let found = self.find_doc(|d| matches!(&d.kind, DocKind::Git(v) if v.what == what));
        match found {
            Some(i) => self.switch_to(i),
            None => self.open_git_view(
                "git status",
                "loading…\n",
                GitView {
                    what,
                    rows: Vec::new(),
                    tints: Vec::new(),
                },
            ),
        }
        self.request_status(root);
    }

    fn refresh_status_views(&mut self, root: &Path, st: &Status) {
        let what = ViewWhat::Status {
            root: root.to_path_buf(),
        };
        let Some(i) = self.find_doc(|d| matches!(&d.kind, DocKind::Git(v) if v.what == what))
        else {
            return;
        };
        let (text, view) = status_text(st).finish(what);
        self.replace_git_view(i, "git status", &text, view);
    }

    /// An open git panel whose status failed (corrupt index, git missing)
    /// says why instead of "loading…" forever.
    fn status_view_failed(&mut self, root: &Path, err: &str) {
        let what = ViewWhat::Status {
            root: root.to_path_buf(),
        };
        let Some(i) = self.find_doc(|d| matches!(&d.kind, DocKind::Git(v) if v.what == what))
        else {
            return;
        };
        let mut v = ViewText::new();
        v.line(&[(" git status failed", Some(Tint::Del))], Row::None);
        v.line(&[(&format!("   {}", first_line(err)), None)], Row::None);
        v.line(&[("   g retry", Some(Tint::Dim))], Row::None);
        let (text, view) = v.finish(what);
        self.replace_git_view(i, "git status", &text, view);
    }

    fn show_branches(&mut self, root: PathBuf, open: bool) {
        let list = match git::branches(&root) {
            Ok(l) => l,
            Err(e) => {
                self.editor_mut().message = Some(format!("branches: {}", first_line(&e)));
                return;
            }
        };
        let mut v = ViewText::new();
        v.line(&[(" Branches", Some(Tint::Head))], Row::None);
        v.line(
            &[(
                "   Enter switch \u{b7} n new \u{b7} D delete \u{b7} g refresh",
                Some(Tint::Dim),
            )],
            Row::None,
        );
        v.line(&[("", None)], Row::None);
        let w = list
            .iter()
            .map(|b| b.name.chars().count())
            .max()
            .unwrap_or(0);
        for b in &list {
            let mark = if b.current { " \u{25cf} " } else { "   " };
            v.line(
                &[
                    (mark, Some(Tint::Add)),
                    (
                        &cell(&b.name, w),
                        Some(if b.current { Tint::Add } else { Tint::Accent }),
                    ),
                    ("  ", None),
                    (&cell(&b.when, 16), Some(Tint::Dim)),
                    (" ", None),
                    (&b.subject, None),
                ],
                Row::Branch(b.name.clone()),
            );
        }
        let (text, view) = v.finish(ViewWhat::Branches { root: root.clone() });
        self.put_view(open, "git branches", text, view);
    }

    fn show_stashes(&mut self, root: PathBuf, open: bool) {
        let list = match git::stashes(&root) {
            Ok(l) => l,
            Err(e) => {
                self.editor_mut().message = Some(format!("stashes: {}", first_line(&e)));
                return;
            }
        };
        let mut v = ViewText::new();
        v.line(&[(" Stashes", Some(Tint::Head))], Row::None);
        v.line(
            &[(
                "   Enter apply \u{b7} p pop \u{b7} D drop \u{b7} g refresh",
                Some(Tint::Dim),
            )],
            Row::None,
        );
        v.line(&[("", None)], Row::None);
        if list.is_empty() {
            v.line(
                &[(
                    "   no stashes (:stash saves the current changes)",
                    Some(Tint::Dim),
                )],
                Row::None,
            );
        }
        for s in &list {
            v.line(
                &[
                    ("   ", None),
                    (&cell(&s.name, 11), Some(Tint::Accent)),
                    (" ", None),
                    (&cell(&s.when, 16), Some(Tint::Dim)),
                    (" ", None),
                    (&s.subject, None),
                ],
                Row::Stash(s.name.clone()),
            );
        }
        let (text, view) = v.finish(ViewWhat::Stashes { root: root.clone() });
        self.put_view(open, "git stashes", text, view);
    }

    /// Open (`open`) or refresh an already open git view.
    fn put_view(&mut self, open: bool, title: &str, text: String, view: GitView) {
        let what = view.what.clone();
        match self.find_doc(|d| matches!(&d.kind, DocKind::Git(v) if v.what == what)) {
            Some(i) => {
                self.replace_git_view(i, title, &text, view);
                if open {
                    self.switch_to(i);
                }
            }
            None if open => self.open_git_view(title, &text, view),
            None => {}
        }
    }

    pub(crate) fn branches_cmd(&mut self) {
        if let Some(root) = self.git_root() {
            self.show_branches(root, true);
        }
    }

    pub(crate) fn stashes_cmd(&mut self) {
        if let Some(root) = self.git_root() {
            self.show_stashes(root, true);
        }
    }

    pub(crate) fn git_simple(&mut self, what: &str, args: &[&str]) {
        if let Some(root) = self.git_root() {
            self.git_op(what, root, args.iter().map(|s| (*s).to_owned()).collect());
        }
    }

    /// `:stage [PATH]` / `:unstage [PATH]`: the current file by default.
    pub(crate) fn stage_file(&mut self, args: &[&str], stage: bool) {
        let Some(root) = self.git_root() else { return };
        let target = match args.first() {
            Some(p) => (*p).to_owned(),
            None => match self.doc.canon.as_deref().and_then(git::locate) {
                Some(f) => f.rel,
                None => {
                    self.editor_mut().message = Some("no file to stage".into());
                    return;
                }
            },
        };
        let args: Vec<String> = if stage {
            vec!["add".into(), "--".into(), target.clone()]
        } else {
            vec![
                "restore".into(),
                "--staged".into(),
                "--".into(),
                target.clone(),
            ]
        };
        let verb = if stage { "stage" } else { "unstage" };
        self.git_op(&format!("{verb} {target}"), root, args);
    }

    // ---- commit ------------------------------------------------------------

    /// `:commit` with no message: a message tab; saving it commits.
    pub(crate) fn open_commit(&mut self, amend: bool) {
        let Some(root) = self.git_root() else { return };
        let path = match git::run(&root, &["rev-parse", "--git-path", "SKINK_COMMIT_EDITMSG"]) {
            Ok(o) => {
                let p = PathBuf::from(String::from_utf8_lossy(&o).trim());
                if p.is_absolute() { p } else { root.join(p) }
            }
            Err(e) => {
                self.editor_mut().message = Some(format!("commit: {}", first_line(&e)));
                return;
            }
        };
        let staged = git::run(&root, &["diff", "--cached", "--name-status"])
            .map(|o| String::from_utf8_lossy(&o).into_owned())
            .unwrap_or_default();
        let last = if amend {
            git::run(&root, &["log", "-1", "--format=%B"])
                .map(|o| String::from_utf8_lossy(&o).trim_end().to_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut text = format!("{last}\n\n");
        text.push_str(&commit_help(amend, false));
        if staged.trim().is_empty() && !amend {
            text.push_str("#\n# Nothing is staged yet (s in the git panel, or :stage).\n");
        } else {
            text.push_str("#\n# Staged:\n");
            for l in staged.lines() {
                text.push_str(&format!("#   {}\n", l.replace('\t', "  ")));
            }
        }
        if let Err(e) = std::fs::write(&path, &text) {
            self.editor_mut().message = Some(format!("commit: {e}"));
            return;
        }
        self.open_path(&path);
        self.doc.editor.reload(&text);
        self.doc.editor.goto(Some(1), None);
        self.git.draft = Draft::Editing {
            doc: self.doc.id,
            root,
            path,
            amend,
            signoff: false,
        };
        self.editor_mut().message =
            Some("write the message, Ctrl-S commits \u{b7} :amend \u{b7} :signoff \u{b7} close the tab to cancel".into());
    }

    /// `:amend` / `:signoff` in the message tab.
    pub(crate) fn toggle_draft(&mut self, amend: bool) {
        let Draft::Editing {
            amend: a,
            signoff: s,
            ..
        } = &mut self.git.draft
        else {
            if amend {
                return self.open_commit(true);
            }
            self.editor_mut().message = Some("open the commit message first (:commit)".into());
            return;
        };
        if amend {
            *a = !*a;
        } else {
            *s = !*s;
        }
        let note = format!(
            "commit: amend {} \u{b7} sign-off {}",
            if *a { "on" } else { "off" },
            if *s { "on" } else { "off" }
        );
        self.editor_mut().message = Some(note);
    }

    pub(crate) fn commit_cmd(&mut self, args: &[&str]) {
        if args.is_empty() {
            return self.open_commit(false);
        }
        let Some(root) = self.git_root() else { return };
        self.git_op(
            "commit",
            root,
            vec!["commit".into(), "-m".into(), args.join(" ")],
        );
    }

    // ---- side-by-side diff ---------------------------------------------------

    /// `:diff [HEAD|all]`: the live file against the index (or HEAD) side
    /// by side; `all` (or no file) is `git diff` of the whole tree.
    pub(crate) fn diff_cmd(&mut self, args: &[&str]) {
        let arg = args.first().copied().unwrap_or("");
        // `:diff OTHER`: this file against another one.
        if !arg.is_empty() && !arg.eq_ignore_ascii_case("all") && !arg.eq_ignore_ascii_case("head")
        {
            let other = args.join(" ");
            let Some(me) = self.doc.canon.clone() else {
                self.editor_mut().message =
                    Some("Save this buffer as a file first, then :diff OTHER compares them".into());
                return;
            };
            let other = std::path::PathBuf::from(&other);
            if !other.exists() {
                self.editor_mut().message = Some(format!("{} does not exist", other.display()));
                return;
            }
            if let Err(e) = self.open_compare(&me, &other) {
                self.editor_mut().message = Some(e);
            }
            return;
        }
        if arg == "all" || self.doc.canon.is_none() {
            return self.run_to_view("git --no-pager diff", "git diff");
        }
        if self.doc.git.tracked().is_none()
            && let Some(p) = self.doc.canon.clone()
            && let Some(f) = git::locate(&p)
        {
            let base = git::load_base(&f);
            self.doc.git.marks = Marks::Ready(Box::new(Tracked {
                file: f,
                base,
                hunks: Hunks::default(),
                rev: u64::MAX,
            }));
        }
        let head = arg.eq_ignore_ascii_case("head");
        let (old_text, label) = match self.doc.git.tracked().map(|t| &t.base) {
            Some(Base::Tracked { head: h, .. }) if head => (h.clone().unwrap_or_default(), "HEAD"),
            Some(Base::Tracked { index, .. }) => (index.clone(), "index"),
            Some(Base::Untracked) => (String::new(), "nothing (untracked)"),
            _ => {
                let why = self.git_off_reason().unwrap_or("not a file tracked by git");
                self.editor_mut().message = Some(why.into());
                return;
            }
        };
        let new_text = self.doc.editor.buffer.text();
        let old = git::lines(&old_text);
        let new = git::lines(&new_text);
        let hunks = git::diff(&old, &new);
        if hunks.is_empty() {
            self.editor_mut().message = Some(format!("no changes against {label}"));
            return;
        }
        let name = self.doc.title();
        let path = self.doc.canon.clone().unwrap_or_default();
        let width = (self.areas.main.width as usize).saturating_sub(8).max(40);
        let (text, view) = side_by_side(
            &old,
            &new,
            &hunks,
            width,
            (label, "buffer"),
            &SideRows::Buffer(&path),
        );
        self.open_git_view(&format!("diff {name} ({label})"), &text, view);
    }

    // ---- conflict buttons -------------------------------------------------

    pub(crate) fn clear_git_buttons(&mut self) {
        self.git.buttons.clear();
    }

    /// Register a clickable label drawn at screen cells `x..x+len`, row `y`.
    pub(crate) fn add_conflict_button(
        &mut self,
        x: u16,
        len: u16,
        y: u16,
        line: usize,
        action: &'static str,
    ) {
        self.git.buttons.push(Button {
            doc: self.doc.id,
            x: Range16 {
                start: x,
                end: x + len,
            },
            y,
            line,
            action,
        });
    }

    /// A click on a conflict label or a gutter change mark. True when
    /// consumed.
    pub(crate) fn git_click(
        &mut self,
        x: u16,
        y: u16,
        line: Option<usize>,
        gutter_col: bool,
    ) -> bool {
        if let Some(b) = self
            .git
            .buttons
            .iter()
            .find(|b| b.doc == self.doc.id && b.y == y && (b.x.start..b.x.end).contains(&x))
            .copied()
        {
            self.doc.editor.goto(Some(b.line + 1), None);
            self.run_chain(b.action);
            return true;
        }
        if gutter_col && let Some(line) = line {
            let on_mark = self
                .fresh_hunks()
                .is_some_and(|h| h.unstaged.iter().chain(&h.staged).any(|h| h.touches(line)));
            if on_mark {
                self.doc.editor.goto(Some(line + 1), None);
                self.peek_hunk();
                return true;
            }
        }
        false
    }
}

fn commit_help(amend: bool, signoff: bool) -> String {
    format!(
        "# Write the commit message above. Save (Ctrl-S) to commit; close the tab\n\
         # to cancel. Lines starting with '#' are dropped.\n\
         # :amend toggles amending the last commit (now {}), :signoff adds a\n\
         # Signed-off-by line (now {}).\n",
        if amend { "on" } else { "off" },
        if signoff { "on" } else { "off" }
    )
}

/// The git panel's text.
fn status_text(st: &Status) -> ViewText {
    let mut v = ViewText::new();
    let branch = st.branch.as_deref().unwrap_or("(detached HEAD)");
    let mut head: Vec<(String, Option<Tint>)> = vec![
        (" On ".into(), Some(Tint::Dim)),
        (branch.to_owned(), Some(Tint::Head)),
    ];
    if let Some((a, b)) = st.ahead_behind {
        if a > 0 {
            head.push((format!("  \u{2191}{a}"), Some(Tint::Add)));
        }
        if b > 0 {
            head.push((format!("  \u{2193}{b}"), Some(Tint::Warn)));
        }
    }
    if let Some(u) = &st.upstream {
        head.push((format!("  \u{b7} {u}"), Some(Tint::Dim)));
    }
    let parts: Vec<(&str, Option<Tint>)> = head.iter().map(|(s, t)| (s.as_str(), *t)).collect();
    v.line(&parts, Row::None);
    v.line(&[("", None)], Row::None);
    type Section = (&'static str, fn(&git::Entry) -> bool, bool);
    let sections: [Section; 4] = [
        ("Conflicts", |e| e.state == FileState::Conflicted, false),
        ("Staged", git::Entry::staged, true),
        (
            "Changes",
            |e| e.state != FileState::Untracked && e.unstaged(),
            false,
        ),
        ("Untracked", |e| e.state == FileState::Untracked, false),
    ];
    let mut any = false;
    for (title, pick, staged_col) in sections {
        let list: Vec<&git::Entry> = st
            .entries
            .iter()
            .filter(|e| e.state != FileState::Ignored && pick(e))
            .collect();
        if list.is_empty() {
            continue;
        }
        any = true;
        let head = format!(" {title} ({})", list.len());
        v.line(
            &[(
                &head,
                Some(if title == "Conflicts" {
                    Tint::Warn
                } else {
                    Tint::Head
                }),
            )],
            Row::None,
        );
        for e in list {
            let code = match e.state {
                FileState::Conflicted => 'U',
                FileState::Untracked => '?',
                _ if staged_col => e.x,
                _ => e.y,
            };
            let tint = match code {
                'A' | '?' => Tint::Add,
                'D' => Tint::Del,
                'U' => Tint::Warn,
                _ => Tint::Mod,
            };
            let code = code.to_string();
            let name = match &e.from {
                Some(f) => format!("{f} \u{2192} {}", e.path),
                None => e.path.clone(),
            };
            v.line(
                &[
                    ("   ", None),
                    (&code, Some(tint)),
                    ("  ", None),
                    (&name, None),
                ],
                Row::File(e.clone()),
            );
        }
        v.line(&[("", None)], Row::None);
    }
    if !any {
        v.line(
            &[("   nothing to commit, working tree clean", Some(Tint::Dim))],
            Row::None,
        );
        v.line(&[("", None)], Row::None);
    }
    for help in [
        "   Enter open \u{b7} s stage \u{b7} u unstage \u{b7} d discard \u{b7} S / U stage / unstage all",
        "   c commit \u{b7} C amend \u{b7} b branches \u{b7} z stash \u{b7} Z stashes \u{b7} g refresh",
        "   P push \u{b7} p pull \u{b7} f fetch (in a terminal tab)",
    ] {
        v.line(&[(help, Some(Tint::Dim))], Row::None);
    }
    v
}

/// What the rows of a side-by-side view point at.
enum SideRows<'a> {
    /// The right side is this file's buffer: Enter jumps there.
    Buffer(&'a Path),
    /// A comparison of two files: rows know their change and both lines.
    Pair,
}

/// Two files compared side by side (see `compare.rs`).
pub(crate) fn compare_view(
    old: &[&str],
    new: &[&str],
    c: &crate::compare::Compare,
    width: usize,
) -> (String, GitView) {
    let (l, r) = (c.left.display().to_string(), c.right.display().to_string());
    let (text, mut view) = side_by_side(old, new, &c.hunks, width, (&l, &r), &SideRows::Pair);
    view.what = ViewWhat::Compare(Box::new(c.clone()));
    (text, view)
}

/// Two columns: `old` on the left, `new` on the right, changes with 3
/// lines of context.
fn side_by_side(
    old: &[&str],
    new: &[&str],
    hunks: &[Hunk],
    width: usize,
    (label, right_label): (&str, &str),
    rows: &SideRows,
) -> (String, GitView) {
    enum Pair {
        Same(usize, usize),
        Change(Option<usize>, Option<usize>, usize),
    }
    let mut pairs = Vec::new();
    let (mut o, mut n) = (0, 0);
    for (hi, h) in hunks.iter().enumerate() {
        while n < h.new.start && o < h.old.start {
            pairs.push(Pair::Same(o, n));
            o += 1;
            n += 1;
        }
        for i in 0..h.old.len().max(h.new.len()) {
            pairs.push(Pair::Change(
                (i < h.old.len()).then_some(h.old.start + i),
                (i < h.new.len()).then_some(h.new.start + i),
                hi,
            ));
        }
        (o, n) = (h.old.end, h.new.end);
    }
    while n < new.len() && o < old.len() {
        pairs.push(Pair::Same(o, n));
        o += 1;
        n += 1;
    }
    const CONTEXT: usize = 3;
    let changed: Vec<usize> = pairs
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Pair::Change(..)))
        .map(|(i, _)| i)
        .collect();
    let near = |i: usize| {
        let k = changed.partition_point(|&c| c < i);
        let after = changed.get(k).is_some_and(|&c| c - i <= CONTEXT);
        let before = k > 0 && i - changed[k - 1] <= CONTEXT;
        after || before
    };
    let half = (width.saturating_sub(1) / 2).max(20);
    let text_w = half - 6;
    let mut v = ViewText::new();
    let left_head = cell(&format!(" {label}"), half);
    v.line(
        &[
            (&left_head, Some(Tint::Head)),
            ("\u{2502}", Some(Tint::Dim)),
            (&format!(" {right_label}"), Some(Tint::Head)),
        ],
        Row::None,
    );
    let mut skipped = false;
    for (i, p) in pairs.iter().enumerate() {
        if !near(i) {
            skipped = true;
            continue;
        }
        if skipped {
            v.line(
                &[
                    (&cell("  \u{22ef}", half), Some(Tint::Dim)),
                    ("\u{2502}", Some(Tint::Dim)),
                ],
                Row::None,
            );
            skipped = false;
        }
        let side = |idx: Option<usize>, lines: &[&str]| match idx {
            Some(i) => (
                format!("{:>5}", i + 1),
                cell(
                    &format!(" {}", lines.get(i).copied().unwrap_or("")),
                    text_w + 1,
                ),
            ),
            None => ("     ".to_owned(), cell("", text_w + 1)),
        };
        let (l, r, kind) = match *p {
            Pair::Same(a, b) => (
                side(Some(a), old),
                side(Some(b), new),
                (false, false, Some(b)),
            ),
            Pair::Change(a, b, _) => (side(a, old), side(b, new), (a.is_some(), b.is_some(), b)),
        };
        let (del, add, jump_line) = kind;
        let changed_row = matches!(p, Pair::Change(..));
        let ln = |on: bool, num: Tint, bg: Tint| {
            if changed_row && on {
                (Some(num), Some(bg))
            } else {
                (Some(Tint::Dim), None)
            }
        };
        let (ln_l, bg_l) = ln(del, Tint::DelNum, Tint::DelBg);
        let (ln_r, bg_r) = ln(add, Tint::AddNum, Tint::AddBg);
        let row = match (rows, p) {
            (SideRows::Pair, Pair::Same(a, b)) => Row::Pair {
                hunk: None,
                left: Some(*a),
                right: Some(*b),
            },
            (SideRows::Pair, Pair::Change(a, b, h)) => Row::Pair {
                hunk: Some(*h),
                left: *a,
                right: *b,
            },
            (SideRows::Buffer(path), _) => match jump_line {
                Some(line) => Row::Jump(Jump {
                    path: path.to_path_buf(),
                    line,
                    col: 0,
                }),
                None => Row::None,
            },
        };
        v.line(
            &[
                (&l.0, ln_l),
                (&l.1, bg_l),
                ("\u{2502}", Some(Tint::Dim)),
                (&r.0, ln_r),
                (&r.1, bg_r),
            ],
            row,
        );
    }
    v.finish(ViewWhat::Diff)
}
