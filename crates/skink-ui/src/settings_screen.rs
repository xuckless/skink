//! `:settings`: every setting of the schema table (`skink_config::schema`),
//! grouped and searchable, each with an editor for its kind (toggle, list,
//! number, text, a theme list with live preview). A change goes to the
//! user's settings.json or the project's `.skink/settings.json` (Tab picks),
//! written with its comments kept, and applies at once. `:settings json`
//! still opens the file itself.

use crate::settingsui::Scope;
use crate::theme::Theme;
use crate::{App, Overlay};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use serde_json::{Map, Value};
use skink_config::schema::{self, Kind};
use unicode_width::UnicodeWidthStr;

/// One setting as the screen lists it.
#[derive(Debug, Clone)]
pub struct Row {
    /// The key (`tabsize`, `lsp.dart`), or a family (`lsp.`) to add one of.
    pub key: String,
    pub group: &'static str,
    pub kind: Kind,
    pub about: &'static str,
    pub default: Option<Value>,
    pub user: Option<Value>,
    pub project: Option<Value>,
}

/// Where the value in effect comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Default,
    User,
    Project,
    /// No file sets it and it has no default value (a family, `ai.model.ask`).
    Unset,
}

impl Row {
    pub fn effective(&self) -> (Option<&Value>, Source) {
        match (&self.project, &self.user, &self.default) {
            (Some(v), _, _) => (Some(v), Source::Project),
            (None, Some(v), _) => (Some(v), Source::User),
            (None, None, Some(v)) => (Some(v), Source::Default),
            (None, None, None) => (None, Source::Unset),
        }
    }

    fn in_scope(&self, scope: Scope) -> Option<&Value> {
        match scope {
            Scope::User => self.user.as_ref(),
            Scope::Project => self.project.as_ref(),
        }
    }

    /// `lsp.` / `ai.model.`: a family of keys, added in the file.
    fn is_family(&self) -> bool {
        self.key.ends_with('.')
    }

    fn shown_key(&self) -> String {
        match self.key.as_str() {
            "lsp." => "lsp.<language>".into(),
            k if k.ends_with('.') => format!("{k}<mode>"),
            k => k.to_owned(),
        }
    }

    fn matches(&self, words: &[String]) -> bool {
        let hay = format!(
            "{} {} {} {}",
            self.key,
            self.about,
            self.group,
            self.effective()
                .0
                .map(|v| show(v, self.kind))
                .unwrap_or_default()
        )
        .to_lowercase();
        words.iter().all(|w| hay.contains(w.as_str()))
    }
}

/// `s` cut to `max` columns from the left (a path keeps its end), with `…`.
fn cut_left(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_owned();
    }
    let mut out: Vec<char> = Vec::new();
    let mut w = 1;
    for ch in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    std::iter::once('…').chain(out.into_iter().rev()).collect()
}

/// A value as the list shows it: on/off, a bare string, else JSON.
fn show(v: &Value, kind: Kind) -> String {
    match (kind, v) {
        (Kind::Bool, Value::Bool(true)) => "on".into(),
        (Kind::Bool, Value::Bool(false)) => "off".into(),
        (_, Value::String(s)) => s.clone(),
        (Kind::Columns, Value::Array(a)) if a.is_empty() => "none".into(),
        (_, v) => v.to_string(),
    }
}

/// What the text editor starts with for a value.
fn edit_text(v: Option<&Value>, kind: Kind) -> String {
    match (kind, v) {
        (_, None) => String::new(),
        (Kind::Columns, Some(Value::Array(a))) => a
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        (_, Some(Value::String(s))) => s.clone(),
        (_, Some(v)) => v.to_string(),
    }
}

/// Typed text as the JSON to write for `kind`, or why it is not valid.
pub fn parse_input(kind: Kind, input: &str) -> Result<String, String> {
    let input = input.trim();
    let json = |s: &str| {
        serde_json::from_str::<Value>(&skink_config::relaxed_json(s)).map_err(|e| e.to_string())
    };
    match kind {
        Kind::Int { min, max } => match input.parse::<u64>() {
            Ok(n) if (min..=max).contains(&n) => Ok(n.to_string()),
            _ => Err(format!("a whole number from {min} to {max}")),
        },
        Kind::Str | Kind::Enum(_) if input.is_empty() => {
            Err("empty: Delete resets it to the default".into())
        }
        Kind::Str => Ok(Value::String(input.to_owned()).to_string()),
        Kind::Enum(options) => match options.iter().find(|o| **o == input) {
            Some(o) => Ok(Value::String((*o).to_owned()).to_string()),
            None => Err(format!("one of {}", options.join(", "))),
        },
        Kind::Columns => {
            let cols: Result<Vec<u64>, _> = input
                .split([',', ' '])
                .filter(|w| !w.is_empty())
                .map(str::parse::<u64>)
                .collect();
            match cols {
                Ok(c) if c.iter().all(|n| (1..=1000).contains(n)) => Ok(Value::from(c).to_string()),
                _ => Err("columns from 1 to 1000, e.g. 80, 100".into()),
            }
        }
        Kind::Bool => match input {
            "on" | "true" => Ok("true".into()),
            "off" | "false" => Ok("false".into()),
            _ => Err("on or off".into()),
        },
        Kind::BoolOrAsk => match input {
            "on" | "true" => Ok("true".into()),
            "off" | "false" => Ok("false".into()),
            "ask" | "\"ask\"" => Ok("\"ask\"".into()),
            _ => Err("true, false or ask".into()),
        },
        // A name or command line, or a JSON list of words.
        Kind::Agent | Kind::Lsp if !input.starts_with(['[', '{', '"']) => match input {
            "" => Err("empty: Delete resets it".into()),
            "false" if kind == Kind::Lsp => Ok("false".into()),
            s => Ok(Value::String(s.to_owned()).to_string()),
        },
        Kind::Agent | Kind::Lsp | Kind::Remotes => json(input).map(|v| v.to_string()),
    }
}

/// What the screen is doing.
pub enum Edit {
    /// Moving through the list; typing filters it.
    Browse,
    /// Typing a value (caret in chars).
    Text { input: String, caret: usize },
    /// One of a few values.
    Choose { options: Vec<Value>, at: usize },
    /// The theme list: moving previews, Esc puts `before` back.
    Theme {
        names: Vec<String>,
        at: usize,
        before: Theme,
    },
}

pub struct SettingsScreen {
    rows: Vec<Row>,
    pub filter: String,
    /// Indices into `rows` that match the filter, in display order.
    shown: Vec<usize>,
    /// Index into `shown`.
    selected: usize,
    /// First list line drawn (lines include group headings).
    top: usize,
    pub scope: Scope,
    pub edit: Edit,
    /// From the last frame, for the mouse: (screen row, index into `shown`).
    hits: Vec<(u16, usize)>,
    area: Rect,
}

/// A settings file's top-level keys (`[lang]` sections left out).
fn layer(text: Option<&str>) -> Map<String, Value> {
    let Some(text) = text else {
        return Map::new();
    };
    match serde_json::from_str::<Value>(&skink_config::relaxed_json(text)) {
        Ok(Value::Object(m)) => m
            .into_iter()
            .filter(|(k, _)| !(k.starts_with('[') && k.ends_with(']')))
            .collect(),
        _ => Map::new(),
    }
}

/// Every setting of the schema table, in group order, with its value in
/// each file: keys of a family (`lsp.dart`) that the files set, and the
/// family itself to add another.
pub fn build_rows(user: Option<&str>, project: Option<&str>) -> Vec<Row> {
    let (u, p) = (layer(user), layer(project));
    let mut rows = Vec::new();
    for k in schema::KEYS {
        let mk = |key: &str| Row {
            key: key.to_owned(),
            group: schema::group_of(key),
            kind: k.kind,
            about: k.about,
            default: schema::default_value(key),
            user: u.get(key).cloned(),
            project: p.get(key).cloned(),
        };
        if k.name.ends_with('.') {
            let mut members: Vec<&String> = u
                .keys()
                .chain(p.keys())
                .filter(|key| schema::lookup(key).is_some_and(|e| e.name == k.name))
                .collect();
            members.sort();
            members.dedup();
            rows.extend(members.into_iter().map(|key| mk(key)));
        }
        rows.push(mk(k.name));
    }
    let order = |r: &Row| schema::GROUPS.iter().position(|g| *g == r.group);
    rows.sort_by_key(order); // stable: table order within a group
    rows
}

impl SettingsScreen {
    pub fn new(rows: Vec<Row>, scope: Scope) -> Self {
        let mut s = Self {
            rows,
            filter: String::new(),
            shown: Vec::new(),
            selected: 0,
            top: 0,
            scope,
            edit: Edit::Browse,
            hits: Vec::new(),
            area: Rect::default(),
        };
        s.refilter();
        s
    }

    /// New values from disk, keeping the filter and the selected key.
    pub fn set_rows(&mut self, rows: Vec<Row>) {
        let key = self.current().map(|r| r.key.clone());
        self.rows = rows;
        self.refilter();
        if let Some(k) = key
            && let Some(i) = self.shown.iter().position(|&r| self.rows[r].key == k)
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

    pub fn select_key(&mut self, key: &str) -> bool {
        match self.shown.iter().position(|&r| self.rows[r].key == key) {
            Some(i) => {
                self.selected = i;
                true
            }
            None => false,
        }
    }

    fn step(&mut self, by: isize) {
        let last = self.shown.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + by).clamp(0, last.max(0)) as usize;
    }

    pub fn type_filter(&mut self, c: char) {
        self.filter.push(c);
        self.selected = 0;
        self.refilter();
    }

    /// The list as lines: `None` is a group heading, `Some(i)` the two
    /// lines of row `shown[i]` (key and value, then its description).
    fn layout(&self) -> Vec<(Option<usize>, &'static str)> {
        let mut out = Vec::new();
        let mut group = "";
        for (i, &r) in self.shown.iter().enumerate() {
            let g = self.rows[r].group;
            if g != group {
                if !out.is_empty() {
                    out.push((None, ""));
                }
                out.push((None, g));
                group = g;
            }
            out.push((Some(i), "1"));
            out.push((Some(i), "2"));
        }
        out
    }
}

impl App {
    /// The settings files' text as they are on disk (`None`: no file).
    fn settings_texts(&self) -> (Option<String>, Option<String>) {
        let read = |s| {
            self.settings_file(s)
                .ok()
                .and_then(|(p, _)| std::fs::read_to_string(p).ok())
        };
        (read(Scope::User), read(Scope::Project))
    }

    fn settings_rows(&self) -> Vec<Row> {
        let (u, p) = self.settings_texts();
        build_rows(u.as_deref(), p.as_deref())
    }

    /// `:settings`: the settings screen.
    pub(crate) fn open_settings_screen(&mut self, select: Option<&str>) {
        if self.config_dir.is_none() {
            self.doc.editor.message = Some("No config directory (set SKINK_CONFIG_DIR)".into());
            return;
        }
        let mut s = SettingsScreen::new(self.settings_rows(), Scope::User);
        if let Some(k) = select
            && !s.select_key(k)
        {
            s.filter = k.to_owned();
            s.refilter();
        }
        self.set_overlay(Overlay::Settings(Box::new(s)));
    }

    fn screen(&mut self) -> Option<&mut SettingsScreen> {
        match &mut self.overlay {
            Overlay::Settings(s) => Some(s),
            _ => None,
        }
    }

    /// Write `key` (JSON text, or `None` to reset it) to the screen's file,
    /// apply the files again and show the new values.
    fn settings_write(&mut self, key: &str, value: Option<String>) {
        let Some(scope) = self.screen().map(|s| s.scope) else {
            return;
        };
        let msg = match self.persist_setting_in(scope, key, value.as_deref()) {
            Ok(()) => {
                let errs = self.load_user_config();
                if errs.is_empty() {
                    match &value {
                        Some(v) => format!("{key} = {v} (saved in {})", scope.file_name()),
                        None => format!("{key} reset (removed from {})", scope.file_name()),
                    }
                } else {
                    errs.join("; ")
                }
            }
            Err(e) => e,
        };
        self.doc.editor.message = Some(msg);
        let rows = self.settings_rows();
        if let Some(s) = self.screen() {
            s.set_rows(rows);
            s.edit = Edit::Browse;
        }
    }

    pub(crate) fn settings_screen_paste(&mut self, text: &str) {
        let Some(s) = self.screen() else { return };
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        match &mut s.edit {
            Edit::Text { input, caret } => {
                let at = input
                    .char_indices()
                    .nth(*caret)
                    .map_or(input.len(), |(i, _)| i);
                input.insert_str(at, &clean);
                *caret += clean.chars().count();
            }
            Edit::Browse => clean.chars().for_each(|c| s.type_filter(c)),
            Edit::Choose { .. } | Edit::Theme { .. } => {}
        }
    }

    pub(crate) fn settings_screen_key(&mut self, ev: KeyEvent) {
        let Some(s) = self.screen() else { return };
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        match &mut s.edit {
            Edit::Text { input, caret } => {
                let len = input.chars().count();
                let byte = |s: &str, c: usize| s.char_indices().nth(c).map_or(s.len(), |(i, _)| i);
                match ev.code {
                    KeyCode::Esc => s.edit = Edit::Browse,
                    KeyCode::Enter => {
                        let input = input.clone();
                        self.settings_commit_text(&input);
                    }
                    KeyCode::Left => *caret = caret.saturating_sub(1),
                    KeyCode::Right => *caret = (*caret + 1).min(len),
                    KeyCode::Home => *caret = 0,
                    KeyCode::End => *caret = len,
                    KeyCode::Char('a') if ctrl => *caret = 0,
                    KeyCode::Char('e') if ctrl => *caret = len,
                    KeyCode::Char('u') if ctrl => {
                        input.clear();
                        *caret = 0;
                    }
                    KeyCode::Backspace if *caret > 0 => {
                        let at = byte(input, *caret - 1);
                        input.remove(at);
                        *caret -= 1;
                    }
                    KeyCode::Delete if *caret < len => {
                        let at = byte(input, *caret);
                        input.remove(at);
                    }
                    KeyCode::Char(c) if !ctrl => {
                        let at = byte(input, *caret);
                        input.insert(at, c);
                        *caret += 1;
                    }
                    _ => {}
                }
            }
            Edit::Choose { options, at } => match ev.code {
                KeyCode::Esc => s.edit = Edit::Browse,
                KeyCode::Up | KeyCode::Left => *at = at.saturating_sub(1),
                KeyCode::Down | KeyCode::Right => *at = (*at + 1).min(options.len() - 1),
                KeyCode::Enter => {
                    let v = options[*at].to_string();
                    let key = s.current().map(|r| r.key.clone()).unwrap_or_default();
                    self.settings_write(&key, Some(v));
                }
                _ => {}
            },
            Edit::Theme { names, at, before } => {
                let before = *before;
                let moved = match ev.code {
                    KeyCode::Esc => {
                        s.edit = Edit::Browse;
                        self.theme = before;
                        return;
                    }
                    KeyCode::Enter => {
                        let v = Value::String(names[*at].clone()).to_string();
                        return self.settings_write("colorscheme", Some(v));
                    }
                    KeyCode::Up => {
                        *at = at.saturating_sub(1);
                        true
                    }
                    KeyCode::Down => {
                        *at = (*at + 1).min(names.len() - 1);
                        true
                    }
                    _ => false,
                };
                if moved {
                    let name = names[*at].clone();
                    self.preview_theme(&name, before);
                }
            }
            Edit::Browse => match ev.code {
                KeyCode::Esc if !s.filter.is_empty() => {
                    s.filter.clear();
                    s.refilter();
                }
                KeyCode::Esc => self.set_overlay(Overlay::None),
                KeyCode::Up => s.step(-1),
                KeyCode::Down => s.step(1),
                KeyCode::PageUp => s.step(-10),
                KeyCode::PageDown => s.step(10),
                KeyCode::Home if ctrl || s.filter.is_empty() => s.selected = 0,
                KeyCode::End if ctrl || s.filter.is_empty() => s.step(isize::MAX / 2),
                KeyCode::Tab | KeyCode::BackTab => {
                    s.scope = match s.scope {
                        Scope::User => Scope::Project,
                        Scope::Project => Scope::User,
                    };
                }
                KeyCode::Backspace => {
                    s.filter.pop();
                    s.refilter();
                }
                KeyCode::Char('o') if ctrl => self.settings_open_file(),
                KeyCode::Delete => self.settings_reset(),
                KeyCode::Enter => self.settings_activate(),
                KeyCode::Left => self.settings_nudge(-1),
                KeyCode::Right => self.settings_nudge(1),
                KeyCode::Char(c) if !ctrl && !ev.modifiers.contains(KeyModifiers::ALT) => {
                    s.type_filter(c)
                }
                _ => {}
            },
        }
    }

    fn preview_theme(&mut self, name: &str, before: Theme) {
        let name = match name {
            "auto" => self.appearance.theme().to_owned(),
            n => n.to_owned(),
        };
        let dirs = {
            let mut d = crate::theme::ThemeDirs::new(self.config_dir.clone());
            d.packages = self.pkgs.active.theme_dirs.clone();
            d
        };
        self.theme = dirs.load(&name, before.true_color).unwrap_or(before);
    }

    /// Enter: toggle, open the list, or start typing a value.
    fn settings_activate(&mut self) {
        let Some(s) = self.screen() else { return };
        let scope = s.scope;
        let Some(row) = s.current().cloned() else {
            return;
        };
        if row.is_family() {
            let (file, _) = match self.settings_file(scope) {
                Ok(f) => f,
                Err(e) => {
                    self.doc.editor.message = Some(e);
                    return;
                }
            };
            self.doc.editor.message = Some(format!(
                "Add \"{}\": ... in {} (Ctrl-O opens it)",
                row.shown_key(),
                file.display()
            ));
            return;
        }
        let current = row.effective().0.cloned();
        let edit = match row.kind {
            Kind::Bool => {
                let on = current.as_ref().and_then(Value::as_bool).unwrap_or(false);
                return self.settings_write(&row.key, Some((!on).to_string()));
            }
            _ if row.key == "colorscheme" => {
                let mut names: Vec<String> = vec!["auto".into()];
                let mut d = crate::theme::ThemeDirs::new(self.config_dir.clone());
                d.packages = self.pkgs.active.theme_dirs.clone();
                names.extend(d.list().into_iter().map(|(n, _)| n));
                let cur = current
                    .as_ref()
                    .and_then(Value::as_str)
                    .unwrap_or("auto")
                    .to_owned();
                let at = names.iter().position(|n| *n == cur).unwrap_or(0);
                let before = self.theme;
                Edit::Theme { names, at, before }
            }
            Kind::Enum(opts) => Edit::Choose {
                at: opts
                    .iter()
                    .position(|o| current.as_ref().and_then(Value::as_str) == Some(*o))
                    .unwrap_or(0),
                options: opts.iter().map(|o| Value::String((*o).into())).collect(),
            },
            Kind::BoolOrAsk => {
                let options = vec![Value::Bool(true), Value::Bool(false), "ask".into()];
                Edit::Choose {
                    at: options
                        .iter()
                        .position(|o| Some(o) == current.as_ref())
                        .unwrap_or(0),
                    options,
                }
            }
            kind => {
                let input = edit_text(current.as_ref(), kind);
                Edit::Text {
                    caret: input.chars().count(),
                    input,
                }
            }
        };
        if let Some(s) = self.screen() {
            s.edit = edit;
        }
    }

    fn settings_commit_text(&mut self, input: &str) {
        let Some(row) = self.screen().and_then(|s| s.current().cloned()) else {
            return;
        };
        match parse_input(row.kind, input) {
            Ok(v) => self.settings_write(&row.key, Some(v)),
            // Stay in the editor so the text can be fixed.
            Err(e) => self.doc.editor.message = Some(format!("{}: {e}", row.key)),
        }
    }

    /// Left / Right: a number one down or up, the previous or next choice,
    /// a toggle flipped.
    fn settings_nudge(&mut self, by: i64) {
        let Some(row) = self.screen().and_then(|s| s.current().cloned()) else {
            return;
        };
        if row.is_family() {
            return;
        }
        let cur = row.effective().0.cloned();
        let next = match row.kind {
            Kind::Bool => Some((!cur.and_then(|v| v.as_bool()).unwrap_or(false)).to_string()),
            Kind::Int { min, max } => {
                let n = cur.and_then(|v| v.as_u64()).unwrap_or(min) as i64;
                let n = (n + by).clamp(min as i64, max as i64);
                Some(n.to_string())
            }
            Kind::Enum(opts) if row.key != "colorscheme" => {
                let at = opts
                    .iter()
                    .position(|o| cur.as_ref().and_then(Value::as_str) == Some(*o))
                    .unwrap_or(0) as i64;
                let n = opts.len() as i64;
                let i = (at + by).rem_euclid(n) as usize;
                Some(Value::String(opts[i].into()).to_string())
            }
            _ => None,
        };
        if let Some(v) = next
            && row.effective().0.map(Value::to_string).as_deref() != Some(v.as_str())
        {
            self.settings_write(&row.key, Some(v));
        }
    }

    /// Delete: take the key out of the screen's file.
    fn settings_reset(&mut self) {
        let Some(s) = self.screen() else { return };
        let scope = s.scope;
        let Some(row) = s.current().cloned() else {
            return;
        };
        if row.is_family() {
            return;
        }
        if row.in_scope(scope).is_none() {
            self.doc.editor.message =
                Some(format!("{} is not set in {}", row.key, scope.file_name()));
            return;
        }
        self.settings_write(&row.key, None);
    }

    /// Ctrl-O: open the screen's file as text, at the selected key.
    fn settings_open_file(&mut self) {
        let Some(s) = self.screen() else { return };
        let scope = s.scope;
        let key = s.current().map(|r| r.key.clone());
        self.set_overlay(Overlay::None);
        match scope {
            Scope::User => self.open_settings(),
            Scope::Project => self.open_project_settings(),
        }
        if let Some(k) = key {
            let quoted = format!("\"{k}\"");
            let ed = &mut self.doc.editor;
            let found =
                (0..ed.buffer.len_lines()).find(|&l| ed.buffer.line_text(l).contains(&quoted));
            if let Some(l) = found {
                ed.cursor = skink_core::Cursor { line: l, col: 0 };
            }
        }
    }

    pub(crate) fn settings_screen_mouse(&mut self, m: MouseEvent) {
        let Some(s) = self.screen() else { return };
        let pos = Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollUp => s.step(-3),
            MouseEventKind::ScrollDown => s.step(3),
            MouseEventKind::Down(MouseButton::Left) => {
                if !s.area.contains(pos) {
                    return;
                }
                let Some(&(_, i)) = s.hits.iter().find(|(y, _)| *y == m.row) else {
                    return;
                };
                if matches!(s.edit, Edit::Browse) && s.selected == i {
                    return self.settings_activate();
                }
                // A click elsewhere ends an edit (Theme puts the old one back).
                let restore = match std::mem::replace(&mut s.edit, Edit::Browse) {
                    Edit::Theme { before, .. } => Some(before),
                    _ => None,
                };
                s.selected = i;
                if let Some(t) = restore {
                    self.theme = t;
                }
            }
            _ => {}
        }
    }

    pub(crate) fn render_settings_screen(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme;
        let file = |scope| {
            self.settings_file(scope)
                .map(|(p, _)| p.display().to_string())
                .unwrap_or_default()
        };
        let (user_file, project_file) = (file(Scope::User), file(Scope::Project));
        let Overlay::Settings(s) = &mut self.overlay else {
            return;
        };
        if area.width < 20 || area.height < 6 {
            return;
        }
        s.area = area;
        frame.render_widget(Clear, area);
        let target = match s.scope {
            Scope::User => format!(" Saving to {user_file} "),
            Scope::Project => format!(" Saving to {project_file} (this project) "),
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.c(t.blue)))
            .style(Style::new().bg(t.c(t.bg_popup)).fg(t.c(t.fg)))
            .title(Span::styled(
                " Settings ",
                Style::new().fg(t.c(t.blue)).add_modifier(Modifier::BOLD),
            ))
            .title(
                Line::styled(
                    cut_left(&target, area.width.saturating_sub(14) as usize),
                    Style::new().fg(t.c(t.comment)),
                )
                .right_aligned(),
            );
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let w = inner.width as usize;
        // Search line.
        let search = Rect { height: 1, ..inner };
        let placeholder = s.filter.is_empty();
        let shown_filter = if placeholder {
            "type to search settings".to_owned()
        } else {
            s.filter.clone()
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Search ", Style::new().fg(t.c(t.blue))),
                Span::styled(
                    shown_filter,
                    Style::new().fg(t.c(if placeholder { t.comment } else { t.fg })),
                ),
                Span::styled(
                    format!("   {} of {}", s.shown.len(), s.rows.len()),
                    Style::new().fg(t.c(t.comment)),
                ),
            ])),
            search,
        );
        let hint = match s.edit {
            Edit::Browse => {
                "Enter change · ←/→ step · Del reset · Tab user/project file · Ctrl-O open the file · Esc close"
            }
            Edit::Text { .. } => "Type the value · Enter save · Esc cancel",
            Edit::Choose { .. } => "↑/↓ choose · Enter save · Esc cancel",
            Edit::Theme { .. } => "↑/↓ preview · Enter keep it · Esc put the old theme back",
        };
        let foot = Rect {
            y: inner.bottom().saturating_sub(1),
            height: 1,
            ..inner
        };
        frame.render_widget(
            Paragraph::new(Line::styled(
                crate::view::truncate(&format!(" {hint}"), w),
                Style::new().fg(t.c(t.comment)),
            )),
            foot,
        );
        let list = Rect {
            y: inner.y + 2,
            height: inner.height.saturating_sub(3),
            ..inner
        };
        let lines = s.layout();
        let h = list.height as usize;
        // Keep the selected row's two lines (and its heading, when it is the
        // group's first) on screen.
        if let Some(first) = lines.iter().position(|(i, _)| *i == Some(s.selected)) {
            let from = if first >= 1 && lines[first - 1].0.is_none() {
                first - 1
            } else {
                first
            };
            if from < s.top {
                s.top = from;
            }
            if first + 2 > s.top + h {
                s.top = (first + 2).saturating_sub(h);
            }
        }
        s.top = s.top.min(lines.len().saturating_sub(h.min(lines.len())));
        let key_w = s
            .shown
            .iter()
            .map(|&r| s.rows[r].shown_key().width())
            .max()
            .unwrap_or(10)
            .clamp(10, 28);
        let val_w = w.saturating_sub(key_w + 18).clamp(8, 36);
        s.hits.clear();
        let mut cursor = None;
        let mut popup = None;
        for (n, (entry, part)) in lines.iter().enumerate().skip(s.top).take(h) {
            let y = list.y + (n - s.top) as u16;
            let row_area = Rect {
                y,
                height: 1,
                ..list
            };
            let Some(i) = *entry else {
                frame.render_widget(
                    Paragraph::new(Line::styled(
                        format!(" {part}"),
                        Style::new().fg(t.c(t.purple)).add_modifier(Modifier::BOLD),
                    )),
                    row_area,
                );
                continue;
            };
            s.hits.push((y, i));
            let row = &s.rows[s.shown[i]];
            let selected = i == s.selected;
            let bg = t.c(if selected { t.bg_sel } else { t.bg_popup });
            let base = Style::new().bg(bg);
            let (value, source) = row.effective();
            let line = if *part == "1" {
                let mark = if selected { "▸ " } else { "  " };
                let key = format!("{mark}{:<key_w$}  ", row.shown_key());
                let editing_here = selected && !matches!(s.edit, Edit::Browse);
                let val_text = match (&s.edit, editing_here) {
                    (Edit::Text { input, caret }, true) => {
                        let x = list.x + key.width() as u16 + 1;
                        let before: String = input.chars().take(*caret).collect();
                        cursor = Some(Position::new(
                            (x + before.width() as u16).min(list.right().saturating_sub(1)),
                            y,
                        ));
                        format!("[{input}]")
                    }
                    _ if row.is_family() => "add in the file".into(),
                    _ => value.map_or_else(|| "not set".into(), |v| show(v, row.kind)),
                };
                if editing_here && !matches!(s.edit, Edit::Text { .. }) {
                    popup = Some((list.x + key.width() as u16, y + 1));
                }
                let (src, src_c) = match source {
                    Source::Default => ("default", t.comment),
                    Source::User => ("settings.json", t.green),
                    Source::Project => ("project", t.orange),
                    Source::Unset => ("", t.comment),
                };
                let val_style = match source {
                    Source::Default | Source::Unset => Style::new().fg(t.c(t.fg_dim)),
                    _ => Style::new().fg(t.c(t.cyan)).add_modifier(Modifier::BOLD),
                };
                Line::from(vec![
                    Span::styled(
                        key,
                        Style::new().fg(t.c(if selected { t.blue } else { t.fg })),
                    ),
                    Span::styled(
                        format!("{:<val_w$}  ", crate::view::truncate(&val_text, val_w)),
                        val_style,
                    ),
                    Span::styled(src.to_owned(), Style::new().fg(t.c(src_c))),
                ])
            } else {
                let mut about = row.about.to_owned();
                // Both files set it: the project's wins here.
                if let (Some(u), Some(_)) = (&row.user, &row.project) {
                    about.push_str(&format!(" · yours: {}", show(u, row.kind)));
                }
                if source != Source::Default
                    && let Some(d) = &row.default
                {
                    about.push_str(&format!(" · default: {}", show(d, row.kind)));
                }
                Line::styled(
                    crate::view::truncate(&format!("    {about}"), w),
                    Style::new().fg(t.c(t.comment)),
                )
            };
            frame.render_widget(Paragraph::new(line).style(base), row_area);
        }
        if lines.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "  No setting matches; Esc clears the search",
                    Style::new().fg(t.c(t.comment)),
                )),
                Rect { height: 1, ..list },
            );
        }
        // The list of choices under the row being changed.
        if let Some((x, y)) = popup {
            let (items, at): (Vec<String>, usize) = match &s.edit {
                Edit::Choose { options, at } => {
                    (options.iter().map(|o| show(o, Kind::Str)).collect(), *at)
                }
                Edit::Theme { names, at, .. } => (names.clone(), *at),
                _ => (Vec::new(), 0),
            };
            let pw = (items.iter().map(|i| i.width()).max().unwrap_or(4) + 4) as u16;
            let room = inner.bottom().saturating_sub(y + 1);
            let ph = (items.len() as u16 + 2).min(room.max(3));
            let rect = Rect {
                x: x.min(inner.right().saturating_sub(pw)),
                y: if room >= ph {
                    y
                } else {
                    y.saturating_sub(ph + 1)
                },
                width: pw.min(inner.width),
                height: ph,
            };
            frame.render_widget(Clear, rect);
            let b = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(t.c(t.blue)))
                .style(Style::new().bg(t.c(t.bg_popup)));
            let pin = b.inner(rect);
            frame.render_widget(b, rect);
            let visible = pin.height as usize;
            let first = at.saturating_sub(visible.saturating_sub(1));
            for (k, item) in items.iter().enumerate().skip(first).take(visible) {
                let style = if k == at {
                    Style::new().bg(t.c(t.bg_sel)).fg(t.c(t.fg))
                } else {
                    Style::new().fg(t.c(t.fg_dim))
                };
                frame.render_widget(
                    Paragraph::new(format!(" {item:<w$} ", w = pin.width as usize - 2))
                        .style(style),
                    Rect {
                        y: pin.y + (k - first) as u16,
                        height: 1,
                        ..pin
                    },
                );
            }
        }
        let cursor = cursor.or_else(|| {
            matches!(s.edit, Edit::Browse).then(|| {
                let x = search.x + " Search ".width() as u16 + s.filter.width() as u16;
                Position::new(x.min(search.right().saturating_sub(1)), search.y)
            })
        });
        if let Some(c) = cursor {
            frame.set_cursor_position(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_cover_the_schema_and_say_where_values_come_from() {
        let rows = build_rows(
            Some("{\"tabsize\": 2, \"lsp.dart\": \"dart lsp\", \"[rust]\": {\"tabsize\": 8}}"),
            Some("{\"tabsize\": 3}"),
        );
        let row = |k: &str| rows.iter().find(|r| r.key == k).unwrap();
        assert_eq!(
            row("tabsize").effective(),
            (Some(&Value::from(3)), Source::Project)
        );
        assert_eq!(row("icons").effective().1, Source::Default);
        assert_eq!(row("lsp.dart").effective().1, Source::User);
        assert!(row("lsp.").is_family());
        // Every table key is listed, in group order.
        for k in schema::KEYS {
            assert!(rows.iter().any(|r| r.key == k.name), "{} missing", k.name);
        }
        let groups: Vec<_> = rows.iter().map(|r| r.group).collect();
        let mut sorted = groups.clone();
        sorted.sort_by_key(|g| schema::GROUPS.iter().position(|x| x == g));
        assert_eq!(groups, sorted);
    }

    #[test]
    fn typed_values_are_checked_for_their_kind() {
        assert_eq!(
            parse_input(Kind::Int { min: 1, max: 16 }, " 8 ").as_deref(),
            Ok("8")
        );
        assert!(parse_input(Kind::Int { min: 1, max: 16 }, "99").is_err());
        assert_eq!(
            parse_input(Kind::Columns, "80, 100").as_deref(),
            Ok("[80,100]")
        );
        assert_eq!(parse_input(Kind::Columns, "").as_deref(), Ok("[]"));
        assert_eq!(parse_input(Kind::Str, "x").as_deref(), Ok("\"x\""));
        assert_eq!(
            parse_input(Kind::Lsp, "gopls -remote=auto").as_deref(),
            Ok("\"gopls -remote=auto\"")
        );
        assert_eq!(parse_input(Kind::Lsp, "false").as_deref(), Ok("false"));
        assert!(parse_input(Kind::Remotes, "{oops").is_err());
    }
}
