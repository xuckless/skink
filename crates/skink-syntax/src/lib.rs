//! Tree-sitter syntax highlighting for the Skink editor (UI-free).
//!
//! ```
//! # #[cfg(feature = "rust")] {
//! use skink_syntax::{language_for_path, Highlighter, Hl};
//! let lang = language_for_path("main.rs".as_ref()).unwrap();
//! let mut h = Highlighter::new(lang);
//! let text = "fn main() {}\n";
//! h.update(text);
//! let lines = h.highlights(text, 0, 1);
//! assert_eq!(lines[0][0].hl, Hl::Keyword); // `fn`
//! # }
//! ```
//!
//! * [`language_for_path`] / [`Lang`]: file type detection.
//! * [`Highlighter`]: parse (full or incremental), query visible lines, bracket matching.
//! * [`Hl`]: highlight classes, mapped from capture names (see its docs).
//!
//! Each language is behind a cargo feature. On by default: `rust`,
//! `typescript`, `javascript`, `python`, `go`, `zig`, `json`, `toml`, `bash`,
//! `markdown`, `yaml`, `html`, `css`, `lua`, `make`, `ini`, `diff`, `xml`.
//! `all-langs` adds the bigger grammars (`c`, `cpp`, `java`, `csharp`, `ruby`,
//! `php`, `sql`, `elixir`, `dart`, `haskell`, `swift`, `scala`, `nix`,
//! `svelte`). Injection queries highlight embedded code (Markdown fences and
//! inline markup, HTML `<script>`/`<style>`, Svelte, PHP, Lua).

mod highlighter;
mod hl;
mod items;
mod lang;
mod structural;

pub use highlighter::{
    CompileMode, Edit, Highlighter, Pos, compile_queries_in_background, warm_up,
};
pub use hl::Hl;
pub use items::{enclosing_item, extract_variable};
pub use lang::{Lang, language_for_path};
pub use structural::{StructMatch, structural_find, structural_replace};

/// A highlighted stretch of one line, in **char columns**: `start..end`
/// (half-open, `char` indices, not bytes and not display cells).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub hl: Hl,
}
