//! Code folding: a fold hides the lines below a header. The foldable spans
//! come from the language server when it offers them (`foldingRange`),
//! else from indentation (the lines below a header indented deeper than it,
//! blank lines inside counting as part of it). Folds and server spans follow
//! edits through the buffer's edit journal; an edit that crosses a fold's
//! header or boundary opens that fold.

use crate::{Buffer, ByteEdit};

/// One closed fold: `header` stays visible, `header + 1 ..= end` are hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    pub header: usize,
    pub end: usize,
}

impl Fold {
    pub fn hides(&self, line: usize) -> bool {
        self.header < line && line <= self.end
    }

    pub fn hidden_lines(&self) -> usize {
        self.end - self.header
    }

    /// Follow one edit; `None` when the edit crossed the fold's edges.
    fn map(self, e: &ByteEdit) -> Option<Fold> {
        let (s, oe, ne) = (e.start.row, e.old_end.row, e.new_end.row);
        let delta = ne as isize - oe as isize;
        let shift = |r: usize| r.saturating_add_signed(delta);
        if oe < self.header || (oe == self.header && e.old_end.col == 0) {
            // Entirely above (ends where the header begins): move.
            return Some(Fold {
                header: shift(self.header),
                end: shift(self.end),
            });
        }
        if s > self.end {
            return Some(self);
        }
        if s == self.header && oe == self.header && delta == 0 {
            return Some(self); // typing on the header line
        }
        if s > self.header && oe <= self.end {
            // Inside the hidden body: it grows or shrinks.
            let end = shift(self.end);
            return (end > self.header).then_some(Fold {
                header: self.header,
                end,
            });
        }
        None
    }
}

/// The closed folds of a buffer, sorted and non-overlapping (a fold
/// inside a closed fold is absorbed by it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folds {
    closed: Vec<Fold>,
    /// Buffer revision the folds were last mapped to.
    rev: u64,
    source: FoldSource,
}

/// Where foldable spans come from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FoldSource {
    /// Indentation; a language server may still offer spans.
    #[default]
    Indent,
    /// Indentation only: the server has none to offer (stop asking).
    IndentOnly,
    /// The server's spans, sorted by header, kept in step with edits. A line
    /// without a span of its own still folds by indentation.
    Server {
        spans: Vec<Fold>,
        /// Buffer revision the server computed them at (older than the text
        /// means: worth asking again).
        from: u64,
    },
}

/// Indent width of a line (tabs count as 4); `None` for blank lines.
pub fn indent(buf: &Buffer, line: usize) -> Option<usize> {
    let text = buf.line_text(line);
    if text.trim().is_empty() {
        return None;
    }
    Some(
        text.chars()
            .take_while(|c| c.is_whitespace())
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum(),
    )
}

/// The fold that `line` would head: the following lines indented deeper,
/// up to the last such non-blank line.
pub fn range_at(buf: &Buffer, line: usize) -> Option<Fold> {
    let base = indent(buf, line)?;
    let n = buf.len_lines();
    let mut end = None;
    for l in line + 1..n {
        match indent(buf, l) {
            None => continue,
            Some(i) if i > base => end = Some(l),
            Some(_) => break,
        }
    }
    end.map(|end| Fold { header: line, end })
}

impl Folds {
    pub fn is_empty(&self) -> bool {
        self.closed.is_empty()
    }

    pub fn closed(&self) -> &[Fold] {
        &self.closed
    }

    /// Catch up with edits made since the last call.
    pub fn sync(&mut self, buf: &Buffer) {
        let rev = buf.revision();
        if rev == self.rev {
            return;
        }
        let edits = buf.edits_since(self.rev);
        let follow = |folds: Vec<Fold>| -> Vec<Fold> {
            let mut folds = folds;
            for e in edits.iter().flatten() {
                folds = folds.into_iter().filter_map(|f| f.map(e)).collect();
            }
            folds
        };
        match &edits {
            Some(_) => {
                self.closed = follow(std::mem::take(&mut self.closed));
                if let FoldSource::Server { spans, .. } = &mut self.source {
                    *spans = follow(std::mem::take(spans));
                }
            }
            // Too far behind (or a reloaded buffer): open everything, and
            // forget the server's spans until it sends new ones.
            None => {
                self.closed.clear();
                if matches!(self.source, FoldSource::Server { .. }) {
                    self.source = FoldSource::Indent;
                }
            }
        }
        let last = buf.len_lines().saturating_sub(1);
        self.closed.retain(|f| f.end <= last && f.header < f.end);
        if let FoldSource::Server { spans, .. } = &mut self.source {
            spans.retain(|f| f.end <= last && f.header < f.end);
        }
        self.rev = rev;
    }

    pub fn source(&self) -> &FoldSource {
        &self.source
    }

    /// The server's foldable spans, as of buffer revision `at` (they are
    /// moved through the edits made since). `(header, end)` pairs, 0-based,
    /// inclusive.
    pub fn set_server_spans(&mut self, buf: &Buffer, spans: &[(usize, usize)], at: u64) {
        self.sync(buf);
        let mut folds: Vec<Fold> = spans
            .iter()
            .filter(|(h, e)| h < e)
            .map(|&(header, end)| Fold { header, end })
            .collect();
        if at != buf.revision() {
            let Some(edits) = buf.edits_since(at) else {
                return; // too old to place: keep what there is
            };
            for e in &edits {
                folds = folds.into_iter().filter_map(|f| f.map(e)).collect();
            }
        }
        folds.sort_by_key(|f| (f.header, std::cmp::Reverse(f.end)));
        folds.dedup_by_key(|f| f.header);
        self.source = FoldSource::Server {
            spans: folds,
            from: at,
        };
    }

    /// The server offers no spans: fold by indentation and stop asking.
    pub fn server_has_none(&mut self) {
        self.source = FoldSource::IndentOnly;
    }

    /// The span `line` heads: the server's when it has one there, else by
    /// indentation.
    pub fn range_at(&self, buf: &Buffer, line: usize) -> Option<Fold> {
        if matches!(self.source, FoldSource::Server { .. }) && self.rev != buf.revision() {
            // Edits since the last sync: look at the spans where they are now.
            let mut now = self.clone();
            now.sync(buf);
            return now.range_at(buf, line);
        }
        if let FoldSource::Server { spans, .. } = &self.source
            && let Some(f) = spans.iter().find(|f| f.header == line)
        {
            return Some(*f);
        }
        range_at(buf, line)
    }

    /// The innermost server span around `line` (its header above it).
    fn server_span_around(&self, line: usize) -> Option<Fold> {
        let FoldSource::Server { spans, .. } = &self.source else {
            return None;
        };
        spans
            .iter()
            .filter(|f| f.hides(line))
            .min_by_key(|f| f.end - f.header)
            .copied()
    }

    /// The closed fold hiding `line`, if any.
    pub fn hiding(&self, line: usize) -> Option<Fold> {
        self.closed.iter().copied().find(|f| f.hides(line))
    }

    /// The closed fold headed by `line`.
    pub fn at_header(&self, line: usize) -> Option<Fold> {
        self.closed.iter().copied().find(|f| f.header == line)
    }

    pub fn is_hidden(&self, line: usize) -> bool {
        self.hiding(line).is_some()
    }

    /// Close the fold `line` heads (or, on a line with nothing to fold, the
    /// one around it). Returns the new fold.
    pub fn close(&mut self, buf: &Buffer, line: usize) -> Option<Fold> {
        self.sync(buf);
        let f = self
            .range_at(buf, line)
            .or_else(|| self.server_span_around(line))
            .or_else(|| {
                // Inside a block: fold the nearest enclosing header.
                let own = indent(buf, line)?;
                (0..line)
                    .rev()
                    .find(|&l| indent(buf, l).is_some_and(|i| i < own))
                    .and_then(|h| range_at(buf, h))
                    .filter(|f| f.end >= line)
            })?;
        self.closed
            .retain(|g| !(f.header <= g.header && g.end <= f.end));
        if self.hiding(f.header).is_some() {
            return None;
        }
        let at = self.closed.partition_point(|g| g.header < f.header);
        self.closed.insert(at, f);
        Some(f)
    }

    /// Open the fold headed by (or hiding) `line`. True when one opened.
    pub fn open(&mut self, line: usize) -> bool {
        let before = self.closed.len();
        self.closed.retain(|f| f.header != line && !f.hides(line));
        self.closed.len() != before
    }

    /// Open the fold hiding `line`. True when one opened.
    pub fn open_hiding(&mut self, line: usize) -> bool {
        let before = self.closed.len();
        self.closed.retain(|f| !f.hides(line));
        self.closed.len() != before
    }

    /// Open when closed, close when open.
    pub fn toggle(&mut self, buf: &Buffer, line: usize) {
        self.sync(buf);
        if !self.open(line) {
            self.close(buf, line);
        }
    }

    /// Close every outermost foldable block.
    pub fn close_all(&mut self, buf: &Buffer) {
        self.sync(buf);
        self.closed.clear();
        let mut line = 0;
        while line < buf.len_lines() {
            match self.range_at(buf, line) {
                Some(f) => {
                    self.closed.push(f);
                    line = f.end + 1;
                }
                None => line += 1,
            }
        }
    }

    pub fn open_all(&mut self) {
        self.closed.clear();
    }

    /// The next visible line after `line` (`None` at the end).
    pub fn next_visible(&self, line: usize, last: usize) -> Option<usize> {
        let next = match self.at_header(line) {
            Some(f) => f.end + 1,
            None => line + 1,
        };
        (next <= last).then_some(next)
    }

    /// The previous visible line before `line`.
    pub fn prev_visible(&self, line: usize) -> Option<usize> {
        let prev = line.checked_sub(1)?;
        Some(self.hiding(prev).map_or(prev, |f| f.header))
    }

    /// Up to `rows` visible lines starting at `from` (moved to its fold's
    /// header when hidden).
    pub fn visible_from(&self, from: usize, rows: usize, last: usize) -> Vec<usize> {
        let mut out = Vec::with_capacity(rows);
        let mut line = self.hiding(from).map_or(from, |f| f.header);
        while out.len() < rows && line <= last {
            out.push(line);
            match self.next_visible(line, last) {
                Some(n) => line = n,
                None => break,
            }
        }
        out
    }

    /// Visible lines in `[from, to)`.
    pub fn rows_between(&self, from: usize, to: usize) -> usize {
        let hidden: usize = self
            .closed
            .iter()
            .map(|f| {
                let lo = (f.header + 1).max(from);
                let hi = (f.end + 1).min(to);
                hi.saturating_sub(lo)
            })
            .sum();
        to.saturating_sub(from) - hidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;

    const SRC: &str = "fn a() {\n    one;\n\n    two;\n}\nfn b() {\n    three;\n}\n";

    #[test]
    fn server_spans_win_where_they_exist_and_follow_edits() {
        let src = "// region\na\nb\n// end\nfn g() {\n    x;\n}\n";
        let mut e = crate::Editor::new(Buffer::from_text(src));
        let rev = e.buffer.revision();
        e.set_fold_spans(&[(0, 3), (5, 5), (9, 2)], rev); // empty and bad spans dropped
        assert_eq!(e.fold_range_at(0), Some(Fold { header: 0, end: 3 }));
        // No server span on line 4: indentation.
        assert_eq!(e.fold_range_at(4), Some(Fold { header: 4, end: 5 }));
        // Inside a server span, folding closes it.
        e.toggle_fold_at(2);
        assert_eq!(e.folds().closed(), &[Fold { header: 0, end: 3 }]);
        e.unfold_all();
        // Spans computed before an edit are moved through it.
        let old = e.buffer.revision();
        e.buffer.insert(0, "new\n");
        e.set_fold_spans(&[(0, 3)], old);
        assert_eq!(e.fold_range_at(1), Some(Fold { header: 1, end: 4 }));
        // A server without spans: indentation only.
        e.fold_spans_unavailable();
        assert_eq!(e.folds().source(), &FoldSource::IndentOnly);
        assert_eq!(e.fold_range_at(1), None);
    }

    #[test]
    fn folds_follow_edits_above_inside_and_across() {
        let mut e = Editor::new(Buffer::from_text(SRC));
        e.toggle_fold_at(5); // fn b
        assert_eq!(e.folds().closed(), [Fold { header: 5, end: 6 }]);
        e.toggle_fold_at(0); // fn a: hides 1..=3
        assert_eq!(e.folds().closed()[0], Fold { header: 0, end: 3 });
        // Moving down from the header skips the hidden body.
        e.goto(Some(1), Some(1));
        e.move_vertical(1, false);
        assert_eq!(e.cursor.line, 4);
        // A line added above both shifts them.
        e.goto(Some(1), Some(1));
        e.insert_text("// top\n");
        e.sync_folds();
        assert_eq!(
            e.folds().closed(),
            [Fold { header: 1, end: 4 }, Fold { header: 6, end: 7 }]
        );
        // Typing on a header keeps its fold.
        e.goto(Some(2), Some(1));
        e.insert_text("pub ");
        e.sync_folds();
        assert_eq!(e.folds().closed().len(), 2);
        // Deleting across a fold's header opens it; the other stays.
        e.goto(Some(1), Some(1));
        e.move_vertical(1, true);
        e.move_vertical(1, true);
        e.backspace();
        e.sync_folds();
        assert_eq!(e.folds().closed().len(), 1);
        assert_eq!(e.buffer.line_text(e.folds().closed()[0].header), "fn b() {");
        // A cursor landing in a closed fold opens it.
        e.goto(Some(e.folds().closed()[0].header + 2), Some(1));
        assert!(e.folds().is_empty());
    }

    #[test]
    fn visible_rows_skip_closed_bodies() {
        let mut e = Editor::new(Buffer::from_text(SRC));
        e.toggle_fold_at(0);
        let f = e.folds();
        assert_eq!(f.visible_from(0, 4, 8), [0, 4, 5, 6]);
        assert_eq!(f.visible_from(2, 2, 8), [0, 4]);
        assert_eq!(f.rows_between(0, 6), 3);
        e.fold_all();
        assert_eq!(e.folds().closed().len(), 2);
        e.unfold_all();
        assert!(e.folds().is_empty());
    }
}
