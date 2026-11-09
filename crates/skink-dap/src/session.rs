//! One debug session: the protocol's start-up dance, the stop / step
//! cycle and the data a debugger UI shows (frames, a lazily expanded
//! variable tree, watches, a console). Headless: the UI calls
//! [`Session::pump`] when the wake callback fires and reads the fields.
//!
//! Start-up order (what VS Code does, which every adapter expects):
//! `initialize` → response → `launch`/`attach` → `initialized` event →
//! `setBreakpoints` per file (+ `setExceptionBreakpoints`) →
//! `configurationDone`. Many adapters answer `launch` only after
//! `configurationDone`, so nothing waits for that response.

use crate::Wake;
use crate::client::{AdapterProcess, Client, Incoming};
use crate::types::{Breakpoint, Capabilities, Scope, StackFrame, Thread, Variable};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

/// Lines the console keeps.
const CONSOLE_MAX: usize = 2000;
/// Frames asked for per stop.
const FRAMES: i64 = 64;
/// Scopes loaded right away on a stop (Locals, Arguments...).
const AUTO_SCOPES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Launch,
    Attach,
}

/// What to debug: the adapter and the `launch` / `attach` arguments.
#[derive(Debug, Clone)]
pub struct DebugConfig {
    pub name: String,
    /// `initialize.adapterID` (`gdb`, `lldb`, `debugpy`, `go`...).
    pub adapter_id: String,
    pub adapter: AdapterProcess,
    pub request: RequestKind,
    /// Sent as is (variables already substituted).
    pub arguments: Value,
}

/// Where the session is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Starting(Phase),
    Running,
    Stopped(Stop),
    Exited(Option<i64>),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// `initialize` sent.
    Initializing,
    /// `launch` sent; waiting for the `initialized` event.
    Launching,
    /// Breakpoints sent; `configurationDone` goes when they are answered.
    Configuring { left: usize },
    /// `configurationDone` sent.
    Finishing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    pub thread: i64,
    pub reason: String,
    pub text: Option<String>,
}

/// One breakpoint as the user set it (0-based line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakpointSpec {
    pub line: usize,
    pub kind: BreakKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakKind {
    Plain,
    /// Stop when the expression is true.
    Condition(String),
    /// Stop when the hit count matches (`5`, `>= 3`, `% 2`).
    HitCount(String),
    /// Log the message (`{expr}` interpolated) instead of stopping.
    Log(String),
}

/// What the adapter made of one breakpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BpResult {
    /// Bound, possibly moved to another (0-based) line.
    Verified {
        line: usize,
    },
    Rejected(String),
}

/// What the UI should do something about.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// Redraw.
    Changed,
    /// Stopped (or another frame selected) here: show it.
    Location {
        path: PathBuf,
        line: usize,
    },
    /// Answers for a file's breakpoints, in the order they were sent.
    Breakpoints {
        path: PathBuf,
        results: Vec<BpResult>,
    },
    /// One breakpoint changed later (the adapter's `breakpoint` event).
    Breakpoint {
        path: PathBuf,
        index: usize,
        result: BpResult,
    },
    /// Run the program in a terminal and call [`Session::ran_in_terminal`].
    RunInTerminal {
        seq: i64,
        title: String,
        args: Vec<String>,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
    },
    Hover(Result<String, String>),
    /// The session is over (exited, terminated or failed).
    Ended,
}

/// A variable (or scope) and its children, loaded on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarNode {
    pub name: String,
    pub value: String,
    pub ty: Option<String>,
    /// `variablesReference`: > 0 when it has children.
    pub reference: i64,
    pub children: Children,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Children {
    Leaf,
    Collapsed,
    Loading,
    Expanded(Vec<VarNode>),
}

impl VarNode {
    fn new(name: String, value: String, ty: Option<String>, reference: i64) -> VarNode {
        VarNode {
            name,
            value,
            ty,
            reference,
            children: if reference > 0 {
                Children::Collapsed
            } else {
                Children::Leaf
            },
        }
    }

    fn from_var(v: Variable) -> VarNode {
        VarNode::new(v.name, v.value, v.ty, v.variables_reference)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    pub expr: String,
    pub value: WatchValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchValue {
    /// Not stopped yet.
    Unknown,
    Pending,
    Value(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Program or adapter output (`stdout`, `console`).
    Output,
    Stderr,
    /// Typed in the console.
    Input,
    Result,
    Error,
    /// Skink's own notes (stopped, exited).
    Info,
}

/// Whether the console's last line still takes more output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tail {
    Closed,
    Open(LineKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    pub kind: LineKind,
    pub text: String,
}

/// What a sent request was for.
#[derive(Debug, Clone)]
enum Pending {
    Initialize,
    Launch,
    SetBreakpoints(PathBuf),
    ExceptionBreakpoints,
    ConfigurationDone,
    Threads,
    StackTrace,
    Scopes { frame: i64 },
    Variables(i64),
    Evaluate(EvalFor),
    Control(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvalFor {
    Watch(usize),
    Hover,
    Repl,
}

pub struct Session {
    client: Client,
    pub name: String,
    state: State,
    caps: Capabilities,
    request: RequestKind,
    arguments: Value,
    /// Breakpoints to send on `initialized`, then as last sent per file.
    breakpoints: Vec<(PathBuf, Vec<BreakpointSpec>)>,
    /// Adapter ids of the last sent breakpoints, per file, by position.
    bp_ids: HashMap<PathBuf, Vec<Option<i64>>>,
    pending: HashMap<i64, Pending>,
    exit_code: Option<i64>,
    /// The thread controls apply to while running (last stopped one).
    thread: i64,
    pub frames: Vec<StackFrame>,
    /// Index into `frames` of the frame shown.
    pub frame: usize,
    /// Scopes of the shown frame as the tree's roots.
    pub scopes: Vec<VarNode>,
    pub watches: Vec<Watch>,
    pub console: VecDeque<ConsoleLine>,
    tail: Tail,
}

impl Session {
    /// Start the adapter for `config` and begin the handshake.
    pub fn start(
        config: DebugConfig,
        breakpoints: Vec<(PathBuf, Vec<BreakpointSpec>)>,
        watches: &[String],
        wake: Wake,
    ) -> Result<Session, String> {
        let client = Client::spawn(&config.adapter, wake).map_err(|e| e.to_string())?;
        Ok(Session::with_client(client, config, breakpoints, watches))
    }

    /// Like [`start`](Self::start) over an existing connection (tests).
    pub fn with_client(
        client: Client,
        config: DebugConfig,
        breakpoints: Vec<(PathBuf, Vec<BreakpointSpec>)>,
        watches: &[String],
    ) -> Session {
        let mut s = Session {
            client,
            name: config.name,
            state: State::Starting(Phase::Initializing),
            caps: Capabilities::default(),
            request: config.request,
            arguments: config.arguments,
            breakpoints,
            bp_ids: HashMap::new(),
            pending: HashMap::new(),
            exit_code: None,
            thread: 1,
            frames: Vec::new(),
            frame: 0,
            scopes: Vec::new(),
            watches: watches
                .iter()
                .map(|e| Watch {
                    expr: e.clone(),
                    value: WatchValue::Unknown,
                })
                .collect(),
            console: VecDeque::new(),
            tail: Tail::Closed,
        };
        s.send(
            "initialize",
            json!({
                "clientID": "skink",
                "clientName": "Skink",
                "adapterID": config.adapter_id,
                "pathFormat": "path",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "supportsVariableType": true,
                "supportsRunInTerminalRequest": true,
                "locale": "en",
            }),
            Pending::Initialize,
        );
        s
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Running, stopped or starting: controls make sense.
    pub fn is_live(&self) -> bool {
        matches!(
            self.state,
            State::Starting(_) | State::Running | State::Stopped(_)
        )
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self.state, State::Stopped(_))
    }

    /// The frame shown, while stopped.
    pub fn current_frame(&self) -> Option<&StackFrame> {
        match self.state {
            State::Stopped(_) => self.frames.get(self.frame),
            _ => None,
        }
    }

    fn send(&mut self, command: &str, args: Value, p: Pending) {
        let seq = self.client.request(command, args);
        self.pending.insert(seq, p);
    }

    fn note(&mut self, kind: LineKind, text: impl Into<String>) {
        let text: String = text.into();
        let mut pieces = text.split_inclusive('\n').peekable();
        while let Some(piece) = pieces.next() {
            let line = piece.trim_end_matches(['\n', '\r']);
            match (self.tail, self.console.back_mut()) {
                // Output arrives in pieces: finish the open line first.
                (Tail::Open(k), Some(last)) if k == kind => last.text.push_str(line),
                _ => self.console.push_back(ConsoleLine {
                    kind,
                    text: line.to_owned(),
                }),
            }
            let unfinished = pieces.peek().is_none()
                && !piece.ends_with('\n')
                && matches!(kind, LineKind::Output | LineKind::Stderr);
            self.tail = if unfinished {
                Tail::Open(kind)
            } else {
                Tail::Closed
            };
        }
        while self.console.len() > CONSOLE_MAX {
            self.console.pop_front();
        }
    }

    /// Handle everything the adapter sent since the last call.
    pub fn pump(&mut self) -> Vec<Notice> {
        let mut out = Vec::new();
        for msg in self.client.drain() {
            match msg {
                Incoming::Response {
                    request_seq,
                    success,
                    message,
                    body,
                } => {
                    if let Some(p) = self.pending.remove(&request_seq) {
                        self.on_response(p, success, message, body, &mut out);
                    }
                }
                Incoming::Event { event, body } => self.on_event(&event, body, &mut out),
                Incoming::Request {
                    seq,
                    command,
                    arguments,
                } => self.on_reverse(seq, &command, arguments, &mut out),
                Incoming::Stderr(line) => self.note(LineKind::Stderr, line + "\n"),
                Incoming::Closed(why) => {
                    if self.is_live() {
                        self.state = match self.state {
                            // Gone after `terminated`/`exited`: a normal end.
                            _ if self.exit_code.is_some() => State::Exited(self.exit_code),
                            State::Starting(_) => State::Failed(why.clone()),
                            _ => State::Exited(self.exit_code),
                        };
                        self.note(LineKind::Info, format!("{why}\n"));
                        out.push(Notice::Ended);
                    }
                }
            }
            out.push(Notice::Changed);
        }
        out.dedup();
        out
    }

    fn on_response(
        &mut self,
        p: Pending,
        ok: bool,
        message: Option<String>,
        body: Value,
        out: &mut Vec<Notice>,
    ) {
        let msg = || message.clone().unwrap_or_else(|| "request failed".into());
        match p {
            Pending::Initialize if ok => {
                self.caps = serde_json::from_value(body).unwrap_or_default();
                let cmd = match self.request {
                    RequestKind::Launch => "launch",
                    RequestKind::Attach => "attach",
                };
                let args = self.arguments.clone();
                self.send(cmd, args, Pending::Launch);
                self.state = State::Starting(Phase::Launching);
            }
            Pending::Initialize | Pending::Launch if !ok => self.fail(msg(), out),
            Pending::Initialize => {}
            Pending::Launch => {}
            Pending::SetBreakpoints(path) => {
                let bps: Vec<Breakpoint> = body
                    .get("breakpoints")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                let sent = self
                    .breakpoints
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map_or(0, |(_, l)| l.len());
                let results: Vec<BpResult> = (0..sent)
                    .map(|i| match (ok, bps.get(i)) {
                        (false, _) => BpResult::Rejected(msg()),
                        (true, Some(b)) => result_of(b, self.line_sent(&path, i)),
                        (true, None) => BpResult::Rejected("no answer from the adapter".into()),
                    })
                    .collect();
                self.bp_ids
                    .insert(path.clone(), bps.iter().map(|b| b.id).collect());
                out.push(Notice::Breakpoints { path, results });
                if let State::Starting(Phase::Configuring { left }) = self.state {
                    self.configured_one(left);
                }
            }
            Pending::ExceptionBreakpoints => {
                if let State::Starting(Phase::Configuring { left }) = self.state {
                    self.configured_one(left);
                }
            }
            Pending::ConfigurationDone => {
                if !ok {
                    return self.fail(msg(), out);
                }
                if self.state == State::Starting(Phase::Finishing) {
                    self.state = State::Running;
                }
            }
            Pending::Threads => {
                let threads: Vec<Thread> = body
                    .get("threads")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                if let (State::Stopped(stop), Some(t)) = (&mut self.state, threads.first()) {
                    stop.thread = t.id;
                    self.thread = t.id;
                    self.ask_stack();
                }
            }
            Pending::StackTrace => {
                if !self.is_stopped() {
                    return;
                }
                self.frames = body
                    .get("stackFrames")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                // The first frame with source is the interesting one.
                self.frame = self
                    .frames
                    .iter()
                    .position(|f| f.path().is_some())
                    .unwrap_or(0);
                self.load_frame(out);
            }
            Pending::Scopes { frame } => {
                if self.current_frame().map(|f| f.id) != Some(frame) {
                    return; // another frame was selected meanwhile
                }
                let scopes: Vec<Scope> = body
                    .get("scopes")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                self.scopes = scopes
                    .iter()
                    .map(|s| {
                        VarNode::new(s.name.clone(), String::new(), None, s.variables_reference)
                    })
                    .collect();
                let auto: Vec<usize> = scopes
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.expensive && s.variables_reference > 0)
                    .map(|(i, _)| i)
                    .take(AUTO_SCOPES)
                    .collect();
                for i in auto {
                    self.expand(&[i]);
                }
            }
            Pending::Variables(reference) => {
                let vars: Vec<Variable> = body
                    .get("variables")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                let kids: Vec<VarNode> = vars.into_iter().map(VarNode::from_var).collect();
                fill(&mut self.scopes, reference, &kids, ok);
            }
            Pending::Evaluate(what) => {
                let value = if ok {
                    Ok(body
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned())
                } else {
                    Err(msg())
                };
                match what {
                    EvalFor::Watch(i) => {
                        if let Some(w) = self.watches.get_mut(i) {
                            w.value = match value {
                                Ok(v) => WatchValue::Value(v),
                                Err(e) => WatchValue::Error(e),
                            };
                        }
                    }
                    EvalFor::Hover => out.push(Notice::Hover(value)),
                    EvalFor::Repl => match value {
                        Ok(v) => self.note(LineKind::Result, v + "\n"),
                        Err(e) => self.note(LineKind::Error, e + "\n"),
                    },
                }
            }
            Pending::Control(what) if !ok => {
                self.note(LineKind::Error, format!("{what}: {}\n", msg()));
            }
            Pending::Control(_) => {}
        }
    }

    /// 0-based line of the `i`th breakpoint last sent for `path`.
    fn line_sent(&self, path: &Path, i: usize) -> usize {
        self.breakpoints
            .iter()
            .find(|(p, _)| p == path)
            .and_then(|(_, l)| l.get(i))
            .map_or(0, |b| b.line)
    }

    fn configured_one(&mut self, left: usize) {
        match left.saturating_sub(1) {
            0 => self.configuration_done(),
            left => self.state = State::Starting(Phase::Configuring { left }),
        }
    }

    fn configuration_done(&mut self) {
        if self.caps.supports_configuration_done_request {
            self.send("configurationDone", json!({}), Pending::ConfigurationDone);
            self.state = State::Starting(Phase::Finishing);
        } else {
            self.state = State::Running;
        }
    }

    fn fail(&mut self, why: String, out: &mut Vec<Notice>) {
        self.note(LineKind::Error, format!("{why}\n"));
        self.state = State::Failed(why);
        self.client.kill();
        out.push(Notice::Ended);
    }

    fn on_event(&mut self, event: &str, body: Value, out: &mut Vec<Notice>) {
        let s = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_owned);
        match event {
            "initialized" => {
                let files = self.breakpoints.clone();
                let exceptions = !self.caps.exception_breakpoint_filters.is_empty();
                let left = files.len() + usize::from(exceptions);
                for (path, list) in files {
                    self.send_breakpoints(&path, &list);
                }
                if exceptions {
                    self.send(
                        "setExceptionBreakpoints",
                        json!({"filters": []}),
                        Pending::ExceptionBreakpoints,
                    );
                }
                match left {
                    0 => self.configuration_done(),
                    left => self.state = State::Starting(Phase::Configuring { left }),
                }
            }
            "stopped" => {
                let thread = body.get("threadId").and_then(Value::as_i64);
                let reason = s("reason").unwrap_or_else(|| "paused".into());
                let text = s("description").or_else(|| s("text"));
                self.note(
                    LineKind::Info,
                    format!(
                        "Stopped: {reason}{}\n",
                        text.as_deref()
                            .map(|t| format!(" ({t})"))
                            .unwrap_or_default()
                    ),
                );
                self.state = State::Stopped(Stop {
                    thread: thread.unwrap_or(self.thread),
                    reason,
                    text,
                });
                self.frames.clear();
                self.scopes.clear();
                match thread {
                    Some(t) => {
                        self.thread = t;
                        self.ask_stack();
                    }
                    None => self.send("threads", json!({}), Pending::Threads),
                }
            }
            "continued" => self.resumed(),
            "exited" => {
                self.exit_code = body.get("exitCode").and_then(Value::as_i64);
                if let Some(c) = self.exit_code {
                    self.note(LineKind::Info, format!("Program exited with code {c}\n"));
                }
            }
            "terminated" => {
                if self.is_live() {
                    self.state = State::Exited(self.exit_code);
                    self.send(
                        "disconnect",
                        json!({"terminateDebuggee": true}),
                        Pending::Control("disconnect"),
                    );
                    out.push(Notice::Ended);
                }
            }
            "output" => {
                let cat = s("category").unwrap_or_default();
                let text = s("output").unwrap_or_default();
                match cat.as_str() {
                    "telemetry" => {}
                    "stderr" => self.note(LineKind::Stderr, text),
                    _ => self.note(LineKind::Output, text),
                }
            }
            "breakpoint" => {
                let Ok(b) = serde_json::from_value::<Breakpoint>(
                    body.get("breakpoint").cloned().unwrap_or_default(),
                ) else {
                    return;
                };
                let Some(id) = b.id else { return };
                let hit = self.bp_ids.iter().find_map(|(p, ids)| {
                    ids.iter()
                        .position(|&x| x == Some(id))
                        .map(|i| (p.clone(), i))
                });
                if let Some((path, index)) = hit {
                    let line = self.line_sent(&path, index);
                    out.push(Notice::Breakpoint {
                        path,
                        index,
                        result: result_of(&b, line),
                    });
                }
            }
            _ => {}
        }
    }

    fn on_reverse(&mut self, seq: i64, command: &str, args: Value, out: &mut Vec<Notice>) {
        match command {
            "runInTerminal" => {
                let strs = |k: &str| -> Vec<String> {
                    args.get(k)
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let env = args
                    .get("env")
                    .and_then(Value::as_object)
                    .map(|m| {
                        m.iter()
                            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(Notice::RunInTerminal {
                    seq,
                    title: args
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("debuggee")
                        .to_owned(),
                    args: strs("args"),
                    cwd: args.get("cwd").and_then(Value::as_str).map(PathBuf::from),
                    env,
                });
            }
            other => self.client.respond(
                seq,
                other,
                Err(format!("Skink does not support the {other} request")),
            ),
        }
    }

    /// The UI started (or failed to start) a `runInTerminal` program.
    pub fn ran_in_terminal(&mut self, seq: i64, pid: Result<Option<u32>, String>) {
        let body = pid.map(|p| match p {
            Some(pid) => json!({ "processId": pid }),
            None => json!({}),
        });
        self.client.respond(seq, "runInTerminal", body);
    }

    fn ask_stack(&mut self) {
        let thread = self.thread;
        self.send(
            "stackTrace",
            json!({"threadId": thread, "startFrame": 0, "levels": FRAMES}),
            Pending::StackTrace,
        );
    }

    /// Scopes and watches for the shown frame; tell the UI where it is.
    fn load_frame(&mut self, out: &mut Vec<Notice>) {
        let Some(f) = self.frames.get(self.frame).cloned() else {
            return;
        };
        self.scopes.clear();
        self.send(
            "scopes",
            json!({"frameId": f.id}),
            Pending::Scopes { frame: f.id },
        );
        for i in 0..self.watches.len() {
            self.eval_watch(i);
        }
        if let Some(p) = f.path() {
            out.push(Notice::Location {
                path: PathBuf::from(p),
                line: (f.line.max(1) - 1) as usize,
            });
        }
    }

    fn resumed(&mut self) {
        if self.is_live() {
            self.state = State::Running;
        }
        self.frames.clear();
        self.scopes.clear();
        for w in &mut self.watches {
            w.value = WatchValue::Unknown;
        }
    }

    // ---- controls ----------------------------------------------------------

    fn control(&mut self, command: &'static str) {
        let thread = self.thread;
        self.send(
            command,
            json!({"threadId": thread}),
            Pending::Control(command),
        );
    }

    pub fn continue_(&mut self) {
        if self.is_stopped() {
            self.control("continue");
            self.resumed();
        }
    }

    pub fn step_over(&mut self) {
        if self.is_stopped() {
            self.control("next");
            self.resumed();
        }
    }

    pub fn step_in(&mut self) {
        if self.is_stopped() {
            self.control("stepIn");
            self.resumed();
        }
    }

    pub fn step_out(&mut self) {
        if self.is_stopped() {
            self.control("stepOut");
            self.resumed();
        }
    }

    pub fn pause(&mut self) {
        if self.state == State::Running {
            self.control("pause");
        }
    }

    /// Ask the program to end. A second call kills the adapter.
    pub fn stop(&mut self) -> Vec<Notice> {
        let asked = self
            .pending
            .values()
            .any(|p| matches!(p, Pending::Control("terminate" | "disconnect")));
        if !self.is_live() {
            return Vec::new();
        }
        if asked || matches!(self.state, State::Starting(_)) {
            self.client.kill();
            self.state = State::Exited(self.exit_code);
            self.note(LineKind::Info, "Debug adapter stopped\n");
            return vec![Notice::Ended, Notice::Changed];
        }
        if self.caps.supports_terminate_request {
            self.send("terminate", json!({}), Pending::Control("terminate"));
        } else {
            self.send(
                "disconnect",
                json!({"terminateDebuggee": true}),
                Pending::Control("disconnect"),
            );
        }
        Vec::new()
    }

    /// Replace the breakpoints of one file (sent at once when configured).
    pub fn set_breakpoints(&mut self, path: &Path, list: Vec<BreakpointSpec>) {
        match self.breakpoints.iter_mut().find(|(p, _)| p == path) {
            Some((_, l)) => *l = list.clone(),
            None => self.breakpoints.push((path.to_path_buf(), list.clone())),
        }
        if matches!(self.state, State::Running | State::Stopped(_))
            || matches!(self.state, State::Starting(Phase::Finishing))
        {
            self.send_breakpoints(path, &list);
        }
    }

    fn send_breakpoints(&mut self, path: &Path, list: &[BreakpointSpec]) {
        let bps: Vec<Value> = list
            .iter()
            .map(|b| {
                let mut v = json!({"line": b.line + 1});
                match &b.kind {
                    BreakKind::Plain => {}
                    BreakKind::Condition(c) => v["condition"] = json!(c),
                    BreakKind::HitCount(h) => v["hitCondition"] = json!(h),
                    BreakKind::Log(m) => v["logMessage"] = json!(m),
                }
                v
            })
            .collect();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        self.send(
            "setBreakpoints",
            json!({
                "source": {"name": name, "path": path.to_string_lossy()},
                "breakpoints": bps,
                "lines": list.iter().map(|b| b.line + 1).collect::<Vec<_>>(),
            }),
            Pending::SetBreakpoints(path.to_path_buf()),
        );
    }

    /// Show frame `i` of the call stack.
    pub fn select_frame(&mut self, i: usize) -> Vec<Notice> {
        let mut out = Vec::new();
        if self.is_stopped() && i < self.frames.len() {
            self.frame = i;
            self.load_frame(&mut out);
        }
        out
    }

    /// Expand or collapse the tree node at `path` (indices from the scopes).
    pub fn toggle(&mut self, path: &[usize]) {
        let Some(node) = node_mut(&mut self.scopes, path) else {
            return;
        };
        match node.children {
            Children::Expanded(_) => node.children = Children::Collapsed,
            Children::Collapsed => self.expand(path),
            Children::Leaf | Children::Loading => {}
        }
    }

    fn expand(&mut self, path: &[usize]) {
        let Some(node) = node_mut(&mut self.scopes, path) else {
            return;
        };
        if node.children != Children::Collapsed {
            return;
        }
        node.children = Children::Loading;
        let r = node.reference;
        self.send(
            "variables",
            json!({"variablesReference": r}),
            Pending::Variables(r),
        );
    }

    pub fn add_watch(&mut self, expr: &str) {
        self.watches.push(Watch {
            expr: expr.to_owned(),
            value: WatchValue::Unknown,
        });
        self.eval_watch(self.watches.len() - 1);
    }

    pub fn remove_watch(&mut self, i: usize) {
        if i < self.watches.len() {
            self.watches.remove(i);
            // Answers in flight point at old indices: ask again.
            for i in 0..self.watches.len() {
                self.eval_watch(i);
            }
        }
    }

    fn eval_watch(&mut self, i: usize) {
        let Some(frame) = self.current_frame().map(|f| f.id) else {
            return;
        };
        let expr = self.watches[i].expr.clone();
        self.watches[i].value = WatchValue::Pending;
        self.send(
            "evaluate",
            json!({"expression": expr, "frameId": frame, "context": "watch"}),
            Pending::Evaluate(EvalFor::Watch(i)),
        );
    }

    /// Evaluate a console line.
    pub fn repl(&mut self, expr: &str) {
        self.note(LineKind::Input, format!("> {expr}\n"));
        let mut args = json!({"expression": expr, "context": "repl"});
        if let Some(f) = self.current_frame() {
            args["frameId"] = json!(f.id);
        }
        self.send("evaluate", args, Pending::Evaluate(EvalFor::Repl));
    }

    /// Evaluate for a hover; the answer comes as [`Notice::Hover`].
    pub fn hover(&mut self, expr: &str) -> bool {
        let Some(frame) = self.current_frame().map(|f| f.id) else {
            return false;
        };
        let context = if self.caps.supports_evaluate_for_hovers {
            "hover"
        } else {
            "watch"
        };
        self.send(
            "evaluate",
            json!({"expression": expr, "frameId": frame, "context": context}),
            Pending::Evaluate(EvalFor::Hover),
        );
        true
    }

    /// Loaded variables of the shown frame's scopes, by name (inline
    /// values). Only the top level: children are not fetched for this.
    pub fn locals(&self) -> Vec<(&str, &str)> {
        self.scopes
            .iter()
            .filter_map(|s| match &s.children {
                Children::Expanded(k) => Some(k),
                _ => None,
            })
            .flatten()
            .map(|v| (v.name.as_str(), v.value.as_str()))
            .collect()
    }
}

fn result_of(b: &Breakpoint, sent_line: usize) -> BpResult {
    if b.verified {
        BpResult::Verified {
            line: b.line.map_or(sent_line, |l| (l.max(1) - 1) as usize),
        }
    } else {
        BpResult::Rejected(b.message.clone().unwrap_or_else(|| "not bound yet".into()))
    }
}

fn node_mut<'a>(nodes: &'a mut [VarNode], path: &[usize]) -> Option<&'a mut VarNode> {
    let (&first, rest) = path.split_first()?;
    let node = nodes.get_mut(first)?;
    if rest.is_empty() {
        return Some(node);
    }
    match &mut node.children {
        Children::Expanded(kids) => node_mut(kids, rest),
        _ => None,
    }
}

/// Put `kids` under every loading node with `reference`.
fn fill(nodes: &mut [VarNode], reference: i64, kids: &[VarNode], ok: bool) {
    for n in nodes {
        match &mut n.children {
            Children::Loading if n.reference == reference => {
                n.children = if ok {
                    Children::Expanded(kids.to_vec())
                } else {
                    Children::Collapsed
                };
            }
            Children::Expanded(k) => fill(k, reference, kids, ok),
            _ => {}
        }
    }
}
