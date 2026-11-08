//! ratatui front end. `App` is state + `render`; the event loop in [`run`]
//! feeds it terminal events, and tests feed it synthetic ones against
//! ratatui's `TestBackend`.

mod clip;
mod commands;
mod icons;
mod input;
mod lsp;
mod picker;
mod run;
mod termview;
mod text;
mod theme;
mod tree;
mod view;

use clip::Clipboard;
use ratatui::{
    Frame,
    crossterm::{
        event::{
            self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
            EnableMouseCapture, Event, KeyEvent, KeyEventKind, MouseButton, MouseEvent,
            MouseEventKind,
        },
        execute,
    },
    layout::{Constraint, Layout, Position, Rect},
    style::Style,
    text::{Line, Span},
};
use skink_config::Keymap;
use skink_core::{Backend, CommandRegistry, Cursor, Editor, Request};
use skink_term::{CommandSpec, MouseKind, Screen, Term, TermEvent, WakeFn};
use std::{
    io,
    sync::{Arc, mpsc},
};
use text::char_col_at;

const SCROLLBACK: usize = 3000;
const WHEEL_LINES: isize = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Editor,
    Term,
    Tree,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PromptKind {
    SaveAs,
    Find,
    Command,
    Open,
    QuitConfirm,
}

struct Prompt {
    kind: PromptKind,
    label: String,
    input: String,
    /// Position while browsing command history (None = editing a new line).
    hist: Option<usize>,
}

struct TermPane {
    term: Term,
    screen: Screen,
    exited: bool,
    /// Program name, shown until the app inside sets a window title.
    label: String,
    /// Output arrived while this tab was in the background.
    activity: bool,
}

/// Everything that belongs to one open file besides the `Editor` itself.
struct BufState {
    editor: Editor,
    hscroll: usize,
    git_branch: Option<String>,
    lsp_rev: u64,
    lsp_dirty: bool,
    was_dirty: bool,
}

/// A tab is a file or a terminal. The *live* file's state sits in `App`'s own
/// fields (`Buffer(None)`); other files are parked here until selected.
enum Tab {
    Buffer(Option<Box<BufState>>),
    Term(Box<TermPane>),
}

#[derive(Default, Clone, Copy)]
struct Areas {
    top: Rect,
    tree: Rect,
    editor: Rect,
    term: Rect,
}

pub struct App {
    pub editor: Editor,
    keymap: Keymap,
    registry: CommandRegistry,
    backend: Backend,
    hscroll: usize,
    prompt: Option<Prompt>,
    tabs: Vec<Tab>,
    /// Selected tab.
    active: usize,
    /// Index of the file tab whose state is live in `editor` & co.
    cur_buf: usize,
    /// (x_start, x_end, tab index, close button x) per tab-bar entry.
    tab_hits: Vec<(u16, u16, usize, Option<u16>)>,
    focus: Focus,
    areas: Areas,
    clip: Clipboard,
    wake: Option<WakeFn>,
    quit: bool,
    osc52: bool,
    lsp: Option<lsp::LspLink>,
    lsp_requested: bool,
    lsp_rev: u64,
    lsp_dirty: bool,
    was_dirty: bool,
    diag_status: (usize, usize),
    diag_here: Option<(skink_lsp::Severity, String)>,
    theme: theme::Theme,
    icons: icons::IconSet,
    tree: Option<tree::FileTree>,
    tree_visible: bool,
    git_branch: Option<String>,
    cursor_screen: Option<(u16, u16)>,
    picker: Option<picker::Picker>,
    settings: skink_config::Settings,
    config_dir: Option<std::path::PathBuf>,
    state_dir: Option<std::path::PathBuf>,
    recent: Vec<std::path::PathBuf>,
    conflict_cache: (usize, u64, Vec<skink_core::Conflict>),
    cmd_history: Vec<String>,
    syntax: Option<skink_syntax::Highlighter>,
    hl_rev: u64,
    hl_text: String,
}

impl App {
    pub fn new(editor: Editor, keymap: Keymap, backend: Backend) -> Self {
        Self {
            editor,
            keymap,
            registry: CommandRegistry::builtin(),
            backend,
            hscroll: 0,
            prompt: None,
            tabs: vec![Tab::Buffer(None)],
            active: 0,
            cur_buf: 0,
            tab_hits: Vec::new(),
            focus: Focus::Editor,
            areas: Areas::default(),
            clip: Clipboard::new(),
            wake: None,
            quit: false,
            osc52: false,
            lsp: None,
            lsp_requested: false,
            lsp_rev: 0,
            lsp_dirty: false,
            was_dirty: false,
            diag_status: (0, 0),
            diag_here: None,
            theme: theme::Theme::auto(),
            icons: icons::IconSet::auto(),
            tree: None,
            tree_visible: false,
            git_branch: None,
            cursor_screen: None,
            picker: None,
            settings: skink_config::Settings::default(),
            config_dir: skink_config::config_dir(),
            state_dir: skink_config::state_dir(),
            recent: Vec::new(),
            conflict_cache: (usize::MAX, u64::MAX, Vec::new()),
            cmd_history: Vec::new(),
            syntax: None,
            hl_rev: u64::MAX,
            hl_text: String::new(),
        }
    }

    /// Called when terminal output arrives, to wake the event loop.
    pub fn set_wake(&mut self, wake: WakeFn) {
        self.wake = Some(wake);
    }

    /// Start language-server support for the open file (servers spawn
    /// lazily and only if installed). Needs the wake callback set first.
    pub fn request_lsp(&mut self) {
        self.lsp_requested = true;
    }

    fn enable_lsp(&mut self) {
        let Some(path) = self.editor.buffer.path().map(Into::into) else {
            return;
        };
        let path: std::path::PathBuf = path;
        if path.is_dir() || self.wake.is_none() {
            return;
        }
        let root = self.cwd().unwrap_or_else(|| std::path::PathBuf::from("."));
        let wake = self.wake.clone();
        let link = lsp::LspLink::start(root, move || {
            if let Some(w) = &wake {
                w();
            }
        });
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        link.open(&path, self.editor.buffer.text());
        self.lsp_rev = self.editor.buffer.revision();
        self.lsp = Some(link);
    }

    /// True while an edit has not been sent to the language server yet.
    pub fn lsp_pending(&self) -> bool {
        self.lsp.is_some() && self.lsp_dirty
    }

    /// Send the latest text (called after the typing pause).
    pub fn flush_lsp(&mut self) {
        let (Some(link), Some(path)) = (&self.lsp, self.editor.buffer.path()) else {
            return;
        };
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if self.lsp_dirty {
            link.change(&path, self.editor.buffer.text());
            self.lsp_dirty = false;
        }
        let dirty = self.editor.buffer.is_dirty();
        if self.was_dirty && !dirty {
            link.save(&path);
        }
        self.was_dirty = dirty;
        link.drain();
    }

    fn track_changes(&mut self) {
        let rev = self.editor.buffer.revision();
        if rev != self.lsp_rev {
            self.lsp_rev = rev;
            self.lsp_dirty = true;
        }
        if self.was_dirty && !self.editor.buffer.is_dirty() {
            self.lsp_dirty = true; // a save: flush the text, then notify
        }
    }

    /// Show a file tree rooted at `root`; `visible` opens the sidebar.
    pub fn set_tree_root(&mut self, root: std::path::PathBuf, visible: bool) {
        let mut t = tree::FileTree::new(root);
        if let Some(p) = self.editor.buffer.path() {
            if let Ok(abs) = std::fs::canonicalize(p) {
                t.reveal(&abs);
            }
        }
        self.tree = Some(t);
        self.tree_visible = visible;
        if visible {
            self.focus = Focus::Tree;
        }
    }

    /// Project root for the sidebar: the nearest ancestor with a VCS or
    /// manifest marker, else the file's directory.
    pub fn project_root(&self) -> std::path::PathBuf {
        let start = self.cwd().unwrap_or_else(|| std::path::PathBuf::from("."));
        let start = std::fs::canonicalize(&start).unwrap_or(start);
        for dir in start.ancestors() {
            if [
                ".git",
                "Cargo.toml",
                "package.json",
                "go.mod",
                "pyproject.toml",
            ]
            .iter()
            .any(|m| dir.join(m).exists())
            {
                return dir.to_path_buf();
            }
        }
        start
    }

    // ---- tabs --------------------------------------------------------------

    fn is_buffer(&self, i: usize) -> bool {
        matches!(self.tabs.get(i), Some(Tab::Buffer(_)))
    }

    fn active_is_term(&self) -> bool {
        matches!(self.tabs.get(self.active), Some(Tab::Term(_)))
    }

    fn active_term(&mut self) -> Option<&mut TermPane> {
        match self.tabs.get_mut(self.active) {
            Some(Tab::Term(p)) => Some(p),
            _ => None,
        }
    }

    /// The editor of file tab `i` (live or parked).
    fn tab_editor(&self, i: usize) -> Option<&Editor> {
        match self.tabs.get(i)? {
            Tab::Buffer(None) => Some(&self.editor),
            Tab::Buffer(Some(st)) => Some(&st.editor),
            Tab::Term(_) => None,
        }
    }

    fn stash_live(&mut self) -> BufState {
        BufState {
            editor: std::mem::replace(&mut self.editor, Editor::new(skink_core::Buffer::empty())),
            hscroll: std::mem::take(&mut self.hscroll),
            git_branch: self.git_branch.take(),
            lsp_rev: self.lsp_rev,
            lsp_dirty: self.lsp_dirty,
            was_dirty: self.was_dirty,
        }
    }

    fn load_live(&mut self, st: BufState) {
        self.editor = st.editor;
        self.hscroll = st.hscroll;
        self.git_branch = st.git_branch;
        self.lsp_rev = st.lsp_rev;
        self.lsp_dirty = st.lsp_dirty;
        self.was_dirty = st.was_dirty;
        self.editor
            .set_viewport_height(self.areas.editor.height as usize);
        self.syntax_reset();
    }

    /// Select tab `idx`; file tabs are swapped into the live fields.
    pub fn switch_to(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        if self.is_buffer(idx) {
            if idx != self.cur_buf {
                if let Tab::Buffer(Some(st)) =
                    std::mem::replace(&mut self.tabs[idx], Tab::Buffer(None))
                {
                    let live = self.stash_live();
                    self.tabs[self.cur_buf] = Tab::Buffer(Some(Box::new(live)));
                    self.load_live(*st);
                    self.cur_buf = idx;
                }
            }
            self.focus = Focus::Editor;
        } else {
            self.focus = Focus::Term;
            if let Some(Tab::Term(p)) = self.tabs.get_mut(idx) {
                p.activity = false;
            }
        }
        self.active = idx;
    }

    pub fn next_tab(&mut self, delta: isize) {
        let n = self.tabs.len() as isize;
        let i = (self.active as isize + delta).rem_euclid(n) as usize;
        self.switch_to(i);
    }

    fn lsp_open_live(&mut self) {
        if let (Some(link), Some(p)) = (&self.lsp, self.editor.buffer.path()) {
            if !self.editor.buffer.is_read_only() {
                let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                link.open(&abs, self.editor.buffer.text());
            }
        }
        self.lsp_rev = self.editor.buffer.revision();
        self.was_dirty = false;
        self.lsp_dirty = false;
    }

    /// Open a file in a tab (switching to it if it is already open).
    pub fn open_path(&mut self, path: &std::path::Path) {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        for i in 0..self.tabs.len() {
            let same = self
                .tab_editor(i)
                .and_then(|e| e.buffer.path())
                .is_some_and(|p| {
                    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()) == canon
                });
            if same {
                return self.switch_to(i);
            }
        }
        match skink_core::Buffer::open(path) {
            Ok(b) => {
                if !b.is_read_only() {
                    if let Some(sd) = &self.state_dir {
                        self.recent = skink_config::recent_push(sd, &canon);
                    }
                }
                self.add_buffer(b, false);
                self.editor.tab_width = self.settings.tabsize;
            }
            Err(e) => self.editor.message = Some(format!("{}: {e}", path.display())),
        }
    }

    /// Add a file tab. A pristine lone scratch buffer is replaced instead,
    /// unless `force_new`.
    fn add_buffer(&mut self, buffer: skink_core::Buffer, force_new: bool) {
        let path = buffer.path().map(std::path::Path::to_path_buf);
        let files = self
            .tabs
            .iter()
            .filter(|t| matches!(t, Tab::Buffer(_)))
            .count();
        let pristine = files == 1
            && self.editor.buffer.path().is_none()
            && !self.editor.buffer.is_dirty()
            && self.editor.buffer.len_chars() == 0;
        if pristine && !force_new {
            self.switch_to(self.cur_buf);
            self.editor = Editor::new(buffer);
            self.editor
                .set_viewport_height(self.areas.editor.height as usize);
            self.hscroll = 0;
            self.git_branch = path.as_deref().and_then(view::git_branch);
            self.syntax_reset();
            self.lsp_open_live();
            return;
        }
        let st = BufState {
            editor: Editor::new(buffer),
            hscroll: 0,
            git_branch: path.as_deref().and_then(view::git_branch),
            lsp_rev: 0,
            lsp_dirty: false,
            was_dirty: false,
        };
        self.tabs.push(Tab::Buffer(Some(Box::new(st))));
        let idx = self.tabs.len() - 1;
        self.switch_to(idx);
        self.lsp_open_live();
    }

    pub fn new_scratch_tab(&mut self) {
        self.add_buffer(skink_core::Buffer::empty(), true);
    }

    /// Close tab `idx`; closing the last file tab quits.
    pub fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        if self.is_buffer(idx) {
            let files: Vec<usize> = (0..self.tabs.len())
                .filter(|&i| self.is_buffer(i))
                .collect();
            if files.len() == 1 {
                self.quit = true;
                return;
            }
            if idx == self.cur_buf {
                let target =
                    files.iter().copied().find(|&i| i > idx).unwrap_or_else(|| {
                        files.iter().copied().rev().find(|&i| i < idx).unwrap_or(0)
                    });
                self.switch_to(target);
            }
            if let (Some(link), Some(Tab::Buffer(Some(st)))) = (&self.lsp, self.tabs.get(idx)) {
                if let Some(p) = st.editor.buffer.path() {
                    link.close(&std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
                }
            }
        }
        let was_active = self.active == idx;
        self.tabs.remove(idx);
        if self.cur_buf > idx {
            self.cur_buf -= 1;
        }
        if self.active > idx {
            self.active -= 1;
        } else if was_active {
            self.active = idx.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.switch_to(self.active);
    }

    /// Ask to close the live file tab (prompting when it has unsaved changes).
    fn request_close_live(&mut self) {
        if self.editor.buffer.is_dirty() {
            let what = if self
                .tabs
                .iter()
                .filter(|t| matches!(t, Tab::Buffer(_)))
                .count()
                > 1
            {
                "closing"
            } else {
                "quitting"
            };
            self.open_prompt(
                PromptKind::QuitConfirm,
                &format!("Save changes before {what}? (y)es / (n)o / Esc "),
                "",
            );
        } else {
            self.close_tab(self.cur_buf);
        }
    }

    fn any_dirty(&self) -> bool {
        (0..self.tabs.len())
            .filter_map(|i| self.tab_editor(i))
            .any(|e| e.buffer.is_dirty())
    }

    /// Click a tab to select it, its close button (or middle-click) to close.
    fn tab_bar_mouse(&mut self, m: MouseEvent) {
        let hit = self
            .tab_hits
            .iter()
            .find(|(a, b, _, _)| m.column >= *a && m.column < *b)
            .copied();
        let Some((_, _, idx, close_x)) = hit else {
            return;
        };
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if close_x == Some(m.column) {
                    self.close_tab_with_confirm(idx);
                } else {
                    self.switch_to(idx);
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => self.close_tab_with_confirm(idx),
            _ => {}
        }
    }

    fn close_tab_with_confirm(&mut self, idx: usize) {
        if self.is_buffer(idx) {
            // Select it first so the unsaved-changes prompt is about this file.
            self.switch_to(idx);
            self.request_close_live();
        } else {
            self.close_tab(idx);
        }
    }

    fn tree_mouse(&mut self, m: MouseEvent) {
        let rows = self.areas.tree.height.saturating_sub(2) as usize;
        let top = self.areas.tree.y + 2;
        let Some(t) = &mut self.tree else { return };
        match m.kind {
            MouseEventKind::ScrollUp => t.scroll = t.scroll.saturating_sub(WHEEL_LINES as usize),
            MouseEventKind::ScrollDown => {
                t.scroll = (t.scroll + WHEEL_LINES as usize)
                    .min(t.nodes.len().saturating_sub(rows.max(1)));
            }
            MouseEventKind::Down(MouseButton::Left) if m.row >= top => {
                let idx = t.scroll + (m.row - top) as usize;
                if idx < t.nodes.len() {
                    t.select(idx);
                    self.focus = Focus::Tree;
                    let open = t.activate();
                    if let Some(p) = open {
                        self.open_path(&p);
                    }
                }
            }
            _ => {}
        }
    }

    fn toggle_tree(&mut self) {
        if self.tree.is_none() {
            let root = self.project_root();
            self.set_tree_root(root, true);
            return;
        }
        match (self.tree_visible, self.focus) {
            (false, _) => {
                self.tree_visible = true;
                self.focus = Focus::Tree;
            }
            (true, Focus::Tree) => self.focus = Focus::Editor,
            (true, _) => self.focus = Focus::Tree,
        }
    }

    fn tree_key(&mut self, ev: KeyEvent) {
        use ratatui::crossterm::event::KeyCode;
        let h = self.areas.tree.height.saturating_sub(2) as usize;
        let Some(t) = &mut self.tree else { return };
        let mut open = None;
        match ev.code {
            KeyCode::Up => t.move_selection(-1),
            KeyCode::Down => t.move_selection(1),
            KeyCode::PageUp => t.move_selection(-(h.max(2) as isize - 1)),
            KeyCode::PageDown => t.move_selection(h.max(2) as isize - 1),
            KeyCode::Home => t.select(0),
            KeyCode::End => t.select(usize::MAX),
            KeyCode::Enter | KeyCode::Right => open = t.activate(),
            KeyCode::Left => t.collapse_or_parent(),
            KeyCode::Char('.') => t.toggle_hidden(),
            KeyCode::Char('q') | KeyCode::Esc => self.focus = Focus::Editor,
            KeyCode::Char('h') if self.tree_visible => self.tree_visible = false,
            _ => {}
        }
        if self.tree_visible && self.focus == Focus::Tree && self.tree.is_none() {
            self.focus = Focus::Editor;
        }
        if !self.tree_visible {
            self.focus = Focus::Editor;
        }
        if let Some(p) = open {
            self.open_path(&p);
        }
    }

    // ---- settings, views, quick open ------------------------------------------

    /// Apply `settings.json` values to the app (tab size, icons, ...).
    pub fn apply_settings(&mut self, s: skink_config::Settings) {
        self.editor.tab_width = s.tabsize;
        for t in &mut self.tabs {
            if let Tab::Buffer(Some(st)) = t {
                st.editor.tab_width = s.tabsize;
            }
        }
        self.icons = match s.icons.as_str() {
            "nerd" => icons::IconSet::Nerd,
            "ascii" => icons::IconSet::Ascii,
            _ => icons::IconSet::auto(),
        };
        self.settings = s;
    }

    /// One-time setup for the initial buffer: user config, git branch,
    /// syntax highlighting, recent files. Problems show in the status bar.
    pub fn startup(&mut self) {
        let errs = self.load_user_config();
        self.syntax_reset();
        if let Some(p) = self.editor.buffer.path() {
            self.git_branch = view::git_branch(p);
        }
        if !errs.is_empty() {
            self.editor.message = Some(errs.join("; "));
        }
    }

    /// Load `settings.json` and `bindings.json` from the config directory.
    /// Returns problems found (shown as a status message).
    pub fn load_user_config(&mut self) -> Vec<String> {
        let Some(dir) = self.config_dir.clone() else {
            return Vec::new();
        };
        let (s, mut errs) = skink_config::Settings::load(&dir);
        self.apply_settings(s);
        if let Ok(text) = std::fs::read_to_string(dir.join("bindings.json")) {
            match skink_config::parse_bindings(&text) {
                Ok(pairs) => {
                    errs.extend(
                        self.keymap
                            .apply_user(&pairs)
                            .into_iter()
                            .map(|k| format!("bindings.json: unknown key {k}")),
                    );
                }
                Err(e) => errs.push(e),
            }
        }
        if let Some(sd) = &self.state_dir {
            self.recent = skink_config::recent_load(sd);
        }
        self.syntax_reset();
        errs
    }

    /// Open anything that is not a file the same way as a file: in a tab.
    pub fn open_view(&mut self, title: &str, text: &str) {
        self.add_buffer(skink_core::Buffer::view(title, text), true);
    }

    /// Open a file from the config directory, creating it with defaults.
    fn open_config_file(&mut self, name: &str, default: &str) {
        let Some(dir) = self.config_dir.clone() else {
            self.editor.message = Some("No config directory (set SKINK_CONFIG_DIR)".into());
            return;
        };
        let path = dir.join(name);
        if !path.exists() {
            if let Err(e) =
                std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, default))
            {
                self.editor.message = Some(format!("{}: {e}", path.display()));
                return;
            }
        }
        self.open_path(&path);
    }

    fn project_dir(&self) -> std::path::PathBuf {
        self.project_root()
    }

    /// Run a shell command and show its output in a tab.
    fn run_to_view(&mut self, cmdline: &str, title: &str) {
        let (out, ok) = run::run_capture(
            cmdline,
            Some(&self.project_dir()),
            std::time::Duration::from_secs(15),
        );
        let mut text = format!("$ {cmdline}\n\n{out}");
        if !ok && !out.contains("[killed") {
            text.push_str("\n[command failed]\n");
        }
        self.open_view(title, &text);
    }

    /// Open every file git reports as conflicted and jump to the first marker.
    fn open_conflicts(&mut self) {
        let root = self.project_dir();
        let (top, ok) = run::run_capture(
            "git rev-parse --show-toplevel",
            Some(&root),
            std::time::Duration::from_secs(5),
        );
        if !ok {
            self.editor.message = Some("Not a git repository".into());
            return;
        }
        let top = std::path::PathBuf::from(top.trim());
        let (list, _) = run::run_capture(
            "git diff --name-only --diff-filter=U",
            Some(&top),
            std::time::Duration::from_secs(10),
        );
        let files: Vec<std::path::PathBuf> = list
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| top.join(l.trim()))
            .collect();
        if files.is_empty() {
            self.editor.message = Some("No merge conflicts".into());
            return;
        }
        for f in &files {
            self.open_path(f);
        }
        self.editor.goto_conflict(true);
        self.editor.message = Some(format!(
            "{} conflicted file(s) opened; :ours / :theirs / :both resolve the block",
            files.len()
        ));
    }

    fn open_picker(&mut self) {
        let root = self.project_dir();
        let mut tab_items = Vec::new();
        for i in 0..self.tabs.len() {
            let Some(e) = self.tab_editor(i) else {
                continue;
            };
            let path = e
                .buffer
                .path()
                .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
            let label = e.buffer.title().unwrap_or_else(|| "[scratch]".into());
            let detail = path
                .as_ref()
                .map(|p| {
                    p.strip_prefix(&root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .into_owned()
                })
                .unwrap_or_default();
            tab_items.push(picker::Item {
                label,
                detail,
                target: match path {
                    Some(p) => picker::Target::File(p),
                    None => picker::Target::Tab(i),
                },
                tag: "open",
            });
        }
        let project = picker::project_files(&root, 5000);
        self.picker = Some(picker::Picker::new(tab_items, &self.recent, project, &root));
    }

    fn picker_key(&mut self, ev: KeyEvent) {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};
        let Some(p) = &mut self.picker else { return };
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        match ev.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Up => p.move_selection(-1),
            KeyCode::Down => p.move_selection(1),
            KeyCode::Char('p') if ctrl => p.move_selection(-1),
            KeyCode::Char('n') if ctrl => p.move_selection(1),
            KeyCode::Backspace => p.backspace(),
            KeyCode::Char('u') if ctrl => {
                p.input.clear();
                p.selected = 0;
            }
            KeyCode::Char(c) if !ctrl => p.type_char(c),
            KeyCode::Enter => {
                let target = p.current();
                self.picker = None;
                match target {
                    Some(picker::Target::Tab(i)) => self.switch_to(i),
                    Some(picker::Target::File(f)) => self.open_path(&f),
                    None => {}
                }
            }
            _ => {}
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    // ---- terminal tabs ---------------------------------------------------

    /// Open a terminal in a new tab running `spec`, and switch to it.
    pub fn open_terminal(&mut self, spec: CommandSpec) -> io::Result<()> {
        let label = std::path::Path::new(&spec.program).file_name().map_or_else(
            || spec.program.clone(),
            |n| n.to_string_lossy().into_owned(),
        );
        let spec = match self.cwd() {
            Some(dir) => spec.cwd(dir),
            None => spec,
        };
        let (rows, cols) = (
            self.areas.editor.height.max(8),
            self.areas.editor.width.max(20),
        );
        let term = Term::spawn_with_wake(spec, rows, cols, SCROLLBACK, self.wake.clone())?;
        let screen = term.screen();
        self.tabs.push(Tab::Term(Box::new(TermPane {
            term,
            screen,
            exited: false,
            label,
            activity: false,
        })));
        let idx = self.tabs.len() - 1;
        self.switch_to(idx);
        Ok(())
    }

    /// Close the active terminal tab.
    pub fn close_terminal(&mut self) {
        if self.active_is_term() {
            self.close_tab(self.active);
        }
    }

    /// F12: jump between the current file and the most recent terminal tab,
    /// opening a terminal if there is none.
    fn toggle_terminal_focus(&mut self) {
        if self.active_is_term() {
            return self.switch_to(self.cur_buf);
        }
        let last_term = (0..self.tabs.len())
            .rev()
            .find(|&i| matches!(self.tabs[i], Tab::Term(_)));
        match last_term {
            Some(i) => self.switch_to(i),
            None => {
                let _ = self.open_terminal(CommandSpec::default_shell());
            }
        }
    }

    fn cwd(&self) -> Option<std::path::PathBuf> {
        let p = self.editor.buffer.path()?;
        if p.is_dir() {
            Some(p.to_path_buf())
        } else {
            p.parent().filter(|d| d.is_dir()).map(Into::into)
        }
    }

    /// Drain terminal events from every terminal tab; true if anything changed.
    pub fn poll_terminal(&mut self) -> bool {
        let mut changed = false;
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let Tab::Term(pane) = tab else { continue };
            let mut got = false;
            while let Ok(ev) = pane.term.events().try_recv() {
                got = true;
                if matches!(ev, TermEvent::Exited) {
                    pane.exited = true;
                }
            }
            if got {
                changed = true;
                if i != active {
                    pane.activity = true;
                    // Keep the tab title (set by the program inside) fresh.
                    pane.term.snapshot_into(&mut pane.screen);
                }
            }
            // Apps inside (e.g. an AI CLI) may ask to set the clipboard.
            for req in pane.term.take_clipboard_requests() {
                if req.data_base64 != "?" {
                    clip::write_osc52(&req.data_base64);
                }
            }
        }
        changed
    }

    // ---- input -------------------------------------------------------------

    pub fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) => self.handle_key(k),
            Event::Mouse(m) => self.handle_mouse(m),
            Event::Paste(s) => self.handle_paste(&s),
            _ => {}
        }
        self.track_changes();
    }

    pub fn handle_paste(&mut self, s: &str) {
        if let Some(p) = &mut self.picker {
            for c in s.chars().filter(|c| !c.is_control()) {
                p.type_char(c);
            }
        } else if let Some(p) = &mut self.prompt {
            p.input.push_str(&s.replace(['\n', '\r'], " "));
        } else if self.focus == Focus::Term {
            if let Some(pane) = self.active_term() {
                let _ = pane.term.paste(s);
            }
        } else {
            self.editor.insert_text(s);
        }
    }

    pub fn handle_key(&mut self, ev: KeyEvent) {
        // Windows reports key releases too; only act on presses/repeats.
        if ev.kind == KeyEventKind::Release {
            return;
        }
        if self.picker.is_some() {
            return self.picker_key(ev);
        }
        if self.prompt.is_some() {
            return self.prompt_key(ev);
        }
        let Some(spec) = input::key_spec(ev) else {
            return;
        };

        if spec.key == skink_config::Key::F(12) {
            if spec.shift {
                let _ = self.open_terminal(CommandSpec::default_shell());
                return;
            }
            return self.toggle_terminal_focus();
        }
        // Tab switching works from every kind of tab (Alt-1..9, Ctrl-PageUp/Down).
        if spec.alt && !spec.ctrl {
            if let skink_config::Key::Char(c @ '1'..='9') = spec.key {
                return self.switch_to(c as usize - '1' as usize);
            }
        }
        if spec.ctrl && !spec.alt {
            match spec.key {
                skink_config::Key::PageUp => return self.next_tab(-1),
                skink_config::Key::PageDown => return self.next_tab(1),
                _ => {}
            }
        }
        if spec.key == skink_config::Key::F(11) {
            return self.close_terminal();
        }
        if spec.key == skink_config::Key::F(8) {
            return self.toggle_tree();
        }
        if self.focus == Focus::Tree {
            return self.tree_key(ev);
        }

        if self.focus == Focus::Term {
            let exited = self.active_term().is_some_and(|p| p.exited);
            if exited {
                return self.close_terminal();
            }
            if let Some(pane) = self.active_term() {
                let _ = pane.term.send_key(spec);
            }
            return;
        }

        match self.keymap.command(&spec).map(str::to_owned) {
            Some(chain) => self.run_chain(&chain),
            None => {
                if let skink_config::Key::Char(c) = spec.key {
                    if !spec.ctrl && !spec.alt {
                        self.editor.insert_char(c);
                    }
                }
            }
        }
    }

    /// Run a Micro action string, then act on whatever the editor asked for.
    pub fn run_chain(&mut self, chain: &str) {
        if let Some(rest) = chain.strip_prefix("command-edit:") {
            return self.open_prompt(PromptKind::Command, ":", rest);
        }
        // Tab commands act on the app, not the buffer.
        match chain.split(['|', ',']).next().unwrap_or("") {
            "PreviousTab" => return self.next_tab(-1),
            "NextTab" => return self.next_tab(1),
            "FirstTab" => return self.switch_to(0),
            "LastTab" => return self.switch_to(self.tabs.len().saturating_sub(1)),
            "AddTab" => return self.new_scratch_tab(),
            "OpenFile" => return self.open_prompt(PromptKind::Open, "Open: ", ""),
            "QuickOpen" => return self.open_picker(),
            _ => {}
        }
        if chain.contains("Paste") {
            if let Some(t) = self.clip.get() {
                self.editor.clipboard = t;
            }
        }
        self.registry.run_chain(chain, &mut self.editor);
        if self.editor.clipboard_changed {
            self.editor.clipboard_changed = false;
            self.clip.set(&self.editor.clipboard, self.osc52);
        }
        match self.editor.request.take() {
            Some(Request::Quit) => self.request_close_live(),
            Some(Request::SaveAs) => self.open_prompt(PromptKind::SaveAs, "Save as: ", ""),
            Some(Request::Find) => {
                let q = self.editor.last_search.clone();
                self.open_prompt(PromptKind::Find, "Find: ", &q);
            }
            None => {}
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, label: &str, input: &str) {
        self.prompt = Some(Prompt {
            kind,
            label: label.to_owned(),
            input: input.to_owned(),
            hist: None,
        });
    }

    fn prompt_key(&mut self, ev: KeyEvent) {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};
        let Some(p) = &mut self.prompt else { return };
        if p.kind == PromptKind::QuitConfirm {
            let answer = match ev.code {
                KeyCode::Char('y' | 'Y') => Some(true),
                KeyCode::Char('n' | 'N') => Some(false),
                _ => None,
            };
            self.prompt = None;
            match answer {
                Some(true) => {
                    self.editor.save();
                    if self.editor.request.take() == Some(Request::SaveAs) {
                        self.open_prompt(PromptKind::SaveAs, "Save as: ", "");
                    } else if !self.editor.buffer.is_dirty() {
                        self.close_tab(self.cur_buf);
                    }
                }
                Some(false) => self.close_tab(self.cur_buf),
                None => {}
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
                if n > 0 {
                    let up = ev.code == KeyCode::Up;
                    let next = match (p.hist, up) {
                        (None, true) => Some(n - 1),
                        (Some(0), true) => Some(0),
                        (Some(i), true) => Some(i - 1),
                        (Some(i), false) if i + 1 < n => Some(i + 1),
                        _ => None,
                    };
                    p.hist = next;
                    p.input = next.map_or_else(String::new, |i| self.cmd_history[i].clone());
                }
            }
            KeyCode::Tab if matches!(p.kind, PromptKind::Command | PromptKind::Open) => {
                let input = p.input.clone();
                let completions = self.complete_input(&input, p_kind_is_command(&self.prompt));
                if let Some(p) = &mut self.prompt {
                    match completions.len() {
                        0 => {}
                        1 => p.input = completions.into_iter().next().unwrap_or_default(),
                        _ => {
                            let common = commands::common_prefix(&completions);
                            let shown: Vec<_> = completions.iter().take(8).cloned().collect();
                            if common.len() > input.len() {
                                p.input = common;
                            }
                            self.editor.message = Some(format!(
                                "{}{}",
                                shown.join("  "),
                                if completions.len() > 8 { " ..." } else { "" }
                            ));
                        }
                    }
                }
            }
            KeyCode::Char('u') if ev.modifiers.contains(KeyModifiers::CONTROL) => p.input.clear(),
            KeyCode::Char(c) if !ev.modifiers.contains(KeyModifiers::CONTROL) => {
                p.input.push(c);
                p.hist = None;
            }
            _ => {}
        }
    }

    /// Complete the command name, or the path after `open`/`e`/`tabnew`.
    /// Returns full replacement lines.
    fn complete_input(&self, input: &str, is_command: bool) -> Vec<String> {
        if !is_command {
            return commands::complete_path(input);
        }
        let trimmed = input.trim_start();
        match trimmed.split_once(' ') {
            None => commands::complete_name(trimmed, &self.registry),
            Some((cmd, arg)) if matches!(cmd, "open" | "e" | "tabnew") => {
                commands::complete_path(arg.trim_start())
                    .into_iter()
                    .map(|p| format!("{cmd} {p}"))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn finish_prompt(&mut self, p: Prompt) {
        match p.kind {
            PromptKind::SaveAs => {
                if !p.input.trim().is_empty() {
                    self.editor.buffer.set_path(p.input.trim().into());
                    self.editor.save();
                }
            }
            PromptKind::Find => {
                self.editor.find(&p.input, true);
            }
            PromptKind::Command => {
                let line = p.input.trim().to_owned();
                if !line.is_empty() && self.cmd_history.last() != Some(&line) {
                    self.cmd_history.push(line.clone());
                }
                self.run_command(&line);
            }
            PromptKind::Open => {
                let t = p.input.trim();
                if !t.is_empty() {
                    let loc = skink_core::parse_location(t, std::path::Path::exists);
                    if loc.path.is_dir() {
                        self.set_tree_root(loc.path.clone(), true);
                    } else if loc.path.exists() {
                        self.open_path(&loc.path);
                        self.editor.goto(loc.line, loc.col);
                    } else {
                        self.add_buffer(skink_core::Buffer::new_file(&loc.path), false);
                    }
                }
            }
            PromptKind::QuitConfirm => {}
        }
    }

    /// Micro-style command bar: `goto 12[:3]`, `save`, `quit`, `term [cmd..]`.
    pub fn run_command(&mut self, line: &str) {
        let mut parts = line.split_whitespace();
        let Some(cmd) = parts.next() else { return };
        let args: Vec<&str> = parts.collect();
        match cmd {
            "goto" => {
                let mut it = args.first().copied().unwrap_or("").split(':');
                let l = it.next().and_then(|s| s.parse().ok());
                let c = it.next().and_then(|s| s.parse().ok());
                if l.is_some() {
                    self.editor.goto(l, c);
                } else {
                    self.editor.message = Some("usage: goto LINE[:COL]".into());
                }
            }
            "tree" => self.toggle_tree(),
            "help" | "docs" => {
                let topic = args.first().copied();
                let t = commands::help_text(topic, skink_config::DEFAULT_BINDINGS, &self.registry);
                let title = topic.map_or_else(|| "Help".to_owned(), |t| format!("Help: {t}"));
                self.open_view(&title, &t);
            }
            "settings" => self.open_config_file("settings.json", skink_config::DEFAULT_SETTINGS),
            "bindings" | "keys" => {
                self.open_config_file("bindings.json", skink_config::DEFAULT_BINDINGS_FILE);
            }
            "reload" => {
                let errs = self.load_user_config();
                self.editor.message = Some(if errs.is_empty() {
                    "Config reloaded".into()
                } else {
                    errs.join("; ")
                });
            }
            "recent" | "quickopen" => self.open_picker(),
            "run" | "!" if !args.is_empty() => {
                let c = args.join(" ");
                self.run_to_view(&c, &format!("$ {c}"));
            }
            "diff" => self.run_to_view("git --no-pager diff", "git diff"),
            "gitstatus" | "status" => self.run_to_view("git --no-pager status -sb", "git status"),
            "git" if !args.is_empty() => {
                let c = format!("git --no-pager {}", args.join(" "));
                self.run_to_view(&c, &format!("$ {c}"));
            }
            "conflicts" => self.open_conflicts(),
            "ours" => self.run_chain("AcceptOurs"),
            "theirs" => self.run_chain("AcceptTheirs"),
            "both" => self.run_chain("AcceptBoth"),
            "nextconflict" => self.run_chain("ConflictNext"),
            "prevconflict" => self.run_chain("ConflictPrevious"),
            "save" | "w" => self.run_chain("Save"),
            "quit" | "q" => self.run_chain("Quit"),
            "qa" | "quitall" => {
                if self.any_dirty() {
                    self.editor.message =
                        Some("Unsaved changes in some tabs (use :q per tab)".into());
                } else {
                    self.quit = true;
                }
            }
            "open" | "e" | "tabnew" => match args.first() {
                Some(p) => self.open_path(std::path::Path::new(p)),
                None => self.new_scratch_tab(),
            },
            "tab" => {
                if let Some(n) = args.first().and_then(|s| s.parse::<usize>().ok()) {
                    self.switch_to(n.saturating_sub(1));
                }
            }
            "term" | "terminal" | "agent" | "tabterm" => {
                let spec = match args.split_first() {
                    Some((prog, rest)) => CommandSpec::new(*prog).args(rest.iter().copied()),
                    None => CommandSpec::default_shell(),
                };
                if let Err(e) = self.open_terminal(spec) {
                    self.editor.message = Some(format!("terminal: {e}"));
                }
            }
            other if self.registry.get(other).is_some() => self.run_chain(other),
            other => self.editor.message = Some(format!("Unknown command: {other} (try :help)")),
        }
    }

    pub fn handle_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        if self.areas.top.contains(pos) {
            return self.tab_bar_mouse(m);
        }
        if self.active_is_term() && self.areas.term.contains(pos) {
            self.focus = Focus::Term;
            let (col, row) = (m.column - self.areas.term.x, m.row - self.areas.term.y);
            let kind = match m.kind {
                MouseEventKind::Down(b) => Some(MouseKind::Down(map_button(b))),
                MouseEventKind::Up(b) => Some(MouseKind::Up(map_button(b))),
                MouseEventKind::Drag(b) => Some(MouseKind::Drag(map_button(b))),
                MouseEventKind::Moved => Some(MouseKind::Move),
                MouseEventKind::ScrollUp => Some(MouseKind::ScrollUp),
                MouseEventKind::ScrollDown => Some(MouseKind::ScrollDown),
                _ => None,
            };
            if let (Some(kind), Some(pane)) = (kind, self.active_term()) {
                let ev = skink_term::MouseEvent::new(kind, col, row);
                let handled = pane.term.send_mouse(ev).unwrap_or(false);
                if !handled {
                    match kind {
                        MouseKind::ScrollUp => pane.term.scroll_up(WHEEL_LINES as usize),
                        MouseKind::ScrollDown => pane.term.scroll_down(WHEEL_LINES as usize),
                        _ => {}
                    }
                }
            }
            return;
        }
        if self.tree_visible && self.areas.tree.contains(pos) {
            return self.tree_mouse(m);
        }
        if self.active_is_term() || !self.areas.editor.contains(pos) {
            return;
        }
        match m.kind {
            MouseEventKind::ScrollUp => self.editor.scroll_by(-WHEEL_LINES),
            MouseEventKind::ScrollDown => self.editor.scroll_by(WHEEL_LINES),
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
                self.focus = Focus::Editor;
                let gutter = self.gutter_width() as u16;
                let row = (m.row - self.areas.editor.y) as usize + self.editor.scroll;
                let line = row.min(self.editor.buffer.len_lines() - 1);
                let x =
                    (m.column.saturating_sub(self.areas.editor.x + gutter)) as usize + self.hscroll;
                let col = char_col_at(
                    &self.editor.buffer.line_text(line),
                    x,
                    self.editor.tab_width,
                );
                let extend = matches!(m.kind, MouseEventKind::Drag(_));
                self.editor.click(line, col, extend);
            }
            _ => {}
        }
    }
}

fn p_kind_is_command(p: &Option<Prompt>) -> bool {
    p.as_ref().is_some_and(|p| p.kind == PromptKind::Command)
}

fn map_button(b: MouseButton) -> skink_term::MouseButton {
    match b {
        MouseButton::Left => skink_term::MouseButton::Left,
        MouseButton::Right => skink_term::MouseButton::Right,
        MouseButton::Middle => skink_term::MouseButton::Middle,
    }
}

/// Selected char-column range on `line`, if any (end is exclusive).
fn line_selection(line: usize, s: Cursor, e: Cursor, len: usize) -> Option<(usize, usize)> {
    if line < s.line || line > e.line {
        return None;
    }
    let from = if line == s.line { s.col } else { 0 };
    let to = if line == e.line { e.col } else { len + 1 };
    (from < to).then_some((from, to))
}

enum Msg {
    Input(Event),
    Wake,
    InputClosed,
}

/// Run the TUI. With `exit_after_first_frame` it draws once and returns
/// (used by the startup/RSS measurement script).
pub fn run(mut app: App, exit_after_first_frame: bool) -> io::Result<()> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let wake_tx = tx.clone();
    app.set_wake(Arc::new(move || {
        let _ = wake_tx.send(Msg::Wake);
    }));
    app.osc52 = true;
    if app.lsp_requested {
        app.enable_lsp();
    }

    let mut terminal = ratatui::init();
    let _ = execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    if !exit_after_first_frame {
        std::thread::spawn(move || {
            while let Ok(ev) = event::read() {
                if tx.send(Msg::Input(ev)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Msg::InputClosed);
        });
    }
    let result = (|| {
        loop {
            terminal.draw(|f| app.render(f))?;
            if exit_after_first_frame || app.should_quit() {
                return Ok(());
            }
            // Block until something happens (zero idle CPU), then batch
            // everything queued so a burst costs one redraw.
            let mut msg = if app.lsp_pending() {
                match rx.recv_timeout(std::time::Duration::from_millis(250)) {
                    Ok(m) => Some(m),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        app.flush_lsp();
                        continue;
                    }
                    Err(_) => None,
                }
            } else {
                rx.recv().ok()
            };
            while let Some(m) = msg {
                match m {
                    Msg::Input(ev) => app.handle_event(ev),
                    Msg::Wake => {}
                    Msg::InputClosed => app.quit = true,
                }
                msg = rx.try_recv().ok();
            }
            app.poll_terminal();
            if app.lsp.is_some() && !app.lsp_dirty {
                app.flush_lsp();
            }
        }
    })();
    let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests;
