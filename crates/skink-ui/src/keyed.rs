//! `:keys`: every action with its typed command and its keys, searchable by
//! action or key. Enter takes the next key pressed for the selected action
//! (saying what that key did before and when terminals cannot send it),
//! Ctrl-N adds a key, Delete unbinds, Ctrl-R resets to the defaults. Changes
//! go to bindings.json (comments kept) and apply at once.

use crate::{App, Overlay, commands};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use skink_config::{Key, KeySpec, Keymap};
use unicode_width::UnicodeWidthStr;

/// The action name that unbinds a key (Micro's).
pub const UNBOUND: &str = "None";

/// One action as the editor lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// An action name, or a `command-edit:` chain.
    pub action: String,
    pub about: String,
    /// What to type after Ctrl-E for it.
    pub command: Option<String>,
    /// Keys whose chain runs it now (readable names).
    pub keys: Vec<String>,
    /// Keys that run it without bindings.json.
    pub default_keys: Vec<String>,
}

impl Row {
    fn changed(&self) -> bool {
        self.keys != self.default_keys
    }

    fn matches(&self, words: &[String]) -> bool {
        let hay = format!(
            "{} {} {} {}",
            self.action,
            self.about,
            self.command.as_deref().unwrap_or(""),
            self.keys.join(" ")
        )
        .to_lowercase();
        words.iter().all(|w| hay.contains(w.as_str()))
    }
}

/// Whether `chain` (`A|B`, `A,B`) runs `action`.
fn runs(chain: &str, action: &str) -> bool {
    chain == action || chain.split(['|', ',']).any(|n| n.trim() == action)
}

/// `chain` without `action` (`Copy|CopyLine` minus Copy is `CopyLine`);
/// nothing left unbinds the key.
fn chain_without(chain: &str, action: &str) -> String {
    if chain == action {
        return UNBOUND.into();
    }
    let parts: Vec<String> = chain
        .split(',')
        .map(|part| {
            part.split('|')
                .map(str::trim)
                .filter(|n| *n != action)
                .collect::<Vec<_>>()
                .join("|")
        })
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        UNBOUND.into()
    } else {
        parts.join(",")
    }
}

/// Keys of `map` that run `action`, simplest first.
fn keys_of(bindings: &[(String, String)], action: &str) -> Vec<String> {
    let mut keys: Vec<String> = bindings
        .iter()
        .filter(|(_, c)| runs(c, action))
        .map(|(k, _)| k.clone())
        .collect();
    keys.sort_by_key(|k| (k.len(), k.clone()));
    keys
}

/// Every action (editor, app, and the command-line keys) with its keys now
/// and by default.
pub fn build_rows(
    keymap: &Keymap,
    defaults: &Keymap,
    registry: &skink_core::CommandRegistry,
) -> Vec<Row> {
    let now = keymap.bindings();
    let def = defaults.bindings();
    let mut rows: Vec<Row> = Vec::new();
    let mut push = |action: &str, about: &str| {
        if rows.iter().any(|r| r.action == action) {
            return;
        }
        rows.push(Row {
            action: action.to_owned(),
            about: about.to_owned(),
            command: commands::command_for(action),
            keys: keys_of(&now, action),
            default_keys: keys_of(&def, action),
        });
    };
    for a in commands::APP_ACTIONS {
        push(a.name, a.about);
    }
    for c in registry.iter() {
        push(c.name, c.description);
    }
    // Keys that open the command line with some text (Ctrl-E, Ctrl-L).
    let mut edits: Vec<&String> = now
        .iter()
        .chain(&def)
        .map(|(_, c)| c)
        .filter(|c| c.starts_with("command-edit:"))
        .collect();
    edits.sort();
    edits.dedup();
    for c in edits {
        let text = c.trim_start_matches("command-edit:");
        let about = if text.trim().is_empty() {
            "open the command line".to_owned()
        } else {
            format!("open the command line with \"{text}\"")
        };
        push(c, &about);
    }
    rows.sort_by_key(|r| r.action.to_lowercase());
    rows
}

/// Why a key may not reach Skink, if it may not.
pub fn terminal_note(k: &KeySpec) -> Option<&'static str> {
    match (k.ctrl, k.alt, k.shift, k.key) {
        (false, false, _, Key::Char(_)) => {
            Some("a plain character: it would no longer type itself")
        }
        (true, _, true, Key::Char(_)) => Some(
            "Ctrl-Shift-letter only arrives in terminals with the kitty keyboard protocol \
             (kitty, Ghostty, WezTerm, foot); elsewhere it is Ctrl-letter",
        ),
        (true, false, false, Key::Char('i')) => Some("most terminals send Ctrl-I as Tab"),
        (true, false, false, Key::Char('m')) => Some("most terminals send Ctrl-M as Enter"),
        (true, false, false, Key::Char('h')) => Some("many terminals send Ctrl-H as Backspace"),
        (true, false, false, Key::Char('[')) => Some("terminals send Ctrl-[ as Escape"),
        (true, _, _, Key::Char(c)) if c.is_ascii_digit() => {
            Some("legacy terminals send most Ctrl-digit keys as other keys")
        }
        (_, _, _, Key::F(n)) if n > 12 => Some("F13 and above are missing on most keyboards"),
        _ => None,
    }
}

/// What a captured key will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// The key replaces the action's keys.
    Replace,
    /// The key is added to the action's keys.
    Add,
}

/// What the editor is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Browse,
    /// Waiting for the key to use.
    Capture(Op),
    /// The key does something else now, or terminals may not send it.
    Confirm {
        op: Op,
        key: KeySpec,
        taken: Option<String>,
        note: Option<&'static str>,
    },
}

pub struct KeyEditor {
    rows: Vec<Row>,
    pub filter: String,
    shown: Vec<usize>,
    selected: usize,
    top: usize,
    pub mode: Mode,
    hits: Vec<(u16, usize)>,
    area: Rect,
}

impl KeyEditor {
    pub fn new(rows: Vec<Row>) -> Self {
        let mut k = Self {
            rows,
            filter: String::new(),
            shown: Vec::new(),
            selected: 0,
            top: 0,
            mode: Mode::Browse,
            hits: Vec::new(),
            area: Rect::default(),
        };
        k.refilter();
        k
    }

    fn set_rows(&mut self, rows: Vec<Row>) {
        let cur = self.current().map(|r| r.action.clone());
        self.rows = rows;
        self.refilter();
        if let Some(a) = cur
            && let Some(i) = self.shown.iter().position(|&r| self.rows[r].action == a)
        {
            self.selected = i;
        }
    }

    fn refilter(&mut self) {
        let words: Vec<String> = self
            .filter
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        self.shown = (0..self.rows.len())
            .filter(|&i| self.rows[i].matches(&words))
            .collect();
        self.selected = self.selected.min(self.shown.len().saturating_sub(1));
    }

    pub fn current(&self) -> Option<&Row> {
        self.shown.get(self.selected).map(|&i| &self.rows[i])
    }

    fn step(&mut self, by: isize) {
        let last = self.shown.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + by).clamp(0, last.max(0)) as usize;
    }

    pub fn select(&mut self, action: &str) -> bool {
        match self
            .shown
            .iter()
            .position(|&r| self.rows[r].action == action)
        {
            Some(i) => {
                self.selected = i;
                true
            }
            None => false,
        }
    }
}

/// A key as bindings.json names it, read back as the same key (`Alt-y`;
/// Micro reads a capital after Alt as Shift, so letters stay lower case and
/// Shift is written out).
pub fn micro_name(k: &KeySpec) -> String {
    let mut s = String::new();
    for (on, m) in [(k.ctrl, "Ctrl-"), (k.alt, "Alt-"), (k.shift, "Shift-")] {
        if on {
            s.push_str(m);
        }
    }
    match k.key {
        Key::Char(' ') => s.push_str("Space"),
        Key::Char(c) => s.extend(c.to_lowercase()),
        Key::F(n) => s.push_str(&format!("F{n}")),
        Key::BackTab => s.push_str("Backtab"),
        other => s.push_str(&format!("{other:?}")),
    }
    s
}

/// One key per line `"Key": "chain",` when every entry has its own line
/// (comments kept); otherwise `None` and the caller rewrites the file.
fn entry_lines(text: &str) -> Option<Vec<(usize, KeySpec)>> {
    let pairs = skink_config::parse_bindings(text).ok()?;
    let mut found = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix('"') else {
            continue;
        };
        let Some(end) = rest.find('"') else {
            continue;
        };
        if !rest[end + 1..].trim_start().starts_with(':') {
            continue;
        }
        let tail = rest[end + 1..].trim_start()[1..].trim();
        // The value must end on this line: one entry per line.
        if !(tail.starts_with('"') && tail.trim_end_matches(',').trim_end().ends_with('"')) {
            return None;
        }
        if let Some(k) = skink_config::parse_key(&rest[..end]) {
            found.push((n, k));
        }
    }
    (found.len() == pairs.len()).then_some(found)
}

/// bindings.json `text` with `key` bound to `chain` (`None`: the entry
/// removed, so the default comes back). Any spelling of the key already in
/// the file is replaced.
pub fn with_binding(text: &str, key: &KeySpec, chain: Option<&str>) -> String {
    let name = micro_name(key);
    let entry = chain.map(|c| {
        format!(
            "{}: {}",
            serde_json::Value::String(name.clone()),
            serde_json::Value::String(c.to_owned())
        )
    });
    let Some(lines) = entry_lines(text) else {
        // Not one entry per line: write the file anew from its pairs.
        let mut pairs = skink_config::parse_bindings(text).unwrap_or_default();
        pairs.retain(|(k, _)| skink_config::parse_key(k).as_ref() != Some(key));
        let mut out = String::from("{\n");
        let mut all: Vec<String> = pairs
            .iter()
            .map(|(k, c)| {
                format!(
                    "  {}: {}",
                    serde_json::Value::String(k.clone()),
                    serde_json::Value::String(c.clone())
                )
            })
            .collect();
        if let Some(e) = &entry {
            all.push(format!("  {e}"));
        }
        out.push_str(&all.join(",\n"));
        out.push_str("\n}\n");
        return out;
    };
    let mut out: Vec<String> = text.lines().map(str::to_owned).collect();
    let mine: Vec<usize> = lines
        .iter()
        .filter(|(_, k)| k == key)
        .map(|(n, _)| *n)
        .collect();
    match (mine.first(), &entry) {
        (Some(&n), Some(e)) => {
            let line = &out[n];
            let indent = &line[..line.len() - line.trim_start().len()];
            let comma = if line.trim_end().ends_with(',') {
                ","
            } else {
                ""
            };
            out[n] = format!("{indent}{e}{comma}");
            for &d in mine[1..].iter().rev() {
                out.remove(d);
            }
        }
        (Some(_), None) => {
            for &d in mine.iter().rev() {
                out.remove(d);
            }
        }
        (None, Some(e)) => {
            // Last, so it wins over anything before it.
            let close = out.iter().rposition(|l| l.trim_start().starts_with('}'));
            match close {
                Some(c) => {
                    if let Some(&(prev, _)) = lines.last() {
                        let p = &mut out[prev];
                        if !p.trim_end().ends_with(',') {
                            p.push(',');
                        }
                    }
                    out.insert(c, format!("  {e}"));
                }
                None => return format!("{{\n  {e}\n}}\n"),
            }
        }
        (None, None) => {}
    }
    let mut s = out.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    s
}

impl App {
    /// The keymap without bindings.json: the defaults with the preset.
    fn default_keymap(&self) -> Keymap {
        let mut k = self.base_keymap.clone();
        k.apply_preset(self.settings.keymap);
        k
    }

    fn key_rows(&self) -> Vec<Row> {
        build_rows(&self.keymap, &self.default_keymap(), &self.registry)
    }

    /// `:keys [ACTION]`: the key-binding editor.
    pub(crate) fn open_key_editor(&mut self, select: Option<&str>) {
        if self.config_dir.is_none() {
            self.doc.editor.message = Some("No config directory (set SKINK_CONFIG_DIR)".into());
            return;
        }
        let mut k = KeyEditor::new(self.key_rows());
        if let Some(a) = select {
            let a = commands::action_for(a, &self.registry).unwrap_or_else(|| a.to_owned());
            if !k.select(&a) {
                k.filter = a;
                k.refilter();
            }
        }
        self.set_overlay(Overlay::Keys(Box::new(k)));
    }

    fn key_editor(&mut self) -> Option<&mut KeyEditor> {
        match &mut self.overlay {
            Overlay::Keys(k) => Some(k),
            _ => None,
        }
    }

    /// Change bindings.json with `edit` (refused while it has unsaved edits
    /// open or errors), apply it and show the new keys.
    fn edit_bindings(&mut self, done: &str, edit: impl FnOnce(&str) -> String) {
        let msg = match self.write_bindings(edit) {
            Ok(()) => {
                let errs = self.load_user_config();
                if errs.is_empty() {
                    format!("{done} (saved in bindings.json)")
                } else {
                    errs.join("; ")
                }
            }
            Err(e) => e,
        };
        self.doc.editor.message = Some(msg);
        let rows = self.key_rows();
        if let Some(k) = self.key_editor() {
            k.set_rows(rows);
            k.mode = Mode::Browse;
        }
    }

    fn write_bindings(&mut self, edit: impl FnOnce(&str) -> String) -> Result<(), String> {
        let dir = self.config_dir.clone().ok_or("no config folder")?;
        let path = dir.join("bindings.json");
        let canon = crate::doc::canonical(&path);
        let dirty = (0..self.tabs.len()).any(|i| {
            self.doc_at(i).is_some_and(|d| {
                d.canon.as_deref() == Some(canon.as_path()) && d.editor.buffer.is_dirty()
            })
        });
        if dirty {
            return Err("bindings.json has unsaved edits; save or close its tab first".into());
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| skink_config::DEFAULT_BINDINGS_FILE.to_owned());
        skink_config::parse_bindings(&text)
            .map_err(|e| format!("bindings.json: {e}; fix it first (:bindings opens it)"))?;
        let new = edit(&text);
        skink_config::parse_bindings(&new).map_err(|e| format!("not saved: {e}"))?;
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, &new))
            .map_err(|e| format!("bindings.json: {e}"))?;
        if let Some(i) = self.find_doc(|d| d.canon.as_deref() == Some(canon.as_path()))
            && let Some(d) = self.doc_at_mut(i)
        {
            d.editor.reload(&new);
        }
        Ok(())
    }

    /// The selected action takes `key` (replacing its keys or adding one).
    fn bind_key(&mut self, op: Op, key: KeySpec) {
        let Some(row) = self.key_editor().and_then(|k| k.current().cloned()) else {
            return;
        };
        let now = self.keymap.bindings();
        let others: Vec<(KeySpec, String)> = match op {
            Op::Add => Vec::new(),
            Op::Replace => now
                .iter()
                .filter(|(k, c)| runs(c, &row.action) && skink_config::parse_key(k) != Some(key))
                .filter_map(|(k, c)| {
                    Some((skink_config::parse_key(k)?, chain_without(c, &row.action)))
                })
                .collect(),
        };
        let action = row.action.clone();
        let done = format!(
            "{} {} {}",
            commands::pretty_key(&micro_name(&key)),
            match op {
                Op::Replace => "now runs",
                Op::Add => "also runs",
            },
            action
        );
        self.edit_bindings(&done, |text| {
            let mut t = with_binding(text, &key, Some(&action));
            for (k, c) in &others {
                t = with_binding(&t, k, Some(c));
            }
            t
        });
    }

    /// Delete: the selected action loses its keys.
    fn unbind_action(&mut self) {
        let Some(row) = self.key_editor().and_then(|k| k.current().cloned()) else {
            return;
        };
        if row.keys.is_empty() {
            self.doc.editor.message = Some(format!("{} has no key", row.action));
            return;
        }
        let changes: Vec<(KeySpec, String)> = self
            .keymap
            .bindings()
            .iter()
            .filter(|(_, c)| runs(c, &row.action))
            .filter_map(|(k, c)| Some((skink_config::parse_key(k)?, chain_without(c, &row.action))))
            .collect();
        self.edit_bindings(&format!("{} has no key now", row.action), |text| {
            changes
                .iter()
                .fold(text.to_owned(), |t, (k, c)| with_binding(&t, k, Some(c)))
        });
    }

    /// Ctrl-R: bindings.json entries that change the action's keys go.
    fn reset_action(&mut self) {
        let Some(row) = self.key_editor().and_then(|k| k.current().cloned()) else {
            return;
        };
        let touched: Vec<KeySpec> = self
            .user_bindings
            .iter()
            .filter(|(k, c)| {
                runs(c, &row.action)
                    || row.keys.contains(k)
                    || row
                        .default_keys
                        .iter()
                        .any(|d| skink_config::parse_key(d) == skink_config::parse_key(k))
            })
            .filter_map(|(k, _)| skink_config::parse_key(k))
            .collect();
        if touched.is_empty() {
            self.doc.editor.message = Some(format!("{} has its default keys", row.action));
            return;
        }
        self.edit_bindings(
            &format!("{} has its default keys again", row.action),
            |text| {
                touched
                    .iter()
                    .fold(text.to_owned(), |t, k| with_binding(&t, k, None))
            },
        );
    }

    pub(crate) fn key_editor_paste(&mut self, text: &str) {
        if let Some(k) = self.key_editor()
            && k.mode == Mode::Browse
        {
            k.filter.extend(text.chars().filter(|c| !c.is_control()));
            k.selected = 0;
            k.refilter();
        }
    }

    pub(crate) fn key_editor_key(&mut self, ev: KeyEvent) {
        let Some(k) = self.key_editor() else { return };
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        match k.mode.clone() {
            Mode::Capture(op) => {
                if ev.code == KeyCode::Esc {
                    k.mode = Mode::Browse;
                    return;
                }
                let Some(key) = crate::input::key_spec(ev) else {
                    return;
                };
                let action = k.current().map(|r| r.action.clone()).unwrap_or_default();
                let taken = self
                    .keymap
                    .command(&key)
                    .filter(|c| *c != UNBOUND && !runs(c, &action))
                    .map(str::to_owned);
                let note = terminal_note(&key);
                match (taken, note) {
                    (None, None) => self.bind_key(op, key),
                    (taken, note) => {
                        if let Some(k) = self.key_editor() {
                            k.mode = Mode::Confirm {
                                op,
                                key,
                                taken,
                                note,
                            };
                        }
                    }
                }
            }
            Mode::Confirm { op, key, .. } => match ev.code {
                KeyCode::Enter | KeyCode::Char('y') => self.bind_key(op, key),
                _ => k.mode = Mode::Browse,
            },
            Mode::Browse => match ev.code {
                KeyCode::Esc if !k.filter.is_empty() => {
                    k.filter.clear();
                    k.refilter();
                }
                KeyCode::Esc => self.set_overlay(Overlay::None),
                KeyCode::Up => k.step(-1),
                KeyCode::Down => k.step(1),
                KeyCode::PageUp => k.step(-10),
                KeyCode::PageDown => k.step(10),
                KeyCode::Home => k.selected = 0,
                KeyCode::End => k.step(isize::MAX / 2),
                KeyCode::Backspace => {
                    k.filter.pop();
                    k.refilter();
                }
                KeyCode::Enter if k.current().is_some() => k.mode = Mode::Capture(Op::Replace),
                KeyCode::Char('n') if ctrl && k.current().is_some() => {
                    k.mode = Mode::Capture(Op::Add)
                }
                KeyCode::Delete => self.unbind_action(),
                KeyCode::Char('r') if ctrl => self.reset_action(),
                KeyCode::Char('o') if ctrl => {
                    self.set_overlay(Overlay::None);
                    self.run_command("bindings");
                }
                KeyCode::Char(c) if !ctrl && !ev.modifiers.contains(KeyModifiers::ALT) => {
                    k.filter.push(c);
                    k.selected = 0;
                    k.refilter();
                }
                _ => {}
            },
        }
    }

    pub(crate) fn key_editor_mouse(&mut self, m: MouseEvent) {
        let Some(k) = self.key_editor() else { return };
        match m.kind {
            MouseEventKind::ScrollUp => k.step(-3),
            MouseEventKind::ScrollDown => k.step(3),
            MouseEventKind::Down(MouseButton::Left)
                if k.mode == Mode::Browse && k.area.contains(Position::new(m.column, m.row)) =>
            {
                if let Some(&(_, i)) = k.hits.iter().find(|(y, _)| *y == m.row) {
                    if k.selected == i {
                        k.mode = Mode::Capture(Op::Replace);
                    } else {
                        k.selected = i;
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn render_key_editor(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme;
        let Overlay::Keys(k) = &mut self.overlay else {
            return;
        };
        if area.width < 20 || area.height < 6 {
            return;
        }
        k.area = area;
        frame.render_widget(Clear, area);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.c(t.blue)))
            .style(Style::new().bg(t.c(t.bg_popup)).fg(t.c(t.fg)))
            .title(Span::styled(
                " Key bindings ",
                Style::new().fg(t.c(t.blue)).add_modifier(Modifier::BOLD),
            ))
            .title(
                Line::styled(" bindings.json ", Style::new().fg(t.c(t.comment))).right_aligned(),
            );
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let w = inner.width as usize;
        let placeholder = k.filter.is_empty();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Search ", Style::new().fg(t.c(t.blue))),
                Span::styled(
                    if placeholder {
                        "an action, a command or a key (ctrl-s)".to_owned()
                    } else {
                        k.filter.clone()
                    },
                    Style::new().fg(t.c(if placeholder { t.comment } else { t.fg })),
                ),
                Span::styled(
                    format!("   {} of {}", k.shown.len(), k.rows.len()),
                    Style::new().fg(t.c(t.comment)),
                ),
            ])),
            Rect { height: 1, ..inner },
        );
        let action = k.current().map(|r| r.action.clone()).unwrap_or_default();
        let (hint, hint_c) = match &k.mode {
            Mode::Browse => (
                "Enter new key · Ctrl-N add a key · Del unbind · Ctrl-R reset · Ctrl-O open bindings.json · Esc close"
                    .to_owned(),
                t.comment,
            ),
            Mode::Capture(Op::Replace) => {
                (format!("Press the new key for {action} (Esc cancels)"), t.yellow)
            }
            Mode::Capture(Op::Add) => {
                (format!("Press a key to add to {action} (Esc cancels)"), t.yellow)
            }
            Mode::Confirm {
                key, taken, note, ..
            } => {
                let mut s = commands::pretty_key(&micro_name(key));
                if let Some(c) = taken {
                    s.push_str(&format!(" runs {c} now and would run {action} instead."));
                }
                if let Some(n) = note {
                    s.push_str(&format!(" Note: {n}."));
                }
                s.push_str(" Enter use it · Esc cancel");
                (s, t.orange)
            }
        };
        let foot = Rect {
            y: inner.bottom().saturating_sub(1),
            height: 1,
            ..inner
        };
        frame.render_widget(
            Paragraph::new(Line::styled(
                crate::view::truncate(&format!(" {hint}"), w),
                Style::new().fg(t.c(hint_c)),
            )),
            foot,
        );
        let list = Rect {
            y: inner.y + 2,
            height: inner.height.saturating_sub(3),
            ..inner
        };
        let h = list.height as usize;
        if k.selected < k.top {
            k.top = k.selected;
        }
        if k.selected >= k.top + h {
            k.top = k.selected + 1 - h;
        }
        let aw = k
            .shown
            .iter()
            .map(|&r| k.rows[r].action.width())
            .max()
            .unwrap_or(10)
            .clamp(10, 28);
        let kw = 24.min(w / 3);
        let cw = 16;
        k.hits.clear();
        for (n, &r) in k.shown.iter().enumerate().skip(k.top).take(h) {
            let y = list.y + (n - k.top) as u16;
            k.hits.push((y, n));
            let row = &k.rows[r];
            let selected = n == k.selected;
            let bg = t.c(if selected { t.bg_sel } else { t.bg_popup });
            let keys = if row.keys.is_empty() {
                "no key".to_owned()
            } else {
                row.keys
                    .iter()
                    .map(|k| commands::pretty_key(k))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let mark = match (selected, row.changed()) {
                (true, _) => "▸",
                (false, true) => "•",
                (false, false) => " ",
            };
            let cmd = row
                .command
                .as_deref()
                .map_or(String::new(), |c| format!(":{c}"));
            let used = 2 + aw + 2 + kw + 2 + cw + 2;
            let line = Line::from(vec![
                Span::styled(
                    format!("{mark} "),
                    Style::new().fg(t.c(if row.changed() { t.orange } else { t.blue })),
                ),
                Span::styled(
                    format!("{:<aw$}  ", crate::view::truncate(&row.action, aw)),
                    Style::new().fg(t.c(if selected { t.blue } else { t.fg })),
                ),
                Span::styled(
                    format!("{:<kw$}  ", crate::view::truncate(&keys, kw)),
                    Style::new().fg(t.c(if row.keys.is_empty() {
                        t.comment
                    } else if row.changed() {
                        t.orange
                    } else {
                        t.cyan
                    })),
                ),
                Span::styled(
                    format!("{:<cw$}  ", crate::view::truncate(&cmd, cw)),
                    Style::new().fg(t.c(t.purple)),
                ),
                Span::styled(
                    crate::view::truncate(&row.about, w.saturating_sub(used)),
                    Style::new().fg(t.c(t.comment)),
                ),
            ]);
            frame.render_widget(
                Paragraph::new(line).style(Style::new().bg(bg)),
                Rect {
                    y,
                    height: 1,
                    ..list
                },
            );
        }
        if k.shown.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "  Nothing matches; Esc clears the search",
                    Style::new().fg(t.c(t.comment)),
                )),
                Rect { height: 1, ..list },
            );
        }
        if k.mode == Mode::Browse {
            let x = inner.x + " Search ".width() as u16 + k.filter.width() as u16;
            frame.set_cursor_position(Position::new(
                x.min(inner.right().saturating_sub(1)),
                inner.y,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(s: &str) -> KeySpec {
        skink_config::parse_key(s).unwrap()
    }

    #[test]
    fn readable_key_names_read_back_as_the_same_key() {
        for k in [
            "Ctrl-s",
            "Alt-J",
            "Alt-y",
            "Alt-Shift-Up",
            "CtrlShift-f",
            "F5",
            "Ctrl-Space",
            "Ctrl-/",
        ] {
            let s = spec(k);
            assert_eq!(skink_config::parse_key(&micro_name(&s)), Some(s), "{k}");
        }
    }

    #[test]
    fn bindings_change_in_place_keeping_comments() {
        let t = "{\n  // mine\n  \"CtrlJ\": \"Save\",\n  \"F5\": \"Find\"\n}\n";
        // Another spelling of the same key is replaced, not repeated.
        assert_eq!(
            with_binding(t, &spec("Ctrl-j"), Some("Quit")),
            "{\n  // mine\n  \"Ctrl-j\": \"Quit\",\n  \"F5\": \"Find\"\n}\n"
        );
        assert_eq!(
            with_binding(t, &spec("Alt-x"), Some("Undo")),
            "{\n  // mine\n  \"CtrlJ\": \"Save\",\n  \"F5\": \"Find\",\n  \"Alt-x\": \"Undo\"\n}\n"
        );
        assert_eq!(
            with_binding(t, &spec("F5"), None),
            "{\n  // mine\n  \"CtrlJ\": \"Save\",\n}\n"
        );
        // A compact file is rewritten one entry per line.
        let one = with_binding("{\"Ctrl-j\": \"SelectAll\"}", &spec("F2"), Some("Save"));
        assert_eq!(
            skink_config::parse_bindings(&one).unwrap(),
            [
                ("Ctrl-j".to_owned(), "SelectAll".to_owned()),
                ("F2".to_owned(), "Save".to_owned())
            ]
        );
        assert_eq!(
            with_binding("{\n}\n", &spec("Ctrl-k"), Some("Undo")),
            "{\n  \"Ctrl-k\": \"Undo\"\n}\n"
        );
    }

    #[test]
    fn unbinding_keeps_the_rest_of_a_chain() {
        assert_eq!(chain_without("Copy|CopyLine", "Copy"), "CopyLine");
        assert_eq!(chain_without("Save", "Save"), UNBOUND);
        assert_eq!(chain_without("Escape,Deselect", "Deselect"), "Escape");
        assert!(terminal_note(&spec("CtrlShift-f")).is_some());
        assert!(terminal_note(&spec("Ctrl-i")).is_some());
        assert!(terminal_note(&spec("Alt-j")).is_none());
    }
}
