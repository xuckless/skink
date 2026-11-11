//! Language identification, grammars and highlight queries.

use std::path::Path;
use tree_sitter::Language;

/// A supported language. Variants exist only when the matching cargo feature
/// is enabled (all are on by default).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Lang {
    #[cfg(feature = "rust")]
    Rust,
    #[cfg(feature = "typescript")]
    TypeScript,
    #[cfg(feature = "typescript")]
    Tsx,
    /// JavaScript including JSX.
    #[cfg(feature = "javascript")]
    JavaScript,
    #[cfg(feature = "python")]
    Python,
    #[cfg(feature = "go")]
    Go,
    #[cfg(feature = "zig")]
    Zig,
    #[cfg(feature = "json")]
    Json,
    #[cfg(feature = "toml")]
    Toml,
    #[cfg(feature = "bash")]
    Bash,
    #[cfg(feature = "markdown")]
    Markdown,
    #[cfg(feature = "markdown")]
    MarkdownInline,
    #[cfg(feature = "yaml")]
    Yaml,
    #[cfg(feature = "html")]
    Html,
    #[cfg(feature = "css")]
    Css,
    #[cfg(feature = "lua")]
    Lua,
    #[cfg(feature = "make")]
    Make,
    #[cfg(feature = "ini")]
    Ini,
    #[cfg(feature = "diff")]
    Diff,
    #[cfg(feature = "xml")]
    Xml,
    #[cfg(feature = "c")]
    C,
    #[cfg(feature = "cpp")]
    Cpp,
    #[cfg(feature = "java")]
    Java,
    #[cfg(feature = "csharp")]
    CSharp,
    #[cfg(feature = "ruby")]
    Ruby,
    #[cfg(feature = "php")]
    Php,
    #[cfg(feature = "sql")]
    Sql,
    #[cfg(feature = "elixir")]
    Elixir,
    #[cfg(feature = "dart")]
    Dart,
    #[cfg(feature = "haskell")]
    Haskell,
    #[cfg(feature = "swift")]
    Swift,
    #[cfg(feature = "scala")]
    Scala,
    #[cfg(feature = "nix")]
    Nix,
    #[cfg(feature = "svelte")]
    Svelte,
    /// A grammar installed from a package (tree-sitter WASM).
    #[cfg(feature = "wasm")]
    Dyn(crate::dynlang::DynLang),
}

impl Lang {
    /// All languages compiled in.
    pub fn all() -> &'static [Lang] {
        &[
            #[cfg(feature = "rust")]
            Lang::Rust,
            #[cfg(feature = "typescript")]
            Lang::TypeScript,
            #[cfg(feature = "typescript")]
            Lang::Tsx,
            #[cfg(feature = "javascript")]
            Lang::JavaScript,
            #[cfg(feature = "python")]
            Lang::Python,
            #[cfg(feature = "go")]
            Lang::Go,
            #[cfg(feature = "zig")]
            Lang::Zig,
            #[cfg(feature = "json")]
            Lang::Json,
            #[cfg(feature = "toml")]
            Lang::Toml,
            #[cfg(feature = "bash")]
            Lang::Bash,
            #[cfg(feature = "markdown")]
            Lang::Markdown,
            #[cfg(feature = "markdown")]
            Lang::MarkdownInline,
            #[cfg(feature = "yaml")]
            Lang::Yaml,
            #[cfg(feature = "html")]
            Lang::Html,
            #[cfg(feature = "css")]
            Lang::Css,
            #[cfg(feature = "lua")]
            Lang::Lua,
            #[cfg(feature = "make")]
            Lang::Make,
            #[cfg(feature = "ini")]
            Lang::Ini,
            #[cfg(feature = "diff")]
            Lang::Diff,
            #[cfg(feature = "xml")]
            Lang::Xml,
            #[cfg(feature = "c")]
            Lang::C,
            #[cfg(feature = "cpp")]
            Lang::Cpp,
            #[cfg(feature = "java")]
            Lang::Java,
            #[cfg(feature = "csharp")]
            Lang::CSharp,
            #[cfg(feature = "ruby")]
            Lang::Ruby,
            #[cfg(feature = "php")]
            Lang::Php,
            #[cfg(feature = "sql")]
            Lang::Sql,
            #[cfg(feature = "elixir")]
            Lang::Elixir,
            #[cfg(feature = "dart")]
            Lang::Dart,
            #[cfg(feature = "haskell")]
            Lang::Haskell,
            #[cfg(feature = "swift")]
            Lang::Swift,
            #[cfg(feature = "scala")]
            Lang::Scala,
            #[cfg(feature = "nix")]
            Lang::Nix,
            #[cfg(feature = "svelte")]
            Lang::Svelte,
        ]
    }

    /// Stable lower-case identifier: `"rust"`, `"typescript"`, `"markdown"`, ...
    /// (the names code fences and injection queries use).
    pub fn name(self) -> &'static str {
        match self {
            #[cfg(feature = "rust")]
            Lang::Rust => "rust",
            #[cfg(feature = "typescript")]
            Lang::TypeScript => "typescript",
            #[cfg(feature = "typescript")]
            Lang::Tsx => "tsx",
            #[cfg(feature = "javascript")]
            Lang::JavaScript => "javascript",
            #[cfg(feature = "python")]
            Lang::Python => "python",
            #[cfg(feature = "go")]
            Lang::Go => "go",
            #[cfg(feature = "zig")]
            Lang::Zig => "zig",
            #[cfg(feature = "json")]
            Lang::Json => "json",
            #[cfg(feature = "toml")]
            Lang::Toml => "toml",
            #[cfg(feature = "bash")]
            Lang::Bash => "bash",
            #[cfg(feature = "markdown")]
            Lang::Markdown => "markdown",
            #[cfg(feature = "markdown")]
            Lang::MarkdownInline => "markdown_inline",
            #[cfg(feature = "yaml")]
            Lang::Yaml => "yaml",
            #[cfg(feature = "html")]
            Lang::Html => "html",
            #[cfg(feature = "css")]
            Lang::Css => "css",
            #[cfg(feature = "lua")]
            Lang::Lua => "lua",
            #[cfg(feature = "make")]
            Lang::Make => "make",
            #[cfg(feature = "ini")]
            Lang::Ini => "ini",
            #[cfg(feature = "diff")]
            Lang::Diff => "diff",
            #[cfg(feature = "xml")]
            Lang::Xml => "xml",
            #[cfg(feature = "c")]
            Lang::C => "c",
            #[cfg(feature = "cpp")]
            Lang::Cpp => "cpp",
            #[cfg(feature = "java")]
            Lang::Java => "java",
            #[cfg(feature = "csharp")]
            Lang::CSharp => "csharp",
            #[cfg(feature = "ruby")]
            Lang::Ruby => "ruby",
            #[cfg(feature = "php")]
            Lang::Php => "php",
            #[cfg(feature = "sql")]
            Lang::Sql => "sql",
            #[cfg(feature = "elixir")]
            Lang::Elixir => "elixir",
            #[cfg(feature = "dart")]
            Lang::Dart => "dart",
            #[cfg(feature = "haskell")]
            Lang::Haskell => "haskell",
            #[cfg(feature = "swift")]
            Lang::Swift => "swift",
            #[cfg(feature = "scala")]
            Lang::Scala => "scala",
            #[cfg(feature = "nix")]
            Lang::Nix => "nix",
            #[cfg(feature = "svelte")]
            Lang::Svelte => "svelte",
            #[cfg(feature = "wasm")]
            Lang::Dyn(d) => d.name(),
        }
    }

    /// Inverse of [`Lang::name`] (also accepts a few common aliases such as
    /// `"rs"`, `"ts"`, `"js"`, `"py"`, `"sh"`).
    pub fn from_name(name: &str) -> Option<Lang> {
        let n = name.to_ascii_lowercase();
        let built_in = Lang::all().iter().copied().find(|l| {
            l.name() == n
                || matches!(
                    (l.name(), n.as_str()),
                    ("rust", "rs")
                        | ("typescript", "ts")
                        | ("javascript", "js" | "jsx")
                        | ("python", "py")
                        | ("bash", "sh" | "shell" | "zsh")
                        | ("markdown", "md")
                        | ("yaml", "yml")
                        | ("make", "makefile")
                        | ("diff", "patch")
                        | ("cpp", "c++")
                        | ("csharp", "c#")
                        | ("csharp", "cs")
                        | ("ruby", "rb")
                        | ("elixir", "ex")
                        | ("haskell", "hs")
                )
        });
        #[cfg(feature = "wasm")]
        if built_in.is_none() {
            return crate::dynlang::by_name(&n).map(Lang::Dyn);
        }
        built_in
    }

    /// A parser set to this language (a WASM grammar gets its own store).
    /// `None` when a package grammar cannot load.
    pub(crate) fn parser(self) -> Option<tree_sitter::Parser> {
        let g = self.grammar()?;
        let mut p = tree_sitter::Parser::new();
        #[cfg(feature = "wasm")]
        if g.is_wasm() {
            crate::dynlang::attach_store(&mut p)?;
        }
        p.set_language(&g).ok()?;
        Some(p)
    }

    /// The grammar; `None` only for a package grammar that cannot load.
    pub(crate) fn grammar(self) -> Option<Language> {
        Some(match self {
            #[cfg(feature = "rust")]
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            #[cfg(feature = "typescript")]
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            #[cfg(feature = "typescript")]
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            #[cfg(feature = "javascript")]
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            #[cfg(feature = "python")]
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            #[cfg(feature = "go")]
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            #[cfg(feature = "zig")]
            Lang::Zig => tree_sitter_zig::LANGUAGE.into(),
            #[cfg(feature = "json")]
            Lang::Json => tree_sitter_json::LANGUAGE.into(),
            #[cfg(feature = "toml")]
            Lang::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            #[cfg(feature = "bash")]
            Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
            #[cfg(feature = "markdown")]
            Lang::Markdown => tree_sitter_md::LANGUAGE.into(),
            #[cfg(feature = "markdown")]
            Lang::MarkdownInline => tree_sitter_md::INLINE_LANGUAGE.into(),
            #[cfg(feature = "yaml")]
            Lang::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            #[cfg(feature = "html")]
            Lang::Html => tree_sitter_html::LANGUAGE.into(),
            #[cfg(feature = "css")]
            Lang::Css => tree_sitter_css::LANGUAGE.into(),
            #[cfg(feature = "lua")]
            Lang::Lua => tree_sitter_lua::LANGUAGE.into(),
            #[cfg(feature = "make")]
            Lang::Make => tree_sitter_make::LANGUAGE.into(),
            #[cfg(feature = "ini")]
            Lang::Ini => tree_sitter_ini::LANGUAGE.into(),
            #[cfg(feature = "diff")]
            Lang::Diff => tree_sitter_diff::LANGUAGE.into(),
            #[cfg(feature = "xml")]
            Lang::Xml => tree_sitter_xml::LANGUAGE_XML.into(),
            #[cfg(feature = "c")]
            Lang::C => tree_sitter_c::LANGUAGE.into(),
            #[cfg(feature = "cpp")]
            Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            #[cfg(feature = "java")]
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            #[cfg(feature = "csharp")]
            Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            #[cfg(feature = "ruby")]
            Lang::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            #[cfg(feature = "php")]
            Lang::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            #[cfg(feature = "sql")]
            Lang::Sql => tree_sitter_sequel::LANGUAGE.into(),
            #[cfg(feature = "elixir")]
            Lang::Elixir => tree_sitter_elixir::LANGUAGE.into(),
            #[cfg(feature = "dart")]
            Lang::Dart => tree_sitter_dart::LANGUAGE.into(),
            #[cfg(feature = "haskell")]
            Lang::Haskell => tree_sitter_haskell::LANGUAGE.into(),
            #[cfg(feature = "swift")]
            Lang::Swift => tree_sitter_swift::LANGUAGE.into(),
            #[cfg(feature = "scala")]
            Lang::Scala => tree_sitter_scala::LANGUAGE.into(),
            #[cfg(feature = "nix")]
            Lang::Nix => tree_sitter_nix::LANGUAGE.into(),
            #[cfg(feature = "svelte")]
            Lang::Svelte => tree_sitter_svelte_ng::LANGUAGE.into(),
            #[cfg(feature = "wasm")]
            Lang::Dyn(d) => return d.language(),
        })
    }

    /// The highlight query source: the grammar crate's bundled query (layered
    /// where the grammar README says so) plus small Skink additions.
    ///
    /// Later patterns take precedence over earlier ones when two captures
    /// cover exactly the same node (Neovim convention), so the additions are
    /// appended at the end.
    #[allow(unreachable_code)]
    pub(crate) fn query_source(self) -> String {
        let mut q: String = match self {
            #[cfg(feature = "rust")]
            Lang::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
            // The TypeScript query only adds TS-specific patterns; it is meant
            // to be layered on the JavaScript one (as the grammar README says).
            #[cfg(feature = "typescript")]
            Lang::TypeScript => format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            ),
            #[cfg(feature = "typescript")]
            Lang::Tsx => format!(
                "{}\n{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            ),
            #[cfg(feature = "javascript")]
            Lang::JavaScript => format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            ),
            #[cfg(feature = "python")]
            Lang::Python => tree_sitter_python::HIGHLIGHTS_QUERY.to_string(),
            // The bundled Go query lists the generic identifier captures
            // *after* the specific ones (first-match-wins style). Hoist them
            // so that function/method names win under later-wins semantics.
            #[cfg(feature = "go")]
            Lang::Go => format!(
                "(type_identifier) @type\n(field_identifier) @property\n(identifier) @variable\n{}",
                tree_sitter_go::HIGHLIGHTS_QUERY
                    .replace("(type_identifier) @type\n", "")
                    .replace("(field_identifier) @property\n", "")
                    .replace("(identifier) @variable\n", "")
            ),
            #[cfg(feature = "zig")]
            Lang::Zig => tree_sitter_zig::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "json")]
            Lang::Json => tree_sitter_json::HIGHLIGHTS_QUERY.to_string(),
            // The bundled query paints the whole `key = value` pair as
            // `@property`; drop that and colour just the key (see below).
            #[cfg(feature = "toml")]
            Lang::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY
                .replace("(pair\n  (bare_key)) @property\n", ""),
            #[cfg(feature = "bash")]
            Lang::Bash => tree_sitter_bash::HIGHLIGHT_QUERY.to_string(),
            #[cfg(feature = "markdown")]
            Lang::Markdown => tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.to_string(),
            #[cfg(feature = "markdown")]
            Lang::MarkdownInline => tree_sitter_md::HIGHLIGHT_QUERY_INLINE.to_string(),
            #[cfg(feature = "yaml")]
            Lang::Yaml => tree_sitter_yaml::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "html")]
            Lang::Html => tree_sitter_html::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "css")]
            Lang::Css => tree_sitter_css::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "lua")]
            Lang::Lua => tree_sitter_lua::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "make")]
            Lang::Make => tree_sitter_make::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "ini")]
            Lang::Ini => tree_sitter_ini::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "diff")]
            Lang::Diff => tree_sitter_diff::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "xml")]
            Lang::Xml => tree_sitter_xml::XML_HIGHLIGHT_QUERY.to_string(),
            #[cfg(feature = "c")]
            Lang::C => tree_sitter_c::HIGHLIGHT_QUERY.to_string(),
            // The C++ query only adds C++ patterns on top of the C one.
            #[cfg(feature = "cpp")]
            Lang::Cpp => format!(
                "{}\n{}",
                tree_sitter_c::HIGHLIGHT_QUERY,
                tree_sitter_cpp::HIGHLIGHT_QUERY
            ),
            #[cfg(feature = "java")]
            Lang::Java => tree_sitter_java::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "csharp")]
            Lang::CSharp => tree_sitter_c_sharp::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "ruby")]
            Lang::Ruby => tree_sitter_ruby::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "php")]
            Lang::Php => tree_sitter_php::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "sql")]
            Lang::Sql => tree_sitter_sequel::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "elixir")]
            Lang::Elixir => tree_sitter_elixir::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "dart")]
            Lang::Dart => tree_sitter_dart::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "haskell")]
            Lang::Haskell => tree_sitter_haskell::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "swift")]
            Lang::Swift => tree_sitter_swift::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "scala")]
            Lang::Scala => tree_sitter_scala::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "nix")]
            Lang::Nix => tree_sitter_nix::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "svelte")]
            Lang::Svelte => tree_sitter_svelte_ng::HIGHLIGHTS_QUERY.to_string(),
            #[cfg(feature = "wasm")]
            Lang::Dyn(d) => d.highlights(),
        };
        // Some bundled queries (Zig) come from Neovim and use `#lua-match?`,
        // which the Rust bindings would silently treat as always-true. The
        // patterns used are plain enough to be valid regexes, so map them.
        if q.contains("#lua-match?") {
            q = q.replace("#lua-match?", "#match?");
        }
        // Skink additions (original, MIT OR Apache-2.0): number/boolean literals
        // get their own classes; the bundled queries fold them into constants.
        q.push('\n');
        q.push_str(match self {
            #[cfg(feature = "rust")]
            Lang::Rust => {
                "[(integer_literal) (float_literal)] @number\n(boolean_literal) @boolean\n"
            }
            #[cfg(feature = "typescript")]
            Lang::TypeScript | Lang::Tsx => "[(true) (false)] @boolean\n",
            #[cfg(feature = "javascript")]
            Lang::JavaScript => "[(true) (false)] @boolean\n",
            #[cfg(feature = "python")]
            Lang::Python => "[(true) (false)] @boolean\n",
            #[cfg(feature = "go")]
            Lang::Go => "[(true) (false)] @boolean\n",
            #[cfg(feature = "json")]
            Lang::Json => "[(true) (false)] @boolean\n(pair key: (_) @property)\n",
            #[cfg(feature = "toml")]
            Lang::Toml => "(pair (bare_key) @property)\n",
            #[allow(unreachable_patterns)]
            _ => "",
        });
        q
    }

    /// The first error compiling this language's highlight or injection query
    /// (queries that fail to compile would silently disable highlighting).
    #[doc(hidden)]
    pub fn query_error(self) -> Option<String> {
        let Some(g) = self.grammar() else {
            return Some("the grammar does not load".into());
        };
        if let Err(e) = tree_sitter::Query::new(&g, &self.query_source()) {
            return Some(format!("highlights: {e}"));
        }
        if let Some(src) = self.injection_source()
            && let Err(e) = tree_sitter::Query::new(&g, &src)
        {
            return Some(format!("injections: {e}"));
        }
        None
    }

    /// The injection query (embedded languages: code fences in Markdown,
    /// `<script>` / `<style>` in HTML), if the grammar ships one.
    pub(crate) fn injection_source(self) -> Option<std::borrow::Cow<'static, str>> {
        match self {
            #[cfg(feature = "markdown")]
            Lang::Markdown => Some(tree_sitter_md::INJECTION_QUERY_BLOCK.into()),
            #[cfg(feature = "markdown")]
            Lang::MarkdownInline => Some(tree_sitter_md::INJECTION_QUERY_INLINE.into()),
            #[cfg(feature = "html")]
            Lang::Html => Some(tree_sitter_html::INJECTIONS_QUERY.into()),
            #[cfg(feature = "lua")]
            Lang::Lua => Some(tree_sitter_lua::INJECTIONS_QUERY.into()),
            #[cfg(feature = "php")]
            Lang::Php => Some(tree_sitter_php::INJECTIONS_QUERY.into()),
            #[cfg(feature = "svelte")]
            Lang::Svelte => Some(tree_sitter_svelte_ng::INJECTIONS_QUERY.into()),
            #[cfg(feature = "wasm")]
            Lang::Dyn(d) => d.injections().map(Into::into),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }
}

/// Detects the language of a file from its name or extension (case-insensitive).
///
/// Returns `None` for unknown files or languages whose feature is disabled.
pub fn language_for_path(path: &Path) -> Option<Lang> {
    let file = path.file_name()?.to_str()?;
    if let Some(l) = by_file_name(file) {
        return Some(l);
    }
    #[cfg(feature = "wasm")]
    if let Some(d) = crate::dynlang::by_file_name(file) {
        return Some(Lang::Dyn(d));
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let built_in = by_extension(&ext);
    #[cfg(feature = "wasm")]
    if built_in.is_none() {
        return crate::dynlang::by_extension(&ext).map(Lang::Dyn);
    }
    built_in
}

#[allow(unused_variables)]
fn by_file_name(file: &str) -> Option<Lang> {
    match file {
        #[cfg(feature = "toml")]
        "Cargo.lock" | "Pipfile" | "poetry.lock" | "uv.lock" | "Gopkg.lock" => Some(Lang::Toml),
        #[cfg(feature = "bash")]
        ".bashrc" | ".bash_profile" | ".bash_login" | ".bash_logout" | ".bash_aliases"
        | ".profile" | ".zshrc" | ".zshenv" | ".zprofile" | ".zlogin" | ".envrc" | "PKGBUILD"
        | "APKBUILD" => Some(Lang::Bash),
        #[cfg(feature = "json")]
        ".babelrc" | ".eslintrc" | ".prettierrc" | "tsconfig.json" | "composer.lock" | ".swcrc" => {
            Some(Lang::Json)
        }
        #[cfg(feature = "python")]
        "SConstruct" | "SConscript" => Some(Lang::Python),
        #[cfg(feature = "yaml")]
        ".clang-format" | ".clangd" => Some(Lang::Yaml),
        #[cfg(feature = "lua")]
        ".luacheckrc" => Some(Lang::Lua),
        #[cfg(feature = "make")]
        "Makefile" | "makefile" | "GNUmakefile" => Some(Lang::Make),
        #[cfg(feature = "ini")]
        ".editorconfig" | ".gitconfig" | ".npmrc" | ".gitmodules" => Some(Lang::Ini),
        #[cfg(feature = "ruby")]
        "Rakefile" | "Gemfile" | "Guardfile" | "Vagrantfile" => Some(Lang::Ruby),
        #[cfg(feature = "elixir")]
        "mix.lock" => Some(Lang::Elixir),
        _ => None,
    }
}

#[allow(unused_variables, unreachable_code)]
fn by_extension(ext: &str) -> Option<Lang> {
    Some(match ext {
        #[cfg(feature = "rust")]
        "rs" => Lang::Rust,
        #[cfg(feature = "typescript")]
        "ts" | "mts" | "cts" => Lang::TypeScript,
        #[cfg(feature = "typescript")]
        "tsx" => Lang::Tsx,
        #[cfg(feature = "javascript")]
        "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
        #[cfg(feature = "python")]
        "py" | "pyi" | "pyw" | "gyp" | "gypi" | "bzl" => Lang::Python,
        #[cfg(feature = "go")]
        "go" => Lang::Go,
        #[cfg(feature = "zig")]
        "zig" | "zon" => Lang::Zig,
        #[cfg(feature = "json")]
        "json" | "jsonc" | "geojson" | "webmanifest" | "ipynb" => Lang::Json,
        #[cfg(feature = "toml")]
        "toml" => Lang::Toml,
        #[cfg(feature = "bash")]
        "sh" | "bash" | "zsh" | "ksh" | "ebuild" | "bats" => Lang::Bash,
        #[cfg(feature = "markdown")]
        "md" | "markdown" | "mkd" => Lang::Markdown,
        #[cfg(feature = "yaml")]
        "yaml" | "yml" => Lang::Yaml,
        #[cfg(feature = "html")]
        "html" | "htm" | "xhtml" => Lang::Html,
        #[cfg(feature = "css")]
        "css" => Lang::Css,
        #[cfg(feature = "lua")]
        "lua" => Lang::Lua,
        #[cfg(feature = "make")]
        "mk" | "mak" | "make" => Lang::Make,
        #[cfg(feature = "ini")]
        "ini" | "cfg" | "editorconfig" => Lang::Ini,
        #[cfg(feature = "diff")]
        "diff" | "patch" => Lang::Diff,
        #[cfg(feature = "xml")]
        "xml" | "xsd" | "xsl" | "xslt" | "svg" | "plist" | "csproj" | "fsproj" | "vcxproj" => {
            Lang::Xml
        }
        #[cfg(feature = "c")]
        "c" | "h" => Lang::C,
        #[cfg(feature = "cpp")]
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ipp" => Lang::Cpp,
        #[cfg(feature = "java")]
        "java" => Lang::Java,
        #[cfg(feature = "csharp")]
        "cs" | "csx" => Lang::CSharp,
        #[cfg(feature = "ruby")]
        "rb" | "rake" | "gemspec" | "ru" => Lang::Ruby,
        #[cfg(feature = "php")]
        "php" | "phtml" => Lang::Php,
        #[cfg(feature = "sql")]
        "sql" => Lang::Sql,
        #[cfg(feature = "elixir")]
        "ex" | "exs" => Lang::Elixir,
        #[cfg(feature = "dart")]
        "dart" => Lang::Dart,
        #[cfg(feature = "haskell")]
        "hs" => Lang::Haskell,
        #[cfg(feature = "swift")]
        "swift" => Lang::Swift,
        #[cfg(feature = "scala")]
        "scala" | "sc" | "sbt" => Lang::Scala,
        #[cfg(feature = "nix")]
        "nix" => Lang::Nix,
        #[cfg(feature = "svelte")]
        "svelte" => Lang::Svelte,
        _ => return None,
    })
}
