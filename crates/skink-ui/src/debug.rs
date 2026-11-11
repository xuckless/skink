//! The debugger in the editor: breakpoints that follow edits, the
//! session lifecycle (launch configurations, a pre-launch task, the
//! skink-dap session), step controls, jumping to the stopped line,
//! `runInTerminal` in a terminal tab, hover and inline values while
//! stopped. The panel is in `debugview.rs`.

use crate::doc::DocId;
use crate::launch::Launch;
use crate::nav::Level;
use crate::{App, Tab};
use skink_core::ByteEdit;
use skink_dap::{BpResult, BreakKind, BreakpointSpec, DebugConfig, Notice, Session, State};
use std::path::{Path, PathBuf};

/// One breakpoint as Skink shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bp {
    /// 0-based.
    pub line: usize,
    pub kind: BreakKind,
    pub state: BpState,
}

/// What the adapter said about a breakpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BpState {
    /// No session, or not answered yet.
    Pending,
    Verified,
    Rejected(String),
}

/// Which buffer revision a file's breakpoint lines match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Synced {
    Never,
    At { doc: DocId, rev: u64 },
}

#[derive(Debug, Clone)]
pub struct FileBps {
    /// Canonical path.
    pub path: PathBuf,
    pub list: Vec<Bp>,
    synced: Synced,
}

/// Where the debugger is.
pub enum Run {
    Idle,
    /// The pre-launch task is running; `launch` starts when it succeeds.
    Building {
        task: String,
        launch: Box<Launch>,
    },
    /// A session (live, or ended and kept for its console until the next).
    Active(Box<Session>),
}

pub struct Debug {
    pub run: Run,
    pub files: Vec<FileBps>,
    /// Watch expressions (kept between sessions).
    pub watches: Vec<String>,
    /// The configuration F5 starts again.
    pub last: Option<Box<Launch>>,
    pub panel: crate::docks::Dock,
}

impl Default for Debug {
    fn default() -> Self {
        Debug {
            run: Run::Idle,
            files: Vec::new(),
            watches: Vec::new(),
            last: None,
            panel: crate::docks::Dock::Closed,
        }
    }
}

impl Debug {
    pub fn session(&self) -> Option<&Session> {
        match &self.run {
            Run::Active(s) => Some(s),
            _ => None,
        }
    }

    pub fn session_mut(&mut self) -> Option<&mut Session> {
        match &mut self.run {
            Run::Active(s) => Some(s),
            _ => None,
        }
    }

    /// A session that is starting, running or stopped.
    pub fn live(&mut self) -> Option<&mut Session> {
        self.session_mut().filter(|s| s.is_live())
    }

    pub fn breakpoints_in(&self, path: &Path) -> &[Bp] {
        self.files
            .iter()
            .find(|f| f.path == path)
            .map_or(&[], |f| f.list.as_slice())
    }

    fn specs(list: &[Bp]) -> Vec<BreakpointSpec> {
        list.iter()
            .map(|b| BreakpointSpec {
                line: b.line,
                kind: b.kind.clone(),
            })
            .collect()
    }

    fn all_specs(&self) -> Vec<(PathBuf, Vec<BreakpointSpec>)> {
        self.files
            .iter()
            .filter(|f| !f.list.is_empty())
            .map(|f| (f.path.clone(), Self::specs(&f.list)))
            .collect()
    }

    /// Restore breakpoints saved with a session.
    pub fn restore(&mut self, saved: Vec<(PathBuf, usize, BreakKind)>) {
        for (path, line, kind) in saved {
            let f = self.file_mut(&path);
            if !f.list.iter().any(|b| b.line == line) {
                f.list.push(Bp {
                    line,
                    kind,
                    state: BpState::Pending,
                });
                f.list.sort_by_key(|b| b.line);
            }
        }
    }

    /// Every breakpoint, for saving.
    pub fn saved(&self) -> Vec<(PathBuf, usize, BreakKind)> {
        self.files
            .iter()
            .flat_map(|f| {
                f.list
                    .iter()
                    .map(|b| (f.path.clone(), b.line, b.kind.clone()))
            })
            .collect()
    }

    fn file_mut(&mut self, path: &Path) -> &mut FileBps {
        let i = match self.files.iter().position(|f| f.path == path) {
            Some(i) => i,
            None => {
                self.files.push(FileBps {
                    path: path.to_path_buf(),
                    list: Vec::new(),
                    synced: Synced::Never,
                });
                self.files.len() - 1
            }
        };
        &mut self.files[i]
    }
}

/// Where `line` goes after edit `e` (breakpoints and inline marks follow
/// the text they are on).
pub fn map_line(line: usize, e: &ByteEdit) -> usize {
    let (s, old, new) = (e.start.row, e.old_end.row, e.new_end.row);
    let shift = |l: usize| (l + new).saturating_sub(old);
    if line < s {
        line
    } else if line == s {
        // Text inserted at the very start of the line pushes it down.
        if e.start_byte == e.old_end_byte && e.start.col == 0 && new > s {
            shift(line)
        } else {
            line
        }
    } else if line > old {
        shift(line)
    } else {
        s // inside deleted text: onto the line the edit starts on
    }
}

/// Step controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Over,
    Into,
    Out,
}

impl App {
    // ---- breakpoints ---------------------------------------------------------

    /// Bring the current document's breakpoint lines up to date with its
    /// edits (called before drawing and before sending them).
    pub(crate) fn sync_breakpoints(&mut self) {
        let Some(path) = self.doc.canon.clone() else {
            return;
        };
        let (doc, rev) = (self.doc.id, self.doc.editor.buffer.revision());
        let Some(f) = self.debug.files.iter_mut().find(|f| f.path == path) else {
            return;
        };
        let now = Synced::At { doc, rev };
        if f.synced == now {
            return;
        }
        if let Synced::At { doc: d, rev: r } = f.synced
            && d == doc
            && let Some(edits) = self.doc.editor.buffer.edits_since(r)
        {
            for e in &edits {
                for b in &mut f.list {
                    b.line = map_line(b.line, e);
                }
            }
            let last = self.doc.editor.buffer.len_lines().saturating_sub(1);
            for b in &mut f.list {
                b.line = b.line.min(last);
            }
            f.list.sort_by_key(|b| b.line);
            f.list.dedup_by_key(|b| b.line);
        }
        f.synced = now;
    }

    /// Send a file's breakpoints to a live session (they show as pending
    /// until it answers).
    fn push_breakpoints(&mut self, path: &Path) {
        let list = self.debug.breakpoints_in(path).to_vec();
        if let Some(s) = self.debug.live() {
            s.set_breakpoints(path, Debug::specs(&list));
            if let Some(f) = self.debug.files.iter_mut().find(|f| f.path == path) {
                for b in &mut f.list {
                    b.state = BpState::Pending;
                }
            }
        }
        self.refresh_debug_panel();
    }

    /// F9 / a click in the sign column: a plain breakpoint on `line` of the
    /// current file, or none.
    pub(crate) fn toggle_breakpoint_at(&mut self, line: usize) {
        let Some(path) = self.doc.canon.clone() else {
            self.doc.editor.message = Some("Breakpoints need a file on disk".into());
            return;
        };
        self.sync_breakpoints();
        let (doc, rev) = (self.doc.id, self.doc.editor.buffer.revision());
        let f = self.debug.file_mut(&path);
        f.synced = Synced::At { doc, rev };
        match f.list.iter().position(|b| b.line == line) {
            Some(i) => {
                f.list.remove(i);
            }
            None => {
                f.list.push(Bp {
                    line,
                    kind: BreakKind::Plain,
                    state: BpState::Pending,
                });
                f.list.sort_by_key(|b| b.line);
            }
        }
        self.push_breakpoints(&path);
    }

    pub(crate) fn remove_breakpoint(&mut self, path: &Path, line: usize) {
        if let Some(f) = self.debug.files.iter_mut().find(|f| f.path == path) {
            f.list.retain(|b| b.line != line);
        }
        self.push_breakpoints(path);
    }

    pub fn toggle_breakpoint(&mut self) {
        let line = self.doc.editor.cursor.line;
        self.toggle_breakpoint_at(line);
    }

    /// `:breakpoint [if COND | hits N | log MSG | clear | list]`.
    pub(crate) fn breakpoint_command(&mut self, args: &[&str]) {
        let rest = args.get(1..).unwrap_or_default().join(" ");
        let kind = match args.first().copied() {
            None => return self.toggle_breakpoint(),
            Some("list" | "ls") => return self.open_debug_panel(),
            Some("clear") => {
                if let Some(p) = self.doc.canon.clone() {
                    self.debug.file_mut(&p).list.clear();
                    self.push_breakpoints(&p);
                }
                return;
            }
            Some("clearall") => {
                let paths: Vec<PathBuf> = self.debug.files.iter().map(|f| f.path.clone()).collect();
                self.debug.files.iter_mut().for_each(|f| f.list.clear());
                for p in paths {
                    self.push_breakpoints(&p);
                }
                return;
            }
            Some("if") if !rest.is_empty() => BreakKind::Condition(rest),
            Some("hits") if !rest.is_empty() => BreakKind::HitCount(rest),
            Some("log") if !rest.is_empty() => BreakKind::Log(rest),
            Some(_) => {
                self.doc.editor.message = Some(
                    "breakpoint [if COND | hits N | log MESSAGE {expr} | clear | clearall | list]"
                        .into(),
                );
                return;
            }
        };
        let Some(path) = self.doc.canon.clone() else {
            self.doc.editor.message = Some("Breakpoints need a file on disk".into());
            return;
        };
        self.sync_breakpoints();
        let line = self.doc.editor.cursor.line;
        let (doc, rev) = (self.doc.id, self.doc.editor.buffer.revision());
        let f = self.debug.file_mut(&path);
        f.synced = Synced::At { doc, rev };
        f.list.retain(|b| b.line != line);
        f.list.push(Bp {
            line,
            kind,
            state: BpState::Pending,
        });
        f.list.sort_by_key(|b| b.line);
        self.push_breakpoints(&path);
    }

    /// Alt-F9: edit the cursor line's breakpoint condition on the command line.
    pub fn edit_breakpoint(&mut self) {
        let line = self.doc.editor.cursor.line;
        let current = self
            .doc
            .canon
            .as_deref()
            .and_then(|p| {
                self.debug
                    .breakpoints_in(p)
                    .iter()
                    .find(|b| b.line == line)
                    .cloned()
            })
            .map(|b| match b.kind {
                BreakKind::Plain => "breakpoint if ".to_owned(),
                BreakKind::Condition(c) => format!("breakpoint if {c}"),
                BreakKind::HitCount(h) => format!("breakpoint hits {h}"),
                BreakKind::Log(m) => format!("breakpoint log {m}"),
            })
            .unwrap_or_else(|| "breakpoint if ".into());
        self.open_prompt(crate::prompt::PromptKind::Command, ":", &current);
    }

    fn apply_bp_results(&mut self, path: &Path, results: &[BpResult]) {
        let canon = crate::doc::canonical(path);
        let Some(f) = self.debug.files.iter_mut().find(|f| f.path == canon) else {
            return;
        };
        for (b, r) in f.list.iter_mut().zip(results) {
            b.state = match r {
                BpResult::Verified { line } => {
                    b.line = *line;
                    BpState::Verified
                }
                BpResult::Rejected(why) => BpState::Rejected(why.clone()),
            };
        }
        f.list.sort_by_key(|b| b.line);
        f.list.dedup_by_key(|b| b.line);
    }

    // ---- sessions ------------------------------------------------------------

    /// F5: continue when stopped; otherwise start the last configuration
    /// (or pick one).
    pub fn debug_continue(&mut self) {
        if self.refuse_in_lean("The debugger") {
            return;
        }
        let note = match &mut self.debug.run {
            Run::Active(s) if s.is_stopped() => {
                s.continue_();
                None
            }
            Run::Active(s) if s.is_live() => Some("Running (F6 pauses, Shift-F5 stops)".to_owned()),
            Run::Building { task, .. } => Some(format!("Building first: {task}")),
            Run::Active(_) | Run::Idle => {
                match self.debug.last.clone() {
                    Some(l) => self.start_debugging(*l),
                    None => self.pick_launch(),
                }
                None
            }
        };
        if let Some(n) = note {
            self.doc.editor.message = Some(n);
        }
        self.refresh_debug_panel();
    }

    /// Step keys: true when a live session took them (F10 / F11 fall back
    /// to their other actions without one).
    pub(crate) fn debug_step(&mut self, step: Step) -> bool {
        let Some(s) = self.debug.live() else {
            return false;
        };
        if !s.is_stopped() {
            self.doc.editor.message = Some("Running: F6 pauses first".into());
            return true;
        }
        match step {
            Step::Over => s.step_over(),
            Step::Into => s.step_in(),
            Step::Out => s.step_out(),
        }
        self.refresh_debug_panel();
        true
    }

    /// F6 while the program runs: pause it.
    pub(crate) fn debug_pause(&mut self) -> bool {
        match self.debug.live() {
            Some(s) if *s.state() == State::Running => {
                s.pause();
                true
            }
            _ => false,
        }
    }

    /// Shift-F5: stop the program (again: kill the adapter).
    pub fn debug_stop(&mut self) {
        match &mut self.debug.run {
            Run::Active(s) => {
                let n = s.stop();
                self.handle_debug_notices(n);
            }
            Run::Building { task, .. } => {
                self.doc.editor.message =
                    Some(format!("Debugging cancelled ({task} keeps running)"));
                self.debug.run = Run::Idle;
            }
            Run::Idle => self.doc.editor.message = Some("Not debugging".into()),
        }
        self.refresh_debug_panel();
    }

    /// `:debug` without a name: choose a configuration.
    pub(crate) fn pick_launch(&mut self) {
        let configs = self.launch_configs();
        if configs.is_empty() {
            self.doc.editor.message =
                Some("Nothing to debug here: add .vscode/launch.json or .skink/launch.json".into());
            return;
        }
        let last = self.debug.last.as_ref().map(|l| l.name.clone());
        let mut items: Vec<crate::picker::Item> = configs
            .into_iter()
            .map(|l| crate::picker::Item {
                detail: format!(
                    "{}{}",
                    l.adapter.type_name(),
                    if l.adapter.installed() {
                        String::new()
                    } else {
                        format!("  (not installed: {})", l.adapter.install_hint())
                    }
                ),
                tag: match l.origin {
                    crate::launch::Origin::File => "launch.json",
                    crate::launch::Origin::Default => "",
                },
                target: crate::picker::Target::Command(format!("debug {}", l.name)),
                label: l.name,
            })
            .collect();
        if let Some(name) = last
            && let Some(i) = items.iter().position(|i| i.label == name)
        {
            let it = items.remove(i);
            items.insert(0, it);
        }
        self.open_choices("Debug", items);
    }

    /// `:debug [NAME|stop|pause|restart|panel|eval EXPR|watch EXPR]`.
    pub(crate) fn debug_command(&mut self, args: &[&str]) {
        if self.refuse_in_lean("The debugger") {
            return;
        }
        let rest = args.get(1..).unwrap_or_default().join(" ");
        match args.first().copied() {
            None => self.pick_launch(),
            Some("stop") => self.debug_stop(),
            Some("pause") => {
                if !self.debug_pause() {
                    self.doc.editor.message = Some("Nothing running to pause".into());
                }
            }
            Some("continue" | "c") => self.debug_continue(),
            Some("restart") => {
                if let Some(s) = self.debug.live() {
                    let _ = s.stop();
                    let _ = s.stop();
                }
                match self.debug.last.clone() {
                    Some(l) => self.start_debugging(*l),
                    None => self.pick_launch(),
                }
            }
            Some("panel") => self.open_debug_panel(),
            Some("eval" | "e" | "print" | "p") if !rest.is_empty() => self.debug_eval(&rest),
            Some("watch" | "w") if !rest.is_empty() => self.add_watch(&rest),
            Some(_) => {
                let name = args.join(" ");
                match self.launch_configs().into_iter().find(|l| l.name == name) {
                    Some(l) => self.start_debugging(l),
                    None => {
                        self.doc.editor.message = Some(format!(
                            "No debug configuration {name:?} (:debug lists them)"
                        ))
                    }
                }
            }
        }
    }

    /// Run the pre-launch task first when there is one.
    pub(crate) fn start_debugging(&mut self, l: Launch) {
        if let Some(s) = self.debug.live() {
            let _ = s.stop();
            let _ = s.stop();
        }
        self.debug.last = Some(Box::new(l.clone()));
        let Some(task_name) = l.pre_launch_task.clone() else {
            return self.launch_now(l);
        };
        let task = self
            .all_tasks()
            .into_iter()
            .find(|t| t.label == task_name || t.line == task_name);
        match task {
            Some(t) => {
                self.debug.run = Run::Building {
                    task: t.label.clone(),
                    launch: Box::new(l),
                };
                let back = self.cur_doc;
                self.run_task(t);
                self.switch_to(back); // the build shows in its tab; stay here
            }
            None => self.toast(
                Level::Error,
                format!("{}: no task {task_name:?} (preLaunchTask)", l.name),
            ),
        }
    }

    /// A task ended: start the debug session that waited for it.
    pub(crate) fn debug_task_finished(&mut self, label: &str, ok: bool) {
        let Run::Building { task, .. } = &self.debug.run else {
            return;
        };
        if task != label {
            return;
        }
        let Run::Building { launch, .. } = std::mem::replace(&mut self.debug.run, Run::Idle) else {
            return;
        };
        if ok {
            self.launch_now(*launch);
        } else {
            self.toast(Level::Error, format!("Not debugging: {label} failed"));
        }
    }

    fn launch_now(&mut self, l: Launch) {
        let mut process = match l.adapter.process() {
            Ok(p) => p,
            Err(e) => {
                self.debug.run = Run::Idle;
                return self.toast(Level::Error, format!("{}: {e}", l.name));
            }
        };
        let root = self.project_root();
        process.cwd = Some(root);
        let config = DebugConfig {
            name: l.name.clone(),
            adapter_id: l.adapter.id().to_owned(),
            adapter: process,
            request: l.request,
            arguments: l.arguments.clone(),
        };
        let wake = self
            .wake
            .clone()
            .unwrap_or_else(|| std::sync::Arc::new(|| {}));
        let bps = self.debug.all_specs();
        let watches = self.debug.watches.clone();
        match Session::start(config, bps, &watches, wake) {
            Ok(s) => self.begin_session(s),
            Err(e) => {
                self.debug.run = Run::Idle;
                self.toast(Level::Error, format!("{}: {e}", l.name));
            }
        }
    }

    /// Show a started session (also used by tests with a fake adapter).
    pub(crate) fn begin_session(&mut self, s: Session) {
        let name = s.name.clone();
        for f in &mut self.debug.files {
            for b in &mut f.list {
                b.state = BpState::Pending;
            }
        }
        self.debug.run = Run::Active(Box::new(s));
        self.doc.editor.message = Some(format!("Debugging {name}"));
        self.open_debug_panel();
    }

    /// Handle what the adapter sent (called when the loop wakes).
    pub(crate) fn poll_debug(&mut self) {
        let Some(s) = self.debug.session_mut() else {
            return;
        };
        let n = s.pump();
        if !n.is_empty() {
            self.handle_debug_notices(n);
        }
    }

    pub(crate) fn handle_debug_notices(&mut self, notices: Vec<Notice>) {
        let mut changed = false;
        for n in notices {
            match n {
                Notice::Changed => changed = true,
                Notice::Location { path, line } => self.show_stop(&path, line),
                Notice::Breakpoints { path, results } => self.apply_bp_results(&path, &results),
                Notice::Breakpoint {
                    path,
                    index,
                    result,
                } => {
                    let canon = crate::doc::canonical(&path);
                    if let Some(b) = self
                        .debug
                        .files
                        .iter_mut()
                        .find(|f| f.path == canon)
                        .and_then(|f| f.list.get_mut(index))
                    {
                        b.state = match result {
                            BpResult::Verified { line } => {
                                b.line = line;
                                BpState::Verified
                            }
                            BpResult::Rejected(w) => BpState::Rejected(w),
                        };
                    }
                }
                Notice::RunInTerminal {
                    seq,
                    title,
                    args,
                    cwd,
                    env,
                } => self.run_debuggee(seq, title, args, cwd, env),
                Notice::Hover(r) => {
                    let lines = match r {
                        Ok(v) => v.lines().map(str::to_owned).collect(),
                        Err(e) => vec![e],
                    };
                    self.intel.popup = crate::intel::Popup::Info(crate::intel::Info {
                        kind: crate::intel::InfoKind::Hover,
                        lines,
                    });
                }
                Notice::Ended => {
                    let what = match self.debug.session().map(Session::state) {
                        Some(State::Exited(Some(c))) => format!("Program exited with code {c}"),
                        Some(State::Exited(None)) => "Debug session ended".into(),
                        Some(State::Failed(e)) => format!("Debugging failed: {e}"),
                        _ => "Debug session ended".into(),
                    };
                    let level = match self.debug.session().map(Session::state) {
                        Some(State::Failed(_)) => Level::Error,
                        _ => Level::Info,
                    };
                    // A "Running (F6 pauses…)" note from before is stale now;
                    // the toast says what happened.
                    self.doc.editor.message = None;
                    self.toast(level, what);
                }
            }
        }
        if changed {
            self.refresh_debug_panel();
        }
    }

    /// Stopped (or a frame was chosen): show the line in an editor pane.
    fn show_stop(&mut self, path: &Path, line: usize) {
        self.leave_debug_panel();
        let col = self.doc_line_indent(path, line);
        self.jump(path, line, col);
    }

    fn doc_line_indent(&self, path: &Path, line: usize) -> usize {
        if self.doc.canon.as_deref() == Some(&crate::doc::canonical(path)) {
            let t = self.doc.editor.buffer.line_text(line);
            return t.chars().take_while(|c| c.is_whitespace()).count();
        }
        0
    }

    /// `runInTerminal`: the program runs in a terminal tab.
    fn run_debuggee(
        &mut self,
        seq: i64,
        title: String,
        args: Vec<String>,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
    ) {
        let Some((program, rest)) = args.split_first() else {
            if let Some(s) = self.debug.session_mut() {
                s.ran_in_terminal(seq, Err("nothing to run".into()));
            }
            return;
        };
        let mut spec = skink_term::CommandSpec::new(program).args(rest.iter().map(String::as_str));
        if let Some(c) = cwd.filter(|c| c.is_dir()) {
            spec = spec.cwd(c);
        }
        for (k, v) in &env {
            spec = spec.env(k, v);
        }
        let back = self.cur_doc;
        let result = match self.spawn_terminal_tab(spec) {
            Ok(i) => {
                let pid = match &mut self.tabs[i] {
                    Tab::Term(p) => {
                        p.label = format!("debug: {title}");
                        p.term.process_id()
                    }
                    Tab::Doc(_) => None,
                };
                self.switch_to(back);
                Ok(pid)
            }
            Err(e) => Err(e.to_string()),
        };
        if let Some(s) = self.debug.session_mut() {
            s.ran_in_terminal(seq, result);
        }
    }

    // ---- evaluation ------------------------------------------------------------

    pub(crate) fn debug_eval(&mut self, expr: &str) {
        match self.debug.session_mut() {
            Some(s) if s.is_live() => s.repl(expr),
            _ => self.doc.editor.message = Some("Not debugging".into()),
        }
        self.refresh_debug_panel();
    }

    pub(crate) fn add_watch(&mut self, expr: &str) {
        let expr = expr.trim();
        if expr.is_empty() || self.debug.watches.iter().any(|w| w == expr) {
            return;
        }
        self.debug.watches.push(expr.to_owned());
        if let Some(s) = self.debug.session_mut() {
            s.add_watch(expr);
        }
        self.refresh_debug_panel();
    }

    pub(crate) fn remove_watch(&mut self, i: usize) {
        if i < self.debug.watches.len() {
            self.debug.watches.remove(i);
            if let Some(s) = self.debug.session_mut() {
                s.remove_watch(i);
            }
        }
        self.refresh_debug_panel();
    }

    /// Alt-K while stopped: the debugger's value of the word (or selection)
    /// at the cursor. False when not stopped (hover asks the language server).
    pub(crate) fn debug_hover(&mut self) -> bool {
        if !self.debug.session().is_some_and(Session::is_stopped) {
            return false;
        }
        let ed = &self.doc.editor;
        let expr = match ed.selection() {
            Some((s, e)) if e > s && e - s < 200 => {
                (s..e).filter_map(|i| ed.buffer.char_at(i)).collect()
            }
            _ => {
                let is_word = |c: char| c.is_alphanumeric() || c == '_';
                let mut s = ed.cursor_idx();
                while s > 0 && ed.buffer.char_at(s - 1).is_some_and(is_word) {
                    s -= 1;
                }
                let mut e = ed.cursor_idx();
                while ed.buffer.char_at(e).is_some_and(is_word) {
                    e += 1;
                }
                (s..e)
                    .filter_map(|i| ed.buffer.char_at(i))
                    .collect::<String>()
            }
        };
        if expr.trim().is_empty() {
            return false;
        }
        self.debug
            .session_mut()
            .is_some_and(|s| s.hover(expr.trim()))
    }

    /// The stopped frame's line in the current document, if it is shown.
    pub(crate) fn stopped_line(&self) -> Option<usize> {
        let f = self.debug.session()?.current_frame()?;
        let p = crate::doc::canonical(Path::new(f.path()?));
        (self.doc.canon.as_deref() == Some(&p)).then(|| (f.line.max(1) - 1) as usize)
    }

    /// Inline values for line `line` of the current document while stopped
    /// in it: loaded locals whose names appear on the line, up to the
    /// stopped line and a screenful above it.
    pub(crate) fn inline_values(&self, line: usize, text: &str) -> Option<String> {
        const ABOVE: usize = 60;
        let stop = self.stopped_line()?;
        if line > stop || stop - line > ABOVE {
            return None;
        }
        let locals = self.debug.session()?.locals();
        let words: Vec<&str> = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|w| !w.is_empty())
            .collect();
        let mut parts: Vec<String> = Vec::new();
        for (name, value) in locals {
            if parts.len() == 4 {
                break;
            }
            if words.contains(&name) {
                let v: String = value.chars().take(40).collect();
                let v = if value.chars().count() > 40 {
                    format!("{v}…")
                } else {
                    v
                };
                parts.push(format!("{name} = {v}"));
            }
        }
        (!parts.is_empty()).then(|| format!("  {}", parts.join(", ")))
    }

    /// Status line text and colour while debugging.
    pub(crate) fn debug_status(&self) -> Option<(String, (u8, u8, u8))> {
        let t = &self.theme;
        Some(match &self.debug.run {
            Run::Idle => return None,
            Run::Building { task, .. } => (format!("  \u{2699} building ({task})"), t.yellow),
            Run::Active(s) => match s.state() {
                State::Starting(_) => (format!("  \u{25b6} starting {}", s.name), t.yellow),
                State::Running => (format!("  \u{25b6} running {}", s.name), t.green),
                State::Stopped(stop) => {
                    let place = s
                        .current_frame()
                        .map(|f| {
                            let file = f
                                .path()
                                .and_then(|p| Path::new(p).file_name())
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| f.name.clone());
                            format!(" at {file}:{}", f.line)
                        })
                        .unwrap_or_default();
                    (
                        format!("  \u{23f8} paused: {}{place}", stop.reason),
                        t.orange,
                    )
                }
                State::Exited(Some(c)) => (format!("  \u{25a0} exited {c}"), t.comment),
                State::Exited(None) => ("  \u{25a0} ended".into(), t.comment),
                State::Failed(e) => {
                    let e: String = e.chars().take(60).collect();
                    (format!("  \u{2716} debug failed: {e}"), t.red)
                }
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skink_core::TextPoint;

    fn edit(sr: usize, sc: usize, or: usize, nr: usize, insert: bool) -> ByteEdit {
        ByteEdit {
            start_byte: 10,
            old_end_byte: if insert { 10 } else { 20 },
            new_end_byte: 30,
            start: TextPoint { row: sr, col: sc },
            old_end: TextPoint { row: or, col: 0 },
            new_end: TextPoint { row: nr, col: 0 },
        }
    }

    #[test]
    fn breakpoint_lines_follow_inserted_and_deleted_lines() {
        // Two lines typed above line 5.
        assert_eq!(map_line(5, &edit(2, 3, 2, 4, true)), 7);
        // A newline typed at the very start of line 5 pushes it down.
        assert_eq!(map_line(5, &edit(5, 0, 5, 6, true)), 6);
        // Typing inside line 5 keeps it there.
        assert_eq!(map_line(5, &edit(5, 4, 5, 5, true)), 5);
        // Lines 3..=6 deleted: 5 lands on 3, 8 moves up.
        assert_eq!(map_line(5, &edit(3, 0, 6, 3, false)), 3);
        assert_eq!(map_line(8, &edit(3, 0, 6, 3, false)), 5);
        // Edits below do nothing.
        assert_eq!(map_line(1, &edit(3, 0, 6, 3, false)), 1);
    }
}
