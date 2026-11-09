//! Config files as first-class documents: the project's
//! `.skink/settings.json` (it layers over the user's) reloads on save;
//! settings.json and bindings.json are checked on open and save, with the
//! problems shown as diagnostics (gutter, Shift-F8); key completion inside
//! settings files; JSON schemas for a JSON language server.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use skink_lsp::{CompletionItem, Diagnostic, ItemKind, Position, Range, Severity, TextEdit};

use crate::App;
use crate::agents::{FileStamp, file_stamp};

/// Which config file a document is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFile {
    Settings,
    ProjectSettings,
    Bindings,
}

/// Problems found in config files, per canonical path.
#[derive(Debug, Default)]
pub enum ConfigProblems {
    #[default]
    Unchecked,
    Checked {
        by_path: HashMap<PathBuf, (FileStamp, Vec<Diagnostic>)>,
    },
}

impl ConfigProblems {
    pub fn for_path(&self, p: &Path) -> &[Diagnostic] {
        match self {
            ConfigProblems::Unchecked => &[],
            ConfigProblems::Checked { by_path } => {
                by_path.get(p).map_or(&[], |(_, d)| d.as_slice())
            }
        }
    }

    fn stamp(&self, p: &Path) -> Option<FileStamp> {
        match self {
            ConfigProblems::Unchecked => None,
            ConfigProblems::Checked { by_path } => by_path.get(p).map(|(s, _)| *s),
        }
    }

    /// One transition: record what a check of `p` found.
    fn record(&mut self, p: PathBuf, stamp: FileStamp, found: Vec<Diagnostic>) {
        match self {
            ConfigProblems::Unchecked => {
                *self = ConfigProblems::Checked {
                    by_path: HashMap::from([(p, (stamp, found))]),
                };
            }
            ConfigProblems::Checked { by_path } => {
                by_path.insert(p, (stamp, found));
            }
        }
    }
}

fn diag(line: usize, col: usize, message: String, error: bool) -> Diagnostic {
    let p = Position { line, col };
    Diagnostic {
        range: Range {
            start: p,
            end: Position { line, col: col + 1 },
        },
        severity: if error {
            Severity::Error
        } else {
            Severity::Warning
        },
        message,
        source: Some("skink".into()),
        code: None,
    }
}

/// Starter text for a project's settings file.
pub const PROJECT_SETTINGS: &str = "{\n    // Settings for this project only: they override your settings.json.\n    // Language sections apply to one language, e.g.\n    //   \"[rust]\": {\"tabsize\": 4, \"formatonsave\": true},\n    //   \"[python]\": {\"lsp\": \"pylsp\"}\n}\n";

/// The last version of the project settings file the app has loaded, and
/// what the config checks found.
#[derive(Default)]
pub struct ConfigState {
    project_seen: Option<FileStamp>,
    pub problems: ConfigProblems,
}

impl App {
    /// Whose `.skink/settings.json` applies: the workspace (the tree's root,
    /// the folder Skink opened), else the live file's project.
    pub(crate) fn settings_root(&self) -> PathBuf {
        self.roots()
            .into_iter()
            .next()
            .unwrap_or_else(|| self.project_root())
    }

    /// The project's settings file, canonical (as open documents are keyed).
    pub(crate) fn project_settings_path(&self) -> PathBuf {
        crate::doc::canonical(&skink_config::project_settings_path(&self.settings_root()))
    }

    /// `:settings project`: open (creating) `<project>/.skink/settings.json`.
    pub(crate) fn open_project_settings(&mut self) {
        let path = skink_config::project_settings_path(&self.settings_root());
        if !path.exists() {
            let made = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, PROJECT_SETTINGS));
            if let Err(e) = made {
                self.doc.editor.message = Some(format!("{}: {e}", path.display()));
                return;
            }
        }
        self.open_path(&path);
        self.cfg.project_seen = file_stamp(&path);
        self.check_config_doc();
    }

    /// Called after input: a save of the project settings reloads them.
    pub(crate) fn watch_project_settings_save(&mut self) {
        if self.doc.editor.buffer.is_dirty() {
            return;
        }
        let path = self.project_settings_path();
        if self.doc.canon.as_deref() != Some(path.as_path()) {
            return;
        }
        let stamp = file_stamp(&path);
        let seen = std::mem::replace(&mut self.cfg.project_seen, stamp);
        if seen.is_none() || seen == stamp {
            return;
        }
        let errs = self.load_user_config();
        self.doc.editor.message = Some(match errs.is_empty() {
            true => "Project settings applied".into(),
            false => errs.join("; "),
        });
    }
}

impl App {
    /// Which config file `p` (canonical) is, if any.
    pub(crate) fn config_file(&self, p: &Path) -> Option<ConfigFile> {
        let dir = self.config_dir.as_ref().map(|d| crate::doc::canonical(d));
        let in_cfg = |name: &str| {
            dir.as_ref()
                .is_some_and(|d| p == crate::doc::canonical(&d.join(name)))
        };
        if in_cfg("settings.json") {
            Some(ConfigFile::Settings)
        } else if in_cfg("bindings.json") {
            Some(ConfigFile::Bindings)
        } else if p == self.project_settings_path() {
            Some(ConfigFile::ProjectSettings)
        } else {
            None
        }
    }

    /// Check the live document if it is a config file whose saved version
    /// was not checked yet (on open and on every save).
    pub(crate) fn check_config_doc(&mut self) {
        let Some(p) = self.doc.canon.clone() else {
            return;
        };
        let Some(kind) = self.config_file(&p) else {
            return;
        };
        let Some(stamp) = file_stamp(&p) else {
            return;
        };
        if self.cfg.problems.stamp(&p) == Some(stamp) {
            return;
        }
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        let found = match kind {
            ConfigFile::Settings | ConfigFile::ProjectSettings => {
                skink_config::schema::check_settings(&text)
                    .into_iter()
                    .map(|i| diag(i.line, i.col, i.message, i.error))
                    .collect()
            }
            ConfigFile::Bindings => self.check_bindings(&text),
        };
        self.cfg.problems.record(p, stamp, found);
        self.write_schemas();
    }

    /// bindings.json: JSON, key names, and action names.
    fn check_bindings(&self, text: &str) -> Vec<Diagnostic> {
        let pairs = match skink_config::parse_bindings(text) {
            Ok(p) => p,
            Err(e) => return vec![diag(0, 0, e, true)],
        };
        let at = |key: &str| {
            let q = format!("\"{key}\"");
            let i = text.find(&q).unwrap_or(0);
            let before = &text[..i];
            (
                before.matches('\n').count(),
                before.rsplit('\n').next().map_or(0, |l| l.chars().count()),
            )
        };
        let mut out = Vec::new();
        // What each key does without bindings.json: Micro's default or the
        // preset's.
        let mut base = self.base_keymap.clone();
        base.apply_preset(self.settings.keymap);
        let preset = self.settings.keymap;
        let mut seen: Vec<(skink_config::KeySpec, &str)> = Vec::new();
        for (key, chain) in &pairs {
            let (line, col) = at(key);
            if let Some(spec) = skink_config::parse_key(key) {
                if let Some((_, first)) = seen.iter().find(|(s, _)| *s == spec) {
                    out.push(diag(
                        line,
                        col,
                        format!("{key} is the same key as {first}: only the last one counts"),
                        false,
                    ));
                }
                seen.push((spec, key));
                if let Some(was) = base.command(&spec)
                    && was != chain
                {
                    let from_preset = preset
                        .overrides()
                        .iter()
                        .any(|(k, _)| skink_config::parse_key(k) == Some(spec));
                    let whose = if from_preset {
                        format!("the {} keymap", preset.name())
                    } else {
                        "the default".to_owned()
                    };
                    let mut d = diag(line, col, format!("{key} replaces {was} ({whose})"), false);
                    d.severity = Severity::Info;
                    out.push(d);
                }
            }
            if skink_config::parse_key(key).is_none() {
                out.push(diag(
                    line,
                    col,
                    format!("unknown key name \"{key}\" (e.g. Ctrl-x, Alt-Shift-Up, F5)"),
                    true,
                ));
            }
            if chain.starts_with("command-edit:") {
                continue;
            }
            for name in chain.split(['|', ',']).map(str::trim) {
                if !self.action_exists(name) {
                    let (l, c) = at(chain);
                    out.push(diag(
                        if c == 0 && l == 0 { line } else { l },
                        c,
                        format!("unknown action \"{name}\" (F1 lists them)"),
                        true,
                    ));
                }
            }
        }
        out
    }

    /// After `:bindings`: say how many keys replace a default or repeat.
    pub(crate) fn say_binding_conflicts(&mut self) {
        let Some(p) = self.doc.canon.clone() else {
            return;
        };
        let found = self.cfg.problems.for_path(&p);
        let replaced = found
            .iter()
            .filter(|d| d.message.contains(" replaces "))
            .count();
        let repeated = found
            .iter()
            .filter(|d| d.message.contains("is the same key as"))
            .count();
        let other = found.len() - replaced - repeated;
        if found.is_empty() {
            self.doc.editor.message = Some(format!(
                "No conflicts with the {} keymap",
                self.settings.keymap.name()
            ));
            return;
        }
        self.doc.editor.message = Some(format!(
            "{replaced} replace a {} key, {repeated} repeat a key, {other} other problems (Shift-F8 lists them)",
            self.settings.keymap.name()
        ));
    }

    /// An action a binding may name.
    fn action_exists(&self, name: &str) -> bool {
        self.registry.get(name).is_some()
            || crate::commands::APP_ACTIONS.iter().any(|a| a.name == name)
            || skink_config::DEFAULT_BINDINGS
                .iter()
                .any(|(_, c)| c.split(['|', ',']).any(|n| n.trim() == name))
    }

    /// The JSON schemas, for a JSON language server (written when changed).
    pub(crate) fn schema_dir(&self) -> Option<PathBuf> {
        self.state_dir.as_ref().map(|d| d.join("schema"))
    }

    fn write_schemas(&self) {
        let Some(dir) = self.schema_dir() else {
            return;
        };
        let files = [
            (
                "settings.schema.json",
                skink_config::schema::settings_schema(),
            ),
            (
                "bindings.schema.json",
                skink_config::schema::bindings_schema(),
            ),
        ];
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        for (name, v) in files {
            let text = serde_json::to_string_pretty(&v).unwrap_or_default();
            let path = dir.join(name);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                let _ = std::fs::write(&path, text);
            }
        }
    }

    /// `json.schemas` for the JSON language server: Skink's config files.
    pub(crate) fn json_server_settings(&self) -> Option<serde_json::Value> {
        let dir = self.schema_dir()?;
        let uri = |n: &str| Some(skink_lsp::uri::path_to_uri(&dir.join(n)));
        let cfg = self.config_dir.as_ref()?;
        let m = |n: &str| cfg.join(n).to_string_lossy().into_owned();
        Some(serde_json::json!({"json": {"schemas": [
            {"fileMatch": [m("settings.json"), "**/.skink/settings.json"], "url": uri("settings.schema.json")?},
            {"fileMatch": [m("bindings.json")], "url": uri("bindings.schema.json")?}
        ]}}))
    }

    /// Completion of setting names inside a settings file: the cursor sits
    /// in a key string (`"ta|`).
    pub(crate) fn config_completion(&self) -> Option<Vec<CompletionItem>> {
        let p = self.doc.canon.as_deref()?;
        if !matches!(
            self.config_file(p)?,
            ConfigFile::Settings | ConfigFile::ProjectSettings
        ) {
            return None;
        }
        let ed = &self.doc.editor;
        let line = ed.cursor.line;
        let text = ed.buffer.line_text(line);
        let chars: Vec<char> = text.chars().collect();
        let col = ed.cursor.col.min(chars.len());
        let quote = chars[..col].iter().rposition(|&c| c == '"')?;
        let prefix: String = chars[quote + 1..col].iter().collect();
        let head: String = chars[..quote].iter().collect();
        let in_key_spot = head.trim().is_empty() || head.trim_end().ends_with(['{', ',']);
        if !in_key_spot
            || !prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.')
        {
            return None;
        }
        let closed = chars.get(col) == Some(&'"');
        let items = skink_config::schema::complete_keys(&prefix)
            .into_iter()
            .map(|(name, about)| CompletionItem {
                label: name.to_owned(),
                insert_text: name.to_owned(),
                snippet: false,
                documentation: Some(about.to_owned()),
                kind: ItemKind::Property,
                detail: Some(about.to_owned()),
                sort_text: None,
                edit: Some(TextEdit {
                    range: Range {
                        start: Position {
                            line,
                            col: quote + 1,
                        },
                        end: Position { line, col },
                    },
                    new_text: if closed {
                        name.to_owned()
                    } else {
                        format!("{name}\": ")
                    },
                }),
                additional_edits: Vec::new(),
                resolve: Default::default(),
            })
            .collect::<Vec<_>>();
        Some(items)
    }
}
