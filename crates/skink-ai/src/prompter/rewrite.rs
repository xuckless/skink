//! "Rewrite the selection" requests (idea from ThePrimeagen/99: visual
//! selection + instruction in, replacement code out, shown as an inline diff).
//! Re-implemented from the README concepts only; the template wording and the
//! extractor are original.

use std::path::PathBuf;

use super::context::{
    Assembled, ContextConfig, Diagnostic, FileSnapshot, ProjectFiles, assemble, render_context,
};
use super::coords::{CharRange, TextIndex, to_crlf, to_lf, uses_crlf};
use super::edit::EditProposal;
use super::{Prompt, RequestKind};
use crate::agent::markdown_fence;

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteRequest {
    /// The instruction typed by the user (may contain `@file` / `#rule`).
    pub instruction: String,
    pub file: FileSnapshot,
    /// Selection in char coordinates (an empty range means "this line").
    pub selection: CharRange,
    /// Replace whole lines (default in the editor: models do better and the
    /// diff is cleaner). `false` replaces exactly the selected characters.
    pub line_aligned: bool,
}

impl RewriteRequest {
    pub fn new(instruction: impl Into<String>, file: FileSnapshot, selection: CharRange) -> Self {
        Self {
            instruction: instruction.into(),
            file,
            selection,
            line_aligned: true,
        }
    }
}

/// A prepared request: the prompt to send and what is needed to interpret
/// the reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltRewrite {
    pub prompt: Prompt,
    pub path: PathBuf,
    /// Region that the reply replaces.
    pub region: CharRange,
    pub original: String,
    pub assembled: Assembled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractError {
    /// The reply contained no code.
    Empty,
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the agent returned no code")
    }
}

impl std::error::Error for ExtractError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractMethod {
    /// The reply was already just code.
    Raw,
    /// Code was taken from a Markdown fence (chatter around it dropped).
    Fence,
    /// Leading/trailing chatter lines were removed from an unfenced reply.
    ChatterStripped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// Code with `\n` line endings, no leading blank lines, no trailing blank lines.
    pub code: String,
    pub method: ExtractMethod,
}

const CONTRACT: &str = "\
You are a code rewriting engine embedded in a text editor. Replace the SELECTION \
of the file below as the INSTRUCTION says.

Output contract (strict):
- Reply with ONLY the replacement text for the selection. It is pasted over the selection verbatim.
- If the INSTRUCTION asks for comments, documentation or an explanation, write it INSIDE the code as \
comments or doc comments in the file's language. Never put prose outside the code.
- No greetings, no Markdown code fences, no line numbers.
- Do not repeat code from outside the selection. Keep the file's indentation style.
- Do not call tools or edit files yourself; the editor applies your reply.
- If the selection should stay as it is, reply with the selection unchanged.
";

/// Assemble context and render the rewrite prompt.
pub fn build_rewrite(
    cfg: &ContextConfig,
    files: &dyn ProjectFiles,
    req: &RewriteRequest,
    diagnostics: &[Diagnostic],
) -> BuiltRewrite {
    let a = assemble(
        cfg,
        files,
        &req.file,
        Some(req.selection),
        req.line_aligned,
        diagnostics,
        &req.instruction,
    );
    let sel = a.selection.clone().unwrap_or_else(|| {
        // `assemble` always returns a selection when one was passed
        super::context::build_selection(cfg, &req.file.text, req.selection, req.line_aligned)
    });
    let lang = a.language.unwrap_or("");
    let mut t = String::from(CONTRACT);
    t.push('\n');
    render_context(&mut t, cfg, &a);
    t.push_str(&format!(
        "## File\n{}{}, selected lines {}-{}\n\n",
        a.file,
        a.language.map_or(String::new(), |l| format!(" ({l})")),
        sel.first_line,
        sel.last_line
    ));
    if !sel.before.is_empty() {
        t.push_str(&format!(
            "## Code before the selection (context only)\n{}\n\n",
            markdown_fence(lang, &sel.before)
        ));
    }
    t.push_str(&format!(
        "## SELECTION\n{}\n\n",
        markdown_fence(lang, &sel.text)
    ));
    if !sel.after.is_empty() {
        t.push_str(&format!(
            "## Code after the selection (context only)\n{}\n\n",
            markdown_fence(lang, &sel.after)
        ));
    }
    t.push_str(&format!("## INSTRUCTION\n{}\n", req.instruction.trim()));
    BuiltRewrite {
        prompt: Prompt {
            kind: RequestKind::RewriteSelection,
            text: t,
        },
        path: req.file.path.clone(),
        region: sel.range,
        original: sel.text,
        assembled: a,
    }
}

impl BuiltRewrite {
    /// Turn the agent's final answer into a reviewable edit. Line endings and
    /// the trailing newline follow the original region, so the diff shows only
    /// real changes. `Err(Empty)` when there is nothing to paste (callers that
    /// want "delete the selection" can build an `EditProposal` with `""`).
    pub fn finish(&self, reply: &str) -> Result<EditProposal, ExtractError> {
        let ex = extract_code(reply);
        if ex.code.trim().is_empty() {
            return Err(ExtractError::Empty);
        }
        let replacement = match_style(&ex.code, &self.original);
        Ok(EditProposal::new(
            self.path.clone(),
            self.region,
            self.original.clone(),
            replacement,
        ))
    }

    /// Exact text of the region in a buffer (for staleness checks).
    pub fn region_text<'a>(&self, buffer: &'a str) -> &'a str {
        TextIndex::new(buffer).slice(self.region)
    }
}

/// Make `code` use the same line endings and trailing-newline convention as
/// `like` (the original region).
pub fn match_style(code: &str, like: &str) -> String {
    let mut s = if uses_crlf(like) {
        to_crlf(code)
    } else {
        to_lf(code)
    };
    let like_nl = like.ends_with('\n');
    let has_nl = s.ends_with('\n');
    if like_nl && !has_nl {
        s.push_str(if uses_crlf(like) { "\r\n" } else { "\n" });
    } else if !like_nl && has_nl {
        s.truncate(s.trim_end_matches(['\n', '\r']).len());
    }
    s
}

// ---- reply extraction ----------------------------------------------------

struct Fence<'a> {
    body: Vec<&'a str>,
}

fn fence_open(line: &str) -> Option<(char, usize, &str)> {
    let t = line.trim_start();
    let c = t.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = t.chars().take_while(|x| *x == c).count();
    (n >= 3).then(|| (c, n, t[n..].trim()))
}

fn fence_close(line: &str, c: char, n: usize) -> bool {
    let t = line.trim();
    t.chars().count() >= n && t.chars().all(|x| x == c)
}

fn parse_fences<'a>(lines: &[&'a str]) -> Vec<Fence<'a>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some((c, n, _info)) = fence_open(lines[i]) {
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && !fence_close(lines[i], c, n) {
                body.push(lines[i]);
                i += 1;
            }
            out.push(Fence { body }); // unclosed (truncated reply): keep what we have
        }
        i += 1;
    }
    out
}

const PREAMBLE_STARTS: &[&str] = &[
    "here is",
    "here's",
    "here are",
    "sure",
    "certainly",
    "of course",
    "okay",
    "ok,",
    "ok.",
    "i've",
    "i have",
    "i'll",
    "i will",
    "below is",
    "the following",
    "this is the",
    "got it",
    "absolutely",
    "done",
];
const POSTAMBLE_STARTS: &[&str] = &[
    "this ",
    "these ",
    "note",
    "i've ",
    "i have ",
    "the above",
    "explanation",
    "let me know",
    "hope this",
    "i also ",
    "in this ",
    "changes",
    "the code above",
    "i added",
    "i changed",
    "i updated",
    "i removed",
];

fn looks_like_preamble(line: &str) -> bool {
    let l = line.trim().to_ascii_lowercase();
    PREAMBLE_STARTS.iter().any(|p| l.starts_with(p))
        && matches!(l.chars().last(), Some(':' | '.' | '!'))
}

fn looks_like_postamble(par: &[&str]) -> bool {
    let first = par.first().map_or("", |l| l.trim()).to_ascii_lowercase();
    let last = par.last().map_or("", |l| l.trim());
    POSTAMBLE_STARTS.iter().any(|p| first.starts_with(p))
        && matches!(last.chars().last(), Some('.' | '!' | ':' | '?') | None)
        || (first.starts_with("**") || first.starts_with("explanation"))
            && matches!(last.chars().last(), Some('.' | '!' | ':'))
}

fn trim_blank_edges(lines: &[&str]) -> Vec<String> {
    let mut a = 0;
    let mut b = lines.len();
    while a < b && lines[a].trim().is_empty() {
        a += 1;
    }
    while b > a && lines[b - 1].trim().is_empty() {
        b -= 1;
    }
    lines[a..b].iter().map(|s| (*s).to_owned()).collect()
}

fn join(lines: &[String]) -> String {
    let mut s = lines.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// Pull the replacement code out of an agent reply that was supposed to be
/// "only the code" but may be fenced, chatty, or both.
///
/// Rules, in order:
/// 1. If the reply starts with a fence (or a chatter line followed later by
///    one), the fenced body is the code; with several fences the longest wins.
///    Longer outer fences (` ```` `) may contain shorter inner ones.
/// 2. Otherwise the reply is treated as raw code. A single leading chatter
///    line ("Here is the updated function:") and a trailing chatter paragraph
///    ("This change ...") are removed. Fences inside raw code (e.g. a
///    Markdown file being edited) are left alone.
pub fn extract_code(reply: &str) -> Extracted {
    let text = reply.replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let first_idx = lines.iter().position(|l| !l.trim().is_empty());
    let Some(fi) = first_idx else {
        return Extracted {
            code: String::new(),
            method: ExtractMethod::Raw,
        };
    };
    let first = lines[fi];
    let fenced_mode = fence_open(first).is_some()
        || (looks_like_preamble(first) && lines[fi + 1..].iter().any(|l| fence_open(l).is_some()));
    if fenced_mode {
        let fences = parse_fences(&lines[fi..]);
        if let Some(best) = fences
            .iter()
            .max_by_key(|f| f.body.iter().map(|l| l.len() + 1).sum::<usize>())
        {
            return Extracted {
                code: join(&trim_blank_edges(&best.body)),
                method: ExtractMethod::Fence,
            };
        }
    }
    let mut body: Vec<&str> = lines[fi..].to_vec();
    let mut stripped = false;
    if body.len() > 1
        && looks_like_preamble(body[0])
        && (body[1].trim().is_empty() || body[0].trim_end().ends_with(':'))
    {
        body.remove(0);
        stripped = true;
    }
    // trailing chatter paragraph
    while body.last().is_some_and(|l| l.trim().is_empty()) {
        body.pop();
    }
    if let Some(blank) = body.iter().rposition(|l| l.trim().is_empty())
        && blank > 0
        && looks_like_postamble(&body[blank + 1..])
    {
        body.truncate(blank);
        stripped = true;
    }
    Extracted {
        code: join(&trim_blank_edges(&body)),
        method: if stripped {
            ExtractMethod::ChatterStripped
        } else {
            ExtractMethod::Raw
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::context::testfs::MemFiles;
    use super::super::coords::CharPos;
    use super::*;

    fn code(reply: &str) -> String {
        extract_code(reply).code
    }

    #[test]
    fn raw_code_passes_through() {
        let e = extract_code("fn a() {\n    1\n}\n");
        assert_eq!(
            (e.code.as_str(), e.method),
            ("fn a() {\n    1\n}\n", ExtractMethod::Raw)
        );
        // indentation of the first line is preserved
        assert_eq!(
            code("    let x = 1;\n    let y = 2;"),
            "    let x = 1;\n    let y = 2;\n"
        );
    }

    #[test]
    fn strips_a_plain_fence() {
        let e = extract_code("```rust\nfn a() {}\n```\n");
        assert_eq!(
            (e.code.as_str(), e.method),
            ("fn a() {}\n", ExtractMethod::Fence)
        );
        assert_eq!(code("```\nx\n```"), "x\n");
        assert_eq!(code("~~~python\nprint(1)\n~~~"), "print(1)\n");
    }

    #[test]
    fn strips_chatter_around_fences() {
        let r = "Sure! Here's the updated function:\n\n```rust\nfn a() {}\n```\n\nThis version avoids the clone.";
        assert_eq!(code(r), "fn a() {}\n");
        let r = "Here is the code:\n```js\nlet a = 1;\n```\nHope this helps!";
        assert_eq!(code(r), "let a = 1;\n");
    }

    #[test]
    fn multiple_fences_longest_wins() {
        let r = "```\nold\n```\nand now\n```rust\nfn big() {\n    1\n}\n```";
        assert_eq!(code(r), "fn big() {\n    1\n}\n");
    }

    #[test]
    fn nested_fences_with_longer_outer() {
        let r = "````markdown\n# T\n```rust\nfn x() {}\n```\nend\n````";
        assert_eq!(code(r), "# T\n```rust\nfn x() {}\n```\nend\n");
    }

    #[test]
    fn unclosed_fence_from_truncated_reply() {
        assert_eq!(code("```rust\nfn a() {\n    1"), "fn a() {\n    1\n");
    }

    #[test]
    fn raw_markdown_with_fences_is_not_mangled() {
        let r = "# Title\n\n```sh\ncargo run\n```\n\nText.\n";
        let e = extract_code(r);
        assert_eq!(e.method, ExtractMethod::Raw);
        assert_eq!(e.code, "# Title\n\n```sh\ncargo run\n```\n\nText.\n");
    }

    #[test]
    fn strips_unfenced_chatter() {
        let r =
            "Here is the rewritten function:\n\nfn a() {}\n\nThis keeps the behaviour the same.";
        let e = extract_code(r);
        assert_eq!(
            (e.code.as_str(), e.method),
            ("fn a() {}\n", ExtractMethod::ChatterStripped)
        );
        let r = "Certainly:\nlet x = 1;\nlet y = 2;";
        assert_eq!(code(r), "let x = 1;\nlet y = 2;\n");
    }

    #[test]
    fn code_that_resembles_chatter_is_kept() {
        // comment lines and statements are not stripped
        assert_eq!(
            code("// Here is a note.\nfn a() {}\n"),
            "// Here is a note.\nfn a() {}\n"
        );
        assert_eq!(
            code("done = True\nprint(done)\n"),
            "done = True\nprint(done)\n"
        );
        // a trailing paragraph that is code-ish stays
        assert_eq!(code("a();\n\nthis.b();\n"), "a();\n\nthis.b();\n");
    }

    #[test]
    fn crlf_and_blank_edges() {
        assert_eq!(code("\n\n```\r\nx\r\ny\r\n```\r\n\n"), "x\ny\n");
        assert_eq!(code("\n\n  a\n\n\n"), "  a\n");
    }

    #[test]
    fn empty_and_whitespace() {
        assert_eq!(code(""), "");
        assert_eq!(code("  \n\n"), "");
        assert_eq!(code("```\n```"), "");
    }

    #[test]
    fn match_style_follows_original() {
        assert_eq!(match_style("a\nb\n", "x\ny\n"), "a\nb\n");
        assert_eq!(match_style("a\nb\n", "x\ny"), "a\nb");
        assert_eq!(match_style("a\nb", "x\ny\n"), "a\nb\n");
        assert_eq!(match_style("a\nb\n", "x\r\ny\r\n"), "a\r\nb\r\n");
        assert_eq!(match_style("a\r\nb\r\n", "x\ny\n"), "a\nb\n");
    }

    fn built() -> (ContextConfig, MemFiles, RewriteRequest) {
        let cfg = ContextConfig::new("/p");
        let files = MemFiles::with(&[
            ("/p/AGENTS.md", "Be terse."),
            ("/p/src/util.rs", "pub fn u() {}\n"),
        ]);
        let req = RewriteRequest::new(
            "rename x to count, use @src/util.rs",
            FileSnapshot {
                path: "/p/src/main.rs".into(),
                text: "fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n".into(),
            },
            CharRange::new(CharPos::new(1, 4), CharPos::new(2, 3)),
        );
        (cfg, files, req)
    }

    #[test]
    fn prompt_contains_contract_context_and_selection() {
        let (cfg, files, req) = built();
        let b = build_rewrite(&cfg, &files, &req, &[]);
        let t = &b.prompt.text;
        assert_eq!(b.prompt.kind, RequestKind::RewriteSelection);
        assert!(t.contains("Reply with ONLY the replacement text"));
        assert!(t.contains("Be terse."));
        assert!(t.contains("### src/util.rs"));
        assert!(t.contains("src/main.rs (rust), selected lines 2-3"));
        assert!(t.contains("## SELECTION\n```rust\n    let x = 1;\n    println!(\"{x}\");\n```"));
        assert!(
            t.contains("## Code before the selection (context only)\n```rust\nfn main() {\n```")
        );
        assert!(t.contains("## Code after the selection (context only)\n```rust\n}\n```"));
        assert!(
            t.trim_end()
                .ends_with("rename x to count, use @src/util.rs")
        );
        assert_eq!(b.original, "    let x = 1;\n    println!(\"{x}\");\n");
    }

    #[test]
    fn finish_builds_reviewable_edit() {
        let (cfg, files, req) = built();
        let b = build_rewrite(&cfg, &files, &req, &[]);
        let reply = "Here's the updated code:\n```rust\n    let count = 1;\n    println!(\"{count}\");\n```";
        let edit = b.finish(reply).unwrap();
        assert_eq!(edit.original, b.original);
        assert_eq!(
            edit.replacement,
            "    let count = 1;\n    println!(\"{count}\");\n"
        );
        assert_eq!(edit.diff.stats().blocks, 1);
        let new = edit.accept_all_to(&req.file.text).unwrap();
        assert_eq!(
            new,
            "fn main() {\n    let count = 1;\n    println!(\"{count}\");\n}\n"
        );
        assert_eq!(b.finish("```\n```"), Err(ExtractError::Empty));
        assert_eq!(b.region_text(&req.file.text), b.original);
    }

    #[test]
    fn unchanged_reply_is_a_noop() {
        let (cfg, files, req) = built();
        let b = build_rewrite(&cfg, &files, &req, &[]);
        let edit = b.finish(&b.original.clone()).unwrap();
        assert!(edit.is_noop());
    }

    #[test]
    fn crlf_file_gets_crlf_replacement() {
        let cfg = ContextConfig::new("/p");
        let files = MemFiles::default();
        let req = RewriteRequest::new(
            "shout",
            FileSnapshot {
                path: "/p/a.txt".into(),
                text: "one\r\ntwo\r\nthree\r\n".into(),
            },
            CharRange::new(CharPos::new(1, 0), CharPos::new(1, 3)),
        );
        let b = build_rewrite(&cfg, &files, &req, &[]);
        assert_eq!(b.original, "two\r\n");
        let e = b.finish("TWO").unwrap();
        assert_eq!(e.replacement, "TWO\r\n");
        assert_eq!(
            e.accept_all_to(&req.file.text).unwrap(),
            "one\r\nTWO\r\nthree\r\n"
        );
    }
}
