//! Input routing: keys, mouse and paste go to the topmost thing that wants
//! them — quick-open picker, then a prompt, then global tab keys, then the
//! focused area (file tree, terminal tab or document).

use crate::{App, Focus, Overlay, SidebarView, TabHit, commands, input, picker};
use ratatui::{
    crossterm::event::{
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    layout::Position,
};
use skink_config::{Key, KeySpec};
use skink_core::Request;
use skink_term::MouseKind;

const WHEEL_LINES: usize = 3;

fn plain(key: Key) -> KeySpec {
    KeySpec {
        ctrl: false,
        alt: false,
        shift: false,
        key,
    }
}

/// Keys that reach the app even while a terminal tab has focus (when bound
/// to an app action). Everything else goes to the program in the terminal,
/// so shells and TUIs keep Ctrl-T, Alt-., F10 and friends.
fn is_global_key(k: &KeySpec) -> bool {
    match k.key {
        Key::F(_) => true,
        Key::PageUp | Key::PageDown => k.ctrl && !k.alt,
        Key::Char('1'..='9') => k.alt && !k.ctrl,
        _ => false,
    }
}

/// Shown when a letter the tree does not use is pressed in it.
const TREE_KEYS: &str = "Files: Enter open · a new file · A new folder · r rename · d trash · \
                         . dotfiles · / filter · : command · Esc back to the editor";

impl App {
    pub fn handle_event(&mut self, ev: Event) {
        enum Kind {
            Input,
            Other,
        }
        let ev_kind = match ev {
            Event::Key(_) | Event::Mouse(_) | Event::Paste(_) => Kind::Input,
            _ => Kind::Other,
        };
        match ev {
            Event::Key(k) => self.handle_key(k),
            Event::Mouse(m) => self.handle_mouse(m),
            Event::Paste(s) => self.handle_paste(&s),
            Event::FocusGained => {
                self.check_disk(true);
                self.git_refresh_all();
            }
            _ => {}
        }
        if matches!(ev_kind, Kind::Input) {
            self.check_disk(false);
        }
        self.track_changes();
        if matches!(ev_kind, Kind::Input) {
            self.watch_settings_save();
            self.watch_project_settings_save();
            self.check_config_doc();
        }
    }

    pub fn handle_paste(&mut self, s: &str) {
        match self.overlay {
            Overlay::Settings(_) => return self.settings_screen_paste(s),
            Overlay::Keys(_) => return self.key_editor_paste(s),
            Overlay::Menu(_) => return,
            _ => {}
        }
        if self.ai_paste(s) {
            return;
        }
        if let Overlay::Picker(p) = &mut self.overlay {
            s.chars()
                .filter(|c| !c.is_control())
                .for_each(|c| p.type_char(c));
        } else if let Overlay::Prompt(p) = &mut self.overlay {
            p.insert(&s.replace(['\n', '\r'], " "));
        } else if self.focus() == Focus::Tree {
            // Nothing to paste into.
        } else if let Some(pane) = self.active_term() {
            let _ = pane.term.paste(s);
        } else {
            self.doc.editor.insert_text(s);
        }
    }

    pub fn handle_key(&mut self, ev: KeyEvent) {
        // Windows reports key releases too; only act on presses/repeats.
        if ev.kind == KeyEventKind::Release {
            return;
        }
        if self.esc_tail.drop_key(&ev, std::time::Instant::now()) {
            return;
        }
        let ev = self.mac_option_event(ev);
        self.record_key(ev);
        if matches!(self.overlay, Overlay::Picker(_)) {
            return self.picker_key(ev);
        }
        if matches!(self.overlay, Overlay::Prompt(_)) {
            return self.prompt_key(ev);
        }
        match self.overlay {
            Overlay::Settings(_) => return self.settings_screen_key(ev),
            Overlay::Keys(_) => return self.key_editor_key(ev),
            Overlay::Menu(_) => return self.menu_key_event(ev),
            _ => {}
        }
        let Some(spec) = input::key_spec(ev) else {
            return;
        };
        if self.ai_key(&spec) {
            return;
        }

        if is_global_key(&spec) {
            if let Key::Char(c @ '1'..='9') = spec.key {
                return self.switch_to(c as usize - '1' as usize);
            }
            let chain = self.keymap.command(&spec).map(str::to_owned);
            // F10 in a terminal tab is the program's (htop, mc quit with it),
            // not the menu.
            let in_term = self.active_is_term() && self.focus() == Focus::Main;
            let chain = chain.map(|c| match in_term {
                true => c
                    .split('|')
                    .filter(|n| n.trim() != "Menu")
                    .collect::<Vec<_>>()
                    .join("|"),
                false => c,
            });
            if let Some(chain) = chain.map(|c| self.without_idle_debug_steps(&c))
                && commands::APP_ACTIONS
                    .iter()
                    .any(|a| chain.split(['|', ',']).any(|n| n.trim() == a.name))
            {
                return self.run_chain(&chain);
            }
        }
        if self.focus() == Focus::Tree {
            // The tree owns plain navigation keys; chords (Ctrl/Alt) and
            // F-keys still reach the keymap so Ctrl-E, Ctrl-Q, Alt-P work.
            let chord = spec.ctrl || spec.alt || matches!(spec.key, Key::F(_));
            if self.tree_key(ev) || !chord {
                return;
            }
            if let Some(chain) = self.keymap.command(&spec).map(str::to_owned) {
                self.run_chain(&chain);
            }
            return;
        }
        if let Some(pane) = self.active_term() {
            if pane.exited() {
                // Enter / r start it again (an AI CLI resumes), Shift-R from
                // scratch; any other key dismisses it.
                let idx = self.active;
                let plain = !spec.ctrl && !spec.alt;
                return match spec.key {
                    Key::Enter | Key::Char('r') if plain && self.restart_hint().is_some() => {
                        self.restart_terminal(idx, crate::termrestart::Restart::Again)
                    }
                    Key::Char('R') if plain && self.restart_hint().is_some() => {
                        self.restart_terminal(idx, crate::termrestart::Restart::Fresh)
                    }
                    _ => self.close_terminal(),
                };
            }
            let _ = pane.term.send_key(spec);
            return;
        }
        if self.git_peek_key(&spec) {
            return;
        }
        if self.popup_key(&spec) {
            return;
        }
        if self.preview_key(&spec) {
            return;
        }
        if !spec.ctrl && !spec.alt && (self.git_view_key(&spec) || self.debug_view_key(&spec)) {
            return;
        }
        if spec.key == Key::Tab && !spec.shift && self.next_snippet_stop() {
            return;
        }
        if spec.key == Key::Escape {
            self.intel.snippet = crate::intel::SnippetSession::Inactive;
        }
        if spec.key == Key::Enter && spec == plain(Key::Enter) && self.follow_link() {
            return;
        }
        match self.keymap.command(&spec).map(str::to_owned) {
            Some(chain) => {
                self.run_chain(&chain);
                self.after_edit_key();
            }
            None => match spec.key {
                Key::Char(c) if !spec.ctrl && !spec.alt => {
                    self.doc.editor.insert_char(c);
                    self.after_typed(c);
                }
                // Unbound chords and keys go to scripts' `key` handlers.
                _ => {
                    self.script_key(&spec.to_string());
                }
            },
        }
        self.script_edited();
    }

    /// macOS terminals deliver Option+Z as "Ω": read such characters as the
    /// Alt chord they stand for when Skink has a use for it (see
    /// `input::translate_option` and the "macoptionisalt" setting).
    fn mac_option_event(&self, ev: KeyEvent) -> KeyEvent {
        if self.focus() == Focus::Main
            && self.active_is_term()
            && matches!(self.overlay, Overlay::None)
        {
            return ev; // the program in the terminal gets what was typed
        }
        let Some(spec) = input::key_spec(ev) else {
            return ev;
        };
        let find_open = matches!(&self.overlay, Overlay::Prompt(p) if matches!(p.kind, crate::prompt::PromptKind::Find { .. }));
        let bound = |k: &KeySpec| {
            self.keymap.command(k).is_some()
                || (find_open && matches!(k.key, Key::Char('r' | 'c' | 'w' | 's')) && !k.shift)
        };
        let t = input::translate_option(
            spec,
            self.settings.mac_option,
            cfg!(target_os = "macos"),
            bound,
        );
        match t.key {
            Key::Char(c) if t != spec => {
                let mut mods = KeyModifiers::ALT;
                if t.shift {
                    mods |= KeyModifiers::SHIFT;
                }
                KeyEvent::new(
                    KeyCode::Char(if t.shift { c.to_ascii_uppercase() } else { c }),
                    mods,
                )
            }
            _ => ev,
        }
    }

    /// A chain like `DebugStepOver|Quit` without its debug step while no
    /// session is live, so F10 / F11 still reach a terminal's program
    /// (htop, mc) when the rest is not an app action.
    fn without_idle_debug_steps(&mut self, chain: &str) -> String {
        const STEPS: [&str; 4] = [
            "DebugStepOver",
            "DebugStepInto",
            "DebugStepOut",
            "DebugPause",
        ];
        let live = self.debug.live().is_some();
        if live {
            return chain.to_owned();
        }
        chain
            .split('|')
            .map(str::trim)
            .filter(|n| !STEPS.contains(n))
            .collect::<Vec<_>>()
            .join("|")
    }

    /// Run a Micro action chain (`A|B` first that succeeds, `A,B` all),
    /// where each name is an app action or an editor action, then act on
    /// whatever the editor asked for.
    pub fn run_chain(&mut self, chain: &str) {
        if let Some(rest) = chain.strip_prefix("command-edit:") {
            return self.open_prompt(crate::prompt::PromptKind::Command, ":", rest);
        }
        if chain.contains("Paste")
            && let Some(t) = self.clip.get()
        {
            self.doc.editor.clipboard = t;
        }
        for part in chain.split(',') {
            for name in part.split('|').map(str::trim) {
                let ok = match name {
                    // Actions that may decline, so `A|B` falls through.
                    "Autocomplete" => self.accept_completion(),
                    // Debug keys share F10 / F11 / F6 with other actions.
                    "DebugStepOver" => self.debug_step(crate::debug::Step::Over),
                    "DebugStepInto" => self.debug_step(crate::debug::Step::Into),
                    "DebugStepOut" => self.debug_step(crate::debug::Step::Out),
                    "DebugPause" => self.debug_pause(),
                    "Save" if self.format_before_save() => true,
                    _ => false,
                } || commands::run_app_action(self, name)
                    || self.registry.run(name, &mut self.doc.editor);
                if ok {
                    break;
                }
            }
        }
        let saved = chain.split([',', '|']).any(|n| n.trim() == "Save");
        if saved && self.doc.editor.request.is_none() {
            self.git_after_save();
            if !self.doc.editor.buffer.is_dirty() {
                self.script_event(crate::script::Event::Save);
            }
        }
        if self.doc.editor.clipboard_changed {
            self.doc.editor.clipboard_changed = false;
            self.clip.set(&self.doc.editor.clipboard, self.osc52);
        }
        if let Some(r) = self.doc.editor.request.take() {
            self.handle_request(r);
        }
    }

    /// Act on what the editor asked the app for.
    pub(crate) fn handle_request(&mut self, r: Request) {
        use crate::prompt::{Choice, PromptKind};
        match r {
            Request::Quit => self.request_close(self.cur_doc),
            Request::SaveAs => {
                self.open_prompt(PromptKind::SaveAs { close_after: false }, "Save as: ", "")
            }
            Request::Find => self.open_find(),
            Request::ConfirmOverwrite => self.open_choice(Choice::Overwrite),
            Request::SaveDenied => self.open_choice(Choice::SudoSave),
        }
    }

    // ---- file tree ---------------------------------------------------------

    /// Returns whether the tree consumed the key. Plain letters are the
    /// tree's (see [`TREE_KEYS`]); `:` opens the command line.
    fn tree_key(&mut self, ev: KeyEvent) -> bool {
        let page = (self.areas.tree.height.saturating_sub(2) as isize - 1).max(1);
        let Some(t) = self.sidebar.as_mut().map(|s| &mut s.tree) else {
            return false;
        };
        if ev
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        let mut open = None;
        match ev.code {
            KeyCode::Up => t.move_selection(-1),
            KeyCode::Down => t.move_selection(1),
            KeyCode::PageUp => t.move_selection(-page),
            KeyCode::PageDown => t.move_selection(page),
            KeyCode::Home => t.select(0),
            KeyCode::End => t.select(usize::MAX),
            KeyCode::Enter | KeyCode::Right => open = t.activate(),
            KeyCode::Left => t.collapse_or_parent(),
            KeyCode::Char('.') => t.toggle_hidden(),
            KeyCode::Char('-') => t.collapse_all(),
            KeyCode::Char('*') => t.expand_recursive(),
            KeyCode::Char('a') => {
                self.prompt_new(crate::tree::NewEntry::File);
                return true;
            }
            KeyCode::Char('A') => {
                self.prompt_new(crate::tree::NewEntry::Folder);
                return true;
            }
            KeyCode::Esc if matches!(t.mode, crate::tree::TreeMode::Filter { .. }) => {
                t.set_filter(None)
            }
            KeyCode::Char('q') | KeyCode::Esc => self.focus_main(),
            KeyCode::Char('h') => self.set_sidebar_view(SidebarView::Hidden),
            KeyCode::Char('r') | KeyCode::F(2) => self.prompt_rename(),
            KeyCode::Char('d') | KeyCode::Delete => self.prompt_trash(),
            KeyCode::Char('y') => self.copy_tree_path(false),
            KeyCode::Char('Y') => self.copy_tree_path(true),
            KeyCode::Char('/') => self.prompt_tree_filter(),
            KeyCode::Char('<') => self.resize_sidebar(-2),
            KeyCode::Char('>') => self.resize_sidebar(2),
            KeyCode::Char(':') => {
                self.open_prompt(crate::prompt::PromptKind::Command, ":", "");
                return true;
            }
            // Any other letter: say what the tree's keys are rather than
            // swallowing it silently.
            KeyCode::Char(_) => {
                self.doc.editor.message = Some(TREE_KEYS.into());
                return true;
            }
            _ => return false,
        }
        if let Some(p) = open {
            self.open_path(&p);
        }
        true
    }

    fn tree_mouse(&mut self, m: MouseEvent) {
        // Any click or scroll in the tree (rows, header, empty space) gives
        // it the keys, so `.`, arrows and Enter go where the user looks.
        if matches!(
            m.kind,
            MouseEventKind::Down(_)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
        ) {
            self.set_sidebar_view(SidebarView::Focused);
        }
        let rows = self.areas.tree.height.saturating_sub(2) as usize;
        let top = self.areas.tree.y + 2; // below the "Files" and root rows
        let Some(t) = self.tree_mut() else {
            return;
        };
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        let clicked = match m.kind {
            MouseEventKind::ScrollLeft => {
                t.hscroll = t.hscroll.saturating_sub(crate::doc::HWHEEL_COLS);
                None
            }
            MouseEventKind::ScrollUp if shift => {
                t.hscroll = t.hscroll.saturating_sub(crate::doc::HWHEEL_COLS);
                None
            }
            MouseEventKind::ScrollRight => {
                t.hscroll += crate::doc::HWHEEL_COLS;
                None
            }
            MouseEventKind::ScrollDown if shift => {
                t.hscroll += crate::doc::HWHEEL_COLS;
                None
            }
            MouseEventKind::ScrollUp => {
                t.scroll = t.scroll.saturating_sub(WHEEL_LINES);
                None
            }
            MouseEventKind::ScrollDown => {
                t.scroll = (t.scroll + WHEEL_LINES).min(t.nodes.len().saturating_sub(rows.max(1)));
                None
            }
            MouseEventKind::Down(MouseButton::Left) if m.row >= top => {
                let idx = t.scroll + (m.row - top) as usize;
                (idx < t.nodes.len()).then(|| {
                    t.select(idx);
                    t.activate()
                })
            }
            _ => None,
        };
        if let Some(Some(p)) = clicked {
            self.open_path(&p);
        }
    }

    // ---- quick open ----------------------------------------------------------

    pub(crate) fn open_picker(&mut self) {
        let root = self.project_root();
        let tab_items = (0..self.tabs.len())
            .filter_map(|i| {
                let d = self.doc_at(i)?;
                let detail = d.canon.as_ref().map_or_else(String::new, |p| {
                    p.strip_prefix(&root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .into_owned()
                });
                Some(picker::Item {
                    label: d.title(),
                    detail,
                    target: d
                        .canon
                        .clone()
                        .map_or(picker::Target::Tab(i), picker::Target::File),
                    tag: "open",
                })
            })
            .collect();
        // With an event loop, project files stream in from a worker; without
        // one (tests, headless) the walk runs inline.
        let (project, stream) = match &self.wake {
            Some(w) => (
                Vec::new(),
                Some(picker::stream_project_files(self.roots(), Some(w.clone()))),
            ),
            None => (
                self.roots()
                    .iter()
                    .flat_map(|r| picker::project_files(r, usize::MAX))
                    .collect(),
                None,
            ),
        };
        let mut p = picker::Picker::new(tab_items, &self.recent, project, &root);
        if let Some(rx) = stream {
            p.attach_walk(rx);
        }
        p.refresh_preview();
        self.set_overlay(Overlay::Picker(Box::new(p)));
    }

    fn picker_key(&mut self, ev: KeyEvent) {
        self.picker_key_inner(ev);
        if let Overlay::Picker(p) = &mut self.overlay {
            p.refresh_preview();
        }
        self.theme_picker_step(ev.code == KeyCode::Esc);
    }

    fn picker_key_inner(&mut self, ev: KeyEvent) {
        let Overlay::Picker(p) = &mut self.overlay else {
            return;
        };
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        match ev.code {
            KeyCode::Esc => self.set_overlay(Overlay::None),
            KeyCode::Up => p.move_selection(-1),
            KeyCode::Down => p.move_selection(1),
            KeyCode::Char('p') if ctrl => p.move_selection(-1),
            KeyCode::Char('n') if ctrl => p.move_selection(1),
            KeyCode::Backspace => {
                p.backspace();
                self.picker_input_changed();
            }
            KeyCode::Char('u') if ctrl => {
                p.clear();
                self.picker_input_changed();
            }
            KeyCode::Char(c) if !ctrl => {
                p.type_char(c);
                self.picker_input_changed();
            }
            // F1 in the command palette: the help about that command.
            KeyCode::F(1) if matches!(p.mode, picker::Mode::Commands) => {
                if let Some(picker::Target::Command(c)) = p.current() {
                    self.set_overlay(Overlay::None);
                    let q = commands::command_for(&c).unwrap_or(c);
                    self.open_help(Some(&q));
                }
            }
            KeyCode::Enter => {
                let target = p.current();
                self.set_overlay(Overlay::None);
                match target {
                    Some(picker::Target::Tab(i)) => self.switch_to(i),
                    Some(picker::Target::File(f)) => self.open_path(&f),
                    Some(picker::Target::Location { path, line, col }) => {
                        self.jump(&path, line, col)
                    }
                    Some(picker::Target::Action(a)) => self.run_code_action(*a),
                    Some(picker::Target::Command(c)) => self.run_command(&c),
                    Some(picker::Target::Script { index, label }) => {
                        self.script_picked(index, label)
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    // ---- mouse ---------------------------------------------------------------

    pub fn handle_mouse(&mut self, m: MouseEvent) {
        match self.overlay {
            Overlay::Picker(_) => return, // modal
            Overlay::Settings(_) => return self.settings_screen_mouse(m),
            Overlay::Keys(_) => return self.key_editor_mouse(m),
            _ => {}
        }
        if self.menu_mouse(m) {
            return;
        }
        let pos = Position::new(m.column, m.row);
        if self.border_drag(m) {
            return;
        }
        if self.areas.top.contains(pos) {
            if matches!(m.kind, MouseEventKind::Down(_)) {
                self.focus_main();
            }
            return self.tab_bar_mouse(m);
        }
        if self.areas.tree.contains(pos) {
            return self.tree_mouse(m);
        }
        if self.pane_mouse(m) {
            return;
        }
        if !self.areas.main.contains(pos) {
            return;
        }
        self.focus_main();
        let (col, row) = (m.column - self.areas.main.x, m.row - self.areas.main.y);
        if self.active_is_term() {
            if m.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                && self.terminal_click_link(col, row)
            {
                return;
            }
            self.terminal_mouse(m.kind, col, row);
        } else {
            self.editor_mouse(m, col, row);
        }
    }

    /// Drag the sidebar's right border to resize it. True when consumed.
    fn border_drag(&mut self, m: MouseEvent) -> bool {
        use crate::sidebar::BorderDrag;
        let tree = self.areas.tree;
        let Some(s) = &mut self.sidebar else {
            return false;
        };
        match (s.drag, m.kind) {
            (BorderDrag::Idle, MouseEventKind::Down(MouseButton::Left))
                if tree.width > 0
                    && m.column + 1 == tree.right()
                    && (tree.y..tree.bottom()).contains(&m.row) =>
            {
                s.drag = BorderDrag::Resizing;
                true
            }
            (BorderDrag::Resizing, MouseEventKind::Drag(MouseButton::Left)) => {
                let cols = (m.column + 1).saturating_sub(tree.x);
                self.set_sidebar_columns(cols);
                true
            }
            (BorderDrag::Resizing, _) => {
                s.drag = BorderDrag::Idle;
                true
            }
            (BorderDrag::Idle, _) => false,
        }
    }

    /// Click a tab to select it, its close button (or middle-click) to close.
    fn tab_bar_mouse(&mut self, m: MouseEvent) {
        let hit = self
            .tab_hits
            .iter()
            .find(|h| (h.x0..h.x1).contains(&m.column))
            .copied();
        let Some(TabHit { tab, close_x, .. }) = hit else {
            return;
        };
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) if close_x == Some(m.column) => {
                self.request_close(tab)
            }
            MouseEventKind::Down(MouseButton::Left) => self.switch_to(tab),
            MouseEventKind::Down(MouseButton::Middle) => self.request_close(tab),
            _ => {}
        }
    }

    fn terminal_mouse(&mut self, kind: MouseEventKind, col: u16, row: u16) {
        let button = |b: MouseButton| match b {
            MouseButton::Left => skink_term::MouseButton::Left,
            MouseButton::Right => skink_term::MouseButton::Right,
            MouseButton::Middle => skink_term::MouseButton::Middle,
        };
        let kind = match kind {
            MouseEventKind::Down(b) => MouseKind::Down(button(b)),
            MouseEventKind::Up(b) => MouseKind::Up(button(b)),
            MouseEventKind::Drag(b) => MouseKind::Drag(button(b)),
            MouseEventKind::Moved => MouseKind::Move,
            MouseEventKind::ScrollUp => MouseKind::ScrollUp,
            MouseEventKind::ScrollDown => MouseKind::ScrollDown,
            _ => return,
        };
        let Some(pane) = self.active_term() else {
            return;
        };
        // Programs that track the mouse get the event; otherwise the wheel
        // scrolls the scrollback.
        if !pane
            .term
            .send_mouse(skink_term::MouseEvent::new(kind, col, row))
            .unwrap_or(false)
        {
            match kind {
                MouseKind::ScrollUp => pane.term.scroll_up(WHEEL_LINES),
                MouseKind::ScrollDown => pane.term.scroll_down(WHEEL_LINES),
                _ => {}
            }
        }
    }

    fn editor_mouse(&mut self, m: MouseEvent, col: u16, row: u16) {
        let kind = m.kind;
        let sideways = m.modifiers.contains(KeyModifiers::SHIFT);
        let cols = crate::doc::HWHEEL_COLS as isize;
        let ed = &mut self.doc.editor;
        match kind {
            // Shift+wheel scrolls sideways, like most editors.
            MouseEventKind::ScrollUp if sideways => self.doc.scroll_horizontally(-cols),
            MouseEventKind::ScrollDown if sideways => self.doc.scroll_horizontally(cols),
            MouseEventKind::ScrollLeft => self.doc.scroll_horizontally(-cols),
            MouseEventKind::ScrollRight => self.doc.scroll_horizontally(cols),
            MouseEventKind::ScrollUp => ed.scroll_by(-(WHEEL_LINES as isize)),
            MouseEventKind::ScrollDown => ed.scroll_by(WHEEL_LINES as isize),
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
                let gutter = self.gutter_width();
                let height = self.areas.main.height as usize;
                let vrow = self.vrow_at_row(row as usize, height);
                let line = vrow.map_or(self.doc.editor.scroll, |r| r.line);
                if matches!(kind, MouseEventKind::Down(_))
                    && self.git_click(
                        m.column,
                        m.row,
                        self.line_at_row(row as usize, height),
                        col as usize + 1 == gutter,
                    )
                {
                    return;
                }
                // The sign column opens a closed fold, else toggles a
                // breakpoint (files on disk; views fold as before).
                if col == 0 && matches!(kind, MouseEventKind::Down(_)) {
                    if self.doc.editor.folds().at_header(line).is_some() {
                        return self.doc.editor.toggle_fold_at(line);
                    }
                    if self.doc.canon.is_some() && !self.doc.editor.buffer.is_read_only() {
                        return self.toggle_breakpoint_at(line);
                    }
                    if skink_core::fold::range_at(&self.doc.editor.buffer, line).is_some() {
                        return self.doc.editor.toggle_fold_at(line);
                    }
                }
                let x = (col as usize).saturating_sub(gutter);
                let char_col = vrow.map_or(0, |r| self.col_at(&r, x));
                let ed = &mut self.doc.editor;
                match kind {
                    // Alt-click adds (or removes) a cursor.
                    MouseEventKind::Down(_) if m.modifiers.contains(KeyModifiers::ALT) => {
                        ed.toggle_cursor_at(line, char_col)
                    }
                    MouseEventKind::Drag(_) => ed.click(line, char_col, true),
                    _ => ed.click(line, char_col, false),
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tab_keys_are_global() {
        let k = |key, ctrl, alt| KeySpec {
            key,
            ctrl,
            alt,
            shift: false,
        };
        assert!(is_global_key(&k(Key::F(12), false, false)));
        assert!(is_global_key(&k(Key::PageDown, true, false)));
        assert!(is_global_key(&k(Key::Char('3'), false, true)));
        assert!(!is_global_key(&k(Key::Char('t'), true, false))); // fzf's Ctrl-T
        assert!(!is_global_key(&k(Key::Char('.'), false, true))); // shell's Alt-.
        assert!(!is_global_key(&k(Key::PageDown, false, false)));
    }

    #[test]
    fn terminal_tabs_in_the_tab_enum_are_not_docs() {
        // Guard for the "live doc" invariant used throughout: exactly one
        // Tab::Doc(None) exists, at cur_doc.
        let a = crate::App::new(
            skink_core::Editor::new(skink_core::Buffer::empty()),
            skink_config::Keymap::defaults(),
            skink_core::Backend::Local,
        );
        assert!(matches!(a.tabs[a.cur_doc], crate::Tab::Doc(None)));
    }
}
