//! Small tree-sitter refactors used when no language server helps: the item
//! (function, struct, class, ...) around a position, for safe delete, and
//! extract-variable for Rust, TypeScript / JavaScript and Python.

use crate::Lang;
use tree_sitter::{Node, Tree};

fn parse(lang: Lang, src: &str) -> Option<Tree> {
    let mut p = lang.parser()?;
    p.parse(src, None)
}

/// Node kinds that are a whole declaration.
fn is_item(kind: &str) -> bool {
    kind.ends_with("_item") && kind != "attribute_item" && kind != "use_item"
        || kind.ends_with("_declaration")
        || kind.ends_with("_definition")
        || matches!(
            kind,
            "function_declaration" | "method_definition" | "decorated_definition"
        )
}

/// Byte range of the innermost declaration around `byte`, widened to whole
/// lines and to the doc comments / attributes / decorators right above it.
pub fn enclosing_item(lang: Lang, src: &str, byte: usize) -> Option<(usize, usize)> {
    let tree = parse(lang, src)?;
    let mut n = tree
        .root_node()
        .descendant_for_byte_range(byte, byte.min(src.len()))?;
    while !is_item(n.kind()) {
        n = n.parent()?;
    }
    // A decorated Python definition: take the decorators with it.
    if let Some(p) = n.parent().filter(|p| p.kind() == "decorated_definition") {
        n = p;
    }
    let mut start = n.start_byte();
    let mut prev = n.prev_sibling();
    while let Some(p) = prev {
        let k = p.kind();
        let attached = k.contains("comment") || k.contains("attribute") || k == "decorator";
        // Only when directly above (no blank line in between).
        let gap = &src[p.end_byte()..start];
        if !attached || gap.matches('\n').count() > 1 {
            break;
        }
        start = p.start_byte();
        prev = p.prev_sibling();
    }
    let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
    let start = if src[line_start..start].trim().is_empty() {
        line_start
    } else {
        start
    };
    let mut end = n.end_byte();
    if let Some(nl) = src[end..].find('\n')
        && src[end..end + nl].trim().is_empty()
    {
        end += nl + 1;
        // Swallow one blank line after it so no double gap is left.
        let line_before_blank = start == 0 || src[..start].ends_with("\n\n");
        if let Some(nl2) = src[end..].find('\n')
            && src[end..end + nl2].trim().is_empty()
            && line_before_blank
        {
            end += nl2 + 1;
        }
    }
    Some((start, end))
}

/// Kinds whose children are statements.
fn is_statement_list(kind: &str) -> bool {
    matches!(
        kind,
        "block" | "statement_block" | "source_file" | "program" | "module" | "switch_body"
    )
}

/// Extract the expression `start..end` (bytes) into a variable `name`
/// declared on the line before its statement. Errors say why not.
pub fn extract_variable(
    lang: Lang,
    src: &str,
    start: usize,
    end: usize,
    name: &str,
) -> Result<String, String> {
    let decl = match lang.name() {
        "rust" => |n: &str, e: &str| format!("let {n} = {e};"),
        "typescript" | "tsx" | "javascript" => |n: &str, e: &str| format!("const {n} = {e};"),
        "python" => |n: &str, e: &str| format!("{n} = {e}"),
        other => return Err(format!("extract variable: no fallback for {other}")),
    };
    let tree = parse(lang, src).ok_or("parse failed")?;
    let (s, e) = trim_range(src, start, end);
    let node: Node = tree
        .root_node()
        .descendant_for_byte_range(s, e)
        .ok_or("nothing selected")?;
    if node.start_byte() != s || node.end_byte() != e || !node.is_named() {
        return Err("select a whole expression".into());
    }
    // The statement holding it: the ancestor whose parent lists statements.
    let mut stmt = node;
    loop {
        let p = stmt.parent().ok_or("not inside a statement")?;
        if is_statement_list(p.kind()) {
            break;
        }
        stmt = p;
    }
    if stmt.id() == node.id() {
        return Err("select an expression inside a statement".into());
    }
    let line_start = src[..stmt.start_byte()].rfind('\n').map_or(0, |i| i + 1);
    let indent: String = src[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let mut out = String::with_capacity(src.len() + name.len() * 2 + 16);
    out.push_str(&src[..line_start]);
    out.push_str(&indent);
    out.push_str(&decl(name, &src[s..e]));
    out.push('\n');
    out.push_str(&src[line_start..s]);
    out.push_str(name);
    out.push_str(&src[e..]);
    Ok(out)
}

fn trim_range(src: &str, mut s: usize, mut e: usize) -> (usize, usize) {
    e = e.min(src.len());
    while s < e && src[s..].starts_with(char::is_whitespace) {
        s += src[s..].chars().next().map_or(1, char::len_utf8);
    }
    while e > s && src[..e].ends_with(char::is_whitespace) {
        e -= src[..e].chars().next_back().map_or(1, char::len_utf8);
    }
    (s, e)
}

#[cfg(all(test, feature = "rust", feature = "python", feature = "typescript"))]
mod tests {
    use super::*;

    #[test]
    fn enclosing_item_takes_docs_and_attributes() {
        let src = "fn a() {}\n\n/// doc\n#[inline]\nfn b() {\n    1\n}\n\nfn c() {}\n";
        let at = src.find("1").unwrap();
        let (s, e) = enclosing_item(Lang::Rust, src, at).unwrap();
        let out = format!("{}{}", &src[..s], &src[e..]);
        assert_eq!(out, "fn a() {}\n\nfn c() {}\n");
        let py = "import x\n\n@dec\ndef f():\n    return 1\n";
        let (s, _) = enclosing_item(Lang::Python, py, py.find("return").unwrap()).unwrap();
        assert_eq!(&py[s..s + 4], "@dec");
    }

    #[test]
    fn extract_variable_in_three_languages() {
        let src = "fn f() {\n    g(a + b, 2);\n}\n";
        let s = src.find("a + b").unwrap();
        let out = extract_variable(Lang::Rust, src, s, s + 5, "sum").unwrap();
        assert_eq!(out, "fn f() {\n    let sum = a + b;\n    g(sum, 2);\n}\n");
        let ts = "function f() {\n  return h(x * 2);\n}\n";
        let s = ts.find("x * 2").unwrap();
        let out = extract_variable(Lang::TypeScript, ts, s, s + 5, "y").unwrap();
        assert_eq!(
            out,
            "function f() {\n  const y = x * 2;\n  return h(y);\n}\n"
        );
        let py = "def f():\n    print(len(xs))\n";
        let s = py.find("len(xs)").unwrap();
        let out = extract_variable(Lang::Python, py, s, s + 7, "n").unwrap();
        assert_eq!(out, "def f():\n    n = len(xs)\n    print(n)\n");
        assert!(extract_variable(Lang::Rust, src, s, s + 2, "x").is_err());
    }
}
