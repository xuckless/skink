//! Structural search and replace on top of `ast-grep-core` (MIT,
//! github.com/ast-grep/ast-grep). Patterns are code with `$NAME` (one node)
//! and `$$$NAME` (any number of nodes) meta variables; replacement templates
//! use the same names.
//!
//! The `Language` glue is adapted from `ast-grep-language` (MIT): languages
//! whose grammar does not accept `$` in identifiers get the pattern's `$`
//! swapped for an "expando" char before parsing.

use crate::Lang;
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use ast_grep_core::{Language, Pattern, PatternError, matcher::PatternBuilder};
use std::borrow::Cow;

/// One structural match, in byte offsets plus a 0-based line / char column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructMatch {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Copy)]
struct Sg(Lang);

impl Sg {
    fn expando(self) -> char {
        match self.0.name() {
            "css" => '_',
            "rust" | "python" | "go" | "zig" | "c" | "cpp" | "csharp" | "elixir" | "haskell"
            | "php" | "ruby" | "swift" | "lua" | "nix" => 'µ',
            _ => '$',
        }
    }
}

impl Language for Sg {
    fn pre_process_pattern<'q>(&self, query: &'q str) -> Cow<'q, str> {
        let e = self.expando();
        if e == '$' {
            return Cow::Borrowed(query);
        }
        // `$` that starts a meta variable (`$A`, `$_`, `$$$`) becomes the
        // expando char; other `$`s stay.
        let mut out = String::with_capacity(query.len());
        let mut dollars = 0;
        for c in query.chars() {
            if c == '$' {
                dollars += 1;
                continue;
            }
            let meta = c.is_ascii_uppercase() || c == '_';
            for _ in 0..dollars {
                out.push(if meta { e } else { '$' });
            }
            dollars = 0;
            out.push(c);
        }
        for _ in 0..dollars {
            out.push(e);
        }
        Cow::Owned(out)
    }
    fn expando_char(&self) -> char {
        self.expando()
    }
    fn kind_to_id(&self, kind: &str) -> u16 {
        self.get_ts_language().id_for_node_kind(kind, true)
    }
    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.get_ts_language()
            .field_id_for_name(field)
            .map(|f| f.get())
    }
    fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
        builder.build(|src| StrDoc::try_new(src, *self))
    }
}

impl LanguageExt for Sg {
    fn get_ts_language(&self) -> TSLanguage {
        self.0.grammar()
    }
}

fn pattern(lang: Lang, pat: &str) -> Result<Pattern, String> {
    if pat.trim().is_empty() {
        return Err("empty pattern".into());
    }
    Pattern::try_new(pat, Sg(lang)).map_err(|e| format!("bad pattern: {e}"))
}

/// Every match of `pat` in `src`, outermost first, in source order.
pub fn structural_find(lang: Lang, src: &str, pat: &str) -> Result<Vec<StructMatch>, String> {
    let p = pattern(lang, pat)?;
    let root = Sg(lang).ast_grep(src);
    Ok(root
        .root()
        .find_all(&p)
        .map(|m| {
            let r = m.range();
            let pos = m.start_pos();
            let line_start = src[..r.start].rfind('\n').map_or(0, |i| i + 1);
            StructMatch {
                start: r.start,
                end: r.end,
                line: pos.line(),
                col: src[line_start..r.start].chars().count(),
            }
        })
        .collect())
}

/// `src` with every match of `pat` rewritten by `template`; `None` when
/// nothing matched.
pub fn structural_replace(
    lang: Lang,
    src: &str,
    pat: &str,
    template: &str,
) -> Result<Option<String>, String> {
    let p = pattern(lang, pat)?;
    let root = Sg(lang).ast_grep(src);
    let edits: Vec<_> = root
        .root()
        .find_all(&p)
        .map(|m| m.replace_by(template))
        .collect();
    if edits.is_empty() {
        return Ok(None);
    }
    let mut out = String::with_capacity(src.len());
    let mut at = 0;
    for e in edits {
        if e.position < at {
            continue; // nested in an earlier match
        }
        out.push_str(&src[at..e.position]);
        out.push_str(&String::from_utf8_lossy(&e.inserted_text));
        at = e.position + e.deleted_length;
    }
    out.push_str(&src[at..]);
    Ok(Some(out))
}

#[cfg(all(test, feature = "rust", feature = "python", feature = "typescript"))]
mod tests {
    use super::*;

    #[test]
    fn rust_find_and_replace() {
        let src = "fn f() { let a = foo(1, 2); bar(foo(x, y)); }\n";
        let m = structural_find(Lang::Rust, src, "foo($A, $B)").unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(&src[m[0].start..m[0].end], "foo(1, 2)");
        let out = structural_replace(Lang::Rust, src, "foo($A, $B)", "foo($B, $A)")
            .unwrap()
            .unwrap();
        assert_eq!(out, "fn f() { let a = foo(2, 1); bar(foo(y, x)); }\n");
    }

    #[test]
    fn python_and_ts() {
        let py = "print(a)\nx = print(b)\n";
        let out = structural_replace(Lang::Python, py, "print($X)", "log($X)")
            .unwrap()
            .unwrap();
        assert_eq!(out, "log(a)\nx = log(b)\n");
        let ts = "console.log(1, 2);\n";
        let m = structural_find(Lang::TypeScript, ts, "console.log($$$ARGS)").unwrap();
        assert_eq!(m.len(), 1);
        let out = structural_replace(Lang::TypeScript, ts, "console.log($$$A)", "debug($$$A)")
            .unwrap()
            .unwrap();
        assert_eq!(out, "debug(1, 2);\n");
        assert!(structural_find(Lang::Rust, "x", "").is_err());
    }
}
