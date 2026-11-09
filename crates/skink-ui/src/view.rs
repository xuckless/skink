//! All drawing: tab bar, file tree, editor, popups, terminal tab, status line.
//! Layout and look follow a Tokyo-Night-style editor: rounded pills, powerline
//! separators and Nerd Font icons (with an ASCII fallback).
//!
//! Rendering only *reads* app state, apart from layout bookkeeping that the
//! next input event needs (areas, tab hit boxes, horizontal scroll) and the
//! lazily updated syntax tree.

use crate::{
    App, Areas, Focus, Overlay, SidebarView, Tab, TabHit, icons, lsp, picker, prompt::PromptKind,
    termview,
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use skink_core::{Backend, Cursor};
use skink_lsp::Severity;
use std::{collections::HashMap, path::Path};
use unicode_width::UnicodeWidthStr;

type Rgb = (u8, u8, u8);

/// Diagnostics of the live document, gathered once per frame.
#[derive(Default)]
pub(crate) struct Diags {
    /// Worst severity per 0-based line.
    lines: HashMap<usize, Severity>,
    errors: usize,
    warnings: usize,
    /// The most severe diagnostic on the cursor line.
    here: Option<(Severity, String)>,
}

struct TabLabel {
    icon: String,
    title: String,
    marker: Option<(&'static str, Rgb)>,
    is_term: bool,
}

impl App {
    fn st(&self, fg: Rgb) -> Style {
        Style::new().fg(self.theme.c(fg))
    }

    fn pill(&self, text: &str, fg: Rgb, bg: Rgb, bar: Rgb) -> Vec<Span<'static>> {
        let t = &self.theme;
        let edge = Style::new().fg(t.c(bg)).bg(t.c(bar));
        vec![
            Span::styled(self.icons.pill_left(), edge),
            Span::styled(
                format!(" {text} "),
                Style::new()
                    .fg(t.c(fg))
                    .bg(t.c(bg))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(self.icons.pill_right(), edge),
        ]
    }

    pub(crate) fn gutter_width(&self) -> usize {
        self.doc.editor.buffer.len_lines().to_string().len() + 2
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let full = frame.area();
        let t = self.theme;
        frame.render_widget(
            Block::new().style(Style::new().bg(t.c(t.bg)).fg(t.c(t.fg))),
            full,
        );

        let zoomed = self.view_mode == crate::zoom::ViewMode::Zoomed;
        // Zoomed: only the main area; the status line comes back while a
        // prompt or a message needs it.
        let status_h = u16::from(
            !zoomed || !matches!(self.overlay, Overlay::None) || self.doc.editor.message.is_some(),
        );
        let [top, body, status] = Layout::vertical([
            Constraint::Length(u16::from(!zoomed)),
            Constraint::Min(1),
            Constraint::Length(status_h),
        ])
        .areas(full);
        let wants_tree = matches!(
            self.sidebar_view(),
            Some(SidebarView::Shown | SidebarView::Focused)
        );
        let show_tree = wants_tree && !zoomed && body.width >= crate::sidebar::HIDE_BELOW;
        if wants_tree && !show_tree {
            // Not drawn (narrow window, zoomed): keys must not go to an
            // invisible tree.
            self.focus_main();
        }
        let (tree, main) = if show_tree {
            let w = self
                .sidebar
                .as_ref()
                .map_or(22, |s| s.width.columns(body.width));
            let [a, b] =
                Layout::horizontal([Constraint::Length(w), Constraint::Min(1)]).areas(body);
            (a, b)
        } else {
            (Rect::default(), body)
        };
        // Split panes: the focused one is drawn below like the single view;
        // the others first, so the focused cursor is the one that shows.
        let (rects, dividers) = self.panes.layout(main);
        let layout = crate::splits::PaneLayout {
            area: main,
            rects,
            dividers,
        };
        let main = layout
            .rects
            .iter()
            .find(|r| r.id == self.panes.focused())
            .map_or(main, |r| r.content);
        self.areas = Areas { top, tree, main };
        self.refresh_preview(main.width, main.height);

        // Size and snapshot the active terminal first: its title feeds the
        // tab bar drawn below.
        if let Some(pane) = self.active_term() {
            if pane.term.size() != (main.height, main.width) && main.height > 0 && main.width > 0 {
                let _ = pane.term.resize(main.height, main.width);
            }
            pane.term.snapshot_into(&mut pane.screen);
        }

        if top.height > 0 {
            self.render_tab_bar(frame, top);
        }
        if show_tree {
            self.render_tree(frame, tree);
        }
        if self.panes.is_split() {
            self.render_other_panes(frame, &layout);
        }
        self.pane_layout = layout;
        let mut anchor = None;
        if self.active_is_term() {
            self.render_terminal(frame, main);
            if status.height > 0 {
                self.render_term_status(frame, status);
            }
        } else if let Some(img) = self.svg_preview() {
            crate::preview::draw_svg(frame.buffer_mut(), main, img, &self.theme);
            if status.height > 0 {
                let diags = Diags::default();
                self.render_doc_status(frame, status, &diags, &[]);
            }
        } else {
            let diags = self.diagnostics();
            let cursor = self.render_editor(frame, main, &diags);
            if status.height > 0 {
                let crumbs = self.doc.scopes();
                self.render_doc_status(frame, status, &diags, &crumbs);
            }
            anchor = cursor;
            if let Some(c) = cursor {
                match &self.intel.popup {
                    crate::intel::Popup::Closed => self.render_diag_popup(frame, main, c, &diags),
                    _ => self.render_intel_popup(frame, main, c),
                }
            }
        }
        self.render_prompt(frame, status);
        self.render_picker(frame, full);
        self.render_ai(frame, main, anchor);
    }

    pub(crate) fn diagnostics(&self) -> Diags {
        let (Some(link), Some(path)) = (&self.lsp, &self.doc.canon) else {
            return Diags::default();
        };
        let all = link.diagnostics(path);
        let (lines, errors, warnings) = lsp::summarize(&all);
        let cur = self.doc.editor.cursor.line;
        let here = all
            .iter()
            .filter(|d| d.range.start.line == cur)
            .min_by_key(|d| d.severity as u8)
            .map(|d| (d.severity, d.message.clone()));
        Diags {
            lines,
            errors,
            warnings,
            here,
        }
    }

    // ---- tab bar -------------------------------------------------------------

    fn tab_label(&self, i: usize) -> TabLabel {
        let t = &self.theme;
        match &self.tabs[i] {
            Tab::Term(p) => TabLabel {
                icon: self.icons.terminal().to_owned(),
                title: p.title().to_owned(),
                marker: if p.exited {
                    Some(("exited", t.red))
                } else if p.activity {
                    Some(("\u{2022}", t.green))
                } else {
                    None
                },
                is_term: true,
            },
            Tab::Doc(_) => {
                let d = self.doc_at(i).expect("document tab");
                let icon = match (d.editor.buffer.path(), self.icons) {
                    (Some(p), icons::IconSet::Nerd) => self.icons.file(p).to_owned(),
                    (None, icons::IconSet::Nerd) => "\u{f15b}".to_owned(),
                    _ => String::new(),
                };
                TabLabel {
                    icon,
                    title: d.title(),
                    marker: d
                        .editor
                        .buffer
                        .is_dirty()
                        .then_some((self.icons.dirty(), t.yellow)),
                    is_term: false,
                }
            }
        }
    }

    /// Ghostty-style tab bar: equal-width tabs, the active one filled, a
    /// close button on it, and activity/unsaved markers.
    fn render_tab_bar(&mut self, frame: &mut Frame, area: Rect) {
        const MIN_W: u16 = 14;
        const MAX_W: u16 = 28;
        let t = self.theme;
        self.tab_hits.clear();
        let n = self.tabs.len();
        let fit = ((area.width / MIN_W) as usize).max(1);
        // Scroll so the active tab is always visible.
        let start = if n > fit {
            (self.active + 1).saturating_sub(fit)
        } else {
            0
        };
        let shown = (n - start).min(fit);
        let w = (area.width / shown as u16).clamp(MIN_W.min(area.width), MAX_W);
        let close_glyph = if self.icons == icons::IconSet::Nerd {
            " \u{00d7}"
        } else {
            " x"
        };
        let mut spans: Vec<Span> = Vec::new();
        let mut x = area.x;
        for i in start..start + shown {
            let label = self.tab_label(i);
            let active = i == self.active;
            let accent = if label.is_term { t.green } else { t.blue };
            let (fg, bg) = if active {
                (t.bg_dark, accent)
            } else {
                (t.fg_dim, t.bg_dark)
            };
            let mut st = Style::new().fg(t.c(fg)).bg(t.c(bg));
            if active {
                st = st.add_modifier(Modifier::BOLD);
            }
            // [ icon title ........ marker close ]
            let close = if active && n > 1 { close_glyph } else { "" };
            let marker = label.marker.map(|(m, c)| (format!(" {m}"), c));
            let tail_w = marker.as_ref().map_or(0, |(m, _)| m.width()) + close.width();
            let head = if label.icon.is_empty() {
                format!(" {}", label.title)
            } else {
                format!(" {} {}", label.icon, label.title)
            };
            // A thin separator ends each inactive tab (none next to the
            // filled active tab), so a marker clearly belongs to its tab.
            let sep = !active && i + 1 != self.active && i + 1 < start + shown;
            let sep_w = if sep {
                1 + usize::from(marker.is_some())
            } else {
                0
            };
            let tail_w = tail_w + sep_w;
            let room = (w as usize).saturating_sub(tail_w + 1);
            let head = truncate(&head, room);
            let pad = (w as usize).saturating_sub(head.width() + tail_w);
            spans.push(Span::styled(head, st));
            spans.push(Span::styled(" ".repeat(pad), st));
            if let Some((m, c)) = marker {
                let ms = if active {
                    st
                } else {
                    Style::new().fg(t.c(c)).bg(t.c(bg))
                };
                spans.push(Span::styled(m, ms));
                if sep {
                    spans.push(Span::styled(" ", st));
                }
            }
            if !close.is_empty() {
                spans.push(Span::styled(close, st));
            }
            if sep {
                spans.push(Span::styled(
                    "\u{2502}",
                    Style::new().fg(t.c(t.guide)).bg(t.c(bg)),
                ));
            }
            self.tab_hits.push(TabHit {
                x0: x,
                x1: x + w,
                tab: i,
                close_x: (!close.is_empty()).then(|| x + w - 1),
            });
            x += w;
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::new().bg(t.c(t.bg_dark))),
            area,
        );
    }

    // ---- file tree -------------------------------------------------------------

    fn render_tree(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme;
        let focused = self.focus() == Focus::Tree;
        let icons = self.icons;
        let rows = area.height.saturating_sub(2) as usize;
        let current = self.doc.canon.clone();
        let Some(tree) = self.tree_mut() else { return };
        tree.keep_visible(rows);
        let w = area.width.saturating_sub(1) as usize;
        // Sideways scroll stops once the widest visible row fits.
        let widest = tree
            .nodes
            .iter()
            .skip(tree.scroll)
            .take(rows)
            .map(|n| 2 * n.depth + 3 + n.name.width())
            .max()
            .unwrap_or(0);
        tree.hscroll = tree.hscroll.min(widest.saturating_sub(w.saturating_sub(2)));
        let hs = tree.hscroll;
        let dim = Style::new().fg(t.c(t.comment)).bg(t.c(t.bg_dark));
        // The focused pane wears the accent: header, border, selection.
        let header = if focused {
            Style::new()
                .fg(t.c(t.blue))
                .bg(t.c(t.bg_dark))
                .add_modifier(Modifier::BOLD)
        } else {
            dim
        };

        let mut lines: Vec<Line> = vec![match &tree.mode {
            crate::tree::TreeMode::Browse => Line::styled(
                if focused {
                    " Files  (focused)"
                } else {
                    " Files"
                },
                header,
            ),
            crate::tree::TreeMode::Filter { query } => Line::from(vec![
                Span::styled(" Filter: ", dim),
                Span::styled(
                    query.clone(),
                    Style::new().fg(t.c(t.yellow)).bg(t.c(t.bg_dark)),
                ),
                Span::styled("  Esc clears", dim),
            ]),
        }];
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", icons.folder(true)),
                Style::new().fg(t.c(t.blue)),
            ),
            Span::styled(
                home_relative(&tree.root),
                Style::new().fg(t.c(t.blue)).add_modifier(Modifier::BOLD),
            ),
        ]));
        let end = (tree.scroll + rows).min(tree.nodes.len());
        for (i, n) in tree.nodes.iter().enumerate().take(end).skip(tree.scroll) {
            let is_current = current.as_ref() == Some(&n.path);
            let icon = if let crate::tree::NodeKind::Dir { open } = n.kind {
                icons.folder(open)
            } else {
                icons.file(&n.path)
            };
            let selected = i == tree.selected;
            let row_bg = match (selected, focused) {
                (true, true) => t.blue,
                (true, false) => t.bg_hl,
                _ => t.bg_dark,
            };
            let indent = "  ".repeat(n.depth);
            let used = 1 + indent.len() + icon.width() + 1 + n.name.width();
            let name_fg = match (selected && focused, is_current, n.ignored) {
                (true, ..) => t.bg_dark,
                (false, true, _) => t.blue,
                (false, false, true) => t.comment,
                (false, false, false) => t.fg_dim,
            };
            let mut name_style = Style::new().fg(t.c(name_fg)).bg(t.c(row_bg));
            if is_current {
                name_style = name_style.add_modifier(Modifier::BOLD);
            }
            lines.push(Line::from(skip_cols(
                vec![
                    Span::styled(format!(" {indent}"), Style::new().bg(t.c(row_bg))),
                    Span::styled(
                        format!("{icon} "),
                        Style::new()
                            .fg(t.c(match (selected && focused, n.is_dir()) {
                                (true, _) => t.bg_dark,
                                (false, true) => t.blue,
                                (false, false) => t.cyan,
                            }))
                            .bg(t.c(row_bg)),
                    ),
                    Span::styled(n.name.clone(), name_style),
                    Span::styled(
                        " ".repeat((w + hs).saturating_sub(used)),
                        Style::new().bg(t.c(row_bg)),
                    ),
                ],
                hs,
            )));
        }
        if tree.hidden_at_root > 0 && end == tree.nodes.len() {
            lines.push(Line::styled(
                format!(" {{{} hidden items}}", tree.hidden_at_root),
                dim,
            ));
        }
        let inner = Rect {
            width: area.width.saturating_sub(1),
            ..area
        };
        frame.render_widget(
            Paragraph::new(lines).style(Style::new().bg(t.c(t.bg_dark)).fg(t.c(t.fg_dim))),
            inner,
        );
        // Right border: accent while the tree has the keys.
        let sep = Style::new()
            .fg(t.c(if focused { t.blue } else { t.guide }))
            .bg(t.c(t.bg_dark));
        let x = area.right().saturating_sub(1);
        for y in area.y..area.bottom() {
            frame.buffer_mut()[(x, y)]
                .set_symbol("\u{2502}")
                .set_style(sep);
        }
    }

    // ---- editor ----------------------------------------------------------------

    /// Draw the live document; returns the cursor's screen position.
    pub(crate) fn render_editor(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        diags: &Diags,
    ) -> Option<(u16, u16)> {
        let t = self.theme;
        self.doc.editor.set_viewport_height(area.height as usize);
        let gutter = self.gutter_width();
        let width = (area.width as usize).saturating_sub(gutter);
        let focused = self.editor_focused() && matches!(self.overlay, Overlay::None);
        let show_guides = self.settings.indent_guides;

        // Buffer lines on screen, top to bottom (closed folds skipped).
        let rows = self.doc.editor.folds().visible_from(
            self.doc.editor.scroll,
            area.height as usize,
            self.doc.editor.buffer.len_lines() - 1,
        );
        let first = rows.first().copied().unwrap_or(self.doc.editor.scroll);
        let last = rows.last().map_or(first, |l| l + 1);
        let sticky = self.sticky_lines(&rows, area.height as usize);
        let sticky_syntax: Vec<_> = sticky
            .iter()
            .map(|&h| self.syntax_styles(h, h + 1).pop().unwrap_or_default())
            .collect();

        // Horizontal scroll follows the cursor unless the user scrolled
        // sideways and has not moved the cursor since.
        let ed = &self.doc.editor;
        let tab = ed.tab_width;
        let cursor_x =
            crate::text::display_col(&ed.buffer.line_text(ed.cursor.line), ed.cursor.col, tab);
        let widest = (first..last)
            .map(|l| {
                let text = ed.buffer.line_text(l);
                crate::text::display_col(&text, text.chars().count(), tab)
            })
            .max()
            .unwrap_or(0);
        let hv = &mut self.doc.hview;
        if let crate::doc::HMode::Manual { anchor } = hv.mode
            && anchor != ed.cursor
        {
            hv.mode = crate::doc::HMode::FollowCursor;
        }
        match hv.mode {
            crate::doc::HMode::FollowCursor => {
                if cursor_x < hv.offset {
                    hv.offset = cursor_x;
                } else if width > 0 && cursor_x >= hv.offset + width {
                    hv.offset = cursor_x + 1 - width;
                }
            }
            crate::doc::HMode::Manual { .. } => {
                // Never scroll past the longest visible line.
                hv.offset = hv.offset.min(widest.saturating_sub(width / 2));
            }
        }
        let hscroll = hv.offset;
        self.refresh_inlays(first, last);
        let inlays: Vec<Vec<(usize, String)>> = (first..last).map(|l| self.inlays_on(l)).collect();
        let syntax = self.syntax_styles(first, last);
        let bulb = self.bulb_line();
        let conflicts = self.doc.conflicts().to_vec();
        let ed = &self.doc.editor;
        let sel = ed
            .selection()
            .map(|(s, e)| (ed.cursor_at(s), ed.cursor_at(e)));
        let guide = show_guides.then(|| (self.icons.guide(), Style::new().fg(t.c(t.guide))));

        let mut lines = Vec::with_capacity(rows.len());
        for &i in &rows {
            let text = ed.buffer.line_text(i);
            let len = text.chars().count();
            let is_cur = i == ed.cursor.line;
            let range = sel.and_then(|(s, e)| line_selection(i, s, e, len));
            let mut row_bg = if is_cur && focused && sel.is_none() {
                t.bg_hl
            } else {
                t.bg
            };
            let mut fg = t.fg;
            if let Some(c) = conflicts.iter().find(|c| c.start <= i && i <= c.end) {
                if i == c.start {
                    (row_bg, fg) = (t.bg_hl, t.green);
                } else if i == c.end {
                    (row_bg, fg) = (t.bg_hl, t.blue);
                } else if i == c.mid || Some(i) == c.base {
                    (row_bg, fg) = (t.bg_hl, t.comment);
                } else if i < c.base.unwrap_or(c.mid) {
                    row_bg = t.diff_ours;
                } else if i > c.mid {
                    row_bg = t.diff_theirs;
                }
            }
            let (sign, sign_fg) = match diags.lines.get(&i) {
                Some(Severity::Error) => (self.icons.error(), t.red),
                Some(Severity::Warning) => (self.icons.warning(), t.yellow),
                Some(_) => (self.icons.info(), t.cyan),
                None if bulb == Some(i) => (self.icons.bulb(), t.yellow),
                None if ed.folds().at_header(i).is_some() => (self.icons.folded(), t.comment),
                None if is_cur && skink_core::fold::range_at(&ed.buffer, i).is_some() => {
                    (self.icons.foldable(), t.comment)
                }
                None => (" ", t.line_nr),
            };
            let mut num_style = Style::new()
                .fg(t.c(if is_cur { t.blue } else { t.line_nr }))
                .bg(t.c(row_bg));
            if is_cur {
                num_style = num_style.add_modifier(Modifier::BOLD);
            }
            let ls = crate::text::LineStyle {
                base: Style::new().fg(t.c(fg)).bg(t.c(row_bg)),
                hl: syntax.get(i - first).map_or(&[], Vec::as_slice),
                sel: range,
                sel_style: Style::new().bg(t.c(t.bg_sel)),
                sel_eol: range.is_some_and(|(_, e)| e > len),
                guide,
                tab,
            };
            let mut row = vec![
                Span::styled(sign, Style::new().fg(t.c(sign_fg)).bg(t.c(row_bg))),
                Span::styled(format!("{:>w$} ", i + 1, w = gutter - 2), num_style),
            ];
            row.extend(crate::text::spans(&text, hscroll, width, &ls));
            // Inlay hints, right to left so earlier columns stay valid.
            let mut hints = inlays[i - first].clone();
            hints.sort_by_key(|h| std::cmp::Reverse(h.0));
            for (col, label) in hints {
                let x = crate::text::display_col(&text, col.min(len), tab);
                if let Some(at) = x.checked_sub(hscroll) {
                    insert_virtual(
                        &mut row,
                        2,
                        at,
                        Span::styled(label, Style::new().fg(t.c(t.comment)).bg(t.c(row_bg))),
                    );
                }
            }
            if let Some(f) = ed.folds().at_header(i) {
                let n = f.hidden_lines();
                let end = crate::text::display_col(&text, len, tab) + 1;
                if let Some(at) = end.checked_sub(hscroll) {
                    insert_virtual(
                        &mut row,
                        2,
                        at,
                        Span::styled(
                            format!("\u{22ef} {n} line{} ", if n == 1 { "" } else { "s" }),
                            Style::new().fg(t.c(t.comment)).bg(t.c(t.bg_hl)),
                        ),
                    );
                }
            }
            lines.push(Line::from(row));
        }
        frame.render_widget(Paragraph::new(lines), area);
        self.render_rulers(frame, area, rows.len(), gutter, hscroll);
        self.render_sticky(frame, area, &sticky, sticky_syntax, gutter, hscroll);
        self.render_hscrollbar(frame, area, gutter, width, widest, hscroll);
        let row_of = |line: usize| -> Option<usize> {
            rows.iter()
                .position(|&l| l == line)
                .filter(|&r| r >= sticky.len())
        };

        // Extra cursors: their selections, then a block at each head.
        let ed = &self.doc.editor;
        let cell = |idx: usize| -> Option<(u16, u16)> {
            let c = ed.cursor_at(idx);
            let row = row_of(c.line)?;
            let x = crate::text::display_col(&ed.buffer.line_text(c.line), c.col, tab);
            let col = x.checked_sub(hscroll)? + gutter;
            (row < area.height as usize && col < area.width as usize)
                .then(|| (area.x + col as u16, area.y + row as u16))
        };
        let last_line = ed.buffer.len_lines() - 1;
        let bottom = last.saturating_sub(1).min(last_line);
        let vis_lo = ed.buffer.line_to_char(first.min(last_line));
        let vis_hi = ed.buffer.line_to_char(bottom) + ed.buffer.line_len(bottom);
        // While the find bar is open, every visible match is marked.
        if let Overlay::Prompt(p) = &self.overlay
            && let PromptKind::Find { within, .. } = p.kind
            && !p.input.is_empty()
            && let Ok(re) = ed.search_opts.compile(&p.input)
        {
            let current = ed.selection();
            let (lo, hi) = match within {
                skink_core::Within::File => (vis_lo, vis_hi),
                skink_core::Within::Range(a, b) => (vis_lo.max(a), vis_hi.min(b)),
            };
            for (a, b) in ed.matches_in(&re, lo, hi.max(lo)) {
                if Some((a, b)) == current {
                    continue;
                }
                for idx in a..b {
                    if let Some(pos) = cell(idx) {
                        frame.buffer_mut()[pos].set_bg(t.c(t.bg_search));
                    }
                }
            }
        }
        for s in ed.secondary() {
            if let Some((a, b)) = s.range() {
                for idx in a.max(vis_lo)..b.min(vis_hi) {
                    if let Some(pos) = cell(idx) {
                        frame.buffer_mut()[pos].set_bg(t.c(t.bg_sel));
                    }
                }
            }
            if let Some(pos) = cell(s.head) {
                frame.buffer_mut()[pos].set_style(Style::new().add_modifier(Modifier::REVERSED));
            }
        }

        // Matching bracket pair: bold + underlined over the syntax colour.
        if let Some((open, close)) = self.doc.bracket_pair() {
            let ed = &self.doc.editor;
            let style = Style::new()
                .fg(t.c(t.orange))
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            for idx in [open, close] {
                let c = ed.cursor_at(idx);
                let Some(row) = row_of(c.line) else {
                    continue;
                };
                let x = crate::text::display_col(&ed.buffer.line_text(c.line), c.col, tab);
                let Some(col) = x.checked_sub(hscroll).map(|x| x + gutter) else {
                    continue;
                };
                if row < area.height as usize && col < area.width as usize {
                    let pos = (area.x + col as u16, area.y + row as u16);
                    frame.buffer_mut()[pos].set_style(style);
                }
            }
        }

        let ed = &self.doc.editor;
        let row = row_of(ed.cursor.line).filter(|&r| r < area.height as usize)?;
        let Some(col) = (gutter + cursor_x).checked_sub(hscroll) else {
            return None; // scrolled off to the left
        };
        if col < gutter || col >= area.width as usize {
            return None;
        }
        let pos = (area.x + col as u16, area.y + row as u16);
        if focused {
            frame.set_cursor_position(Position::new(pos.0, pos.1));
        }
        Some(pos)
    }

    /// A thin thumb on the bottom row when visible lines are wider than the
    /// view (only the thumb is drawn, so that row's text stays readable).
    fn render_hscrollbar(
        &self,
        frame: &mut Frame,
        area: Rect,
        gutter: usize,
        width: usize,
        widest: usize,
        offset: usize,
    ) {
        // The cursor may sit one past the widest line's end.
        let content = (widest + 1).max(offset + width);
        if width < 8 || area.height < 3 || (widest < width && offset == 0) {
            return;
        }
        let len = (width * width / content).max(2);
        let start = (offset * width / content).min(width - len);
        let y = area.bottom() - 1;
        let style = Style::new().fg(self.theme.c(self.theme.comment));
        for dx in start..start + len {
            let x = area.x + (gutter + dx) as u16;
            frame.buffer_mut()[(x, y)]
                .set_symbol("\u{2581}")
                .set_style(style);
        }
    }

    /// Syntax colours per visible line (char columns).
    fn syntax_styles(&mut self, first: usize, last: usize) -> Vec<Vec<(usize, usize, Style)>> {
        let n = last.saturating_sub(first);
        if let Some(r) = self.markdown_preview() {
            let theme = self.theme;
            return (first..last)
                .map(|i| {
                    r.spans
                        .get(i)
                        .into_iter()
                        .flatten()
                        .map(|s| (s.start, s.end, theme.hl_style(s.hl)))
                        .collect()
                })
                .collect();
        }
        // Large files have no `syntax` (see `disk::SizeClass`).
        let buf = &self.doc.editor.buffer;
        let Some(syn) = &mut self.doc.syntax else {
            return vec![Vec::new(); n];
        };
        syn.refresh(buf, self.wake.as_ref());
        let crate::doc::Shown::Tree { text, .. } = &syn.shown else {
            return vec![Vec::new(); n]; // first parse of a big file still running
        };
        let theme = self.theme;
        syn.hl
            .highlights(text, first, last)
            .into_iter()
            .map(|line| {
                line.into_iter()
                    .map(|s| (s.start, s.end, theme.hl_style(s.hl)))
                    .collect()
            })
            .collect()
    }

    /// A rounded box under the cursor with the diagnostic on its line.
    fn render_diag_popup(
        &self,
        frame: &mut Frame,
        area: Rect,
        (cx, cy): (u16, u16),
        diags: &Diags,
    ) {
        let Some((sev, msg)) = &diags.here else {
            return;
        };
        if !self.editor_focused()
            || matches!(self.overlay, Overlay::Prompt(_))
            || self.doc.editor.message.is_some()
        {
            return;
        }
        let t = &self.theme;
        let (color, icon) = match sev {
            Severity::Error => (t.red, self.icons.error()),
            Severity::Warning => (t.yellow, self.icons.warning()),
            _ => (t.cyan, self.icons.info()),
        };
        let max_w = (area.width as usize).saturating_sub(6).clamp(10, 70);
        let mut wrapped = wrap(msg, max_w, 8);
        wrapped.truncate(6);
        let inner_w = wrapped.iter().map(|l| l.width()).max().unwrap_or(0) + 2;
        let w = (inner_w as u16 + 2).min(area.width);
        let h = wrapped.len() as u16 + 2;
        let y = if cy + 1 + h <= area.bottom() {
            cy + 1
        } else {
            cy.saturating_sub(h)
        };
        let rect = Rect {
            x: cx.min(area.right().saturating_sub(w)),
            y,
            width: w,
            height: h,
        };
        if rect.bottom() > area.bottom() || y < area.y {
            return;
        }
        frame.render_widget(Clear, rect);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.c(color)))
            .style(Style::new().bg(t.c(t.bg_popup)).fg(t.c(t.fg)))
            .title(Span::styled(
                format!(" {icon} "),
                Style::new().fg(t.c(color)),
            ))
            .title_alignment(Alignment::Left);
        let lines: Vec<Line> = wrapped
            .into_iter()
            .map(|l| Line::raw(format!(" {l}")))
            .collect();
        frame.render_widget(Paragraph::new(lines).block(block), rect);
    }

    /// Completion list (with a docs panel) or an info box at the cursor.
    fn render_intel_popup(&self, frame: &mut Frame, area: Rect, (cx, cy): (u16, u16)) {
        use crate::intel::{InfoKind, Popup};
        if !self.editor_focused() || !matches!(self.overlay, Overlay::None) {
            return;
        }
        let t = &self.theme;
        let place = |w: u16, h: u16, x: u16| -> Option<Rect> {
            let w = w.min(area.width);
            let below = cy + 1 + h <= area.bottom();
            let y = if below { cy + 1 } else { cy.checked_sub(h)? };
            (y >= area.y).then_some(Rect {
                x: x.min(area.right().saturating_sub(w)),
                y,
                width: w,
                height: h,
            })
        };
        let block = |color, title: String| {
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(t.c(color)))
                .style(Style::new().bg(t.c(t.bg_popup)).fg(t.c(t.fg)))
                .title(Span::styled(title, Style::new().fg(t.c(color))))
        };
        match &self.intel.popup {
            Popup::Closed => {}
            Popup::Completion(c) => {
                const ROWS: usize = 10;
                let typed = self.doc.editor.cursor_idx().saturating_sub(c.start) as u16;
                let x = cx.saturating_sub(typed + 1);
                let start = c.selected.saturating_sub(ROWS - 1);
                let shown: Vec<usize> = c.ranked.iter().skip(start).take(ROWS).copied().collect();
                let label_w = shown
                    .iter()
                    .map(|&i| c.items[i].label.width())
                    .max()
                    .unwrap_or(0)
                    .clamp(12, 40);
                let detail_w = 24;
                let w = (label_w + detail_w + 5) as u16;
                let h = shown.len() as u16 + 2;
                let Some(rect) = place(w, h, x) else { return };
                frame.render_widget(Clear, rect);
                let title = format!(" {}/{} ", c.selected + 1, c.ranked.len());
                let inner_w = rect.width.saturating_sub(2) as usize;
                let lines: Vec<Line> = shown
                    .iter()
                    .enumerate()
                    .map(|(row, &i)| {
                        let it = &c.items[i];
                        let sel = start + row == c.selected;
                        let bg = t.c(if sel { t.bg_sel } else { t.bg_popup });
                        let label = truncate(&it.label, label_w);
                        let room = inner_w.saturating_sub(label.width() + 2);
                        let detail = truncate(crate::intel::item_detail(it), room);
                        let pad = inner_w.saturating_sub(1 + label.width() + 1 + detail.width());
                        Line::from(vec![
                            Span::styled(
                                format!(" {label}"),
                                Style::new().fg(t.c(if sel { t.blue } else { t.fg })).bg(bg),
                            ),
                            Span::styled(" ".repeat(pad + 1), Style::new().bg(bg)),
                            Span::styled(detail, Style::new().fg(t.c(t.comment)).bg(bg)),
                        ])
                    })
                    .collect();
                frame.render_widget(Paragraph::new(lines).block(block(t.blue, title)), rect);
                // Docs of the selected item beside the list, when there is room.
                let doc_lines = c
                    .ranked
                    .get(c.selected)
                    .and_then(|&i| c.items[i].documentation.as_deref())
                    .map(crate::intel::markdown_lines)
                    .unwrap_or_default();
                let free = area.right().saturating_sub(rect.right());
                if !doc_lines.is_empty() && free >= 24 {
                    let dw = free.min(60);
                    let wrapped: Vec<String> = doc_lines
                        .iter()
                        .flat_map(|l| wrap(l, dw.saturating_sub(4) as usize, 4))
                        .take(14)
                        .collect();
                    let dr = Rect {
                        x: rect.right(),
                        y: rect.y,
                        width: dw,
                        height: (wrapped.len() as u16 + 2).min(area.bottom() - rect.y),
                    };
                    frame.render_widget(Clear, dr);
                    let ls: Vec<Line> = wrapped
                        .into_iter()
                        .map(|l| Line::raw(format!(" {l}")))
                        .collect();
                    frame.render_widget(
                        Paragraph::new(ls).block(block(t.comment, " docs ".into())),
                        dr,
                    );
                }
            }
            Popup::Info(info) => {
                let max_w = (area.width as usize).saturating_sub(4).clamp(20, 90);
                let mut rows: Vec<Line> = Vec::new();
                for (n, l) in info.lines.iter().enumerate() {
                    match (&info.kind, n) {
                        (
                            InfoKind::Signature {
                                active: Some((a, b)),
                            },
                            0,
                        ) => {
                            let chars: Vec<char> = l.chars().collect();
                            let part = |r: std::ops::Range<usize>| -> String {
                                chars[r.start.min(chars.len())..r.end.min(chars.len())]
                                    .iter()
                                    .collect()
                            };
                            rows.push(Line::from(vec![
                                Span::raw(format!(" {}", part(0..*a))),
                                Span::styled(
                                    part(*a..*b),
                                    Style::new().fg(t.c(t.orange)).add_modifier(Modifier::BOLD),
                                ),
                                Span::raw(part(*b..chars.len())),
                            ]));
                        }
                        (InfoKind::Peek { .. }, _) => {
                            rows.push(Line::raw(format!(" {}", truncate(l, max_w - 2))))
                        }
                        _ => rows.extend(
                            wrap(l, max_w - 2, 4)
                                .into_iter()
                                .map(|w| Line::raw(format!(" {w}"))),
                        ),
                    }
                    if rows.len() >= 16 {
                        break;
                    }
                }
                let w = rows.iter().map(Line::width).max().unwrap_or(10) as u16 + 3;
                let h = rows.len() as u16 + 2;
                let (color, title) = match &info.kind {
                    InfoKind::Hover => (t.cyan, " info ".to_owned()),
                    InfoKind::Signature { .. } => (t.purple, " signature ".to_owned()),
                    InfoKind::Peek { title } => (t.blue, format!(" {title} ")),
                };
                let Some(rect) = place(w.max(title.width() as u16 + 4), h, cx) else {
                    return;
                };
                frame.render_widget(Clear, rect);
                frame.render_widget(Paragraph::new(rows).block(block(color, title)), rect);
            }
        }
    }

    // ---- overlays ----------------------------------------------------------------

    /// Centered quick-open overlay: the list, plus a preview of the selected
    /// file beside it when the terminal is wide enough.
    fn render_picker(&self, frame: &mut Frame, full: Rect) {
        let Overlay::Picker(p) = &self.overlay else {
            return;
        };
        let t = &self.theme;
        let has_preview = !matches!(p.preview, picker::Preview::Empty) && full.width >= 110;
        let max_w = if has_preview { 170 } else { 90 };
        let w = full
            .width
            .saturating_sub(6)
            .clamp(20, max_w)
            .min(full.width);
        let h = full
            .height
            .saturating_sub(4)
            .clamp(5, if has_preview { 30 } else { 16 })
            .min(full.height);
        let rect = Rect {
            x: full.x + (full.width - w) / 2,
            y: full.y + 2.min(full.height - h),
            width: w,
            height: h,
        };
        frame.render_widget(Clear, rect);
        let walking = match &p.walk {
            picker::Walk::Streaming { .. } => format!(" {} files, scanning… ", p.items.len()),
            picker::Walk::Done { files } => format!(" {files} files "),
            picker::Walk::None => String::new(),
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.c(t.blue)))
            .style(Style::new().bg(t.c(t.bg_popup)).fg(t.c(t.fg)))
            .title(Span::styled(
                format!(" {} ", p.mode.title()),
                Style::new().fg(t.c(t.blue)).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Line::styled(walking, Style::new().fg(t.c(t.comment))).right_aligned());
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let (list, preview) = if has_preview {
            let lw = (inner.width * 45 / 100).max(30).min(inner.width);
            (
                Rect { width: lw, ..inner },
                Rect {
                    x: inner.x + lw + 1,
                    width: inner.width.saturating_sub(lw + 1),
                    ..inner
                },
            )
        } else {
            (inner, Rect::default())
        };
        let rows = list.height.saturating_sub(1) as usize;
        let width = list.width as usize;
        let matches = p.matches();
        let mut lines = vec![Line::from(vec![
            Span::styled(" > ", Style::new().fg(t.c(t.blue))),
            Span::raw(p.input.clone()),
        ])];
        let start = p.selected.saturating_sub(rows.saturating_sub(1));
        for (i, item) in matches.iter().enumerate().skip(start).take(rows) {
            let bg = t.c(if i == p.selected {
                t.bg_sel
            } else {
                t.bg_popup
            });
            let icon = match &item.target {
                picker::Target::File(f) | picker::Target::Location { path: f, .. } => {
                    self.icons.file(f)
                }
                picker::Target::Tab(_) => self.icons.file(Path::new("")),
                picker::Target::Action(_) | picker::Target::Command(_) => " ",
            };
            let tag = if item.tag.is_empty() {
                String::new()
            } else {
                format!(" {} ", item.tag)
            };
            let (label_hits, detail_hits) = p.positions(item);
            let hit = Style::new()
                .fg(t.c(t.orange))
                .bg(bg)
                .add_modifier(Modifier::BOLD);
            let prefix = format!(" {icon} ");
            let label_w = item.label.width();
            let room = width.saturating_sub(prefix.width() + label_w + tag.width() + 2);
            let detail = if item.detail == item.label {
                String::new()
            } else {
                truncate(&item.detail, room)
            };
            // Truncation keeps the head and ends in `…`: drop hits past it.
            let shown = detail.chars().count() - usize::from(detail.ends_with('…'));
            let detail_hits: Vec<usize> = detail_hits.into_iter().filter(|&i| i < shown).collect();
            let used = prefix.width() + label_w + 1 + detail.width() + tag.width();
            let mut spans = vec![Span::styled(prefix, Style::new().fg(t.c(t.fg)).bg(bg))];
            spans.extend(hit_spans(
                &item.label,
                &label_hits,
                Style::new().fg(t.c(t.fg)).bg(bg),
                hit,
            ));
            spans.push(Span::styled(" ", Style::new().bg(bg)));
            spans.extend(hit_spans(
                &detail,
                &detail_hits,
                Style::new().fg(t.c(t.comment)).bg(bg),
                hit.remove_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                " ".repeat(width.saturating_sub(used)),
                Style::new().bg(bg),
            ));
            spans.push(Span::styled(tag, Style::new().fg(t.c(t.teal)).bg(bg)));
            lines.push(Line::from(spans));
        }
        if matches.is_empty() {
            let msg = match p.walk {
                picker::Walk::Streaming { .. } => "   scanning…",
                _ => "   no matches",
            };
            lines.push(Line::styled(msg, Style::new().fg(t.c(t.comment))));
        }
        frame.render_widget(Paragraph::new(lines), list);
        if has_preview {
            self.render_picker_preview(frame, preview, &p.preview);
        }
        let cx = list.x + 3 + p.input.width() as u16;
        frame.set_cursor_position(Position::new(
            cx.min(list.right().saturating_sub(1)),
            list.y,
        ));
    }

    /// The selected file's first lines (or the lines around a location),
    /// syntax coloured, with line numbers.
    fn render_picker_preview(&self, frame: &mut Frame, area: Rect, preview: &picker::Preview) {
        let t = &self.theme;
        let sep = Block::new()
            .borders(Borders::LEFT)
            .border_style(Style::new().fg(t.c(t.guide)));
        let inner = sep.inner(area);
        frame.render_widget(sep, area);
        let lines: Vec<Line> = match preview {
            picker::Preview::Empty => Vec::new(),
            picker::Preview::Unavailable { .. } => vec![Line::styled(
                " binary or unreadable file",
                Style::new().fg(t.c(t.comment)),
            )],
            picker::Preview::Text {
                lines,
                spans,
                focus,
                first,
                ..
            } => {
                let skip = focus.saturating_sub(3);
                lines
                    .iter()
                    .enumerate()
                    .skip(skip)
                    .take(inner.height as usize)
                    .map(|(i, l)| {
                        let num_style = if i == *focus && first + focus > 0 {
                            Style::new().fg(t.c(t.orange))
                        } else {
                            Style::new().fg(t.c(t.comment))
                        };
                        let mut out =
                            vec![Span::styled(format!("{:>4} ", first + i + 1), num_style)];
                        let chars: Vec<char> =
                            l.chars().map(|c| if c == '\t' { ' ' } else { c }).collect();
                        let mut col = 0;
                        for sp in spans.get(i).into_iter().flatten() {
                            let (a, b) = (sp.start.min(chars.len()), sp.end.min(chars.len()));
                            if a > col {
                                out.push(Span::raw(chars[col..a].iter().collect::<String>()));
                            }
                            if b > a {
                                out.push(Span::styled(
                                    chars[a..b].iter().collect::<String>(),
                                    t.hl_style(sp.hl),
                                ));
                            }
                            col = col.max(b);
                        }
                        if col < chars.len() {
                            out.push(Span::raw(chars[col..].iter().collect::<String>()));
                        }
                        Line::from(out)
                    })
                    .collect()
            }
        };
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_terminal(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme;
        let overlay = !matches!(self.overlay, Overlay::None) || self.focus() == Focus::Tree;
        let Some(pane) = self.active_term() else {
            return;
        };
        termview::render(frame.buffer_mut(), area, &pane.screen, &t);
        let c = pane.screen.cursor;
        if !overlay && c.visible && !pane.exited && c.col < area.width && c.row < area.height {
            frame.set_cursor_position(Position::new(area.x + c.col, area.y + c.row));
        }
    }

    // ---- status line -------------------------------------------------------------

    /// The prompt replaces the status line while it is open.
    fn render_prompt(&self, frame: &mut Frame, area: Rect) {
        let Overlay::Prompt(p) = &self.overlay else {
            return;
        };
        let t = &self.theme;
        let text = format!(" {}{}", p.label, p.input);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(text.clone()).style(Style::new().bg(t.c(t.bg_dark)).fg(t.c(t.fg_dim))),
            area,
        );
        // Completion candidates and errors show to the right of the input.
        let used = text.width() as u16 + 2;
        if let Some(m) = &self.doc.editor.message
            && area.width > used + 4
        {
            let room = Rect {
                x: area.x + used,
                width: area.width - used,
                ..area
            };
            frame.render_widget(
                Paragraph::new(m.as_str())
                    .style(Style::new().bg(t.c(t.bg_dark)).fg(t.c(t.comment))),
                room,
            );
        }
        if !matches!(p.kind, PromptKind::Choice(_)) {
            let x = (area.x + text.width() as u16).min(area.right().saturating_sub(1));
            frame.set_cursor_position(Position::new(x, area.y));
        }
    }

    /// Status line layout: left spans, a gap, right spans.
    fn render_status_line(&self, frame: &mut Frame, area: Rect, left: Vec<Span>, right: Vec<Span>) {
        let t = &self.theme;
        let width = |v: &[Span]| v.iter().map(|s| s.content.width()).sum::<usize>();
        let gap = (area.width as usize).saturating_sub(width(&left) + width(&right));
        let mut all = left;
        all.push(Span::raw(" ".repeat(gap)));
        all.extend(right);
        frame.render_widget(
            Paragraph::new(Line::from(all))
                .style(Style::new().bg(t.c(t.bg_dark)).fg(t.c(t.fg_dim))),
            area,
        );
    }

    fn mode_pill(&self, fallback: (&str, Rgb)) -> Vec<Span<'static>> {
        let t = &self.theme;
        let (mode, color) = if self.focus() == Focus::Tree {
            ("TREE", t.purple)
        } else {
            fallback
        };
        let mut v = vec![Span::raw(" ")];
        v.extend(self.pill(mode, t.bg_dark, color, t.bg_dark));
        v
    }

    fn render_term_status(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme;
        let mut left = self.mode_pill(("TERM", t.green));
        let Some(pane) = self.active_term() else {
            return;
        };
        let (title, exited, (rows, cols)) =
            (pane.title().to_owned(), pane.exited, pane.term.size());
        left.push(Span::styled(format!("  {title}"), self.st(t.fg_dim)));
        if exited {
            left.push(Span::styled(
                "  exited, press any key to close",
                self.st(t.red),
            ));
        }
        let right = vec![
            Span::styled("F12 back to file  ", self.st(t.comment)),
            Span::styled(format!("{cols}x{rows} "), self.st(t.comment)),
        ];
        self.render_status_line(frame, area, left, right);
    }

    fn render_doc_status(&self, frame: &mut Frame, area: Rect, diags: &Diags, crumbs: &[String]) {
        let t = &self.theme;
        let b = &self.doc.editor.buffer;
        let mut left = self.mode_pill(if b.is_read_only() {
            ("VIEW", t.orange)
        } else {
            ("EDIT", t.blue)
        });
        if let Some(br) = &self.doc.git_branch {
            left.push(Span::styled(
                format!("  {} {br}", self.icons.branch()),
                self.st(t.purple),
            ));
        }
        let name = b
            .path()
            .map_or_else(|| self.doc.title(), |p| p.display().to_string());
        left.push(Span::styled(format!("  {name}"), self.st(t.fg_dim)));
        if b.is_read_only() {
            left.push(Span::styled(
                format!(" {}", self.icons.lock()),
                self.st(t.orange),
            ));
        }
        if !crumbs.is_empty() {
            left.push(Span::styled(
                format!(" \u{203a} {}", crumbs.join(" \u{203a} ")),
                self.st(t.comment),
            ));
        }
        let note = match (&self.doc.editor.message, &diags.here) {
            (Some(m), _) => Some((m.clone(), t.yellow)),
            (None, Some((sev, d))) => Some((
                d.lines().next().unwrap_or("").to_owned(),
                severity_color(t, *sev),
            )),
            _ => None,
        };
        if let Some((m, c)) = note {
            left.push(Span::styled(format!("  {m}"), self.st(c)));
        }

        let mut right: Vec<Span> = Vec::new();
        if diags.errors > 0 {
            right.push(Span::styled(
                format!("{} {} ", self.icons.error(), diags.errors),
                self.st(t.red),
            ));
        }
        if diags.warnings > 0 {
            right.push(Span::styled(
                format!("{} {} ", self.icons.warning(), diags.warnings),
                self.st(t.yellow),
            ));
        }
        if let Some(p) = self.lsp_note() {
            right.push(Span::styled(
                format!("{} ", truncate(&p, 30)),
                self.st(t.comment),
            ));
        }
        let delegated = self.backend == Backend::Delegated
            || self.lsp.as_ref().and_then(lsp::LspLink::backend) == Some(Backend::Delegated);
        if delegated {
            right.push(Span::styled("EveryAI ", self.st(t.teal)));
        }
        if self.doc.size == crate::disk::SizeClass::Large {
            right.push(Span::styled("LARGE ", self.st(t.yellow)));
        }
        if let skink_core::Access::ReadOnly(_) = b.access() {
            right.push(Span::styled("RO ", self.st(t.yellow)));
        }
        if let Some(enc) = b.encoding().label() {
            right.push(Span::styled(format!("{enc} "), self.st(t.comment)));
        }
        right.push(Span::styled(
            format!("{} ", b.line_ending().label()),
            self.st(t.comment),
        ));
        if let Some(p) = b.path() {
            let lang = self
                .doc
                .syntax
                .as_ref()
                .map_or_else(|| filetype(p), |s| s.hl.lang().name());
            right.push(Span::styled(
                format!("{} {lang} ", self.icons.file(p)),
                self.st(t.blue),
            ));
        }
        if diags.errors == 0 {
            right.push(Span::styled(
                format!("{} ", self.icons.check()),
                self.st(t.green),
            ));
        }
        let c = self.doc.editor.cursor;
        right.extend(self.pill(
            &match self.doc.editor.cursor_count() {
                1 => format!("{}:{}", c.line + 1, c.col + 1),
                n => format!("{}:{} ×{n}", c.line + 1, c.col + 1),
            },
            t.bg_dark,
            t.blue,
            t.bg_dark,
        ));
        right.push(Span::raw(" "));
        self.render_status_line(frame, area, left, right);
    }
}

fn severity_color(t: &crate::theme::Theme, s: Severity) -> Rgb {
    match s {
        Severity::Error => t.red,
        Severity::Warning => t.yellow,
        _ => t.cyan,
    }
}

/// `text` as spans, with the chars at `hits` (char indices) in `hit` style.
fn hit_spans(text: &str, hits: &[usize], base: Style, hit: Style) -> Vec<Span<'static>> {
    if hits.is_empty() {
        return vec![Span::styled(text.to_owned(), base)];
    }
    let mut out = Vec::new();
    let mut run = String::new();
    let mut run_hit = false;
    for (i, c) in text.chars().enumerate() {
        let h = hits.contains(&i);
        if h != run_hit && !run.is_empty() {
            out.push(Span::styled(
                std::mem::take(&mut run),
                if run_hit { hit } else { base },
            ));
        }
        run_hit = h;
        run.push(c);
    }
    if !run.is_empty() {
        out.push(Span::styled(run, if run_hit { hit } else { base }));
    }
    out
}

/// Cut `s` to at most `max` display columns, ending in `…` when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    if max > 0 {
        out.push('\u{2026}');
    }
    out
}

/// Greedy word wrap of `msg` at `width` columns, at most `max_lines` input lines.
fn wrap(msg: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut out = Vec::new();
    for raw in msg.lines().take(max_lines) {
        let mut cur = String::new();
        for word in raw.split_whitespace() {
            if !cur.is_empty() && cur.width() + 1 + word.width() > width {
                out.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        out.push(cur);
    }
    out
}

/// `~/...` for paths under the home directory.
fn home_relative(p: &Path) -> String {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match home
        .as_deref()
        .map(Path::new)
        .and_then(|h| p.strip_prefix(h).ok())
    {
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => p.display().to_string(),
    }
}

/// File type label for files without a highlighter.
fn filetype(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        _ => "text",
    }
}

/// Selected char-column range on `line`, if any (end exclusive; past the
/// end when the newline is selected too).
fn line_selection(line: usize, s: Cursor, e: Cursor, len: usize) -> Option<(usize, usize)> {
    if line < s.line || line > e.line {
        return None;
    }
    let from = if line == s.line { s.col } else { 0 };
    let to = if line == e.line { e.col } else { len + 1 };
    (from < to).then_some((from, to))
}

/// Drop the first `n` display columns of a row of spans (sideways scroll).
fn skip_cols(spans: Vec<Span<'static>>, mut n: usize) -> Vec<Span<'static>> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut out = Vec::with_capacity(spans.len());
    for sp in spans {
        if n == 0 {
            out.push(sp);
            continue;
        }
        let mut rest = String::new();
        for g in sp.content.graphemes(true) {
            let w = g.width();
            if n == 0 {
                rest.push_str(g);
            } else if w <= n {
                n -= w;
            } else {
                // A wide cluster cut in half: its visible part as spaces.
                rest.push_str(&" ".repeat(w - n));
                n = 0;
            }
        }
        if !rest.is_empty() {
            out.push(Span::styled(rest, sp.style));
        }
    }
    out
}

/// Insert `span` at display column `at` of the content that follows the
/// first `skip` spans (the gutter), splitting the span it falls into.
fn insert_virtual(row: &mut Vec<Span<'static>>, skip: usize, at: usize, span: Span<'static>) {
    let mut col = 0;
    let mut i = skip;
    while i < row.len() {
        let w = row[i].content.width();
        if col + w > at {
            let cut = at - col;
            let content = row[i].content.to_string();
            let mut acc = 0;
            let byte = content
                .char_indices()
                .find(|(_, c)| {
                    let fits = acc >= cut;
                    acc += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
                    fits
                })
                .map_or(content.len(), |(b, _)| b);
            let style = row[i].style;
            let (a, b) = content.split_at(byte);
            row.splice(
                i..=i,
                [
                    Span::styled(a.to_owned(), style),
                    span,
                    Span::styled(b.to_owned(), style),
                ],
            );
            return;
        }
        col += w;
        i += 1;
    }
    row.push(span);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_by_display_width() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefgh", 5), "abcd\u{2026}");
        // Wide chars count double.
        assert_eq!(truncate("日本語テキスト", 5), "日本\u{2026}");
        assert_eq!(truncate("x", 0), "");
    }

    #[test]
    fn wrap_breaks_on_words() {
        assert_eq!(wrap("aa bb cc", 5, 8), ["aa bb", "cc"]);
        assert_eq!(wrap("one\ntwo", 10, 1), ["one"]);
    }
}
