//! All drawing: title bar, file tree, editor, popups, terminal pane, status line.
//! Layout and look follow a Tokyo-Night-style editor: rounded pills, powerline
//! separators and Nerd Font icons (with an ASCII fallback).

use super::*;
use crate::text::{LineStyle, display_col, spans};
use ratatui::{
    layout::Alignment,
    style::Modifier,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use skink_lsp::Severity;
use std::path::Path;

/// (icon, label, marker glyph + color, is a terminal)
type TabEntry = (String, String, Option<(&'static str, (u8, u8, u8))>, bool);

impl App {
    fn st(&self, fg: (u8, u8, u8)) -> Style {
        Style::new().fg(self.theme.c(fg))
    }

    fn pill(
        &self,
        text: &str,
        fg: (u8, u8, u8),
        bg: (u8, u8, u8),
        bar: (u8, u8, u8),
    ) -> Vec<Span<'static>> {
        let t = &self.theme;
        vec![
            Span::styled(
                self.icons.pill_left(),
                Style::new().fg(t.c(bg)).bg(t.c(bar)),
            ),
            Span::styled(
                format!(" {text} "),
                Style::new()
                    .fg(t.c(fg))
                    .bg(t.c(bg))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                self.icons.pill_right(),
                Style::new().fg(t.c(bg)).bg(t.c(bar)),
            ),
        ]
    }

    pub(crate) fn gutter_width(&self) -> usize {
        self.editor.buffer.len_lines().to_string().len() + 2
    }

    /// (Re)create the highlighter for the live file; cheap, parsing is lazy.
    pub(crate) fn syntax_reset(&mut self) {
        self.hl_rev = u64::MAX;
        self.hl_text.clear();
        self.syntax = self
            .editor
            .buffer
            .path()
            .filter(|_| self.settings.syntax)
            .and_then(skink_syntax::language_for_path)
            .map(skink_syntax::Highlighter::new);
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let full = frame.area();
        let t = self.theme;
        frame.render_widget(
            Block::new().style(Style::new().bg(t.c(t.bg)).fg(t.c(t.fg))),
            full,
        );

        let [top, body, status_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(full);
        let show_tree = self.tree_visible && self.tree.is_some() && body.width >= 50;
        let (tree_area, main) = if show_tree {
            let w = (body.width / 4).clamp(22, 34);
            let [a, b] =
                Layout::horizontal([Constraint::Length(w), Constraint::Min(1)]).areas(body);
            (a, b)
        } else {
            (Rect::default(), body)
        };
        let editor_area = main;
        let term_area = if self.active_is_term() {
            main
        } else {
            Rect::default()
        };
        self.areas = Areas {
            top,
            tree: tree_area,
            editor: editor_area,
            term: term_area,
        };

        // Fresh title for the tab bar (it is drawn before the terminal itself).
        if let Some(p) = self.active_term() {
            p.term.snapshot_into(&mut p.screen);
        }
        self.render_top(frame, top);
        if show_tree {
            self.render_tree(frame, tree_area);
        }
        if self.active_is_term() {
            self.render_terminal(frame, term_area);
        } else {
            self.render_editor(frame, editor_area);
        }
        self.render_status(frame, status_area);
        self.render_popup(frame, editor_area);
        self.render_picker(frame, full);
    }

    /// Title and kind of tab `i`: (icon, label, dirty/exited/activity marker).
    fn tab_entry(&self, i: usize) -> TabEntry {
        let t = &self.theme;
        match &self.tabs[i] {
            Tab::Term(p) => {
                let title = if p.screen.title.is_empty() {
                    p.label.clone()
                } else {
                    p.screen.title.clone()
                };
                let marker = if p.exited {
                    Some(("exited", t.red))
                } else if p.activity {
                    Some(("\u{2022}", t.green))
                } else {
                    None
                };
                (self.icons.terminal().to_owned(), title, marker, true)
            }
            Tab::Buffer(_) => {
                let e = self.tab_editor(i);
                let path = e.and_then(|e| e.buffer.path());
                let name = e
                    .and_then(|e| e.buffer.title())
                    .unwrap_or_else(|| "[scratch]".to_owned());
                let icon = match (path, self.icons) {
                    (Some(p), icons::IconSet::Nerd) => self.icons.file(p).to_owned(),
                    (None, icons::IconSet::Nerd) => "\u{f15b}".to_owned(),
                    _ => String::new(),
                };
                let dirty = e.is_some_and(|e| e.buffer.is_dirty());
                let marker = dirty.then_some((self.icons.dirty(), t.yellow));
                (icon, name, marker, false)
            }
        }
    }

    /// Ghostty-style tab bar: equal-width tabs, the active one filled, a close
    /// button on it, and activity/unsaved markers.
    fn render_top(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme;
        self.tab_hits.clear();
        let n = self.tabs.len();
        let min_w = 14u16;
        let max_vis = ((area.width / min_w) as usize).max(1);
        let start = if n > max_vis {
            (self.active + 1).saturating_sub(max_vis)
        } else {
            0
        };
        let shown = (n - start).min(max_vis);
        let w = (area.width / shown as u16).clamp(min_w, 28);
        let mut spans: Vec<Span> = Vec::new();
        let mut x = area.x;
        for i in start..start + shown {
            let (icon, label, marker, is_term) = self.tab_entry(i);
            let active = i == self.active;
            let accent = if is_term { t.green } else { t.blue };
            let (fg, bg) = if active {
                (t.bg_dark, accent)
            } else {
                (t.fg_dim, t.bg_dark)
            };
            let mut st = Style::new().fg(t.c(fg)).bg(t.c(bg));
            if active {
                st = st.add_modifier(Modifier::BOLD);
            }
            // [ icon label ........ marker close ]
            let close = if active && n > 1 { " \u{00d7}" } else { "" };
            let close = if close.is_empty() || self.icons == icons::IconSet::Nerd {
                close
            } else {
                " x"
            };
            let tail = format!(
                "{}{}",
                marker.map_or(String::new(), |(m, _)| format!(" {m}")),
                close
            );
            let tail_w = tail.chars().count();
            let head = if icon.is_empty() {
                format!(" {label}")
            } else {
                format!(" {icon} {label}")
            };
            let room = (w as usize).saturating_sub(tail_w + 1);
            let mut head: String = head.chars().take(room).collect();
            if head.chars().count() < format!(" {label}").chars().count() && room > 1 {
                head.pop();
                head.push('\u{2026}');
            }
            let pad = (w as usize).saturating_sub(head.chars().count() + tail_w);
            spans.push(Span::styled(head, st));
            spans.push(Span::styled(" ".repeat(pad), st));
            if let Some((m, c)) = marker {
                let ms = if active {
                    st
                } else {
                    Style::new().fg(t.c(c)).bg(t.c(bg))
                };
                spans.push(Span::styled(format!(" {m}"), ms));
            }
            let close_x = (!close.is_empty()).then(|| x + w - 1);
            if !close.is_empty() {
                spans.push(Span::styled(close, st));
            }
            self.tab_hits.push((x, x + w, i, close_x));
            x += w;
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::new().bg(t.c(t.bg_dark))),
            area,
        );
    }

    fn render_tree(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme;
        let focused = self.focus == Focus::Tree;
        let current = self
            .editor
            .buffer
            .path()
            .and_then(|p| std::fs::canonicalize(p).ok());
        let icons = self.icons;
        let rows = area.height.saturating_sub(2) as usize;
        let Some(tree) = &mut self.tree else { return };
        tree.keep_visible(rows);
        let bg = Style::new().bg(t.c(t.bg_dark)).fg(t.c(t.fg_dim));
        let sep = Style::new().fg(t.c(t.guide)).bg(t.c(t.bg_dark));
        let w = area.width.saturating_sub(1) as usize;

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::styled(
            " Files",
            Style::new().fg(t.c(t.comment)).bg(t.c(t.bg_dark)),
        ));
        let home = std::env::var("HOME").unwrap_or_default();
        let root = tree.root.to_string_lossy().into_owned();
        let root = match root.strip_prefix(&home) {
            Some(rest) if !home.is_empty() => format!("~{rest}"),
            _ => root,
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", icons.folder(true)),
                Style::new().fg(t.c(t.blue)),
            ),
            Span::styled(
                root,
                Style::new().fg(t.c(t.blue)).add_modifier(Modifier::BOLD),
            ),
        ]));
        let end = (tree.scroll + rows).min(tree.nodes.len());
        for (i, n) in tree.nodes.iter().enumerate().take(end).skip(tree.scroll) {
            let selected = i == tree.selected;
            let is_current = current.as_ref().is_some_and(|c| *c == n.path);
            let icon = if n.is_dir {
                icons.folder(n.expanded)
            } else {
                icons.file(&n.path)
            };
            let icon_fg = if n.is_dir { t.blue } else { t.cyan };
            let name_fg = if is_current { t.blue } else { t.fg_dim };
            let mut row_bg = t.bg_dark;
            if selected {
                row_bg = if focused { t.bg_sel } else { t.bg_hl };
            }
            let indent = "  ".repeat(n.depth);
            let used =
                1 + indent.chars().count() + icon.chars().count() + 1 + n.name.chars().count();
            let pad = " ".repeat(w.saturating_sub(used));
            let mut name_style = Style::new().fg(t.c(name_fg)).bg(t.c(row_bg));
            if is_current {
                name_style = name_style.add_modifier(Modifier::BOLD);
            }
            lines.push(Line::from(vec![
                Span::styled(format!(" {indent}"), Style::new().bg(t.c(row_bg))),
                Span::styled(
                    format!("{icon} "),
                    Style::new().fg(t.c(icon_fg)).bg(t.c(row_bg)),
                ),
                Span::styled(n.name.clone(), name_style),
                Span::styled(pad, Style::new().bg(t.c(row_bg))),
            ]));
        }
        if tree.hidden_at_root > 0 && end == tree.nodes.len() {
            lines.push(Line::styled(
                format!(" {{{} hidden items}}", tree.hidden_at_root),
                Style::new().fg(t.c(t.comment)).bg(t.c(t.bg_dark)),
            ));
        }
        let inner = Rect {
            width: area.width.saturating_sub(1),
            ..area
        };
        frame.render_widget(Paragraph::new(lines).style(bg), inner);
        // Right border.
        let x = area.right().saturating_sub(1);
        for y in area.y..area.bottom() {
            frame.buffer_mut()[(x, y)]
                .set_symbol("\u{2502}")
                .set_style(sep);
        }
    }

    fn render_editor(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme;
        self.editor.set_viewport_height(area.height as usize);
        let gutter = self.gutter_width();
        let width = (area.width as usize).saturating_sub(gutter);
        let tab = self.editor.tab_width;
        let focused = self.focus == Focus::Editor && self.prompt.is_none();

        let cur_line = self.editor.buffer.line_text(self.editor.cursor.line);
        let cursor_x = display_col(&cur_line, self.editor.cursor.col, tab);
        if cursor_x < self.hscroll {
            self.hscroll = cursor_x;
        } else if width > 0 && cursor_x >= self.hscroll + width {
            self.hscroll = cursor_x + 1 - width;
        }

        let diags = match (&self.lsp, self.editor.buffer.path()) {
            (Some(l), Some(p)) => {
                l.diagnostics(&std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
            }
            _ => Vec::new(),
        };
        let (diag_lines, n_err, n_warn) = lsp::summarize(&diags);
        self.diag_status = (n_err, n_warn);
        self.diag_here = diags
            .iter()
            .filter(|d| d.range.start.line == self.editor.cursor.line)
            .min_by_key(|d| d.severity as u8)
            .map(|d| (d.severity, d.message.clone()));

        let sel = self
            .editor
            .selection()
            .map(|(s, e)| (self.editor.cursor_at(s), self.editor.cursor_at(e)));
        let first = self.editor.scroll;
        let last = (first + area.height as usize).min(self.editor.buffer.len_lines());
        let hl_styles = self.syntax_styles(first, last);
        let rev = self.editor.buffer.revision();
        if self.conflict_cache.0 != self.cur_buf || self.conflict_cache.1 != rev {
            self.conflict_cache = (self.cur_buf, rev, self.editor.conflicts());
        }
        let conflicts = self.conflict_cache.2.clone();
        let mut lines = Vec::with_capacity(last - first);
        for i in first..last {
            let text = self.editor.buffer.line_text(i);
            let is_cur = i == self.editor.cursor.line;
            let range = sel.and_then(|(s, e)| line_selection(i, s, e, text.chars().count()));
            let mut row_bg = if is_cur && focused && sel.is_none() {
                t.bg_hl
            } else {
                t.bg
            };
            let mut marker_fg = None;
            if let Some(c) = conflicts.iter().find(|c| c.start <= i && i <= c.end) {
                if i == c.start {
                    (row_bg, marker_fg) = (t.bg_hl, Some(t.green));
                } else if i == c.end {
                    (row_bg, marker_fg) = (t.bg_hl, Some(t.blue));
                } else if i == c.mid || Some(i) == c.base {
                    (row_bg, marker_fg) = (t.bg_hl, Some(t.comment));
                } else if i < c.base.unwrap_or(c.mid) {
                    row_bg = t.diff_ours;
                } else if i > c.mid {
                    row_bg = t.diff_theirs;
                }
            }
            let base = Style::new()
                .fg(t.c(marker_fg.unwrap_or(t.fg)))
                .bg(t.c(row_bg));
            let (sign, sign_fg) = match diag_lines.get(&i) {
                Some(Severity::Error) => (self.icons.error(), t.red),
                Some(Severity::Warning) => (self.icons.warning(), t.yellow),
                Some(_) => (self.icons.info(), t.cyan),
                None => (" ", t.line_nr),
            };
            let num_style = if is_cur {
                Style::new()
                    .fg(t.c(t.blue))
                    .bg(t.c(row_bg))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(t.c(t.line_nr)).bg(t.c(row_bg))
            };
            let ls = LineStyle {
                base,
                hl: hl_styles.get(i - first).map_or(&[], Vec::as_slice),
                sel: range,
                sel_style: Style::new().bg(t.c(t.bg_sel)),
                sel_eol: range.is_some_and(|(_, e)| e > text.chars().count()),
                guide: self
                    .settings
                    .indent_guides
                    .then(|| (self.icons.guide(), Style::new().fg(t.c(t.guide)))),
                tab,
            };
            let mut row = vec![
                Span::styled(sign, Style::new().fg(t.c(sign_fg)).bg(t.c(row_bg))),
                Span::styled(format!("{:>w$} ", i + 1, w = gutter - 2), num_style),
            ];
            row.extend(spans(&text, self.hscroll, width, &ls));
            lines.push(Line::from(row));
        }
        frame.render_widget(Paragraph::new(lines), area);

        let cx = area.x + (gutter + cursor_x - self.hscroll) as u16;
        let cy = area.y + (self.editor.cursor.line - self.editor.scroll) as u16;
        self.cursor_screen = Some((cx, cy));
        if focused {
            frame.set_cursor_position(Position::new(cx, cy));
        }
    }

    /// Syntax colors per visible line (char columns). Filled in by the
    /// highlighter when one is attached.
    fn syntax_styles(&mut self, first: usize, last: usize) -> Vec<Vec<(usize, usize, Style)>> {
        const MAX_BYTES: usize = 4 << 20; // skip highlighting enormous files
        let n = last.saturating_sub(first);
        let Some(hl) = &mut self.syntax else {
            return vec![Vec::new(); n];
        };
        let rev = self.editor.buffer.revision();
        if self.hl_rev != rev {
            if self.editor.buffer.len_chars() > MAX_BYTES {
                return vec![Vec::new(); n];
            }
            self.hl_text = self.editor.buffer.text();
            hl.update(&self.hl_text);
            self.hl_rev = rev;
        }
        let theme = self.theme;
        hl.highlights(&self.hl_text, first, last)
            .into_iter()
            .map(|line| {
                line.into_iter()
                    .map(|s| (s.start, s.end, theme.hl_style(s.hl)))
                    .collect()
            })
            .collect()
    }

    /// A rounded box under the cursor with the diagnostic on its line.
    fn render_popup(&self, frame: &mut Frame, editor_area: Rect) {
        let t = &self.theme;
        let Some((sev, msg)) = &self.diag_here else {
            return;
        };
        if self.focus != Focus::Editor || self.prompt.is_some() || self.editor.message.is_some() {
            return;
        }
        let Some((cx, cy)) = self.cursor_screen else {
            return;
        };
        let (color, icon) = match sev {
            Severity::Error => (t.red, self.icons.error()),
            Severity::Warning => (t.yellow, self.icons.warning()),
            _ => (t.cyan, self.icons.info()),
        };
        let max_w = (editor_area.width as usize).saturating_sub(6).clamp(10, 70);
        let mut wrapped: Vec<String> = Vec::new();
        for raw in msg.lines().take(8) {
            let mut cur = String::new();
            for word in raw.split_whitespace() {
                if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > max_w {
                    wrapped.push(std::mem::take(&mut cur));
                }
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(word);
            }
            wrapped.push(cur);
        }
        wrapped.truncate(6);
        let inner_w = wrapped.iter().map(|l| l.chars().count()).max().unwrap_or(0) + 2;
        let w = (inner_w as u16 + 2).min(editor_area.width);
        let h = wrapped.len() as u16 + 2;
        let below = cy + 1 + h <= editor_area.bottom();
        let y = if below { cy + 1 } else { cy.saturating_sub(h) };
        let x = cx.min(editor_area.right().saturating_sub(w));
        let rect = Rect {
            x,
            y,
            width: w,
            height: h,
        };
        if rect.bottom() > editor_area.bottom() || y < editor_area.y {
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

    /// Centered quick-open overlay.
    fn render_picker(&self, frame: &mut Frame, full: Rect) {
        let Some(p) = &self.picker else { return };
        let t = &self.theme;
        let w = full.width.saturating_sub(6).clamp(20, 90);
        let h = full.height.saturating_sub(4).clamp(5, 16);
        let rect = Rect {
            x: full.x + (full.width.saturating_sub(w)) / 2,
            y: full.y + 2.min(full.height.saturating_sub(h)),
            width: w.min(full.width),
            height: h.min(full.height),
        };
        frame.render_widget(Clear, rect);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.c(t.blue)))
            .style(Style::new().bg(t.c(t.bg_popup)).fg(t.c(t.fg)))
            .title(Span::styled(
                " Quick open ",
                Style::new().fg(t.c(t.blue)).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let rows = inner.height.saturating_sub(1) as usize;
        let matches = p.matches();
        let prompt_line = Line::from(vec![
            Span::styled(" > ", Style::new().fg(t.c(t.blue))),
            Span::raw(p.input.clone()),
        ]);
        let mut lines = vec![prompt_line];
        let start = p.selected.saturating_sub(rows.saturating_sub(1));
        for (i, item) in matches.iter().enumerate().skip(start).take(rows) {
            let sel = i == p.selected;
            let bg = if sel { t.bg_sel } else { t.bg_popup };
            let icon = match &item.target {
                picker::Target::File(f) => self.icons.file(f),
                picker::Target::Tab(_) => self.icons.file(Path::new("")),
            };
            let width = inner.width as usize;
            let tag = if item.tag.is_empty() {
                String::new()
            } else {
                format!(" {} ", item.tag)
            };
            let left = format!(" {icon} {}", item.label);
            let detail_room = width.saturating_sub(left.chars().count() + tag.chars().count() + 2);
            let detail: String = if item.detail == item.label {
                String::new()
            } else {
                item.detail.chars().take(detail_room).collect()
            };
            let used = left.chars().count() + 1 + detail.chars().count() + tag.chars().count();
            lines.push(Line::from(vec![
                Span::styled(left, Style::new().fg(t.c(t.fg)).bg(t.c(bg))),
                Span::styled(
                    format!(" {detail}"),
                    Style::new().fg(t.c(t.comment)).bg(t.c(bg)),
                ),
                Span::styled(
                    " ".repeat(width.saturating_sub(used)),
                    Style::new().bg(t.c(bg)),
                ),
                Span::styled(tag, Style::new().fg(t.c(t.teal)).bg(t.c(bg))),
            ]));
        }
        if matches.is_empty() {
            lines.push(Line::styled(
                "   no matches",
                Style::new().fg(t.c(t.comment)),
            ));
        }
        frame.render_widget(Paragraph::new(lines), inner);
        frame.set_cursor_position(Position::new(
            inner.x + 3 + p.input.chars().count() as u16,
            inner.y,
        ));
    }

    fn render_terminal(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme;
        let prompt = self.prompt.is_some();
        let Some(pane) = self.active_term() else {
            return;
        };
        if pane.term.size() != (area.height, area.width) && area.height > 0 && area.width > 0 {
            let _ = pane.term.resize(area.height, area.width);
        }
        pane.term.snapshot_into(&mut pane.screen);
        termview::render(frame.buffer_mut(), area, &pane.screen, &t);
        if !prompt && pane.screen.cursor.visible && !pane.exited {
            let c = pane.screen.cursor;
            if c.col < area.width && c.row < area.height {
                frame.set_cursor_position(Position::new(area.x + c.col, area.y + c.row));
            }
        }
    }

    fn render_status(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme;
        let bar_style = Style::new().bg(t.c(t.bg_dark)).fg(t.c(t.fg_dim));
        if let Some(p) = &self.prompt {
            let text = format!(" {}{}", p.label, p.input);
            frame.render_widget(Paragraph::new(text.clone()).style(bar_style), area);
            if let Some(m) = &self.editor.message {
                let used = text.chars().count() as u16 + 2;
                if area.width > used + 4 {
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
            }
            if p.kind != PromptKind::QuitConfirm {
                let x = area.x + text.chars().count() as u16;
                frame.set_cursor_position(Position::new(
                    x.min(area.right().saturating_sub(1)),
                    area.y,
                ));
            }
            return;
        }
        let b = &self.editor.buffer;
        let (mode, mode_color) = match (self.prompt.is_some(), self.focus) {
            (_, Focus::Term) => ("TERM", t.green),
            (_, Focus::Tree) => ("TREE", t.purple),
            _ if b.is_read_only() => ("VIEW", t.orange),
            _ => ("EDIT", t.blue),
        };
        let mut left: Vec<Span> = vec![Span::raw(" ")];
        left.extend(self.pill(mode, t.bg_dark, mode_color, t.bg_dark));
        if let Some(br) = &self.git_branch {
            left.push(Span::styled(
                format!("  {} {br}", self.icons.branch()),
                self.st(t.purple),
            ));
        }
        let name = match &self.tabs[self.active] {
            Tab::Term(p) if !p.screen.title.is_empty() => p.screen.title.clone(),
            Tab::Term(p) => p.label.clone(),
            Tab::Buffer(_) => b.path().map_or_else(
                || b.title().unwrap_or_else(|| "[scratch]".to_owned()),
                |p| p.display().to_string(),
            ),
        };
        left.push(Span::styled(format!("  {name}"), self.st(t.fg_dim)));
        if b.is_read_only() {
            left.push(Span::styled(
                format!(" {}", self.icons.lock()),
                self.st(t.orange),
            ));
        }
        let note = match (&self.editor.message, &self.diag_here) {
            (Some(m), _) => Some((m.clone(), t.yellow)),
            (None, Some((sev, d))) => {
                let c = match sev {
                    Severity::Error => t.red,
                    Severity::Warning => t.yellow,
                    _ => t.cyan,
                };
                Some((d.lines().next().unwrap_or("").to_owned(), c))
            }
            _ => None,
        };
        if let Some((m, c)) = note {
            left.push(Span::styled(format!("  {m}"), self.st(c)));
        }

        let mut right: Vec<Span> = Vec::new();
        let (e, w) = self.diag_status;
        if e > 0 {
            right.push(Span::styled(
                format!("{} {e} ", self.icons.error()),
                self.st(t.red),
            ));
        }
        if w > 0 {
            right.push(Span::styled(
                format!("{} {w} ", self.icons.warning()),
                self.st(t.yellow),
            ));
        }
        let delegated = self.backend == Backend::Delegated
            || self.lsp.as_ref().and_then(lsp::LspLink::backend) == Some(Backend::Delegated);
        if delegated {
            right.push(Span::styled("EveryAI ", self.st(t.teal)));
        }
        right.push(Span::styled(
            format!("{} ", b.line_ending().label()),
            self.st(t.comment),
        ));
        if let Some(p) = b.path() {
            let ft = view_filetype(p);
            if !ft.is_empty() {
                right.push(Span::styled(
                    format!("{} {ft} ", self.icons.file(p)),
                    self.st(t.blue),
                ));
            }
        }
        if e == 0 {
            right.push(Span::styled(
                format!("{} ", self.icons.check()),
                self.st(t.green),
            ));
        }
        let c = self.editor.cursor;
        right.extend(self.pill(
            &format!("{}:{}", c.line + 1, c.col + 1),
            t.bg_dark,
            t.blue,
            t.bg_dark,
        ));
        right.push(Span::raw(" "));

        let width = |v: &[Span]| v.iter().map(|s| s.content.chars().count()).sum::<usize>();
        let gap = (area.width as usize).saturating_sub(width(&left) + width(&right));
        let mut all = left;
        all.push(Span::raw(" ".repeat(gap)));
        all.extend(right);
        frame.render_widget(Paragraph::new(Line::from(all)).style(bar_style), area);
    }
}

/// File type label for the status line.
pub(crate) fn view_filetype(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "ts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "zig" => "zig",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "sh" | "bash" => "shell",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        _ => "text",
    }
}

/// Current branch from `.git/HEAD`, walking up from `path` (no git binary).
pub(crate) fn git_branch(path: &Path) -> Option<String> {
    let start = path.canonicalize().ok()?;
    for dir in start.ancestors() {
        let git = dir.join(".git");
        if git.is_dir() {
            let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
            let head = head.trim();
            return Some(match head.strip_prefix("ref: refs/heads/") {
                Some(b) => b.to_owned(),
                None => head.chars().take(7).collect(),
            });
        }
    }
    None
}
