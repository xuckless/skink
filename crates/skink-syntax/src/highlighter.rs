//! The incremental parser + range-limited highlight query driver.

use crate::{Hl, Lang, Span};
use std::sync::{Arc, Mutex, Weak};
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Node, Parser, Point, Query, QueryCursor, Tree};

/// A (row, byte-column) position, as tree-sitter wants for edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pos {
    pub row: usize,
    /// Byte offset within the line (not a char index).
    pub col: usize,
}

impl Pos {
    /// Position of `byte` in `text` (clamped to the text length).
    pub fn of_byte(text: &str, byte: usize) -> Pos {
        let b = &text.as_bytes()[..byte.min(text.len())];
        let row = b.iter().filter(|&&c| c == b'\n').count();
        let line_start = b.iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1);
        Pos {
            row,
            col: b.len() - line_start,
        }
    }
}

/// One text edit, expressed against the text *before* (`old_*`) and *after*
/// (`new_*`) the change. Byte offsets must be on char boundaries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_pos: Pos,
    pub old_end_pos: Pos,
    pub new_end_pos: Pos,
}

impl Edit {
    /// Builds an edit (computing the row/column positions) from the texts
    /// before and after the change. Scans each text once; if you already know
    /// the positions (e.g. from a rope) fill the struct directly instead.
    pub fn from_texts(
        old_text: &str,
        new_text: &str,
        start_byte: usize,
        old_end_byte: usize,
        new_end_byte: usize,
    ) -> Edit {
        Edit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_pos: Pos::of_byte(old_text, start_byte),
            old_end_pos: Pos::of_byte(old_text, old_end_byte),
            new_end_pos: Pos::of_byte(new_text, new_end_byte),
        }
    }

    fn to_input(self) -> InputEdit {
        let p = |p: Pos| Point {
            row: p.row,
            column: p.col,
        };
        InputEdit {
            start_byte: self.start_byte,
            old_end_byte: self.old_end_byte,
            new_end_byte: self.new_end_byte,
            start_position: p(self.start_pos),
            old_end_position: p(self.old_end_pos),
            new_end_position: p(self.new_end_pos),
        }
    }
}

/// Compiled highlight query plus the capture-index -> class table.
struct Compiled {
    query: Query,
    caps: Vec<Option<Hl>>,
}

/// Compiled queries are shared between live highlighters of the same language
/// and freed when the last one is dropped.
static CACHE: Mutex<Vec<(Lang, Weak<Compiled>)>> = Mutex::new(Vec::new());

fn compiled(lang: Lang) -> Option<Arc<Compiled>> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(_, w)| w.strong_count() > 0);
    if let Some(c) = cache
        .iter()
        .find(|(l, _)| *l == lang)
        .and_then(|(_, w)| w.upgrade())
    {
        return Some(c);
    }
    let query = Query::new(&lang.grammar(), &lang.query_source()).ok()?;
    let caps = query
        .capture_names()
        .iter()
        .map(|n| crate::hl::from_capture(n))
        .collect();
    let c = Arc::new(Compiled { query, caps });
    cache.push((lang, Arc::downgrade(&c)));
    Some(c)
}

/// Syntax highlighter for one buffer.
///
/// Holds the parse tree (the only substantial memory) and shares the compiled
/// query with other highlighters of the same language. Everything is created
/// lazily on first use and released on drop (or [`Highlighter::clear`]).
/// `Highlighter` is `Send`.
pub struct Highlighter {
    lang: Lang,
    parser: Option<Parser>,
    tree: Option<Tree>,
    query: Option<Arc<Compiled>>,
    /// Reused across `highlights` calls (it owns match-state buffers).
    cursor: Option<QueryCursor>,
    /// Length of the text the tree was built from; a mismatch means the caller
    /// forgot to call `update` and triggers a safe full re-parse.
    parsed_len: usize,
}

struct Cap {
    start: usize,
    end: usize,
    hl: Hl,
    pattern: usize,
}

impl Highlighter {
    /// Creates an empty highlighter. Allocates nothing until first use.
    pub fn new(lang: Lang) -> Self {
        Highlighter {
            lang,
            parser: None,
            tree: None,
            query: None,
            cursor: None,
            parsed_len: 0,
        }
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// Drops the parse tree, parser and query handle (memory for an inactive
    /// buffer). The highlighter stays usable and re-parses on demand.
    pub fn clear(&mut self) {
        self.parser = None;
        self.tree = None;
        self.query = None;
        self.cursor = None;
        self.parsed_len = 0;
    }

    /// Builds the parser and compiles the highlight query now instead of on
    /// first use. Compiling a query takes milliseconds (about 15 ms for Rust in
    /// release builds), so call this from a worker thread (`Highlighter` is
    /// `Send`) if opening a file must stay instant.
    pub fn prepare(&mut self) {
        if self.parser.is_none() {
            let mut p = Parser::new();
            if p.set_language(&self.lang.grammar()).is_ok() {
                self.parser = Some(p);
            }
        }
        if self.query.is_none() {
            self.query = compiled(self.lang);
        }
    }

    /// A highlighter holding the same tree, for parsing the next revision on
    /// another thread while this one keeps serving highlights. Cheap: trees
    /// are reference counted, and an incremental parse of the copy shares
    /// every unchanged subtree with the original.
    pub fn snapshot(&self) -> Highlighter {
        Highlighter {
            lang: self.lang,
            parser: None,
            tree: self.tree.clone(),
            query: self.query.clone(),
            cursor: None,
            parsed_len: self.parsed_len,
        }
    }

    /// Re-parses `text` from scratch.
    pub fn update(&mut self, text: &str) {
        self.tree = None;
        self.parse(text);
    }

    /// Re-parses `text` after a single edit, reusing the previous tree.
    pub fn update_with_edit(&mut self, text: &str, edit: &Edit) {
        self.update_with_edits(text, std::slice::from_ref(edit));
    }

    /// Like [`update_with_edit`](Self::update_with_edit) for several edits
    /// applied in order since the last update (each against the text produced
    /// by the previous one).
    pub fn update_with_edits(&mut self, text: &str, edits: &[Edit]) {
        if let Some(t) = self.tree.as_mut() {
            for e in edits {
                t.edit(&e.to_input());
            }
        }
        self.parse(text);
    }

    fn parse(&mut self, text: &str) {
        if self.parser.is_none() {
            let mut p = Parser::new();
            if p.set_language(&self.lang.grammar()).is_err() {
                self.tree = None;
                return;
            }
            self.parser = Some(p);
        }
        if let Some(p) = self.parser.as_mut() {
            self.tree = p.parse(text, self.tree.as_ref());
        }
        self.parsed_len = text.len();
    }

    fn ensure_tree(&mut self, text: &str) -> bool {
        if self.tree.is_none() || self.parsed_len != text.len() {
            self.update(text);
        }
        self.tree.is_some()
    }

    /// Highlights lines `first_line..last_line` (0-based, half-open) of `text`.
    ///
    /// Returns one `Vec<Span>` per line, in order, truncated at the end of the
    /// text (so the result can be shorter than `last_line - first_line`).
    /// Spans are non-overlapping, sorted, in **char columns** within the line
    /// (a trailing `\r` is not part of the line); the innermost capture wins,
    /// and for identical nodes the later query pattern wins. Unhighlighted
    /// stretches have no span. Only the requested byte range is queried.
    ///
    /// `text` must be the text last passed to `update*`; if its length differs
    /// the tree is rebuilt first (a safety net, not an incremental path).
    pub fn highlights(
        &mut self,
        text: &str,
        first_line: usize,
        last_line: usize,
    ) -> Vec<Vec<Span>> {
        if first_line >= last_line || !self.ensure_tree(text) {
            return Vec::new();
        }
        if self.query.is_none() {
            self.query = compiled(self.lang);
        }
        let (Some(q), Some(tree)) = (self.query.as_ref(), self.tree.as_ref()) else {
            return Vec::new();
        };

        // Byte bounds of each requested line (content only, without "\n"/"\r").
        let mut off = 0;
        for _ in 0..first_line {
            match text[off..].find('\n') {
                Some(i) => off += i + 1,
                None => return Vec::new(),
            }
        }
        let mut lines: Vec<(usize, usize)> = Vec::new();
        let mut range_end = off;
        loop {
            if lines.len() >= last_line - first_line {
                break;
            }
            let (eol, next) = match text[off..].find('\n') {
                Some(i) => (off + i, Some(off + i + 1)),
                None => (text.len(), None),
            };
            let mut end = eol;
            if end > off && text.as_bytes()[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push((off, end));
            range_end = eol;
            match next {
                Some(n) => off = n,
                None => break,
            }
        }
        let range_start = lines.first().map_or(0, |l| l.0);

        // Query only that byte range.
        let cursor = self.cursor.get_or_insert_with(QueryCursor::new);
        cursor.set_byte_range(range_start..range_end);
        let mut caps: Vec<Cap> = Vec::new();
        let mut matches = cursor.matches(&q.query, tree.root_node(), text.as_bytes());
        while let Some(m) = matches.next() {
            for c in m.captures() {
                let Some(hl) = q.caps.get(c.index as usize).copied().flatten() else {
                    continue;
                };
                let r = c.node.byte_range();
                if r.end <= range_start || r.start >= range_end || r.start == r.end {
                    continue;
                }
                caps.push(Cap {
                    start: r.start,
                    end: r.end,
                    hl,
                    pattern: m.pattern_index,
                });
            }
        }
        // Parents before children; identical nodes in pattern order.
        caps.sort_by(|a, b| {
            a.start
                .cmp(&b.start)
                .then(b.end.cmp(&a.end))
                .then(a.pattern.cmp(&b.pattern))
        });
        let segs = flatten(&caps, range_start, range_end);

        // Split into lines and convert byte offsets to char columns.
        let mut out = Vec::with_capacity(lines.len());
        let mut si = 0;
        for &(ls, le) in &lines {
            while si < segs.len() && segs[si].1 <= ls {
                si += 1;
            }
            let line = &text[ls..le];
            let ascii = line.is_ascii();
            let mut conv = ColConv {
                line,
                ascii,
                byte: 0,
                col: 0,
            };
            let mut spans = Vec::new();
            for &(s, e, hl) in &segs[si..] {
                if s >= le {
                    break;
                }
                let (s, e) = (s.max(ls) - ls, e.min(le) - ls);
                if s >= e {
                    continue;
                }
                let (cs, ce) = (conv.col_of(s), conv.col_of(e));
                if cs < ce {
                    spans.push(Span {
                        start: cs,
                        end: ce,
                        hl,
                    });
                }
            }
            out.push(spans);
        }
        out
    }

    /// Finds the bracket matching the one at (or, failing that, just before)
    /// byte offset `pos`, using the syntax tree. Returns `(open, close)` byte
    /// offsets of the pair, or `None` if `pos` is not next to a bracket token
    /// (brackets inside strings and comments are not tokens, so are ignored).
    /// Handles `()`, `[]` and `{}`.
    pub fn bracket_pair(&mut self, text: &str, pos: usize) -> Option<(usize, usize)> {
        if !self.ensure_tree(text) {
            return None;
        }
        let root = self.tree.as_ref()?.root_node();
        pair_at(root, text, pos).or_else(|| pair_at(root, text, pos.checked_sub(1)?))
    }
}

impl Highlighter {
    /// The smallest syntax node strictly larger than bytes `[start, end)`
    /// that contains it (for "expand selection"). Nodes with the same range
    /// as the selection are skipped so each call grows the selection.
    pub fn enclosing_node(
        &mut self,
        text: &str,
        start: usize,
        end: usize,
    ) -> Option<(usize, usize)> {
        if !self.ensure_tree(text) {
            return None;
        }
        let root = self.tree.as_ref()?.root_node();
        let mut node = root.descendant_for_byte_range(start, end)?;
        loop {
            let r = node.byte_range();
            if r.start <= start && r.end >= end && (r.start, r.end) != (start, end) {
                return Some((r.start, r.end));
            }
            node = node.parent()?;
        }
    }
}

fn pair_at(root: Node<'_>, text: &str, pos: usize) -> Option<(usize, usize)> {
    let b = *text.as_bytes().get(pos)?;
    let (open, close, is_open) = match b {
        b'(' => (b'(', b')', true),
        b')' => (b'(', b')', false),
        b'[' => (b'[', b']', true),
        b']' => (b'[', b']', false),
        b'{' => (b'{', b'}', true),
        b'}' => (b'{', b'}', false),
        _ => return None,
    };
    let node = root.descendant_for_byte_range(pos, pos + 1)?;
    if node.child_count() != 0 || node.byte_range() != (pos..pos + 1) {
        return None;
    }
    let is_br = |n: Node<'_>, c: u8| {
        n.child_count() == 0 && n.byte_range().len() == 1 && text.as_bytes()[n.start_byte()] == c
    };
    let mut depth = 0usize;
    let mut cur = node;
    loop {
        cur = if is_open {
            cur.next_sibling()?
        } else {
            cur.prev_sibling()?
        };
        if is_br(cur, if is_open { open } else { close }) {
            depth += 1;
        } else if is_br(cur, if is_open { close } else { open }) {
            if depth == 0 {
                let other = cur.start_byte();
                return Some(if is_open { (pos, other) } else { (other, pos) });
            }
            depth -= 1;
        }
    }
}

fn push_seg(out: &mut Vec<(usize, usize, Hl)>, a: usize, b: usize, hl: Hl) {
    if a >= b {
        return;
    }
    match out.last_mut() {
        Some(l) if l.2 == hl && l.1 == a => l.1 = b,
        _ => out.push((a, b, hl)),
    }
}

/// Flattens nested captures (sorted parents-first) into disjoint, sorted
/// segments where the innermost capture wins.
fn flatten(caps: &[Cap], rs: usize, re: usize) -> Vec<(usize, usize, Hl)> {
    let mut out = Vec::new();
    let mut stack: Vec<(usize, Hl)> = Vec::new();
    let mut pos = rs;
    for c in caps {
        let (s, e) = (c.start.max(rs), c.end.min(re));
        if s >= e {
            continue;
        }
        while let Some(&(end, hl)) = stack.last() {
            if end > s {
                break;
            }
            push_seg(&mut out, pos, end, hl);
            pos = pos.max(end);
            stack.pop();
        }
        if let Some(&(_, hl)) = stack.last() {
            push_seg(&mut out, pos, s, hl);
        }
        pos = pos.max(s);
        let e = stack.last().map_or(e, |&(pe, _)| e.min(pe));
        stack.push((e, c.hl));
    }
    while let Some((end, hl)) = stack.pop() {
        push_seg(&mut out, pos, end, hl);
        pos = pos.max(end);
    }
    out
}

/// Monotonic byte-offset -> char-column converter for one line.
struct ColConv<'a> {
    line: &'a str,
    ascii: bool,
    byte: usize,
    col: usize,
}

impl ColConv<'_> {
    fn col_of(&mut self, byte: usize) -> usize {
        if self.ascii {
            return byte;
        }
        let mut b = byte.min(self.line.len());
        while !self.line.is_char_boundary(b) {
            b -= 1;
        }
        if b < self.byte {
            self.byte = 0;
            self.col = 0;
        }
        self.col += self.line[self.byte..b].chars().count();
        self.byte = b;
        self.col
    }
}

#[cfg(test)]
mod enclosing_tests {
    use super::*;

    #[test]
    fn enclosing_nodes_grow_outwards() {
        let text = "fn main() { let x = foo(1, 22); }";
        let mut h = Highlighter::new(Lang::Rust);
        let at = text.find("22").unwrap();
        let mut r = (at, at);
        let mut seen = Vec::new();
        while let Some(n) = h.enclosing_node(text, r.0, r.1) {
            seen.push(text[n.0..n.1].to_owned());
            r = n;
        }
        assert_eq!(seen[0], "22");
        assert!(seen.contains(&"(1, 22)".to_owned()));
        assert!(seen.contains(&"foo(1, 22)".to_owned()));
        assert_eq!(seen.last().unwrap(), text);
    }
}
