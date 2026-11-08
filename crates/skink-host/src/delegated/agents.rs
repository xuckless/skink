//! [`DelegatedAgents`]: Skink's [`AgentController`] served by EveryAI's
//! `agents.*`, `prompt.*` and `models.list` tools.
//!
//! How the controller vocabulary maps onto the contract (`docs/HANDOVER.md`):
//!
//! * `start_agent(label, spec)` resolves the host's agent id (`agents.list`:
//!   by id or name equal to `label`, by slug of `label`, by `spec.program`)
//!   on a worker thread and reports [`AgentEvent::Ready`] or
//!   [`AgentEvent::StartFailed`]. Nothing is spawned locally.
//! * `new_session(agent, cwd)` is `agents.start`: EveryAI opens an ACP session
//!   in its own UI and Skink keeps the returned id
//!   ([`AgentEvent::SessionStarted`]).
//! * `send_prompt` is `prompt.run` in `ask` mode (EveryAI's own AI route; the
//!   contract has no tool that writes into an agent session, so the session id
//!   only routes events and guards against overlapping turns). Streaming
//!   `notifications/progress` become `ThoughtChunk` (status), `MessageChunk`
//!   (text), and `ToolCall` (edit / search hit) events; the turn ends with
//!   `TurnFinished` (`EndTurn` or `Cancelled`) or `TurnFailed`. For `edit` and
//!   `search` modes use [`DelegatedAgents::run_prompt`] and read the structured
//!   result with [`DelegatedAgents::take_result`].
//! * `cancel` drops the turn's socket through a per-turn [`CancelHandle`]
//!   (which also sends `notifications/cancelled`) and then calls
//!   `prompt.cancel` as a second, host-side route.
//! * Authentication, `load_session` and `set_mode` are EveryAI's business and
//!   answer `Unsupported`.
//!
//! No call blocks: everything slow runs on a short-lived thread and reports
//! through [`poll_events`](AgentController::poll_events) (plus the optional
//! waker). A tripped [`HostLink`] makes calls fail fast with `Unsupported`.

use super::{HostLink, LinkError, lock};
use crate::client::{CancelHandle, Timeouts};
use crate::error::Error;
use crate::tools::{self, ContextKind, ContextRef, PromptEvent, PromptMode, PromptRunArgs};
use serde_json::json;
use skink_ai::agent::{
    AgentEvent, AgentId, AgentInfo, AgentSpec, ErrorInfo, PromptBlock, RequestId, SessionId,
    SessionInfo, StopReason, ToolCallInfo, ToolContent, ToolKind, ToolLocation, ToolStatus, TurnId,
};
use skink_ai::local::Waker;
use skink_ai::{AgentController, AgentError, AgentState, AgentStatus};
use skink_core::Backend;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How many unread structured results [`DelegatedAgents::take_result`] keeps.
const MAX_RESULTS: usize = 32;
const UNAVAILABLE: &str = "EveryAI host unavailable";

type Sink = Arc<dyn Fn(AgentEvent) + Send + Sync>;

struct SlotState {
    phase: AgentState,
    info: Option<AgentInfo>,
    /// The host's agent id; `None` for the prompt-only pseudo agent.
    host_id: Option<String>,
}

struct Slot {
    label: String,
    state: Arc<Mutex<SlotState>>,
}

struct Turn {
    agent: AgentId,
    session: SessionId,
    task_id: String,
    cancelled: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    handle: CancelHandle,
}

type Turns = Arc<Mutex<HashMap<TurnId, Turn>>>;
type Results = Arc<Mutex<BTreeMap<TurnId, tools::PromptRunResult>>>;

/// The delegated [`AgentController`]. See the module docs.
pub struct DelegatedAgents {
    link: Arc<HostLink>,
    next_agent: u64,
    next_request: u64,
    next_turn: u64,
    agents: HashMap<AgentId, Slot>,
    tx: Sender<(AgentId, AgentEvent)>,
    rx: Receiver<(AgentId, AgentEvent)>,
    waker: Option<Waker>,
    turns: Turns,
    results: Results,
    model: Option<String>,
    cwd: Option<PathBuf>,
}

fn unavailable() -> AgentError {
    AgentError::Unsupported(UNAVAILABLE.into())
}

fn agent_err(e: LinkError) -> AgentError {
    match e {
        LinkError::Unavailable => unavailable(),
        LinkError::ToolMissing(t) => AgentError::Unsupported(format!("host has no tool {t}")),
        LinkError::Host(e) => AgentError::Other(e.to_string()),
    }
}

fn error_info(e: &LinkError) -> ErrorInfo {
    ErrorInfo {
        message: e.to_string(),
        code: match e {
            LinkError::Host(Error::Rpc { code, .. }) => Some(*code),
            _ => None,
        },
        auth_required: false,
    }
}

/// `Claude Code` -> `claude-code`.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

fn program_stem(program: &str) -> String {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
    base.strip_suffix(".exe").unwrap_or(base).to_lowercase()
}

/// Find the host agent a Skink `label` / `spec.program` refers to.
fn pick<'a>(
    list: &'a [tools::AgentInfo],
    label: &str,
    program: &str,
) -> Option<&'a tools::AgentInfo> {
    let (slug, prog) = (slug(label), program_stem(program));
    list.iter()
        .find(|a| a.id == label)
        .or_else(|| list.iter().find(|a| a.name.eq_ignore_ascii_case(label)))
        .or_else(|| list.iter().find(|a| a.id == slug))
        .or_else(|| list.iter().find(|a| !prog.is_empty() && a.id == prog))
}

struct Resolved {
    id: String,
    name: String,
    description: Option<String>,
}

fn resolve(link: &HostLink, label: &str, program: &str) -> Result<Resolved, String> {
    match link.call::<_, tools::AgentsListResult>(tools::AGENTS_LIST, &json!({})) {
        Ok(list) => match pick(&list.agents, label, program) {
            Some(a) if a.installed => Ok(Resolved {
                id: a.id.clone(),
                name: a.name.clone(),
                description: a.description.clone(),
            }),
            Some(a) => Err(format!("{} is not installed", a.name)),
            None => Err(format!("EveryAI does not know an agent called {label}")),
        },
        // The host cannot list agents: trust the label and let `agents.start` decide.
        Err(LinkError::ToolMissing(_) | LinkError::Host(Error::Rpc { .. })) => Ok(Resolved {
            id: slug(label),
            name: label.to_owned(),
            description: None,
        }),
        Err(e) => Err(e.to_string()),
    }
}

fn file_name_of(uri: &str, fallback: &str) -> String {
    skink_lsp::uri::uri_to_path(uri)
        .map_or_else(|| fallback.to_owned(), |p| p.to_string_lossy().into_owned())
}

/// Prompt blocks to a `prompt.run` instruction plus `@file` context.
fn blocks_to_prompt(blocks: &[PromptBlock]) -> Result<(String, Vec<ContextRef>), AgentError> {
    let mut text = Vec::new();
    let mut context = Vec::new();
    for b in blocks {
        match b {
            PromptBlock::Text(t) => text.push(t.as_str()),
            PromptBlock::ResourceLink { uri, name, .. } => context.push(ContextRef {
                kind: ContextKind::File,
                name: file_name_of(uri, name),
                text: None,
            }),
            PromptBlock::Resource { uri, text, .. } => context.push(ContextRef {
                kind: ContextKind::File,
                name: file_name_of(uri, uri),
                text: Some(text.clone()),
            }),
        }
    }
    let instruction = text.join("\n\n");
    if instruction.trim().is_empty() {
        return Err(AgentError::Other("the prompt has no text".into()));
    }
    Ok((instruction, context))
}

impl DelegatedAgents {
    /// Use `link`. Each turn runs on its own [`fork`](crate::McpClient::fork)
    /// of the link's client, so cancelling one turn never touches another.
    pub fn new(link: Arc<HostLink>) -> Self {
        let (tx, rx) = mpsc::channel();
        DelegatedAgents {
            link,
            next_agent: 1,
            next_request: 1,
            next_turn: 1,
            agents: HashMap::new(),
            tx,
            rx,
            waker: None,
            turns: Arc::default(),
            results: Arc::default(),
            model: None,
            cwd: None,
        }
    }

    /// Called from a background thread whenever an event is queued.
    #[must_use]
    pub fn with_waker(mut self, waker: Waker) -> Self {
        self.waker = Some(waker);
        self
    }

    /// Model id (from [`models`](Self::models)) for later prompts and
    /// sessions; `None` is the host default.
    pub fn set_model(&mut self, model: Option<String>) {
        self.model = model;
    }

    /// Project root sent as `cwd` with prompts.
    pub fn set_cwd(&mut self, cwd: Option<PathBuf>) {
        self.cwd = cwd;
    }

    pub fn link(&self) -> &Arc<HostLink> {
        &self.link
    }

    /// `agents.list`, blocking: call from a worker thread.
    pub fn host_agents(&self) -> Result<Vec<tools::AgentInfo>, AgentError> {
        self.link
            .call::<_, tools::AgentsListResult>(tools::AGENTS_LIST, &json!({}))
            .map(|r| r.agents)
            .map_err(agent_err)
    }

    /// `models.list`, blocking: call from a worker thread.
    pub fn models(&self) -> Result<Vec<tools::Model>, AgentError> {
        self.link
            .call::<_, tools::ModelsResult>(tools::MODELS_LIST, &json!({}))
            .map(|r| r.models)
            .map_err(agent_err)
    }

    /// Register the host's own prompt route (`prompt.run`) as an agent that is
    /// ready immediately: no ACP agent is involved, [`new_session`] yields a
    /// local session id without a host call, and prompts go to EveryAI's AI.
    /// Use it for rewrite / search / ask when no named agent is wanted.
    ///
    /// [`new_session`]: AgentController::new_session
    pub fn attach_prompt_agent(&mut self) -> AgentId {
        let id = self.alloc("EveryAI", AgentState::Ready, None);
        let info = AgentInfo {
            name: "EveryAI".into(),
            ..AgentInfo::default()
        };
        if let Some(s) = self.agents.get(&id) {
            lock(&s.state).info = Some(info.clone());
        }
        (self.sink(id))(AgentEvent::Ready { info });
        id
    }

    /// [`new_session`](AgentController::new_session) with a first message
    /// (`agents.start`'s `prompt`, shown in EveryAI's session UI).
    pub fn new_session_with_prompt(
        &mut self,
        agent: AgentId,
        cwd: &Path,
        prompt: &str,
    ) -> Result<RequestId, AgentError> {
        self.start_session(agent, cwd, Some(prompt.to_owned()))
    }

    /// Start a `prompt.run` turn with full control over mode, selection,
    /// file and context. `args.task_id` is replaced by a generated id. Events
    /// stream as for [`send_prompt`](AgentController::send_prompt); the
    /// authoritative structured result (edits, locations, text) is available
    /// from [`take_result`](Self::take_result) once `TurnFinished` arrives.
    pub fn run_prompt(
        &mut self,
        agent: AgentId,
        session: &SessionId,
        args: PromptRunArgs,
    ) -> Result<TurnId, AgentError> {
        self.ready(agent)?;
        self.require_link()?;
        if lock(&self.turns).values().any(|t| t.session == *session) {
            return Err(AgentError::Busy);
        }
        let turn = TurnId(self.next_turn);
        self.next_turn += 1;
        let task_id = format!("skink-{}-{}", std::process::id(), turn.0);
        let mut args = args;
        args.task_id.clone_from(&task_id);
        if args.model.is_none() {
            args.model.clone_from(&self.model);
        }
        if args.cwd.is_none() {
            args.cwd = self.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
        }
        let client = self.link.client().fork();
        let cancelled = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        lock(&self.turns).insert(
            turn,
            Turn {
                agent,
                session: session.clone(),
                task_id,
                cancelled: Arc::clone(&cancelled),
                done: Arc::clone(&done),
                handle: client.cancel_handle(),
            },
        );
        let ctx = TurnCtx {
            link: Arc::clone(&self.link),
            client,
            sink: self.sink(agent),
            turns: Arc::clone(&self.turns),
            results: Arc::clone(&self.results),
            turn,
            session: session.clone(),
            args,
            cancelled,
            done,
        };
        thread::Builder::new()
            .name("host-prompt".into())
            .spawn(move || run_turn(ctx))
            .map_err(|e| {
                lock(&self.turns).remove(&turn);
                AgentError::Other(e.to_string())
            })?;
        Ok(turn)
    }

    /// The structured `prompt.run` result of a finished turn (once).
    pub fn take_result(&mut self, turn: TurnId) -> Option<tools::PromptRunResult> {
        lock(&self.results).remove(&turn)
    }

    // ------------------------------------------------------------- internals

    fn sink(&self, id: AgentId) -> Sink {
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        Arc::new(move |ev| {
            let _ = tx.send((id, ev));
            if let Some(w) = &waker {
                w();
            }
        })
    }

    fn require_link(&self) -> Result<(), AgentError> {
        if self.link.is_available() {
            Ok(())
        } else {
            Err(unavailable())
        }
    }

    fn alloc(&mut self, label: &str, phase: AgentState, host_id: Option<String>) -> AgentId {
        let id = AgentId(self.next_agent);
        self.next_agent += 1;
        self.agents.insert(
            id,
            Slot {
                label: label.to_owned(),
                state: Arc::new(Mutex::new(SlotState {
                    phase,
                    info: None,
                    host_id,
                })),
            },
        );
        id
    }

    /// The agent's host id (`None` for the prompt-only agent) if it is ready.
    fn ready(&self, agent: AgentId) -> Result<Option<String>, AgentError> {
        let slot = self
            .agents
            .get(&agent)
            .ok_or(AgentError::UnknownAgent(agent))?;
        let st = lock(&slot.state);
        match st.phase {
            AgentState::Ready => Ok(st.host_id.clone()),
            AgentState::Starting => Err(AgentError::NotReady),
            _ => Err(AgentError::Closed),
        }
    }

    fn request_id(&mut self) -> RequestId {
        let r = RequestId(self.next_request);
        self.next_request += 1;
        r
    }

    fn start_session(
        &mut self,
        agent: AgentId,
        cwd: &Path,
        prompt: Option<String>,
    ) -> Result<RequestId, AgentError> {
        let host_id = self.ready(agent)?;
        let request = self.request_id();
        let sink = self.sink(agent);
        let session_info = |id: String| SessionInfo {
            id: SessionId(id),
            modes: Vec::new(),
            current_mode: None,
        };
        let Some(host_id) = host_id else {
            sink(AgentEvent::SessionStarted {
                request,
                session: session_info(format!("everyai-{}", request.0)),
            });
            return Ok(request);
        };
        self.require_link()?;
        let args = tools::AgentsStartArgs {
            agent_id: host_id,
            cwd: Some(cwd.to_string_lossy().into_owned()),
            prompt,
            model: self.model.clone(),
        };
        let link = Arc::clone(&self.link);
        thread::Builder::new()
            .name("host-session".into())
            .spawn(move || {
                let ev = match link.call::<_, tools::AgentsStartResult>(tools::AGENTS_START, &args)
                {
                    Ok(r) => AgentEvent::SessionStarted {
                        request,
                        session: session_info(r.session_id),
                    },
                    Err(e) => AgentEvent::RequestFailed {
                        request,
                        error: error_info(&e),
                    },
                };
                sink(ev);
            })
            .map_err(|e| AgentError::Other(e.to_string()))?;
        Ok(request)
    }

    fn cancel_turns(&self, pred: impl Fn(&Turn) -> bool) {
        for t in lock(&self.turns).values().filter(|t| pred(t)) {
            spawn_cancel(&self.link, t);
        }
    }
}

/// Cancel one turn off-thread: drop its socket (and tell the host through
/// `notifications/cancelled`), retrying briefly in case the call has not
/// reached the wire yet, then ask the host to stop via `prompt.cancel`.
fn spawn_cancel(link: &Arc<HostLink>, t: &Turn) {
    let link = Arc::clone(link);
    let handle = t.handle.clone();
    let cancelled = Arc::clone(&t.cancelled);
    let done = Arc::clone(&t.done);
    let task_id = t.task_id.clone();
    let _ = thread::Builder::new()
        .name("host-cancel".into())
        .spawn(move || {
            cancelled.store(true, Ordering::SeqCst);
            for _ in 0..100 {
                if done.load(Ordering::SeqCst) || handle.cancel() > 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            let quick = link.client().fork_with_timeouts(Timeouts {
                connect: Duration::from_secs(1),
                request: Duration::from_secs(3),
                call_idle: Duration::from_secs(3),
                call_total: Some(Duration::from_secs(5)),
            });
            let _ = link.run_on(tools::PROMPT_CANCEL, &quick, |c| {
                c.call_tool(tools::PROMPT_CANCEL, &json!({"task_id": task_id}))
            });
        });
}

struct TurnCtx {
    link: Arc<HostLink>,
    client: crate::McpClient,
    sink: Sink,
    turns: Turns,
    results: Results,
    turn: TurnId,
    session: SessionId,
    args: PromptRunArgs,
    cancelled: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

/// What was already shown while streaming, so the final result adds only
/// what the stream did not carry.
#[derive(Default)]
struct Streamed {
    text: bool,
    edits: Vec<tools::FileEdit>,
    locations: Vec<tools::Location>,
    calls: usize,
}

fn edit_call(id: String, f: &tools::FileEdit) -> ToolCallInfo {
    ToolCallInfo {
        id,
        title: format!("Edit {} ({} change(s))", f.path, f.edits.len()),
        kind: ToolKind::Edit,
        status: ToolStatus::Completed,
        content: vec![ToolContent::Text(format!(
            "{} edit(s) proposed for {}",
            f.edits.len(),
            f.path
        ))],
        locations: vec![ToolLocation {
            path: PathBuf::from(&f.path),
            line: f.edits.first().map(|e| e.range.start.line + 1),
        }],
    }
}

fn location_call(id: String, l: &tools::Location) -> ToolCallInfo {
    ToolCallInfo {
        id,
        title: format!("{}:{}", l.path, l.range.start.line + 1),
        kind: ToolKind::Search,
        status: ToolStatus::Completed,
        content: Vec::new(),
        locations: vec![ToolLocation {
            path: PathBuf::from(&l.path),
            line: Some(l.range.start.line + 1),
        }],
    }
}

fn run_turn(ctx: TurnCtx) {
    let TurnCtx {
        link,
        client,
        sink,
        turns,
        results,
        turn,
        session,
        args,
        cancelled,
        done,
    } = ctx;
    let task_id = args.task_id.clone();
    // The turn leaves the active set *before* its final event is visible, so
    // the UI may start the next prompt as soon as it sees it.
    let end = |ev: AgentEvent| {
        done.store(true, Ordering::SeqCst);
        lock(&turns).remove(&turn);
        sink(ev);
    };
    let finished = |stop: StopReason| AgentEvent::TurnFinished {
        session: session.clone(),
        turn,
        stop,
    };
    let failed = |error: ErrorInfo| AgentEvent::TurnFailed {
        session: session.clone(),
        turn,
        error,
    };

    if cancelled.load(Ordering::SeqCst) {
        return end(finished(StopReason::Cancelled));
    }
    if let Err(e) = link.begin(tools::PROMPT_RUN) {
        return end(failed(error_info(&e)));
    }
    let args_json = match serde_json::to_value(&args) {
        Ok(v) => v,
        Err(e) => return end(failed(error_info(&LinkError::Host(e.into())))),
    };

    let mut seen = Streamed::default();
    let mut on_progress = |p: &crate::Progress| {
        let Some(ev) = p.message.as_deref().and_then(PromptEvent::parse) else {
            return;
        };
        let msg = |text| AgentEvent::MessageChunk {
            session: session.clone(),
            text,
        };
        match ev {
            PromptEvent::Status { text } => sink(AgentEvent::ThoughtChunk {
                session: session.clone(),
                text: format!("{text}\n"),
            }),
            PromptEvent::TextDelta { text } => {
                seen.text = true;
                sink(msg(text));
            }
            PromptEvent::Edit { file } => {
                seen.calls += 1;
                let call = edit_call(format!("{task_id}#{}", seen.calls), &file);
                seen.edits.push(file);
                sink(AgentEvent::ToolCall {
                    session: session.clone(),
                    call,
                });
            }
            PromptEvent::Location { location } => {
                seen.calls += 1;
                let call = location_call(format!("{task_id}#{}", seen.calls), &location);
                seen.locations.push(location);
                sink(AgentEvent::ToolCall {
                    session: session.clone(),
                    call,
                });
            }
        }
    };
    let outcome = client
        .call_tool_progress(tools::PROMPT_RUN, &args_json, &mut on_progress)
        .and_then(|r| r.parse::<tools::PromptRunResult>());

    match link.finish(tools::PROMPT_RUN, outcome) {
        Ok(result) => {
            // The final result is authoritative: show whatever the stream missed.
            if let Some(t) = result.text.as_deref().filter(|t| !t.is_empty())
                && !seen.text
            {
                sink(AgentEvent::MessageChunk {
                    session: session.clone(),
                    text: t.to_owned(),
                });
            }
            for f in result.files.iter().filter(|f| !seen.edits.contains(f)) {
                seen.calls += 1;
                sink(AgentEvent::ToolCall {
                    session: session.clone(),
                    call: edit_call(format!("{task_id}#{}", seen.calls), f),
                });
            }
            for l in result
                .locations
                .iter()
                .filter(|l| !seen.locations.contains(l))
            {
                seen.calls += 1;
                sink(AgentEvent::ToolCall {
                    session: session.clone(),
                    call: location_call(format!("{task_id}#{}", seen.calls), l),
                });
            }
            {
                let mut store = lock(&results);
                store.insert(turn, result);
                while store.len() > MAX_RESULTS {
                    store.pop_first();
                }
            }
            end(finished(StopReason::EndTurn));
        }
        Err(LinkError::Host(Error::Cancelled)) => end(finished(StopReason::Cancelled)),
        // A cancel that raced with the socket drop shows up as an I/O error.
        Err(_) if cancelled.load(Ordering::SeqCst) => end(finished(StopReason::Cancelled)),
        Err(e) => end(failed(error_info(&e))),
    }
}

impl AgentController for DelegatedAgents {
    fn backend(&self) -> Backend {
        Backend::Delegated
    }

    fn start_agent(&mut self, label: &str, spec: AgentSpec) -> Result<AgentId, AgentError> {
        self.require_link()?;
        let id = self.alloc(label, AgentState::Starting, None);
        let state = Arc::clone(&self.agents[&id].state);
        let link = Arc::clone(&self.link);
        let sink = self.sink(id);
        let label = label.to_owned();
        let spawned = thread::Builder::new()
            .name("host-agent-start".into())
            .spawn(move || match resolve(&link, &label, &spec.program) {
                Ok(host) => {
                    let info = AgentInfo {
                        name: host.name,
                        title: host.description,
                        ..AgentInfo::default()
                    };
                    {
                        let mut st = lock(&state);
                        st.phase = AgentState::Ready;
                        st.info = Some(info.clone());
                        st.host_id = Some(host.id);
                    }
                    sink(AgentEvent::Ready { info });
                }
                Err(message) => {
                    lock(&state).phase = AgentState::Failed;
                    sink(AgentEvent::StartFailed { message });
                }
            });
        if let Err(e) = spawned {
            self.agents.remove(&id);
            return Err(AgentError::Other(e.to_string()));
        }
        Ok(id)
    }

    fn stop_agent(&mut self, agent: AgentId) {
        if let Some(slot) = self.agents.remove(&agent) {
            lock(&slot.state).phase = AgentState::Exited;
            self.cancel_turns(|t| t.agent == agent);
        }
    }

    fn agents(&self) -> Vec<AgentStatus> {
        let mut v: Vec<_> = self
            .agents
            .iter()
            .map(|(id, s)| {
                let st = lock(&s.state);
                AgentStatus {
                    id: *id,
                    label: s.label.clone(),
                    state: st.phase,
                    info: st.info.clone(),
                }
            })
            .collect();
        v.sort_by_key(|a| a.id);
        v
    }

    fn authenticate(&mut self, agent: AgentId, _: &str) -> Result<RequestId, AgentError> {
        self.ready(agent)?;
        Err(AgentError::Unsupported(
            "EveryAI manages agent authentication".into(),
        ))
    }

    fn new_session(&mut self, agent: AgentId, cwd: &Path) -> Result<RequestId, AgentError> {
        self.start_session(agent, cwd, None)
    }

    fn load_session(
        &mut self,
        agent: AgentId,
        _: &SessionId,
        _: &Path,
    ) -> Result<RequestId, AgentError> {
        self.ready(agent)?;
        Err(AgentError::Unsupported(
            "EveryAI owns agent sessions; open them in EveryAI".into(),
        ))
    }

    fn set_mode(&mut self, agent: AgentId, _: &SessionId, _: &str) -> Result<(), AgentError> {
        self.ready(agent)?;
        Err(AgentError::Unsupported(
            "modes are managed in EveryAI".into(),
        ))
    }

    fn send_prompt(
        &mut self,
        agent: AgentId,
        session: &SessionId,
        blocks: Vec<PromptBlock>,
    ) -> Result<TurnId, AgentError> {
        let (instruction, context) = blocks_to_prompt(&blocks)?;
        self.run_prompt(
            agent,
            session,
            PromptRunArgs {
                task_id: String::new(),
                mode: PromptMode::Ask,
                instruction,
                file: None,
                selection: None,
                context,
                model: None,
                cwd: None,
            },
        )
    }

    fn cancel(&mut self, agent: AgentId, session: &SessionId) -> Result<(), AgentError> {
        if !self.agents.contains_key(&agent) {
            return Err(AgentError::UnknownAgent(agent));
        }
        self.cancel_turns(|t| t.agent == agent && t.session == *session);
        Ok(())
    }

    fn poll_events(&mut self) -> Vec<(AgentId, AgentEvent)> {
        let mut out = Vec::new();
        while let Ok((id, ev)) = self.rx.try_recv() {
            // Events of a stopped agent are dropped.
            if self.agents.contains_key(&id) {
                out.push((id, ev));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: &str, name: &str, installed: bool) -> tools::AgentInfo {
        tools::AgentInfo {
            id: id.into(),
            name: name.into(),
            description: None,
            installed,
            running: false,
        }
    }

    #[test]
    fn slugs_and_stems() {
        assert_eq!(slug("Claude Code"), "claude-code");
        assert_eq!(slug("  Gemini  CLI! "), "gemini-cli");
        assert_eq!(program_stem("/usr/bin/goose"), "goose");
        assert_eq!(program_stem("C:\\bin\\agy.exe"), "agy");
    }

    #[test]
    fn picks_by_id_name_slug_then_program() {
        let l = [
            host("claude-code", "Claude", true),
            host("gemini", "Gemini CLI", true),
            host("goose", "Goose", true),
        ];
        let p = |label, prog| pick(&l, label, prog).map(|a| a.id.as_str());
        assert_eq!(p("gemini", ""), Some("gemini"));
        assert_eq!(p("gemini cli", "x"), Some("gemini"));
        assert_eq!(p("Claude Code", "npx"), Some("claude-code"));
        assert_eq!(p("Something", "/usr/bin/goose"), Some("goose"));
        assert_eq!(p("Something", "npx"), None);
        assert_eq!(p("Something", ""), None);
    }

    #[test]
    fn blocks_become_instruction_and_context() {
        let (i, c) = blocks_to_prompt(&[
            PromptBlock::text("explain"),
            PromptBlock::ResourceLink {
                uri: "file:///w/a%20b.rs".into(),
                name: "a b.rs".into(),
                mime_type: None,
            },
            PromptBlock::Resource {
                uri: "file:///w/c.rs".into(),
                text: "fn c() {}".into(),
                mime_type: None,
            },
            PromptBlock::text("this"),
        ])
        .unwrap();
        assert_eq!(i, "explain\n\nthis");
        assert_eq!(c.len(), 2);
        assert_eq!(
            c[0].name,
            "/w/a b.rs".replace('/', std::path::MAIN_SEPARATOR_STR)
        );
        assert_eq!(c[0].text, None);
        assert_eq!(c[1].text.as_deref(), Some("fn c() {}"));
        assert!(blocks_to_prompt(&[PromptBlock::text("  ")]).is_err());
    }
}
