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

mod agents;
mod ai;
mod aiview;
mod clip;
mod commands;
mod debug;
mod debugview;
mod disk;
mod doc;
mod docks;
mod events;
mod folding;
mod git;
mod gitui;
mod handoff;
mod help;
mod icons;
mod input;
mod intel;
mod launch;
pub mod lean;
mod lsp;
mod macros;
mod nav;
mod panes;
mod picker;
pub mod pkgcli;
mod pkgui;
mod ports;
mod preview;
mod projsearch;
mod prompt;
mod refactor;
mod refactors;
pub mod remote;
mod remote_session;
pub use remote_session::notify_main as remote_notify_main;
mod minitoml;
pub mod packages;
mod run;
mod session;
mod settingsui;
mod sidebar;
mod splits;
pub mod ssh;
mod tasks;
mod termtools;
mod termview;
mod text;
mod theme;
mod themeui;
mod trash;
mod tree;
mod undo_store;
pub mod update;
mod view;
mod zoom;

use clip::Clipboard;
use doc::{Doc, DocId};
use ratatui::{
    crossterm::{
        event::{
            self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture,
            EnableBracketedPaste, EnableFocusChange, EnableMouseCapture, Event,
            KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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

/// What sits on top of the main area and takes every key first.
enum Overlay {
    None,
    /// A status-line prompt (save as, find, open, command line, new entry).
    Prompt(prompt::Prompt),
    /// Quick open.
    Picker(Box<picker::Picker>),
}

/// The file tree beside the main area (absent until a folder is opened).
struct Sidebar {
    tree: tree::FileTree,
    view: SidebarView,
    width: sidebar::SidebarWidth,
    drag: sidebar::BorderDrag,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SidebarView {
    Hidden,
    /// On screen, keys go to the main area.
    Shown,
    /// On screen and receiving keys.
    Focused,
}

/// Why a terminal tab exists.
enum Purpose {
    /// A shell or program the user started.
    Shell,
    /// `sudo tee` writing a document the user could not save.
    SudoSave(disk::SudoSave),
    /// A task from `:task` (its output feeds problem matchers on exit).
    Task(Box<tasks::TaskRun>),
}

/// Whether a terminal tab's program still runs; once it exits, how.
enum Life {
    Running,
    Exited(Option<skink_term::ExitStatus>),
}

impl TermPane {
    fn exited(&self) -> bool {
        matches!(self.life, Life::Exited(_))
    }

    /// "exited", "exited 3" or "killed by SIGKILL".
    fn exit_note(&self) -> Option<String> {
        match &self.life {
            Life::Running => None,
            Life::Exited(Some(s)) if s.signal.is_some() => {
                Some(format!("killed by {}", s.signal.as_deref().unwrap_or("")))
            }
            Life::Exited(Some(s)) if s.code != 0 => Some(format!("exited {}", s.code)),
            Life::Exited(_) => Some("exited".into()),
        }
    }
}

struct TermPane {
    term: Term,
    screen: Screen,
    purpose: Purpose,
    life: Life,
    /// Program name, shown until the app inside sets a window title.
    label: String,
    /// For an interactive shell: the folder it started in (sessions reopen
    /// shells; other programs are not restarted).
    shell_cwd: Option<PathBuf>,
    /// The folder it started in (relative links in its output).
    cwd: PathBuf,
    /// Output arrived while this tab was in the background.
    activity: bool,
    /// Whether its shell sends prompt marks, and how.
    integration: skink_term::Integration,
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

    overlay: Overlay,
    /// Scroll position a new preview takes on its first render.
    preview_sync: preview::PreviewSync,
    sidebar: Option<Sidebar>,
    tab_hits: Vec<TabHit>,
    areas: Areas,
    /// Split panes over the main area (one pane when not split).
    panes: panes::Panes,
    /// Pane rectangles and dividers from the last frame (mouse).
    pane_layout: splits::PaneLayout,
    /// Which pane the editor is drawing right now (only the focused one
    /// shows a cursor).
    pass: splits::Pass,
    /// Project-wide search (streams into a results tab).
    search: projsearch::SearchJob,
    /// The open terminal history view (prompt jumps there).
    term_history: termtools::HistoryView,
    /// Keyboard macro recording / replay, and the last recorded keys.
    macro_state: macros::MacroState,
    last_macro: Vec<ratatui::crossterm::event::KeyEvent>,
    /// The task `:task last` reruns.
    last_task: Option<tasks::Task>,
    /// Problems the last finished task's output showed.
    task_problems: tasks::TaskProblems,
    /// Whether quitting saves a per-project session.
    session: session::SessionState,
    /// Bottom terminal and right agent panel (panes along the edges).
    docks: docks::Docks,
    /// Back / forward over jumps (Alt-U, Alt-Shift-U).
    jumps: nav::JumpList,
    /// Short notes in the corner that expire.
    toasts: Vec<nav::Toast>,
    /// Every toast's full text, newest last (`:messages`).
    messages: std::collections::VecDeque<String>,
    /// Lua scripts (see `script.rs`), their status-bar items and the
    /// debounced `change` event.
    script: script::Scripting,
    script_status: Vec<(String, String)>,
    script_change: script::ChangeWatch,

    clip: Clipboard,
    osc52: bool,
    wake: Option<WakeFn>,
    quit: bool,
    lsp: Option<lsp::LspLink>,
    lsp_enabled: bool,
    /// Code intelligence: popups, requests in flight, snippet tabstops.
    intel: intel::Intel,
    /// AI: the agent, the prompt box, the agent panel, the refactor preview.
    ai: ai::Ai,
    git: gitui::GitState,
    /// The debugger: breakpoints, the session, the panel.
    debug: debug::Debug,
    /// Automatic restarts after crashes (server, when), to avoid loops.
    lsp_restarts: Vec<(String, std::time::Instant)>,
    jobs: run::Jobs,

    theme: theme::Theme,
    /// The terminal's background, for `"colorscheme": "auto"`.
    appearance: theme::Appearance,
    theme_pick: themeui::ThemePick,
    icons: icons::IconSet,
    settings: skink_config::Settings,
    /// One effective `Settings` per `"[lang]"` section (precomputed so
    /// per-frame lookups stay cheap).
    lang_settings: Vec<(String, skink_config::Settings)>,
    cfg: settingsui::ConfigState,
    config_dir: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    /// Packages (`installed.json`, `packages/`, `tools/`).
    data_dir: Option<PathBuf>,
    recent: Vec<PathBuf>,
    cmd_history: Vec<String>,
    update: update::UpdateState,
    pkgs: pkgui::Packages,
    /// Last time open files were compared with the disk.
    disk_clock: std::time::Instant,
    view_mode: zoom::ViewMode,
    font_job: zoom::FontJob,
    /// Full editor, or one lean file for another tool (`lean.rs`).
    launch: lean::LaunchMode,
    /// Hand-off socket for `skink FILE` run inside a terminal tab.
    handoff: handoff::ServerState,
    /// Running under `skink ssh` (port detection and forwards).
    remote: remote_session::RemoteState,
    /// This process's exit status (`:cq` cancels).
    exit: Exit,
}

/// How the process ends: `:cq` exits 1 so git aborts the commit or rebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Done,
    Cancelled,
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
            overlay: Overlay::None,
            preview_sync: preview::PreviewSync::Done,
            sidebar: None,
            tab_hits: Vec::new(),
            areas: Areas::default(),
            panes: panes::Panes::new(0),
            pane_layout: splits::PaneLayout::default(),
            pass: splits::Pass::Focused,
            search: projsearch::SearchJob::Idle,
            last_task: None,
            macro_state: macros::MacroState::Idle,
            last_macro: Vec::new(),
            term_history: termtools::HistoryView::Closed,
            task_problems: tasks::TaskProblems::None,
            session: session::SessionState::Off,
            docks: docks::Docks::default(),
            jumps: nav::JumpList::default(),
            toasts: Vec::new(),
            messages: std::collections::VecDeque::new(),
            script: script::Scripting::Off,
            script_status: Vec::new(),
            script_change: script::ChangeWatch::Quiet { seen: 0 },
            clip: Clipboard::new(),
            osc52: false,
            wake: None,
            quit: false,
            lsp: None,
            lsp_enabled: false,
            intel: intel::Intel::default(),
            ai: ai::Ai::default(),
            git: gitui::GitState::new(),
            debug: debug::Debug::default(),
            lsp_restarts: Vec::new(),
            jobs: run::Jobs::default(),
            theme: theme::Theme::auto(),
            appearance: themeui::appearance_from_env().unwrap_or(theme::Appearance::Unknown),
            theme_pick: themeui::ThemePick::Closed,
            icons: icons::IconSet::auto(),
            settings,
            lang_settings: Vec::new(),
            cfg: settingsui::ConfigState::default(),
            config_dir: skink_config::config_dir(),
            state_dir: skink_config::state_dir(),
            // Tests set their own (never the real packages).
            data_dir: if cfg!(test) {
                None
            } else {
                skink_config::data_dir()
            },
            recent: Vec::new(),
            update: update::UpdateState::Idle,
            pkgs: pkgui::Packages::default(),
            cmd_history: Vec::new(),
            disk_clock: std::time::Instant::now(),
            view_mode: zoom::ViewMode::Normal,
            font_job: zoom::FontJob::Idle,
            launch: lean::LaunchMode::Project,
            handoff: handoff::ServerState::NotStarted,
            remote: remote_session::RemoteState::Local,
            exit: Exit::Done,
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
        self.start_remote();
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
        self.check_config_doc();
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
        self.focus() == Focus::Main && !self.active_is_term() && self.pass == splits::Pass::Focused
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
            self.intel.popup = intel::Popup::Closed;
            self.intel.snippet = intel::SnippetSession::Inactive;
            let Tab::Doc(Some(mut parked)) = std::mem::replace(&mut self.tabs[idx], Tab::Doc(None))
            else {
                unreachable!("checked above")
            };
            std::mem::swap(&mut self.doc, &mut parked);
            // Still on screen in another pane: keep its parse tree.
            if !self.panes.others_showing(self.cur_doc) {
                parked.park();
            }
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
        self.panes.show_in_focused(idx);
        self.focus_main();
        if self.is_doc(idx) {
            self.follow_active_file();
            self.prompt_disk_conflict();
        }
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
        let path = editor.buffer.path().map(Path::to_path_buf);
        doc::apply_edit_settings(&mut editor, self.settings_for(path.as_deref()));
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

    /// The file named on the command line is in the first tab (main built
    /// its editor directly): do what [`App::open_path`] does after a load.
    /// v1.0 finding: crash backups were written but never offered.
    pub fn launched_with_file(&mut self) {
        if self.doc.canon.is_none() {
            return;
        }
        self.restore_undo();
        self.note_opened();
        if !self.lean_process() {
            self.suggest_package();
        }
        self.offer_recovery();
        self.script_event(script::Event::Open);
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
                self.restore_undo();
                self.note_opened();
                self.suggest_package();
                self.offer_recovery();
                self.script_event(script::Event::Open);
            }
            Err(e) => self.doc.editor.message = Some(format!("{}: {e}", path.display())),
        }
    }

    /// Status note for a file that just opened in a limited mode.
    fn note_opened(&mut self) {
        let b = &self.doc.editor.buffer;
        let note = if let skink_core::Access::ReadOnly(why) = b.access() {
            Some(why.reason().to_owned())
        } else if self.doc.size == disk::SizeClass::Large {
            Some("Large file: syntax colours and language server are off".to_owned())
        } else if b
            .path()
            .is_some_and(|p| std::fs::OpenOptions::new().append(true).open(p).is_err())
        {
            Some("No write permission: saving will offer sudo".to_owned())
        } else {
            None
        };
        if note.is_some() {
            self.doc.editor.message = note;
        }
    }

    /// Open a path typed by the user: `file[:line[:col]]`, a folder (shown
    /// in the tree) or a new file (created on first save).
    fn open_location(&mut self, input: &str) {
        self.record_jump();
        let loc = skink_core::parse_location(input, Path::exists);
        if loc.path.is_dir() {
            self.set_tree_root(doc::canonical(&loc.path), true);
        } else if loc.path.exists() {
            self.open_path(&loc.path);
            if loc.line.is_some() {
                self.doc.editor.goto(loc.line, loc.col);
            }
        } else {
            // Say that it is new (a mistyped name or folder otherwise looks
            // like an empty file).
            let name = loc.path.display().to_string();
            let parent = loc
                .path
                .parent()
                .filter(|p| !p.as_os_str().is_empty() && !p.is_dir());
            let note = match parent {
                Some(p) => format!(
                    "{name} does not exist, and neither does the folder {}: \
                     create the folder first (a in the file tree)",
                    p.display()
                ),
                None => format!("New file {name}: Ctrl-S creates it"),
            };
            self.add_doc(Editor::new(Buffer::new_file(&loc.path)), true);
            self.doc.editor.message = Some(note);
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
        self.doc_at(idx)
            .map(|d| d.id)
            .expect("open_view selects or adds a document tab")
    }

    /// Close tab `idx` without asking. Closing the last document quits.
    pub fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        if self.is_remote_lean(idx) {
            // A child `skink` in a terminal tab waits for this tab: answer
            // it, and never quit the instance it (and git) runs in.
            return self.close_remote_lean(idx, Exit::Done);
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
            if let Some(Tab::Doc(Some(d))) = self.tabs.get(idx) {
                self.keep_undo(d);
                self.drop_backup(d);
            }
            if let (Some(link), Some(Tab::Doc(Some(d)))) = (&self.lsp, self.tabs.get(idx))
                && d.lsp.is_open()
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
        // Other panes that showed it show the current document instead.
        self.panes.tab_removed(idx, self.cur_doc);
        self.switch_to(next);
    }

    /// Close tab `idx`, asking first when it holds unsaved changes.
    fn request_close(&mut self, idx: usize) {
        if !self.doc_at(idx).is_some_and(|d| d.editor.buffer.is_dirty()) {
            return self.close_tab(idx);
        }
        self.switch_to(idx); // the question is about the document on screen
        self.open_choice(prompt::Choice::Close);
    }

    // ---- terminals -------------------------------------------------------

    /// Open a terminal in a new tab running `spec`, and switch to it.
    pub fn open_terminal(&mut self, spec: CommandSpec) -> io::Result<()> {
        let idx = self.spawn_terminal_tab(spec)?;
        self.switch_to(idx);
        Ok(())
    }

    /// Start a terminal tab without selecting it; returns its index.
    fn spawn_terminal_tab(&mut self, spec: CommandSpec) -> io::Result<usize> {
        let label = Path::new(&spec.program).file_name().map_or_else(
            || spec.program.clone(),
            |n| n.to_string_lossy().into_owned(),
        );
        let mut spec = match spec.cwd {
            Some(_) => spec,
            None => spec.cwd(self.project_root()),
        };
        // `skink FILE` from this terminal (git's editor, Ctrl-G in an AI
        // TUI) opens here as a tab instead of nesting (handoff.rs).
        spec.env.extend(self.handoff_env());
        // Tools that packages downloaded (language servers, formatters).
        if let Some(path) = self.package_path() {
            spec.env.push(("PATH".into(), path));
        }
        let default = CommandSpec::default_shell();
        let shell_cwd = (spec.program == default.program && spec.args.is_empty())
            .then(|| spec.cwd.clone())
            .flatten();
        let (spec, integration) = match (&shell_cwd, self.settings.terminal_shell_integration) {
            (Some(_), true) => skink_term::integrate(spec, &self.shell_integration_dir()),
            _ => (spec, skink_term::Integration::Off),
        };
        let rows = self.areas.main.height.max(8);
        let cols = self.areas.main.width.max(20);
        let cwd = spec.cwd.clone().unwrap_or_else(|| self.project_root());
        let term = Term::spawn_with_wake(spec, rows, cols, SCROLLBACK, self.wake.clone())?;
        let screen = term.screen();
        self.tabs.push(Tab::Term(Box::new(TermPane {
            term,
            screen,
            purpose: Purpose::Shell,
            life: Life::Running,
            label,
            shell_cwd,
            cwd,
            activity: false,
            integration,
        })));
        Ok(self.tabs.len() - 1)
    }

    /// Where the shell-integration init files live.
    fn shell_integration_dir(&self) -> PathBuf {
        match &self.state_dir {
            Some(sd) => sd.join("shell-integration"),
            None => std::env::temp_dir().join(format!(
                "skink-shell-integration-{}",
                std::env::var("USER")
                    .or_else(|_| std::env::var("USERNAME"))
                    .unwrap_or_default()
            )),
        }
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
        let mut finished = Vec::new();
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let Tab::Term(pane) = tab else { continue };
            let mut got = false;
            for ev in pane.term.events().try_iter() {
                got = true;
                if matches!(ev, TermEvent::Exited) && !pane.exited() {
                    pane.life = Life::Exited(pane.term.exit_status());
                    if let Purpose::Task(run) = &pane.purpose {
                        let out = pane.term.history().text();
                        finished.push((run.clone(), out, pane.term.exit_status()));
                    }
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
        for (run, out, status) in finished {
            self.task_finished(&run, &out, status);
        }
        if changed {
            self.remote_output();
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
        // Leaving another project: remember its open folders first.
        self.save_tree_state();
        let width = self
            .sidebar
            .as_ref()
            .map_or(sidebar::SidebarWidth::Auto, |s| s.width);
        self.sidebar = Some(Sidebar {
            tree: t,
            view,
            width,
            drag: sidebar::BorderDrag::Idle,
        });
        self.restore_tree_state();
        self.follow_active_file();
    }

    /// The one transition point for overlays.
    fn set_overlay(&mut self, o: Overlay) {
        self.overlay = o;
    }

    /// Close the prompt and hand it back (for finishing it).
    fn take_prompt(&mut self) -> Option<prompt::Prompt> {
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Prompt(p) => Some(p),
            other => {
                self.overlay = other;
                None
            }
        }
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

    /// Alt-H: show or hide dotfiles in the tree, from any focus. Without a
    /// tree yet, one is prepared (hidden) so the choice applies when it opens.
    fn toggle_hidden_files(&mut self) {
        if self.sidebar.is_none() {
            self.set_tree_root(self.project_root(), false);
        }
        let Some(t) = self.tree_mut() else { return };
        t.toggle_hidden();
        let shown = t.show_hidden;
        self.doc.editor.message = Some(
            if shown {
                "Hidden files shown"
            } else {
                "Hidden files hidden"
            }
            .into(),
        );
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
            || self.doc.lsp.is_open()
        {
            return;
        }
        if self.lsp.is_none() {
            let wake = self.wake.clone();
            let langs = self.lsp_languages();
            self.lsp = Some(lsp::LspLink::start(self.project_root(), langs, move || {
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
        self.doc.lsp = doc::LspSync::Open(doc::OpenSync {
            rev: b.revision(),
            pending: false,
            was_dirty: b.is_dirty(),
        });
    }

    /// True while an edit has not been sent to the language server yet.
    pub fn lsp_pending(&self) -> bool {
        self.lsp.is_some() && self.doc.lsp.is_pending()
    }

    /// Note edits to the live document (sent after the typing pause).
    fn track_changes(&mut self) {
        let b = &self.doc.editor.buffer;
        let doc::LspSync::Open(s) = &mut self.doc.lsp else {
            return;
        };
        if b.revision() != s.rev || (s.was_dirty && !b.is_dirty()) {
            s.rev = b.revision();
            s.pending = true;
        }
    }

    /// Send unsent edits now (before a question to the server).
    pub(crate) fn flush_lsp(&mut self) {
        let (Some(link), doc::LspSync::Open(s), Some(path)) =
            (&self.lsp, &mut self.doc.lsp, &self.doc.canon)
        else {
            return;
        };
        if s.pending {
            link.change(path, self.doc.editor.buffer.text());
            s.pending = false;
        }
    }

    /// Send pending edits (and a save notification after a save).
    pub fn sync_lsp(&mut self) {
        let Some(link) = &self.lsp else { return };
        let d = &mut self.doc;
        if let doc::LspSync::Open(s) = &mut d.lsp
            && let Some(path) = &d.canon
        {
            if s.pending {
                link.change(path, d.editor.buffer.text());
                s.pending = false;
            }
            let dirty = d.editor.buffer.is_dirty();
            if s.was_dirty && !dirty {
                link.save(path);
            }
            s.was_dirty = dirty;
        }
    }

    // ---- settings & background work ---------------------------------------

    /// Apply `settings.json` values (tab size, icons, syntax) to all tabs.
    /// Built-in language servers with the `"lsp.<lang>"` settings applied.
    pub(crate) fn lsp_languages(&self) -> Vec<skink_lsp::Language> {
        use skink_config::LspSetting;
        let overrides: Vec<(String, skink_lsp::Override)> = self
            .settings
            .lsp
            .iter()
            .map(|(name, l)| {
                let ov = match l {
                    LspSetting::Off => skink_lsp::Override::Off,
                    LspSetting::Server {
                        argv,
                        extensions,
                        root_markers,
                        init_options,
                        env,
                    } => skink_lsp::Override::Server {
                        argv: argv.clone(),
                        extensions: extensions.clone(),
                        root_markers: root_markers.clone(),
                        init_options: init_options.clone(),
                        env: env.clone(),
                    },
                };
                (name.clone(), ov)
            })
            .collect();
        // Packages' servers, then the user's settings over them.
        let with_packages =
            skink_lsp::merge_overrides(skink_lsp::default_languages(), &self.pkgs.active.lsp);
        let mut langs = skink_lsp::merge_overrides(with_packages, &overrides);
        // The JSON server checks Skink's own config files against its schemas.
        if let Some(json) = langs
            .iter_mut()
            .find(|l| l.id == "json" && l.source == skink_lsp::Source::BuiltIn)
        {
            for s in &mut json.servers {
                s.settings = self.json_server_settings();
                s.init_options = Some(serde_json::json!({"provideFormatter": true}));
            }
        }
        langs
    }

    pub fn apply_settings(&mut self, s: skink_config::Settings) {
        if self.lsp.is_some() && s.lsp != self.settings.lsp {
            self.doc.editor.message =
                Some("Language server settings apply the next time Skink starts".into());
        }
        self.icons = match s.icons.as_str() {
            "nerd" => icons::IconSet::Nerd,
            "ascii" => icons::IconSet::Ascii,
            _ => icons::IconSet::auto(),
        };
        self.lang_settings = s
            .sections
            .iter()
            .filter_map(|(n, _)| Some((n.clone(), s.for_language(&[n.as_str()])?)))
            .collect();
        self.settings = s;
        // One small file at most: cheap enough to redo on every reload.
        if let Some(e) = self.apply_colorscheme() {
            self.doc.editor.message = Some(e);
        }
        for i in 0..self.tabs.len() {
            let Some(path) = self
                .doc_at(i)
                .map(|d| d.editor.buffer.path().map(Path::to_path_buf))
            else {
                continue;
            };
            let eff = self.settings_for(path.as_deref()).clone();
            if let Some(d) = self.doc_at_mut(i) {
                doc::apply_edit_settings(&mut d.editor, &eff);
                d.reset_syntax(eff.syntax);
            }
        }
    }

    /// The settings for a file: its `"[lang]"` section (by language name or
    /// extension) over the user and project settings.
    pub(crate) fn settings_for(&self, path: Option<&Path>) -> &skink_config::Settings {
        let Some(p) = path else {
            return &self.settings;
        };
        if self.lang_settings.is_empty() {
            return &self.settings;
        }
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let lang = skink_syntax::language_for_path(p).map(skink_syntax::Lang::name);
        self.lang_settings
            .iter()
            .find(|(n, _)| Some(n.as_str()) == lang || *n == ext)
            .map_or(&self.settings, |(_, s)| s)
    }

    /// [`settings_for`](Self::settings_for) the live document.
    pub(crate) fn doc_settings(&self) -> &skink_config::Settings {
        self.settings_for(self.doc.editor.buffer.path())
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
        let project = self.settings_root();
        let file = skink_config::project_settings_path(&project);
        let (settings, mut errs, programs) =
            skink_config::Settings::load_with_project(&dir, Some(&project), &|text| {
                self.trusts(&file, text)
            });
        self.apply_settings(settings);
        if let skink_config::ProjectPrograms::Held(keys) = programs {
            self.ask_trust_settings(file, keys);
        }
        // Start from the base keymap so removed overrides really go away;
        // the preset goes under bindings.json.
        self.keymap = self.base_keymap.clone();
        self.keymap.apply_preset(self.settings.keymap);
        self.user_bindings.clear();
        if let Ok(text) = std::fs::read_to_string(dir.join("bindings.json")) {
            match skink_config::parse_bindings(&text) {
                Ok(mut pairs) => {
                    // A typo'd action would take the key away (Ctrl-Q to
                    // "Quitt" left no way out): such keys keep their default.
                    pairs.retain(|(k, chain)| {
                        let unknown = (!chain.starts_with("command-edit:"))
                            .then(|| {
                                chain
                                    .split(['|', ','])
                                    .map(str::trim)
                                    .find(|n| !self.action_exists(n))
                            })
                            .flatten();
                        if let Some(n) = unknown {
                            errs.push(format!(
                                "bindings.json: {k}: unknown action \"{n}\" (default kept)"
                            ));
                        }
                        unknown.is_none()
                    });
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
            || self.lean_process()
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
        self.expire_toasts();
        self.poll_update();
        self.poll_terminals();
        self.finish_sudo_saves();
        self.poll_font();
        self.poll_jobs();
        if let Overlay::Picker(p) = &mut self.overlay
            && p.poll_walk()
        {
            p.refresh_preview();
        }
        if !self.doc.lsp.is_pending() {
            self.sync_lsp();
        }
        self.poll_intel();
        self.poll_search();
        self.poll_packages();
        self.update_bulb();
        self.resolve_selected_completion();
        self.poll_ai();
        self.poll_git();
        self.poll_debug();
        self.poll_handoff();
        self.remote_tick();
        self.stop_idle_servers();
    }

    /// Language servers with no open files for 10 minutes are stopped (they
    /// restart when a file needs them). Checked whenever the loop wakes.
    fn stop_idle_servers(&mut self) {
        const IDLE: std::time::Duration = std::time::Duration::from_secs(10 * 60);
        let Some(link) = &self.lsp else {
            return;
        };
        let stopped = link.stop_idle(IDLE);
        if !stopped.is_empty() {
            self.toast(
                nav::Level::Info,
                format!(
                    "Stopped idle language server {} (no open files)",
                    stopped.join(", ")
                ),
            );
        }
    }

    /// Normal exit: drop crash backups, remember the tree's open folders.
    pub fn shutdown(&mut self) {
        self.keep_undo(&self.doc);
        for t in &self.tabs {
            if let Tab::Doc(Some(d)) = t {
                self.keep_undo(d);
            }
        }
        self.finish_all_lean();
        self.discard_backups();
        self.save_tree_state();
        self.save_session();
    }

    /// Persistent undo: keep a closing document's steps (not in lean mode,
    /// where Skink edits a file for another tool and must leave no trace).
    fn keep_undo(&self, d: &Doc) {
        if let (Some(state), Some(canon), false) = (&self.state_dir, &d.canon, self.lean_process())
        {
            undo_store::save(state, canon, &d.editor);
        }
    }

    /// Persistent undo: bring back the live document's saved steps.
    fn restore_undo(&mut self) {
        if self.lean_process() {
            return;
        }
        let (Some(state), Some(canon)) = (self.state_dir.clone(), self.doc.canon.clone()) else {
            return;
        };
        if let undo_store::UndoLoad::Restored { steps } =
            undo_store::load(&state, &canon, &mut self.doc.editor)
        {
            self.doc.editor.message = Some(format!(
                "Undo history restored ({steps} step{})",
                if steps == 1 { "" } else { "s" }
            ));
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
    project_root_of(start).unwrap_or_else(|| start.to_path_buf())
}

/// The project `start` belongs to: the nearest `.git` above it, else the
/// nearest manifest; `None` for a lone file (lean mode opens those).
pub(crate) fn project_root_of(start: &Path) -> Option<PathBuf> {
    const MANIFESTS: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "build.zig",
        "Makefile",
        "CMakeLists.txt",
        "meson.build",
        "pubspec.yaml",
        "deno.json",
        "setup.py",
        "Gemfile",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "composer.json",
        // Not bare `.vscode`: ~/.vscode holds VS Code's extensions.
        ".vscode/tasks.json",
        ".vscode/launch.json",
    ];
    let has = |dir: &Path, names: &[&str]| names.iter().any(|m| dir.join(m).exists());
    start
        .ancestors()
        .find(|d| has(d, &[".git"]))
        .or_else(|| start.ancestors().find(|d| has(d, MANIFESTS)))
        .map(Path::to_path_buf)
}

/// Whether the terminal speaks the kitty keyboard protocol. The query can
/// wait up to 2 s on a terminal that never answers, so it is only sent when
/// the environment names one known to support it. Off with
/// `SKINK_NO_KITTY_KEYS=1` (escape hatch for odd setups).
fn enhanced_keys_supported() -> bool {
    let var = |k: &str| std::env::var(k).unwrap_or_default().to_lowercase();
    let (term, program) = (var("TERM"), var("TERM_PROGRAM"));
    let known = ["ghostty", "kitty", "wezterm", "foot", "alacritty", "rio"]
        .iter()
        .any(|t| term.contains(t) || program.contains(t))
        || program == "iterm.app"
        || std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
        || std::env::var_os("WEZTERM_EXECUTABLE").is_some();
    known
        && std::env::var_os("SKINK_NO_KITTY_KEYS").is_none()
        && ratatui::crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
}

enum Msg {
    Input(Event),
    Appearance(theme::Appearance),
    Wake,
    InputClosed,
    /// SIGTERM / SIGHUP / SIGINT from outside.
    #[cfg(unix)]
    Signal(i32),
}

/// Run the TUI. With `exit_after_first_frame` it draws once and returns
/// (used by the startup/RSS measurement script).
pub fn run(mut app: App, exit_after_first_frame: bool) -> io::Result<Exit> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let wake_tx = tx.clone();
    app.set_wake(Arc::new(move || {
        let _ = wake_tx.send(Msg::Wake);
    }));
    // Highlight queries take 8-15 ms to compile: do it on a worker so the
    // first frame is instant (drawn uncoloured, recoloured on the wake).
    // Started before the terminal set-up so it usually wins the race.
    let query_tx = tx.clone();
    skink_syntax::compile_queries_in_background(move || {
        let _ = query_tx.send(Msg::Wake);
    });
    if let Some(syn) = &app.doc.syntax {
        skink_syntax::warm_up(syn.hl.lang());
    }
    app.osc52 = true;
    app.load_packages();
    app.lsp_attach_live();
    app.start_update_check();
    app.load_scripts(skink_config::config_dir());

    let mut terminal = ratatui::init();
    let _ = execute!(
        io::stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        EnableFocusChange
    );
    // Kitty keyboard protocol (Ghostty, Kitty, WezTerm, foot, ...): keys
    // arrive unambiguously (Esc vs Alt, Ctrl-I vs Tab, Ctrl-]) and a
    // terminal with Option-as-Alt reports Alt. Asked before the input thread
    // starts, since the query reads the terminal's answer.
    let kitty = enhanced_keys_supported();
    if kitty {
        let _ = execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
            prev(info);
        }));
    }
    // "colorscheme": "auto": ask the terminal for its background on the
    // input thread, before it reads keys; the first frame does not wait.
    let ask_background = !exit_after_first_frame
        && app.wants_appearance()
        && matches!(app.appearance, theme::Appearance::Unknown)
        && themeui::answers_background_query();
    // v1.0 finding: a kill within the backup delay lost the last edits.
    #[cfg(unix)]
    if !exit_after_first_frame {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        if let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGTERM, SIGHUP, SIGINT]) {
            let sig_tx = tx.clone();
            std::thread::spawn(move || {
                if let Some(s) = signals.forever().next() {
                    let _ = sig_tx.send(Msg::Signal(s));
                }
            });
        }
    }
    if !exit_after_first_frame {
        std::thread::spawn(move || {
            if ask_background && let Some(a) = themeui::query_background(Duration::from_millis(250))
            {
                let _ = tx.send(Msg::Appearance(a));
            }
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
            // Block until something happens (zero idle CPU); wake earlier only
            // for edits waiting for the language server or a crash backup.
            let first = match app.next_timeout() {
                Some(t) => match rx.recv_timeout(t) {
                    Ok(m) => Some(m),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        app.on_timeout();
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => None,
                },
                None => rx.recv().ok(),
            };
            // Batch everything already queued so a burst costs one redraw.
            for msg in first
                .into_iter()
                .chain(std::iter::from_fn(|| rx.try_recv().ok()))
            {
                match msg {
                    Msg::Input(ev) => app.handle_event(ev),
                    Msg::Wake => {}
                    Msg::Appearance(a) => app.set_appearance(a),
                    Msg::InputClosed => app.quit = true,
                    // Keep unsaved edits as crash backups (offered on the
                    // next open); an error skips `shutdown`, which would
                    // discard them.
                    #[cfg(unix)]
                    Msg::Signal(s) => {
                        app.write_backups();
                        return Err(io::Error::other(format!(
                            "stopped by signal {s}; unsaved edits are kept for recovery"
                        )));
                    }
                }
            }
            app.poll_background();
        }
    })();
    if kitty {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        io::stdout(),
        DisableFocusChange,
        DisableBracketedPaste,
        DisableMouseCapture
    );
    ratatui::restore();
    if result.is_ok() {
        app.shutdown();
    }
    result.map(|()| app.exit_status())
}

#[cfg(test)]
mod ai_tests;
#[cfg(test)]
mod bench_tests;
#[cfg(test)]
mod git_tests;
#[cfg(test)]
mod lean_tests;
#[cfg(test)]
mod pkgui_tests;
mod script;
#[cfg(test)]
mod script_tests;
#[cfg(test)]
mod tests;
