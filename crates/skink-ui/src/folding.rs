//! Folding, sticky scroll and rulers in the editor view.

use crate::App;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use skink_core::fold;

/// Most header lines pinned at the top.
const MAX_STICKY: usize = 3;

impl App {
    /// Headers of the blocks the top of the view is inside, outermost
    /// first, as pinned at the top. They never cover the cursor's row.
    pub(crate) fn sticky_lines(&self, rows: &[usize], height: usize) -> Vec<usize> {
        if !self.settings.sticky_scroll
            || rows.first().is_none_or(|&l| l == 0)
            || self.markdown_preview().is_some()
        {
            return Vec::new();
        }
        let buf = &self.doc.editor.buffer;
        let max = MAX_STICKY.min(height / 4);
        let enclosing = |line: usize| -> Vec<usize> {
            // The indent of the line, or of the next non-blank one. A closing
            // `}` / `)` / `]` still belongs to the block it ends, so its
            // header stays pinned until that line scrolls under it.
            let Some((at, indent)) =
                (line..buf.len_lines()).find_map(|l| fold::indent(buf, l).map(|i| (l, i)))
            else {
                return Vec::new();
            };
            let closes = buf.line_text(at).trim_start().starts_with(['}', ')', ']']);
            let mut bound = indent + usize::from(closes);
            let mut out = Vec::new();
            for l in (0..line).rev() {
                if bound == 0 {
                    break;
                }
                if let Some(i) = fold::indent(buf, l)
                    && i < bound
                {
                    out.push(l);
                    bound = i;
                }
            }
            out.reverse();
            out
        };
        // n headers cover rows 0..n, so they are the n outermost blocks
        // around rows[n] that start above the view (lines on screen need no
        // pin). Take the largest n that holds: one answer per scroll
        // position, and the outer headers stay put while inner blocks come
        // and go (owner report: the top jumped down and back while
        // scrolling).
        let top = rows[0];
        let mut hdrs: Vec<usize> = Vec::new();
        for n in 1..=max {
            let Some(&target) = rows.get(n) else {
                break;
            };
            let mut chain: Vec<usize> =
                enclosing(target).into_iter().filter(|&l| l < top).collect();
            if chain.len() >= n {
                chain.truncate(n);
                hdrs = chain;
            }
        }
        // Keep the cursor's row uncovered.
        let cursor = self.doc.editor.cursor.line;
        if let Some(r) = rows.iter().position(|&l| l == cursor)
            && r < hdrs.len()
        {
            hdrs.drain(..hdrs.len() - r);
        }
        hdrs
    }

    pub(crate) fn render_sticky(
        &self,
        frame: &mut Frame,
        area: Rect,
        sticky: &[usize],
        syntax: Vec<Vec<(usize, usize, Style)>>,
        gutter: usize,
        hscroll: usize,
    ) {
        let t = &self.theme;
        let ed = &self.doc.editor;
        let width = (area.width as usize).saturating_sub(gutter);
        for (j, (&line, hl)) in sticky.iter().zip(&syntax).enumerate() {
            let bg = t.bg_dark;
            let mut base = Style::new().fg(t.c(t.fg)).bg(t.c(bg));
            if j + 1 == sticky.len() {
                base = base.add_modifier(Modifier::UNDERLINED);
            }
            let ls = crate::text::LineStyle {
                base,
                hl,
                sel: None,
                sel_style: base,
                sel_eol: false,
                guide: None,
                tab: ed.tab_width,
            };
            let text = ed.buffer.line_text(line);
            let mut row = vec![Span::styled(
                format!(" {:>w$} ", line + 1, w = gutter - 2),
                Style::new().fg(t.c(t.line_nr)).bg(t.c(bg)),
            )];
            row.extend(crate::text::spans(&text, hscroll, width, &ls));
            let r = Rect {
                y: area.y + j as u16,
                height: 1,
                ..area
            };
            frame.render_widget(
                Paragraph::new(Line::from(row)).style(Style::new().bg(t.c(bg))),
                r,
            );
        }
    }

    /// Tint the ruler columns where nothing else coloured the cell.
    pub(crate) fn render_rulers(
        &self,
        frame: &mut Frame,
        area: Rect,
        rows: usize,
        gutter: usize,
        hscroll: usize,
    ) {
        let t = &self.theme;
        let buf = frame.buffer_mut();
        for col in self.rulers() {
            let Some(x) = (col - 1).checked_sub(hscroll).map(|x| x + gutter) else {
                continue;
            };
            if x >= area.width as usize {
                continue;
            }
            for y in 0..rows.min(area.height as usize) {
                if let Some(cell) =
                    buf.cell_mut(Position::new(area.x + x as u16, area.y + y as u16))
                    && cell.bg == t.c(t.bg)
                {
                    cell.set_bg(t.c(t.bg_hl));
                }
            }
        }
    }

    /// Buffer line under screen row `row` of the editor (sticky headers,
    /// closed folds); `None` below the text.
    pub(crate) fn line_at_row(&self, row: usize, height: usize) -> Option<usize> {
        let ed = &self.doc.editor;
        let rows = ed
            .folds()
            .visible_from(ed.scroll, height, ed.buffer.len_lines() - 1);
        let sticky = self.sticky_lines(&rows, height);
        sticky
            .get(row)
            .or_else(|| rows.get(row))
            .or(rows.last())
            .copied()
    }

    /// Fold or unfold the block at the cursor.
    pub fn toggle_fold(&mut self) {
        let line = self.doc.editor.cursor.line;
        self.doc.editor.toggle_fold_at(line);
        if self.doc.editor.folds().is_empty()
            && fold::range_at(&self.doc.editor.buffer, line).is_none()
        {
            self.doc.editor.message = Some("Nothing to fold here".into());
        }
    }

    pub fn fold_all(&mut self) {
        self.doc.editor.fold_all();
    }

    pub fn unfold_all(&mut self) {
        self.doc.editor.unfold_all();
    }
}
