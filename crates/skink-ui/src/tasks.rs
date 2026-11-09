//! Tasks: `.skink/tasks.json` / `.vscode/tasks.json` (a VS Code subset:
//! label, command, args, options.cwd / options.env, group,
//! problemMatcher) plus tasks detected from the project's manifests (cargo,
//! npm / pnpm / yarn, go, zig, pytest, make). A task runs in a terminal tab;
//! when it exits its output goes through problem matchers whose findings
//! show as diagnostics (gutter, F9, F6) until the task runs again.

use crate::App;
use serde_json::Value;
use skink_lsp::{Diagnostic, Position, Range, Severity};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Which output parser a task's problems come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matcher {
    /// Try every parser below.
    Auto,
    /// `error[E..]: msg` + `--> file:line:col` (rustc, cargo, clippy).
    Rustc,
    /// `file(line,col): error TS..: msg` or `file:line:col - error TS..`.
    Tsc,
    /// eslint stylish (file line, then `line:col  error  msg`) and compact.
    Eslint,
    /// `file:line:col: msg` (go build / vet / test).
    Go,
    /// Python tracebacks and pytest `file:line: Error`.
    Python,
    /// `file:line[:col]: [error|warning|note]: msg` (gcc, clang, many more).
    Gcc,
    /// Problem matching off (`"problemMatcher": []`).
    Off,
}

impl Matcher {
    /// A VS Code matcher name (`$rustc`, `$tsc`, `$eslint-stylish`, ...).
    fn from_name(n: &str) -> Matcher {
        match n.trim_start_matches('$') {
            "rustc" | "rustc-watch" | "rust" => Matcher::Rustc,
            "tsc" | "tsc-watch" => Matcher::Tsc,
            n if n.starts_with("eslint") => Matcher::Eslint,
            "go" => Matcher::Go,
            "python" | "pytest" => Matcher::Python,
            "gcc" | "msCompile" | "lessCompile" => Matcher::Gcc,
            _ => Matcher::Auto,
        }
    }
}

/// Where a task came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    File,
    Detected(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub label: String,
    /// Shell command line (args appended, quoted when needed).
    pub line: String,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// `build`, `test` or empty.
    pub group: String,
    pub matcher: Matcher,
    pub origin: Origin,
}

/// Problems the last run of a task found.
#[derive(Debug, Default)]
pub enum TaskProblems {
    #[default]
    None,
    Found {
        label: String,
        by_path: HashMap<PathBuf, Vec<Diagnostic>>,
    },
}

impl TaskProblems {
    pub fn for_path(&self, p: &Path) -> &[Diagnostic] {
        match self {
            TaskProblems::None => &[],
            TaskProblems::Found { by_path, .. } => by_path.get(p).map_or(&[], Vec::as_slice),
        }
    }
    pub fn paths(&self) -> Vec<PathBuf> {
        match self {
            TaskProblems::None => Vec::new(),
            TaskProblems::Found { by_path, .. } => by_path.keys().cloned().collect(),
        }
    }
}

/// A running (or finished) task's terminal tab.
#[derive(Debug, Clone)]
pub struct TaskRun {
    pub task: Task,
}

fn quote(a: &str) -> String {
    if !a.is_empty()
        && a.chars()
            .all(|c| c.is_alphanumeric() || "-_./=:@%+,".contains(c))
    {
        a.to_owned()
    } else {
        format!("\"{}\"", a.replace('"', "\\\""))
    }
}

/// `${workspaceFolder}`, `${file}`, `${fileDirname}`, `${fileBasename}`,
/// `${relativeFile}`, `${env:NAME}`.
fn expand(s: &str, root: &Path, file: Option<&Path>) -> String {
    let mut out = s.replace("${workspaceFolder}", &root.to_string_lossy());
    out = out.replace("${workspaceRoot}", &root.to_string_lossy());
    if let Some(f) = file {
        out = out
            .replace(
                "${relativeFile}",
                &f.strip_prefix(root).unwrap_or(f).to_string_lossy(),
            )
            .replace(
                "${fileDirname}",
                &f.parent().unwrap_or(root).to_string_lossy(),
            )
            .replace(
                "${fileBasename}",
                &f.file_name().unwrap_or_default().to_string_lossy(),
            )
            .replace("${file}", &f.to_string_lossy());
    }
    while let Some(i) = out.find("${env:") {
        let Some(j) = out[i..].find('}') else { break };
        let name = &out[i + 6..i + j];
        let val = std::env::var(name).unwrap_or_default();
        out.replace_range(i..i + j + 1, &val);
    }
    out
}

/// Tasks from a tasks.json text.
pub fn parse_tasks_json(text: &str, root: &Path, file: Option<&Path>) -> Result<Vec<Task>, String> {
    let v: Value = serde_json::from_str(&skink_config::relaxed_json(text))
        .map_err(|e| format!("tasks.json: {e}"))?;
    let os_key = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    };
    let mut out = Vec::new();
    for t in v["tasks"].as_array().into_iter().flatten() {
        // Per-OS overrides, as in VS Code.
        let mut t = t.clone();
        if let Some(obj) = t.as_object_mut()
            && let Some(o) = obj.get(os_key).and_then(Value::as_object).cloned()
        {
            for (k, val) in o {
                obj.insert(k, val);
            }
        }
        let Some(command) = t["command"].as_str() else {
            continue;
        };
        let mut line = expand(command, root, file);
        for a in t["args"].as_array().into_iter().flatten() {
            let a = a
                .as_str()
                .map(str::to_owned)
                .or_else(|| a["value"].as_str().map(str::to_owned))
                .unwrap_or_default();
            line.push(' ');
            line.push_str(&quote(&expand(&a, root, file)));
        }
        let label = t["label"]
            .as_str()
            .or(t["taskName"].as_str())
            .map_or_else(|| command.to_owned(), str::to_owned);
        let cwd = t["options"]["cwd"]
            .as_str()
            .map_or_else(|| root.to_path_buf(), |c| root.join(expand(c, root, file)));
        let env = t["options"]["env"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            expand(v.as_str().unwrap_or_default(), root, file),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let group = t["group"]
            .as_str()
            .or(t["group"]["kind"].as_str())
            .unwrap_or_default()
            .to_owned();
        let matcher = match &t["problemMatcher"] {
            Value::String(s) => Matcher::from_name(s),
            Value::Array(a) if a.is_empty() => Matcher::Off,
            Value::Array(a) => a
                .iter()
                .find_map(Value::as_str)
                .map_or(Matcher::Auto, Matcher::from_name),
            _ => Matcher::Auto,
        };
        out.push(Task {
            label,
            line,
            cwd,
            env,
            group,
            matcher,
            origin: Origin::File,
        });
    }
    Ok(out)
}

fn detected(
    label: &str,
    line: &str,
    cwd: &Path,
    group: &str,
    m: Matcher,
    from: &'static str,
) -> Task {
    Task {
        label: label.to_owned(),
        line: line.to_owned(),
        cwd: cwd.to_path_buf(),
        env: Vec::new(),
        group: group.to_owned(),
        matcher: m,
        origin: Origin::Detected(from),
    }
}

/// Tasks guessed from the files in `root`.
pub fn detect(root: &Path) -> Vec<Task> {
    let has = |f: &str| root.join(f).exists();
    let mut out = Vec::new();
    if has("Cargo.toml") {
        for (l, g) in [
            ("cargo build", "build"),
            ("cargo check", "build"),
            ("cargo clippy", "build"),
            ("cargo test", "test"),
            ("cargo run", ""),
        ] {
            out.push(detected(l, l, root, g, Matcher::Rustc, "cargo"));
        }
    }
    if has("package.json") {
        let pm = if has("pnpm-lock.yaml") {
            "pnpm"
        } else if has("yarn.lock") {
            "yarn"
        } else if has("bun.lockb") || has("bun.lock") {
            "bun"
        } else {
            "npm"
        };
        let pkg: Value = std::fs::read_to_string(root.join("package.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        for (name, _) in pkg["scripts"].as_object().into_iter().flatten() {
            let group = match name.as_str() {
                "build" | "compile" | "typecheck" | "lint" => "build",
                n if n.starts_with("test") => "test",
                _ => "",
            };
            let line = format!("{pm} run {name}");
            out.push(detected(&line, &line, root, group, Matcher::Auto, "npm"));
        }
    }
    if has("go.mod") {
        for (l, g) in [
            ("go build ./...", "build"),
            ("go vet ./...", "build"),
            ("go test ./...", "test"),
        ] {
            out.push(detected(l, l, root, g, Matcher::Go, "go"));
        }
    }
    if has("build.zig") {
        for (l, g) in [
            ("zig build", "build"),
            ("zig build test", "test"),
            ("zig build run", ""),
        ] {
            out.push(detected(l, l, root, g, Matcher::Gcc, "zig"));
        }
    }
    if has("pytest.ini")
        || has("conftest.py")
        || has("tests") && (has("pyproject.toml") || has("setup.py") || has("setup.cfg"))
        || std::fs::read_to_string(root.join("pyproject.toml"))
            .is_ok_and(|t| t.contains("[tool.pytest"))
    {
        out.push(detected(
            "pytest",
            "pytest",
            root,
            "test",
            Matcher::Python,
            "python",
        ));
    }
    if let Some(mk) = ["Makefile", "makefile", "GNUmakefile"]
        .iter()
        .find_map(|f| std::fs::read_to_string(root.join(f)).ok())
    {
        out.push(detected(
            "make",
            "make",
            root,
            "build",
            Matcher::Gcc,
            "make",
        ));
        for t in make_targets(&mk) {
            let line = format!("make {t}");
            let g = if t.contains("test") { "test" } else { "" };
            out.push(detected(&line, &line, root, g, Matcher::Gcc, "make"));
        }
    }
    out
}

/// Plain targets of a Makefile (`name:` at line start, no pattern rules).
fn make_targets(mk: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in mk.lines() {
        let Some((name, rest)) = l.split_once(':') else {
            continue;
        };
        if rest.starts_with('=')
            || name.is_empty()
            || name.starts_with(['.', '\t', ' ', '#'])
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || "-_/".contains(c))
        {
            continue;
        }
        if !out.iter().any(|t| t == name) {
            out.push(name.to_owned());
        }
        if out.len() == 40 {
            break;
        }
    }
    out
}

// ---- problem matchers --------------------------------------------------------

fn diag(line: usize, col: usize, sev: Severity, msg: &str, source: &str) -> Diagnostic {
    let p = Position {
        line: line.saturating_sub(1),
        col: col.saturating_sub(1),
    };
    Diagnostic {
        range: Range { start: p, end: p },
        severity: sev,
        message: msg.trim().to_owned(),
        source: Some(source.to_owned()),
        code: None,
    }
}

fn severity(word: &str) -> Option<Severity> {
    match word.trim().to_ascii_lowercase().as_str() {
        "error" | "fatal error" | "fatal" | "err" | "panic" => Some(Severity::Error),
        "warning" | "warn" => Some(Severity::Warning),
        "note" | "info" | "help" => Some(Severity::Info),
        _ => None,
    }
}

/// `path:line[:col]` at the start of `s`; the rest after the next `:`.
pub(crate) fn file_line_col(s: &str) -> Option<(&str, usize, usize, &str)> {
    // Windows drive letters: skip a leading `C:`.
    let skip = if s.len() > 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
        2
    } else {
        0
    };
    let i = skip + s[skip..].find(':')?;
    let path = &s[..i];
    if path.is_empty() || path.contains(char::is_whitespace) && !path.contains('/') {
        return None;
    }
    let rest = &s[i + 1..];
    let n_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let line: usize = rest[..n_end].parse().ok()?;
    let rest = &rest[n_end..];
    let (col, rest) = match rest.strip_prefix(':') {
        Some(r) => {
            let c_end = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
            match r[..c_end].parse() {
                Ok(c) => (c, &r[c_end..]),
                Err(_) => (1, rest),
            }
        }
        None => (1, rest),
    };
    Some((path, line, col, rest))
}

type Found = Vec<(String, Diagnostic)>;

fn rustc(text: &str, out: &mut Found) {
    let mut pending: Option<(Severity, String)> = None;
    for l in text.lines() {
        let t = l.trim_start();
        if let Some((head, msg)) = t.split_once(": ")
            && !t.starts_with("-->")
        {
            let word = head.split('[').next().unwrap_or(head);
            if let Some(sev) = severity(word).filter(|_| !head.contains(' ') || word == head) {
                pending = Some((sev, msg.to_owned()));
                continue;
            }
        }
        if let Some(loc) = t.strip_prefix("--> ")
            && let Some((sev, msg)) = pending.take()
            && let Some((p, line, col, _)) = file_line_col(loc.trim())
        {
            out.push((p.to_owned(), diag(line, col, sev, &msg, "rustc")));
        }
    }
}

fn tsc(text: &str, out: &mut Found) {
    for l in text.lines() {
        let l = l.trim();
        // file(line,col): error TS1234: msg
        if let Some(open) = l.find('(')
            && let Some(close) = l[open..].find("): ")
        {
            let inner = &l[open + 1..open + close];
            if let Some((a, b)) = inner.split_once(',')
                && let (Ok(line), Ok(col)) = (a.parse(), b.parse())
            {
                let rest = &l[open + close + 3..];
                let (word, msg) = rest.split_once(' ').unwrap_or((rest, ""));
                if let Some(sev) = severity(word) {
                    out.push((l[..open].to_owned(), diag(line, col, sev, msg, "tsc")));
                }
            }
            continue;
        }
        // file:line:col - error TS1234: msg
        if let Some((p, line, col, rest)) = file_line_col(l)
            && let Some(rest) = rest.strip_prefix(" - ")
        {
            let (word, msg) = rest.split_once(' ').unwrap_or((rest, ""));
            if let Some(sev) = severity(word) {
                out.push((p.to_owned(), diag(line, col, sev, msg, "tsc")));
            }
        }
    }
}

fn eslint(text: &str, out: &mut Found) {
    let mut file: Option<String> = None;
    for l in text.lines() {
        // compact: path: line 3, col 7, Error - msg (rule)
        if let Some((p, rest)) = l.split_once(": line ")
            && let Some((line, rest)) = rest.split_once(", col ")
            && let Some((col, rest)) = rest.split_once(", ")
            && let Some((word, msg)) = rest.split_once(" - ")
            && let (Ok(line), Ok(col), Some(sev)) = (line.parse(), col.parse(), severity(word))
        {
            out.push((p.to_owned(), diag(line, col, sev, msg, "eslint")));
            continue;
        }
        // stylish: a path line, then "  3:7  error  msg  rule"
        if !l.starts_with(char::is_whitespace) && !l.trim().is_empty() {
            file = Some(l.trim().to_owned());
            continue;
        }
        let t = l.trim();
        let Some((pos, rest)) = t.split_once(char::is_whitespace) else {
            continue;
        };
        let Some((a, b)) = pos.split_once(':') else {
            continue;
        };
        let (Ok(line), Ok(col)) = (a.parse(), b.parse()) else {
            continue;
        };
        let rest = rest.trim_start();
        let (word, msg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if let (Some(f), Some(sev)) = (&file, severity(word)) {
            out.push((f.clone(), diag(line, col, sev, msg.trim(), "eslint")));
        }
    }
}

/// `path:line[:col]: [severity:] msg`; `default` when there is no word.
fn gcc_like(text: &str, out: &mut Found, default: Severity, source: &str) {
    for l in text.lines() {
        let t = l.trim_start();
        let Some((p, line, col, rest)) = file_line_col(t) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let rest = rest.trim_start();
        let (sev, msg) = match rest.split_once(':') {
            Some((w, m)) if severity(w).is_some() => (severity(w).unwrap_or(default), m),
            _ => (default, rest),
        };
        if msg.trim().is_empty() {
            continue;
        }
        out.push((p.to_owned(), diag(line, col, sev, msg, source)));
    }
}

fn python(text: &str, out: &mut Found) {
    let mut frame: Option<(String, usize)> = None;
    for l in text.lines() {
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix("File \"")
            && let Some((p, rest)) = rest.split_once("\", line ")
        {
            let n_end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            if let Ok(n) = rest[..n_end].parse() {
                frame = Some((p.to_owned(), n));
            }
            continue;
        }
        // The exception line ends the traceback: `ValueError: msg`.
        if !l.starts_with(char::is_whitespace)
            && let Some((exc, _)) = t.split_once(':')
            && (exc.ends_with("Error") || exc.ends_with("Exception") || exc.ends_with("Exit"))
            && let Some((p, n)) = frame.take()
        {
            out.push((p, diag(n, 1, Severity::Error, t, "python")));
        }
    }
    // pytest: tests/test_x.py:12: AssertionError
    gcc_like(text, out, Severity::Error, "pytest");
}

/// Problems in `text` (a task's output) by `m`; relative paths resolve
/// against `cwd`, and only files that exist are kept.
pub fn parse_problems(text: &str, m: Matcher, cwd: &Path) -> HashMap<PathBuf, Vec<Diagnostic>> {
    let mut found = Found::new();
    match m {
        Matcher::Off => {}
        Matcher::Rustc => rustc(text, &mut found),
        Matcher::Tsc => tsc(text, &mut found),
        Matcher::Eslint => eslint(text, &mut found),
        Matcher::Go => gcc_like(text, &mut found, Severity::Error, "go"),
        Matcher::Python => python(text, &mut found),
        Matcher::Gcc => gcc_like(text, &mut found, Severity::Error, "gcc"),
        Matcher::Auto => {
            rustc(text, &mut found);
            tsc(text, &mut found);
            eslint(text, &mut found);
            python(text, &mut found);
        }
    }
    let mut by_path: HashMap<PathBuf, Vec<Diagnostic>> = HashMap::new();
    for (p, d) in found {
        let path = cwd.join(p.trim());
        if !path.is_file() {
            continue;
        }
        let v = by_path.entry(crate::doc::canonical(&path)).or_default();
        if !v
            .iter()
            .any(|x| x.range == d.range && x.message == d.message)
        {
            v.push(d);
        }
    }
    by_path
}

// ---- running ----------------------------------------------------------------

impl App {
    /// File tasks first (`.skink/tasks.json`, then `.vscode/tasks.json`),
    /// then detected ones not shadowed by a file task's command.
    pub(crate) fn all_tasks(&mut self) -> Vec<Task> {
        let root = self.project_root();
        let file = self.doc.canon.clone();
        let mut out: Vec<Task> = Vec::new();
        for rel in [".skink/tasks.json", ".vscode/tasks.json"] {
            let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
                continue;
            };
            match parse_tasks_json(&text, &root, file.as_deref()) {
                Ok(ts) => out.extend(ts),
                Err(e) => self.toast(crate::nav::Level::Error, format!("{rel}: {e}")),
            }
        }
        for t in detect(&root) {
            if !out.iter().any(|o| o.line == t.line || o.label == t.label) {
                out.push(t);
            }
        }
        out
    }

    /// `:task [LABEL|last]`: the picker, a task by label, or the last one.
    pub fn task_command(&mut self, arg: &str) {
        let arg = arg.trim();
        match arg {
            "" => self.pick_task(),
            "last" | "again" | "rerun" => self.rerun_task(),
            label => {
                let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
                match self
                    .all_tasks()
                    .into_iter()
                    .find(|t| norm(&t.label) == norm(label))
                {
                    Some(t) => self.run_task(t),
                    None => {
                        self.doc.editor.message =
                            Some(format!("No task {label:?} (:task lists them)"))
                    }
                }
            }
        }
    }

    fn pick_task(&mut self) {
        let tasks = self.all_tasks();
        if tasks.is_empty() {
            self.doc.editor.message = Some(
                "No tasks: add .vscode/tasks.json or .skink/tasks.json (none detected here)".into(),
            );
            return;
        }
        let items = tasks
            .into_iter()
            .map(|t| crate::picker::Item {
                detail: match t.origin {
                    Origin::File => format!("{}  (tasks.json)", t.line),
                    Origin::Detected(k) => format!("{}  ({k})", t.group),
                },
                tag: if t.group == "build" {
                    "build"
                } else if t.group == "test" {
                    "test"
                } else {
                    ""
                },
                target: crate::picker::Target::Command(format!("task {}", t.label)),
                label: t.label,
            })
            .collect();
        self.open_choices("Run task", items);
    }

    pub fn rerun_task(&mut self) {
        match self.last_task.clone() {
            Some(t) => self.run_task(t),
            None => self.pick_task(),
        }
    }

    /// Start `t` in a new terminal tab.
    pub(crate) fn run_task(&mut self, t: Task) {
        let shell = if cfg!(windows) {
            skink_term::CommandSpec::new("cmd").args(["/C", t.line.as_str()])
        } else {
            skink_term::CommandSpec::new("sh").args(["-c", t.line.as_str()])
        };
        let mut spec = shell.cwd(&t.cwd);
        for (k, v) in &t.env {
            spec = spec.env(k, v);
        }
        self.last_task = Some(t.clone());
        match self.spawn_terminal_tab(spec) {
            Ok(i) => {
                if let crate::Tab::Term(p) = &mut self.tabs[i] {
                    p.purpose = crate::Purpose::Task(Box::new(TaskRun { task: t.clone() }));
                    p.label = format!("task: {}", t.label);
                }
                self.switch_to(i);
                self.doc.editor.message = Some(format!("Running {}", t.line));
            }
            Err(e) => self.doc.editor.message = Some(format!("task: {e}")),
        }
    }

    /// A task's terminal exited: match its output into problems.
    pub(crate) fn task_finished(
        &mut self,
        run: &TaskRun,
        out: &str,
        status: Option<skink_term::ExitStatus>,
    ) {
        let by_path = parse_problems(out, run.task.matcher, &run.task.cwd);
        let n: usize = by_path.values().map(Vec::len).sum();
        let ok = status.as_ref().is_some_and(skink_term::ExitStatus::success);
        let code = status.map_or_else(|| "?".into(), |s| s.code.to_string());
        self.task_problems = TaskProblems::Found {
            label: run.task.label.clone(),
            by_path,
        };
        let level = if ok && n == 0 {
            crate::nav::Level::Info
        } else {
            crate::nav::Level::Error
        };
        let probs = match n {
            0 => String::new(),
            1 => " · 1 problem (F9)".into(),
            n => format!(" · {n} problems (F9)"),
        };
        let verdict = if ok {
            "done".to_owned()
        } else {
            format!("failed (exit {code})")
        };
        self.toast(level, format!("{} {verdict}{probs}", run.task.label));
    }

    /// Diagnostics for `p`: the language server's plus the last task's.
    pub(crate) fn all_diags(&self, p: &Path) -> Vec<Diagnostic> {
        let mut v = self
            .lsp
            .as_ref()
            .map_or_else(Vec::new, |l| l.diagnostics(p));
        v.extend_from_slice(self.task_problems.for_path(p));
        v
    }

    /// Files with diagnostics: the open ones plus those a task named.
    pub(crate) fn diag_paths(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = (0..self.tabs.len())
            .filter_map(|i| self.doc_at(i).and_then(|d| d.canon.clone()))
            .collect();
        for p in self.task_problems.paths() {
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_json_subset_with_variables_and_matchers() {
        let root = Path::new("/r");
        let text = r#"{
          // comments and trailing commas are fine
          "version": "2.0.0",
          "tasks": [
            {"label": "build", "type": "shell", "command": "cargo", "args": ["build", "--bin", "my app"],
             "group": {"kind": "build", "isDefault": true}, "problemMatcher": "$rustc"},
            {"label": "lint", "command": "eslint ${relativeFile}", "options": {"cwd": "web", "env": {"A": "1"}},
             "problemMatcher": ["$eslint-stylish"]},
            {"label": "quiet", "command": "true", "problemMatcher": []},
          ],
        }"#;
        let ts = parse_tasks_json(text, root, Some(Path::new("/r/src/x.ts"))).unwrap();
        assert_eq!(ts.len(), 3);
        assert_eq!(ts[0].line, "cargo build --bin \"my app\"");
        assert_eq!(ts[0].group, "build");
        assert_eq!(ts[0].matcher, Matcher::Rustc);
        assert_eq!(ts[1].line, "eslint src/x.ts");
        assert_eq!(ts[1].cwd, root.join("web"));
        assert_eq!(ts[1].env, vec![("A".into(), "1".into())]);
        assert_eq!(ts[1].matcher, Matcher::Eslint);
        assert_eq!(ts[2].matcher, Matcher::Off);
    }

    #[test]
    fn matchers_find_problems_in_real_output_shapes() {
        let d = std::env::temp_dir().join(format!("skink-tasks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        for f in ["src/main.rs", "src/a.ts", "src/b.js", "x.go", "t.py", "c.c"] {
            std::fs::write(d.join(f), "").unwrap();
        }
        let count = |text: &str, m| {
            let r = parse_problems(text, m, &d);
            let mut v: Vec<(String, usize, Severity)> = r
                .into_iter()
                .flat_map(|(p, ds)| {
                    let name = p.file_name().unwrap().to_string_lossy().into_owned();
                    ds.into_iter()
                        .map(move |x| (name.clone(), x.range.start.line + 1, x.severity))
                })
                .collect();
            v.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
            v
        };
        let cargo = "   Compiling x v0.1.0\nerror[E0308]: mismatched types\n --> src/main.rs:4:18\n  |\nwarning: unused variable: `y`\n --> src/main.rs:2:9\nerror: could not compile `x`\n";
        assert_eq!(
            count(cargo, Matcher::Rustc),
            [
                ("main.rs".into(), 2, Severity::Warning),
                ("main.rs".into(), 4, Severity::Error)
            ]
        );
        let tsc = "src/a.ts(3,5): error TS2322: Type 'x' is not assignable.\nsrc/a.ts:7:1 - warning TS6133: unused\n";
        assert_eq!(count(tsc, Matcher::Tsc).len(), 2);
        let stylish = "\n/abs/nowhere.js\n  1:1  error  nope  rule\nsrc/b.js\n  2:5  error  'x' is not defined  no-undef\n  3:1  warning  Unexpected console  no-console\n";
        assert_eq!(count(stylish, Matcher::Eslint).len(), 2);
        let go = "# example\n./x.go:10:2: undefined: foo\n";
        assert_eq!(
            count(go, Matcher::Go),
            [("x.go".into(), 10, Severity::Error)]
        );
        let py = "Traceback (most recent call last):\n  File \"t.py\", line 3, in <module>\n    f()\nValueError: bad\n";
        assert_eq!(
            count(py, Matcher::Python),
            [("t.py".into(), 3, Severity::Error)]
        );
        let gcc = "c.c:5:3: warning: unused [-Wunused]\nc.c:9:1: error: expected ';'\nhttp://example.com:80: not a file\n";
        assert_eq!(
            count(gcc, Matcher::Gcc),
            [
                ("c.c".into(), 5, Severity::Warning),
                ("c.c".into(), 9, Severity::Error)
            ]
        );
        // Auto mode runs the specific ones.
        assert_eq!(count(cargo, Matcher::Auto).len(), 2);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn detects_project_tasks() {
        let d = std::env::temp_dir().join(format!("skink-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("Cargo.toml"), "").unwrap();
        std::fs::write(
            d.join("package.json"),
            r#"{"scripts": {"build": "tsc", "test": "vitest"}}"#,
        )
        .unwrap();
        std::fs::write(d.join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(
            d.join("Makefile"),
            "VAR := 1\nall: x\n\ttouch x\n.PHONY: all\ncheck:\n%.o: %.c\n",
        )
        .unwrap();
        let labels: Vec<String> = detect(&d).into_iter().map(|t| t.label).collect();
        for want in [
            "cargo build",
            "cargo test",
            "pnpm run build",
            "pnpm run test",
            "make",
            "make all",
            "make check",
        ] {
            assert!(labels.iter().any(|l| l == want), "{want} in {labels:?}");
        }
        assert!(
            !labels.iter().any(|l| l.contains('%') || l.contains("VAR")),
            "{labels:?}"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }
}
