//! Skink as another tool's editor (`core.editor`, `$EDITOR`, `$VISUAL`):
//! git commit messages and rebase lists, the Ctrl-G prompt files of Claude
//! Code and Codex, `crontab -e`, shell edit-command-line. Those open one
//! file, lean: no tree, project scan, language server, git gutter, update
//! check, session or recents; a hint line; `:cq` exits 1 so git aborts.
//!
//! Detection order: explicit flag, then the file path (git's own files,
//! files in a temp folder), then the parent process (Linux only, `/proc`).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `--scratch` / `--project` on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeFlag {
    Auto,
    Scratch,
    Project,
}

/// How this process (or a handed-off tab) runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaunchMode {
    /// The full editor (today's behaviour).
    Project,
    /// One file for another tool.
    Lean(Lean),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lean {
    pub kind: LeanKind,
    /// Why lean mode was picked (`:mode` shows it).
    pub why: Why,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeanKind {
    /// A note or prompt (AI TUIs, shells, crontab, kubectl).
    Scratch,
    GitMessage(GitMsg),
    RebaseTodo,
    /// `git add -e` / `git add -p` then `e`.
    HunkEdit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitMsg {
    Commit,
    Merge,
    Tag,
    Notes,
    /// Squash / fixup message during a rebase.
    Squash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Why {
    /// `--scratch`.
    Flag,
    /// One of git's own files (COMMIT_EDITMSG, git-rebase-todo, ...).
    GitFile,
    /// A file in a temp folder (what Ctrl-G in Claude Code / Codex, bash's
    /// edit-command-line, crontab -e, kubectl edit hand over).
    TempFile,
    /// Started by this program (name from the process table).
    Parent(String),
}

/// Programs that start `$EDITOR` for a one-off edit.
const TOOLS: &[&str] = &[
    "git", "claude", "codex", "crontab", "sudoedit", "kubectl", "gh",
];

/// Shells between the tool and Skink (`sh -c 'skink "$@"'`).
const SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "fish", "ksh"];

/// Pick the mode. `parent` names the parent program (skipping shells); it
/// is only asked when the flag and the path decide nothing.
pub fn detect(
    flag: ModeFlag,
    path: Option<&Path>,
    parent: impl FnOnce() -> Option<String>,
) -> LaunchMode {
    let git = path.and_then(git_kind);
    match flag {
        ModeFlag::Project => return LaunchMode::Project,
        ModeFlag::Scratch => {
            return LaunchMode::Lean(Lean {
                kind: git.unwrap_or(LeanKind::Scratch),
                why: Why::Flag,
            });
        }
        ModeFlag::Auto => {}
    }
    let Some(path) = path else {
        return LaunchMode::Project;
    };
    if let Some(kind) = git {
        return LaunchMode::Lean(Lean {
            kind,
            why: Why::GitFile,
        });
    }
    if in_temp(path) {
        return LaunchMode::Lean(Lean {
            kind: LeanKind::Scratch,
            why: Why::TempFile,
        });
    }
    match parent() {
        Some(p) if TOOLS.contains(&p.as_str()) => LaunchMode::Lean(Lean {
            kind: LeanKind::Scratch,
            why: Why::Parent(p),
        }),
        _ => LaunchMode::Project,
    }
}

/// Git's own files, when they sit in a git directory.
fn git_kind(path: &Path) -> Option<LeanKind> {
    let kind = match path.file_name()?.to_str()? {
        "COMMIT_EDITMSG" => LeanKind::GitMessage(GitMsg::Commit),
        "MERGE_MSG" => LeanKind::GitMessage(GitMsg::Merge),
        "TAG_EDITMSG" => LeanKind::GitMessage(GitMsg::Tag),
        "NOTES_EDITMSG" => LeanKind::GitMessage(GitMsg::Notes),
        "SQUASH_MSG" => LeanKind::GitMessage(GitMsg::Squash),
        "git-rebase-todo" => LeanKind::RebaseTodo,
        "addp-hunk-edit.diff" => LeanKind::HunkEdit,
        _ => return None,
    };
    in_git_dir(path).then_some(kind)
}

/// Under a `.git` folder (also worktree and submodule gitdirs, which live
/// inside one), or in a bare repository / `--git-dir` (HEAD + objects).
fn in_git_dir(path: &Path) -> bool {
    let dir = path.parent().unwrap_or(Path::new(""));
    dir.components().any(|c| c.as_os_str() == ".git")
        || dir.ancestors().take(3).any(|d| {
            let d = if d.as_os_str().is_empty() {
                Path::new(".")
            } else {
                d
            };
            d.join("HEAD").is_file() && d.join("objects").is_dir()
        })
}

/// The temp folders tools write their one-off files to.
fn temp_roots() -> Vec<PathBuf> {
    let mut roots = vec![std::env::temp_dir()];
    #[cfg(unix)]
    roots.extend(["/tmp", "/var/tmp"].map(PathBuf::from));
    roots
        .into_iter()
        .map(|r| crate::doc::canonical(&r))
        .collect()
}

/// A file in a temp folder, unless it belongs to a project cloned there
/// (a `.git` between the file and the temp root).
fn in_temp(path: &Path) -> bool {
    let abs = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => crate::doc::canonical(p),
        _ => crate::doc::canonical(Path::new(".")),
    };
    temp_roots().iter().any(|root| {
        abs.starts_with(root)
            && !abs
                .ancestors()
                .take_while(|d| d != root)
                .any(|d| d.join(".git").exists())
    })
}

/// The program that started Skink, looking past shells. Linux only (it is
/// a backup to the path rules, and `/proc` makes it a few file reads).
pub fn parent_program() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let mut pid = std::process::id();
        for _ in 0..4 {
            pid = linux_ppid(pid)?;
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
            let name = comm.trim().to_owned();
            if !SHELLS.contains(&name.as_str()) {
                return Some(name);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = SHELLS;
        None
    }
}

#[cfg(target_os = "linux")]
fn linux_ppid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // "pid (comm) state ppid ...": comm may contain spaces and parens.
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

impl Lean {
    /// What the hint line and `:mode` call it.
    pub fn label(&self) -> &'static str {
        match self.kind {
            LeanKind::Scratch => "scratch",
            LeanKind::GitMessage(GitMsg::Commit) => "commit message",
            LeanKind::GitMessage(GitMsg::Merge) => "merge message",
            LeanKind::GitMessage(GitMsg::Tag) => "tag message",
            LeanKind::GitMessage(GitMsg::Notes) => "git note",
            LeanKind::GitMessage(GitMsg::Squash) => "squash message",
            LeanKind::RebaseTodo => "rebase list",
            LeanKind::HunkEdit => "hunk edit",
        }
    }

    pub fn reason(&self) -> String {
        match &self.why {
            Why::Flag => "--scratch".into(),
            Why::GitFile => "a file git hands its editor".into(),
            Why::TempFile => "a file in a temp folder".into(),
            Why::Parent(p) => format!("started by {p}"),
        }
    }

    /// Rulers when the user has none: a commit subject fits 50 columns,
    /// the body 72 (a ruler at column N marks the N-th column).
    pub fn default_rulers(&self) -> &'static [usize] {
        match self.kind {
            LeanKind::GitMessage(_) => &[51, 73],
            _ => &[],
        }
    }
}

/// Colours for git's files, which have no grammar: `#` comment lines, and
/// in a rebase list the verb and the commit hash. Char columns.
pub fn git_styles(kind: LeanKind, line: &str) -> Vec<(usize, usize, skink_syntax::Hl)> {
    use skink_syntax::Hl;
    let len = line.chars().count();
    match kind {
        LeanKind::GitMessage(_) | LeanKind::RebaseTodo if line.starts_with('#') => {
            vec![(0, len, Hl::Comment)]
        }
        LeanKind::RebaseTodo => {
            let mut out = Vec::new();
            let mut words = word_spans(line);
            if let Some((s, e)) = words.next() {
                out.push((s, e, Hl::Keyword));
            }
            if let Some((s, e)) = words.next() {
                out.push((s, e, Hl::Number));
            }
            out
        }
        _ => Vec::new(),
    }
}

/// `(start, end)` char spans of whitespace-separated words.
fn word_spans(line: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            return None;
        }
        let s = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        Some((s, i))
    })
}

/// The rebase verbs Alt-Shift-K cycles through, in order.
pub const VERBS: &[&str] = &["pick", "reword", "edit", "squash", "fixup", "drop"];

/// The long form of a cyclable verb (`p` -> `pick`).
fn long_verb(w: &str) -> Option<&'static str> {
    VERBS
        .iter()
        .copied()
        .find(|v| *v == w || (w.len() == 1 && v.starts_with(w)))
}

/// The cursor line's verb span and the verb after it, if the line is a
/// commit line (`pick 1a2b3c subject`).
pub fn next_verb(line: &str) -> Option<((usize, usize), &'static str)> {
    let (s, e) = word_spans(line).next()?;
    let word: String = line.chars().skip(s).take(e - s).collect();
    let cur = long_verb(&word)?;
    let i = VERBS.iter().position(|v| *v == cur)?;
    Some(((s, e), VERBS[(i + 1) % VERBS.len()]))
}

/// A verb typed for `:rebase VERB` (long or one-letter form).
pub fn parse_verb(w: &str) -> Option<&'static str> {
    long_verb(w)
}

/// The span to replace on a commit line with any verb (`:rebase VERB`).
pub fn verb_span(line: &str) -> Option<(usize, usize)> {
    let (s, e) = word_spans(line).next()?;
    let word: String = line.chars().skip(s).take(e - s).collect();
    long_verb(&word).map(|_| (s, e))
}

// ---- the app side ----------------------------------------------------------

use crate::doc::DocRole;
use crate::handoff::{Incoming, Reply, Waiter};
use crate::{App, Exit};

/// Shown at the bottom of a lean document.
pub const HINT: &str = "Ctrl-S save \u{b7} Ctrl-Q done \u{b7} :cq cancel";

impl App {
    /// `main`: this process edits one file for another tool. Call after
    /// `startup` (settings decide rulers and the like).
    pub fn set_launch(&mut self, mode: LaunchMode) {
        if let LaunchMode::Lean(l) = &mode {
            self.make_live_doc_lean(l.clone(), Waiter::Process);
        }
        self.launch = mode;
    }

    /// How the process should exit (`:cq` → [`Exit::Cancelled`]).
    pub fn exit_status(&self) -> Exit {
        self.exit
    }

    /// The whole process is a lean editor (not just one tab).
    pub(crate) fn lean_process(&self) -> bool {
        matches!(self.launch, LaunchMode::Lean(_))
    }

    /// Project features asked for in a lean process: say why not.
    pub(crate) fn refuse_in_lean(&mut self, what: &str) -> bool {
        if self.lean_process() {
            self.doc.editor.message = Some(format!(
                "{what} is off while Skink edits a file for another tool (skink --project FILE for the full editor)"
            ));
        }
        self.lean_process()
    }

    /// The live document's lean extras, if it is one.
    pub(crate) fn live_lean(&self) -> Option<&Lean> {
        self.doc.role.lean()
    }

    fn make_live_doc_lean(&mut self, lean: Lean, waiter: Waiter) {
        // Opened as an ordinary tab a moment ago: undo what that started.
        if self.doc.lsp.is_open() {
            if let (Some(link), Some(p)) = (&self.lsp, &self.doc.canon) {
                link.close(p);
            }
            self.doc.lsp = crate::doc::LspSync::Closed;
        }
        self.doc.git.marks = crate::gitui::Marks::Off;
        if lean.kind == LeanKind::Scratch {
            let ext = self
                .doc
                .editor
                .buffer
                .path()
                .and_then(|p| p.extension())
                .map(|e| e.to_string_lossy().to_lowercase());
            if matches!(ext.as_deref(), None | Some("md" | "markdown")) && self.settings.syntax {
                self.doc.force_syntax(skink_syntax::Lang::Markdown);
            }
            // Jot below what is there (a prompt being drafted, a note).
            self.doc.editor.goto(Some(usize::MAX), Some(usize::MAX));
        }
        // A previous waiter for the same file (opened twice) is done.
        if let DocRole::Lean { waiter: old, .. } = std::mem::take(&mut self.doc.role) {
            finish(old, Exit::Done, &mut self.exit);
        }
        self.doc.role = DocRole::Lean { lean, waiter };
    }

    /// Tell whoever waits how tab `idx` ended; the doc becomes ordinary.
    pub(crate) fn finish_lean(&mut self, idx: usize, outcome: Exit) {
        let Some(d) = self.doc_at_mut(idx) else {
            return;
        };
        if let DocRole::Lean { waiter, .. } = std::mem::take(&mut d.role) {
            finish(waiter, outcome, &mut self.exit);
        }
    }

    /// Answer the child waiting on tab `idx`, then close the tab. The
    /// instance stays up (the child and git run in one of its terminals):
    /// the last document is replaced by an empty one.
    pub(crate) fn close_remote_lean(&mut self, idx: usize, outcome: Exit) {
        self.finish_lean(idx, outcome);
        if self.doc_count() == 1 {
            self.new_scratch_tab();
        }
        self.close_tab(idx);
    }

    /// Whether closing tab `idx` answers a child waiting in a terminal.
    pub(crate) fn is_remote_lean(&self, idx: usize) -> bool {
        self.doc_at(idx).is_some_and(|d| {
            matches!(
                d.role,
                DocRole::Lean {
                    waiter: Waiter::Remote(_),
                    ..
                }
            )
        })
    }

    /// `:cq`: leave without saving and tell the caller to abort.
    pub(crate) fn cancel_quit(&mut self) {
        let idx = self.cur_doc;
        if self.is_remote_lean(idx) {
            // Unsaved edits are dropped: `close_tab` does not ask.
            self.close_remote_lean(idx, Exit::Cancelled);
            return;
        }
        if matches!(self.doc.role, DocRole::Normal) && self.any_dirty() {
            self.doc.editor.message = Some(
                "Unsaved changes: :cq leaves without saving only in a file for another tool".into(),
            );
            return;
        }
        self.finish_lean(idx, Exit::Cancelled);
        self.exit = Exit::Cancelled;
        self.quit = true;
    }

    /// Quitting with lean tabs still open: clean ones are done, the rest
    /// cancelled (their edits never reached the disk).
    pub(crate) fn finish_all_lean(&mut self) {
        for i in 0..self.tabs.len() {
            let dirty = self.doc_at(i).is_some_and(|d| d.editor.buffer.is_dirty());
            let outcome = if dirty { Exit::Cancelled } else { Exit::Done };
            if self.doc_at(i).is_some_and(|d| d.role.lean().is_some()) {
                self.finish_lean(i, outcome);
            }
        }
    }

    /// Environment for a terminal tab's shell: where a nested `skink`
    /// hands its file to this instance. Starts the listener on first use.
    pub(crate) fn handoff_env(&mut self) -> Vec<(String, String)> {
        if matches!(self.handoff, crate::handoff::ServerState::NotStarted) {
            let dir = std::env::var_os("XDG_RUNTIME_DIR")
                .map(std::path::PathBuf::from)
                .filter(|d| d.is_dir())
                .unwrap_or_else(std::env::temp_dir);
            self.handoff = match &self.wake {
                Some(w) => match crate::handoff::Server::start(&dir, w.clone()) {
                    Ok(s) => crate::handoff::ServerState::Listening(s),
                    Err(_) => crate::handoff::ServerState::Failed,
                },
                // Tests and headless use: no event loop to wake.
                None => crate::handoff::ServerState::NotStarted,
            };
        }
        match &self.handoff {
            crate::handoff::ServerState::Listening(s) => vec![
                (crate::handoff::ENV_ADDR.into(), s.endpoint.to_env()),
                (crate::handoff::ENV_TOKEN.into(), s.token.clone()),
            ],
            _ => Vec::new(),
        }
    }

    /// Open what nested `skink` processes asked for.
    pub(crate) fn poll_handoff(&mut self) {
        let crate::handoff::ServerState::Listening(s) = &self.handoff else {
            return;
        };
        let got: Vec<Incoming> = std::iter::from_fn(|| s.try_recv()).collect();
        for i in got {
            self.open_handed_off(i);
        }
    }

    pub(crate) fn open_handed_off(&mut self, Incoming { req, conn }: Incoming) {
        let canon = crate::doc::canonical(&req.path);
        match self.find_doc(|d| d.canon.as_ref() == Some(&canon)) {
            Some(i) => self.switch_to(i),
            None => {
                let editor = match skink_core::Buffer::open(&req.path) {
                    Ok(b) => skink_core::Editor::new(b),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        skink_core::Editor::new(skink_core::Buffer::new_file(&req.path))
                    }
                    Err(e) => return conn.reply(Reply::Refused(e.to_string())),
                };
                self.add_doc(editor, true);
            }
        }
        if req.line.is_some() {
            self.doc.editor.goto(req.line, req.col);
        }
        self.focus_main();
        match req.mode {
            LaunchMode::Project => conn.reply(Reply::Opened),
            LaunchMode::Lean(l) => {
                let name = self.doc.title();
                self.make_live_doc_lean(l, Waiter::Remote(conn));
                self.doc.editor.message = Some(format!("{name}: {HINT}"));
            }
        }
    }

    /// `:mode`.
    pub(crate) fn describe_mode(&mut self) {
        self.doc.editor.message = Some(match (&self.launch, self.live_lean()) {
            (_, Some(l)) => format!("Lean: {} ({}). {HINT}", l.label(), l.reason()),
            (LaunchMode::Lean(l), None) => format!("Lean process: {} ({})", l.label(), l.reason()),
            (LaunchMode::Project, None) => "Project mode (the full editor)".into(),
        });
    }

    /// Rulers for the live document: the user's, else the lean defaults.
    pub(crate) fn rulers(&self) -> Vec<usize> {
        if !self.settings.rulers.is_empty() {
            return self.settings.rulers.clone();
        }
        self.live_lean()
            .map_or_else(Vec::new, |l| l.default_rulers().to_vec())
    }

    /// The hint line's text (kind, keys, a subject-length note).
    pub(crate) fn lean_hint(&self) -> Option<String> {
        let l = self.live_lean()?;
        let mut s = format!(" {} \u{b7} {HINT}", l.label());
        if let LeanKind::GitMessage(_) = l.kind {
            let n = self.doc.editor.buffer.line_text(0).chars().count();
            if n > 50 {
                s.push_str(&format!(" \u{b7} subject {n} chars (50 reads best)"));
            }
        }
        if l.kind == LeanKind::RebaseTodo {
            s.push_str(" \u{b7} Alt-Shift-K next verb, Alt-Up/Down reorder");
        }
        Some(s)
    }

    /// Alt-Shift-K in a rebase list: pick → reword → edit → squash → fixup
    /// → drop on the cursor line.
    pub(crate) fn cycle_rebase_verb(&mut self) {
        let line = self.doc.editor.cursor.line;
        let text = self.doc.editor.buffer.line_text(line);
        match next_verb(&text) {
            Some((span, verb)) => self.set_rebase_verb(line, span, verb),
            None => {
                self.doc.editor.message = Some(match self.live_lean().map(|l| l.kind) {
                    Some(LeanKind::RebaseTodo) => "Not a commit line".into(),
                    _ => "Alt-Shift-K cycles rebase verbs in git-rebase-todo".into(),
                })
            }
        }
    }

    /// `:rebase VERB` on the cursor line.
    pub(crate) fn rebase_command(&mut self, args: &[&str]) {
        let line = self.doc.editor.cursor.line;
        let text = self.doc.editor.buffer.line_text(line);
        match (args.first().and_then(|w| parse_verb(w)), verb_span(&text)) {
            (Some(verb), Some(span)) => self.set_rebase_verb(line, span, verb),
            (None, _) => {
                self.doc.editor.message = Some(format!("usage: :rebase {}", VERBS.join("|")))
            }
            (_, None) => self.doc.editor.message = Some("Not a commit line".into()),
        }
    }

    fn set_rebase_verb(&mut self, line: usize, (s, e): (usize, usize), verb: &str) {
        let ed = &mut self.doc.editor;
        let start = ed.buffer.line_to_char(line);
        let col = ed.cursor.col;
        ed.apply_edits(vec![(start + s, start + e, verb.to_owned())]);
        let len = ed.buffer.line_len(line);
        ed.goto(Some(line + 1), Some(col.min(len) + 1));
    }
}

/// What happened to a `skink FILE` started inside a Skink terminal tab.
pub enum HandOff {
    /// Not inside a Skink terminal (or no file): edit here.
    NotNested,
    /// The parent opened it and (for a lean file) the tab has closed.
    Finished(Exit),
    /// The parent did not answer or refused: edit here instead.
    Unreachable(String),
}

/// `main`: give the file to the Skink whose terminal tab this runs in,
/// and wait for its tab to close when the file is for another tool.
pub fn hand_off(
    path: &Path,
    line: Option<usize>,
    col: Option<usize>,
    mode: &LaunchMode,
) -> HandOff {
    use crate::handoff::{AskError, Endpoint, Request, ask};
    let Some((endpoint, token)) = Endpoint::from_env() else {
        return HandOff::NotNested;
    };
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    if let LaunchMode::Lean(l) = mode {
        eprintln!(
            "skink: editing {} ({}) in the Skink window; Ctrl-Q there when done, :cq to cancel",
            path.display(),
            l.label()
        );
    }
    let req = Request {
        token,
        path,
        line,
        col,
        mode: mode.clone(),
    };
    match ask(&endpoint, &req) {
        Ok(Reply::Opened | Reply::Done) => HandOff::Finished(Exit::Done),
        Ok(Reply::Cancelled) => HandOff::Finished(Exit::Cancelled),
        Ok(Reply::Refused(why)) => HandOff::Unreachable(why),
        Err(AskError::Unreachable(e)) => HandOff::Unreachable(e.to_string()),
        // The Skink window went away while the file was open: the edit
        // did not finish, so the caller should not go on with it.
        Err(AskError::Lost(e)) => {
            eprintln!("skink: lost the Skink window ({e}); the edit is cancelled");
            HandOff::Finished(Exit::Cancelled)
        }
    }
}

fn finish(waiter: Waiter, outcome: Exit, exit: &mut Exit) {
    match waiter {
        Waiter::Process => *exit = outcome,
        Waiter::Remote(c) => c.reply(match outcome {
            Exit::Done => Reply::Done,
            Exit::Cancelled => Reply::Cancelled,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lean(kind: LeanKind, why: Why) -> LaunchMode {
        LaunchMode::Lean(Lean { kind, why })
    }

    #[test]
    fn detection_table() {
        let tmp = std::env::temp_dir().join(format!("skink-lean-{}", std::process::id()));
        let repo = tmp.join("repo");
        std::fs::create_dir_all(repo.join(".git/rebase-merge")).unwrap();
        std::fs::create_dir_all(repo.join(".git/worktrees/w")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let bare = tmp.join("bare.git");
        std::fs::create_dir_all(bare.join("objects")).unwrap();
        std::fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let none = || None;
        let msg = |m| LeanKind::GitMessage(m);
        let cases: Vec<(ModeFlag, PathBuf, LaunchMode)> = vec![
            (
                ModeFlag::Auto,
                repo.join(".git/COMMIT_EDITMSG"),
                lean(msg(GitMsg::Commit), Why::GitFile),
            ),
            (
                ModeFlag::Auto,
                PathBuf::from(".git/MERGE_MSG"),
                lean(msg(GitMsg::Merge), Why::GitFile),
            ),
            (
                ModeFlag::Auto,
                repo.join(".git/worktrees/w/COMMIT_EDITMSG"),
                lean(msg(GitMsg::Commit), Why::GitFile),
            ),
            (
                ModeFlag::Auto,
                repo.join(".git/rebase-merge/git-rebase-todo"),
                lean(LeanKind::RebaseTodo, Why::GitFile),
            ),
            (
                ModeFlag::Auto,
                repo.join(".git/addp-hunk-edit.diff"),
                lean(LeanKind::HunkEdit, Why::GitFile),
            ),
            (
                ModeFlag::Auto,
                bare.join("TAG_EDITMSG"),
                lean(msg(GitMsg::Tag), Why::GitFile),
            ),
            // A file named like git's, outside a git dir, is just a file;
            // here it sits in the temp folder, so it is a scratch note.
            (
                ModeFlag::Auto,
                tmp.join("COMMIT_EDITMSG"),
                lean(LeanKind::Scratch, Why::TempFile),
            ),
            (
                ModeFlag::Auto,
                tmp.join("claude-prompt-1234.md"),
                lean(LeanKind::Scratch, Why::TempFile),
            ),
            // A project cloned into /tmp opens as a project.
            (
                ModeFlag::Auto,
                repo.join("src/main.rs"),
                LaunchMode::Project,
            ),
            (
                ModeFlag::Project,
                repo.join(".git/COMMIT_EDITMSG"),
                LaunchMode::Project,
            ),
            (
                ModeFlag::Scratch,
                repo.join("src/main.rs"),
                lean(LeanKind::Scratch, Why::Flag),
            ),
            (
                ModeFlag::Scratch,
                repo.join(".git/COMMIT_EDITMSG"),
                lean(msg(GitMsg::Commit), Why::Flag),
            ),
        ];
        for (flag, path, want) in cases {
            assert_eq!(detect(flag, Some(&path), none), want, "{flag:?} {path:?}");
        }
        // Outside temp and git, the parent program decides.
        let home_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert_eq!(
            detect(ModeFlag::Auto, Some(&home_file), || Some("claude".into())),
            lean(LeanKind::Scratch, Why::Parent("claude".into()))
        );
        assert_eq!(
            detect(ModeFlag::Auto, Some(&home_file), || Some("ghostty".into())),
            LaunchMode::Project
        );
        assert_eq!(detect(ModeFlag::Auto, None, none), LaunchMode::Project);
        assert_eq!(
            detect(ModeFlag::Scratch, None, none),
            lean(LeanKind::Scratch, Why::Flag)
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rebase_verbs_cycle_and_colour() {
        assert_eq!(next_verb("pick 1a2b fix"), Some(((0, 4), "reword")));
        assert_eq!(next_verb("f 1a2b fix"), Some(((0, 1), "drop")));
        assert_eq!(next_verb("drop 1a2b fix"), Some(((0, 4), "pick")));
        assert_eq!(next_verb("exec make"), None);
        assert_eq!(next_verb("# comment"), None);
        let st = git_styles(LeanKind::RebaseTodo, "pick 1a2b fix it");
        assert_eq!(
            st,
            vec![
                (0, 4, skink_syntax::Hl::Keyword),
                (5, 9, skink_syntax::Hl::Number)
            ]
        );
    }
}
