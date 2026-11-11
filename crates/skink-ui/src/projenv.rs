//! Project environments: a Python virtualenv (`.venv`, `venv`), direnv's
//! `.envrc` and mise's tool versions (`mise.toml`, `.mise.toml`,
//! `.tool-versions`), applied to terminals, tasks and language servers.
//!
//! A virtualenv is only files, so it applies at once. `.envrc` and mise
//! configs run code, so they wait for the same trust question as a project's
//! `.skink/init.lua` (asked again when they change), and then the tools run
//! in the background with a deadline. Skink never runs `direnv allow` or
//! `mise trust` itself: if the tool refuses, `:env` says so.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant, SystemTime},
};

use crate::{App, nav::Level};

/// How long direnv or mise may take.
const TOOL_DEADLINE: Duration = Duration::from_secs(5);

/// mise's config files, in the order mise reads them.
const MISE_FILES: [&str; 3] = ["mise.toml", ".mise.toml", ".tool-versions"];

/// A file in the project that sets up its environment by running code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvFile {
    /// `.envrc`, through direnv.
    Envrc(PathBuf),
    /// `mise.toml` / `.mise.toml` / `.tool-versions`, through mise.
    Mise(PathBuf),
}

impl EnvFile {
    pub(crate) fn path(&self) -> &Path {
        match self {
            EnvFile::Envrc(p) | EnvFile::Mise(p) => p,
        }
    }

    fn tool(&self) -> &'static str {
        match self {
            EnvFile::Envrc(_) => "direnv",
            EnvFile::Mise(_) => "mise",
        }
    }
}

/// What a project has (file checks only; nothing run).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Found {
    /// A virtualenv folder with a `pyvenv.cfg`.
    venv: Vec<PathBuf>,
    /// Files whose tool is installed and that run code.
    files: Vec<EnvFile>,
    /// Modification times of everything above (and where they may appear),
    /// to notice changes.
    stamps: Vec<(PathBuf, Option<SystemTime>)>,
}

/// The tools' part of the environment.
#[derive(Debug)]
pub(crate) enum Tools {
    /// Nothing that runs code (a virtualenv at most).
    NotNeeded,
    /// Waiting for the user's trust.
    AwaitingTrust,
    /// direnv / mise running in the background.
    Loading {
        rx: Receiver<Result<Vec<(String, String)>, String>>,
        since: Instant,
    },
    Loaded(Vec<(String, String)>),
    /// The user said no (asked again when the files change).
    Declined,
    Failed(String),
}

/// The project environment, for the life of the process.
#[derive(Debug, Default)]
pub(crate) enum ProjectEnv {
    /// Not looked at (lean / scratch processes never look).
    #[default]
    Unknown,
    Known {
        found: Found,
        tools: Tools,
    },
}

/// Where in a virtualenv its programs are.
fn venv_bin(venv: &Path) -> PathBuf {
    venv.join(if cfg!(windows) { "Scripts" } else { "bin" })
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Look at `root`. `installed` says whether a tool is on PATH (a parameter
/// so tests need neither direnv nor mise).
pub(crate) fn detect(root: &Path, installed: impl Fn(&str) -> bool) -> Found {
    let mut f = Found::default();
    for name in [".venv", "venv"] {
        let dir = root.join(name);
        let cfg = dir.join("pyvenv.cfg");
        f.stamps.push((cfg.clone(), mtime(&cfg)));
        if cfg.is_file() && f.venv.is_empty() {
            f.venv.push(dir);
        }
    }
    let envrc = root.join(".envrc");
    f.stamps.push((envrc.clone(), mtime(&envrc)));
    if envrc.is_file() && installed("direnv") {
        f.files.push(EnvFile::Envrc(envrc));
    }
    let mut mise = None;
    for name in MISE_FILES {
        let p = root.join(name);
        f.stamps.push((p.clone(), mtime(&p)));
        if mise.is_none() && p.is_file() {
            mise = Some(p);
        }
    }
    if let Some(p) = mise
        && installed("mise")
    {
        f.files.push(EnvFile::Mise(p));
    }
    f
}

/// The virtualenv's variables: `VIRTUAL_ENV` and its programs first on PATH.
fn venv_vars(found: &Found, base_path: Option<std::ffi::OsString>) -> Vec<(String, String)> {
    let Some(venv) = found.venv.first() else {
        return Vec::new();
    };
    let mut dirs = vec![venv_bin(venv)];
    dirs.extend(std::env::split_paths(&base_path.unwrap_or_default()));
    let mut out = vec![("VIRTUAL_ENV".to_owned(), venv.display().to_string())];
    if let Ok(p) = std::env::join_paths(dirs) {
        out.push(("PATH".to_owned(), p.to_string_lossy().into_owned()));
    }
    out
}

/// Run `program args` in `dir`, returning stdout; an error names the tool's
/// own complaint (first stderr line) or the deadline.
fn run_tool(
    dir: &Path,
    program: &str,
    args: &[&str],
    deadline: Duration,
) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("DIRENV_LOG_FORMAT", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let read = |r: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut r) = r {
                let _ = r.read_to_string(&mut s);
            }
            s
        })
    };
    let out = read(child.stdout.take().map(|r| Box::new(r) as _));
    let err = read(child.stderr.take().map(|r| Box::new(r) as _));
    let end = Instant::now() + deadline;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} took longer than {} s",
                    deadline.as_secs()
                ));
            }
        }
    };
    let (out, err) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    let complaint = err
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("failed")
        .to_owned();
    if !status.success() {
        return Err(format!("{program}: {complaint}"));
    }
    if out.trim().is_empty() && !err.trim().is_empty() {
        // direnv with a blocked .envrc prints only its complaint.
        return Err(format!("{program}: {complaint}"));
    }
    Ok(out)
}

/// The variables direnv and mise set for `root`, applied in that order over
/// `start` (the virtualenv's).
fn run_tools(
    root: &Path,
    files: &[EnvFile],
    start: Vec<(String, String)>,
) -> Result<Vec<(String, String)>, String> {
    let mut vars = start;
    let mut set = |k: String, v: String| {
        vars.retain(|(x, _)| *x != k);
        vars.push((k, v));
    };
    for f in files {
        let json = match f {
            EnvFile::Envrc(_) => run_tool(root, "direnv", &["export", "json"], TOOL_DEADLINE)?,
            EnvFile::Mise(_) => run_tool(root, "mise", &["env", "--json"], TOOL_DEADLINE)?,
        };
        if json.trim().is_empty() {
            continue; // nothing to change
        }
        let v: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| format!("{}: unexpected output ({e})", f.tool()))?;
        let Some(map) = v.as_object() else {
            return Err(format!("{}: unexpected output", f.tool()));
        };
        for (k, v) in map {
            // direnv's own bookkeeping, and unset variables (null).
            if k.starts_with("DIRENV_") {
                continue;
            }
            if let Some(s) = v.as_str() {
                set(k.clone(), s.to_owned());
            }
        }
    }
    Ok(vars)
}

impl ProjectEnv {
    /// Variables to set for programs started in the project (PATH as a whole
    /// value).
    pub(crate) fn vars(&self) -> Vec<(String, String)> {
        match self {
            ProjectEnv::Known {
                tools: Tools::Loaded(v),
                ..
            } => v.clone(),
            ProjectEnv::Known { found, .. } => venv_vars(found, std::env::var_os("PATH")),
            ProjectEnv::Unknown => Vec::new(),
        }
    }

    /// One line for `:env` and the status of what is active.
    pub(crate) fn describe(&self) -> Vec<String> {
        let ProjectEnv::Known { found, tools } = self else {
            return vec!["Project environment: not looked at (scratch file)".into()];
        };
        let mut out = Vec::new();
        match found.venv.first() {
            Some(v) => out.push(format!("virtualenv: {} (active)", v.display())),
            None => out.push("virtualenv: none (.venv or venv with a pyvenv.cfg)".into()),
        }
        let names: Vec<String> = found
            .files
            .iter()
            .map(|f| {
                let file = f.path().file_name().unwrap_or_default().to_string_lossy();
                format!("{file} ({})", f.tool())
            })
            .collect();
        let files = names.join(", ");
        out.push(match tools {
            Tools::NotNeeded => {
                "direnv / mise: nothing to load (no .envrc or mise config, or the tool is not installed)"
                    .into()
            }
            Tools::AwaitingTrust => format!("{files}: waiting for your answer to load it"),
            Tools::Loading { .. } => format!("{files}: loading"),
            Tools::Loaded(_) => format!("{files}: loaded"),
            Tools::Declined => format!("{files}: not loaded (you said no; :env reload asks again)"),
            Tools::Failed(e) => format!("{files}: failed: {e}"),
        });
        out
    }

    /// A short name for what is active, for toasts ("" when nothing).
    pub(crate) fn active_label(&self) -> String {
        let ProjectEnv::Known { found, tools } = self else {
            return String::new();
        };
        let mut parts: Vec<String> = found
            .venv
            .iter()
            .map(|v| {
                v.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        if let Tools::Loaded(_) = tools {
            parts.extend(found.files.iter().map(|f| f.tool().to_owned()));
        }
        parts.join(" + ")
    }
}

impl App {
    /// Look at the project's environment (startup and `:env reload`).
    pub(crate) fn load_project_env(&mut self) {
        if self.lean_process() {
            return;
        }
        let root = self.project_root();
        let found = detect(&root, |t| skink_lsp::find_in_path(t).is_some());
        let tools = match found.files.is_empty() {
            true => Tools::NotNeeded,
            false if self.env_files_trusted(&found.files) => self.start_tools(&root, &found),
            false => {
                let files = found.files.iter().map(|f| f.path().to_path_buf()).collect();
                self.pkgs
                    .to_trust
                    .push(crate::script::TrustAsk::Env { files });
                if !matches!(self.overlay, crate::Overlay::Prompt(_)) {
                    self.ask_next_trust();
                }
                Tools::AwaitingTrust
            }
        };
        self.penv = ProjectEnv::Known { found, tools };
    }

    fn env_files_trusted(&self, files: &[EnvFile]) -> bool {
        files.iter().all(|f| {
            std::fs::read_to_string(f.path()).is_ok_and(|text| self.trusts(f.path(), &text))
        })
    }

    fn start_tools(&self, root: &Path, found: &Found) -> Tools {
        let (tx, rx) = mpsc::channel();
        let root = root.to_path_buf();
        let files = found.files.clone();
        let start = venv_vars(found, std::env::var_os("PATH"));
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let _ = tx.send(run_tools(&root, &files, start));
            if let Some(w) = wake {
                w();
            }
        });
        Tools::Loading {
            rx,
            since: Instant::now(),
        }
    }

    /// The answer to "load this project's environment?".
    pub(crate) fn answer_trust_env(&mut self, files: &[PathBuf], trust: bool) {
        let ProjectEnv::Known { found, tools } = &mut self.penv else {
            return;
        };
        let found = found.clone();
        if !trust {
            *tools = Tools::Declined;
            let names: Vec<String> = files
                .iter()
                .map(|f| {
                    f.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            self.toast(
                Level::Info,
                format!("Project environment not loaded ({})", names.join(", ")),
            );
            self.ask_next_trust();
            return;
        }
        for f in files {
            if let Ok(text) = std::fs::read_to_string(f) {
                self.remember_trust(f, &text);
            }
        }
        let root = self.project_root();
        let next = self.start_tools(&root, &found);
        if let ProjectEnv::Known { tools, .. } = &mut self.penv {
            *tools = next;
        }
        self.ask_next_trust();
    }

    /// Take the tools' result (event loop).
    pub(crate) fn poll_project_env(&mut self) {
        let ProjectEnv::Known {
            tools: Tools::Loading { rx, since },
            ..
        } = &self.penv
        else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) if since.elapsed() < TOOL_DEADLINE * 3 => return,
            Err(_) => Err("the environment tools stopped answering".into()),
        };
        let ok = result.is_ok();
        if let ProjectEnv::Known { tools, .. } = &mut self.penv {
            *tools = match result {
                Ok(v) => Tools::Loaded(v),
                Err(e) => Tools::Failed(e),
            };
        }
        match ok {
            true => {
                let label = self.penv.active_label();
                self.toast(Level::Info, format!("Project environment: {label}"));
                self.restart_lsp_link();
            }
            false => {
                let why = self.penv.describe().pop().unwrap_or_default();
                self.toast(Level::Warn, format!("Project environment: {why} (:env)"));
            }
        }
    }

    /// Notice changed `.envrc` / mise / virtualenv files (with the disk check).
    pub(crate) fn check_env_files(&mut self) {
        let ProjectEnv::Known { found, .. } = &self.penv else {
            return;
        };
        if found.stamps.iter().any(|(p, t)| mtime(p) != *t) {
            self.load_project_env();
            if let ProjectEnv::Known {
                tools: Tools::NotNeeded,
                ..
            } = &self.penv
            {
                // A virtualenv appeared or went: servers start with it.
                self.restart_lsp_link();
            }
        }
    }

    /// Environment for programs Skink starts: the project's variables, and
    /// PATH with package tools first.
    pub(crate) fn child_env(&self) -> Vec<(String, String)> {
        let mut vars = self.penv.vars();
        let path = vars
            .iter()
            .rev()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| std::ffi::OsString::from(v));
        vars.retain(|(k, _)| k != "PATH");
        let tools = &self.pkgs.active.path;
        if tools.is_empty() && path.is_none() {
            return vars;
        }
        let base = path
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        let dirs = tools.iter().cloned().chain(std::env::split_paths(&base));
        if let Ok(p) = std::env::join_paths(dirs) {
            vars.push(("PATH".into(), p.to_string_lossy().into_owned()));
        }
        vars
    }

    /// `:env [reload]`
    pub(crate) fn env_cmd(&mut self, arg: &str) {
        match arg.trim() {
            "" => {
                let mut lines = vec!["Project environment".to_owned(), String::new()];
                lines.extend(self.penv.describe());
                let vars = self.penv.vars();
                if !vars.is_empty() {
                    lines.push(String::new());
                    lines.push("Variables set for terminals, tasks and language servers:".into());
                    for (k, v) in vars {
                        let v = match v.chars().count() > 120 {
                            true => format!("{}…", v.chars().take(119).collect::<String>()),
                            false => v,
                        };
                        lines.push(format!("  {k}={v}"));
                    }
                }
                lines.push(String::new());
                lines.push(":env reload looks again (and asks again after a no).".into());
                self.open_view("Environment", &lines.join("\n"));
            }
            "reload" => {
                self.load_project_env();
                let label = self.penv.active_label();
                self.doc.editor.message = Some(match label.is_empty() {
                    true => "No project environment found".into(),
                    false => format!("Project environment: {label}"),
                });
                self.restart_lsp_link();
            }
            other => {
                self.doc.editor.message = Some(format!("usage: env [reload] (not {other:?})"));
            }
        }
    }

    /// Start the language servers again with the current environment
    /// (they keep the one they were started with).
    pub(crate) fn restart_lsp_link(&mut self) {
        if self.lsp.take().is_none() {
            return;
        }
        for i in 0..self.tabs.len() {
            if let Some(d) = self.doc_at_mut(i) {
                d.lsp = crate::doc::LspSync::Closed;
            }
        }
        self.doc.lsp = crate::doc::LspSync::Closed;
        self.intel.waiting.clear();
        self.lsp_attach_live();
    }
}

/// A `Found` with these files and nothing else (UI tests).
#[cfg(test)]
pub(crate) fn found_for_test(files: Vec<EnvFile>) -> Found {
    Found {
        files,
        ..Found::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("skink-env-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_virtualenv_applies_without_running_anything() {
        let d = temp("venv");
        std::fs::create_dir_all(d.join(".venv")).unwrap();
        std::fs::write(d.join(".venv/pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        let found = detect(&d, |_| false);
        assert!(found.files.is_empty());
        let env = ProjectEnv::Known {
            found,
            tools: Tools::NotNeeded,
        };
        let vars = env.vars();
        let get = |k: &str| vars.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
        assert_eq!(
            get("VIRTUAL_ENV"),
            Some(d.join(".venv").display().to_string())
        );
        let path = get("PATH").unwrap();
        let first = std::env::split_paths(&path).next().unwrap();
        assert_eq!(first, venv_bin(&d.join(".venv")));
        assert_eq!(env.active_label(), ".venv");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn envrc_and_mise_count_only_when_their_tool_is_installed() {
        let d = temp("tools");
        std::fs::write(d.join(".envrc"), "export A=1\n").unwrap();
        std::fs::write(d.join(".tool-versions"), "nodejs 22\n").unwrap();
        assert!(detect(&d, |_| false).files.is_empty());
        let found = detect(&d, |t| t == "direnv" || t == "mise");
        assert_eq!(
            found.files,
            [
                EnvFile::Envrc(d.join(".envrc")),
                EnvFile::Mise(d.join(".tool-versions"))
            ]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_tool_that_hangs_is_stopped_and_one_that_refuses_is_explained() {
        let d = temp("run");
        let t = Instant::now();
        let e = run_tool(&d, "sleep", &["30"], Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("longer than"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(5));
        let e = run_tool(
            &d,
            "sh",
            &["-c", "echo 'direnv: error .envrc is blocked' >&2"],
            TOOL_DEADLINE,
        )
        .unwrap_err();
        assert!(e.contains("is blocked"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
