//! `skink-ai`: everything AI. Two halves:
//!
//! * [`acp`] + [`local`]: a lightweight blocking ACP client and the
//!   [`AgentController`] seam the editor talks to. [`LocalAgents`] implements
//!   it over [`acp::AcpClient`]; a `Delegated` implementation (EveryAI MCP
//!   tools `agents.*` / `prompt.run`) lives in `skink-host` and uses only the
//!   ACP-free types in [`agent`].
//! * [`prompter`]: the "99-style" UI-free prompt layer (context assembly,
//!   rewrite/search/worker requests, reply extraction, diffs).

pub mod acp;
pub mod agent;
pub mod local;
pub mod prompter;

pub use agent::*;
pub use local::LocalAgents;

use std::path::Path;

use skink_core::Backend;

/// Why a controller call failed synchronously.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    UnknownAgent(AgentId),
    /// Handshake not finished yet (wait for [`AgentEvent::Ready`]).
    NotReady,
    Spawn(String),
    /// The agent connection is gone.
    Closed,
    /// A turn is already running in that session.
    Busy,
    Unsupported(String),
    Other(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownAgent(id) => write!(f, "unknown agent {}", id.0),
            Self::NotReady => f.write_str("agent is still starting"),
            Self::Spawn(e) => write!(f, "cannot start agent: {e}"),
            Self::Closed => f.write_str("agent has exited"),
            Self::Busy => f.write_str("a turn is already running in this session"),
            Self::Unsupported(e) => write!(f, "unsupported: {e}"),
            Self::Other(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<acp::AcpError> for AgentError {
    fn from(e: acp::AcpError) -> Self {
        match e {
            acp::AcpError::Closed => Self::Closed,
            acp::AcpError::TurnInProgress => Self::Busy,
            acp::AcpError::Unsupported(m) => Self::Unsupported(m),
            other => Self::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Starting,
    Ready,
    Failed,
    Exited,
}

#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub id: AgentId,
    pub label: String,
    pub state: AgentState,
    pub info: Option<AgentInfo>,
}

/// The seam between the editor and "whoever runs agents": object safe,
/// non-blocking, free of ACP wire types. Results of asynchronous calls
/// (`start_agent`, `new_session`, ...) and all streaming arrive from
/// [`poll_events`](Self::poll_events), which the UI loop drains each tick.
///
/// Standalone, [`LocalAgents`] spawns and speaks ACP itself; inside EveryAI a
/// delegated controller forwards the same calls over the host channel.
pub trait AgentController: Send {
    fn backend(&self) -> Backend;

    /// Launch `spec` and handshake in the background. Returns at once with an
    /// id; [`AgentEvent::Ready`] or [`AgentEvent::StartFailed`] follows.
    fn start_agent(&mut self, label: &str, spec: AgentSpec) -> Result<AgentId, AgentError>;

    /// Stop the agent and forget it.
    fn stop_agent(&mut self, agent: AgentId);

    fn agents(&self) -> Vec<AgentStatus>;

    /// Pick an `authMethods` entry reported in [`AgentInfo`]. Result:
    /// [`AgentEvent::Authenticated`] / [`AgentEvent::RequestFailed`].
    fn authenticate(&mut self, agent: AgentId, method_id: &str) -> Result<RequestId, AgentError>;

    /// Result: [`AgentEvent::SessionStarted`] / [`AgentEvent::RequestFailed`].
    fn new_session(&mut self, agent: AgentId, cwd: &Path) -> Result<RequestId, AgentError>;

    /// Resume a stored session (agent must advertise `load_session`).
    fn load_session(
        &mut self,
        agent: AgentId,
        session: &SessionId,
        cwd: &Path,
    ) -> Result<RequestId, AgentError>;

    fn set_mode(
        &mut self,
        agent: AgentId,
        session: &SessionId,
        mode_id: &str,
    ) -> Result<(), AgentError>;

    /// Pick the session's model (from [`SessionInfo::models`]). Result:
    /// [`AgentEvent::ModelChanged`] / [`AgentEvent::RequestFailed`].
    fn set_model(
        &mut self,
        _agent: AgentId,
        _session: &SessionId,
        _model_id: &str,
    ) -> Result<RequestId, AgentError> {
        Err(AgentError::Unsupported(
            "this agent backend cannot switch models".into(),
        ))
    }

    /// Start a turn; chunks, tool calls and finally `TurnFinished`/`TurnFailed`
    /// come back as events.
    fn send_prompt(
        &mut self,
        agent: AgentId,
        session: &SessionId,
        blocks: Vec<PromptBlock>,
    ) -> Result<TurnId, AgentError>;

    /// Cancel the running turn (also cancels pending permission requests).
    fn cancel(&mut self, agent: AgentId, session: &SessionId) -> Result<(), AgentError>;

    /// Drain events that arrived since the last call. Never blocks.
    fn poll_events(&mut self) -> Vec<(AgentId, AgentEvent)>;
}

/// Controller for builds/hosts without agents: every call fails.
pub struct NoAgents(pub Backend);

impl AgentController for NoAgents {
    fn backend(&self) -> Backend {
        self.0
    }
    fn start_agent(&mut self, _: &str, _: AgentSpec) -> Result<AgentId, AgentError> {
        Err(AgentError::Unsupported("no agent backend".into()))
    }
    fn stop_agent(&mut self, _: AgentId) {}
    fn agents(&self) -> Vec<AgentStatus> {
        Vec::new()
    }
    fn authenticate(&mut self, a: AgentId, _: &str) -> Result<RequestId, AgentError> {
        Err(AgentError::UnknownAgent(a))
    }
    fn new_session(&mut self, a: AgentId, _: &Path) -> Result<RequestId, AgentError> {
        Err(AgentError::UnknownAgent(a))
    }
    fn load_session(
        &mut self,
        a: AgentId,
        _: &SessionId,
        _: &Path,
    ) -> Result<RequestId, AgentError> {
        Err(AgentError::UnknownAgent(a))
    }
    fn set_mode(&mut self, a: AgentId, _: &SessionId, _: &str) -> Result<(), AgentError> {
        Err(AgentError::UnknownAgent(a))
    }
    fn send_prompt(
        &mut self,
        a: AgentId,
        _: &SessionId,
        _: Vec<PromptBlock>,
    ) -> Result<TurnId, AgentError> {
        Err(AgentError::UnknownAgent(a))
    }
    fn cancel(&mut self, a: AgentId, _: &SessionId) -> Result<(), AgentError> {
        Err(AgentError::UnknownAgent(a))
    }
    fn poll_events(&mut self) -> Vec<(AgentId, AgentEvent)> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_is_object_safe() {
        let mut c: Box<dyn AgentController> = Box::new(NoAgents(Backend::Local));
        assert_eq!(c.backend(), Backend::Local);
        assert!(c.start_agent("x", AgentSpec::new("y")).is_err());
        assert!(c.poll_events().is_empty());
    }
}
