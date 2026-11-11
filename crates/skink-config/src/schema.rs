//! Every settings.json key in one table. The JSON schema, the built-in
//! validation (diagnostics in the settings tab) and key completion are all
//! generated from it, and a test checks it against the parser so the three
//! never drift apart.

use serde_json::{Map, Value, json};

/// What a setting holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int {
        min: u64,
        max: u64,
    },
    Enum(&'static [&'static str]),
    Str,
    /// A list of column numbers.
    Columns,
    /// `"ai.agent"`: a name or a command list.
    Agent,
    /// `"lsp.<lang>"`: a command, a list, an object or `false`.
    Lsp,
    /// `"ssh.autoforward"`: true, false or "ask".
    BoolOrAsk,
    /// `"remotes"`: name -> {host, path, forwards, autoforward}.
    Remotes,
}

/// One key (or a family: `name` ending in `.` matches any suffix).
#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub name: &'static str,
    pub kind: Kind,
    pub about: &'static str,
}

const EFFORTS: &[&str] = &[
    "default", "auto", "low", "medium", "med", "high", "max", "maximum",
];

pub static KEYS: &[Key] = &[
    Key {
        name: "colorscheme",
        kind: Kind::Str,
        about: "theme name, or \"auto\" (Tokyo Night day or night after the terminal); :theme lists them",
    },
    Key {
        name: "keymap",
        kind: Kind::Enum(&["micro", "vscode", "sublime"]),
        about: "default keys (bindings.json goes on top); :bindings shows what overrides what",
    },
    Key {
        name: "tabsize",
        kind: Kind::Int { min: 1, max: 16 },
        about: "columns per tab and indent step",
    },
    Key {
        name: "icons",
        kind: Kind::Enum(&["auto", "nerd", "ascii"]),
        about: "file icons: Nerd Font glyphs or plain",
    },
    Key {
        name: "syntax",
        kind: Kind::Bool,
        about: "syntax colours",
    },
    Key {
        name: "indentguides",
        kind: Kind::Bool,
        about: "faint lines at each indent level",
    },
    Key {
        name: "autoclose",
        kind: Kind::Bool,
        about: "close brackets and quotes while typing",
    },
    Key {
        name: "tabstospaces",
        kind: Kind::Bool,
        about: "indent with spaces when a file's style is unknown",
    },
    Key {
        name: "formatonsave",
        kind: Kind::Bool,
        about: "ask the language server to format before saving",
    },
    Key {
        name: "autocomplete",
        kind: Kind::Bool,
        about: "open the completion list while typing",
    },
    Key {
        name: "inlayhints",
        kind: Kind::Bool,
        about: "inline type and parameter hints",
    },
    Key {
        name: "restoresession",
        kind: Kind::Bool,
        about: "reopen a project's last tabs",
    },
    Key {
        name: "restoreunsaved",
        kind: Kind::Bool,
        about: "bring back unsaved edits after a crash without asking",
    },
    Key {
        name: "rulers",
        kind: Kind::Columns,
        about: "vertical lines at these columns, e.g. [80, 100]",
    },
    Key {
        name: "colorcolumn",
        kind: Kind::Int { min: 1, max: 1000 },
        about: "one ruler (Micro's name)",
    },
    Key {
        name: "stickyscroll",
        kind: Kind::Bool,
        about: "keep enclosing block headers at the top",
    },
    Key {
        name: "menubar",
        kind: Kind::Bool,
        about: "a menu bar above the tabs (F10 opens the menus either way)",
    },
    Key {
        name: "gitgutter",
        kind: Kind::Bool,
        about: "git change marks next to the line numbers",
    },
    Key {
        name: "inlineblame",
        kind: Kind::Bool,
        about: "last commit of the cursor line, faded",
    },
    Key {
        name: "macoptionisalt",
        kind: Kind::Enum(&["auto", "always", "never"]),
        about: "read macOS Option characters as Alt chords",
    },
    Key {
        name: "updates",
        kind: Kind::Enum(&["notify", "auto", "off"]),
        about: "new releases: tell, install, or stay quiet",
    },
    Key {
        name: "terminal.shellintegration",
        kind: Kind::Bool,
        about: "prompt marks in terminal tabs (rc files untouched)",
    },
    Key {
        name: "ssh.autoforward",
        kind: Kind::BoolOrAsk,
        about: "skink ssh: forward ports opened on the remote (true), offer it (\"ask\"), or only listed ones (false)",
    },
    Key {
        name: "remotes",
        kind: Kind::Remotes,
        about: "saved skink ssh targets: {\"pi\": {\"host\": \"pi@10.0.0.5\", \"path\": \"~/app\", \"forwards\": [\"3000\"]}}",
    },
    Key {
        name: "ai.agent",
        kind: Kind::Agent,
        about: "the ACP agent: a name (claude, gemini, ...) or a command list",
    },
    Key {
        name: "ai.model",
        kind: Kind::Str,
        about: "model for AI requests (\"default\": the agent's)",
    },
    Key {
        name: "ai.effort",
        kind: Kind::Enum(EFFORTS),
        about: "reasoning effort",
    },
    Key {
        name: "ai.model.",
        kind: Kind::Str,
        about: "model for one prompt-box mode (rewrite, ask, search, implement)",
    },
    Key {
        name: "ai.effort.",
        kind: Kind::Enum(EFFORTS),
        about: "effort for one prompt-box mode",
    },
    Key {
        name: "lsp.",
        kind: Kind::Lsp,
        about: "a language server command, or false",
    },
];

/// The heading a setting is listed under in the settings screen.
pub fn group_of(key: &str) -> &'static str {
    match key {
        "colorscheme" | "icons" | "indentguides" | "rulers" | "colorcolumn" | "stickyscroll"
        | "inlayhints" | "menubar" => "Appearance",
        "tabsize" | "tabstospaces" | "autoclose" | "autocomplete" | "formatonsave" | "syntax" => {
            "Editing"
        }
        "keymap" | "macoptionisalt" => "Keys",
        "gitgutter" | "inlineblame" => "Git",
        "terminal.shellintegration" | "ssh.autoforward" | "remotes" => "Terminal and remote",
        "restoresession" | "updates" => "Sessions and updates",
        k if k.starts_with("ai.") => "AI",
        k if k.starts_with("lsp.") => "Language servers",
        _ => "Other",
    }
}

/// The groups in the order the settings screen shows them.
pub const GROUPS: &[&str] = &[
    "Appearance",
    "Editing",
    "Keys",
    "Git",
    "Terminal and remote",
    "AI",
    "Language servers",
    "Sessions and updates",
    "Other",
];

/// What a key means when no settings file sets it: the value in
/// [`crate::DEFAULT_SETTINGS`], else the built-in behaviour.
pub fn default_value(key: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(&crate::relaxed_json(crate::DEFAULT_SETTINGS)).ok()?;
    if let Some(d) = v.get(key) {
        return Some(d.clone());
    }
    match key {
        "ssh.autoforward" => Some(json!(true)),
        "remotes" => Some(json!({})),
        _ => None,
    }
}

/// The table entry for `key`.
pub fn lookup(key: &str) -> Option<&'static Key> {
    KEYS.iter().find(|k| match k.name.strip_suffix('.') {
        Some(prefix) => key.len() > k.name.len() && key.starts_with(k.name) && !prefix.is_empty(),
        None => k.name == key,
    })
}

fn kind_schema(kind: Kind) -> Value {
    let lsp_obj = json!({
        "type": "object",
        "properties": {
            "command": {"type": ["string", "array"]},
            "args": {"type": "array", "items": {"type": "string"}},
            "extensions": {"type": "array", "items": {"type": "string"}},
            "rootmarkers": {"type": "array", "items": {"type": "string"}},
            "initoptions": {},
            "env": {"type": "object", "additionalProperties": {"type": "string"}}
        },
        "required": ["command"],
        "additionalProperties": false
    });
    match kind {
        Kind::Bool => json!({"type": "boolean"}),
        Kind::Int { min, max } => json!({"type": "integer", "minimum": min, "maximum": max}),
        Kind::Enum(v) => json!({"type": "string", "enum": v}),
        Kind::Str => json!({"type": "string"}),
        Kind::Columns => {
            json!({"type": "array", "items": {"type": "integer", "minimum": 1, "maximum": 1000}})
        }
        Kind::Agent => json!({"type": ["string", "array"], "items": {"type": "string"}}),
        Kind::Lsp => json!({"oneOf": [
            {"type": "string"},
            {"type": "array", "items": {"type": "string"}},
            {"const": false},
            {"const": true},
            lsp_obj
        ]}),
        Kind::BoolOrAsk => json!({"oneOf": [{"type": "boolean"}, {"const": "ask"}]}),
        Kind::Remotes => json!({
            "type": "object",
            "additionalProperties": {
                "type": "object",
                "properties": {
                    "host": {"type": "string"},
                    "path": {"type": "string"},
                    "forwards": {"type": "array", "items": {"type": ["string", "integer"]}},
                    "autoforward": {"oneOf": [{"type": "boolean"}, {"const": "ask"}]}
                },
                "required": ["host"],
                "additionalProperties": false
            }
        }),
    }
}

/// JSON Schema (draft 7) for settings.json, generated from [`KEYS`].
pub fn settings_schema() -> Value {
    let mut props = Map::new();
    let mut patterns = Map::new();
    for k in KEYS {
        let mut s = kind_schema(k.kind);
        s["description"] = json!(k.about);
        match k.name.strip_suffix('.') {
            Some(p) => {
                patterns.insert(format!("^{}\\..+$", p.replace('.', "\\.")), s);
            }
            None => {
                props.insert(k.name.to_owned(), s);
            }
        }
    }
    let section = json!({
        "type": "object",
        "description": "settings for one language, e.g. [rust]",
        "properties": Value::Object(props.clone()),
        "patternProperties": Value::Object(patterns.clone()),
    });
    patterns.insert("^\\[.+\\]$".into(), section);
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Skink settings.json",
        "type": "object",
        "properties": Value::Object(props),
        "patternProperties": Value::Object(patterns),
        "additionalProperties": false
    })
}

/// JSON Schema for bindings.json: key name -> action chain.
pub fn bindings_schema() -> Value {
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Skink bindings.json",
        "type": "object",
        "additionalProperties": {
            "type": "string",
            "description": "an action, A|B (first that applies), A,B (both) or command-edit:TEXT"
        }
    })
}

/// One problem in a config file (0-based line and column, in chars).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub line: usize,
    pub col: usize,
    pub message: String,
    /// `false`: a warning (unknown key), `true`: an error (bad value).
    pub error: bool,
}

/// Line and column (chars) of byte offset `at` in `text`.
fn line_col(text: &str, at: usize) -> (usize, usize) {
    let before = &text[..at.min(text.len())];
    let line = before.matches('\n').count();
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count());
    (line, col)
}

/// Where `"key"` is written as a key, searching from byte `from`.
fn key_at(text: &str, key: &str, from: usize) -> usize {
    let quoted = format!("\"{key}\"");
    let mut start = from.min(text.len());
    while let Some(i) = text[start..].find(&quoted) {
        let at = start + i;
        let rest = text[at + quoted.len()..].trim_start();
        if rest.starts_with(':') {
            return at;
        }
        start = at + quoted.len();
    }
    from
}

/// The known key closest to `key` (for "did you mean").
fn closest(key: &str) -> Option<&'static str> {
    fn dist(a: &str, b: &str) -> usize {
        let b: Vec<char> = b.chars().collect();
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.chars().enumerate() {
            let mut prev = row[0];
            row[0] = i + 1;
            for (j, cb) in b.iter().enumerate() {
                let cur = row[j + 1];
                row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
                prev = cur;
            }
        }
        row[b.len()]
    }
    KEYS.iter()
        .filter(|k| !k.name.ends_with('.'))
        .map(|k| (dist(key, k.name), k.name))
        .filter(|(d, _)| *d <= 3)
        .min()
        .map(|(_, n)| n)
}

fn type_ok(kind: Kind, v: &Value) -> bool {
    match kind {
        Kind::Bool => v.is_boolean(),
        Kind::Int { .. } => v.is_u64(),
        Kind::Enum(_) | Kind::Str => v.is_string(),
        Kind::Columns => v.is_array(),
        Kind::Agent => v.is_string() || v.is_array(),
        Kind::Lsp => v.is_string() || v.is_array() || v.is_boolean() || v.is_object(),
        Kind::BoolOrAsk => v.is_boolean() || v.as_str() == Some("ask"),
        Kind::Remotes => v.is_object(),
    }
}

fn expected(kind: Kind) -> String {
    match kind {
        Kind::Bool => "true or false".into(),
        Kind::Int { min, max } => format!("a number {min}-{max}"),
        Kind::Enum(v) => v
            .iter()
            .map(|x| format!("\"{x}\""))
            .collect::<Vec<_>>()
            .join(", "),
        Kind::Str => "a string".into(),
        Kind::Columns => "a list of columns, e.g. [80, 100]".into(),
        Kind::Agent => "a name or a list of strings".into(),
        Kind::Lsp => "a command, a list, an object or false".into(),
        Kind::BoolOrAsk => "true, false or \"ask\"".into(),
        Kind::Remotes => "an object of remotes".into(),
    }
}

/// Check settings.json text: syntax, unknown keys (with a suggestion),
/// wrong types and bad values, each at the key's position.
pub fn check_settings(text: &str) -> Vec<Issue> {
    let relaxed = crate::settings::relaxed_json(text);
    let v: Value = match serde_json::from_str(&relaxed) {
        Ok(v) => v,
        Err(e) => {
            return vec![Issue {
                line: e.line().saturating_sub(1),
                col: e.column().saturating_sub(1),
                message: format!("not valid JSON: {e}"),
                error: true,
            }];
        }
    };
    let Value::Object(obj) = v else {
        return vec![Issue {
            line: 0,
            col: 0,
            message: "settings.json must be an object: { ... }".into(),
            error: true,
        }];
    };
    let mut out = Vec::new();
    for (k, v) in &obj {
        let at = key_at(text, k, 0);
        if k.len() > 2 && k.starts_with('[') && k.ends_with(']') {
            match v {
                Value::Object(inner) => {
                    for (ik, iv) in inner {
                        let key = if ik == "lsp" {
                            format!("lsp.{}", &k[1..k.len() - 1])
                        } else {
                            ik.clone()
                        };
                        check_one(text, &key, ik, iv, key_at(text, ik, at), &mut out);
                    }
                }
                _ => out.push(issue(
                    text,
                    at,
                    format!("{k} must be an object of settings"),
                    true,
                )),
            }
            continue;
        }
        check_one(text, k, k, v, at, &mut out);
    }
    out.sort_by_key(|i| (i.line, i.col));
    out
}

fn issue(text: &str, at: usize, message: String, error: bool) -> Issue {
    let (line, col) = line_col(text, at);
    Issue {
        line,
        col,
        message,
        error,
    }
}

/// `key` as the parser sees it (`lsp.rust` for `lsp` in `[rust]`), `shown`
/// as written.
fn check_one(text: &str, key: &str, shown: &str, v: &Value, at: usize, out: &mut Vec<Issue>) {
    if key == "$schema" {
        return;
    }
    let Some(entry) = lookup(key) else {
        let hint = closest(key).map_or_else(String::new, |c| format!(" (did you mean \"{c}\"?)"));
        out.push(issue(
            text,
            at,
            format!("unknown setting \"{shown}\"{hint}"),
            false,
        ));
        return;
    };
    if !type_ok(entry.kind, v) {
        out.push(issue(
            text,
            at,
            format!("{shown} must be {}", expected(entry.kind)),
            true,
        ));
        return;
    }
    // Ranges and shapes: ask the parser itself.
    let mut one = Map::new();
    one.insert(key.to_owned(), v.clone());
    let text_one = Value::Object(one).to_string();
    let (_, errs) = crate::Settings::parse(&text_one);
    for e in errs {
        out.push(issue(text, at, e, true));
    }
}

/// Settings keys starting with `prefix`, with their descriptions (for
/// completion inside settings.json).
pub fn complete_keys(prefix: &str) -> Vec<(&'static str, &'static str)> {
    KEYS.iter()
        .filter(|k| k.name.starts_with(prefix) && !k.name.ends_with('.'))
        .map(|k| (k.name, k.about))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table and the parser agree: every key's valid values change the
    /// parsed settings (so the parser knows it), and the defaults file only
    /// uses known keys.
    #[test]
    fn the_table_matches_the_parser() {
        let def = crate::Settings::default();
        for k in KEYS {
            let key = match k.name.strip_suffix('.') {
                Some("lsp") => "lsp.rust".to_owned(),
                Some(p) => format!("{p}.rewrite"),
                None => k.name.to_owned(),
            };
            let samples: Vec<Value> = match k.kind {
                Kind::Bool => vec![json!(true), json!(false)],
                Kind::Int { min, max } => vec![json!(min), json!(max)],
                Kind::Enum(v) => v.iter().map(|x| json!(x)).collect(),
                Kind::Str => vec![json!("x-model")],
                Kind::Columns => vec![json!([80])],
                Kind::Agent => vec![json!("gemini")],
                Kind::Lsp => vec![json!("my-ls")],
                Kind::BoolOrAsk => vec![json!(false), json!("ask")],
                Kind::Remotes => vec![json!({"pi": {"host": "pi@h", "forwards": ["3000"]}})],
            };
            let changes = samples.iter().any(|v| {
                let (mut s, errs) = crate::Settings::parse(&json!({ key.clone(): v }).to_string());
                s.base.clear();
                errs.is_empty() && s != def
            });
            assert!(changes, "the parser ignores {key}");
        }
        assert!(
            check_settings(crate::DEFAULT_SETTINGS).is_empty(),
            "{:?}",
            check_settings(crate::DEFAULT_SETTINGS)
        );
    }

    #[test]
    fn issues_point_at_the_key_with_a_reason() {
        let text = "{\n  \"tabsize\": 99,\n  \"sytax\": true,\n  \"gitgutter\": \"yes\",\n  \"[rust]\": {\n    \"tabsize\": 0\n  }\n}\n";
        let got: Vec<(usize, String)> = check_settings(text)
            .into_iter()
            .map(|i| (i.line, i.message))
            .collect();
        assert_eq!(
            got,
            [
                (1, "tabsize must be 1-16".to_owned()),
                (
                    2,
                    "unknown setting \"sytax\" (did you mean \"syntax\"?)".to_owned()
                ),
                (3, "gitgutter must be true or false".to_owned()),
                (5, "tabsize must be 1-16".to_owned()),
            ]
        );
        // A missing comma is reported on the line of the next key.
        assert_eq!(
            check_settings("{\n  \"a\": 1\n  /* x\n  */ \"b\": 2\n}")[0].line,
            3
        );
        assert!(settings_schema()["properties"]["tabsize"]["maximum"] == json!(16));
    }
}
