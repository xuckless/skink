//! Split panes in the app: actions, drawing the unfocused panes, mouse.

use crate::panes::{Axis, Divider, Drag, PaneId, PaneRect, PaneView, RESIZE_STEP};
use crate::{App, Tab, termview};
use ratatui::{
    Frame,
    crossterm::event::{MouseButton, MouseEvent, MouseEventKind},
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// Which pane the editor is being drawn for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Focused,
    Other,
}

/// Pane rectangles and dividers from the last frame.
#[derive(Debug, Clone, Default)]
pub struct PaneLayout {
    /// The whole area the panes share.
    pub area: Rect,
    pub rects: Vec<PaneRect>,
    pub dividers: Vec<Divider>,
}

impl App {
    /// What the focused pane should remember when it loses the focus.
    fn leaving_view(&self) -> PaneView {
        if self.active_is_term() {
            PaneView::Shared
        } else {
            PaneView::Remembered(self.doc.editor.view())
        }
    }

    fn apply_pane_view(&mut self, view: PaneView) {
        if let PaneView::Remembered(v) = view
            && !self.active_is_term()
        {
            self.doc.editor.set_view(v);
        }
        self.reveal_in_focused_pane();
    }

    /// The focused pane changed size or identity: size the editor to it and
    /// bring the cursor into view (its remembered scroll was for another
    /// height).
    fn reveal_in_focused_pane(&mut self) {
        if self.active_is_term() || self.pane_layout.area.height == 0 {
            return;
        }
        let (rects, _) = self.panes.layout(self.pane_layout.area);
        if let Some(r) = rects.iter().find(|r| r.id == self.panes.focused()) {
            self.areas.main = r.content;
            self.doc
                .editor
                .set_viewport_height(r.content.height as usize);
            self.doc.editor.scroll_into_view();
        }
    }

    /// Split the focused pane; the new pane shows the same document with its
    /// own cursor. A terminal cannot show in two panes, so splitting one
    /// puts the current document in the new pane.
    pub fn split(&mut self, axis: Axis) {
        let tab = if self.active_is_term() {
            self.cur_doc
        } else {
            self.active
        };
        let leaving = self.leaving_view();
        self.panes.split(axis, tab, leaving);
        if tab != self.active {
            self.switch_to(tab);
        }
        self.reveal_in_focused_pane();
        self.doc.editor.message =
            Some("Split: Ctrl-W next pane, Alt-Q close it, Alt-= / Alt-- resize".into());
    }

    pub fn focus_pane(&mut self, id: PaneId) {
        let leaving = self.leaving_view();
        if let Some((tab, view)) = self.panes.focus(id, leaving) {
            self.switch_to(tab);
            self.apply_pane_view(view);
        }
    }

    pub fn next_split(&mut self, delta: isize) {
        if !self.panes.is_split() {
            self.doc.editor.message = Some("Only one pane (Ctrl-\\ splits)".into());
            return;
        }
        let id = self.panes.cycle(delta);
        self.focus_pane(id);
    }

    /// Close the focused pane (its tab stays open).
    pub fn unsplit(&mut self) {
        match self.panes.close_focused() {
            Some((tab, view)) => {
                self.switch_to(tab);
                self.apply_pane_view(view);
            }
            None => self.doc.editor.message = Some("Only one pane".into()),
        }
    }

    /// Keep only the focused pane.
    pub fn only_pane(&mut self) {
        self.panes.close_others();
    }

    pub fn resize_split(&mut self, grow: bool) {
        self.panes
            .resize_focused(if grow { RESIZE_STEP } else { -RESIZE_STEP });
    }

    /// Swap document tab `idx` in as the live document without any of
    /// [`App::switch_to`]'s side effects (drawing another pane).
    fn swap_live(&mut self, idx: usize) {
        if idx == self.cur_doc {
            return;
        }
        let Some(Tab::Doc(Some(_))) = self.tabs.get(idx) else {
            return;
        };
        let Tab::Doc(Some(mut d)) = std::mem::replace(&mut self.tabs[idx], Tab::Doc(None)) else {
            unreachable!("checked above")
        };
        std::mem::swap(&mut self.doc, &mut d);
        self.tabs[self.cur_doc] = Tab::Doc(Some(d));
        self.cur_doc = idx;
    }

    /// Draw every pane but the focused one, plus titles and dividers.
    pub(crate) fn render_other_panes(&mut self, frame: &mut Frame, layout: &PaneLayout) {
        let focused = self.panes.focused();
        for r in &layout.rects {
            if r.id == focused {
                continue;
            }
            let Some(pane) = self.panes.pane(r.id).cloned() else {
                continue;
            };
            match self.tabs.get(pane.tab) {
                Some(Tab::Term(_)) if pane.tab == self.active => {
                    self.render_pane_note(frame, r.content, "terminal shown in the focused pane");
                }
                Some(Tab::Term(_)) => {
                    let t = self.theme;
                    if let Some(Tab::Term(p)) = self.tabs.get_mut(pane.tab) {
                        let area = r.content;
                        if p.term.size() != (area.height, area.width)
                            && area.height > 0
                            && area.width > 0
                        {
                            let _ = p.term.resize(area.height, area.width);
                        }
                        p.term.snapshot_into(&mut p.screen);
                        termview::render(frame.buffer_mut(), area, &p.screen, &t);
                    }
                }
                Some(Tab::Doc(_)) => {
                    let prev_cur = self.cur_doc;
                    self.swap_live(pane.tab);
                    let own = self.doc.editor.view();
                    let remembered = matches!(pane.view, PaneView::Remembered(_));
                    if let PaneView::Remembered(v) = pane.view {
                        self.doc.editor.set_view(v);
                    }
                    self.pass = Pass::Other;
                    let diags = self.diagnostics();
                    self.render_editor(frame, r.content, &diags);
                    self.pass = Pass::Focused;
                    if remembered {
                        let after = self.doc.editor.view();
                        self.doc.editor.set_view(own);
                        self.panes.remember(r.id, after);
                    }
                    self.swap_live(prev_cur);
                }
                None => {}
            }
        }
        self.render_pane_chrome(frame, layout);
    }

    fn render_pane_note(&self, frame: &mut Frame, area: Rect, note: &str) {
        let t = &self.theme;
        frame.render_widget(
            Paragraph::new(format!(" {note}")).style(Style::new().fg(t.c(t.comment))),
            area,
        );
    }

    /// Title rows (stacked or side-by-side panes) and column dividers.
    fn render_pane_chrome(&self, frame: &mut Frame, layout: &PaneLayout) {
        let t = &self.theme;
        let focused = self.panes.focused();
        for r in &layout.rects {
            if r.outer.height == r.content.height {
                continue;
            }
            let Some(pane) = self.panes.pane(r.id) else {
                continue;
            };
            let title = match self.tabs.get(pane.tab) {
                Some(Tab::Term(p)) => p.title().to_owned(),
                Some(Tab::Doc(_)) => self.doc_at(pane.tab).map(|d| d.title()).unwrap_or_default(),
                None => String::new(),
            };
            let dirty = self
                .doc_at(pane.tab)
                .is_some_and(|d| d.editor.buffer.is_dirty());
            let style = if r.id == focused {
                Style::new()
                    .fg(t.c(t.blue))
                    .bg(t.c(t.bg_dark))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(t.c(t.fg_dim)).bg(t.c(t.bg_dark))
            };
            let row = Rect {
                height: 1,
                ..r.outer
            };
            let mut spans = vec![Span::styled(format!(" {title}"), style)];
            if dirty {
                spans.push(Span::styled(
                    format!(" {}", self.icons.dirty()),
                    Style::new().fg(t.c(t.yellow)).bg(t.c(t.bg_dark)),
                ));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)).style(Style::new().bg(t.c(t.bg_dark))),
                row,
            );
        }
        for d in &layout.dividers {
            if d.axis != Axis::Columns {
                continue;
            }
            let buf = frame.buffer_mut();
            for y in d.grip.y..d.grip.bottom() {
                if let Some(cell) = buf.cell_mut(Position::new(d.grip.x, y)) {
                    cell.set_symbol("\u{2502}")
                        .set_style(Style::new().fg(t.c(t.guide)).bg(t.c(t.bg)));
                }
            }
        }
    }

    /// Mouse on pane boundaries and unfocused panes. True when consumed.
    pub(crate) fn pane_mouse(&mut self, m: MouseEvent) -> bool {
        if !self.panes.is_split() {
            return false;
        }
        let pos = Position::new(m.column, m.row);
        match (&self.panes.drag, m.kind) {
            (Drag::Resizing { path }, MouseEventKind::Drag(MouseButton::Left)) => {
                let path = path.clone();
                if let Some(d) = self.pane_layout.dividers.iter().find(|d| d.path == path) {
                    let node = d.node;
                    self.panes.drag_to(&path, node, m.column, m.row);
                }
                return true;
            }
            (Drag::Resizing { .. }, _) => {
                self.panes.drag = Drag::Idle;
                return true;
            }
            (Drag::Idle, _) => {}
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind
            && let Some(d) = self
                .pane_layout
                .dividers
                .iter()
                .find(|d| d.grip.contains(pos))
        {
            self.panes.drag = Drag::Resizing {
                path: d.path.clone(),
            };
            return true;
        }
        // A click (or wheel) in another pane focuses it first.
        let hit = self
            .pane_layout
            .rects
            .iter()
            .find(|r| r.outer.contains(pos))
            .copied();
        if let Some(r) = hit
            && r.id != self.panes.focused()
            && matches!(
                m.kind,
                MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            )
        {
            self.focus_pane(r.id);
            self.areas.main = r.content;
            return !r.content.contains(pos);
        }
        false
    }
}
