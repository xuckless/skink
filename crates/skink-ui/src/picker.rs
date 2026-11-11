//! Quick open: one list for open tabs, recent files and project files, with
//! a small subsequence matcher (kept in-house: it is small, ranks file names
//! first and supports path segments; frizbee was considered but its SIMD
//! matcher solves a throughput problem we do not have at picker sizes).
//!
//! Project files stream in from a worker thread (`ignore`-aware walk, no file
//! cap), so the list is usable immediately in huge repositories.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
};

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Tab(usize),
    File(PathBuf),
    /// A place in a file (0-based line and char column).
    Location {
        path: PathBuf,
        line: usize,
        col: usize,
    },
    /// A language-server code action to apply.
    Action(Box<skink_lsp::CodeAction>),
    /// An action or `:` command line to run (command palette).
    Command(String),
    /// Item `index` of a list a script opened with `skink.pick`.
    Script {
        index: usize,
        label: String,
    },
}

/// What the list shows. Quick open switches by the first typed character:
/// `>` commands, `@` symbols in this file, `#` symbols in the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Files,
    Commands,
    Outline,
    Symbols,
    /// A fixed list from elsewhere (code actions, several definitions).
    Choices(String),
}

impl Mode {
    /// The mode quick-open input asks for.
    pub fn for_input(input: &str) -> Mode {
        match input.chars().next() {
            Some('>') => Mode::Commands,
            Some('@') => Mode::Outline,
            Some('#') => Mode::Symbols,
            _ => Mode::Files,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Mode::Files => "Quick open",
            Mode::Commands => "Commands",
            Mode::Outline => "Symbols in file",
            Mode::Symbols => "Symbols in project",
            Mode::Choices(t) => t,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub label: String,
    pub detail: String,
    pub target: Target,
    /// Where it came from, for the right-hand tag.
    pub tag: &'static str,
}

pub struct Picker {
    pub mode: Mode,
    pub input: String,
    pub items: Vec<Item>,
    pub selected: usize,
    /// Lower-cased match key per item (its relative path, or label).
    keys: Vec<String>,
    /// Indices of matching items, best first; recomputed only when the
    /// input changes, not on every frame or arrow key.
    ranked: Vec<usize>,
    /// Paths already listed (tabs and recents win over the walk).
    seen: HashSet<PathBuf>,
    root: PathBuf,
    pub walk: Walk,
    pub preview: Preview,
}

/// At most this many matches are ranked and shown.
const MAX_SHOWN: usize = 200;

/// Higher is better; `None` when `query` is not a subsequence of `text`.
#[cfg(test)]
fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    score_prepared(&prepare_query(query), &text.to_lowercase())
}

pub(crate) fn prepare_query(query: &str) -> Vec<char> {
    query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// `fuzzy_score` with a prepared query and an already lower-cased `text`.
pub(crate) fn score_prepared(q: &[char], text: &str) -> Option<i32> {
    rank(q, text).map(|(s, _)| s)
}

/// Score and matched char positions of `q` in `text` (lower-cased).
///
/// * A query with `/` matches path segments in order: `s/c/grade` finds
///   `src/color/grade.rs` (each segment fuzzy within one path component).
/// * Otherwise a hit inside the file name always beats one spread over
///   directories: typing "ed" means editor.rs, not crates/.../doc.rs.
pub(crate) fn rank(q: &[char], text: &str) -> Option<(i32, Vec<usize>)> {
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let t: Vec<char> = text.chars().collect();
    if q.contains(&'/') {
        return rank_segments(q, &t);
    }
    let name_start = t
        .iter()
        .rposition(|c| *c == '/' || *c == '\\')
        .map_or(0, |i| i + 1);
    let name = &t[name_start..];
    if let Some((s, pos)) = best_alignment(q, name) {
        let pos = pos.into_iter().map(|i| i + name_start).collect();
        return Some((10_000 + s * 10 - name.len() as i32, pos));
    }
    best_alignment(q, &t).map(|(s, pos)| (s * 10 - t.len() as i32 / 4, pos))
}

/// Path-segment matching: each `/`-separated query segment must match (as a
/// subsequence) inside one path component, components in increasing order.
fn rank_segments(q: &[char], t: &[char]) -> Option<(i32, Vec<usize>)> {
    let segs: Vec<&[char]> = q.split(|c| *c == '/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return Some((0, Vec::new()));
    }
    // Components as (start, end) char ranges.
    let mut comps = Vec::new();
    let mut start = 0;
    for (i, c) in t.iter().enumerate() {
        if *c == '/' || *c == '\\' {
            comps.push((start, i));
            start = i + 1;
        }
    }
    comps.push((start, t.len()));
    let (mut ci, mut score, mut pos) = (0, 0i32, Vec::new());
    for (si, seg) in segs.iter().enumerate() {
        // The last segment prefers the file name (the last component).
        let order: Vec<usize> = if si + 1 == segs.len() {
            (ci..comps.len()).rev().collect()
        } else {
            (ci..comps.len()).collect()
        };
        let (k, s, p) = order.into_iter().find_map(|k| {
            let (a, b) = comps[k];
            best_alignment(seg, &t[a..b]).map(|(s, p)| (k, s, p.into_iter().map(move |i| i + a)))
        })?;
        score += s + if k + 1 == comps.len() { 5 } else { 0 };
        pos.extend(p);
        ci = k + 1;
    }
    Some((20_000 + score * 10 - t.len() as i32 / 4, pos))
}

/// Best score of `q` as a subsequence of `t`, trying every start position of
/// the first char (greedy from the leftmost one misses `ed` in `crates/editor`).
fn best_alignment(q: &[char], t: &[char]) -> Option<(i32, Vec<usize>)> {
    (0..t.len())
        .filter(|&s| t[s] == q[0])
        .filter_map(|s| align_from(q, t, s))
        .max_by_key(|(s, _)| *s)
}

fn align_from(q: &[char], t: &[char], start: usize) -> Option<(i32, Vec<usize>)> {
    let (mut qi, mut score, mut prev) = (0, 0i32, None::<usize>);
    let mut pos = Vec::with_capacity(q.len());
    for (i, &c) in t.iter().enumerate().skip(start) {
        if qi == q.len() {
            break;
        }
        if c != q[qi] {
            continue;
        }
        score += 1;
        if i == 0 {
            score += 6; // prefix
        } else if !t[i - 1].is_alphanumeric() {
            score += 3; // word start
        }
        if prev.is_some_and(|p| p + 1 == i) {
            score += 4; // contiguous run
        }
        prev = Some(i);
        pos.push(i);
        qi += 1;
    }
    (qi == q.len()).then_some((score, pos))
}

/// Directories never worth listing, even without a `.gitignore`.
const SKIP: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    "venv",
];

/// A walker over `root`: `.gitignore`-aware, hidden entries and the usual
/// build directories skipped.
fn walker(root: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .filter_entry(|e| {
            !(e.file_type().is_some_and(|t| t.is_dir())
                && SKIP.contains(&e.file_name().to_string_lossy().as_ref()))
        })
        .build()
}

/// Files under `root`, up to `limit` (synchronous; tests and small callers).
pub fn project_files(root: &Path, limit: usize) -> Vec<PathBuf> {
    walker(root)
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
        .take(limit)
        .collect()
}

/// Paths per message from the walker thread.
const BATCH: usize = 1024;

/// Walk `root` on a worker thread, streaming files in batches. The walk stops
/// as soon as the receiver is dropped (the picker closed).
pub fn stream_project_files(
    roots: Vec<PathBuf>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
) -> mpsc::Receiver<Vec<PathBuf>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut batch = Vec::with_capacity(BATCH);
        let send = |batch: Vec<PathBuf>| {
            let ok = tx.send(batch).is_ok();
            if let Some(w) = &wake {
                w();
            }
            ok
        };
        for e in roots.iter().flat_map(|r| walker(r)).filter_map(Result::ok) {
            if e.file_type().is_some_and(|t| t.is_file()) {
                batch.push(e.into_path());
                if batch.len() == BATCH
                    && !send(std::mem::replace(&mut batch, Vec::with_capacity(BATCH)))
                {
                    return;
                }
            }
        }
        if !batch.is_empty() {
            send(batch);
        }
        // Dropping `tx` tells the picker the walk finished.
    });
    rx
}

/// The project-file walk behind quick open.
pub enum Walk {
    /// No walk (choices lists, tests).
    None,
    /// Files are still arriving.
    Streaming {
        root: PathBuf,
        rx: mpsc::Receiver<Vec<PathBuf>>,
    },
    /// The walk finished with this many project files.
    Done { files: usize },
}

/// What the preview pane shows for the selected item.
pub enum Preview {
    Empty,
    /// The first lines of a text file, with syntax spans per line.
    Text {
        path: PathBuf,
        /// File line (0-based) of `lines[0]`.
        first: usize,
        /// Index into `lines` to put near the top: a symbol or match location.
        focus: usize,
        lines: Vec<String>,
        spans: Vec<Vec<skink_syntax::Span>>,
    },
    /// Binary or unreadable.
    Unavailable {
        path: PathBuf,
    },
}

/// Most of a file the preview reads.
const PREVIEW_BYTES: u64 = 256 << 10;
/// Lines highlighted and kept for the preview.
const PREVIEW_LINES: usize = 400;

fn load_preview(path: &Path, focus: usize) -> Preview {
    use std::io::Read;
    let mut buf = Vec::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(PREVIEW_BYTES).read_to_end(&mut buf));
    if read.is_err() || buf.contains(&0) {
        return Preview::Unavailable {
            path: path.to_path_buf(),
        };
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let first = focus.saturating_sub(PREVIEW_LINES / 4);
    let lines: Vec<String> = text
        .lines()
        .skip(first)
        .take(PREVIEW_LINES)
        .map(str::to_owned)
        .collect();
    let spans = match skink_syntax::language_for_path(path) {
        Some(lang) => {
            let mut h = skink_syntax::Highlighter::new_inline(lang);
            h.update(&text);
            h.highlights(&text, first, first + lines.len())
        }
        None => Vec::new(),
    };
    Preview::Text {
        path: path.to_path_buf(),
        first,
        focus: focus - first,
        lines,
        spans,
    }
}

impl Picker {
    pub fn new(tabs: Vec<Item>, recent: &[PathBuf], project: Vec<PathBuf>, root: &Path) -> Self {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut items = Vec::new();
        for t in tabs {
            if let Target::File(p) = &t.target {
                seen.insert(p.clone());
            }
            items.push(t);
        }
        let rel = |p: &Path| {
            p.strip_prefix(root)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned()
        };
        let mut add = |p: &PathBuf, tag: &'static str, items: &mut Vec<Item>| {
            if seen.insert(p.clone()) {
                let name = p
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                items.push(Item {
                    label: name,
                    detail: rel(p),
                    target: Target::File(p.clone()),
                    tag,
                });
            }
        };
        for p in recent {
            if p.is_file() {
                add(p, "recent", &mut items);
            }
        }
        for p in &project {
            add(p, "", &mut items);
        }
        let keys = items
            .iter()
            .map(|i| {
                if i.detail.is_empty() {
                    &i.label
                } else {
                    &i.detail
                }
                .to_lowercase()
            })
            .collect();
        let mut p = Self {
            mode: Mode::Files,
            input: String::new(),
            items,
            selected: 0,
            keys,
            ranked: Vec::new(),
            seen,
            root: root.to_path_buf(),
            walk: Walk::None,
            preview: Preview::Empty,
        };
        p.rerank();
        p
    }

    /// Stream project files from `rx` into the list (see [`stream_project_files`]).
    pub fn attach_walk(&mut self, rx: mpsc::Receiver<Vec<PathBuf>>) {
        self.walk = Walk::Streaming {
            root: self.root.clone(),
            rx,
        };
    }

    /// Take whatever the walker sent since the last call; returns whether the
    /// list changed. The selected item stays selected while results arrive.
    pub fn poll_walk(&mut self) -> bool {
        let Walk::Streaming { rx, root } = &self.walk else {
            return false;
        };
        let root = root.clone();
        let mut got = Vec::new();
        let finished = loop {
            match rx.try_recv() {
                Ok(batch) => got.extend(batch),
                Err(mpsc::TryRecvError::Empty) => break false,
                Err(mpsc::TryRecvError::Disconnected) => break true,
            }
        };
        let changed = !got.is_empty();
        if changed {
            let keep = self.current();
            for p in got {
                if self.seen.insert(p.clone()) {
                    let detail = p
                        .strip_prefix(&root)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .into_owned();
                    let label = p
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    if self.mode == Mode::Files {
                        self.keys.push(detail.to_lowercase());
                        self.items.push(Item {
                            label,
                            detail,
                            target: Target::File(p),
                            tag: "",
                        });
                    }
                }
            }
            self.rerank();
            if let Some(k) = keep
                && let Some(i) = self.ranked.iter().position(|&i| self.items[i].target == k)
            {
                self.selected = i;
            }
        }
        if finished {
            let files = self.items.iter().filter(|i| i.tag.is_empty()).count();
            self.walk = Walk::Done { files };
        }
        changed || finished
    }

    /// Matched char positions in the item's label and detail (for highlighting).
    pub fn positions(&self, item: &Item) -> (Vec<usize>, Vec<usize>) {
        let q = prepare_query(self.query());
        if q.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let key_is_detail = matches!(self.mode, Mode::Files) && !item.detail.is_empty();
        let key = if key_is_detail {
            &item.detail
        } else {
            &item.label
        };
        let Some((_, pos)) = rank(&q, &key.to_lowercase()) else {
            return (Vec::new(), Vec::new());
        };
        if !key_is_detail {
            return (pos, Vec::new());
        }
        // Positions in the file-name part of the detail also light up the label.
        let name_start = item.detail.chars().count() - item.label.chars().count();
        let label = pos
            .iter()
            .filter(|&&i| i >= name_start && item.detail.ends_with(&item.label))
            .map(|&i| i - name_start)
            .collect();
        (label, pos)
    }

    /// Load the preview for the selected item when it changed.
    pub fn refresh_preview(&mut self) {
        let (path, focus) = match self.current() {
            Some(Target::File(p)) => (p, 0),
            Some(Target::Location { path, line, .. }) => (path, line),
            _ => {
                self.preview = Preview::Empty;
                return;
            }
        };
        let same = match &self.preview {
            Preview::Text { path: p, .. } | Preview::Unavailable { path: p } => *p == path,
            Preview::Empty => false,
        };
        if !same {
            self.preview = load_preview(&path, focus);
        }
    }

    /// A list that is not quick open (code actions, definitions).
    pub fn choices(title: &str, items: Vec<Item>) -> Self {
        let mut p = Self::new(Vec::new(), &[], Vec::new(), Path::new(""));
        p.set_mode(Mode::Choices(title.to_owned()), items);
        p
    }

    /// Switch what the list shows, keeping the typed input.
    pub fn set_mode(&mut self, mode: Mode, items: Vec<Item>) {
        self.mode = mode;
        self.set_items(items);
    }

    /// Replace the items (async results), keeping input and mode.
    pub fn set_items(&mut self, items: Vec<Item>) {
        self.keys = items
            .iter()
            .map(|i| match self.mode {
                Mode::Files if !i.detail.is_empty() => i.detail.to_lowercase(),
                // Commands also match their description ("terminal",
                // "search", "multiple cursors").
                Mode::Commands => format!("{} {}", i.label, i.detail).to_lowercase(),
                _ => i.label.to_lowercase(),
            })
            .collect();
        self.items = items;
        self.rerank();
    }

    /// The input without the mode character.
    pub fn query(&self) -> &str {
        match self.mode {
            Mode::Commands | Mode::Outline | Mode::Symbols => {
                self.input.get(1..).unwrap_or_default()
            }
            Mode::Files | Mode::Choices(_) => &self.input,
        }
    }

    fn rerank(&mut self) {
        self.selected = 0;
        let q = prepare_query(self.query());
        if q.is_empty() {
            self.ranked = (0..self.items.len().min(MAX_SHOWN)).collect();
            return;
        }
        let mut scored: Vec<(i32, usize)> = self
            .keys
            .iter()
            .enumerate()
            .filter_map(|(i, k)| {
                // Open tabs and recent files win ties against the rest.
                let boost = match self.items[i].tag {
                    "open" => 30,
                    "recent" => 15,
                    _ => 0,
                };
                score_prepared(&q, k).map(|s| (s + boost, i))
            })
            .collect();
        // Stable: equal scores keep the tabs / recent / project order.
        scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        self.ranked = scored.into_iter().take(MAX_SHOWN).map(|(_, i)| i).collect();
    }

    /// Items matching the input, best first.
    pub fn matches(&self) -> Vec<&Item> {
        self.ranked.iter().map(|&i| &self.items[i]).collect()
    }

    pub fn current(&self) -> Option<Target> {
        let i = *self.ranked.get(self.selected)?;
        Some(self.items[i].target.clone())
    }

    pub fn move_selection(&mut self, delta: isize) {
        let n = self.ranked.len();
        if n > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        }
    }

    pub fn type_char(&mut self, c: char) {
        self.input.push(c);
        self.rerank();
    }

    pub fn clear(&mut self) {
        self.input.clear();
        self.rerank();
    }

    pub fn backspace(&mut self) {
        self.input.pop();
        self.rerank();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_hits_beat_scattered_path_hits() {
        // Regression: "ed" ranked crates/skink-ui/src/doc.rs above editor.rs.
        let a = fuzzy_score("ed", "crates/skink-core/src/editor.rs").unwrap();
        let b = fuzzy_score("ed", "crates/skink-ui/src/doc.rs").unwrap();
        assert!(a > b, "{a} vs {b}");
        // Directory-only matches still work.
        assert!(fuzzy_score("ui", "crates/skink-ui/src/doc.rs").is_some());
        // Best alignment, not leftmost: contiguous "view" in the name.
        assert!(fuzzy_score("view", "crates/v/i/e/w/view.rs").unwrap() > 10_000);
    }

    #[test]
    fn path_segments_match_components_in_order() {
        let q = prepare_query("s/c/grade");
        let (score, pos) = rank(&q, "src/color/grade.rs").unwrap();
        assert!(score > 20_000);
        // s, c, g r a d e
        assert_eq!(pos, [0, 4, 10, 11, 12, 13, 14]);
        // Order matters: the components must appear in sequence.
        assert!(rank(&prepare_query("grade/c"), "src/color/grade.rs").is_none());
        // Plain queries report positions in the file name.
        let (_, pos) = rank(&prepare_query("ed"), "crates/skink-core/src/editor.rs").unwrap();
        assert_eq!(pos, [22, 23]);
    }

    #[test]
    fn streamed_files_join_the_list_and_keep_the_selection() {
        let (tx, rx) = mpsc::channel();
        let tabs = vec![Item {
            label: "a.rs".into(),
            detail: "a.rs".into(),
            target: Target::File("/p/a.rs".into()),
            tag: "open",
        }];
        let mut p = Picker::new(tabs, &[], Vec::new(), Path::new("/p"));
        p.attach_walk(rx);
        tx.send(vec!["/p/a.rs".into(), "/p/src/b.rs".into()])
            .unwrap();
        assert!(p.poll_walk());
        assert_eq!(p.items.len(), 2, "duplicate of the open tab skipped");
        assert_eq!(p.current(), Some(Target::File("/p/a.rs".into())));
        drop(tx);
        p.poll_walk();
        assert!(matches!(p.walk, Walk::Done { files: 1 }));
    }

    #[test]
    fn subsequence_ranking() {
        assert!(fuzzy_score("xyz", "abc").is_none());
        let a = fuzzy_score("edrs", "src/editor.rs").unwrap();
        let b = fuzzy_score("edrs", "src/some/deeper/rs_stuff.txt").unwrap();
        assert!(a > b);
        // File-name hits beat directory hits.
        assert!(
            fuzzy_score("main", "src/main.rs").unwrap()
                > fuzzy_score("main", "main_dir/other.rs").unwrap()
        );
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn picker_orders_tabs_recent_project_without_duplicates() {
        let root = Path::new("/p");
        let tabs = vec![Item {
            label: "a.rs".into(),
            detail: "a.rs".into(),
            target: Target::File("/p/a.rs".into()),
            tag: "open",
        }];
        let dir = std::env::temp_dir().join(format!("skink-picker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rf = dir.join("r.txt");
        std::fs::write(&rf, "").unwrap();
        let p = Picker::new(
            tabs,
            &[rf.clone(), "/gone/missing".into()],
            vec!["/p/a.rs".into(), "/p/b.rs".into()],
            root,
        );
        let labels: Vec<_> = p.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["a.rs", "r.txt", "b.rs"]);
        let mut p = p;
        p.type_char('b');
        assert_eq!(p.current(), Some(Target::File("/p/b.rs".into())));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn project_walk_skips_hidden_and_build_dirs() {
        let d = std::env::temp_dir().join(format!("skink-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for sub in ["src", ".git", "target", "node_modules"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
            std::fs::write(d.join(sub).join("f.txt"), "").unwrap();
        }
        std::fs::write(d.join("top.txt"), "").unwrap();
        let mut names: Vec<_> = project_files(&d, 100)
            .iter()
            .map(|p| {
                p.strip_prefix(&d)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        names.sort();
        assert_eq!(names, ["src/f.txt", "top.txt"]);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
