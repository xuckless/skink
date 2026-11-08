//! [`LocalAgents`]: the standalone [`AgentController`], implemented over
//! [`AcpClient`]. It owns the agents (child processes or host-provided
//! transports), runs handshakes and slow calls on helper threads, and funnels
//! every agent's events into one queue the UI drains with `poll_events`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use skink_core::Backend;

use crate::acp::{
    self, AcpClient, AcpError, AcpHandler, BufferSource, ClientOptions, EventSink, Transport,
    WorkspaceFs,
};
use crate::agent::{
    AgentEvent, AgentId, AgentInfo, AgentSpec, PromptBlock, RequestId, SessionId, TurnId,
};
use crate::{AgentController, AgentError, AgentState, AgentStatus};

/// Builds the client-side handler (fs/terminal policy) for a new agent; the
/// argument is the agent's label.
pub type HandlerFactory = Arc<dyn Fn(&str) -> Arc<dyn AcpHandler> + Send + Sync>;
/// Called (from any thread) whenever a new event is queued, to wake the UI loop.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

struct SlotState {
    phase: AgentState,
    client: Option<Arc<AcpClient>>,
    info: Option<AgentInfo>,
}

struct Slot {
    label: String,
    state: Arc<Mutex<SlotState>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct LocalAgents {
    next_agent: u64,
    next_request: u64,
    agents: HashMap<AgentId, Slot>,
    tx: Sender<(AgentId, AgentEvent)>,
    rx: Receiver<(AgentId, AgentEvent)>,
    handlers: HandlerFactory,
    waker: Option<Waker>,
    opts: ClientOptions,
}

impl LocalAgents {
    pub fn new(handlers: HandlerFactory) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            next_agent: 1,
            next_request: 1,
            agents: HashMap::new(),
            tx,
            rx,
            handlers,
            waker: None,
            opts: ClientOptions::default(),
        }
    }

    /// Agents serve `fs/*` from `root`, preferring `buffers` (unsaved text).
    pub fn for_workspace(
        root: impl Into<std::path::PathBuf>,
        buffers: Arc<dyn BufferSource>,
    ) -> Self {
        let root = root.into();
        Self::new(Arc::new(move |_| {
            Arc::new(WorkspaceFs::new(&root, Arc::clone(&buffers)))
        }))
    }

    #[must_use]
    pub fn with_waker(mut self, waker: Waker) -> Self {
        self.waker = Some(waker);
        self
    }

    #[must_use]
    pub fn with_options(mut self, opts: ClientOptions) -> Self {
        self.opts = opts;
        self
    }

    fn sink(&self, id: AgentId) -> EventSink {
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        Arc::new(move |ev| {
            let _ = tx.send((id, ev));
            if let Some(w) = &waker {
                w();
            }
        })
    }

    /// Adopt a transport the host already set up (agent running in a terminal
    /// pane, a socket, an in-process thread...). Handshake runs in the background.
    pub fn attach(&mut self, label: &str, transport: Transport) -> AgentId {
        let id = AgentId(self.next_agent);
        self.next_agent += 1;
        let state = Arc::new(Mutex::new(SlotState {
            phase: AgentState::Starting,
            client: None,
            info: None,
        }));
        self.agents.insert(
            id,
            Slot {
                label: label.to_owned(),
                state: Arc::clone(&state),
            },
        );
        let handler = (self.handlers)(label);
        let sink = self.sink(id);
        let opts = self.opts.clone();
        let spawned = thread::Builder::new().name("acp-start".into()).spawn({
            let sink = Arc::clone(&sink);
            move || match AcpClient::start_with_sink(transport, handler, Arc::clone(&sink), opts) {
                Ok(c) => {
                    let info = c.info();
                    {
                        let mut st = lock(&state);
                        st.phase = AgentState::Ready;
                        st.info = Some(info.clone());
                        st.client = Some(Arc::new(c));
                    }
                    sink(AgentEvent::Ready { info });
                }
                Err(e) => {
                    lock(&state).phase = AgentState::Failed;
                    sink(AgentEvent::StartFailed {
                        message: e.to_string(),
                    });
                }
            }
        });
        if let Err(e) = spawned {
            if let Some(slot) = self.agents.get(&id) {
                lock(&slot.state).phase = AgentState::Failed;
            }
            (self.sink(id))(AgentEvent::StartFailed {
                message: format!("cannot start handshake thread: {e}"),
            });
        }
        id
    }

    fn client(&self, agent: AgentId) -> Result<Arc<AcpClient>, AgentError> {
        let slot = self
            .agents
            .get(&agent)
            .ok_or(AgentError::UnknownAgent(agent))?;
        let st = lock(&slot.state);
        match (&st.client, st.phase) {
            (Some(c), AgentState::Ready) => Ok(Arc::clone(c)),
            (_, AgentState::Starting) => Err(AgentError::NotReady),
            _ => Err(AgentError::Closed),
        }
    }

    fn request_id(&mut self) -> RequestId {
        let r = RequestId(self.next_request);
        self.next_request += 1;
        r
    }

    /// Run a blocking client call on a helper thread and report as an event.
    fn run_async(
        &mut self,
        agent: AgentId,
        f: impl FnOnce(&AcpClient, RequestId) -> Result<AgentEvent, AcpError> + Send + 'static,
    ) -> Result<RequestId, AgentError> {
        let client = self.client(agent)?;
        let request = self.request_id();
        let sink = self.sink(agent);
        thread::Builder::new()
            .name("acp-call".into())
            .spawn(move || {
                let ev = f(&client, request).unwrap_or_else(|e| AgentEvent::RequestFailed {
                    request,
                    error: e.info(),
                });
                sink(ev);
            })
            .map_err(|e| AgentError::Other(e.to_string()))?;
        Ok(request)
    }
}

impl AgentController for LocalAgents {
    fn backend(&self) -> Backend {
        Backend::Local
    }

    fn start_agent(&mut self, label: &str, spec: AgentSpec) -> Result<AgentId, AgentError> {
        let transport =
            acp::spawn(&spec).map_err(|e| AgentError::Spawn(format!("{}: {e}", spec.program)))?;
        Ok(self.attach(label, transport))
    }

    fn stop_agent(&mut self, agent: AgentId) {
        if let Some(slot) = self.agents.remove(&agent) {
            let client = {
                let mut st = lock(&slot.state);
                st.phase = AgentState::Exited;
                st.client.take()
            };
            drop(client); // Drop closes stdin and kills the process.
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

    fn authenticate(&mut self, agent: AgentId, method_id: &str) -> Result<RequestId, AgentError> {
        let m = method_id.to_owned();
        self.run_async(agent, move |c, request| {
            c.authenticate(&m)?;
            Ok(AgentEvent::Authenticated { request })
        })
    }

    fn new_session(&mut self, agent: AgentId, cwd: &Path) -> Result<RequestId, AgentError> {
        let cwd = cwd.to_path_buf();
        self.run_async(agent, move |c, request| {
            let session = c.new_session(&cwd)?;
            Ok(AgentEvent::SessionStarted { request, session })
        })
    }

    fn load_session(
        &mut self,
        agent: AgentId,
        session: &SessionId,
        cwd: &Path,
    ) -> Result<RequestId, AgentError> {
        let (cwd, session) = (cwd.to_path_buf(), session.clone());
        self.run_async(agent, move |c, request| {
            let session = c.load_session(&session, &cwd, &[])?;
            Ok(AgentEvent::SessionStarted { request, session })
        })
    }

    fn set_mode(
        &mut self,
        agent: AgentId,
        session: &SessionId,
        mode_id: &str,
    ) -> Result<(), AgentError> {
        let (session, mode) = (session.clone(), mode_id.to_owned());
        self.run_async(agent, move |c, _| {
            c.set_mode(&session, &mode)?;
            Ok(AgentEvent::ModeChanged {
                session,
                mode_id: mode,
            })
        })
        .map(|_| ())
    }

    fn send_prompt(
        &mut self,
        agent: AgentId,
        session: &SessionId,
        blocks: Vec<PromptBlock>,
    ) -> Result<TurnId, AgentError> {
        let client = self.client(agent)?;
        let handle = client.prompt(session, &blocks)?;
        Ok(handle.turn)
    }

    fn cancel(&mut self, agent: AgentId, session: &SessionId) -> Result<(), AgentError> {
        Ok(self.client(agent)?.cancel(session)?)
    }

    fn poll_events(&mut self) -> Vec<(AgentId, AgentEvent)> {
        let mut out = Vec::new();
        while let Ok((id, ev)) = self.rx.try_recv() {
            let Some(slot) = self.agents.get(&id) else {
                continue; // stopped agent: drop its trailing events
            };
            if matches!(ev, AgentEvent::Exited { .. }) {
                let mut st = lock(&slot.state);
                if st.phase == AgentState::Ready {
                    st.phase = AgentState::Exited;
                }
                st.client = None;
            }
            out.push((id, ev));
        }
        out
    }
}
