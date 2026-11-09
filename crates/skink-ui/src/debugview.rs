//! The debug panel: a read-only tab docked on the right while debugging
//! (Alt-L toggles it). Sections: state, call stack, variables (expand with
//! Enter), watch, breakpoints, console. Rows know what they stand for so
//! the keys act on them.

use crate::App;
use crate::debug::{BpState, Run};
use crate::doc::DocKind;
use crate::docks::Dock;
use crate::gitui::Tint;
use crate::panes::{Axis, PaneId};
use skink_config::{Key, KeySpec};
use skink_dap::{BreakKind, Children, LineKind, Session, State, VarNode, WatchValue};
use std::path::PathBuf;

const TITLE: &str = "Debug";
/// What the editor panes keep (‰) when the panel docks.
const SHARE: u16 = 620;
/// Console lines shown.
const CONSOLE_LINES: usize = 200;

/// What a panel row stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DRow {
    None,
    Frame(usize),
    /// Index path into the session's scope tree.
    Var(Vec<usize>),
    Watch(usize),
    AddWatch,
    Breakpoint {
        path: PathBuf,
        line: usize,
    },
    Console,
}

/// Per line: char-column ranges and their colour.
pub type Tints = Vec<Vec<(usize, usize, Tint)>>;

pub struct DebugView {
    pub rows: Vec<DRow>,
    pub tints: Tints,
}

struct Builder {
    text: String,
    rows: Vec<DRow>,
    tints: Tints,
}

impl Builder {
    fn line(&mut self, parts: &[(&str, Option<Tint>)], row: DRow) {
        let mut col = 0;
        let mut tints = Vec::new();
        for (s, t) in parts {
            // One line per row: newlines in values would break the mapping.
            let s = s.replace(['\n', '\r'], " ");
            let n = s.chars().count();
            if let Some(t) = t
                && n > 0
            {
                tints.push((col, col + n, *t));
            }
            col += n;
            self.text.push_str(&s);
        }
        self.text.push('\n');
        self.rows.push(row);
        self.tints.push(tints);
    }

    fn head(&mut self, title: &str, hint: &str) {
        self.line(&[("", None)], DRow::None);
        self.line(
            &[(title, Some(Tint::Head)), (hint, Some(Tint::Dim))],
            DRow::None,
        );
    }
}

fn vars(b: &mut Builder, nodes: &[VarNode], path: &mut Vec<usize>, depth: usize) {
    for (i, n) in nodes.iter().enumerate() {
        path.push(i);
        let indent = "  ".repeat(depth + 1);
        let arrow = match n.children {
            Children::Leaf => "  ",
            Children::Collapsed => "\u{25b8} ",
            Children::Loading => "\u{2026} ",
            Children::Expanded(_) => "\u{25be} ",
        };
        let ty =
            n.ty.as_deref()
                .map(|t| format!("  {t}"))
                .unwrap_or_default();
        let value: String = n.value.chars().take(200).collect();
        if depth == 0 && n.value.is_empty() {
            b.line(
                &[
                    (&indent, None),
                    (arrow, Some(Tint::Dim)),
                    (&n.name, Some(Tint::Accent)),
                ],
                DRow::Var(path.clone()),
            );
        } else {
            b.line(
                &[
                    (&indent, None),
                    (arrow, Some(Tint::Dim)),
                    (&n.name, Some(Tint::Mod)),
                    (" = ", Some(Tint::Dim)),
                    (&value, None),
                    (&ty, Some(Tint::Dim)),
                ],
                DRow::Var(path.clone()),
            );
        }
        if let Children::Expanded(kids) = &n.children {
            vars(b, kids, path, depth + 1);
        }
        path.pop();
    }
}

impl App {
    fn build_debug_panel(&self) -> (String, DebugView) {
        let mut b = Builder {
            text: String::new(),
            rows: Vec::new(),
            tints: Vec::new(),
        };
        let session: Option<&Session> = self.debug.session();
        let (state, tint) = match (&self.debug.run, session.map(Session::state)) {
            (Run::Idle, _) => (
                "Not debugging (F5 starts, :debug picks a configuration)".to_owned(),
                Tint::Dim,
            ),
            (Run::Building { task, .. }, _) => (format!("Building: {task}"), Tint::Mod),
            (_, Some(State::Starting(_))) => ("Starting".into(), Tint::Mod),
            (_, Some(State::Running)) => ("Running".into(), Tint::Add),
            (_, Some(State::Stopped(s))) => (
                format!(
                    "Paused: {}{}",
                    s.reason,
                    s.text
                        .as_deref()
                        .map(|t| format!(" ({t})"))
                        .unwrap_or_default()
                ),
                Tint::Warn,
            ),
            (_, Some(State::Exited(Some(c)))) => (format!("Exited with code {c}"), Tint::Dim),
            (_, Some(State::Exited(None))) => ("Ended".into(), Tint::Dim),
            (_, Some(State::Failed(e))) => (format!("Failed: {e}"), Tint::Del),
            (_, None) => (String::new(), Tint::Dim),
        };
        let name = session.map(|s| format!("{}  ", s.name)).unwrap_or_default();
        b.line(
            &[
                (" ", None),
                (&name, Some(Tint::Accent)),
                (&state, Some(tint)),
            ],
            DRow::None,
        );
        b.line(
            &[(
                " F5 continue · F10 over · F11 into · Shift-F11 out · F6 pause · Shift-F5 stop",
                Some(Tint::Dim),
            )],
            DRow::None,
        );
        if let Some(s) = session {
            if !s.frames.is_empty() {
                b.head(" CALL STACK", "   Enter shows the frame");
                for (i, f) in s.frames.iter().enumerate() {
                    let mark = if i == s.frame { " \u{2192} " } else { "   " };
                    let place = match f.path() {
                        Some(p) => format!(
                            "  {}:{}",
                            std::path::Path::new(p)
                                .file_name()
                                .map_or_else(|| p.into(), |n| n.to_string_lossy()),
                            f.line
                        ),
                        None => "  (no source)".into(),
                    };
                    b.line(
                        &[
                            (mark, Some(Tint::Warn)),
                            (
                                &f.name,
                                Some(if i == s.frame {
                                    Tint::Head
                                } else {
                                    Tint::Accent
                                }),
                            ),
                            (&place, Some(Tint::Dim)),
                        ],
                        DRow::Frame(i),
                    );
                }
            }
            if !s.scopes.is_empty() {
                b.head(" VARIABLES", "   Enter expands");
                vars(&mut b, &s.scopes, &mut Vec::new(), 0);
            }
        }
        b.head(" WATCH", "   w adds · d removes");
        let values: Vec<WatchValue> = session
            .map(|s| s.watches.iter().map(|w| w.value.clone()).collect())
            .unwrap_or_default();
        for (i, w) in self.debug.watches.iter().enumerate() {
            let (v, t) = match values.get(i) {
                Some(WatchValue::Value(v)) => (v.clone(), None),
                Some(WatchValue::Error(e)) => (e.clone(), Some(Tint::Del)),
                Some(WatchValue::Pending) => ("\u{2026}".into(), Some(Tint::Dim)),
                Some(WatchValue::Unknown) | None => ("(not paused)".into(), Some(Tint::Dim)),
            };
            b.line(
                &[
                    ("   ", None),
                    (w, Some(Tint::Mod)),
                    (" = ", Some(Tint::Dim)),
                    (&v, t),
                ],
                DRow::Watch(i),
            );
        }
        b.line(&[("   + add a watch", Some(Tint::Dim))], DRow::AddWatch);
        b.head(
            " BREAKPOINTS",
            "   F9 toggles · Alt-F9 condition · d removes",
        );
        let root = self.project_root();
        let mut any = false;
        for f in &self.debug.files {
            for bp in &f.list {
                any = true;
                let (glyph, t) = match (&bp.state, &bp.kind) {
                    (BpState::Rejected(_), _) => (self.icons.breakpoint_off(), Tint::Dim),
                    (_, BreakKind::Log(_)) => (self.icons.logpoint(), Tint::Accent),
                    (_, BreakKind::Condition(_) | BreakKind::HitCount(_)) => {
                        (self.icons.breakpoint_cond(), Tint::Del)
                    }
                    (_, BreakKind::Plain) => (self.icons.breakpoint(), Tint::Del),
                };
                let rel = f.path.strip_prefix(&root).unwrap_or(&f.path);
                let place = format!(" {}:{}", rel.display(), bp.line + 1);
                let extra = match (&bp.kind, &bp.state) {
                    (_, BpState::Rejected(w)) => format!("  {w}"),
                    (BreakKind::Condition(c), _) => format!("  if {c}"),
                    (BreakKind::HitCount(h), _) => format!("  hits {h}"),
                    (BreakKind::Log(m), _) => format!("  log {m}"),
                    (BreakKind::Plain, _) => String::new(),
                };
                b.line(
                    &[
                        ("   ", None),
                        (glyph, Some(t)),
                        (&place, None),
                        (&extra, Some(Tint::Dim)),
                    ],
                    DRow::Breakpoint {
                        path: f.path.clone(),
                        line: bp.line,
                    },
                );
            }
        }
        if !any {
            b.line(&[("   none", Some(Tint::Dim))], DRow::None);
        }
        b.head(" CONSOLE", "   e evaluates");
        if let Some(s) = session {
            let skip = s.console.len().saturating_sub(CONSOLE_LINES);
            for l in s.console.iter().skip(skip) {
                let t = match l.kind {
                    LineKind::Output => None,
                    LineKind::Stderr | LineKind::Error => Some(Tint::Del),
                    LineKind::Input => Some(Tint::Accent),
                    LineKind::Result => Some(Tint::Add),
                    LineKind::Info => Some(Tint::Dim),
                };
                b.line(&[("   ", None), (&l.text, t)], DRow::Console);
            }
        }
        b.line(&[("   > evaluate\u{2026}", Some(Tint::Dim))], DRow::Console);
        (
            b.text,
            DebugView {
                rows: b.rows,
                tints: b.tints,
            },
        )
    }

    fn debug_panel_doc(&self) -> Option<usize> {
        self.find_doc(|d| matches!(d.kind, DocKind::Debug(_)))
    }

    /// The dock's pane, if still there.
    fn debug_pane(&self) -> Option<PaneId> {
        match self.debug.panel {
            Dock::Open(id) if self.panes.pane(id).is_some() => Some(id),
            _ => None,
        }
    }

    /// Rebuild the panel (if open) from the session.
    pub(crate) fn refresh_debug_panel(&mut self) {
        let Some(i) = self.debug_panel_doc() else {
            return;
        };
        let (text, view) = self.build_debug_panel();
        if let Some(d) = self.doc_at_mut(i) {
            let (line, scroll) = (d.editor.cursor.line, d.editor.scroll);
            d.replace_view(TITLE, &text);
            d.editor.goto(Some(line + 1), None);
            d.editor.scroll = scroll.min(d.editor.buffer.len_lines().saturating_sub(1));
            d.kind = DocKind::Debug(Box::new(view));
        }
    }

    /// Show the panel docked on the right; the focus stays where it was.
    pub fn open_debug_panel(&mut self) {
        if self.debug_pane().is_some() {
            return self.refresh_debug_panel();
        }
        let (text, view) = self.build_debug_panel();
        let back = self.panes.focused();
        let id = self.panes.dock(Axis::Columns, SHARE, self.cur_doc);
        self.debug.panel = Dock::Open(id);
        self.focus_pane(id);
        self.open_view(TITLE, &text);
        self.doc.kind = DocKind::Debug(Box::new(view));
        self.focus_pane(back);
    }

    /// Alt-L: the panel on or off.
    pub fn toggle_debug_panel(&mut self) {
        match self.debug_pane() {
            Some(id) => {
                self.debug.panel = Dock::Closed;
                let back = self.panes.focused();
                if let Some((tab, _)) = self.panes.close(id) {
                    self.switch_to(tab);
                }
                if back != id {
                    self.focus_pane(back);
                }
            }
            None => self.open_debug_panel(),
        }
    }

    /// Before jumping to source: move the focus off the panel's pane.
    pub(crate) fn leave_debug_panel(&mut self) {
        let Some(id) = self.debug_pane() else {
            if matches!(self.doc.kind, DocKind::Debug(_)) {
                // The panel is a plain tab here: go back to a file.
                let prev = (0..self.tabs.len())
                    .find(|&i| self.doc_at(i).is_some_and(|d| d.canon.is_some()));
                if let Some(i) = prev {
                    self.switch_to(i);
                }
            }
            return;
        };
        if self.panes.focused() == id {
            let other = self.panes.cycle(1);
            if other != id {
                self.focus_pane(other);
            }
        }
    }

    /// Styles of panel lines `first..last`.
    pub(crate) fn debug_view_tints(&self) -> Option<&Tints> {
        match &self.doc.kind {
            DocKind::Debug(v) => Some(&v.tints),
            _ => None,
        }
    }

    /// Plain keys in the panel. True when consumed.
    pub(crate) fn debug_view_key(&mut self, k: &KeySpec) -> bool {
        let DocKind::Debug(v) = &self.doc.kind else {
            return false;
        };
        if k.ctrl || k.alt {
            return false;
        }
        let row = v
            .rows
            .get(self.doc.editor.cursor.line)
            .cloned()
            .unwrap_or(DRow::None);
        let c = match k.key {
            Key::Enter => '\n',
            Key::Char(c) => c,
            Key::Delete | Key::Backspace => 'd',
            Key::Right => '>',
            Key::Left => '<',
            _ => return false,
        };
        match (c, row) {
            ('\n', DRow::Frame(i)) => {
                if let Some(s) = self.debug.session_mut() {
                    let n = s.select_frame(i);
                    self.handle_debug_notices(n);
                }
            }
            ('\n' | ' ' | '>' | '<', DRow::Var(path)) => {
                if let Some(s) = self.debug.session_mut() {
                    s.toggle(&path);
                }
            }
            ('\n', DRow::Breakpoint { path, line }) => {
                self.leave_debug_panel();
                self.jump(&path, line, 0);
                return true;
            }
            ('d', DRow::Breakpoint { path, line }) => {
                self.remove_breakpoint(&path, line);
            }
            ('d', DRow::Watch(i)) => self.remove_watch(i),
            ('w', _) | ('\n', DRow::AddWatch) => {
                self.open_prompt(crate::prompt::PromptKind::Command, ":", "debug watch ");
                return true;
            }
            ('e', _) | ('\n', DRow::Console) => {
                self.open_prompt(crate::prompt::PromptKind::Command, ":", "debug eval ");
                return true;
            }
            _ => return false,
        }
        self.refresh_debug_panel();
        true
    }
}
