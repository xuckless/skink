//! Project search through an agent (idea from ThePrimeagen/99: ask the agent
//! to find things, get back a results list the editor can jump through).
//! The prompt template and the tolerant parser are original.

use std::path::{Path, PathBuf};

use super::context::{ContextConfig, FileSnapshot, ProjectFiles, assemble, render_context};
use super::coords::CharPos;
use super::{Prompt, RequestKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// What to look for, in natural language (may contain `@file` / `#rule`).
    pub query: String,
    /// The file the user is in, if any (resolves relative `@refs`, gives AGENTS.md).
    pub file: Option<FileSnapshot>,
    pub max_results: usize,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            file: None,
            max_results: 30,
        }
    }
}

/// One location reported by the agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SearchHit {
    /// Absolute path (root-joined when the agent gave a relative one).
    pub path: PathBuf,
    /// 1-based.
    pub line: usize,
    /// 1-based, counted in characters; 1 if the agent gave none.
    pub col: usize,
    pub note: String,
}

impl SearchHit {
    /// 0-based editor position.
    pub fn pos(&self) -> CharPos {
        CharPos::new(self.line.saturating_sub(1), self.col.saturating_sub(1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    /// Lines that looked like locations but were rejected (unknown file,
    /// outside the project, line 0).
    pub rejected: usize,
}

pub(crate) fn search_contract(max_results: usize, tracker: bool) -> String {
    let mut s = format!(
        "\
You are a code search engine inside a text editor. Find the places in the project \
that answer the QUERY. Explore with your read-only tools (search, read files) as \
needed. Do NOT modify any file.

Output contract (strict):
- Your final answer is a list of locations, one per line, exactly like:
  path/to/file.rs:LINE:COL: short note
- path is relative to the project root. LINE and COL are 1-based; COL counts characters.
- No Markdown, no headings, no numbering, no prose between lines.
- At most {max_results} lines, most relevant first.
- If nothing matches, reply with the single line NO RESULTS.
"
    );
    if tracker {
        s.push_str(
            "\
- After the locations, finish with a line `STATUS: DONE` when you searched everything, \
or `STATUS: CONTINUE` followed by a line `REMAINING:` and one `- item` line per area \
you have not searched yet.
",
        );
    }
    s
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltSearch {
    pub prompt: Prompt,
}

pub fn build_search(
    cfg: &ContextConfig,
    files: &dyn ProjectFiles,
    req: &SearchRequest,
) -> BuiltSearch {
    let mut t = search_contract(req.max_results, false);
    t.push('\n');
    t.push_str(&search_body(cfg, files, req));
    BuiltSearch {
        prompt: Prompt {
            kind: RequestKind::Search,
            text: t,
        },
    }
}

/// Context + query sections (shared with the worker's first prompt).
pub(crate) fn search_body(
    cfg: &ContextConfig,
    files: &dyn ProjectFiles,
    req: &SearchRequest,
) -> String {
    let mut t = String::new();
    let a = match &req.file {
        Some(f) => assemble(cfg, files, f, None, false, &[], &req.query),
        None => {
            let empty = FileSnapshot {
                path: cfg.root.join("."),
                text: String::new(),
            };
            assemble(cfg, files, &empty, None, false, &[], &req.query)
        }
    };
    render_context(&mut t, cfg, &a);
    if req.file.is_some() {
        t.push_str(&format!("The user is currently in `{}`.\n\n", a.file));
    }
    t.push_str(&format!("## QUERY\n{}\n", req.query.trim()));
    t
}

fn strip_bullet(s: &str) -> &str {
    let s = s.trim_start();
    if let Some(r) = s
        .strip_prefix("- ")
        .or_else(|| s.strip_prefix("* "))
        .or_else(|| s.strip_prefix("+ "))
    {
        return r.trim_start();
    }
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && digits < 4 {
        if let Some(r) = s[digits..]
            .strip_prefix(". ")
            .or_else(|| s[digits..].strip_prefix(") "))
        {
            return r.trim_start();
        }
    }
    s
}

fn take_digits(s: &str) -> Option<(usize, &str)> {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    (n > 0 && n < 10).then(|| (s[..n].parse().unwrap_or(0), &s[n..]))
}

/// Parse one line into (path, line, col, note) without touching the file system.
fn parse_line(raw: &str) -> Option<(String, usize, usize, String)> {
    let cleaned = raw.replace('`', "").replace("**", "");
    let s = strip_bullet(cleaned.trim());
    // skip a Windows drive prefix like `C:\` or `C:/`
    let b = s.as_bytes();
    let skip = if b.len() > 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/')
    {
        2
    } else {
        0
    };
    // `path#L12` form
    if let Some(i) = s[skip..].find("#L").map(|i| i + skip) {
        if let Some((line, rest)) = take_digits(&s[i + 2..]) {
            let note = rest
                .trim_start_matches(|c: char| {
                    c == '-' || c == 'L' || c == ':' || c.is_ascii_digit()
                })
                .trim_start_matches([':', '-', '\u{2013}', '\u{2014}', ' '])
                .trim();
            return Some((s[..i].trim().to_owned(), line, 1, note.to_owned()));
        }
    }
    let mut from = skip;
    while let Some(off) = s[from..].find(':') {
        let p = from + off;
        if let Some((line, rest)) = take_digits(&s[p + 1..]) {
            let path = s[..p].trim();
            let (col, rest) = match rest.strip_prefix(':').and_then(take_digits) {
                Some((c, r)) => (c, r),
                None => (1, rest),
            };
            // `12-15` ranges: keep the start
            let rest = match rest.strip_prefix('-').and_then(take_digits) {
                Some((_, r)) => r,
                None => rest,
            };
            let note = rest
                .trim_start_matches([':', '-', '\u{2013}', '\u{2014}', ' ', '\t'])
                .trim();
            if !path.is_empty() {
                return Some((path.to_owned(), line, col.max(1), note.to_owned()));
            }
        }
        from = p + 1;
    }
    None
}

/// Parse an agent's search answer into hits, tolerating bullets, numbering,
/// backticks, bold, `path#L12`, line ranges, fenced output, `NO RESULTS`
/// and stray prose. When `files` is given, only existing files inside `root`
/// are accepted; without it, paths containing whitespace are rejected instead.
pub fn parse_search_results(
    reply: &str,
    root: &Path,
    files: Option<&dyn ProjectFiles>,
) -> SearchResults {
    let mut out = SearchResults::default();
    for line in reply.lines() {
        let Some((path, ln, col, note)) = parse_line(line) else {
            continue;
        };
        let p = Path::new(&path);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        };
        let ok = ln >= 1
            && abs.starts_with(root)
            && !abs
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            && match files {
                Some(f) => f.is_file(&abs),
                None => !path.contains(char::is_whitespace),
            };
        if !ok {
            out.rejected += 1;
            continue;
        }
        let hit = SearchHit {
            path: abs,
            line: ln,
            col,
            note,
        };
        if !out
            .hits
            .iter()
            .any(|h| h.path == hit.path && h.line == hit.line && h.col == hit.col)
        {
            out.hits.push(hit);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::context::testfs::MemFiles;
    use super::*;

    fn fs() -> MemFiles {
        MemFiles::with(&[
            ("/p/src/a.rs", ""),
            ("/p/src/b.rs", ""),
            ("/p/dir with space/c.rs", ""),
            ("/p/AGENTS.md", "Search carefully."),
        ])
    }

    fn parse(r: &str) -> SearchResults {
        parse_search_results(r, Path::new("/p"), Some(&fs()))
    }

    #[test]
    fn plain_lines() {
        let r = parse("src/a.rs:12:5: calls foo\nsrc/b.rs:3:1: defines foo\n");
        assert_eq!(r.hits.len(), 2);
        assert_eq!(
            r.hits[0],
            SearchHit {
                path: "/p/src/a.rs".into(),
                line: 12,
                col: 5,
                note: "calls foo".into()
            }
        );
        assert_eq!(r.hits[0].pos(), CharPos::new(11, 4));
        assert_eq!(r.hits[1].note, "defines foo");
    }

    #[test]
    fn tolerates_decoration() {
        let r = parse(
            "Found these:\n\
             - `src/a.rs:12:5` - the call\n\
             2. **src/b.rs:7**: definition\n\
             * src/a.rs#L40 something\n\
             ```\nsrc/b.rs:9:2-ish\n```\n\
             NO RESULTS\n",
        );
        let got: Vec<_> = r
            .hits
            .iter()
            .map(|h| {
                (
                    h.path.to_str().unwrap().replace('\\', "/"),
                    h.line,
                    h.col,
                    h.note.as_str(),
                )
            })
            .collect();
        let got: Vec<_> = got
            .iter()
            .map(|(p, l, c, n)| (p.as_str(), *l, *c, *n))
            .collect();
        assert_eq!(
            got,
            [
                ("/p/src/a.rs", 12, 5, "the call"),
                ("/p/src/b.rs", 7, 1, "definition"),
                ("/p/src/a.rs", 40, 1, "something"),
                ("/p/src/b.rs", 9, 2, "ish"),
            ]
        );
    }

    #[test]
    fn missing_col_ranges_and_dashes() {
        let r = parse(
            "src/a.rs:12 - no column\nsrc/a.rs:20-25: a range\nsrc/b.rs:4:2 \u{2014} em dash note",
        );
        assert_eq!(r.hits.len(), 3);
        assert_eq!(
            (r.hits[0].line, r.hits[0].col, r.hits[0].note.as_str()),
            (12, 1, "no column")
        );
        assert_eq!((r.hits[1].line, r.hits[1].note.as_str()), (20, "a range"));
        assert_eq!(r.hits[2].note, "em dash note");
    }

    #[test]
    fn rejects_unknown_files_escapes_and_zero_lines() {
        let r = parse(
            "nope.rs:1:1: x\n../etc/passwd:1:1: y\n/etc/passwd:1:1: z\nsrc/a.rs:0:0: zero\nResult count: 3\n",
        );
        assert!(r.hits.is_empty());
        assert_eq!(r.rejected, 4, "every location-shaped line counted: {r:?}");
    }

    #[test]
    fn dedupes_and_accepts_absolute_inside_root() {
        let r = parse("/p/src/a.rs:1:1: x\nsrc/a.rs:1:1: x again\n");
        assert_eq!(r.hits.len(), 1);
    }

    #[test]
    fn paths_with_spaces_when_files_known() {
        let r = parse("dir with space/c.rs:2:3: hi");
        assert_eq!(r.hits.len(), 1);
        // without a file index, spaces mean "this is prose"
        let r = parse_search_results("Found results:3\nsrc/a.rs:2:3: ok", Path::new("/p"), None);
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.rejected, 1);
    }

    #[test]
    fn windows_drive_paths() {
        let (p, l, c, n) = parse_line("C:\\proj\\src\\a.rs:12:3: note").unwrap();
        assert_eq!(
            (p.as_str(), l, c, n.as_str()),
            ("C:\\proj\\src\\a.rs", 12, 3, "note")
        );
        let (p, l, ..) = parse_line("C:/proj/a.rs:5").unwrap();
        assert_eq!((p.as_str(), l), ("C:/proj/a.rs", 5));
    }

    #[test]
    fn prompt_mentions_contract_query_and_instructions() {
        let cfg = ContextConfig::new("/p");
        let mut req = SearchRequest::new("where do we parse locations? see @src/a.rs");
        req.file = Some(FileSnapshot {
            path: "/p/src/b.rs".into(),
            text: String::new(),
        });
        let b = build_search(&cfg, &fs(), &req);
        let t = &b.prompt.text;
        assert_eq!(b.prompt.kind, RequestKind::Search);
        assert!(t.contains("path/to/file.rs:LINE:COL: short note"));
        assert!(t.contains("At most 30 lines"));
        assert!(t.contains("Search carefully."));
        assert!(t.contains("## QUERY\nwhere do we parse locations? see @src/a.rs"));
        assert!(t.contains("currently in `src/b.rs`"));
    }
}
