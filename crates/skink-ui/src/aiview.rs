//! Drawing for the AI layers: the prompt box (editing, working, failed), the
//! inline review of an answer, the permission dialog and the refactor preview.
//! Reads state only.

use crate::{
    App,
    ai::{AskBox, Draft, Suggest, chips},
    refactor::{Hunk, PreviewState, Row},
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use skink_ai::prompter::Segment;
use unicode_width::UnicodeWidthStr;

type Rgb = (u8, u8, u8);

/// Cut `s` to at most `w` columns.
fn fit(s: &str, w: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cw > w {
            break;
        }
        used += cw;
        out.push(c);
    }
    out
}

/// Hard-wrap `s` (keeping its line breaks) to `w` columns.
fn wrap_hard(s: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut out = Vec::new();
    for line in s.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            out.push(String::new());
        }
        for chunk in chars.chunks(w) {
            out.push(chunk.iter().collect());
        }
    }
    out
}

/// Fit a styled line into `w` columns (drops what does not fit).
fn fit_line(line: Line<'static>, w: usize) -> Line<'static> {
    let mut used = 0;
    let mut spans = Vec::new();
    for sp in line.spans {
        let cw = sp.content.width();
        if used + cw <= w {
            used += cw;
            spans.push(sp);
        } else {
            spans.push(Span::styled(fit(&sp.content, w - used), sp.style));
            break;
        }
    }
    Line::from(spans)
}

/// One screen row of the inline review.
#[derive(Clone, Copy)]
enum VRow<'a> {
    /// A buffer line no change touches.
    Line(usize),
    /// A buffer line that change `hunk` removes.
    Old { hunk: usize, line: usize },
    /// A line that change `hunk` adds (virtual: not in the buffer).
    New { hunk: usize, text: &'a str },
}

/// The document with the proposal's changes spliced in, row by row.
struct ReviewRows<'a> {
    start: usize,
    region: Vec<VRow<'a>>,
    /// Buffer lines the region covers.
    consumed: usize,
    total: usize,
    /// First row of the current change.
    current_row: usize,
}

impl<'a> ReviewRows<'a> {
    fn new(segments: &'a [Segment], start: usize, current: usize, buffer_lines: usize) -> Self {
        let mut region = Vec::new();
        let (mut line, mut hunk, mut current_row) = (start, 0, start);
        for s in segments {
            match s {
                Segment::Equal(ls) => {
                    for _ in ls {
                        region.push(VRow::Line(line));
                        line += 1;
                    }
                }
                Segment::Change { old, new } => {
                    if hunk == current {
                        current_row = start + region.len();
                    }
                    for _ in old {
                        region.push(VRow::Old { hunk, line });
                        line += 1;
                    }
                    for text in new {
                        region.push(VRow::New { hunk, text });
                    }
                    hunk += 1;
                }
            }
        }
        let consumed = (line - start).min(buffer_lines.saturating_sub(start));
        ReviewRows {
            start,
            total: buffer_lines - consumed + region.len(),
            region,
            consumed,
            current_row,
        }
    }

    fn get(&self, row: usize) -> VRow<'a> {
        if row < self.start {
            VRow::Line(row)
        } else if row < self.start + self.region.len() {
            self.region[row - self.start]
        } else {
            VRow::Line(row - self.region.len() + self.consumed)
        }
    }
}

fn hunk_mark(h: Hunk) -> &'static str {
    match h {
        Hunk::Pending => "[·]",
        Hunk::Accepted => "[✓]",
        Hunk::Rejected => "[✗]",
    }
}

impl App {
    pub(crate) fn render_ai(&self, frame: &mut Frame, main: Rect, anchor: Option<(u16, u16)>) {
        if let PreviewState::Open(_) = &self.ai.preview {
            self.render_refactor(frame, main);
        }
        match &self.ai.ask {
            AskBox::Closed => {}
            AskBox::Editing(d) => self.render_draft(frame, main, anchor, d),
            // Progress is on the status line (`App::ai_status`).
            AskBox::Sending(_) | AskBox::Streaming(_) => {}
            AskBox::Failed { draft, message } => {
                let rows = wrap_hard(message, 70)
                    .into_iter()
                    .map(|l| (l, self.theme.red))
                    .collect();
                self.render_box(
                    frame,
                    main,
                    anchor,
                    draft,
                    rows,
                    " Enter edit again · Esc close ",
                );
            }
            AskBox::Reviewing(r) => self.render_review_bar(frame, main, r),
        }
        if let Some(req) = self.ai.permissions.front() {
            self.render_permission(frame, main, req);
        }
    }

    fn box_rect(main: Rect, anchor: Option<(u16, u16)>, h: u16) -> Rect {
        let w = main.width.saturating_sub(4).clamp(20, 84).min(main.width);
        let h = h.min(main.height);
        let x = main.x + main.width.saturating_sub(w) / 2;
        let y = match anchor {
            Some((_, cy)) if cy + 1 + h <= main.bottom() => cy + 1,
            Some((_, cy)) if cy >= main.y + h => cy - h,
            _ => main.bottom().saturating_sub(h),
        };
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn box_block(&self, d: &Draft, hint: &str, color: Rgb) -> Block<'static> {
        let t = &self.theme;
        let title = format!(
            " ✦ {} · {} · {} ",
            d.mode.label(),
            self.agent_label(),
            d.model.label()
        );
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.c(color)))
            .style(Style::new().bg(t.c(t.bg_popup)).fg(t.c(t.fg)))
            .title(Span::styled(
                title,
                Style::new().fg(t.c(color)).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(
                hint.to_owned(),
                Style::new().fg(t.c(t.comment)),
            ))
    }

    fn render_box(
        &self,
        frame: &mut Frame,
        main: Rect,
        anchor: Option<(u16, u16)>,
        d: &Draft,
        rows: Vec<(String, Rgb)>,
        hint: &str,
    ) {
        let rect = Self::box_rect(main, anchor, rows.len() as u16 + 2);
        let inner = rect.width.saturating_sub(2) as usize;
        let lines: Vec<Line> = rows
            .into_iter()
            .map(|(s, c)| Line::styled(fit(&s, inner), Style::new().fg(self.theme.c(c))))
            .collect();
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines).block(self.box_block(d, hint, self.theme.blue)),
            rect,
        );
    }

    fn render_draft(&self, frame: &mut Frame, main: Rect, anchor: Option<(u16, u16)>, d: &Draft) {
        let t = &self.theme;
        let width = main.width.saturating_sub(4).clamp(20, 84).min(main.width) as usize;
        let inner = width.saturating_sub(3);
        // Input, wrapped; the cursor's row and column.
        let before: String = d.input.chars().take(d.cursor).collect();
        let cur_rows = wrap_hard(&before, inner);
        let (cur_row, cur_col) = (
            cur_rows.len().saturating_sub(1),
            cur_rows.last().map_or(0, |l| l.width()),
        );
        let mut input_rows = wrap_hard(&d.input, inner);
        if input_rows.is_empty() {
            input_rows.push(String::new());
        }
        let first_shown = (cur_row + 1).saturating_sub(5);
        let shown: Vec<String> = input_rows.into_iter().skip(first_shown).take(5).collect();
        let mut lines: Vec<Line> = Vec::new();
        let placeholder = d.input.is_empty();
        for (i, l) in shown.iter().enumerate() {
            let lead = if i + first_shown == 0 { "› " } else { "  " };
            if placeholder {
                let hint = match d.mode {
                    crate::ai::BoxMode::Rewrite => "how should the selection change?",
                    crate::ai::BoxMode::Implement => "what should be written at the cursor?",
                    crate::ai::BoxMode::Ask => "ask about this code",
                    crate::ai::BoxMode::Search => "what are you looking for in the project?",
                    crate::ai::BoxMode::Chat => "message the agent",
                };
                lines.push(Line::from(vec![
                    Span::styled(lead, Style::new().fg(t.c(t.blue))),
                    Span::styled(
                        format!("{hint}  (@file #rule @selection @diagnostics @terminal)"),
                        Style::new().fg(t.c(t.comment)),
                    ),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(lead, Style::new().fg(t.c(t.blue))),
                    Span::raw(l.clone()),
                ]));
            }
        }
        // Context chips.
        let cs = chips(&d.input);
        if !cs.is_empty() {
            let root = self.project_root();
            let mut spans = vec![Span::raw("  ")];
            for c in cs {
                let color = if c.starts_with('#') {
                    t.purple
                } else if ["@selection", "@diagnostics", "@terminal"].contains(&c.as_str()) {
                    t.teal
                } else {
                    let p = c[1..].split(':').next().unwrap_or("");
                    if root.join(p).exists() {
                        t.green
                    } else {
                        t.red
                    }
                };
                spans.push(Span::styled(
                    format!(" {c} "),
                    Style::new().fg(t.c(t.bg)).bg(t.c(color)),
                ));
                spans.push(Span::raw(" "));
            }
            lines.push(Line::from(spans));
        }
        if let Suggest::List {
            items, selected, ..
        } = &d.suggest
        {
            for (i, it) in items.iter().enumerate() {
                let sel = i == *selected;
                lines.push(Line::styled(
                    fit(&format!("    {it}"), inner + 1),
                    Style::new()
                        .fg(t.c(if sel { t.blue } else { t.fg_dim }))
                        .bg(t.c(if sel { t.bg_sel } else { t.bg_popup })),
                ));
            }
        }
        let rect = Self::box_rect(main, anchor, lines.len() as u16 + 2);
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines).block(self.box_block(
                d,
                " Enter send · Shift-Tab mode · Tab model · Esc close ",
                t.blue,
            )),
            rect,
        );
        let x = rect.x + 1 + 2 + cur_col as u16;
        let y = rect.y + 1 + (cur_row - first_shown) as u16;
        if x < rect.right() && y < rect.bottom() {
            frame.set_cursor_position(Position { x, y });
        }
    }

    /// The review bar: one line at the bottom of the editor while the
    /// inline diff is shown (the help that used to be a popup).
    fn render_review_bar(&self, frame: &mut Frame, main: Rect, r: &crate::ai::Review) {
        if main.height == 0 || r.hunks.is_empty() {
            return;
        }
        let t = &self.theme;
        let n = r.hunks.len();
        let st = r.proposal.diff.stats();
        let rect = Rect {
            y: main.bottom() - 1,
            height: 1,
            ..main
        };
        let line = Line::from(vec![
            Span::styled(
                format!(
                    " ✦ change {}/{n} {} · +{} -{} ",
                    r.current + 1,
                    hunk_mark(r.hunks[r.current]),
                    st.added,
                    st.removed
                ),
                Style::new().fg(t.c(t.green)).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " y accept · n reject · ↑↓ ]/[ move · a / Ctrl-Enter accept all · Enter apply · Esc discard",
                Style::new().fg(t.c(t.comment)),
            ),
        ]);
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(fit_line(line, rect.width as usize))
                .style(Style::new().bg(t.c(t.bg_popup))),
            rect,
        );
    }

    /// While a rewrite or implement answer is reviewed: the document with the
    /// proposed change in place (removed lines struck through in red, added
    /// lines as green virtual lines). Drawn instead of the editor.
    pub(crate) fn render_inline_review(&mut self, frame: &mut Frame, area: Rect) {
        let AskBox::Reviewing(r) = &self.ai.ask else {
            return;
        };
        let (segments, hunks, current) =
            (r.proposal.diff.segments.clone(), r.hunks.clone(), r.current);
        let text = self.doc.editor.buffer.text();
        let start = r
            .proposal
            .locate(&text)
            .map_or(r.proposal.range.start.line, |c| c.start.line);
        let rows = ReviewRows::new(
            &segments,
            start,
            current,
            self.doc.editor.buffer.len_lines(),
        );
        let t = self.theme;
        let h = area.height.saturating_sub(1) as usize; // the bar below
        let top = rows
            .current_row
            .saturating_sub(h / 3)
            .min(rows.total.saturating_sub(h));
        let shown: Vec<VRow> = (top..rows.total.min(top + h))
            .map(|i| rows.get(i))
            .collect();
        let on_screen = shown.iter().filter_map(|v| match v {
            VRow::Line(l) => Some(*l),
            _ => None,
        });
        let lo = on_screen.clone().min().unwrap_or(0);
        let hi = on_screen.max().map_or(lo, |l| l + 1);
        let syntax = self.syntax_styles(lo, hi);
        let gutter = self.gutter_width();
        let width = (area.width as usize).saturating_sub(gutter);
        let ed = &self.doc.editor;
        let tab = ed.tab_width;
        let number = |l: usize| format!("{:>w$} ", l + 1, w = gutter - 2);
        let mut out = Vec::with_capacity(shown.len());
        for v in shown {
            let here =
                matches!(v, VRow::Old { hunk, .. } | VRow::New { hunk, .. } if hunk == current);
            let (sign, num, body, base, hl): (&str, String, String, Style, &[_]) = match v {
                VRow::Line(l) => (
                    " ",
                    number(l),
                    ed.buffer.line_text(l),
                    Style::new().fg(t.c(t.fg)).bg(t.c(t.bg)),
                    syntax.get(l - lo).map_or(&[][..], Vec::as_slice),
                ),
                VRow::Old { hunk, line } => {
                    let bg = t.c(if here { t.bg_hl } else { t.bg });
                    let (sign, style) = match hunks[hunk] {
                        Hunk::Rejected => (" ", Style::new().fg(t.c(t.fg))),
                        Hunk::Pending | Hunk::Accepted => (
                            "-",
                            Style::new()
                                .fg(t.c(t.red))
                                .add_modifier(Modifier::CROSSED_OUT),
                        ),
                    };
                    (
                        sign,
                        number(line),
                        ed.buffer.line_text(line),
                        style.bg(bg),
                        &[][..],
                    )
                }
                VRow::New { hunk, text } => {
                    let bg = t.c(if here { t.bg_hl } else { t.diff_ours });
                    let style = match hunks[hunk] {
                        Hunk::Rejected => Style::new()
                            .fg(t.c(t.comment))
                            .add_modifier(Modifier::CROSSED_OUT),
                        Hunk::Accepted => {
                            Style::new().fg(t.c(t.green)).add_modifier(Modifier::BOLD)
                        }
                        Hunk::Pending => Style::new().fg(t.c(t.green)),
                    };
                    (
                        "+",
                        " ".repeat(gutter - 1),
                        text.trim_end_matches(['\n', '\r']).to_owned(),
                        style.bg(bg),
                        &[][..],
                    )
                }
            };
            let sign_fg = match v {
                VRow::Line(_) => t.line_nr,
                _ if here => t.blue,
                VRow::Old { .. } => t.red,
                VRow::New { .. } => t.green,
            };
            let ls = crate::text::LineStyle {
                base,
                hl,
                sel: None,
                sel_style: base,
                sel_eol: false,
                guide: None,
                tab,
            };
            let plain = base.remove_modifier(Modifier::CROSSED_OUT | Modifier::BOLD);
            let mut row = vec![
                Span::styled(sign, plain.fg(t.c(sign_fg))),
                Span::styled(num, plain.fg(t.c(t.line_nr))),
            ];
            row.extend(crate::text::spans(&body, 0, width, &ls));
            out.push(Line::from(row));
        }
        frame.render_widget(Paragraph::new(out), area);
    }

    fn render_permission(&self, frame: &mut Frame, main: Rect, req: &skink_ai::PermissionRequest) {
        let t = &self.theme;
        let mut lines = vec![Line::styled(
            req.tool
                .title
                .clone()
                .unwrap_or_else(|| "The agent wants to run a tool".into()),
            Style::new().add_modifier(Modifier::BOLD),
        )];
        if let Some(locs) = &req.tool.locations {
            for l in locs.iter().take(4) {
                lines.push(Line::styled(
                    format!("  {}", l.path.display()),
                    Style::new().fg(t.c(t.comment)),
                ));
            }
        }
        lines.push(Line::raw(""));
        for (i, o) in req.options.iter().enumerate() {
            let sel = i == self.ai.permission_choice;
            let color = if o.kind.is_allow() { t.green } else { t.red };
            lines.push(Line::styled(
                format!(" {} {}. {} ", if sel { "›" } else { " " }, i + 1, o.name),
                Style::new()
                    .fg(t.c(color))
                    .bg(t.c(if sel { t.bg_sel } else { t.bg_popup })),
            ));
        }
        let w = 70.min(main.width);
        let h = (lines.len() as u16 + 2).min(main.height);
        let rect = Rect {
            x: main.x + (main.width - w) / 2,
            y: main.y + main.height.saturating_sub(h) / 3,
            width: w,
            height: h,
        };
        let more = self.ai.permissions.len().saturating_sub(1);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.c(t.orange)))
            .style(Style::new().bg(t.c(t.bg_popup)).fg(t.c(t.fg)))
            .title(Span::styled(
                if more > 0 {
                    format!(" Agent permission (+{more} waiting) ")
                } else {
                    " Agent permission ".to_owned()
                },
                Style::new().fg(t.c(t.orange)).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(
                " ↑↓ choose · Enter / 1-9 answer · Esc reject ",
                Style::new().fg(t.c(t.comment)),
            ));
        frame.render_widget(Clear, rect);
        frame.render_widget(Paragraph::new(lines).block(block), rect);
    }

    fn render_refactor(&self, frame: &mut Frame, main: Rect) {
        let PreviewState::Open(p) = &self.ai.preview else {
            return;
        };
        let t = &self.theme;
        frame.render_widget(Clear, main);
        let (nf, nh) = p.counts();
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.c(t.purple)))
            .style(Style::new().bg(t.c(t.bg)).fg(t.c(t.fg)))
            .title(Span::styled(
                format!(" {} · {nh} change(s) in {nf} file(s) ", p.title),
                Style::new().fg(t.c(t.purple)).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(
                " ↑↓ move · Space toggle · a all · n none · Enter apply checked · Esc cancel ",
                Style::new().fg(t.c(t.comment)),
            ));
        let inner = block.inner(main);
        frame.render_widget(block, main);
        let list_w = (inner.width * 2 / 5).max(20).min(inner.width);
        let list = Rect {
            width: list_w,
            ..inner
        };
        let diff = Rect {
            x: inner.x + list_w + 1,
            width: inner.width.saturating_sub(list_w + 1),
            ..inner
        };
        // The list of files and hunks.
        let root = self.project_root();
        let rows = p.rows();
        let h = list.height as usize;
        let skip = p.row.saturating_sub(h.saturating_sub(1));
        let lw = list.width as usize;
        let lines: Vec<Line> = rows
            .iter()
            .enumerate()
            .skip(skip)
            .take(h)
            .map(|(i, r)| {
                let sel = i == p.row;
                let bg = t.c(if sel { t.bg_sel } else { t.bg });
                match *r {
                    Row::File(f) => {
                        let fp = &p.files[f];
                        let rel = fp.path.strip_prefix(&root).unwrap_or(&fp.path);
                        let on = fp.hunks.iter().filter(|h| h.applies()).count();
                        let mark = match on {
                            0 => "[ ]",
                            n if n == fp.hunks.len() => "[x]",
                            _ => "[-]",
                        };
                        Line::styled(
                            fit(&format!("{mark} {}", rel.display()), lw),
                            Style::new()
                                .fg(t.c(t.blue))
                                .bg(bg)
                                .add_modifier(Modifier::BOLD),
                        )
                    }
                    Row::Hunk(f, hk) => {
                        let v = p.hunk_view(f, hk, 0);
                        let first = v
                            .new
                            .first()
                            .or(v.old.first())
                            .map_or(String::new(), |l| l.trim().to_owned());
                        let mark = if p.files[f].hunks[hk].applies() {
                            "[x]"
                        } else {
                            "[ ]"
                        };
                        Line::styled(
                            fit(&format!("   {mark} {}: {first}", v.first_line + 1), lw),
                            Style::new().fg(t.c(t.fg)).bg(bg),
                        )
                    }
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), list);
        // Side by side: the selected hunk (or the file's first).
        let (f, hk) = match p.current() {
            Some(Row::Hunk(f, h)) => (f, h),
            Some(Row::File(f)) => (f, 0),
            None => return,
        };
        if diff.width < 8 || diff.height == 0 {
            return; // too narrow for a side-by-side view
        }
        let v = p.hunk_view(f, hk, 3);
        let half = diff.width / 2;
        let col = |x: u16, w: u16| Rect {
            x,
            width: w,
            ..diff
        };
        let side =
            |lines: &[String], ch: &[String], color: Rgb, start: usize| -> Vec<Line<'static>> {
                let mut out = Vec::new();
                let w = half.saturating_sub(1) as usize;
                let mut n = start;
                for l in &v.before {
                    n += 1;
                    out.push(Line::styled(
                        fit(&format!("{n:>4} {}", l.trim_end_matches(['\n', '\r'])), w),
                        Style::new().fg(t.c(t.comment)),
                    ));
                }
                for l in ch {
                    n += 1;
                    out.push(Line::styled(
                        fit(&format!("{n:>4} {}", l.trim_end_matches(['\n', '\r'])), w),
                        Style::new().fg(t.c(color)).bg(t.c(t.bg_hl)),
                    ));
                }
                for l in lines {
                    n += 1;
                    out.push(Line::styled(
                        fit(&format!("{n:>4} {}", l.trim_end_matches(['\n', '\r'])), w),
                        Style::new().fg(t.c(t.comment)),
                    ));
                }
                out
            };
        frame.render_widget(
            Paragraph::new(side(&v.after, &v.old, t.red, v.first_line)),
            col(diff.x, half),
        );
        frame.render_widget(
            Paragraph::new(side(&v.after, &v.new, t.green, v.first_line)),
            col(diff.x + half, diff.width - half),
        );
    }
}
