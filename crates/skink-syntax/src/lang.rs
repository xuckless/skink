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
        ]
    }

    /// Stable lower-case identifier: `"rust"`, `"typescript"`, `"tsx"`,
    /// `"javascript"`, `"python"`, `"go"`, `"zig"`, `"json"`, `"toml"`, `"bash"`.
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
        }
    }

    /// Inverse of [`Lang::name`] (also accepts a few common aliases such as
    /// `"rs"`, `"ts"`, `"js"`, `"py"`, `"sh"`).
    pub fn from_name(name: &str) -> Option<Lang> {
        let n = name.to_ascii_lowercase();
        Lang::all().iter().copied().find(|l| {
            l.name() == n
                || matches!(
                    (l.name(), n.as_str()),
                    ("rust", "rs")
                        | ("typescript", "ts")
                        | ("javascript", "js" | "jsx")
                        | ("python", "py")
                        | ("bash", "sh" | "shell" | "zsh")
                )
        })
    }

    pub(crate) fn grammar(self) -> Language {
        match self {
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
        }
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
}

/// Detects the language of a file from its name or extension (case-insensitive).
///
/// Returns `None` for unknown files or languages whose feature is disabled.
pub fn language_for_path(path: &Path) -> Option<Lang> {
    let file = path.file_name()?.to_str()?;
    if let Some(l) = by_file_name(file) {
        return Some(l);
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    by_extension(&ext)
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
        _ => return None,
    })
}
