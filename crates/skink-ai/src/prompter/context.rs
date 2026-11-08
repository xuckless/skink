//! Context assembly (idea from ThePrimeagen/99, re-implemented; that project
//! has no license so no code was copied): `@path` file references and `#rule`
//! references expanded from the prompt text, `AGENTS.md` / `AGENT.md` walk-up
//! from the file's directory to the project root, plus selection, surrounding
//! lines and diagnostics.
//!
//! Pure functions over a [`ProjectFiles`] abstraction so unsaved editor
//! buffers can be served and tests need no disk.

use std::fs;
use std::path::{Component, Path, PathBuf};

use super::coords::{CharPos, CharRange, TextIndex};
use crate::agent::markdown_fence;

/// Read access to project files (disk, or editor buffers overlaying disk).
pub trait ProjectFiles {
    /// Text of the file, `None` if missing/unreadable/not UTF-8.
    fn read(&self, path: &Path) -> Option<String>;
    fn is_file(&self, path: &Path) -> bool;
}

/// Plain disk access.
pub struct DiskFiles;

impl ProjectFiles for DiskFiles {
    fn read(&self, path: &Path) -> Option<String> {
        fs::read(path).ok().and_then(|b| String::from_utf8(b).ok())
    }
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextConfig {
    /// Project root (walk-up stops here; references must stay inside).
    pub root: PathBuf,
    /// Instruction file names to look for in every directory, in preference order.
    pub agent_doc_names: Vec<String>,
    /// Directories (relative to the root) holding `#rule` files.
    pub rule_dirs: Vec<PathBuf>,
    pub lines_before: usize,
    pub lines_after: usize,
    pub max_reference_bytes: usize,
    pub max_total_reference_bytes: usize,
    pub max_agent_doc_bytes: usize,
    pub max_diagnostics: usize,
}

impl ContextConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            agent_doc_names: vec!["AGENTS.md".into(), "AGENT.md".into()],
            rule_dirs: vec![".skink/rules".into(), ".cursor/rules".into()],
            lines_before: 20,
            lines_after: 20,
            max_reference_bytes: 32 * 1024,
            max_total_reference_bytes: 128 * 1024,
            max_agent_doc_bytes: 16 * 1024,
            max_diagnostics: 20,
        }
    }
}

/// The buffer the request is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSnapshot {
    /// Absolute path (or relative to the config root).
    pub path: PathBuf,
    /// Current buffer text, saved or not.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Hint => "hint",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: CharRange,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDoc {
    pub path: PathBuf,
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    File,
    Rule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub kind: RefKind,
    /// The token as typed, e.g. `@src/a.rs:10-20` or `#style`.
    pub token: String,
    pub path: PathBuf,
    /// 1-based inclusive line window, if the token had one.
    pub lines: Option<(usize, usize)>,
    pub content: String,
    pub truncated: bool,
}

/// The selected region with its surroundings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionContext {
    /// Region that a rewrite would replace.
    pub range: CharRange,
    pub before: String,
    pub text: String,
    pub after: String,
    /// 1-based line numbers of the first and last selected line.
    pub first_line: usize,
    pub last_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Assembled {
    /// Path as shown to the agent (relative to the root when possible).
    pub file: String,
    pub language: Option<&'static str>,
    pub selection: Option<SelectionContext>,
    pub agent_docs: Vec<AgentDoc>,
    pub references: Vec<Reference>,
    /// Reference tokens that could not be resolved (kept as plain text).
    pub unresolved: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Language name for fences/prompts from a file extension.
pub fn language_for(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "zig" => "zig",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" | "cxx" => "cpp",
        "java" => "java",
        "lua" => "lua",
        "rb" => "ruby",
        "sh" | "bash" => "bash",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" => "markdown",
        "html" => "html",
        "css" => "css",
        _ => return None,
    })
}

/// Lexically normalise (`.`/`..`); `None` if `..` escapes.
fn normalize(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            o => out.push(o.as_os_str()),
        }
    }
    Some(out)
}

/// Path shown to the agent: relative to `root` with `/` separators when inside it.
pub fn display_path(root: &Path, path: &Path) -> String {
    let p = path.strip_prefix(root).unwrap_or(path);
    p.to_string_lossy().replace('\\', "/")
}

fn truncate_utf8(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut e = max;
    while !s.is_char_boundary(e) {
        e -= 1;
    }
    (&s[..e], true)
}

// ---- AGENTS.md walk-up --------------------------------------------------

/// Instruction files from the project root down to the directory of `file`
/// (general first, most specific last, so later text can override earlier).
/// A file outside the root only gets its own directory's file.
pub fn collect_agent_docs(
    cfg: &ContextConfig,
    files: &dyn ProjectFiles,
    file: &Path,
) -> Vec<AgentDoc> {
    let abs = if file.is_absolute() {
        file.to_path_buf()
    } else {
        cfg.root.join(file)
    };
    let Some(start) = abs.parent().map(Path::to_path_buf) else {
        return Vec::new();
    };
    let inside = start.starts_with(&cfg.root);
    let mut dirs = Vec::new();
    let mut cur = start.as_path();
    loop {
        dirs.push(cur.to_path_buf());
        if !inside || cur == cfg.root {
            break;
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    dirs.reverse();
    let mut docs = Vec::new();
    for d in dirs {
        for name in &cfg.agent_doc_names {
            let p = d.join(name);
            if let Some(text) = files.read(&p) {
                let (t, truncated) = truncate_utf8(&text, cfg.max_agent_doc_bytes);
                docs.push(AgentDoc {
                    path: p,
                    content: t.to_owned(),
                    truncated,
                });
                break; // one instruction file per directory
            }
        }
    }
    docs
}

// ---- @file / #rule tokens -------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRef {
    pub kind: RefKind,
    pub token: String,
    pub name: String,
    pub lines: Option<(usize, usize)>,
}

fn is_ref_boundary(prev: Option<char>) -> bool {
    prev.is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '{' | '"' | '\'' | '`' | ','))
}

fn parse_line_suffix(name: &str) -> (&str, Option<(usize, usize)>) {
    if let Some((head, tail)) = name.rsplit_once(':') {
        let (a, b) = tail.split_once('-').unwrap_or((tail, tail));
        if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
            if a >= 1 && b >= a && !head.is_empty() {
                return (head, Some((a, b)));
            }
        }
    }
    (name, None)
}

/// Find `@path[:a[-b]]` and `#rule` tokens in `text`. A token must start the
/// text or follow whitespace/an opening bracket/quote, so emails, `#[attr]`
/// and `foo#bar` are left alone. Duplicates are removed.
pub fn extract_references(text: &str) -> Vec<RawRef> {
    let mut out: Vec<RawRef> = Vec::new();
    let mut prev: Option<char> = None;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if (c == '@' || c == '#') && is_ref_boundary(prev) {
            let rest = &text[i + 1..];
            let end = rest
                .find(|ch: char| {
                    ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | ')' | ']' | '}' | ',')
                })
                .unwrap_or(rest.len());
            let raw = rest[..end].trim_end_matches(['.', ';', ':', '!', '?']);
            if !raw.is_empty() {
                let token = format!("{c}{raw}");
                let r = if c == '@' {
                    let (name, lines) = parse_line_suffix(raw);
                    Some(RawRef {
                        kind: RefKind::File,
                        token,
                        name: name.to_owned(),
                        lines,
                    })
                } else if raw
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'))
                    && !raw.chars().all(|ch| ch.is_ascii_digit())
                {
                    Some(RawRef {
                        kind: RefKind::Rule,
                        token,
                        name: raw.to_owned(),
                        lines: None,
                    })
                } else {
                    None
                };
                if let Some(r) = r {
                    if !out.iter().any(|o| o.token == r.token) {
                        out.push(r);
                    }
                }
            }
            // skip past the token so inner '@'/'#' are not re-read
            let skip_to = i + 1 + end;
            while iter.peek().is_some_and(|(j, _)| *j < skip_to) {
                iter.next();
            }
            prev = text[..skip_to].chars().next_back();
            continue;
        }
        prev = Some(c);
    }
    out
}

fn window_lines(content: &str, (a, b): (usize, usize)) -> String {
    content
        .split_inclusive('\n')
        .skip(a - 1)
        .take(b - a + 1)
        .collect()
}

/// Resolve one reference against the project. `None` if it cannot be found
/// or would escape the root.
pub fn resolve_reference(
    cfg: &ContextConfig,
    files: &dyn ProjectFiles,
    current_file: &Path,
    raw: &RawRef,
) -> Option<Reference> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    match raw.kind {
        RefKind::File => {
            let name = Path::new(&raw.name);
            if name.is_absolute() {
                candidates.push(name.to_path_buf());
            } else {
                candidates.push(cfg.root.join(name));
                if let Some(dir) = current_file.parent() {
                    let dir = if dir.is_absolute() {
                        dir.to_path_buf()
                    } else {
                        cfg.root.join(dir)
                    };
                    candidates.push(dir.join(name));
                }
            }
        }
        RefKind::Rule => {
            for d in &cfg.rule_dirs {
                for ext in ["md", "mdc", "txt", ""] {
                    let file = if ext.is_empty() {
                        raw.name.clone()
                    } else {
                        format!("{}.{ext}", raw.name)
                    };
                    candidates.push(cfg.root.join(d).join(file));
                }
            }
        }
    }
    for cand in candidates {
        let Some(p) = normalize(&cand) else { continue };
        if !p.starts_with(&cfg.root) || !files.is_file(&p) {
            continue;
        }
        let Some(text) = files.read(&p) else { continue };
        let text = match raw.lines {
            Some(w) => window_lines(&text, w),
            None => text,
        };
        return Some(Reference {
            kind: raw.kind,
            token: raw.token.clone(),
            path: p,
            lines: raw.lines,
            content: text,
            truncated: false,
        });
    }
    None
}

/// Expand every reference in `prompt`, applying per-item and total byte
/// budgets. Returns (resolved, unresolved tokens).
pub fn expand_references(
    cfg: &ContextConfig,
    files: &dyn ProjectFiles,
    current_file: &Path,
    prompt: &str,
) -> (Vec<Reference>, Vec<String>) {
    let mut refs: Vec<Reference> = Vec::new();
    let mut unresolved = Vec::new();
    let mut budget = cfg.max_total_reference_bytes;
    for raw in extract_references(prompt) {
        match resolve_reference(cfg, files, current_file, &raw) {
            Some(mut r) => {
                if refs.iter().any(|o| o.path == r.path && o.lines == r.lines) {
                    continue;
                }
                let limit = cfg.max_reference_bytes.min(budget);
                if limit == 0 {
                    unresolved.push(format!("{} (skipped: context budget exhausted)", raw.token));
                    continue;
                }
                let (t, truncated) = truncate_utf8(&r.content, limit);
                budget -= t.len();
                r.truncated = truncated;
                r.content = t.to_owned();
                refs.push(r);
            }
            None => {
                // `#word` that is not a rule is ordinary text (heading, issue number)
                if raw.kind == RefKind::File {
                    unresolved.push(raw.token);
                }
            }
        }
    }
    (refs, unresolved)
}

// ---- selection & diagnostics ---------------------------------------------

/// Build the selection block. With `line_aligned` the replaced region is
/// widened to whole lines (what [`super::RewriteRequest`] uses).
pub fn build_selection(
    cfg: &ContextConfig,
    text: &str,
    range: CharRange,
    line_aligned: bool,
) -> SelectionContext {
    let ix = TextIndex::new(text);
    let range = if line_aligned {
        ix.expand_to_lines(range)
    } else {
        range.ordered()
    };
    let first = range.start.line;
    let last_line_incl = if line_aligned && range.end.col == 0 && range.end.line > first {
        range.end.line - 1
    } else {
        range.end.line
    }
    .min(ix.line_count().saturating_sub(1));
    let before_start = CharPos::new(first.saturating_sub(cfg.lines_before), 0);
    let before = ix
        .slice(CharRange::new(before_start, CharPos::new(first, 0)))
        .to_owned();
    let after_from = if line_aligned {
        range.end
    } else {
        CharPos::new(last_line_incl + 1, 0)
    };
    let after_to = CharPos::new(last_line_incl + 1 + cfg.lines_after, 0);
    let after = ix.slice(CharRange::new(after_from, after_to)).to_owned();
    SelectionContext {
        range,
        before,
        text: ix.slice(range).to_owned(),
        after,
        first_line: first + 1,
        last_line: last_line_incl + 1,
    }
}

/// Diagnostics intersecting lines `from..=to` (0-based), worst first, capped.
pub fn relevant_diagnostics(
    cfg: &ContextConfig,
    diags: &[Diagnostic],
    from_line: usize,
    to_line: usize,
) -> Vec<Diagnostic> {
    let lo = from_line.saturating_sub(cfg.lines_before);
    let hi = to_line + cfg.lines_after;
    let mut v: Vec<Diagnostic> = diags
        .iter()
        .filter(|d| {
            let r = d.range.ordered();
            r.end.line >= lo && r.start.line <= hi
        })
        .cloned()
        .collect();
    v.sort_by_key(|d| (d.severity, d.range.start));
    v.truncate(cfg.max_diagnostics);
    v
}

/// Gather everything for a request. `selection` is optional (project-wide
/// search has none); `line_aligned` widens it to whole lines.
pub fn assemble(
    cfg: &ContextConfig,
    files: &dyn ProjectFiles,
    file: &FileSnapshot,
    selection: Option<CharRange>,
    line_aligned: bool,
    diagnostics: &[Diagnostic],
    user_text: &str,
) -> Assembled {
    let sel = selection.map(|r| build_selection(cfg, &file.text, r, line_aligned));
    let (refs, unresolved) = expand_references(cfg, files, &file.path, user_text);
    let abs = if file.path.is_absolute() {
        file.path.clone()
    } else {
        cfg.root.join(&file.path)
    };
    let diags = match &sel {
        Some(s) => relevant_diagnostics(cfg, diagnostics, s.first_line - 1, s.last_line - 1),
        None => Vec::new(),
    };
    Assembled {
        file: display_path(&cfg.root, &abs),
        language: language_for(&abs),
        selection: sel,
        agent_docs: collect_agent_docs(cfg, files, &abs),
        references: refs,
        unresolved,
        diagnostics: diags,
    }
}

/// Append the shared context sections (instructions, references, diagnostics)
/// used by every request template.
pub fn render_context(out: &mut String, cfg: &ContextConfig, a: &Assembled) {
    if !a.agent_docs.is_empty() {
        out.push_str("## Project instructions\n");
        for d in &a.agent_docs {
            out.push_str(&format!(
                "(from {}{})\n{}\n\n",
                display_path(&cfg.root, &d.path),
                if d.truncated { ", truncated" } else { "" },
                d.content.trim_end()
            ));
        }
    }
    let rules: Vec<_> = a
        .references
        .iter()
        .filter(|r| r.kind == RefKind::Rule)
        .collect();
    if !rules.is_empty() {
        out.push_str("## Rules to follow\n");
        for r in rules {
            out.push_str(&format!("(rule {})\n{}\n\n", r.token, r.content.trim_end()));
        }
    }
    let files: Vec<_> = a
        .references
        .iter()
        .filter(|r| r.kind == RefKind::File)
        .collect();
    if !files.is_empty() {
        out.push_str("## Referenced files\n");
        for r in files {
            let win = r
                .lines
                .map_or(String::new(), |(a, b)| format!(" lines {a}-{b}"));
            out.push_str(&format!(
                "### {}{}{}\n{}\n\n",
                display_path(&cfg.root, &r.path),
                win,
                if r.truncated { " (truncated)" } else { "" },
                markdown_fence(language_for(&r.path).unwrap_or(""), &r.content)
            ));
        }
    }
    if !a.diagnostics.is_empty() {
        out.push_str("## Diagnostics near the selection\n");
        for d in &a.diagnostics {
            let r = d.range.ordered();
            out.push_str(&format!(
                "- {}:{} {}: {}{}\n",
                r.start.line + 1,
                r.start.col + 1,
                d.severity.label(),
                d.message.lines().next().unwrap_or(""),
                d.source
                    .as_deref()
                    .map_or(String::new(), |s| format!(" [{s}]"))
            ));
        }
        out.push('\n');
    }
}

#[cfg(test)]
pub(crate) mod testfs {
    use super::*;
    use std::collections::HashMap;

    /// In-memory project for tests.
    #[derive(Default)]
    pub struct MemFiles(pub HashMap<PathBuf, String>);

    impl MemFiles {
        pub fn with(files: &[(&str, &str)]) -> Self {
            Self(
                files
                    .iter()
                    .map(|(p, c)| (PathBuf::from(p), (*c).to_owned()))
                    .collect(),
            )
        }
    }
    impl ProjectFiles for MemFiles {
        fn read(&self, path: &Path) -> Option<String> {
            self.0.get(path).cloned()
        }
        fn is_file(&self, path: &Path) -> bool {
            self.0.contains_key(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testfs::MemFiles;
    use super::*;

    fn cfg() -> ContextConfig {
        ContextConfig::new("/proj")
    }

    #[test]
    fn extracts_file_and_rule_tokens() {
        let r = extract_references(
            "fix @src/a.rs:10-20 and (@b.rs), see #style, ignore a@b.com #123 #[derive] x#y",
        );
        let toks: Vec<_> = r.iter().map(|r| r.token.as_str()).collect();
        assert_eq!(toks, ["@src/a.rs:10-20", "@b.rs", "#style"]);
        assert_eq!(r[0].lines, Some((10, 20)));
        assert_eq!(r[0].name, "src/a.rs");
        assert_eq!(r[2].kind, RefKind::Rule);
    }

    #[test]
    fn trailing_punctuation_and_duplicates() {
        let r = extract_references("look at @a.rs. Also @a.rs, and @a.rs:7!");
        let toks: Vec<_> = r.iter().map(|r| r.token.as_str()).collect();
        assert_eq!(toks, ["@a.rs", "@a.rs:7"]);
        assert_eq!(r[1].lines, Some((7, 7)));
        assert!(extract_references("@").is_empty());
        assert!(extract_references("# heading").is_empty());
    }

    #[test]
    fn resolves_files_rules_and_refuses_escapes() {
        let files = MemFiles::with(&[
            ("/proj/src/a.rs", "l1\nl2\nl3\nl4\n"),
            ("/proj/src/sub/b.rs", "bee"),
            ("/proj/.skink/rules/style.md", "use tabs"),
            ("/secret.txt", "nope"),
        ]);
        let cur = Path::new("/proj/src/sub/cur.rs");
        let (refs, un) = expand_references(
            &cfg(),
            &files,
            cur,
            "@src/a.rs:2-3 @b.rs #style #missing @../../secret.txt @/secret.txt @nothere.rs",
        );
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].content, "l2\nl3\n");
        assert_eq!(
            refs[1].path,
            Path::new("/proj/src/sub/b.rs"),
            "relative to the current file's dir"
        );
        assert_eq!(refs[2].kind, RefKind::Rule);
        assert_eq!(refs[2].content, "use tabs");
        // unknown #rule is plain text; unresolved @ tokens (incl. escapes) are reported
        assert_eq!(un, ["@../../secret.txt", "@/secret.txt", "@nothere.rs"]);
    }

    #[test]
    fn reference_budgets() {
        let big = "x".repeat(100);
        let files = MemFiles::with(&[("/proj/a", &big), ("/proj/b", &big), ("/proj/c", &big)]);
        let mut c = cfg();
        c.max_reference_bytes = 60;
        c.max_total_reference_bytes = 90;
        let (refs, un) = expand_references(&c, &files, Path::new("/proj/x.rs"), "@a @b @c");
        assert_eq!((refs[0].content.len(), refs[0].truncated), (60, true));
        assert_eq!(refs[1].content.len(), 30);
        assert_eq!(refs.len(), 2);
        assert!(un[0].contains("budget"));
    }

    #[test]
    fn agents_md_walk_up_orders_root_first() {
        let files = MemFiles::with(&[
            ("/proj/AGENTS.md", "root rules"),
            ("/proj/AGENT.md", "ignored: AGENTS.md wins in the same dir"),
            ("/proj/crates/x/AGENT.md", "crate rules"),
            ("/proj/crates/x/src/deep/AGENTS.md", "deep rules"),
            ("/AGENTS.md", "outside root"),
        ]);
        let docs = collect_agent_docs(&cfg(), &files, Path::new("/proj/crates/x/src/deep/f.rs"));
        let got: Vec<_> = docs.iter().map(|d| d.content.as_str()).collect();
        assert_eq!(got, ["root rules", "crate rules", "deep rules"]);
        // relative path is taken against the root
        let docs = collect_agent_docs(&cfg(), &files, Path::new("crates/x/f.rs"));
        assert_eq!(docs.len(), 2);
        // a file outside the root only sees its own directory
        let docs = collect_agent_docs(&cfg(), &files, Path::new("/other/f.rs"));
        assert!(docs.is_empty());
        let docs = collect_agent_docs(&cfg(), &files, Path::new("/f.rs"));
        assert_eq!(docs.len(), 1);
    }

    #[test]
    fn selection_with_surroundings() {
        let text = (1..=10).map(|i| format!("line{i}\n")).collect::<String>();
        let mut c = cfg();
        c.lines_before = 2;
        c.lines_after = 2;
        let s = build_selection(
            &c,
            &text,
            CharRange::new(CharPos::new(4, 2), CharPos::new(5, 3)),
            true,
        );
        assert_eq!(s.text, "line5\nline6\n");
        assert_eq!(s.before, "line3\nline4\n");
        assert_eq!(s.after, "line7\nline8\n");
        assert_eq!((s.first_line, s.last_line), (5, 6));
        // exact (not line aligned)
        let s = build_selection(
            &c,
            &text,
            CharRange::new(CharPos::new(4, 2), CharPos::new(4, 4)),
            false,
        );
        assert_eq!(s.text, "ne");
        assert_eq!(s.after, "line6\nline7\n");
        // at file edges
        let s = build_selection(
            &c,
            &text,
            CharRange::new(CharPos::new(0, 0), CharPos::new(0, 1)),
            true,
        );
        assert_eq!((s.before.as_str(), s.text.as_str()), ("", "line1\n"));
    }

    #[test]
    fn diagnostics_filtered_and_sorted() {
        let d = |l, sev, m: &str| Diagnostic {
            range: CharRange::new(CharPos::new(l, 0), CharPos::new(l, 1)),
            severity: sev,
            message: m.into(),
            source: None,
        };
        let mut c = cfg();
        c.lines_before = 1;
        c.lines_after = 1;
        let v = relevant_diagnostics(
            &c,
            &[
                d(1, Severity::Hint, "far"),
                d(10, Severity::Warning, "w"),
                d(11, Severity::Error, "e"),
                d(30, Severity::Error, "far2"),
            ],
            10,
            10,
        );
        let m: Vec<_> = v.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(m, ["e", "w"]);
    }

    #[test]
    fn assemble_and_render() {
        let files = MemFiles::with(&[
            ("/proj/AGENTS.md", "Prefer small functions."),
            ("/proj/src/lib.rs", "pub fn x() {}\n"),
            ("/proj/.skink/rules/safe.md", "No unsafe."),
        ]);
        let file = FileSnapshot {
            path: "/proj/src/main.rs".into(),
            text: "fn main() {\n    todo!()\n}\n".into(),
        };
        let diag = Diagnostic {
            range: CharRange::new(CharPos::new(1, 4), CharPos::new(1, 10)),
            severity: Severity::Warning,
            message: "unfinished\nsecond line".into(),
            source: Some("rustc".into()),
        };
        let a = assemble(
            &cfg(),
            &files,
            &file,
            Some(CharRange::new(CharPos::new(1, 0), CharPos::new(1, 5))),
            true,
            &[diag],
            "implement @src/lib.rs with #safe and @missing.rs",
        );
        assert_eq!(a.file, "src/main.rs");
        assert_eq!(a.language, Some("rust"));
        assert_eq!(a.unresolved, ["@missing.rs"]);
        let mut out = String::new();
        render_context(&mut out, &cfg(), &a);
        assert!(
            out.contains("## Project instructions\n(from AGENTS.md)\nPrefer small functions."),
            "{out}"
        );
        assert!(out.contains("(rule #safe)\nNo unsafe."));
        assert!(out.contains("### src/lib.rs\n```rust\npub fn x() {}\n```"));
        assert!(out.contains("- 2:5 warning: unfinished [rustc]"));
    }
}
