//! The menu bar: File / Edit / Selection / View / Go / Code / Git /
//! Terminal / Help over the app's actions and `:` commands, each entry with
//! its key from the live keymap. F10 opens it (from a document; terminal tabs
//! keep F10 for their programs); `"menubar": true` keeps the bar on screen,
//! where the mouse opens it. Hidden, it costs no row.

use crate::{App, Overlay};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

/// One menu entry: a label and what it runs (an action chain, or a `:`
/// command line). An empty label is a separator.
pub struct Entry {
    pub label: &'static str,
    pub run: &'static str,
}

const fn e(label: &'static str, run: &'static str) -> Entry {
    Entry { label, run }
}

const SEP: Entry = e("", "");

pub static MENUS: &[(&str, &[Entry])] = &[
    (
        "File",
        &[
            e("New tab", "AddTab"),
            e("Open file or folder…", "OpenFile"),
            e("Quick open…", "QuickOpen"),
            e("Recent files", ":recent"),
            SEP,
            e("Save", "Save"),
            e("Save as…", "SaveAs"),
            e("Discard unsaved changes", ":discard"),
            e("Compare with a file…", "command-edit:diff "),
            SEP,
            e("Settings", ":settings"),
            e("Key bindings", ":keys"),
            e("Theme…", ":theme"),
            SEP,
            e("Close tab", "Quit"),
            e("Quit Skink", ":qa"),
        ],
    ),
    (
        "Edit",
        &[
            e("Undo", "Undo"),
            e("Redo", "Redo"),
            SEP,
            e("Cut", "Cut|CutLine"),
            e("Copy", "Copy|CopyLine"),
            e("Paste", "Paste"),
            SEP,
            e("Find and replace", "Find"),
            e("Find next", "FindNext"),
            e("Search the project", "FindInFiles"),
            SEP,
            e("Toggle comment", "ToggleComment"),
            e("Format", "Format"),
            e("Upper case", "UpperCase"),
            e("Lower case", "LowerCase"),
            e("Sort lines", "SortLines"),
        ],
    ),
    (
        "Selection",
        &[
            e("Select all", "SelectAll"),
            e("Expand selection", "ExpandSelection"),
            e("Shrink selection", "ShrinkSelection"),
            SEP,
            e("Add cursor at next match", "SpawnMultiCursor"),
            e("Add cursor above", "SpawnMultiCursorUp"),
            e("Add cursor below", "SpawnMultiCursorDown"),
            e("Remove extra cursors", "RemoveAllMultiCursors"),
            SEP,
            e("Move lines up", "MoveLinesUp"),
            e("Move lines down", "MoveLinesDown"),
            e("Duplicate", "Duplicate|DuplicateLine"),
        ],
    ),
    (
        "View",
        &[
            e("Command palette", "CommandPalette"),
            e("File tree", "ToggleTree"),
            e("Hidden files", "ToggleHiddenFiles"),
            SEP,
            e("Split side by side", "VSplit"),
            e("Split top and bottom", "HSplit"),
            e("Next split", "NextSplit"),
            e("Unsplit", "Unsplit"),
            SEP,
            e("Preview", "TogglePreview"),
            e("Zoom", "ToggleZoom"),
            e("Fold or unfold", "ToggleFold"),
            e("Unfold all", "UnfoldAll"),
            SEP,
            e("Problems", "Problems"),
            e("Messages", ":messages"),
        ],
    ),
    (
        "Go",
        &[
            e("Go to line…", "command-edit:goto "),
            e("Go to definition", "GotoDefinition"),
            e("Go to type definition", "GotoTypeDefinition"),
            e("Find references", "FindReferences"),
            e("Symbols in this file", "Outline"),
            e("Symbols in the project", "WorkspaceSymbols"),
            SEP,
            e("Back", "JumpBack"),
            e("Forward", "JumpForward"),
            e("Matching bracket", "JumpToMatchingBrace"),
            SEP,
            e("Next problem", "NextDiagnostic"),
            e("Previous problem", "PreviousDiagnostic"),
            SEP,
            e("Next tab", "NextTab"),
            e("Previous tab", "PreviousTab"),
        ],
    ),
    (
        "Code",
        &[
            e("Complete", "Complete"),
            e("Show hover", "Hover"),
            e("Code actions", "CodeActions"),
            e("Rename symbol", "RenameSymbol"),
            SEP,
            e("AI prompt", "AiPrompt"),
            e("Agent panel", "AgentPanel"),
            e("Send errors to the agent", "AgentSendErrors"),
            e("Review agent edits", "ReviewAgentEdits"),
            SEP,
            e("Run task…", "RunTask"),
            e("Run the last task again", "RerunTask"),
            SEP,
            e("Debug: start or continue", "DebugContinue"),
            e("Debug: stop", "DebugStop"),
            e("Toggle breakpoint", "ToggleBreakpoint"),
            e("Debug panel", "DebugPanel"),
        ],
    ),
    (
        "Git",
        &[
            e("Git panel", "GitPanel"),
            e("Next change", "NextHunk"),
            e("Previous change", "PreviousHunk"),
            e("Show change", "PeekHunk"),
            e("Stage change", "StageHunk"),
            e("Unstage change", "UnstageHunk"),
            e("Revert change", "RevertHunk"),
            SEP,
            e("Inline blame", "ToggleInlineBlame"),
            e("Blame", ":blame"),
            e("Log", ":log"),
            e("Branches", ":branches"),
            e("Merge conflicts", ":conflicts"),
            SEP,
            e("Review uncommitted changes", ":codereview"),
            e("Review staged changes", ":codereview staged"),
            e("Review this branch", ":codereview branch"),
            e("Review changes since…", "command-edit:codereview since "),
        ],
    ),
    (
        "Terminal",
        &[
            e("Terminal tab", "ToggleTerminal"),
            e("New terminal", "NewTerminal"),
            e("Terminal at the bottom", "ToggleTerminalDock"),
            SEP,
            e("Restart finished terminal", ":restart-term"),
            SEP,
            e("Scrollback history", "TerminalHistory"),
            e("Select last output", "SelectLastOutput"),
            e("Previous prompt", "PreviousPrompt"),
            e("Next prompt", "NextPrompt"),
        ],
    ),
    (
        "Help",
        &[
            e("Help", ":help"),
            e("Every key", ":help keys"),
            e("Typed commands", ":help commands"),
            e("Command palette", "CommandPalette"),
            SEP,
            e("Key bindings", ":keys"),
            e("Settings", ":settings"),
            e("Messages", ":messages"),
        ],
    ),
];

/// An open menu: which one, and the highlighted entry (never a separator).
pub struct Menu {
    pub menu: usize,
    pub item: usize,
    /// The dropdown's entry rows from the last frame (y, entry), for the mouse.
    hits: Vec<(u16, usize)>,
    dropdown: Rect,
}

impl Menu {
    pub fn new(menu: usize) -> Self {
        Self {
            menu: menu.min(MENUS.len() - 1),
            item: 0,
            hits: Vec::new(),
            dropdown: Rect::default(),
        }
    }

    fn entries(&self) -> &'static [Entry] {
        MENUS[self.menu].1
    }

    fn move_item(&mut self, by: isize) {
        let n = self.entries().len() as isize;
        let mut i = self.item as isize;
        for _ in 0..n {
            i = (i + by).rem_euclid(n);
            if !self.entries()[i as usize].label.is_empty() {
                break;
            }
        }
        self.item = i as usize;
    }

    fn switch(&mut self, by: isize) {
        self.menu = (self.menu as isize + by).rem_euclid(MENUS.len() as isize) as usize;
        self.item = 0;
    }
}

/// The bar's titles: (x0, x1, menu) from `x`.
pub fn titles(x: u16) -> Vec<(u16, u16, usize)> {
    let mut at = x + 1;
    MENUS
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            let w = name.width() as u16 + 2;
            let r = (at, at + w, i);
            at += w;
            r
        })
        .collect()
}

impl App {
    /// The shortest key that runs `run` (an action chain), readable.
    fn menu_key(&self, run: &str) -> String {
        if run.starts_with(':') || run.is_empty() {
            return String::new();
        }
        let first = run.split('|').next().unwrap_or(run);
        self.keymap
            .bindings()
            .into_iter()
            .filter(|(_, c)| c == run || c.split(['|', ',']).any(|n| n.trim() == first))
            .map(|(k, _)| k)
            // The chord people know (Ctrl-S) over a function key (F2).
            .min_by_key(|k| (k.starts_with('F'), k.len(), k.clone()))
            .map(|k| crate::commands::pretty_key(&k))
            .unwrap_or_default()
    }

    /// The `Menu` action (F10).
    pub(crate) fn open_menu(&mut self) {
        self.set_overlay(Overlay::Menu(Box::new(Menu::new(0))));
    }

    fn menu(&mut self) -> Option<&mut Menu> {
        match &mut self.overlay {
            Overlay::Menu(m) => Some(m),
            _ => None,
        }
    }

    fn run_menu_entry(&mut self, menu: usize, item: usize) {
        self.set_overlay(Overlay::None);
        let Some(entry) = MENUS.get(menu).and_then(|(_, e)| e.get(item)) else {
            return;
        };
        match entry.run.strip_prefix(':') {
            Some(cmd) => self.run_command(cmd),
            None if entry.run.is_empty() => {}
            None => self.run_chain(entry.run),
        }
    }

    pub(crate) fn menu_key_event(&mut self, ev: KeyEvent) {
        let Some(m) = self.menu() else { return };
        match ev.code {
            KeyCode::Esc | KeyCode::F(10) => self.set_overlay(Overlay::None),
            KeyCode::Left => m.switch(-1),
            KeyCode::Right | KeyCode::Tab => m.switch(1),
            KeyCode::BackTab => m.switch(-1),
            KeyCode::Up => m.move_item(-1),
            KeyCode::Down => m.move_item(1),
            KeyCode::Home => {
                m.item = 0;
            }
            KeyCode::End => {
                m.item = m.entries().len() - 1;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let (menu, item) = (m.menu, m.item);
                self.run_menu_entry(menu, item);
            }
            // A letter jumps to the next entry starting with it.
            KeyCode::Char(c) if !ev.modifiers.contains(KeyModifiers::CONTROL) => {
                let c = c.to_ascii_lowercase();
                let n = m.entries().len();
                let hit = (1..=n).map(|d| (m.item + d) % n).find(|&i| {
                    m.entries()[i]
                        .label
                        .chars()
                        .next()
                        .is_some_and(|f| f.to_ascii_lowercase() == c)
                });
                if let Some(i) = hit {
                    m.item = i;
                }
            }
            _ => {}
        }
    }

    /// Where the bar is drawn: its own row, or over the tab bar while a
    /// menu is open and the bar is hidden.
    pub(crate) fn menu_bar_area(&self) -> Rect {
        if self.areas.menu.height > 0 {
            self.areas.menu
        } else {
            self.areas.top
        }
    }

    /// A click or hover with a menu open, or a click on the visible bar.
    /// True when the mouse event was the menu's.
    pub(crate) fn menu_mouse(&mut self, m: MouseEvent) -> bool {
        let pos = Position::new(m.column, m.row);
        let bar = self.menu_bar_area();
        let on_bar = bar.height > 0 && bar.contains(pos);
        let title = titles(bar.x)
            .into_iter()
            .find(|(x0, x1, _)| (*x0..*x1).contains(&m.column))
            .map(|(_, _, i)| i);
        let open = matches!(self.overlay, Overlay::Menu(_));
        let down = matches!(m.kind, MouseEventKind::Down(MouseButton::Left));
        if !open {
            // The visible bar opens a menu.
            if down && on_bar && self.areas.menu.height > 0 {
                if let Some(i) = title {
                    self.set_overlay(Overlay::Menu(Box::new(Menu::new(i))));
                }
                return true;
            }
            return false;
        }
        let Some(menu) = self.menu() else {
            return false;
        };
        let item = menu
            .hits
            .iter()
            .find(|(y, _)| *y == m.row)
            .filter(|_| menu.dropdown.contains(pos))
            .map(|(_, i)| *i);
        match m.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                if let Some(i) = item {
                    menu.item = i;
                } else if on_bar && let Some(t) = title {
                    menu.menu = t;
                    menu.item = 0;
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
                match (item, on_bar.then_some(title).flatten()) {
                    (Some(i), _) if down => {
                        let menu_i = menu.menu;
                        self.run_menu_entry(menu_i, i);
                    }
                    (_, Some(t)) if down && t == menu.menu => self.set_overlay(Overlay::None),
                    (_, Some(t)) if down => {
                        menu.menu = t;
                        menu.item = 0;
                    }
                    (None, None) if down => self.set_overlay(Overlay::None),
                    _ => {}
                }
            }
            _ => {}
        }
        true
    }

    /// The bar (always when `"menubar"` is on; over the tab bar while a menu
    /// is open otherwise) and the open menu's list.
    pub(crate) fn render_menu(&mut self, frame: &mut Frame) {
        let t = self.theme;
        let open = match &self.overlay {
            Overlay::Menu(m) => Some(m.menu),
            _ => None,
        };
        let bar = self.menu_bar_area();
        if bar.height == 0 || (open.is_none() && self.areas.menu.height == 0) {
            return;
        }
        let bar_style = Style::new().bg(t.c(t.bg_dark)).fg(t.c(t.fg_dim));
        let mut spans = vec![Span::raw(" ")];
        for (i, (name, _)) in MENUS.iter().enumerate() {
            let style = if open == Some(i) {
                Style::new()
                    .bg(t.c(t.bg_sel))
                    .fg(t.c(t.fg))
                    .add_modifier(Modifier::BOLD)
            } else {
                bar_style
            };
            spans.push(Span::styled(format!(" {name} "), style));
        }
        if open.is_none() {
            spans.push(Span::styled("   F10", Style::new().fg(t.c(t.comment))));
        }
        frame.render_widget(Clear, bar);
        frame.render_widget(Paragraph::new(Line::from(spans)).style(bar_style), bar);
        let Some(menu_i) = open else {
            return;
        };
        let keys: Vec<String> = MENUS[menu_i]
            .1
            .iter()
            .map(|e| self.menu_key(e.run))
            .collect();
        let full = frame.area();
        let Overlay::Menu(m) = &mut self.overlay else {
            return;
        };
        let entries = MENUS[menu_i].1;
        let lw = entries.iter().map(|e| e.label.width()).max().unwrap_or(4);
        let kw = keys.iter().map(|k| k.width()).max().unwrap_or(0);
        let width = (lw + kw + 6) as u16;
        let (x0, _, _) = titles(bar.x)[menu_i];
        let height = (entries.len() as u16 + 2).min(full.bottom().saturating_sub(bar.bottom()));
        let rect = Rect {
            x: x0.min(full.right().saturating_sub(width)),
            y: bar.bottom(),
            width: width.min(full.width),
            height,
        };
        m.dropdown = rect;
        frame.render_widget(Clear, rect);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.c(t.blue)))
            .style(Style::new().bg(t.c(t.bg_popup)).fg(t.c(t.fg)));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        m.hits.clear();
        for (i, (entry, key)) in entries
            .iter()
            .zip(&keys)
            .enumerate()
            .take(inner.height as usize)
        {
            let y = inner.y + i as u16;
            let row = Rect {
                y,
                height: 1,
                ..inner
            };
            if entry.label.is_empty() {
                frame.render_widget(
                    Paragraph::new("─".repeat(inner.width as usize))
                        .style(Style::new().fg(t.c(t.guide))),
                    row,
                );
                continue;
            }
            m.hits.push((y, i));
            let selected = i == m.item;
            let bg = t.c(if selected { t.bg_sel } else { t.bg_popup });
            let pad = (inner.width as usize).saturating_sub(entry.label.width() + key.width() + 2);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!(" {}{}", entry.label, " ".repeat(pad)),
                        Style::new().fg(t.c(if selected { t.fg } else { t.fg_dim })),
                    ),
                    Span::styled(format!("{key} "), Style::new().fg(t.c(t.comment))),
                ]))
                .style(Style::new().bg(bg)),
                row,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{APP_ACTIONS, COMMANDS};

    #[test]
    fn every_menu_entry_runs_something_real() {
        let r = skink_core::CommandRegistry::builtin();
        for (menu, entries) in MENUS {
            assert!(
                !entries.first().unwrap().label.is_empty(),
                "{menu} starts with a separator"
            );
            for e in *entries {
                if e.label.is_empty() || e.run.starts_with("command-edit:") {
                    continue;
                }
                match e.run.strip_prefix(':') {
                    Some(cmd) => {
                        let name = cmd.split_whitespace().next().unwrap();
                        assert!(
                            COMMANDS.iter().any(|c| c.name == name),
                            "{menu} > {}: no :{name}",
                            e.label
                        );
                    }
                    None => {
                        for a in e.run.split('|') {
                            assert!(
                                r.get(a).is_some() || APP_ACTIONS.iter().any(|x| x.name == a),
                                "{menu} > {}: no action {a}",
                                e.label
                            );
                        }
                    }
                }
            }
        }
    }
}
