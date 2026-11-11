//! Packages in the editor: what is installed loads at start (grammars,
//! language servers, themes, tool PATH, scripts), `:install` runs on a
//! worker, `:uninstall` and `:packages enable/disable` apply at once, and a
//! file with no grammar gets one hint per type.

use crate::App;
use crate::nav::Level;
use crate::packages::{self, Active, Installed, Job, Spec, Store, When};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};

#[derive(Default)]
pub(crate) struct Packages {
    pub(crate) active: Active,
    pub(crate) installed: Vec<Installed>,
    pub(crate) work: Vec<Work>,
    /// File types already hinted this session.
    hinted: BTreeSet<String>,
    /// Scripts and project settings waiting for the trust question, asked
    /// one at a time.
    pub(crate) to_trust: Vec<crate::script::TrustAsk>,
}

/// Background package work.
pub(crate) enum Work {
    Install {
        label: String,
        rx: Receiver<Job>,
    },
    /// `:install .` working out what the project needs.
    Plan(Receiver<Result<Vec<(Spec, String)>, String>>),
}

impl App {
    fn pkg_store(&self) -> Option<Store> {
        self.data_dir.clone().map(Store::at)
    }

    /// `package kotlin` for a file inside an installed package, else `None`.
    pub(crate) fn script_owner(&self, path: &Path) -> Option<String> {
        let store = self.pkg_store()?;
        let rel = path.strip_prefix(store.data.join("packages")).ok()?;
        let name = rel
            .components()
            .next()?
            .as_os_str()
            .to_string_lossy()
            .into_owned();
        Some(format!("package {name}"))
    }

    /// Register what the installed packages add. Called once before the
    /// first frame; the grammars themselves load on first use, on a worker.
    pub(crate) fn load_packages(&mut self) {
        self.reload_packages();
        if !self.pkgs.active.broken.is_empty() {
            let msg = format!(
                "Broken packages (:packages): {}",
                self.pkgs.active.broken.join("; ")
            );
            self.toast(Level::Warn, msg);
        }
        self.suggest_package();
    }

    pub(crate) fn reload_packages(&mut self) {
        let Some(store) = self.pkg_store() else {
            return;
        };
        let new = store.active();
        for old in &self.pkgs.active.grammars {
            if !new.grammars.iter().any(|g| g.name == old.name) {
                skink_syntax::unregister_grammar(&old.name);
            }
        }
        for g in &new.grammars {
            // Unchanged grammars keep their compiled module.
            if !self.pkgs.active.grammars.contains(g) {
                skink_syntax::register_grammar(g.clone());
            }
        }
        self.pkgs.active = new;
        self.pkgs.installed = store.installed();
        // Files that had no grammar, or a package one, pick theirs again.
        for i in 0..self.tabs.len() {
            let Some(path) = self
                .doc_at(i)
                .filter(|d| {
                    d.syntax
                        .as_ref()
                        .is_none_or(|s| matches!(s.hl.lang(), skink_syntax::Lang::Dyn(_)))
                })
                .and_then(|d| d.editor.buffer.path().map(Path::to_path_buf))
            else {
                continue;
            };
            let on = self.settings_for(Some(&path)).syntax;
            if let Some(d) = self.doc_at_mut(i) {
                d.reset_syntax(on);
            }
        }
        self.refresh_packages_view();
    }

    /// "No grammar for X: :install Y", once per file type.
    pub(crate) fn suggest_package(&mut self) {
        let Some(path) = self.doc.editor.buffer.path().map(Path::to_path_buf) else {
            return;
        };
        if skink_syntax::language_for_path(&path).is_some() {
            return;
        }
        let Some(name) = packages::suggestion_for(&path, &self.pkgs.installed) else {
            return;
        };
        let kind = match path.extension() {
            Some(x) => format!(".{}", x.to_string_lossy()),
            None => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        if self.pkgs.hinted.insert(kind.clone()) {
            self.toast(
                Level::Info,
                format!("No grammar for {kind}: :install {name}"),
            );
        }
    }

    /// `:install NAME | owner/repo[/package][@tag] | DIR | .`
    pub(crate) fn install_cmd(&mut self, arg: &str) {
        let arg = arg.trim();
        if arg.is_empty() {
            self.doc.editor.message =
                Some("usage: :install NAME | owner/repo[/package][@tag] | DIR | .".into());
            return;
        }
        if arg == "." {
            let root = self.project_root();
            let (tx, rx) = channel();
            let wake = self.wake.clone();
            let store = self.pkg_store();
            std::thread::spawn(move || {
                let plan = store
                    .ok_or_else(|| "no data folder (set SKINK_DATA_DIR)".to_owned())
                    .and_then(|s| s.project_plan(&root));
                let _ = tx.send(plan);
                if let Some(w) = wake {
                    w();
                }
            });
            self.pkgs.work.push(Work::Plan(rx));
            self.toast(Level::Info, "Looking at the project's files...");
            return;
        }
        match Spec::parse(arg) {
            Ok(spec) => self.start_install(vec![spec]),
            Err(e) => self.doc.editor.message = Some(e),
        }
    }

    pub(crate) fn start_install(&mut self, specs: Vec<Spec>) {
        for spec in specs {
            let label = spec.to_string();
            let (tx, rx) = channel();
            let wake = self.wake.clone();
            let store = self.pkg_store();
            std::thread::spawn(move || {
                let Some(store) = store else {
                    let _ = tx.send(Job::Failed {
                        step: packages::Step::Resolving,
                        reason: "no data folder (set SKINK_DATA_DIR)".into(),
                    });
                    return;
                };
                store.install(&spec, When::Always, &mut |j| {
                    let _ = tx.send(j.clone());
                    if let Some(w) = &wake {
                        w();
                    }
                });
            });
            self.toast(Level::Info, format!("Installing {label}..."));
            self.pkgs.work.push(Work::Install { label, rx });
        }
    }

    pub(crate) fn poll_packages(&mut self) {
        if self.pkgs.work.is_empty() {
            return;
        }
        let mut done = Vec::new();
        let mut plans = Vec::new();
        self.pkgs.work.retain(|w| match w {
            Work::Install { label, rx } => {
                let mut finished = false;
                while let Ok(j) = rx.try_recv() {
                    if matches!(j, Job::Done(_) | Job::Failed { .. }) {
                        done.push((label.clone(), j));
                        finished = true;
                    }
                }
                !finished
            }
            Work::Plan(rx) => match rx.try_recv() {
                Ok(p) => {
                    plans.push(p);
                    false
                }
                Err(_) => true,
            },
        });
        let mut changed = false;
        for (label, job) in done {
            match &job {
                Job::Done(d) => {
                    changed = true;
                    // Its hint and "Installing..." are answered now.
                    let hint = format!(":install {}", d.name);
                    let busy = format!("Installing {label}");
                    self.toasts
                        .retain(|t| !t.text.ends_with(&hint) && !t.text.starts_with(&busy));
                    let mut msg = job.to_string();
                    // "Adds ..." says what changed; the other notes are
                    // things to do. Never only a cut-off toast: each to-do is
                    // its own toast, and all are kept in :messages.
                    let (adds, todo): (Vec<_>, Vec<_>) =
                        d.notes.iter().partition(|n| n.starts_with("Adds "));
                    for a in adds {
                        msg.push_str(": ");
                        msg.push_str(&a["Adds ".len()..]);
                    }
                    if !todo.is_empty() {
                        msg.push_str(" · next steps in :messages");
                    }
                    self.toast(Level::Info, msg);
                    for n in todo {
                        self.toast(Level::Warn, n.clone());
                    }
                }
                _ => self.toast(Level::Error, format!("{label}: {job}")),
            }
        }
        if changed {
            self.reload_packages();
        }
        for plan in plans {
            match plan {
                Ok(p) if p.is_empty() => self.toast(
                    Level::Info,
                    "This project needs no packages that are not installed",
                ),
                Ok(p) => self.open_choice(crate::prompt::Choice::InstallPlan(p)),
                Err(e) => self.toast(Level::Error, e),
            }
        }
    }

    /// `:uninstall NAME`
    pub(crate) fn uninstall_cmd(&mut self, name: &str) {
        let name = name.trim();
        let Some(store) = self.pkg_store() else {
            return;
        };
        match store.uninstall(name) {
            Ok(()) => {
                self.reload_packages();
                self.toast(Level::Info, format!("Uninstalled {name}"));
            }
            Err(e) => self.doc.editor.message = Some(e),
        }
    }

    /// `:packages [enable NAME | disable NAME]`
    pub(crate) fn packages_cmd(&mut self, args: &[&str]) {
        let toggle = match args {
            ["enable", n] => Some((*n, true)),
            ["disable", n] => Some((*n, false)),
            [] => None,
            _ => {
                self.doc.editor.message =
                    Some("usage: :packages [enable NAME | disable NAME]".into());
                return;
            }
        };
        if let Some((name, on)) = toggle {
            let r = self
                .pkg_store()
                .ok_or_else(|| "no data folder".to_owned())
                .and_then(|s| s.set_enabled(name, on));
            match r {
                Ok(()) => {
                    self.reload_packages();
                    let verb = if on { "Enabled" } else { "Disabled" };
                    self.toast(Level::Info, format!("{verb} {name}"));
                }
                Err(e) => self.doc.editor.message = Some(e),
            }
            return;
        }
        let text = self.packages_text();
        self.open_view("Packages", &text);
    }

    fn packages_text(&self) -> String {
        let mut text = String::from("Installed packages\n\n");
        if self.pkgs.installed.is_empty() {
            text.push_str(
                "  None yet. :install NAME, or :install . for what this project needs.\n",
            );
        }
        for p in &self.pkgs.installed {
            let state = match &p.state {
                packages::PkgState::Installed => "on".to_owned(),
                packages::PkgState::Disabled => "off".to_owned(),
                packages::PkgState::Broken(why) => format!("broken: {why}"),
            };
            text.push_str(&format!(
                "  {:<14} {:<9} {:<4} {}\n",
                p.name, p.version, state, p.source
            ));
        }
        text.push_str(
            "\n:packages disable NAME / enable NAME, :uninstall NAME, :install NAME.\n\
             Grammars and themes apply at once; language servers and scripts with the next start.\n",
        );
        text
    }

    /// An open `:packages` tab follows installs and uninstalls.
    fn refresh_packages_view(&mut self) {
        let text = self.packages_text();
        let same = |d: &crate::doc::Doc| {
            d.canon.is_none()
                && d.editor.buffer.is_read_only()
                && d.editor.buffer.title().as_deref() == Some("Packages")
        };
        if let Some(i) = self.find_doc(same)
            && let Some(d) = self.doc_at_mut(i)
        {
            d.replace_view("Packages", &text);
        }
    }
}
