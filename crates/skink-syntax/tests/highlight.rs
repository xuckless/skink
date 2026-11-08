#![allow(unused_imports, dead_code)] // individual languages are feature-gated
use skink_syntax::{Edit, Highlighter, Hl, Lang, Span, language_for_path};
use std::path::Path;

/// All `(line, text, hl)` triples for the whole text.
fn all(lang: Lang, text: &str) -> Vec<(usize, String, Hl)> {
    let mut h = Highlighter::new(lang);
    h.update(text);
    let n = text.split('\n').count();
    let lines = h.highlights(text, 0, n);
    let src: Vec<Vec<char>> = text.split('\n').map(|l| l.chars().collect()).collect();
    let mut out = Vec::new();
    for (i, spans) in lines.iter().enumerate() {
        for s in spans {
            out.push((i, src[i][s.start..s.end].iter().collect(), s.hl));
        }
    }
    out
}

#[track_caller]
fn expect(lang: Lang, text: &str, want: &[(&str, Hl)]) {
    let got = all(lang, text);
    for (tok, hl) in want {
        assert!(
            got.iter().any(|(_, t, h)| t == tok && h == hl),
            "{}: expected {tok:?} => {hl:?}\ngot: {got:#?}",
            lang.name()
        );
    }
}

#[test]
#[cfg(feature = "rust")]
fn rust() {
    expect(
        Lang::Rust,
        "// note\nfn main() { let x = 42; let s = \"a\\nb\"; foo.bar(); println!(\"x\"); }\nstruct Foo;\n#[derive(Debug)] enum E { A }\n",
        &[
            ("// note", Hl::Comment),
            ("fn", Hl::Keyword),
            ("main", Hl::Function),
            ("let", Hl::Keyword),
            ("42", Hl::Number),
            ("\\n", Hl::Escape),
            ("bar", Hl::Method),
            ("println!", Hl::Macro),
            ("Foo", Hl::Type),
            ("derive", Hl::Attribute),
        ],
    );
    // string content is split around the escape (innermost wins)
    expect(
        Lang::Rust,
        "let s = \"a\\nb\";",
        &[("\"a", Hl::String), ("b\"", Hl::String)],
    );
}

#[test]
#[cfg(feature = "typescript")]
fn typescript_and_tsx() {
    let ts = "// c\ninterface Foo { a: number }\nclass A { method(x: string): void { return 1; } }\nlet s = \"x\";\n";
    expect(
        Lang::TypeScript,
        ts,
        &[
            ("// c", Hl::Comment),
            ("interface", Hl::Keyword),
            ("Foo", Hl::Type),
            ("number", Hl::Builtin),
            ("method", Hl::Method),
            ("return", Hl::Keyword),
            ("1", Hl::Number),
            ("\"x\"", Hl::String),
            ("x", Hl::Parameter),
        ],
    );
    expect(
        Lang::Tsx,
        "const a = <div className=\"x\">{n}</div>;\n",
        &[
            ("div", Hl::Tag),
            ("className", Hl::Attribute),
            ("const", Hl::Keyword),
        ],
    );
}

#[test]
#[cfg(feature = "javascript")]
fn javascript() {
    expect(
        Lang::JavaScript,
        "// c\nfunction foo(a) { return 1.5 + a; }\nconst s = 'x', b = true;\nconst e = <p id=\"1\">t</p>;\n",
        &[
            ("// c", Hl::Comment),
            ("function", Hl::Keyword),
            ("foo", Hl::Function),
            ("1.5", Hl::Number),
            ("'x'", Hl::String),
            ("true", Hl::Boolean),
            ("p", Hl::Tag),
            ("id", Hl::Attribute),
        ],
    );
}

#[test]
#[cfg(feature = "python")]
fn python() {
    expect(
        Lang::Python,
        "# c\nclass Foo:\n    def bar(self):\n        return \"s\" + str(3)\n",
        &[
            ("# c", Hl::Comment),
            ("class", Hl::Keyword),
            ("def", Hl::Keyword),
            ("bar", Hl::Function),
            ("\"s\"", Hl::String),
            ("3", Hl::Number),
            ("str", Hl::Builtin),
            ("Foo", Hl::Constructor),
        ],
    );
}

#[test]
#[cfg(feature = "go")]
fn go() {
    expect(
        Lang::Go,
        "package main\n// c\nfunc main() {\n\tx := 42\n\tfmt.Println(\"hi\")\n}\n",
        &[
            ("package", Hl::Keyword),
            ("// c", Hl::Comment),
            ("func", Hl::Keyword),
            ("main", Hl::Function),
            ("42", Hl::Number),
            ("Println", Hl::Method),
            ("\"hi\"", Hl::String),
        ],
    );
}

#[test]
#[cfg(feature = "zig")]
fn zig() {
    expect(
        Lang::Zig,
        "const std = @import(\"std\");\n// c\npub fn main() void {\n    const x: u32 = 42;\n    _ = x;\n}\n",
        &[
            ("// c", Hl::Comment),
            ("pub", Hl::Keyword),
            ("fn", Hl::Keyword),
            ("main", Hl::Function),
            ("42", Hl::Number),
            ("\"std\"", Hl::String),
            ("u32", Hl::Builtin),
            ("x", Hl::Variable),
        ],
    );
}

#[test]
#[cfg(feature = "json")]
fn json() {
    expect(
        Lang::Json,
        "{\"a\": [1, 2.5, true, null], \"b\": \"x\"}\n",
        &[
            ("\"a\"", Hl::Property),
            ("1", Hl::Number),
            ("true", Hl::Boolean),
            ("null", Hl::Constant),
            ("\"x\"", Hl::String),
        ],
    );
}

#[test]
#[cfg(feature = "toml")]
fn toml() {
    expect(
        Lang::Toml,
        "# c\n[package]\nname = \"x\"\nn = 5\nok = true\n",
        &[
            ("# c", Hl::Comment),
            ("package", Hl::Type),
            ("name", Hl::Property),
            ("\"x\"", Hl::String),
            ("5", Hl::Number),
            ("true", Hl::Boolean),
        ],
    );
}

#[test]
#[cfg(feature = "bash")]
fn bash() {
    expect(
        Lang::Bash,
        "# c\nif [ -f x ]; then echo \"hi $x\"; fi\nfoo() { ls | grep a; }\n",
        &[
            ("# c", Hl::Comment),
            ("if", Hl::Keyword),
            ("then", Hl::Keyword),
            ("echo", Hl::Function),
            ("\"hi ", Hl::String),
            ("foo", Hl::Function),
            ("fi", Hl::Keyword),
        ],
    );
}

#[test]
#[cfg(feature = "rust")]
fn multibyte_columns_are_chars() {
    // emoji (4 bytes), CJK (3 bytes) and combining/accents before tokens
    let text = "let s = \"😀日本語\"; let n = 7; // é日\n// 日本語 😀\nlet 日本 = 1;\n";
    let mut h = Highlighter::new(Lang::Rust);
    h.update(text);
    let lines = h.highlights(text, 0, 3);
    let chars: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
    for (i, spans) in lines.iter().enumerate() {
        for s in spans {
            assert!(
                s.start < s.end && s.end <= chars[i].len(),
                "{s:?} in line {i}"
            );
        }
    }
    let n = lines[0].iter().find(|s| s.hl == Hl::Number).unwrap();
    assert_eq!(chars[0][n.start..n.end].iter().collect::<String>(), "7");
    assert_eq!(n.start, 24);
    let string = lines[0].iter().find(|s| s.hl == Hl::String).unwrap();
    assert_eq!(
        chars[0][string.start..string.end]
            .iter()
            .collect::<String>(),
        "\"😀日本語\""
    );
    let c = lines[0].iter().find(|s| s.hl == Hl::Comment).unwrap();
    assert_eq!(
        chars[0][c.start..c.end].iter().collect::<String>(),
        "// é日"
    );
    assert_eq!(c.end, chars[0].len());
    assert_eq!(
        lines[1],
        vec![Span {
            start: 0,
            end: chars[1].len(),
            hl: Hl::Comment
        }]
    );
    let kw = lines[2].iter().find(|s| s.hl == Hl::Keyword).unwrap();
    assert_eq!((kw.start, kw.end), (0, 3));
    let one = lines[2].iter().find(|s| s.hl == Hl::Number).unwrap();
    assert_eq!(chars[2][one.start], '1');
}

#[test]
#[cfg(feature = "rust")]
fn multiline_constructs_split_per_line() {
    let text = "/* a\n日本 b\n c */ fn x() {}\nlet s = \"l1\nl2\";\n";
    let mut h = Highlighter::new(Lang::Rust);
    h.update(text);
    let l = h.highlights(text, 0, 5);
    assert_eq!(
        l[0],
        vec![Span {
            start: 0,
            end: 4,
            hl: Hl::Comment
        }]
    );
    assert_eq!(
        l[1],
        vec![Span {
            start: 0,
            end: 4,
            hl: Hl::Comment
        }]
    );
    assert_eq!(
        l[2][0],
        Span {
            start: 0,
            end: 5,
            hl: Hl::Comment
        }
    );
    assert_eq!(
        l[2][1],
        Span {
            start: 6,
            end: 8,
            hl: Hl::Keyword
        }
    );
    assert_eq!(l[3].last().unwrap().hl, Hl::String);
    assert_eq!(
        l[4][0],
        Span {
            start: 0,
            end: 3,
            hl: Hl::String
        }
    );
    // starting inside a multi-line comment still highlights the visible part
    let l = h.highlights(text, 1, 2);
    assert_eq!(
        l,
        vec![vec![Span {
            start: 0,
            end: 4,
            hl: Hl::Comment
        }]]
    );
}

#[cfg(feature = "rust")]
fn big_rust(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!(
            "fn f{i}(a: i32) -> i32 {{ let s = \"日本{i}\"; a + {i} }} // c{i}\n"
        ));
    }
    s
}

#[test]
#[cfg(feature = "rust")]
fn range_limited_matches_full() {
    let text = big_rust(2000);
    let mut h = Highlighter::new(Lang::Rust);
    h.update(&text);
    let full = h.highlights(&text, 0, 5000);
    assert_eq!(full.len(), 2001); // trailing newline => final empty line
    for (a, b) in [(0, 1), (500, 520), (1999, 2000), (1990, 2100)] {
        let part = h.highlights(&text, a, b);
        assert_eq!(part.len(), b.min(2001) - a);
        assert_eq!(part[..], full[a..b.min(2001)], "range {a}..{b}");
    }
    assert!(h.highlights(&text, 5000, 6000).is_empty());
    assert!(h.highlights(&text, 3, 3).is_empty());
    assert!(h.highlights(&text, 5, 3).is_empty());
    // a huge upper bound must not allocate per requested line
    assert_eq!(h.highlights(&text, 1999, usize::MAX).len(), 2);
    assert!(h.highlights(&text, 2001, usize::MAX).is_empty());
}

#[test]
#[cfg(feature = "rust")]
fn spans_are_sorted_and_disjoint() {
    let text = big_rust(200);
    let mut h = Highlighter::new(Lang::Rust);
    h.update(&text);
    for spans in h.highlights(&text, 0, 200) {
        let mut end = 0;
        for s in spans {
            assert!(s.start >= end && s.start < s.end, "{s:?}");
            end = s.end;
        }
    }
}

#[test]
#[cfg(feature = "rust")]
fn edits_then_rehighlight_match_fresh_parse() {
    let mut text = String::from("fn a() {\n    let x = 1;\n}\n");
    let mut h = Highlighter::new(Lang::Rust);
    h.update(&text);
    let before = h.highlights(&text, 0, 4);

    // (start, old_end, replacement)
    let steps: &[(usize, usize, &str)] = &[
        (21, 22, "日本42"),   // replace the digit with multibyte+digits
        (0, 0, "// 😀 hi\n"), // insert a line at the top
        (12, 12, "pub "),     // insert in the middle
        (0, 10, ""),          // delete the comment line again
        (usize::MAX, usize::MAX, "\nfn tail() {}\n"), // append
    ];
    for &(s, e, rep) in steps {
        let s = s.min(text.len());
        let e = e.min(text.len());
        if !text.is_char_boundary(s) || !text.is_char_boundary(e) {
            continue;
        }
        let old = text.clone();
        text.replace_range(s..e, rep);
        let edit = Edit::from_texts(&old, &text, s, e, s + rep.len());
        h.update_with_edit(&text, &edit);
        let incr = h.highlights(&text, 0, 10);
        let mut fresh = Highlighter::new(Lang::Rust);
        fresh.update(&text);
        assert_eq!(
            incr,
            fresh.highlights(&text, 0, 10),
            "after edit {s}..{e} {rep:?}\n{text}"
        );
    }
    assert_ne!(before, h.highlights(&text, 0, 4));

    // several edits applied in one update
    let old = text.clone();
    let mut t2 = old.clone();
    t2.insert_str(0, "// x\n");
    let e1 = Edit::from_texts(&old, &t2, 0, 0, 5);
    let t1 = t2.clone();
    t2.push_str("fn z() {}\n");
    let e2 = Edit::from_texts(&t1, &t2, t1.len(), t1.len(), t2.len());
    h.update_with_edits(&t2, &[e1, e2]);
    let mut fresh = Highlighter::new(Lang::Rust);
    fresh.update(&t2);
    let n = t2.lines().count() + 1;
    assert_eq!(h.highlights(&t2, 0, n), fresh.highlights(&t2, 0, n));
}

#[test]
#[cfg(feature = "rust")]
fn stale_text_does_not_panic_and_recovers() {
    let mut h = Highlighter::new(Lang::Rust);
    h.update("fn a() {}\n");
    // caller forgot to update: length differs, we rebuild
    let t = "let 日本 = \"😀\";\n// x\n";
    let l = h.highlights(t, 0, 2);
    assert_eq!(
        l[1],
        vec![Span {
            start: 0,
            end: 4,
            hl: Hl::Comment
        }]
    );
    // edit with a bogus (out of range) edit must not panic either
    let bogus = Edit {
        start_byte: 1000,
        old_end_byte: 2000,
        new_end_byte: 3000,
        ..Default::default()
    };
    h.update_with_edit("fn b() {}\n", &bogus);
    let _ = h.highlights("fn b() {}\n", 0, 1);
}

#[test]
fn empty_and_tiny_text() {
    for &lang in Lang::all() {
        let mut h = Highlighter::new(lang);
        // before any update
        assert_eq!(h.highlights("", 0, 3).len(), 1);
        h.update("");
        let l = h.highlights("", 0, 3);
        assert_eq!(l, vec![Vec::<Span>::new()], "{}", lang.name());
        for t in ["\n", "\n\n", " ", "\r\n", "x", "日"] {
            h.update(t);
            let n = t.split('\n').count();
            assert_eq!(h.highlights(t, 0, n + 2).len(), n, "{} {t:?}", lang.name());
        }
        assert_eq!(h.bracket_pair("", 0), None);
    }
}

#[test]
fn invalid_and_partial_code_never_panics() {
    let garbage = [
        "}}}}{{{{ ((( \"unterminated\n'x\n/* open\n",
        "fn fn fn let = = ; ; <<< >>> ===\n",
        "\u{0}\u{1}\u{7f}\u{fffd} 😀😀😀 \\\\ \\\"\n",
        "<div <div> </span> ${ `\n",
        "def (:\n  return\n\t  x\n",
        "[[[[ = ]]] = \"\"\"\n",
        "if then ${ $( ` ' \"\n",
    ];
    for &lang in Lang::all() {
        let mut h = Highlighter::new(lang);
        for g in garbage {
            h.update(g);
            let n = g.split('\n').count();
            let l = h.highlights(g, 0, n);
            let chars: Vec<usize> = g.split('\n').map(|l| l.chars().count()).collect();
            for (i, spans) in l.iter().enumerate() {
                for s in spans {
                    assert!(
                        s.start < s.end && s.end <= chars[i],
                        "{} {s:?} line {i} of {g:?}",
                        lang.name()
                    );
                }
            }
            for p in 0..=g.len() {
                let _ = h.bracket_pair(g, p);
            }
        }
    }
}

/// Every prefix of a realistic sample (cut at char boundaries) is partial code.
#[test]
#[cfg(feature = "rust")]
fn every_prefix_of_a_sample_is_safe() {
    let text = "fn 日本() -> Option<&'static str> { let x = \"😀\\n\"; /* c */ Some(x) }\n#[test] struct S<T>(T);";
    let mut h = Highlighter::new(Lang::Rust);
    for (i, _) in text.char_indices().chain([(text.len(), ' ')]) {
        let t = &text[..i];
        h.update(t);
        let n = t.split('\n').count();
        for spans in h.highlights(t, 0, n) {
            for s in spans {
                assert!(s.start < s.end);
            }
        }
    }
}

#[test]
#[cfg(feature = "rust")]
fn crlf_lines_exclude_carriage_return() {
    let text = "// a\r\nfn x() {}\r\n";
    let mut h = Highlighter::new(Lang::Rust);
    h.update(text);
    let l = h.highlights(text, 0, 3);
    assert_eq!(l.len(), 3);
    assert_eq!(
        l[0],
        vec![Span {
            start: 0,
            end: 4,
            hl: Hl::Comment
        }]
    );
    assert_eq!(
        l[1][0],
        Span {
            start: 0,
            end: 2,
            hl: Hl::Keyword
        }
    );
    assert!(l[2].is_empty());
}

#[test]
#[cfg(feature = "rust")]
fn bracket_pairs() {
    let text = "fn f(a: (i32, [u8; 2])) { \"(\"; }\n";
    let mut h = Highlighter::new(Lang::Rust);
    assert_eq!(h.bracket_pair(text, 4), Some((4, 22)));
    assert_eq!(h.bracket_pair(text, 22), Some((4, 22)));
    // just after the closing paren also matches
    assert_eq!(h.bracket_pair(text, 23), Some((4, 22)));
    assert_eq!(h.bracket_pair(text, 8), Some((8, 21)));
    assert_eq!(h.bracket_pair(text, 14), Some((14, 20)));
    // the "(" inside a string literal is not a bracket token
    let s = text.find("\"(\"").unwrap() + 1;
    assert_eq!(h.bracket_pair(text, s), None);
    assert_eq!(h.bracket_pair(text, 1), None);
    assert_eq!(h.bracket_pair(text, 10_000), None);
    // braces
    let b = text.find('{').unwrap();
    assert_eq!(h.bracket_pair(text, b), Some((b, text.rfind('}').unwrap())));
}

#[test]
fn path_detection() {
    let p = |s: &str| language_for_path(Path::new(s));
    #[cfg(feature = "rust")]
    {
        assert_eq!(p("src/main.rs"), Some(Lang::Rust));
        assert_eq!(p("A.RS"), Some(Lang::Rust));
    }
    #[cfg(feature = "typescript")]
    {
        assert_eq!(p("a.ts"), Some(Lang::TypeScript));
        assert_eq!(p("a.mts"), Some(Lang::TypeScript));
        assert_eq!(p("a.tsx"), Some(Lang::Tsx));
    }
    #[cfg(feature = "javascript")]
    {
        assert_eq!(p("a.js"), Some(Lang::JavaScript));
        assert_eq!(p("a.mjs"), Some(Lang::JavaScript));
        assert_eq!(p("a.jsx"), Some(Lang::JavaScript));
    }
    #[cfg(feature = "python")]
    assert_eq!(p("a/b.py"), Some(Lang::Python));
    #[cfg(feature = "go")]
    assert_eq!(p("main.go"), Some(Lang::Go));
    #[cfg(feature = "zig")]
    {
        assert_eq!(p("build.zig"), Some(Lang::Zig));
        assert_eq!(p("build.zig.zon"), Some(Lang::Zig));
    }
    #[cfg(feature = "json")]
    {
        assert_eq!(p("a.json"), Some(Lang::Json));
        assert_eq!(p(".eslintrc"), Some(Lang::Json));
    }
    #[cfg(feature = "toml")]
    {
        assert_eq!(p("Cargo.toml"), Some(Lang::Toml));
        assert_eq!(p("Cargo.lock"), Some(Lang::Toml));
    }
    #[cfg(feature = "bash")]
    {
        assert_eq!(p("x.sh"), Some(Lang::Bash));
        assert_eq!(p("/home/u/.bashrc"), Some(Lang::Bash));
        assert_eq!(p(".zshrc"), Some(Lang::Bash));
    }
    assert_eq!(p("README"), None);
    assert_eq!(p("a.unknownext"), None);
    assert_eq!(p(""), None);
    assert_eq!(p("dir/"), None);
}

#[test]
fn lang_names_roundtrip() {
    for &l in Lang::all() {
        assert_eq!(Lang::from_name(l.name()), Some(l));
    }
    #[cfg(feature = "rust")]
    assert_eq!(Lang::Rust.name(), "rust");
    #[cfg(feature = "typescript")]
    assert_eq!(Lang::from_name("TS"), Some(Lang::TypeScript));
    assert_eq!(Lang::from_name("cobol"), None);
}

#[test]
fn highlighter_is_send_and_clear_works() {
    fn is_send<T: Send>() {}
    is_send::<Highlighter>();
    is_send::<Lang>();
    if let Some(&lang) = Lang::all().first() {
        let mut h = Highlighter::new(lang);
        h.prepare();
        h.update("x");
        h.clear();
        assert_eq!(h.lang(), lang);
        // still usable: re-parses on demand
        assert_eq!(h.highlights("x", 0, 1).len(), 1);
        let t = std::thread::spawn(move || h.highlights("x", 0, 1).len());
        assert_eq!(t.join().unwrap(), 1);
    }
}

/// Every language's query compiles and produces some highlights for a trivial
/// program (guards against grammar/query version drift).
#[test]
fn every_query_compiles_and_highlights() {
    let samples: &[(&str, &str)] = &[
        ("rust", "fn main() { let x = 1; }"),
        ("typescript", "const x: number = 1;"),
        ("tsx", "const x = <a b=\"1\" />;"),
        ("javascript", "const x = 1;"),
        ("python", "def f():\n    return 1"),
        ("go", "package main\nfunc f() {}"),
        ("zig", "const x = 1;"),
        ("json", "{\"a\": 1}"),
        ("toml", "a = 1"),
        ("bash", "echo hi"),
    ];
    for &lang in Lang::all() {
        let (_, src) = samples.iter().find(|(n, _)| *n == lang.name()).unwrap();
        assert!(
            !all(lang, src).is_empty(),
            "{} produced no highlights",
            lang.name()
        );
    }
}
