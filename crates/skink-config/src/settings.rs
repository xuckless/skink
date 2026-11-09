//! User files: `settings.json`, `bindings.json` (Micro's format: key name to
//! action) and the recent-files list. Everything here is plain files so that
//! "open the settings" is the same as opening any other file.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Directory for user config (`$SKINK_CONFIG_DIR`, else the platform default).
pub fn config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("SKINK_CONFIG_DIR") {
        return Some(d.into());
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("skink"));
    }
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(x).join("skink"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/skink"))
}

/// Directory for state that is not config (recent files).
pub fn state_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("SKINK_STATE_DIR") {
        return Some(d.into());
    }
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("skink"));
    }
    if let Some(x) = std::env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(x).join("skink"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/skink"))
}

pub const DEFAULT_SETTINGS: &str = r#"{
    "tabsize": 4,
    "icons": "auto",
    "syntax": true,
    "indentguides": true,
    "autoclose": true,
    "tabstospaces": false,
    "formatonsave": false,
    "autocomplete": true,
    "inlayhints": false,
    // `skink` with no arguments inside a project reopens its last tabs,
    // splits, file tree and shells.
    "restoresession": true,
    // Vertical lines at these columns, e.g. [80, 100].
    "rulers": [],
    // Keep the headers of the blocks you are scrolled into at the top.
    "stickyscroll": true,
    // Added / modified / deleted marks next to the line numbers (git).
    "gitgutter": true,
    // Author, date and summary of the last commit at the end of the
    // cursor line (Alt-Shift-B toggles it for the session).
    "inlineblame": true,
    // Prompt marks in terminal tabs (bash, zsh, fish, PowerShell): prompt
    // jumps, select last output, exact "send errors". Your rc files are
    // loaded unchanged and never edited.
    "terminal.shellintegration": true,
    "macoptionisalt": "auto",
    "updates": "notify",
    "ai.agent": "auto",
    "ai.model": "default",
    "ai.effort": "default"
    // Language servers: replace a built-in, add a language, or turn one off.
    //   "lsp.dart": "fvm dart language-server --protocol=lsp",
    //   "lsp.elixir": {"command": "elixir-ls", "extensions": ["ex", "exs"]},
    //   "lsp.python": false
}
"#;

pub const DEFAULT_BINDINGS_FILE: &str = "{\n}\n";

/// macOS terminals send Option+key as a composed character ("Ω" for
/// Option+Z) unless configured otherwise (`"macoptionisalt"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOption {
    /// On macOS, read an Option character as Alt+key when that chord is bound.
    Auto,
    /// Always read Option characters as Alt chords (any platform).
    Always,
    /// Type the characters as they are.
    Never,
}

/// What to do when a newer release exists (`"updates"` in settings.json).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePolicy {
    /// Never check.
    Off,
    /// Check at most once a day and say so in the status line.
    Notify,
    /// Check, download and install; the new version runs on the next start.
    Auto,
}

/// Which coding agent the AI prompt box talks to (`"ai.agent"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentChoice {
    /// The first known agent found on PATH.
    Auto,
    /// A known agent by name: "claude", "gemini", "opencode", "goose",
    /// "codex", "antigravity".
    Named(String),
    /// A command line that speaks ACP on stdio (`["my-agent", "--acp"]`).
    Command(Vec<String>),
}

/// Which model to ask for (`"ai.model"`, `"ai.model.<mode>"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChoice {
    /// Whatever the agent uses by default.
    AgentDefault,
    /// A model id (or name) the agent reports for the session.
    Named(String),
}

/// How hard the model should think (`"ai.effort"`, `"ai.effort.<mode>"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortChoice {
    /// Whatever the agent does by default.
    AgentDefault,
    Low,
    Medium,
    High,
    Max,
}

impl EffortChoice {
    pub fn name(self) -> &'static str {
        match self {
            Self::AgentDefault => "default",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Some(match v.trim().to_ascii_lowercase().as_str() {
            "" | "default" | "auto" => Self::AgentDefault,
            "low" => Self::Low,
            "medium" | "med" => Self::Medium,
            "high" => Self::High,
            "max" | "maximum" => Self::Max,
            _ => return None,
        })
    }
}

/// The prompt box's modes, for per-mode model overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiMode {
    Rewrite,
    Ask,
    Search,
    Implement,
}

impl AiMode {
    pub const ALL: [AiMode; 4] = [Self::Rewrite, Self::Ask, Self::Search, Self::Implement];

    pub fn name(self) -> &'static str {
        match self {
            Self::Rewrite => "rewrite",
            Self::Ask => "ask",
            Self::Search => "search",
            Self::Implement => "implement",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiSettings {
    pub agent: AgentChoice,
    pub model: ModelChoice,
    /// Overrides of `model` per mode (a fast model for rewrite, ...).
    pub mode_models: Vec<(AiMode, ModelChoice)>,
    pub effort: EffortChoice,
    /// Overrides of `effort` per mode.
    pub mode_efforts: Vec<(AiMode, EffortChoice)>,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            agent: AgentChoice::Auto,
            model: ModelChoice::AgentDefault,
            mode_models: Vec::new(),
            effort: EffortChoice::AgentDefault,
            mode_efforts: Vec::new(),
        }
    }
}

impl AiSettings {
    /// The model to use for `mode`.
    pub fn model_for(&self, mode: AiMode) -> &ModelChoice {
        self.mode_models
            .iter()
            .find(|(m, _)| *m == mode)
            .map_or(&self.model, |(_, c)| c)
    }

    /// The effort to use for `mode`.
    pub fn effort_for(&self, mode: AiMode) -> EffortChoice {
        self.mode_efforts
            .iter()
            .find(|(m, _)| *m == mode)
            .map_or(self.effort, |(_, e)| *e)
    }
}

const EFFORT_ERR: &str = "ai.effort must be \"default\", \"low\", \"medium\", \"high\" or \"max\"";

fn model_choice(v: &str) -> ModelChoice {
    match v.trim() {
        "" | "default" | "auto" => ModelChoice::AgentDefault,
        x => ModelChoice::Named(x.to_owned()),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub tabsize: usize,
    /// "auto", "nerd" or "ascii".
    pub icons: String,
    pub syntax: bool,
    pub indent_guides: bool,
    /// Close brackets and quotes while typing.
    pub autoclose: bool,
    /// Indent with spaces in files whose style cannot be detected.
    pub tabs_to_spaces: bool,
    /// Ask the language server to format before every save.
    pub format_on_save: bool,
    /// Open the completion list while typing (Ctrl-Space always works).
    pub autocomplete: bool,
    /// Show the language server's inline type / parameter hints.
    pub inlay_hints: bool,
    /// `skink` with no arguments in a project reopens the last session.
    pub restore_session: bool,
    /// Columns (1-based) marked with a ruler.
    pub rulers: Vec<usize>,
    pub sticky_scroll: bool,
    /// Git change marks in the gutter and the scrollbar.
    pub git_gutter: bool,
    /// Blame of the cursor line as virtual text.
    pub inline_blame: bool,
    pub mac_option: MacOption,
    pub updates: UpdatePolicy,
    pub ai: AiSettings,
    /// Prompt marks (OSC 133) in the shells of terminal tabs.
    pub terminal_shell_integration: bool,
    /// The merged top-level keys (user then project), without sections.
    pub base: serde_json::Map<String, Value>,
    /// `"[rust]": {...}` sections: language name (lower case) and its keys.
    pub sections: Vec<(String, serde_json::Map<String, Value>)>,
    /// `"lsp.<lang>"` entries, sorted by name (later aliases of one
    /// language win: `lsp.typescript` over `lsp.javascript`).
    pub lsp: Vec<(String, LspSetting)>,
}

/// One `"lsp.<lang>"` setting: a server command (string, list, or object
/// with `command`, `args`, `extensions`, `rootmarkers`, `initoptions`,
/// `env`), or `false` for no server.
#[derive(Debug, Clone, PartialEq)]
pub enum LspSetting {
    Off,
    Server {
        argv: Vec<String>,
        extensions: Vec<String>,
        root_markers: Vec<String>,
        init_options: Option<Value>,
        env: Vec<(String, String)>,
    },
}

const LSP_ERR: &str = "must be a command (\"prog args\" or [\"prog\", \"arg\"]), false, or {\"command\": ..., \"args\": [...], \"extensions\": [...], \"rootmarkers\": [...], \"initoptions\": {...}, \"env\": {...}}";

fn strings(v: &Value) -> Option<Vec<String>> {
    v.as_array()?
        .iter()
        .map(|x| x.as_str().map(str::to_owned))
        .collect()
}

fn lsp_setting(v: &Value) -> Option<LspSetting> {
    let server = |argv: Vec<String>| LspSetting::Server {
        argv,
        extensions: Vec::new(),
        root_markers: Vec::new(),
        init_options: None,
        env: Vec::new(),
    };
    Some(match v {
        Value::Bool(false) => LspSetting::Off,
        Value::String(c) if !c.trim().is_empty() => {
            server(c.split_whitespace().map(str::to_owned).collect())
        }
        Value::Array(_) => server(strings(v).filter(|a| !a.is_empty())?),
        Value::Object(o) => {
            let mut argv = match o.get("command")? {
                Value::String(c) => c.split_whitespace().map(str::to_owned).collect(),
                c @ Value::Array(_) => strings(c)?,
                _ => return None,
            };
            if let Some(a) = o.get("args") {
                argv.extend(strings(a)?);
            }
            if argv.is_empty() {
                return None;
            }
            let list = |k: &str| o.get(k).map_or(Some(Vec::new()), strings);
            let env = match o.get("env") {
                None => Vec::new(),
                Some(Value::Object(e)) => e
                    .iter()
                    .map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                    .collect::<Option<_>>()?,
                Some(_) => return None,
            };
            for k in o.keys() {
                if ![
                    "command",
                    "args",
                    "extensions",
                    "rootmarkers",
                    "initoptions",
                    "env",
                ]
                .contains(&k.as_str())
                {
                    return None;
                }
            }
            LspSetting::Server {
                argv,
                extensions: list("extensions")?,
                root_markers: list("rootmarkers")?,
                init_options: o.get("initoptions").cloned(),
                env,
            }
        }
        _ => return None,
    })
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tabsize: 4,
            icons: "auto".into(),
            syntax: true,
            indent_guides: true,
            autoclose: true,
            tabs_to_spaces: false,
            format_on_save: false,
            autocomplete: true,
            inlay_hints: false,
            restore_session: true,
            rulers: Vec::new(),
            sticky_scroll: true,
            git_gutter: true,
            inline_blame: true,
            mac_option: MacOption::Auto,
            updates: UpdatePolicy::Notify,
            ai: AiSettings::default(),
            lsp: Vec::new(),
            terminal_shell_integration: true,
            base: serde_json::Map::new(),
            sections: Vec::new(),
        }
    }
}

/// Hand-edited config is JSON with the usual slips allowed: `//` and `/* */`
/// comments and a trailing comma before `}` or `]`. Strings are left intact.
pub fn relaxed_json(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                // Copy the string literal, escapes included.
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(b.len());
                out.push_str(&text[start..i]);
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    // Keep line numbers right for error positions.
                    if b[i] == b'\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b',' => {
                if !matches!(next_significant(&text[i + 1..]), Some('}' | ']')) {
                    out.push(',');
                }
                i += 1;
            }
            _ => {
                let ch = text[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// The first character of `s` that is not whitespace or inside a comment.
fn next_significant(mut s: &str) -> Option<char> {
    loop {
        s = s.trim_start();
        if let Some(r) = s.strip_prefix("//") {
            s = r.split_once('\n').map_or("", |(_, r)| r);
        } else if let Some(r) = s.strip_prefix("/*") {
            s = r.split_once("*/").map_or("", |(_, r)| r);
        } else {
            return s.chars().next();
        }
    }
}

/// `"[rust]"`-style keys.
fn is_section(k: &str) -> bool {
    k.len() > 2 && k.starts_with('[') && k.ends_with(']')
}

/// A section's keys as top-level keys: `"lsp"` inside `[rust]` means
/// `"lsp.rust"`.
fn section_keys(name: &str, o: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    o.iter()
        .map(|(k, v)| match k.as_str() {
            "lsp" => (format!("lsp.{name}"), v.clone()),
            _ => (k.clone(), v.clone()),
        })
        .collect()
}

impl Settings {
    /// Parse `settings.json`; unknown keys are ignored, bad values fall back
    /// to the defaults and are reported in the second return value.
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        Self::parse_layers(&[("settings.json", text)])
    }

    /// Settings from several files, later ones winning key by key (user
    /// settings, then the project's `.skink/settings.json`). `[lang]`
    /// sections merge key by key too.
    pub fn parse_layers(layers: &[(&str, &str)]) -> (Self, Vec<String>) {
        let mut errs = Vec::new();
        let mut merged = serde_json::Map::new();
        for (label, text) in layers {
            let v: Value = match serde_json::from_str(&relaxed_json(text)) {
                Ok(v) => v,
                Err(e) => {
                    errs.push(format!("{label}: {e}"));
                    continue;
                }
            };
            let Value::Object(obj) = v else {
                errs.push(format!("{label}: expected an object"));
                continue;
            };
            for (k, v) in obj {
                match (merged.get_mut(&k), v) {
                    (Some(Value::Object(old)), Value::Object(new)) if is_section(&k) => {
                        old.extend(new);
                    }
                    (_, v) => {
                        merged.insert(k, v);
                    }
                }
            }
        }
        let mut sections = Vec::new();
        let mut base = serde_json::Map::new();
        for (k, v) in merged {
            match (is_section(&k), v) {
                (true, Value::Object(o)) => {
                    let name = k[1..k.len() - 1].trim().to_ascii_lowercase();
                    // Report a section's bad values once, here.
                    let (_, e) = Self::from_map(&section_keys(&name, &o));
                    errs.extend(e.into_iter().map(|e| format!("{k} {e}")));
                    sections.push((name, o));
                }
                (true, _) => errs.push(format!("{k} must be an object of settings")),
                (false, v) => {
                    base.insert(k, v);
                }
            }
        }
        // `"lsp"` in a section is a server choice for the whole app.
        let mut top = base.clone();
        for (name, o) in &sections {
            if let Some(v) = o.get("lsp") {
                top.insert(format!("lsp.{name}"), v.clone());
            }
        }
        let (mut s, e) = Self::from_map(&top);
        errs.extend(
            e.into_iter().filter(|e| {
                !e.starts_with("lsp.") || base.keys().any(|k| e.starts_with(k.as_str()))
            }),
        );
        s.base = base;
        s.sections = sections;
        (s, errs)
    }

    /// The settings for files of `lang` (any of its names: "rust", "rs"):
    /// the base keys with that language's section on top. `None` when no
    /// section names it.
    pub fn for_language(&self, names: &[&str]) -> Option<Self> {
        let (name, keys) = self
            .sections
            .iter()
            .find(|(n, _)| names.iter().any(|x| x.eq_ignore_ascii_case(n)))?;
        let mut map = self.base.clone();
        map.extend(section_keys(name, keys));
        let (mut s, _) = Self::from_map(&map);
        s.base = self.base.clone();
        s.sections = self.sections.clone();
        Some(s)
    }

    fn from_map(obj: &serde_json::Map<String, Value>) -> (Self, Vec<String>) {
        let mut s = Self::default();
        let mut errs = Vec::new();
        for (k, v) in obj {
            match (k.as_str(), v) {
                ("tabsize", Value::Number(n)) => match n.as_u64() {
                    Some(n) if (1..=16).contains(&n) => s.tabsize = n as usize,
                    _ => errs.push("tabsize must be 1-16".into()),
                },
                ("icons", Value::String(x)) if ["auto", "nerd", "ascii"].contains(&x.as_str()) => {
                    s.icons = x.clone()
                }
                ("icons", _) => errs.push("icons must be \"auto\", \"nerd\" or \"ascii\"".into()),
                ("syntax", Value::Bool(b)) => s.syntax = *b,
                ("indentguides", Value::Bool(b)) => s.indent_guides = *b,
                ("autoclose", Value::Bool(b)) => s.autoclose = *b,
                ("tabstospaces", Value::Bool(b)) => s.tabs_to_spaces = *b,
                ("formatonsave", Value::Bool(b)) => s.format_on_save = *b,
                ("autocomplete", Value::Bool(b)) => s.autocomplete = *b,
                ("inlayhints", Value::Bool(b)) => s.inlay_hints = *b,
                ("restoresession", Value::Bool(b)) => s.restore_session = *b,
                ("stickyscroll", Value::Bool(b)) => s.sticky_scroll = *b,
                ("gitgutter", Value::Bool(b)) => s.git_gutter = *b,
                ("inlineblame", Value::Bool(b)) => s.inline_blame = *b,
                ("terminal.shellintegration", Value::Bool(b)) => s.terminal_shell_integration = *b,
                ("rulers", Value::Array(a)) => {
                    s.rulers = a
                        .iter()
                        .filter_map(Value::as_u64)
                        .filter(|&n| (1..=1000).contains(&n))
                        .map(|n| n as usize)
                        .collect();
                    if s.rulers.len() != a.len() {
                        errs.push("rulers must be columns 1-1000, e.g. [80, 100]".into());
                    }
                }
                // Micro's single ruler.
                ("colorcolumn", Value::Number(n)) => {
                    if let Some(n) = n.as_u64().filter(|&n| n > 0) {
                        s.rulers = vec![n as usize];
                    }
                }
                ("macoptionisalt", Value::String(x)) => match x.as_str() {
                    "auto" => s.mac_option = MacOption::Auto,
                    "always" => s.mac_option = MacOption::Always,
                    "never" => s.mac_option = MacOption::Never,
                    _ => {
                        errs.push("macoptionisalt must be \"auto\", \"always\" or \"never\"".into())
                    }
                },
                ("updates", Value::String(x)) => match x.as_str() {
                    "off" => s.updates = UpdatePolicy::Off,
                    "notify" => s.updates = UpdatePolicy::Notify,
                    "auto" => s.updates = UpdatePolicy::Auto,
                    _ => errs.push("updates must be \"notify\", \"auto\" or \"off\"".into()),
                },
                ("ai.agent", Value::String(x)) => {
                    s.ai.agent = match x.trim() {
                        "" | "auto" => AgentChoice::Auto,
                        x if x.contains(char::is_whitespace) => {
                            AgentChoice::Command(x.split_whitespace().map(str::to_owned).collect())
                        }
                        x => AgentChoice::Named(x.to_owned()),
                    }
                }
                ("ai.agent", Value::Array(a)) => {
                    let argv: Vec<String> = a
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect();
                    if argv.is_empty() || argv.len() != a.len() {
                        errs.push("ai.agent must be a name or a list of strings".into());
                    } else {
                        s.ai.agent = AgentChoice::Command(argv);
                    }
                }
                ("ai.agent", _) => errs.push("ai.agent must be a name or a list of strings".into()),
                ("ai.model", Value::String(x)) => s.ai.model = model_choice(x),
                ("ai.effort", Value::String(x)) => match EffortChoice::parse(x) {
                    Some(e) => s.ai.effort = e,
                    None => errs.push(EFFORT_ERR.into()),
                },
                (k, Value::String(x)) if k.starts_with("ai.effort.") => {
                    let mode = &k["ai.effort.".len()..];
                    match (
                        AiMode::ALL.iter().find(|m| m.name() == mode),
                        EffortChoice::parse(x),
                    ) {
                        (Some(m), Some(e)) => s.ai.mode_efforts.push((*m, e)),
                        (None, _) => errs.push(format!(
                            "{k}: unknown mode (rewrite, ask, search or implement)"
                        )),
                        (_, None) => errs.push(format!("{k}: {EFFORT_ERR}")),
                    }
                }
                // `true` just means "the default server".
                (k, Value::Bool(true)) if k.starts_with("lsp.") => {}
                (k, v) if k.starts_with("lsp.") && k.len() > 4 => match lsp_setting(v) {
                    Some(l) => s.lsp.push((k[4..].to_owned(), l)),
                    None => errs.push(format!("{k} {LSP_ERR}")),
                },
                (k, Value::String(x)) if k.starts_with("ai.model.") => {
                    let mode = &k["ai.model.".len()..];
                    match AiMode::ALL.iter().find(|m| m.name() == mode) {
                        Some(m) => s.ai.mode_models.push((*m, model_choice(x))),
                        None => errs.push(format!(
                            "{k}: unknown mode (rewrite, ask, search or implement)"
                        )),
                    }
                }
                _ => {}
            }
        }
        (s, errs)
    }

    pub fn load(dir: &Path) -> (Self, Vec<String>) {
        Self::load_with_project(dir, None)
    }

    /// User settings from `dir`, then `<project>/.skink/settings.json`.
    pub fn load_with_project(dir: &Path, project: Option<&Path>) -> (Self, Vec<String>) {
        let user = std::fs::read_to_string(dir.join("settings.json")).ok();
        let proj = project.and_then(|p| std::fs::read_to_string(project_settings_path(p)).ok());
        let mut layers: Vec<(&str, &str)> = Vec::new();
        if let Some(t) = &user {
            layers.push(("settings.json", t));
        }
        if let Some(t) = &proj {
            layers.push((".skink/settings.json", t));
        }
        Self::parse_layers(&layers)
    }
}

/// A project's own settings file.
pub fn project_settings_path(root: &Path) -> std::path::PathBuf {
    root.join(".skink").join("settings.json")
}

/// Parse a Micro-style `bindings.json` object into (key, action) pairs.
pub fn parse_bindings(text: &str) -> Result<Vec<(String, String)>, String> {
    let v: Value =
        serde_json::from_str(&relaxed_json(text)).map_err(|e| format!("bindings.json: {e}"))?;
    let obj = v.as_object().ok_or("bindings.json: expected an object")?;
    Ok(obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|a| (k.clone(), a.to_owned())))
        .collect())
}

const RECENT_MAX: usize = 50;

pub fn recent_load(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(dir.join("recent"))
        .map(|t| {
            t.lines()
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Move `path` to the front of the recent list and write it back.
pub fn recent_push(dir: &Path, path: &Path) -> Vec<PathBuf> {
    let mut list = recent_load(dir);
    list.retain(|p| p != path);
    list.insert(0, path.to_path_buf());
    list.truncate(RECENT_MAX);
    let text: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    if std::fs::create_dir_all(dir).is_ok() {
        let _ = std::fs::write(dir.join("recent"), text);
    }
    list
}

#[cfg(test)]
mod tests {
    #[test]
    fn effort_and_per_mode_overrides() {
        let (s, errs) = Settings::parse(
            r#"{"ai.effort": "High", "ai.effort.rewrite": "low", "ai.effort.nope": "x"}"#,
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(s.ai.effort_for(AiMode::Ask), EffortChoice::High);
        assert_eq!(s.ai.effort_for(AiMode::Rewrite), EffortChoice::Low);
        let (_, errs) = Settings::parse(r#"{"ai.effort": "ultra"}"#);
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn project_and_language_settings_layer_over_the_users() {
        let user = r#"{"tabsize": 4, "autoclose": true, "[rust]": {"tabsize": 2}}"#;
        let proj = r#"{"autoclose": false, "[rust]": {"formatonsave": true, "lsp": "ra-custom"},
                       "[python]": {"tabsize": 8}, "[go]": 3}"#;
        let (s, errs) =
            Settings::parse_layers(&[("settings.json", user), (".skink/settings.json", proj)]);
        assert_eq!(errs, ["[go] must be an object of settings"]);
        assert_eq!((s.tabsize, s.autoclose), (4, false));
        let rs = s.for_language(&["rust", "rs"]).unwrap();
        assert_eq!(
            (rs.tabsize, rs.format_on_save, rs.autoclose),
            (2, true, false)
        );
        assert!(
            matches!(&rs.lsp[0], (n, LspSetting::Server { argv, .. }) if n == "rust" && argv == &["ra-custom"])
        );
        assert_eq!(s.for_language(&["python"]).unwrap().tabsize, 8);
        assert!(
            s.lsp.iter().any(|(n, _)| n == "rust"),
            "section lsp reaches the server table"
        );
        assert!(s.for_language(&["zig"]).is_none());
        let (_, errs) = Settings::parse(r#"{"[rust]": {"tabsize": 99}}"#);
        assert_eq!(errs, ["[rust] tabsize must be 1-16"]);
    }

    #[test]
    fn lsp_settings_take_strings_lists_objects_and_false() {
        let (s, errs) = Settings::parse(
            r#"{"lsp.dart": "fvm dart language-server --protocol=lsp",
                "lsp.python": ["pylsp"],
                "lsp.elixir": {"command": "elixir-ls", "extensions": ["ex", "exs"],
                               "env": {"MIX_ENV": "dev"}, "initoptions": {"a": 1}},
                "lsp.go": false, "lsp.rust": true, "lsp.zig": 3}"#,
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].starts_with("lsp.zig"));
        let names: Vec<_> = s.lsp.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["dart", "elixir", "go", "python"]);
        assert!(
            matches!(&s.lsp[0].1, LspSetting::Server { argv, .. } if argv[0] == "fvm" && argv.len() == 4)
        );
        assert!(
            matches!(&s.lsp[1].1, LspSetting::Server { env, extensions, .. }
            if env == &[("MIX_ENV".to_string(), "dev".to_string())] && extensions.len() == 2)
        );
        assert_eq!(s.lsp[2].1, LspSetting::Off);
    }

    #[test]
    fn hand_edited_json_slips_are_accepted() {
        // Regression: a trailing comma made Skink drop the whole settings
        // file, so "ai.agent" silently fell back to "auto".
        let (s, errs) = Settings::parse(
            "{\n  // my agent\n  \"tabsize\": 2,\n  \"ai.agent\": \"claude-code-acp\", /* x */\n}\n",
        );
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(s.tabsize, 2);
        assert_eq!(s.ai.agent, AgentChoice::Named("claude-code-acp".into()));
        // Commas and slashes inside strings are untouched.
        assert_eq!(relaxed_json(r#"{"a": "x, }//y",}"#), r#"{"a": "x, }//y"}"#);
    }

    use super::*;

    #[test]
    fn settings_parse_and_validate() {
        let (s, e) =
            Settings::parse(r#"{"tabsize": 2, "icons": "ascii", "syntax": false, "x": 1}"#);
        assert!(e.is_empty());
        assert_eq!((s.tabsize, s.icons.as_str(), s.syntax), (2, "ascii", false));
        let (s, e) = Settings::parse(r#"{"tabsize": 99, "icons": "wat"}"#);
        assert_eq!(e.len(), 2);
        let plain = |mut s: Settings| {
            s.base.clear();
            s
        };
        assert_eq!(plain(s), Settings::default());
        assert_eq!(Settings::parse("{oops").1.len(), 1);
        assert_eq!(
            plain(Settings::parse(DEFAULT_SETTINGS).0),
            Settings::default()
        );
    }

    #[test]
    fn ai_settings() {
        let (s, e) = Settings::parse(
            r#"{"ai.agent": "gemini", "ai.model": "big", "ai.model.rewrite": "fast", "ai.model.nope": "x"}"#,
        );
        assert_eq!(e.len(), 1);
        assert_eq!(s.ai.agent, AgentChoice::Named("gemini".into()));
        assert_eq!(
            s.ai.model_for(AiMode::Rewrite),
            &ModelChoice::Named("fast".into())
        );
        assert_eq!(
            s.ai.model_for(AiMode::Ask),
            &ModelChoice::Named("big".into())
        );
        let (s, _) = Settings::parse(r#"{"ai.agent": ["my-agent", "--acp"]}"#);
        assert_eq!(
            s.ai.agent,
            AgentChoice::Command(vec!["my-agent".into(), "--acp".into()])
        );
    }

    #[test]
    fn bindings_file() {
        let b = parse_bindings(r#"{"Ctrl-j": "Save", "F5": "Quit", "bad": 3}"#).unwrap();
        assert_eq!(b.len(), 2);
        assert!(parse_bindings("[]").is_err());
        assert!(parse_bindings(DEFAULT_BINDINGS_FILE).unwrap().is_empty());
    }

    #[test]
    fn recent_list_moves_to_front_and_caps() {
        let d = std::env::temp_dir().join(format!("skink-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        recent_push(&d, Path::new("/a"));
        recent_push(&d, Path::new("/b"));
        let l = recent_push(&d, Path::new("/a"));
        assert_eq!(l, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(recent_load(&d), l);
        for i in 0..80 {
            recent_push(&d, Path::new(&format!("/f{i}")));
        }
        assert_eq!(recent_load(&d).len(), RECENT_MAX);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
