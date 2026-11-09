//! Drawing for the AI layers: the prompt box (editing, working, failed), the
//! inline review of an answer, the permission dialog and the refactor preview.
//! Reads state only.

use crate::{
    App,
    ai::{AskBox, Draft, Stage, Suggest, chips},
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
            AskBox::Sending(j) | AskBox::Streaming(j) => {
                let secs = j.started.elapsed().as_secs();
                let what = match &j.stage {
                    Stage::WaitAgent => format!("starting {}", self.agent_label()),
                    Stage::OpenSession { .. } => "opening a session".to_owned(),
                    Stage::SetModel { .. } => format!("switching to {}", j.draft.model.label()),
                    Stage::Running { .. } if j.reply.is_empty() => "thinking".to_owned(),
                    Stage::Running { .. } => format!("writing ({} chars)", j.reply.chars().count()),
                };
                let mut rows = vec![(format!("◐ {what} · {secs}s"), self.theme.blue)];
                if !j.activity.is_empty() {
                    rows.push((format!("  {}", j.activity), self.theme.comment));
                }
                let tail: Vec<&str> = j.reply.lines().rev().take(3).collect();
                for l in tail.into_iter().rev() {
                    rows.push((format!("  {l}"), self.theme.fg_dim));
                }
                self.render_box(frame, main, anchor, &j.draft, rows, " Esc cancel ");
            }
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
            AskBox::Reviewing(r) => self.render_review(frame, main, anchor, r),
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

    fn render_review(
        &self,
        frame: &mut Frame,
        main: Rect,
        anchor: Option<(u16, u16)>,
        r: &crate::ai::Review,
    ) {
        let t = &self.theme;
        let n = r.hunks.len();
        let mut lines: Vec<Line> = Vec::new();
        let mut current_at = 0;
        let mut idx = 0;
        for s in &r.proposal.diff.segments {
            let Segment::Change { old, new } = s else {
                continue;
            };
            let here = idx == r.current;
            if here {
                current_at = lines.len();
            }
            let head = format!(
                "{} change {}/{n}{}",
                hunk_mark(r.hunks[idx]),
                idx + 1,
                if here { "  ◀" } else { "" }
            );
            lines.push(Line::styled(
                head,
                Style::new()
                    .fg(t.c(if here { t.blue } else { t.comment }))
                    .add_modifier(if here {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ));
            for l in old {
                lines.push(Line::styled(
                    format!("- {}", l.trim_end_matches(['\n', '\r'])),
                    Style::new().fg(t.c(t.red)),
                ));
            }
            for l in new {
                lines.push(Line::styled(
                    format!("+ {}", l.trim_end_matches(['\n', '\r'])),
                    Style::new().fg(t.c(t.green)),
                ));
            }
            idx += 1;
        }
        let max_h = (main.height * 2 / 3).max(6);
        let body_h = max_h.saturating_sub(2) as usize;
        let skip = current_at.saturating_sub(body_h / 3);
        let shown: Vec<Line> = lines.into_iter().skip(skip).take(body_h).collect();
        let rect = Self::box_rect(main, anchor, shown.len() as u16 + 2);
        let st = r.proposal.diff.stats();
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.c(t.green)))
            .style(Style::new().bg(t.c(t.bg_popup)).fg(t.c(t.fg)))
            .title(Span::styled(
                format!(" ✦ review · +{} -{} ", st.added, st.removed),
                Style::new().fg(t.c(t.green)).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(
                " y accept · n reject · ↑↓ move · Enter apply · a accept all · Esc discard ",
                Style::new().fg(t.c(t.comment)),
            ));
        frame.render_widget(Clear, rect);
        frame.render_widget(Paragraph::new(shown).block(block), rect);
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
