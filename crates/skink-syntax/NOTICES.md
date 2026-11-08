# skink-syntax notices

No source code was copied into this crate. Everything here is original Skink
code (MIT OR Apache-2.0) that *uses* the crates below through crates.io.
The highlight queries are not copied into the tree either: they are read from
the grammar crates at compile time (`HIGHLIGHTS_QUERY` constants) and layered
in `src/lang.rs`.

| Component | Version | License | Copyright / source |
|---|---|---|---|
| tree-sitter (runtime) | 0.27.2 | MIT | (c) 2018 Max Brunsfeld, https://github.com/tree-sitter/tree-sitter |
| tree-sitter-language | 0.1.9 | MIT | same repository |
| streaming-iterator | 0.1.9 | MIT OR Apache-2.0 | (c) 2016 Steven Fackler |
| regex (+ regex-automata, regex-syntax, memchr) | 1.13.1 | MIT OR Apache-2.0 | Rust Project Developers; used by tree-sitter for `#match?` |
| tree-sitter-rust (grammar + queries) | 0.24.2 | MIT | (c) 2017 Maxim Sokolov, https://github.com/tree-sitter/tree-sitter-rust |
| tree-sitter-typescript (TypeScript + TSX grammars, queries) | 0.23.2 | MIT | (c) 2017 Max Brunsfeld, https://github.com/tree-sitter/tree-sitter-typescript (no LICENSE file in the crate package; MIT confirmed in upstream repo) |
| tree-sitter-javascript (grammar + queries, JSX) | 0.25.0 | MIT | (c) 2014 Max Brunsfeld, https://github.com/tree-sitter/tree-sitter-javascript |
| tree-sitter-python | 0.25.0 | MIT | (c) 2016 Max Brunsfeld, https://github.com/tree-sitter/tree-sitter-python |
| tree-sitter-go | 0.25.0 | MIT | (c) 2014 Max Brunsfeld, https://github.com/tree-sitter/tree-sitter-go |
| tree-sitter-zig | 1.1.2 | MIT | (c) 2024 Amaan Qureshi, https://github.com/tree-sitter-grammars/tree-sitter-zig (no LICENSE file in the crate package; MIT confirmed upstream). Its query originates from nvim-treesitter (Apache-2.0) |
| tree-sitter-json | 0.24.8 | MIT | (c) 2014 Max Brunsfeld, https://github.com/tree-sitter/tree-sitter-json (no LICENSE file in the crate package; MIT confirmed upstream) |
| tree-sitter-toml-ng | 0.7.0 | MIT | (c) Ika (ikatyang) / tree-sitter-grammars, https://github.com/tree-sitter-grammars/tree-sitter-toml (no LICENSE file in the crate package; MIT confirmed upstream) |
| tree-sitter-bash | 0.25.1 | MIT | (c) 2017 Max Brunsfeld, https://github.com/tree-sitter/tree-sitter-bash |

`cargo deny check licenses` (repo `deny.toml`) passes with these.

## Modifications to the bundled queries (`src/lang.rs`, original code)

* TypeScript/TSX: the JavaScript query is layered under the TypeScript one
  (and JSX under TSX/JS), as the grammar READMEs instruct.
* Go: the generic `(identifier) @variable` family is hoisted above the specific
  patterns (the bundled file is ordered for first-match-wins; we use
  later-wins).
* TOML: the whole-pair `@property` pattern is dropped and keys are coloured
  instead.
* Zig: `#lua-match?` (Neovim-only, silently always-true in the Rust bindings)
  is rewritten to `#match?`; the patterns involved are valid regexes.
* A few appended patterns (number/boolean literals for Rust, booleans for
  JS/TS/Python/Go/JSON, JSON keys as properties).

Not used as a source: nvim-treesitter queries (Apache-2.0) were considered but
the bundled grammar queries were good enough and the Neovim query dialect
(`lua-match?`, `set!`, `inherits`) would need translating. Helix and Zed
queries (MPL/GPL) were never consulted.
