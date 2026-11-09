//! Settings beyond the user's settings.json: the project's
//! `.skink/settings.json` (it layers over the user's), reloading it on save.

use std::path::PathBuf;

use crate::App;
use crate::agents::{FileStamp, file_stamp};

/// Starter text for a project's settings file.
pub const PROJECT_SETTINGS: &str = "{\n    // Settings for this project only: they override your settings.json.\n    // Language sections apply to one language, e.g.\n    //   \"[rust]\": {\"tabsize\": 4, \"formatonsave\": true},\n    //   \"[python]\": {\"lsp\": \"pylsp\"}\n}\n";

/// The last version of the project settings file the app has loaded.
#[derive(Default)]
pub struct ConfigState {
    project_seen: Option<FileStamp>,
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
