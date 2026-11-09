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

impl App {
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
        if let Overlay::Picker(p) = &mut self.overlay {
            s.chars()
                .filter(|c| !c.is_control())
                .for_each(|c| p.type_char(c));
        } else if let Overlay::Prompt(p) = &mut self.overlay {
            p.input.push_str(&s.replace(['\n', '\r'], " "));
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
        if matches!(self.overlay, Overlay::Picker(_)) {
            return self.picker_key(ev);
        }
        if matches!(self.overlay, Overlay::Prompt(_)) {
            return self.prompt_key(ev);
        }
        let Some(spec) = input::key_spec(ev) else {
            return;
        };

        if is_global_key(&spec) {
            if let Key::Char(c @ '1'..='9') = spec.key {
                return self.switch_to(c as usize - '1' as usize);
            }
            if let Some(chain) = self.keymap.command(&spec).map(str::to_owned)
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
            if pane.exited {
                // Any key dismisses a finished terminal.
                return self.close_terminal();
            }
            let _ = pane.term.send_key(spec);
            return;
        }
        match self.keymap.command(&spec).map(str::to_owned) {
            Some(chain) => self.run_chain(&chain),
            None => {
                if let Key::Char(c) = spec.key
                    && !spec.ctrl
                    && !spec.alt
                {
                    self.doc.editor.insert_char(c);
                }
            }
        }
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
                let ok = commands::run_app_action(self, name)
                    || self.registry.run(name, &mut self.doc.editor);
                if ok {
                    break;
                }
            }
        }
        if self.doc.editor.clipboard_changed {
            self.doc.editor.clipboard_changed = false;
            self.clip.set(&self.doc.editor.clipboard, self.osc52);
        }
        match self.doc.editor.request.take() {
            Some(Request::Quit) => self.request_close(self.cur_doc),
            Some(Request::SaveAs) => {
                self.open_prompt(
                    crate::prompt::PromptKind::SaveAs { close_after: false },
                    "Save as: ",
                    "",
                );
            }
            Some(Request::Find) => {
                let q = self.doc.editor.last_search.clone();
                self.open_prompt(crate::prompt::PromptKind::Find, "Find: ", &q);
            }
            None => {}
        }
    }

    // ---- file tree ---------------------------------------------------------

    /// Returns whether the tree consumed the key.
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
            KeyCode::Char('q') | KeyCode::Esc => self.focus_main(),
            KeyCode::Char('h') => self.set_sidebar_view(SidebarView::Hidden),
            _ => return false,
        }
        if let Some(p) = open {
            self.open_path(&p);
        }
        true
    }

    fn tree_mouse(&mut self, m: MouseEvent) {
        let rows = self.areas.tree.height.saturating_sub(2) as usize;
        let top = self.areas.tree.y + 2; // below the "Files" and root rows
        let Some(t) = self.tree_mut() else {
            return;
        };
        let clicked = match m.kind {
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
        if let Some(open) = clicked {
            self.set_sidebar_view(SidebarView::Focused);
            if let Some(p) = open {
                self.open_path(&p);
            }
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
        let project = picker::project_files(&root, 5000);
        self.set_overlay(Overlay::Picker(Box::new(picker::Picker::new(
            tab_items,
            &self.recent,
            project,
            &root,
        ))));
    }

    fn picker_key(&mut self, ev: KeyEvent) {
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
            KeyCode::Backspace => p.backspace(),
            KeyCode::Char('u') if ctrl => p.clear(),
            KeyCode::Char(c) if !ctrl => p.type_char(c),
            KeyCode::Enter => {
                let target = p.current();
                self.set_overlay(Overlay::None);
                match target {
                    Some(picker::Target::Tab(i)) => self.switch_to(i),
                    Some(picker::Target::File(f)) => self.open_path(&f),
                    None => {}
                }
            }
            _ => {}
        }
    }

    // ---- mouse ---------------------------------------------------------------

    pub fn handle_mouse(&mut self, m: MouseEvent) {
        if matches!(self.overlay, Overlay::Picker(_)) {
            return; // modal
        }
        let pos = Position::new(m.column, m.row);
        if self.areas.top.contains(pos) {
            return self.tab_bar_mouse(m);
        }
        if self.areas.tree.contains(pos) {
            return self.tree_mouse(m);
        }
        if !self.areas.main.contains(pos) {
            return;
        }
        self.focus_main();
        let (col, row) = (m.column - self.areas.main.x, m.row - self.areas.main.y);
        if self.active_is_term() {
            self.terminal_mouse(m.kind, col, row);
        } else {
            self.editor_mouse(m.kind, col, row);
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

    fn editor_mouse(&mut self, kind: MouseEventKind, col: u16, row: u16) {
        let ed = &mut self.doc.editor;
        match kind {
            MouseEventKind::ScrollUp => ed.scroll_by(-(WHEEL_LINES as isize)),
            MouseEventKind::ScrollDown => ed.scroll_by(WHEEL_LINES as isize),
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
                let gutter = self.gutter_width();
                let ed = &mut self.doc.editor;
                let line = (row as usize + ed.scroll).min(ed.buffer.len_lines() - 1);
                let x = (col as usize).saturating_sub(gutter) + self.doc.hscroll;
                let char_col =
                    crate::text::char_col_at(&ed.buffer.line_text(line), x, ed.tab_width);
                ed.click(line, char_col, matches!(kind, MouseEventKind::Drag(_)));
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
