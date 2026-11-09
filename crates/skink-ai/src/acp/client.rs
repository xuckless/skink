//! Blocking ACP client: std threads + mpsc, newline-delimited JSON-RPC 2.0.
//!
//! Message framing/classification is adapted in spirit from Lapce's
//! `lapce-rpc/src/stdio.rs` (@ f66ffaa, Apache-2.0, Copyright (c) Lapce
//! Authors): one JSON value per line, serialise-then-flush on write, a
//! reader thread that classifies request / notification / response. Changes:
//! the reader never dies on a bad line, tolerates invalid UTF-8, oversized
//! lines, string ids and out-of-order responses, fails every pending call on
//! EOF, and has no crossbeam/anyhow/tracing dependencies. See `NOTICES.md`.
//!
//! Threads: one reader and one writer per client, plus a short-lived thread
//! per `fs/*` / `terminal/*` request from the agent (at most
//! `MAX_HANDLER_THREADS` at once, so a slow handler never stalls streaming
//! and a flood of requests cannot exhaust threads). Outgoing messages are
//! queued to the writer thread, so no caller (the UI thread, the reader thread
//! answering a request, a permission reply) ever blocks on an agent that
//! stopped reading its stdin. Events go to an [`EventSink`];
//! [`AcpClient::start`] wires that to an `mpsc::Receiver`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use super::handler::{AcpHandler, HandlerError, TerminalSpec};
use super::transport::{AgentProcess, Transport};
use super::wire::{self, Update};
use crate::agent::{
    AgentEvent, AgentInfo, ErrorInfo, PermissionChoice, PermissionReply, PromptBlock, SessionId,
    SessionInfo, StopReason, TurnId, WeakReply, markdown_fence,
};

/// The event type of the client (identical to the controller-level event).
pub type AcpEvent = AgentEvent;

/// Receives every event on the reader (or helper) thread. Must not block.
pub type EventSink = Arc<dyn Fn(AcpEvent) + Send + Sync>;

const MAX_LINE: usize = 32 * 1024 * 1024;
const MAX_PROTOCOL_ERROR_EVENTS: u32 = 50;
/// Concurrent `fs/*` / `terminal/*` handler threads; requests beyond this are
/// answered with an error instead of spawning more.
const MAX_HANDLER_THREADS: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub enum AcpError {
    /// The connection is closed (agent exited or client shut down).
    Closed,
    Timeout,
    Io(String),
    /// JSON-RPC error response from the agent.
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    /// The agent sent something that violates the protocol.
    Protocol(String),
    /// Needs a capability the agent did not advertise.
    Unsupported(String),
    /// `prompt` while the session already has a turn running.
    TurnInProgress,
}

impl std::fmt::Display for AcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("agent connection closed"),
            Self::Timeout => f.write_str("timed out waiting for agent"),
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Rpc { code, message, .. } => write!(f, "agent error {code}: {message}"),
            Self::Protocol(e) => write!(f, "protocol error: {e}"),
            Self::Unsupported(e) => write!(f, "unsupported: {e}"),
            Self::TurnInProgress => f.write_str("a turn is already running for this session"),
        }
    }
}

impl std::error::Error for AcpError {}

/// A readable detail from a JSON-RPC error's `data` (a string, or the
/// `details` / `message` field agents commonly put there).
fn rpc_details(data: &Value) -> Option<String> {
    let s = match data {
        Value::String(s) => s.as_str(),
        Value::Object(o) => o
            .get("details")
            .or_else(|| o.get("message"))
            .and_then(Value::as_str)?,
        _ => return None,
    };
    (!s.trim().is_empty()).then(|| truncate(s.trim(), 300))
}

impl AcpError {
    /// Summary suitable for [`AgentEvent`] payloads.
    pub fn info(&self) -> ErrorInfo {
        match self {
            Self::Rpc {
                code,
                message,
                data,
            } => ErrorInfo {
                // "Internal error" alone says nothing: add the agent's details.
                message: match data.as_ref().and_then(rpc_details) {
                    Some(d) => format!("{message}: {d}"),
                    None => message.clone(),
                },
                code: Some(*code),
                auth_required: *code == wire::E_AUTH_REQUIRED,
            },
            other => ErrorInfo {
                message: other.to_string(),
                code: None,
                auth_required: false,
            },
        }
    }
}

/// MCP server the agent should connect to (`session/new` `mcpServers`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServer {
    Stdio {
        name: String,
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    Http {
        name: String,
        url: String,
        headers: Vec<(String, String)>,
    },
    Sse {
        name: String,
        url: String,
        headers: Vec<(String, String)>,
    },
}

fn pairs(v: &[(String, String)]) -> Value {
    Value::Array(
        v.iter()
            .map(|(n, val)| json!({"name": n, "value": val}))
            .collect(),
    )
}

impl McpServer {
    fn to_json(&self) -> Value {
        match self {
            Self::Stdio {
                name,
                command,
                args,
                env,
            } => json!({"name": name, "command": command, "args": args, "env": pairs(env)}),
            Self::Http { name, url, headers } => {
                json!({"type": "http", "name": name, "url": url, "headers": pairs(headers)})
            }
            Self::Sse { name, url, headers } => {
                json!({"type": "sse", "name": name, "url": url, "headers": pairs(headers)})
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClientOptions {
    /// `clientInfo.name`.
    pub name: String,
    pub version: String,
    /// Timeout of blocking calls other than `initialize` and `prompt`.
    pub request_timeout: Duration,
    pub init_timeout: Duration,
    /// How long `shutdown` waits for the agent to exit after closing stdin.
    pub shutdown_grace: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            name: "skink".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            request_timeout: Duration::from_secs(120),
            init_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_millis(500),
        }
    }
}

enum Pending {
    Call(Sender<Result<Value, AcpError>>),
    Prompt {
        session: SessionId,
        turn: TurnId,
        done: Sender<Result<StopReason, AcpError>>,
    },
}

#[derive(Default)]
struct State {
    closed: bool,
    pending: HashMap<u64, Pending>,
    turns: HashMap<String, TurnId>,
    perms: Vec<(String, WeakReply)>,
}

struct Shared {
    /// Queue to the writer thread (one serialised line per item); `None` once
    /// the client closed the agent's stdin.
    out: Mutex<Option<Sender<Vec<u8>>>>,
    handler_threads: AtomicUsize,
    next_id: AtomicU64,
    next_turn: AtomicU64,
    state: Mutex<State>,
    sink: EventSink,
    handler: Arc<dyn AcpHandler>,
    process: Mutex<Option<Box<dyn AgentProcess>>>,
    info: Mutex<AgentInfo>,
    opts: ClientOptions,
    proto_errors: AtomicU32,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn truncate(s: &str, n: usize) -> String {
    let mut t: String = s.chars().take(n).collect();
    if t.len() < s.len() {
        t.push_str("...");
    }
    t
}

impl Shared {
    fn emit(&self, ev: AcpEvent) {
        (self.sink)(ev);
    }

    fn protocol_error(&self, msg: String) {
        let n = self.proto_errors.fetch_add(1, Ordering::Relaxed);
        if n < MAX_PROTOCOL_ERROR_EVENTS {
            self.emit(AgentEvent::ProtocolError { message: msg });
        } else if n == MAX_PROTOCOL_ERROR_EVENTS {
            self.emit(AgentEvent::ProtocolError {
                message: "further protocol errors suppressed".into(),
            });
        }
    }

    /// Serialise as one line and queue it for the writer thread. `Closed`
    /// once stdin was closed or a write failed.
    fn send(&self, v: &Value) -> Result<(), AcpError> {
        let mut line = serde_json::to_vec(v).map_err(|e| AcpError::Protocol(e.to_string()))?;
        line.push(b'\n');
        lock(&self.out)
            .as_ref()
            .ok_or(AcpError::Closed)?
            .send(line)
            .map_err(|_| AcpError::Closed)
    }

    /// Close the agent's stdin (after the already queued messages).
    fn close_input(&self) {
        lock(&self.out).take();
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn respond(&self, id: &Value, r: Result<Value, (i64, String)>) {
        let msg = match r {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        };
        let _ = self.send(&msg);
    }

    /// Register `pending` under a fresh id, then send the request.
    fn request(&self, method: &str, params: Value, pending: Pending) -> Result<u64, AcpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut st = lock(&self.state);
            if st.closed {
                return Err(AcpError::Closed);
            }
            st.pending.insert(id, pending);
        }
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(e) = self.send(&msg) {
            lock(&self.state).pending.remove(&id);
            return Err(e);
        }
        Ok(id)
    }

    fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, AcpError> {
        let (tx, rx) = mpsc::channel();
        let id = self.request(method, params, Pending::Call(tx))?;
        match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => {
                lock(&self.state).pending.remove(&id);
                Err(AcpError::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => Err(AcpError::Closed),
        }
    }

    // ---- reader side ---------------------------------------------------

    fn handle_line(self: &Arc<Self>, raw: &[u8]) {
        let text = String::from_utf8_lossy(raw);
        let t = text.trim();
        if t.is_empty() {
            return;
        }
        let v: Value = match serde_json::from_str(t) {
            Ok(v) => v,
            Err(e) => {
                self.protocol_error(format!(
                    "malformed JSON from agent ({e}): {}",
                    truncate(t, 120)
                ));
                return;
            }
        };
        if !v.is_object() {
            self.protocol_error(format!("expected a JSON object, got: {}", truncate(t, 120)));
            return;
        }
        let method = v.get("method").and_then(Value::as_str);
        let id = v.get("id").filter(|i| !i.is_null());
        match (method, id) {
            (Some(m), Some(id)) => self.handle_request(
                m,
                id.clone(),
                v.get("params").cloned().unwrap_or(Value::Null),
            ),
            (Some(m), None) => {
                self.handle_notification(m, v.get("params").cloned().unwrap_or(Value::Null))
            }
            (None, Some(id)) if v.get("result").is_some() || v.get("error").is_some() => {
                self.handle_response(id, &v);
            }
            _ => self.protocol_error(format!("unclassifiable message: {}", truncate(t, 120))),
        }
    }

    fn handle_response(&self, id: &Value, v: &Value) {
        let key = id
            .as_u64()
            .or_else(|| id.as_str().and_then(|s| s.parse().ok()));
        let pending = key.and_then(|k| lock(&self.state).pending.remove(&k));
        let Some(pending) = pending else {
            self.protocol_error(format!("response for unknown request id {id}"));
            return;
        };
        let result: Result<Value, AcpError> = match v.get("error") {
            Some(e) if !e.is_null() => Err(AcpError::Rpc {
                code: e
                    .get("code")
                    .and_then(Value::as_i64)
                    .unwrap_or(wire::E_INTERNAL),
                message: e
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("(no message)")
                    .to_owned(),
                data: e.get("data").cloned(),
            }),
            _ => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
        };
        match pending {
            Pending::Call(tx) => {
                let _ = tx.send(result);
            }
            Pending::Prompt {
                session,
                turn,
                done,
            } => {
                {
                    let mut st = lock(&self.state);
                    if st.turns.get(&session.0) == Some(&turn) {
                        st.turns.remove(&session.0);
                    }
                }
                match result {
                    Ok(r) => {
                        let stop = wire::parse_stop_reason(&r);
                        self.emit(AgentEvent::TurnFinished {
                            session,
                            turn,
                            stop: stop.clone(),
                        });
                        let _ = done.send(Ok(stop));
                    }
                    Err(e) => {
                        self.emit(AgentEvent::TurnFailed {
                            session,
                            turn,
                            error: e.info(),
                        });
                        let _ = done.send(Err(e));
                    }
                }
            }
        }
    }

    fn handle_notification(&self, method: &str, params: Value) {
        if method != wire::M_SESSION_UPDATE {
            return; // extension / future notification: ignore
        }
        match wire::parse_session_update(params) {
            Ok((sid, u)) => {
                let session = SessionId(sid);
                let ev = match u {
                    Update::User(text) => AgentEvent::UserChunk { session, text },
                    Update::Message(text) => AgentEvent::MessageChunk { session, text },
                    Update::Thought(text) => AgentEvent::ThoughtChunk { session, text },
                    Update::ToolCall(call) => AgentEvent::ToolCall { session, call },
                    Update::ToolCallUpdate(patch) => AgentEvent::ToolCallUpdate { session, patch },
                    Update::Plan(entries) => AgentEvent::Plan { session, entries },
                    Update::Commands(commands) => AgentEvent::Commands { session, commands },
                    Update::Mode(mode_id) => AgentEvent::ModeChanged { session, mode_id },
                    Update::Ignored => return,
                };
                self.emit(ev);
            }
            Err(e) => self.protocol_error(e),
        }
    }

    fn handle_request(self: &Arc<Self>, method: &str, id: Value, params: Value) {
        if method == wire::M_REQUEST_PERMISSION {
            match wire::parse_permission(params) {
                Ok((sid, tool, options)) => {
                    let weak = Arc::downgrade(self);
                    let rid = id.clone();
                    let reply = PermissionReply::new(move |choice| {
                        if let Some(sh) = Weak::upgrade(&weak) {
                            let outcome = match choice {
                                PermissionChoice::Selected(option_id) => {
                                    json!({"outcome": "selected", "optionId": option_id})
                                }
                                PermissionChoice::Cancelled => json!({"outcome": "cancelled"}),
                            };
                            sh.respond(&rid, Ok(json!({"outcome": outcome})));
                        }
                    });
                    {
                        let mut st = lock(&self.state);
                        st.perms.retain(|(_, w)| w.is_live());
                        st.perms.push((sid.clone(), reply.downgrade()));
                    }
                    self.emit(AgentEvent::Permission(crate::agent::PermissionRequest {
                        session: SessionId(sid),
                        tool,
                        options,
                        reply,
                    }));
                }
                Err(e) => {
                    self.respond(&id, Err((wire::E_INVALID_PARAMS, e.clone())));
                    self.protocol_error(e);
                }
            }
            return;
        }
        let handled = matches!(
            method,
            wire::M_FS_READ
                | wire::M_FS_WRITE
                | wire::M_TERMINAL_CREATE
                | wire::M_TERMINAL_OUTPUT
                | wire::M_TERMINAL_RELEASE
                | wire::M_TERMINAL_WAIT
                | wire::M_TERMINAL_KILL
        );
        if !handled {
            self.respond(
                &id,
                Err((
                    wire::E_METHOD_NOT_FOUND,
                    format!("method not found: {method}"),
                )),
            );
            return;
        }
        if self.handler_threads.fetch_add(1, Ordering::AcqRel) >= MAX_HANDLER_THREADS {
            self.handler_threads.fetch_sub(1, Ordering::AcqRel);
            self.respond(
                &id,
                Err((wire::E_INTERNAL, "too many concurrent requests".into())),
            );
            return;
        }
        let sh = Arc::clone(self);
        let m = method.to_owned();
        let id2 = id.clone();
        let spawned = thread::Builder::new()
            .name("acp-request".into())
            .spawn(move || {
                let r = sh.run_client_method(&m, &params);
                sh.respond(&id2, r);
                sh.handler_threads.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            self.handler_threads.fetch_sub(1, Ordering::AcqRel);
            self.respond(
                &id,
                Err((wire::E_INTERNAL, "cannot spawn handler thread".into())),
            );
        }
    }

    fn run_client_method(&self, method: &str, p: &Value) -> Result<Value, (i64, String)> {
        let bad = |what: &str| {
            (
                wire::E_INVALID_PARAMS,
                format!("{method}: missing or invalid {what}"),
            )
        };
        let str_field = |k: &str| p.get(k).and_then(Value::as_str);
        let session = str_field("sessionId").ok_or_else(|| bad("sessionId"))?;
        let h = &*self.handler;
        let map_err = |e: HandlerError| {
            let code = match &e {
                HandlerError::Unsupported => wire::E_METHOD_NOT_FOUND,
                HandlerError::NotFound(_) => wire::E_RESOURCE_NOT_FOUND,
                HandlerError::OutsideWorkspace(_) => wire::E_INVALID_PARAMS,
                HandlerError::Failed(_) => wire::E_INTERNAL,
            };
            (code, e.to_string())
        };
        let u32_field = |k: &str| {
            p.get(k)
                .and_then(Value::as_u64)
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        };
        match method {
            wire::M_FS_READ => {
                let path = str_field("path").ok_or_else(|| bad("path"))?;
                let content = h
                    .read_text_file(
                        session,
                        Path::new(path),
                        u32_field("line"),
                        u32_field("limit"),
                    )
                    .map_err(map_err)?;
                Ok(json!({"content": content}))
            }
            wire::M_FS_WRITE => {
                let path = str_field("path").ok_or_else(|| bad("path"))?;
                let content = str_field("content").ok_or_else(|| bad("content"))?;
                h.write_text_file(session, Path::new(path), content)
                    .map_err(map_err)?;
                Ok(json!({}))
            }
            _ => {
                let term = h.terminal().ok_or((
                    wire::E_METHOD_NOT_FOUND,
                    format!("{method}: terminals not supported"),
                ))?;
                let tid = || str_field("terminalId").ok_or_else(|| bad("terminalId"));
                match method {
                    wire::M_TERMINAL_CREATE => {
                        let spec = TerminalSpec {
                            session: session.to_owned(),
                            command: str_field("command")
                                .ok_or_else(|| bad("command"))?
                                .to_owned(),
                            args: p
                                .get("args")
                                .and_then(Value::as_array)
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|x| x.as_str().map(str::to_owned))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            env: p
                                .get("env")
                                .and_then(Value::as_array)
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|e| {
                                            Some((
                                                e.get("name")?.as_str()?.to_owned(),
                                                e.get("value")?.as_str()?.to_owned(),
                                            ))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                            cwd: str_field("cwd").map(Into::into),
                            output_byte_limit: p.get("outputByteLimit").and_then(Value::as_u64),
                        };
                        let id = term.create(&spec).map_err(map_err)?;
                        Ok(json!({"terminalId": id}))
                    }
                    wire::M_TERMINAL_OUTPUT => {
                        let o = term.output(session, tid()?).map_err(map_err)?;
                        let mut r = json!({"output": o.output, "truncated": o.truncated});
                        if let Some(x) = o.exit {
                            r["exitStatus"] = json!({"exitCode": x.exit_code, "signal": x.signal});
                        }
                        Ok(r)
                    }
                    wire::M_TERMINAL_WAIT => {
                        let x = term.wait_for_exit(session, tid()?).map_err(map_err)?;
                        Ok(json!({"exitCode": x.exit_code, "signal": x.signal}))
                    }
                    wire::M_TERMINAL_KILL => term
                        .kill(session, tid()?)
                        .map(|()| json!({}))
                        .map_err(map_err),
                    _ => term
                        .release(session, tid()?)
                        .map(|()| json!({}))
                        .map_err(map_err),
                }
            }
        }
    }

    /// Reader hit EOF/error: fail everything pending, reap the process, say goodbye.
    fn on_exit(&self) {
        let (pend, perms) = {
            let mut st = lock(&self.state);
            st.closed = true;
            st.turns.clear();
            (
                std::mem::take(&mut st.pending),
                std::mem::take(&mut st.perms),
            )
        };
        for (_, p) in pend {
            match p {
                Pending::Call(tx) => {
                    let _ = tx.send(Err(AcpError::Closed));
                }
                Pending::Prompt {
                    session,
                    turn,
                    done,
                } => {
                    self.emit(AgentEvent::TurnFailed {
                        session,
                        turn,
                        error: AcpError::Closed.info(),
                    });
                    let _ = done.send(Err(AcpError::Closed));
                }
            }
        }
        for (_, w) in perms {
            w.cancel();
        }
        self.close_input();
        let proc = lock(&self.process).take();
        let (status, stderr_tail) = match proc {
            Some(mut p) => {
                let st = p.reap(self.opts.shutdown_grace);
                (st, p.stderr_tail())
            }
            None => (None, Vec::new()),
        };
        self.emit(AgentEvent::Exited {
            status,
            stderr_tail,
        });
    }
}

/// Drains the outgoing queue into the agent's stdin; dropping the writer at
/// the end closes it.
fn writer_loop(mut w: Box<dyn Write + Send>, rx: Receiver<Vec<u8>>) {
    for line in rx {
        if w.write_all(&line).and_then(|()| w.flush()).is_err() {
            return;
        }
    }
}

fn reader_loop(shared: &Arc<Shared>, reader: Box<dyn Read + Send>) {
    let mut r = BufReader::new(reader);
    let mut buf = Vec::new();
    'outer: loop {
        buf.clear();
        match (&mut r)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut buf)
        {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                shared.protocol_error(format!("read error: {e}"));
                break;
            }
        }
        if !buf.ends_with(b"\n") && buf.len() > MAX_LINE {
            shared.protocol_error(format!("line longer than {MAX_LINE} bytes dropped"));
            loop {
                buf.clear();
                match (&mut r)
                    .take(MAX_LINE as u64 + 1)
                    .read_until(b'\n', &mut buf)
                {
                    Ok(0) | Err(_) => break 'outer,
                    Ok(_) if buf.ends_with(b"\n") => break,
                    Ok(_) => {}
                }
            }
            continue;
        }
        shared.handle_line(&buf);
    }
    shared.on_exit();
}

/// Result of one `session/prompt`. Dropping it is fine: the outcome is also
/// delivered as [`AgentEvent::TurnFinished`] / [`AgentEvent::TurnFailed`].
pub struct TurnHandle {
    pub session: SessionId,
    pub turn: TurnId,
    rx: Receiver<Result<StopReason, AcpError>>,
    done: Option<Result<StopReason, AcpError>>,
}

impl TurnHandle {
    /// Non-blocking poll.
    pub fn try_result(&mut self) -> Option<Result<StopReason, AcpError>> {
        if self.done.is_none() {
            match self.rx.try_recv() {
                Ok(r) => self.done = Some(r),
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => self.done = Some(Err(AcpError::Closed)),
            }
        }
        self.done.clone()
    }
    /// Block until the turn ends or `d` elapses.
    pub fn wait_timeout(&mut self, d: Duration) -> Option<Result<StopReason, AcpError>> {
        if self.done.is_none() {
            match self.rx.recv_timeout(d) {
                Ok(r) => self.done = Some(r),
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => self.done = Some(Err(AcpError::Closed)),
            }
        }
        self.done.clone()
    }
    /// Block until the turn ends.
    pub fn wait(mut self) -> Result<StopReason, AcpError> {
        self.wait_timeout(Duration::from_secs(60 * 60 * 24 * 365))
            .unwrap_or(Err(AcpError::Timeout))
    }
}

/// A live ACP connection to one agent. All methods take `&self`; the type is
/// `Send + Sync` so a controller can share it with helper threads.
pub struct AcpClient {
    shared: Arc<Shared>,
}

impl AcpClient {
    /// Handshake over `transport` and return the client plus its event stream.
    ///
    /// Blocks until `initialize` completes (or `init_timeout`). Capabilities
    /// advertised come from `handler`.
    pub fn start(
        transport: Transport,
        handler: Box<dyn AcpHandler>,
    ) -> Result<(Self, Receiver<AcpEvent>), AcpError> {
        Self::start_with_options(transport, handler, ClientOptions::default())
    }

    pub fn start_with_options(
        transport: Transport,
        handler: Box<dyn AcpHandler>,
        opts: ClientOptions,
    ) -> Result<(Self, Receiver<AcpEvent>), AcpError> {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Arc::new(move |ev| {
            let _ = lock(&tx).send(ev);
        });
        let c = Self::start_with_sink(transport, Arc::from(handler), sink, opts)?;
        Ok((c, rx))
    }

    /// Like [`start`](Self::start) but deliver events through `sink` (e.g. to
    /// tag them with an agent id and wake the UI loop).
    pub fn start_with_sink(
        transport: Transport,
        handler: Arc<dyn AcpHandler>,
        sink: EventSink,
        opts: ClientOptions,
    ) -> Result<Self, AcpError> {
        let Transport {
            reader,
            writer,
            process,
        } = transport;
        let (out_tx, out_rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            out: Mutex::new(Some(out_tx)),
            handler_threads: AtomicUsize::new(0),
            next_id: AtomicU64::new(1),
            next_turn: AtomicU64::new(1),
            state: Mutex::new(State::default()),
            sink,
            handler,
            process: Mutex::new(process),
            info: Mutex::new(AgentInfo::default()),
            opts,
            proto_errors: AtomicU32::new(0),
        });
        thread::Builder::new()
            .name("acp-writer".into())
            .stack_size(64 * 1024)
            .spawn(move || writer_loop(writer, out_rx))
            .map_err(|e| AcpError::Io(e.to_string()))?;
        let rs = Arc::clone(&shared);
        thread::Builder::new()
            .name("acp-reader".into())
            .spawn(move || reader_loop(&rs, reader))
            .map_err(|e| AcpError::Io(e.to_string()))?;
        let client = Self { shared };
        client.handshake()?;
        Ok(client)
    }

    fn handshake(&self) -> Result<(), AcpError> {
        let sh = &self.shared;
        let (rd, wr) = sh.handler.fs_capabilities();
        let params = json!({
            "protocolVersion": wire::PROTOCOL_VERSION,
            "clientCapabilities": {
                "fs": {"readTextFile": rd, "writeTextFile": wr},
                "terminal": sh.handler.terminal().is_some(),
            },
            "clientInfo": {"name": sh.opts.name, "title": "Skink", "version": sh.opts.version},
        });
        let res = sh
            .call(wire::M_INITIALIZE, params, sh.opts.init_timeout)
            .and_then(|v| wire::parse_initialize(v).map_err(AcpError::Protocol));
        match res {
            Ok(info) => {
                *lock(&sh.info) = info;
                Ok(())
            }
            Err(e) => {
                self.kill();
                Err(e)
            }
        }
    }

    /// What the agent reported in `initialize` (capabilities, auth methods).
    pub fn info(&self) -> AgentInfo {
        lock(&self.shared.info).clone()
    }

    /// Whether the connection is still up.
    /// The agent's most telling stderr line, if it wrote one: the first line
    /// starting with "Error", else the last non-empty one.
    pub fn stderr_hint(&self) -> Option<String> {
        let tail = lock(&self.shared.process).as_ref()?.stderr_tail();
        tail.iter()
            .find(|l| l.trim_start().starts_with("Error"))
            .or_else(|| tail.iter().rev().find(|l| !l.trim().is_empty()))
            .map(|l| truncate(l.trim(), 300))
    }

    pub fn is_alive(&self) -> bool {
        !lock(&self.shared.state).closed
    }

    /// `authenticate` with one of [`AgentInfo::auth_methods`].
    pub fn authenticate(&self, method_id: &str) -> Result<(), AcpError> {
        self.call(wire::M_AUTHENTICATE, json!({"methodId": method_id}))
            .map(|_| ())
    }

    pub fn new_session(&self, cwd: &Path) -> Result<SessionInfo, AcpError> {
        self.new_session_with(cwd, &[])
    }

    pub fn new_session_with(&self, cwd: &Path, mcp: &[McpServer]) -> Result<SessionInfo, AcpError> {
        let v = self.call(
            wire::M_SESSION_NEW,
            json!({"cwd": abs(cwd), "mcpServers": mcp.iter().map(McpServer::to_json).collect::<Vec<_>>()}),
        )?;
        let p = wire::parse_session(v, None).map_err(AcpError::Protocol)?;
        Ok(session_info(p))
    }

    /// `session/load`; only if the agent advertised `loadSession`. History is
    /// replayed as [`AgentEvent::UserChunk`] / `MessageChunk` events first.
    pub fn load_session(
        &self,
        session: &SessionId,
        cwd: &Path,
        mcp: &[McpServer],
    ) -> Result<SessionInfo, AcpError> {
        if !lock(&self.shared.info).load_session {
            return Err(AcpError::Unsupported(
                "agent does not support session/load".into(),
            ));
        }
        let v = self.call(
            wire::M_SESSION_LOAD,
            json!({"sessionId": session.0, "cwd": abs(cwd),
                   "mcpServers": mcp.iter().map(McpServer::to_json).collect::<Vec<_>>()}),
        )?;
        let p = wire::parse_session(v, Some(&session.0)).map_err(AcpError::Protocol)?;
        Ok(session_info(p))
    }

    /// `session/set_model` (unstable ACP; only for agents that listed models).
    pub fn set_model(&self, session: &SessionId, model_id: &str) -> Result<(), AcpError> {
        self.call(
            wire::M_SESSION_SET_MODEL,
            json!({"sessionId": session.0, "modelId": model_id}),
        )
        .map(|_| ())
    }

    pub fn set_mode(&self, session: &SessionId, mode_id: &str) -> Result<(), AcpError> {
        self.call(
            wire::M_SESSION_SET_MODE,
            json!({"sessionId": session.0, "modeId": mode_id}),
        )
        .map(|_| ())
    }

    /// Start a prompt turn. Returns immediately; stream arrives as events.
    pub fn prompt(
        &self,
        session: &SessionId,
        blocks: &[PromptBlock],
    ) -> Result<TurnHandle, AcpError> {
        let embedded = lock(&self.shared.info).embedded_context;
        let prompt: Vec<Value> = blocks.iter().map(|b| block_json(b, embedded)).collect();
        let turn = TurnId(self.shared.next_turn.fetch_add(1, Ordering::Relaxed));
        {
            let mut st = lock(&self.shared.state);
            if st.closed {
                return Err(AcpError::Closed);
            }
            if st.turns.contains_key(&session.0) {
                return Err(AcpError::TurnInProgress);
            }
            st.turns.insert(session.0.clone(), turn);
        }
        let (tx, rx) = mpsc::channel();
        let r = self.shared.request(
            wire::M_SESSION_PROMPT,
            json!({"sessionId": session.0, "prompt": prompt}),
            Pending::Prompt {
                session: session.clone(),
                turn,
                done: tx,
            },
        );
        if let Err(e) = r {
            let mut st = lock(&self.shared.state);
            if st.turns.get(&session.0) == Some(&turn) {
                st.turns.remove(&session.0);
            }
            return Err(e);
        }
        Ok(TurnHandle {
            session: session.clone(),
            turn,
            rx,
            done: None,
        })
    }

    /// `session/cancel`; also answers every pending permission request of the
    /// session with `cancelled`, as the protocol requires. The turn then ends
    /// with `StopReason::Cancelled`.
    pub fn cancel(&self, session: &SessionId) -> Result<(), AcpError> {
        let perms: Vec<WeakReply> = {
            let mut st = lock(&self.shared.state);
            let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut st.perms)
                .into_iter()
                .partition(|(s, _)| *s == session.0);
            st.perms = rest;
            mine.into_iter().map(|(_, w)| w).collect()
        };
        for w in perms {
            w.cancel();
        }
        self.shared
            .notify(wire::M_SESSION_CANCEL, json!({"sessionId": session.0}))
    }

    /// Is a turn running in `session`?
    pub fn turn_active(&self, session: &SessionId) -> bool {
        lock(&self.shared.state).turns.contains_key(&session.0)
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.shared
            .call(method, params, self.shared.opts.request_timeout)
    }

    /// Close stdin and give the agent `shutdown_grace` to exit on its own,
    /// then kill it. The final [`AgentEvent::Exited`] follows as usual.
    pub fn shutdown(&self) {
        self.shared.close_input();
        let deadline = std::time::Instant::now() + self.shared.opts.shutdown_grace;
        while self.is_alive() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if self.is_alive() {
            self.kill();
        }
    }

    fn kill(&self) {
        self.shared.close_input();
        if let Some(p) = lock(&self.shared.process).as_mut() {
            p.terminate();
        }
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        // Do not block in drop: close stdin and kill; the reader reaps.
        self.kill();
    }
}

fn abs(p: &Path) -> String {
    std::path::absolute(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn block_json(b: &PromptBlock, embedded_ok: bool) -> Value {
    match b {
        PromptBlock::Text(t) => json!({"type": "text", "text": t}),
        PromptBlock::ResourceLink {
            uri,
            name,
            mime_type,
        } => {
            let mut v = json!({"type": "resource_link", "uri": uri, "name": name});
            if let Some(m) = mime_type {
                v["mimeType"] = json!(m);
            }
            v
        }
        PromptBlock::Resource {
            uri,
            text,
            mime_type,
        } => {
            if embedded_ok {
                let mut r = json!({"uri": uri, "text": text});
                if let Some(m) = mime_type {
                    r["mimeType"] = json!(m);
                }
                json!({"type": "resource", "resource": r})
            } else {
                json!({"type": "text", "text": format!("Contents of {uri}:\n{}", markdown_fence("", text))})
            }
        }
    }
}

fn session_info(p: wire::ParsedSession) -> SessionInfo {
    SessionInfo {
        id: SessionId(p.id),
        modes: p.modes,
        current_mode: p.current_mode,
        models: p.models,
        current_model: p.current_model,
    }
}
