//! ratatui front end. `App` is state + `render`; the event loop in [`run`]
//! feeds it terminal events, and tests feed it synthetic ones against
//! ratatui's `TestBackend`.
//!
//! Layout of the crate:
//! - `doc`: one open document and its per-file UI state;
//! - `events`: keyboard, mouse and paste handling;
//! - `prompt`: the status-line prompts (save as, find, open, command line);
//! - `commands`: the `:` command table, completion and the help document;
//! - `view`: all drawing; `text`/`termview`/`theme`/`icons` are its helpers;
//! - `lsp`, `run`, `picker`, `tree`, `clip`: services the app owns.

mod clip;
mod commands;
mod doc;
mod events;
mod icons;
mod input;
mod lsp;
mod picker;
mod prompt;
mod run;
mod termview;
mod text;
mod theme;
mod tree;
pub mod update;
mod view;

use clip::Clipboard;
use doc::{Doc, DocId};
use ratatui::{
    crossterm::{
        event::{
            self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
            EnableMouseCapture, Event,
        },
        execute,
    },
    layout::Rect,
};
use skink_config::Keymap;
use skink_core::{Backend, Buffer, CommandRegistry, Editor};
use skink_term::{CommandSpec, Screen, Term, TermEvent, WakeFn};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};

const SCROLLBACK: usize = 3000;
/// Typing pause before edits are sent to the language server.
const LSP_DEBOUNCE: Duration = Duration::from_millis(250);
const CMD_HISTORY_MAX: usize = 200;

/// Which part of the window receives keys. The main area is whatever the
/// active tab is (a document or a terminal), so focus can never point at a
/// document that is not on screen. Derived from [`SidebarView`], never stored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Main,
    Tree,
}

/// The file tree beside the main area (absent until a folder is opened).
struct Sidebar {
    tree: tree::FileTree,
    view: SidebarView,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SidebarView {
    Hidden,
    /// On screen, keys go to the main area.
    Shown,
    /// On screen and receiving keys.
    Focused,
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

impl TermPane {
    fn title(&self) -> &str {
        if self.screen.title.is_empty() {
            &self.label
        } else {
            &self.screen.title
        }
    }
}

/// A tab is a document or a terminal. The live document sits in `App::doc`;
/// its slot holds `Doc(None)`. Every other document is parked in its slot.
enum Tab {
    Doc(Option<Box<Doc>>),
    Term(Box<TermPane>),
}

/// Screen regions from the last frame, for mouse hit-testing.
#[derive(Default, Clone, Copy)]
struct Areas {
    top: Rect,
    tree: Rect,
    main: Rect,
}

/// A clickable tab-bar entry from the last frame.
#[derive(Clone, Copy)]
struct TabHit {
    x0: u16,
    x1: u16,
    tab: usize,
    close_x: Option<u16>,
}

pub struct App {
    /// The live document (shown when a document tab is active).
    doc: Doc,
    tabs: Vec<Tab>,
    /// Selected tab.
    active: usize,
    /// Slot of the live document in `tabs`.
    cur_doc: usize,
    next_doc_id: u64,

    keymap: Keymap,
    /// The keymap the app was created with; `bindings.json` applies on top.
    base_keymap: Keymap,
    user_bindings: Vec<(String, String)>,
    registry: CommandRegistry,
    backend: Backend,

    prompt: Option<prompt::Prompt>,
    picker: Option<picker::Picker>,
    sidebar: Option<Sidebar>,
    tab_hits: Vec<TabHit>,
    areas: Areas,

    clip: Clipboard,
    osc52: bool,
    wake: Option<WakeFn>,
    quit: bool,
    lsp: Option<lsp::LspLink>,
    lsp_enabled: bool,
    jobs: run::Jobs,

    theme: theme::Theme,
    icons: icons::IconSet,
    settings: skink_config::Settings,
    config_dir: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    recent: Vec<PathBuf>,
    cmd_history: Vec<String>,
    update: update::UpdateState,
}

impl App {
    pub fn new(editor: Editor, keymap: Keymap, backend: Backend) -> Self {
        let settings = skink_config::Settings::default();
        Self {
            doc: Doc::new(DocId(0), editor, settings.syntax),
            tabs: vec![Tab::Doc(None)],
            active: 0,
            cur_doc: 0,
            next_doc_id: 1,
            base_keymap: keymap.clone(),
            keymap,
            user_bindings: Vec::new(),
            registry: CommandRegistry::builtin(),
            backend,
            prompt: None,
            picker: None,
            sidebar: None,
            tab_hits: Vec::new(),
            areas: Areas::default(),
            clip: Clipboard::new(),
            osc52: false,
            wake: None,
            quit: false,
            lsp: None,
            lsp_enabled: false,
            jobs: run::Jobs::default(),
            theme: theme::Theme::auto(),
            icons: icons::IconSet::auto(),
            settings,
            config_dir: skink_config::config_dir(),
            state_dir: skink_config::state_dir(),
            recent: Vec::new(),
            update: update::UpdateState::Idle,
            cmd_history: Vec::new(),
        }
    }

    /// The live document's editor.
    pub fn editor(&self) -> &Editor {
        &self.doc.editor
    }

    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.doc.editor
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Called when terminal output or language-server results arrive, to
    /// wake the event loop.
    pub fn set_wake(&mut self, wake: WakeFn) {
        self.wake = Some(wake);
    }

    /// Turn on language servers. They start lazily, for the first file that
    /// needs one, and only once the event loop provides a wake callback.
    pub fn request_lsp(&mut self) {
        self.lsp_enabled = true;
    }

    /// One-time setup: user config, then the initial document's services.
    pub fn startup(&mut self) {
        let errs = self.load_user_config();
        if !errs.is_empty() {
            self.doc.editor.message = Some(errs.join("; "));
        }
    }

    // ---- documents & tabs ------------------------------------------------

    fn is_doc(&self, i: usize) -> bool {
        matches!(self.tabs.get(i), Some(Tab::Doc(_)))
    }

    fn active_is_term(&self) -> bool {
        matches!(self.tabs.get(self.active), Some(Tab::Term(_)))
    }

    /// Keys go to the live document's editor.
    fn editor_focused(&self) -> bool {
        self.focus() == Focus::Main && !self.active_is_term()
    }

    fn active_term(&mut self) -> Option<&mut TermPane> {
        match self.tabs.get_mut(self.active) {
            Some(Tab::Term(p)) => Some(p),
            _ => None,
        }
    }

    /// The document in tab `i` (live or parked).
    fn doc_at(&self, i: usize) -> Option<&Doc> {
        match self.tabs.get(i)? {
            Tab::Doc(None) => Some(&self.doc),
            Tab::Doc(Some(d)) => Some(d),
            Tab::Term(_) => None,
        }
    }

    fn doc_at_mut(&mut self, i: usize) -> Option<&mut Doc> {
        match self.tabs.get_mut(i)? {
            Tab::Doc(None) => Some(&mut self.doc),
            Tab::Doc(Some(d)) => Some(d),
            Tab::Term(_) => None,
        }
    }

    fn find_doc(&self, pred: impl Fn(&Doc) -> bool) -> Option<usize> {
        (0..self.tabs.len()).find(|&i| self.doc_at(i).is_some_and(&pred))
    }

    fn doc_count(&self) -> usize {
        self.tabs
            .iter()
            .filter(|t| matches!(t, Tab::Doc(_)))
            .count()
    }

    fn any_dirty(&self) -> bool {
        (0..self.tabs.len()).any(|i| self.doc_at(i).is_some_and(|d| d.editor.buffer.is_dirty()))
    }

    /// Select tab `idx`. A parked document is swapped in as the live one.
    pub fn switch_to(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        if matches!(self.tabs[idx], Tab::Doc(Some(_))) {
            self.sync_lsp(); // the live doc is about to be parked: flush its edits
            let Tab::Doc(Some(mut parked)) = std::mem::replace(&mut self.tabs[idx], Tab::Doc(None))
            else {
                unreachable!("checked above")
            };
            std::mem::swap(&mut self.doc, &mut parked);
            parked.park();
            self.tabs[self.cur_doc] = Tab::Doc(Some(parked));
            self.cur_doc = idx;
            self.doc
                .editor
                .set_viewport_height(self.areas.main.height as usize);
        }
        if let Tab::Term(p) = &mut self.tabs[idx] {
            p.activity = false;
        }
        self.active = idx;
        self.focus_main();
        self.lsp_attach_live();
    }

    pub fn next_tab(&mut self, delta: isize) {
        let n = self.tabs.len() as isize;
        self.switch_to((self.active as isize + delta).rem_euclid(n) as usize);
    }

    /// A lone, empty, never-saved scratch document: opening a file replaces
    /// it instead of adding a tab (like an editor's initial untitled tab).
    fn live_is_pristine(&self) -> bool {
        let b = &self.doc.editor.buffer;
        self.doc_count() == 1
            && b.path().is_none()
            && b.title().is_none()
            && !b.is_dirty()
            && b.len_chars() == 0
    }

    /// Add a document tab and select it; returns its tab index.
    fn add_doc(&mut self, mut editor: Editor, replace_pristine: bool) -> usize {
        editor.tab_width = self.settings.tabsize;
        let id = DocId(self.next_doc_id);
        self.next_doc_id += 1;
        let doc = Doc::new(id, editor, self.settings.syntax);
        let idx = if replace_pristine && self.live_is_pristine() {
            self.doc = doc;
            self.cur_doc
        } else {
            self.tabs.push(Tab::Doc(Some(Box::new(doc))));
            self.tabs.len() - 1
        };
        self.switch_to(idx);
        idx
    }

    pub fn new_scratch_tab(&mut self) {
        self.add_doc(Editor::new(Buffer::empty()), false);
    }

    /// Open a file in a tab, or select its tab if it is already open.
    pub fn open_path(&mut self, path: &Path) {
        let canon = doc::canonical(path);
        if let Some(i) = self.find_doc(|d| d.canon.as_ref() == Some(&canon)) {
            return self.switch_to(i);
        }
        match Buffer::open(path) {
            Ok(b) => {
                if let Some(sd) = &self.state_dir {
                    self.recent = skink_config::recent_push(sd, &canon);
                }
                self.add_doc(Editor::new(b), true);
            }
            Err(e) => self.doc.editor.message = Some(format!("{}: {e}", path.display())),
        }
    }

    /// Open a path typed by the user: `file[:line[:col]]`, a folder (shown
    /// in the tree) or a new file (created on first save).
    fn open_location(&mut self, input: &str) {
        let loc = skink_core::parse_location(input, Path::exists);
        if loc.path.is_dir() {
            self.set_tree_root(doc::canonical(&loc.path), true);
        } else if loc.path.exists() {
            self.open_path(&loc.path);
            if loc.line.is_some() {
                self.doc.editor.goto(loc.line, loc.col);
            }
        } else {
            self.add_doc(Editor::new(Buffer::new_file(&loc.path)), true);
        }
    }

    /// Show text that is not a file (help, command output) in a read-only
    /// tab. A view with the same title is reused, like an editor's preview.
    pub fn open_view(&mut self, title: &str, text: &str) -> DocId {
        let same = |d: &Doc| {
            d.canon.is_none()
                && d.editor.buffer.is_read_only()
                && d.editor.buffer.title().as_deref() == Some(title)
        };
        let idx = match self.find_doc(same) {
            Some(i) => {
                if let Some(d) = self.doc_at_mut(i) {
                    d.replace_view(title, text);
                }
                self.switch_to(i);
                i
            }
            None => self.add_doc(Editor::new(Buffer::view(title, text)), false),
        };
        self.doc_at(idx).map_or(DocId(u64::MAX), |d| d.id)
    }

    /// Close tab `idx` without asking. Closing the last document quits.
    pub fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        let was_active = self.active == idx;
        if self.is_doc(idx) {
            if self.doc_count() == 1 {
                self.quit = true;
                return;
            }
            if idx == self.cur_doc {
                // Make another document live so this one is parked and can go.
                let others: Vec<usize> = (0..self.tabs.len())
                    .filter(|&i| i != idx && self.is_doc(i))
                    .collect();
                let next = others
                    .iter()
                    .copied()
                    .find(|&i| i > idx)
                    .or(others.last().copied());
                if let Some(n) = next {
                    self.switch_to(n);
                }
            }
            if let (Some(link), Some(Tab::Doc(Some(d)))) = (&self.lsp, self.tabs.get(idx))
                && d.lsp.opened
                && let Some(p) = &d.canon
            {
                link.close(p);
            }
        }
        self.tabs.remove(idx);
        if self.cur_doc > idx {
            self.cur_doc -= 1;
        }
        if self.active > idx {
            self.active -= 1;
        }
        // Like other editors: closing the selected tab selects its right
        // neighbour (or the new last tab).
        let next = if was_active {
            idx.min(self.tabs.len() - 1)
        } else {
            self.active
        };
        self.switch_to(next);
    }

    /// Close tab `idx`, asking first when it holds unsaved changes.
    fn request_close(&mut self, idx: usize) {
        if !self.doc_at(idx).is_some_and(|d| d.editor.buffer.is_dirty()) {
            return self.close_tab(idx);
        }
        self.switch_to(idx); // the question is about the document on screen
        let verb = if self.doc_count() > 1 {
            "closing"
        } else {
            "quitting"
        };
        self.open_prompt(
            prompt::PromptKind::CloseConfirm,
            &format!("Save changes before {verb}? (y)es / (n)o / Esc "),
            "",
        );
    }

    // ---- terminals -------------------------------------------------------

    /// Open a terminal in a new tab running `spec`, and switch to it.
    pub fn open_terminal(&mut self, spec: CommandSpec) -> io::Result<()> {
        let label = Path::new(&spec.program).file_name().map_or_else(
            || spec.program.clone(),
            |n| n.to_string_lossy().into_owned(),
        );
        let spec = spec.cwd(self.project_root());
        let rows = self.areas.main.height.max(8);
        let cols = self.areas.main.width.max(20);
        let term = Term::spawn_with_wake(spec, rows, cols, SCROLLBACK, self.wake.clone())?;
        let screen = term.screen();
        self.tabs.push(Tab::Term(Box::new(TermPane {
            term,
            screen,
            exited: false,
            label,
            activity: false,
        })));
        self.switch_to(self.tabs.len() - 1);
        Ok(())
    }

    fn open_terminal_or_report(&mut self, spec: CommandSpec) {
        if let Err(e) = self.open_terminal(spec) {
            self.doc.editor.message = Some(format!("terminal: {e}"));
        }
    }

    /// Close the active terminal tab.
    pub fn close_terminal(&mut self) {
        if self.active_is_term() {
            self.close_tab(self.active);
        }
    }

    /// F12: jump between the current document and the most recent terminal
    /// tab, opening a terminal if there is none.
    fn toggle_terminal(&mut self) {
        if self.active_is_term() {
            return self.switch_to(self.cur_doc);
        }
        match (0..self.tabs.len())
            .rev()
            .find(|&i| matches!(self.tabs[i], Tab::Term(_)))
        {
            Some(i) => self.switch_to(i),
            None => self.open_terminal_or_report(CommandSpec::default_shell()),
        }
    }

    /// Drain events from every terminal tab; true if anything changed.
    pub fn poll_terminals(&mut self) -> bool {
        let mut changed = false;
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let Tab::Term(pane) = tab else { continue };
            let mut got = false;
            for ev in pane.term.events().try_iter() {
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
            // Programs inside (e.g. an AI CLI) may ask to set the clipboard.
            for req in pane.term.take_clipboard_requests() {
                if req.data_base64 != "?" {
                    clip::write_osc52(&req.data_base64);
                }
            }
        }
        changed
    }

    // ---- file tree -------------------------------------------------------

    /// Show a file tree rooted at `root`; `visible` opens and focuses it.
    pub fn set_tree_root(&mut self, root: PathBuf, visible: bool) {
        // Same path form as documents, so the open file is highlighted.
        let mut t = tree::FileTree::new(doc::canonical(&root));
        if let Some(p) = &self.doc.canon {
            t.reveal(p);
        }
        let view = if visible {
            SidebarView::Focused
        } else {
            SidebarView::Hidden
        };
        self.sidebar = Some(Sidebar { tree: t, view });
    }

    fn focus(&self) -> Focus {
        match self.sidebar_view() {
            Some(SidebarView::Focused) => Focus::Tree,
            _ => Focus::Main,
        }
    }

    fn sidebar_view(&self) -> Option<SidebarView> {
        self.sidebar.as_ref().map(|s| s.view)
    }

    /// The one transition point for the sidebar (no-op without a tree).
    fn set_sidebar_view(&mut self, view: SidebarView) {
        if let Some(s) = &mut self.sidebar {
            s.view = view;
        }
    }

    /// Keys go back to the main area; the tree stays on screen.
    fn focus_main(&mut self) {
        if self.sidebar_view() == Some(SidebarView::Focused) {
            self.set_sidebar_view(SidebarView::Shown);
        }
    }

    fn tree(&self) -> Option<&tree::FileTree> {
        self.sidebar.as_ref().map(|s| &s.tree)
    }

    fn tree_mut(&mut self) -> Option<&mut tree::FileTree> {
        self.sidebar.as_mut().map(|s| &mut s.tree)
    }

    /// Create a file (opened in a tab) or folder under `dir`; nested names
    /// create their parents. The tree, when there is one, shows the result.
    fn create_entry(&mut self, dir: &Path, name: &str, what: tree::NewEntry) {
        match tree::create(dir, name, what) {
            Ok(path) => {
                let path = doc::canonical(&path);
                if let Some(t) = self.tree_mut() {
                    t.refresh_and_reveal(&path);
                }
                match what {
                    tree::NewEntry::File => self.open_path(&path),
                    tree::NewEntry::Folder => {
                        self.editor_mut().message = Some(format!("created {}", path.display()))
                    }
                }
            }
            Err(e) => self.editor_mut().message = Some(format!("{name}: {e}")),
        }
    }

    /// Ask for a new entry's name; it goes into the tree's selected folder,
    /// else the project root.
    fn prompt_new(&mut self, what: tree::NewEntry) {
        let dir = match self.tree() {
            Some(t) => t.target_dir(),
            None => self.project_root(),
        };
        let label = match what {
            tree::NewEntry::File => "New file: ",
            tree::NewEntry::Folder => "New folder: ",
        };
        self.open_prompt(prompt::PromptKind::New { what, dir }, label, "");
    }

    /// `:new PATH` / `:mkdir PATH`; without a path, ask like `a` / `A` do.
    fn new_entry_command(&mut self, args: &[&str], what: tree::NewEntry) {
        if args.is_empty() {
            return self.prompt_new(what);
        }
        let path = PathBuf::from(args.join(" "));
        let (dir, name) = if path.is_absolute() {
            match (path.parent(), path.file_name()) {
                (Some(d), Some(n)) => (d.to_path_buf(), n.to_string_lossy().into_owned()),
                _ => return,
            }
        } else {
            (self.project_root(), path.to_string_lossy().into_owned())
        };
        self.create_entry(&dir, &name, what);
    }

    /// Run a tree operation when there is a tree.
    fn tree_op(&mut self, op: fn(&mut tree::FileTree)) {
        if let Some(t) = self.tree_mut() {
            op(t);
        }
    }

    /// F8: show the tree, move focus into it, or back to the main area.
    fn toggle_tree(&mut self) {
        match self.sidebar_view() {
            None => self.set_tree_root(self.project_root(), true),
            Some(SidebarView::Hidden | SidebarView::Shown) => {
                self.set_sidebar_view(SidebarView::Focused)
            }
            Some(SidebarView::Focused) => self.set_sidebar_view(SidebarView::Shown),
        }
    }

    /// Ctrl-B: hide or show the sidebar (opening the project tree the first time).
    fn toggle_sidebar(&mut self) {
        match self.sidebar_view() {
            None => self.set_tree_root(self.project_root(), true),
            Some(SidebarView::Hidden) => self.set_sidebar_view(SidebarView::Shown),
            Some(SidebarView::Shown | SidebarView::Focused) => {
                self.set_sidebar_view(SidebarView::Hidden)
            }
        }
    }

    /// Project root: the nearest ancestor of the live file (or of the
    /// working directory) with a VCS or manifest marker.
    pub fn project_root(&self) -> PathBuf {
        let start = self
            .doc
            .canon
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        find_project_root(&start)
    }

    // ---- language servers -------------------------------------------------

    /// Start the LSP link if needed and tell it about the live document.
    fn lsp_attach_live(&mut self) {
        if !self.lsp_enabled
            || self.wake.is_none()
            || !self.doc.is_lsp_file()
            || self.doc.lsp.opened
        {
            return;
        }
        if self.lsp.is_none() {
            let wake = self.wake.clone();
            self.lsp = Some(lsp::LspLink::start(self.project_root(), move || {
                if let Some(w) = &wake {
                    w();
                }
            }));
        }
        let (Some(link), Some(path)) = (&self.lsp, &self.doc.canon) else {
            return;
        };
        link.open(path, self.doc.editor.buffer.text());
        let b = &self.doc.editor.buffer;
        self.doc.lsp = doc::LspSync {
            opened: true,
            rev: b.revision(),
            pending: false,
            was_dirty: b.is_dirty(),
        };
    }

    /// True while an edit has not been sent to the language server yet.
    pub fn lsp_pending(&self) -> bool {
        self.lsp.is_some() && self.doc.lsp.pending
    }

    /// Note edits to the live document (sent after the typing pause).
    fn track_changes(&mut self) {
        let b = &self.doc.editor.buffer;
        let s = &mut self.doc.lsp;
        if !s.opened {
            return;
        }
        if b.revision() != s.rev || (s.was_dirty && !b.is_dirty()) {
            s.rev = b.revision();
            s.pending = true;
        }
    }

    /// Send pending edits (and a save notification after a save), then
    /// drop queued server events (diagnostics are read from the cache).
    pub fn sync_lsp(&mut self) {
        let Some(link) = &self.lsp else { return };
        let d = &mut self.doc;
        if d.lsp.opened
            && let Some(path) = &d.canon
        {
            if d.lsp.pending {
                link.change(path, d.editor.buffer.text());
                d.lsp.pending = false;
            }
            let dirty = d.editor.buffer.is_dirty();
            if d.lsp.was_dirty && !dirty {
                link.save(path);
            }
            d.lsp.was_dirty = dirty;
        }
        link.drain();
    }

    // ---- settings & background work ---------------------------------------

    /// Apply `settings.json` values (tab size, icons, syntax) to all tabs.
    pub fn apply_settings(&mut self, s: skink_config::Settings) {
        self.icons = match s.icons.as_str() {
            "nerd" => icons::IconSet::Nerd,
            "ascii" => icons::IconSet::Ascii,
            _ => icons::IconSet::auto(),
        };
        for i in 0..self.tabs.len() {
            if let Some(d) = self.doc_at_mut(i) {
                d.editor.tab_width = s.tabsize;
                d.reset_syntax(s.syntax);
            }
        }
        self.settings = s;
    }

    /// (Re)load `settings.json`, `bindings.json` and the recent-files list.
    /// Returns problems found, for the status line.
    pub fn load_user_config(&mut self) -> Vec<String> {
        if let Some(sd) = &self.state_dir {
            self.recent = skink_config::recent_load(sd);
        }
        let Some(dir) = self.config_dir.clone() else {
            return Vec::new();
        };
        let (settings, mut errs) = skink_config::Settings::load(&dir);
        self.apply_settings(settings);
        // Start from the base keymap so removed overrides really go away.
        self.keymap = self.base_keymap.clone();
        self.user_bindings.clear();
        if let Ok(text) = std::fs::read_to_string(dir.join("bindings.json")) {
            match skink_config::parse_bindings(&text) {
                Ok(pairs) => {
                    let bad = self.keymap.apply_user(&pairs);
                    errs.extend(
                        bad.iter()
                            .map(|k| format!("bindings.json: unknown key {k}")),
                    );
                    self.user_bindings = pairs
                        .into_iter()
                        .filter(|(k, _)| !bad.contains(k))
                        .collect();
                }
                Err(e) => errs.push(e),
            }
        }
        errs
    }

    /// Run a shell command in the background; its output fills a view tab.
    fn run_to_view(&mut self, cmdline: &str, title: &str) {
        let id = self.open_view(title, &format!("$ {cmdline}\n\nrunning…\n"));
        let cwd = self.project_root();
        self.jobs.spawn(id, title, cmdline, cwd, self.wake.clone());
    }

    /// Put finished command output into its view tab (if still open).
    pub fn poll_jobs(&mut self) -> bool {
        let mut changed = false;
        while let Some(done) = self.jobs.try_recv() {
            changed = true;
            if let Some(i) = self.find_doc(|d| d.id == done.doc)
                && let Some(d) = self.doc_at_mut(i)
            {
                d.replace_view(&done.title, &done.text);
            }
        }
        changed
    }

    // ---- self-update -------------------------------------------------------

    /// Start the once-a-day background check when the policy allows it.
    /// Needs the event loop's wake callback; does nothing in tests.
    pub fn start_update_check(&mut self) {
        use skink_config::UpdatePolicy;
        if self.settings.updates == UpdatePolicy::Off
            || !matches!(self.update, update::UpdateState::Idle)
            || !update::check_due(self.state_dir.as_deref(), std::time::SystemTime::now())
        {
            return;
        }
        let Some(wake) = self.wake.clone() else {
            return;
        };
        if let Some(dir) = &self.state_dir {
            update::stamp(dir);
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(update::latest_version());
            wake();
        });
        self.update = update::UpdateState::Checking(rx);
    }

    /// `:upgrade`: download and install in the background.
    fn begin_upgrade(&mut self) {
        if matches!(self.update, update::UpdateState::Installing(_)) {
            self.editor_mut().message = Some("upgrade already running".into());
            return;
        }
        let wake = self.wake.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let r = update::upgrade(&update::Source::from_env(None), update::Mode::Install)
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
            if let Some(w) = wake {
                w();
            }
        });
        self.update = update::UpdateState::Installing(rx);
        self.editor_mut().message = Some("upgrading skink in the background...".into());
    }

    /// Advance the update state when its worker has answered.
    fn poll_update(&mut self) {
        use update::{Outcome, UpdateState as U};
        let state = std::mem::replace(&mut self.update, U::Idle);
        self.update = match state {
            U::Checking(rx) => match rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => U::Checking(rx),
                Ok(Some(v)) if v > update::Version::current() => {
                    if self.settings.updates == skink_config::UpdatePolicy::Auto {
                        self.begin_upgrade();
                        return;
                    }
                    self.editor_mut().message =
                        Some(format!("skink {v} is available \u{b7} :upgrade to install"));
                    U::Available(v)
                }
                Ok(_) | Err(mpsc::TryRecvError::Disconnected) => U::Idle,
            },
            U::Installing(rx) => match rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => U::Installing(rx),
                Ok(Ok(Outcome::Installed { to, .. })) => {
                    self.editor_mut().message = Some(format!(
                        "skink {to} installed \u{b7} restart skink to use it"
                    ));
                    U::Installed(to)
                }
                Ok(Ok(o)) => {
                    self.editor_mut().message = Some(o.to_string());
                    U::Idle
                }
                Ok(Err(e)) => {
                    self.editor_mut().message = Some(format!("upgrade: {e}"));
                    U::Failed(e)
                }
                Err(mpsc::TryRecvError::Disconnected) => U::Failed("upgrade worker died".into()),
            },
            s @ (U::Idle | U::Available(_) | U::Installed(_) | U::Failed(_)) => s,
        };
    }

    /// Everything that happens between input events: terminal output,
    /// finished jobs, language-server bookkeeping.
    fn poll_background(&mut self) {
        self.poll_update();
        self.poll_terminals();
        self.poll_jobs();
        if !self.doc.lsp.pending {
            self.sync_lsp();
        }
    }

    fn push_history(&mut self, line: &str) {
        if line.is_empty() || self.cmd_history.last().is_some_and(|l| l == line) {
            return;
        }
        if self.cmd_history.len() == CMD_HISTORY_MAX {
            self.cmd_history.remove(0);
        }
        self.cmd_history.push(line.to_owned());
    }
}

/// The repository root (nearest `.git`) when there is one: in a workspace
/// the nearest manifest is only a member crate, not the project. Without
/// git, the nearest manifest; without either, `start` itself.
fn find_project_root(start: &Path) -> PathBuf {
    const MANIFESTS: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "build.zig",
    ];
    let has = |dir: &Path, names: &[&str]| names.iter().any(|m| dir.join(m).exists());
    start
        .ancestors()
        .find(|d| has(d, &[".git"]))
        .or_else(|| start.ancestors().find(|d| has(d, MANIFESTS)))
        .unwrap_or(start)
        .to_path_buf()
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
    app.lsp_attach_live();
    app.start_update_check();

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
            // Block until something happens (zero idle CPU); while edits are
            // waiting for the language server, wake up after the typing pause.
            let first = if app.lsp_pending() {
                match rx.recv_timeout(LSP_DEBOUNCE) {
                    Ok(m) => Some(m),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        app.sync_lsp();
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => None,
                }
            } else {
                rx.recv().ok()
            };
            // Batch everything already queued so a burst costs one redraw.
            for msg in first
                .into_iter()
                .chain(std::iter::from_fn(|| rx.try_recv().ok()))
            {
                match msg {
                    Msg::Input(ev) => app.handle_event(ev),
                    Msg::Wake => {}
                    Msg::InputClosed => app.quit = true,
                }
            }
            app.poll_background();
        }
    })();
    let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests;
