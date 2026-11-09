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
    "updates": "notify"
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
