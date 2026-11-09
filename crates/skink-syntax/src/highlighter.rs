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

/// Compiled highlight query plus the capture-index -> class table, and the
/// injection query when the grammar has one.
struct Compiled {
    query: Query,
    caps: Vec<Option<Hl>>,
    injections: Option<Injections>,
}

struct Injections {
    query: Query,
    content: Option<u32>,
    language: Option<u32>,
}

/// How deep injections nest (Markdown → code fence → HTML → script).
const MAX_INJECTION_DEPTH: u8 = 3;
/// Injected captures sort after the host's for identical nodes.
const INJECTED_PATTERN: usize = 1 << 20;

/// One language's compiled query in the process-wide cache.
enum Slot {
    /// A worker is compiling it; highlighters show no colours meanwhile.
    Compiling,
    /// Compiled by a worker and not taken yet: the cache keeps it alive
    /// until the first highlighter picks it up.
    Fresh(Arc<Compiled>),
    /// Shared by live highlighters; freed when the last one drops it.
    Shared(Weak<Compiled>),
    /// The query does not compile (a bad bundled query): never retried.
    Failed,
}

/// What a highlighter gets when it asks for its language's query.
enum Lookup {
    Ready(Arc<Compiled>),
    /// Compiling on a worker; the ready hook fires when it is done.
    Pending,
    Failed,
}

static CACHE: Mutex<Vec<(Lang, Slot)>> = Mutex::new(Vec::new());

/// Set by the TUI: missing queries then compile on a worker and this is
/// called when one is ready (it wakes the event loop to recolour). Without a
/// hook (tests, snapshots, tools) queries compile inline on first use.
type ReadyHook = Arc<dyn Fn() + Send + Sync>;
static READY_HOOK: Mutex<Option<ReadyHook>> = Mutex::new(None);

/// Compile highlight queries off the calling thread from now on; `ready` is
/// called each time one finishes. Compiling takes milliseconds (Rust ~13 ms,
/// Markdown ~8 ms in release builds), so the TUI calls this to keep its
/// first frame instant: that frame draws uncoloured and recolours on wake.
pub fn compile_queries_in_background(ready: impl Fn() + Send + Sync + 'static) {
    *READY_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(ready));
}

/// Start compiling `lang`'s query now (on a worker when the background hook
/// is set), so it is ready by the time a file of that language is drawn.
pub fn warm_up(lang: Lang) {
    if let Lookup::Ready(c) = lookup(lang, CompileMode::Background) {
        // Compiled inline (no hook): nobody holds it, so it would be freed
        // at once. Keep it for the first highlighter.
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = cache.iter_mut().find(|(l, _)| *l == lang) {
            slot.1 = Slot::Fresh(c);
        }
    }
}

fn compile(lang: Lang) -> Option<Compiled> {
    let query = Query::new(&lang.grammar(), &lang.query_source()).ok()?;
    let caps = query
        .capture_names()
        .iter()
        .map(|n| crate::hl::from_capture(n))
        .collect();
    let injections = lang.injection_source().and_then(|src| {
        let query = Query::new(&lang.grammar(), src).ok()?;
        let content = query.capture_index_for_name("injection.content");
        let language = query.capture_index_for_name("injection.language");
        Some(Injections {
            query,
            content,
            language,
        })
    });
    Some(Compiled {
        query,
        caps,
        injections,
    })
}

/// How a highlighter gets a query that is not compiled yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileMode {
    /// On a worker when [`compile_queries_in_background`] is set (no colours
    /// until it is ready), else inline. For live documents.
    Background,
    /// Always inline (blocks once). For one-off renders whose result is
    /// kept, like a picker preview or a Markdown code block.
    Inline,
}

/// `lang`'s compiled query, shared with other highlighters of the language.
fn lookup(lang: Lang, mode: CompileMode) -> Lookup {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(_, s)| !matches!(s, Slot::Shared(w) if w.strong_count() == 0));
    if let Some((_, slot)) = cache.iter_mut().find(|(l, _)| *l == lang) {
        match slot {
            Slot::Compiling => {
                return match mode {
                    CompileMode::Background => Lookup::Pending,
                    // The worker's copy replaces this one in the cache.
                    CompileMode::Inline => match compile(lang) {
                        Some(c) => Lookup::Ready(Arc::new(c)),
                        None => Lookup::Failed,
                    },
                };
            }
            Slot::Failed => return Lookup::Failed,
            Slot::Fresh(c) => {
                let c = c.clone();
                *slot = Slot::Shared(Arc::downgrade(&c));
                return Lookup::Ready(c);
            }
            Slot::Shared(w) => {
                if let Some(c) = w.upgrade() {
                    return Lookup::Ready(c);
                }
            }
        }
    }
    cache.retain(|(l, _)| *l != lang);
    let hook = match mode {
        CompileMode::Background => READY_HOOK.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        CompileMode::Inline => None,
    };
    match hook {
        Some(hook) => {
            cache.push((lang, Slot::Compiling));
            drop(cache);
            std::thread::spawn(move || {
                let slot = match compile(lang) {
                    Some(c) => Slot::Fresh(Arc::new(c)),
                    None => Slot::Failed,
                };
                let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(s) = cache.iter_mut().find(|(l, _)| *l == lang) {
                    s.1 = slot;
                }
                drop(cache);
                hook();
            });
            Lookup::Pending
        }
        None => match compile(lang) {
            Some(c) => {
                let c = Arc::new(c);
                cache.push((lang, Slot::Shared(Arc::downgrade(&c))));
                Lookup::Ready(c)
            }
            None => {
                cache.push((lang, Slot::Failed));
                Lookup::Failed
            }
        },
    }
}

/// A highlighter's handle on its query.
#[derive(Clone)]
enum QueryHandle {
    /// Not asked for yet, or still compiling: asked again on next use.
    Unloaded,
    Loaded(Arc<Compiled>),
    /// The query does not compile: highlight nothing.
    Unavailable,
}

impl QueryHandle {
    /// Load the query if it is ready; true when one is loaded.
    fn ensure(&mut self, lang: Lang, mode: CompileMode) -> bool {
        if let QueryHandle::Unloaded = self {
            *self = match lookup(lang, mode) {
                Lookup::Ready(c) => QueryHandle::Loaded(c),
                Lookup::Pending => QueryHandle::Unloaded,
                Lookup::Failed => QueryHandle::Unavailable,
            };
        }
        matches!(self, QueryHandle::Loaded(_))
    }

    fn get(&self) -> Option<Arc<Compiled>> {
        match self {
            QueryHandle::Loaded(c) => Some(c.clone()),
            _ => None,
        }
    }
}

/// Syntax highlighter for one buffer.
///
/// Holds the parse tree (the only substantial memory) and shares the compiled
/// query with other highlighters of the same language. Everything is created
/// lazily on first use and released on drop (or [`Highlighter::clear`]).
/// `Highlighter` is `Send`.
pub struct Highlighter {
    lang: Lang,
    mode: CompileMode,
    parser: Option<Parser>,
    tree: Option<Tree>,
    query: QueryHandle,
    /// Reused across `highlights` calls (it owns match-state buffers).
    cursor: Option<QueryCursor>,
    /// Length of the text the tree was built from; a mismatch means the caller
    /// forgot to call `update` and triggers a safe full re-parse.
    parsed_len: usize,
    /// Highlighters for embedded languages, created on first injection.
    children: Vec<Highlighter>,
}

struct Cap {
    start: usize,
    end: usize,
    hl: Hl,
    pattern: usize,
}

impl Highlighter {
    /// Creates an empty highlighter for a live document. Allocates nothing
    /// until first use; the query compiles per [`CompileMode::Background`].
    pub fn new(lang: Lang) -> Self {
        Self::with_mode(lang, CompileMode::Background)
    }

    /// A highlighter whose first use compiles the query inline
    /// ([`CompileMode::Inline`]): for renders that are kept.
    pub fn new_inline(lang: Lang) -> Self {
        Self::with_mode(lang, CompileMode::Inline)
    }

    fn with_mode(lang: Lang, mode: CompileMode) -> Self {
        Highlighter {
            lang,
            mode,
            parser: None,
            tree: None,
            query: QueryHandle::Unloaded,
            cursor: None,
            parsed_len: 0,
            children: Vec::new(),
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
        self.query = QueryHandle::Unloaded;
        self.cursor = None;
        self.parsed_len = 0;
        self.children.clear();
    }

    /// Builds the parser and loads the highlight query now instead of on
    /// first use (with [`compile_queries_in_background`] the query may still
    /// be compiling afterwards).
    pub fn prepare(&mut self) {
        if self.parser.is_none() {
            let mut p = Parser::new();
            if p.set_language(&self.lang.grammar()).is_ok() {
                self.parser = Some(p);
            }
        }
        self.query.ensure(self.lang, self.mode);
    }

    /// A highlighter holding the same tree, for parsing the next revision on
    /// another thread while this one keeps serving highlights. Cheap: trees
    /// are reference counted, and an incremental parse of the copy shares
    /// every unchanged subtree with the original.
    pub fn snapshot(&self) -> Highlighter {
        Highlighter {
            lang: self.lang,
            mode: self.mode,
            parser: None,
            tree: self.tree.clone(),
            query: self.query.clone(),
            cursor: None,
            parsed_len: self.parsed_len,
            children: Vec::new(),
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

    /// Parses only `bytes` of `text`: a window around the view of a big
    /// file, so the tree (~35 bytes per source byte) costs the window, not
    /// the file. Node offsets stay those of the whole text, so
    /// [`highlights`](Self::highlights) works unchanged inside the window.
    /// Always a fresh parse (no incremental reuse).
    pub fn update_window(&mut self, text: &str, bytes: std::ops::Range<usize>) {
        let (start, end) = (bytes.start.min(text.len()), bytes.end.min(text.len()));
        let point = |b: usize| {
            let p = Pos::of_byte(text, b);
            Point {
                row: p.row,
                column: p.col,
            }
        };
        let range = tree_sitter::Range {
            start_byte: start,
            end_byte: end,
            start_point: point(start),
            end_point: point(end),
        };
        self.tree = None;
        if self.parser.is_none() {
            let mut p = Parser::new();
            if p.set_language(&self.lang.grammar()).is_err() {
                return;
            }
            self.parser = Some(p);
        }
        if let Some(p) = self.parser.as_mut()
            && p.set_included_ranges(&[range]).is_ok()
        {
            self.tree = p.parse(text, None);
            // Back to whole-text parsing for the next `update`.
            let _ = p.set_included_ranges(&[]);
        }
        self.parsed_len = text.len();
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
        if !self.query.ensure(self.lang, self.mode) || self.tree.is_none() {
            return Vec::new();
        }

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

        let mut caps = self.captures(text, range_start, range_end, 0);
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

    /// Highlight captures (byte ranges) overlapping `start..end`, including
    /// those of embedded languages found by the injection query.
    fn captures(&mut self, text: &str, start: usize, end: usize, depth: u8) -> Vec<Cap> {
        let mut caps = Vec::new();
        let (Some(q), Some(tree)) = (self.query.get(), self.tree.as_ref()) else {
            return caps;
        };
        let cursor = self.cursor.get_or_insert_with(QueryCursor::new);
        cursor.set_byte_range(start..end);
        let mut matches = cursor.matches(&q.query, tree.root_node(), text.as_bytes());
        while let Some(m) = matches.next() {
            for c in m.captures() {
                let Some(hl) = q.caps.get(c.index as usize).copied().flatten() else {
                    continue;
                };
                let r = c.node.byte_range();
                if r.end <= start || r.start >= end || r.start == r.end {
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
        if depth >= MAX_INJECTION_DEPTH {
            return caps;
        }
        let Some(inj) = &q.injections else {
            return caps;
        };
        // Collect (language, byte range) of embedded regions in view.
        let mut jobs: Vec<(Lang, usize, usize)> = Vec::new();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(start..end);
        let mut matches = cursor.matches(&inj.query, tree.root_node(), text.as_bytes());
        while let Some(m) = matches.next() {
            let mut name: Option<&str> = inj
                .query
                .property_settings(m.pattern_index)
                .iter()
                .find(|p| &*p.key == "injection.language")
                .and_then(|p| p.value.as_deref());
            let mut range = None;
            for c in m.captures() {
                if Some(c.index) == inj.language {
                    name = c.node.utf8_text(text.as_bytes()).ok();
                } else if Some(c.index) == inj.content {
                    range = Some(c.node.byte_range());
                }
            }
            if let (Some(name), Some(r)) = (name, range)
                && let Some(lang) = Lang::from_name(name.trim())
                && r.start < r.end
            {
                jobs.push((lang, r.start, r.end));
            }
        }
        for (lang, a, b) in jobs {
            let child = match self.children.iter().position(|h| h.lang == lang) {
                Some(i) => &mut self.children[i],
                None => {
                    self.children.push(Highlighter::with_mode(lang, self.mode));
                    self.children.last_mut().expect("just pushed")
                }
            };
            if !child.query.ensure(lang, child.mode) {
                continue; // still compiling: coloured on the next frame
            }
            let sub = &text[a..b];
            child.update(sub);
            let (s, e) = (start.saturating_sub(a), end.min(b) - a);
            for c in child.captures(sub, s, e, depth + 1) {
                caps.push(Cap {
                    start: c.start + a,
                    end: c.end + a,
                    hl: c.hl,
                    pattern: c.pattern + INJECTED_PATTERN * (depth as usize + 1),
                });
            }
        }
        caps
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

impl Highlighter {
    /// Named definitions enclosing byte `pos`, outermost first, as short
    /// labels (`impl Foo`, `fn bar`, `class Baz`): breadcrumbs.
    pub fn scopes(&mut self, text: &str, pos: usize) -> Vec<String> {
        if !self.ensure_tree(text) {
            return Vec::new();
        }
        let Some(root) = self.tree.as_ref().map(Tree::root_node) else {
            return Vec::new();
        };
        let Some(mut node) = root.descendant_for_byte_range(pos, pos) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        loop {
            if let Some(label) = scope_label(node, text) {
                out.push(label);
            }
            match node.parent() {
                Some(p) => node = p,
                None => break,
            }
        }
        out.reverse();
        out
    }
}

/// `fn name` for definition-like nodes, by node kind across grammars.
fn scope_label(node: Node<'_>, text: &str) -> Option<String> {
    let kind = node.kind();
    let word = match kind {
        "function_item"
        | "function_declaration"
        | "function_definition"
        | "method_declaration"
        | "method_definition"
        | "func_literal"
        | "function_signature_item" => "fn",
        "impl_item" => "impl",
        "struct_item" | "struct_specifier" => "struct",
        "enum_item" => "enum",
        "trait_item" => "trait",
        "mod_item" | "module" => "mod",
        "class_declaration" | "class_definition" | "class" => "class",
        "interface_declaration" => "interface",
        "type_declaration" | "type_item" => "type",
        "test_declaration" => "test",
        _ => return None,
    };
    let field = |f: &str| {
        node.child_by_field_name(f)
            .and_then(|n| text.get(n.byte_range()))
    };
    let name = field("name").or_else(|| field("type")).or_else(|| {
        // Zig `test "name"`, Go `type X ...` (name inside a type_spec).
        let mut cur = node.walk();
        let c = node
            .named_children(&mut cur)
            .find(|c| matches!(c.kind(), "type_spec" | "string" | "identifier"))?;
        let n = c.child_by_field_name("name").unwrap_or(c);
        text.get(n.byte_range())
    })?;
    let name: String = name.chars().take(40).collect();
    Some(format!("{word} {}", name.replace('\n', " ")))
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

#[cfg(all(test, feature = "rust"))]
mod scope_tests {
    use super::*;

    #[test]
    fn breadcrumbs_for_nested_rust_items() {
        let src = "mod m {\n    impl Foo {\n        fn bar() {\n            let x = 1;\n        }\n    }\n}\n";
        let mut h = Highlighter::new(Lang::Rust);
        h.update(src);
        let at = src.find("let").unwrap();
        assert_eq!(h.scopes(src, at), ["mod m", "impl Foo", "fn bar"]);
        assert!(h.scopes(src, 0).is_empty() || h.scopes(src, 0) == ["mod m"]);
    }
}
