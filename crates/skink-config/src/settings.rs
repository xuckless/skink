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
    "macoptionisalt": "auto",
    "updates": "notify",
    "ai.agent": "auto",
    "ai.model": "default"
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
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            agent: AgentChoice::Auto,
            model: ModelChoice::AgentDefault,
            mode_models: Vec::new(),
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
}

fn model_choice(v: &str) -> ModelChoice {
    match v.trim() {
        "" | "default" | "auto" => ModelChoice::AgentDefault,
        x => ModelChoice::Named(x.to_owned()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub mac_option: MacOption,
    pub updates: UpdatePolicy,
    pub ai: AiSettings,
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
            mac_option: MacOption::Auto,
            updates: UpdatePolicy::Notify,
            ai: AiSettings::default(),
        }
    }
}

impl Settings {
    /// Parse `settings.json`; unknown keys are ignored, bad values fall back
    /// to the defaults and are reported in the second return value.
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut s = Self::default();
        let mut errs = Vec::new();
        let v: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => return (s, vec![format!("settings.json: {e}")]),
        };
        let Some(obj) = v.as_object() else {
            return (s, vec!["settings.json: expected an object".into()]);
        };
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
        match std::fs::read_to_string(dir.join("settings.json")) {
            Ok(t) => Self::parse(&t),
            Err(_) => (Self::default(), Vec::new()),
        }
    }
}

/// Parse a Micro-style `bindings.json` object into (key, action) pairs.
pub fn parse_bindings(text: &str) -> Result<Vec<(String, String)>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("bindings.json: {e}"))?;
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
    use super::*;

    #[test]
    fn settings_parse_and_validate() {
        let (s, e) =
            Settings::parse(r#"{"tabsize": 2, "icons": "ascii", "syntax": false, "x": 1}"#);
        assert!(e.is_empty());
        assert_eq!((s.tabsize, s.icons.as_str(), s.syntax), (2, "ascii", false));
        let (s, e) = Settings::parse(r#"{"tabsize": 99, "icons": "wat"}"#);
        assert_eq!(e.len(), 2);
        assert_eq!(s, Settings::default());
        assert_eq!(Settings::parse("{oops").1.len(), 1);
        assert_eq!(Settings::parse(DEFAULT_SETTINGS).0, Settings::default());
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
