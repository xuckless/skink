//! The takeover side: Skink's [`LspProvider`](skink_lsp::LspProvider) and
//! [`AgentController`](skink_ai::AgentController) implemented over the
//! EveryAI host's MCP tools ([`crate::tools`], `docs/HANDOVER.md`).
//!
//! [`HostLink`] is the shared health gate: it counts consecutive connection
//! failures and, after too many, makes every call return "unavailable" at
//! once (no network) until a cool-down passes and one probe call is let
//! through. Calls the host answers (even with an error) prove it is alive.

mod agents;
mod doc;
mod lsp;

pub use agents::DelegatedAgents;
pub use lsp::{DelegatedLsp, lsp_timeouts};

use crate::client::McpClient;
use crate::error::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// When to give up on a host that stopped answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkPolicy {
    /// This many consecutive connection-level failures (refused, timed out,
    /// garbled, 401, ...) trip the link. Minimum 1.
    pub max_failures: u32,
    /// While tripped, one probe call is allowed through per this interval.
    pub retry_after: Duration,
}

impl Default for LinkPolicy {
    fn default() -> Self {
        LinkPolicy {
            max_failures: 3,
            retry_after: Duration::from_secs(30),
        }
    }
}

/// Why a delegated call did not produce a host answer.
#[derive(Debug)]
pub enum LinkError {
    /// The link is tripped (host unreachable): nothing was sent.
    Unavailable,
    /// The host does not offer this tool: nothing was sent (or it answered
    /// "unknown tool" once and is not asked again).
    ToolMissing(&'static str),
    /// The call went out and failed.
    Host(Error),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::Unavailable => f.write_str("EveryAI host unavailable"),
            LinkError::ToolMissing(t) => write!(f, "EveryAI host has no tool {t}"),
            LinkError::Host(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for LinkError {}

/// The host went away or talks nonsense (as opposed to answering with an
/// error, which proves it is alive).
fn is_connection_failure(e: &Error) -> bool {
    matches!(
        e,
        Error::Io(_)
            | Error::Timeout
            | Error::Protocol(_)
            | Error::Http { .. }
            | Error::Unauthorized
    )
}

/// JSON-RPC "method not found" / "unknown tool".
fn is_tool_missing(e: &Error) -> bool {
    match e {
        Error::Rpc { code: -32601, .. } => true,
        Error::Rpc {
            code: -32602,
            message,
        } => {
            let m = message.to_ascii_lowercase();
            m.contains("unknown tool") || m.contains("tool not found") || m.contains("no such tool")
        }
        _ => false,
    }
}

#[derive(Default)]
struct GateState {
    consecutive: u32,
    tripped_at: Option<Instant>,
    missing: Vec<&'static str>,
    last_error: Option<String>,
}

/// The failure bookkeeping, separate from the client so it can be tested
/// without I/O.
struct Gate {
    policy: LinkPolicy,
    state: Mutex<GateState>,
}

impl Gate {
    fn new(policy: LinkPolicy) -> Self {
        Gate {
            policy: LinkPolicy {
                max_failures: policy.max_failures.max(1),
                ..policy
            },
            state: Mutex::new(GateState::default()),
        }
    }

    fn is_available(&self) -> bool {
        let st = lock(&self.state);
        st.consecutive < self.policy.max_failures
            || st
                .tripped_at
                .is_none_or(|t| t.elapsed() >= self.policy.retry_after)
    }

    fn begin(&self, tool: &'static str) -> std::result::Result<(), LinkError> {
        let mut st = lock(&self.state);
        if st.missing.contains(&tool) {
            return Err(LinkError::ToolMissing(tool));
        }
        if st.consecutive >= self.policy.max_failures {
            match st.tripped_at {
                Some(t) if t.elapsed() < self.policy.retry_after => {
                    return Err(LinkError::Unavailable);
                }
                _ => st.tripped_at = Some(Instant::now()),
            }
        }
        Ok(())
    }

    fn finish<T>(&self, tool: &'static str, r: Result<T>) -> std::result::Result<T, LinkError> {
        let mut st = lock(&self.state);
        match &r {
            Ok(_) => {
                st.consecutive = 0;
                st.tripped_at = None;
            }
            Err(e) if is_connection_failure(e) => {
                st.consecutive = st.consecutive.saturating_add(1);
                st.last_error = Some(e.to_string());
                if st.consecutive >= self.policy.max_failures {
                    st.tripped_at = Some(Instant::now());
                }
            }
            Err(Error::Cancelled) => {}
            Err(e) => {
                // The host answered: it is alive, the call itself failed.
                st.consecutive = 0;
                st.tripped_at = None;
                if is_tool_missing(e) {
                    if !st.missing.contains(&tool) {
                        st.missing.push(tool);
                    }
                    return Err(LinkError::ToolMissing(tool));
                }
            }
        }
        drop(st);
        r.map_err(LinkError::Host)
    }
}

/// An [`McpClient`] plus the failure bookkeeping shared by everything that
/// uses it. Cheap to share behind an `Arc`.
pub struct HostLink {
    client: McpClient,
    gate: Gate,
}

impl fmt::Debug for HostLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let st = lock(&self.gate.state);
        f.debug_struct("HostLink")
            .field("client", &self.client)
            .field("consecutive_failures", &st.consecutive)
            .field("tripped", &st.tripped_at.is_some())
            .finish()
    }
}

impl HostLink {
    pub fn new(client: McpClient) -> Self {
        Self::with_policy(client, LinkPolicy::default())
    }

    pub fn with_policy(client: McpClient, policy: LinkPolicy) -> Self {
        HostLink {
            client,
            gate: Gate::new(policy),
        }
    }

    pub fn client(&self) -> &McpClient {
        &self.client
    }

    /// True unless the link is tripped and no probe is due yet. Does not
    /// claim the probe.
    pub fn is_available(&self) -> bool {
        self.gate.is_available()
    }

    /// The last connection-level failure, for a status line.
    pub fn last_error(&self) -> Option<String> {
        lock(&self.gate.state).last_error.clone()
    }

    /// Is `tool` known to be missing on the host?
    pub fn is_tool_missing(&self, tool: &str) -> bool {
        lock(&self.gate.state).missing.contains(&tool)
    }

    /// Ask the host which tools it offers and remember the ones from
    /// [`crate::tools::ALL`] that are absent (they then answer
    /// [`LinkError::ToolMissing`] without I/O). Optional; blocking. Returns
    /// the host's tool names.
    pub fn probe_tools(&self) -> Result<Vec<String>> {
        let names: Vec<String> = self
            .client
            .list_tools()?
            .into_iter()
            .map(|t| t.name)
            .collect();
        lock(&self.gate.state).missing = crate::tools::ALL
            .iter()
            .copied()
            .filter(|t| !names.iter().any(|n| n == t))
            .collect();
        Ok(names)
    }

    /// Gate a call: `Err` if it must not go out. Claims the probe slot when
    /// the link is tripped and the cool-down is over.
    pub(crate) fn begin(&self, tool: &'static str) -> std::result::Result<(), LinkError> {
        self.gate.begin(tool)
    }

    /// Record the outcome of a call gated by [`begin`](Self::begin).
    pub(crate) fn finish<T>(
        &self,
        tool: &'static str,
        r: Result<T>,
    ) -> std::result::Result<T, LinkError> {
        self.gate.finish(tool, r)
    }

    /// Run `f` on `client` (this link's client or a fork of it) under the
    /// failure bookkeeping.
    pub(crate) fn run_on<T>(
        &self,
        tool: &'static str,
        client: &McpClient,
        f: impl FnOnce(&McpClient) -> Result<T>,
    ) -> std::result::Result<T, LinkError> {
        self.begin(tool)?;
        self.finish(tool, f(client))
    }

    /// Typed call of a contract tool.
    pub fn call<A: Serialize, R: DeserializeOwned>(
        &self,
        tool: &'static str,
        args: &A,
    ) -> std::result::Result<R, LinkError> {
        self.run_on(tool, &self.client, |c| c.call_typed(tool, args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io_err() -> Error {
        Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
    }

    #[test]
    fn trips_after_n_failures_and_probes_after_cooldown() {
        let l = Gate::new(LinkPolicy {
            max_failures: 2,
            retry_after: Duration::from_millis(80),
        });
        assert!(l.begin("lsp.hover").is_ok());
        assert!(l.finish::<()>("lsp.hover", Err(io_err())).is_err());
        assert!(l.begin("lsp.hover").is_ok());
        assert!(l.finish::<()>("lsp.hover", Err(Error::Timeout)).is_err());
        assert!(!l.is_available());
        assert!(matches!(l.begin("lsp.hover"), Err(LinkError::Unavailable)));
        assert!(lock(&l.state).last_error.is_some());

        std::thread::sleep(Duration::from_millis(100));
        assert!(l.is_available());
        assert!(l.begin("lsp.hover").is_ok(), "probe let through");
        assert!(
            matches!(l.begin("lsp.hover"), Err(LinkError::Unavailable)),
            "only one probe per window"
        );
        // A failed probe keeps it tripped; a success resets it.
        assert!(l.finish::<()>("lsp.hover", Err(io_err())).is_err());
        assert!(matches!(l.begin("lsp.hover"), Err(LinkError::Unavailable)));
        std::thread::sleep(Duration::from_millis(100));
        assert!(l.begin("lsp.hover").is_ok());
        assert_eq!(l.finish("lsp.hover", Ok(1)).unwrap(), 1);
        assert!(l.begin("lsp.hover").is_ok() && l.begin("lsp.hover").is_ok());
    }

    #[test]
    fn host_errors_prove_liveness_and_missing_tools_are_remembered() {
        let l = Gate::new(LinkPolicy {
            max_failures: 2,
            retry_after: Duration::from_secs(60),
        });
        let _ = l.finish::<()>("lsp.hover", Err(io_err()));
        let _ = l.finish::<()>("lsp.hover", Err(Error::Tool("no server".into())));
        let _ = l.finish::<()>("lsp.hover", Err(io_err()));
        assert!(l.is_available(), "tool error reset the count");

        let _ = l.finish::<()>(
            "lsp.rename",
            Err(Error::Rpc {
                code: -32602,
                message: "Unknown tool: lsp.rename".into(),
            }),
        );
        assert!(matches!(
            l.begin("lsp.rename"),
            Err(LinkError::ToolMissing("lsp.rename"))
        ));
        assert!(l.begin("lsp.hover").is_ok());
        // Cancellation says nothing about the host.
        let _ = l.finish::<()>("lsp.hover", Err(io_err()));
        let _ = l.finish::<()>("lsp.hover", Err(Error::Cancelled));
        assert!(l.is_available(), "cancelled calls are not counted");
        let _ = l.finish::<()>("lsp.hover", Err(io_err()));
        assert!(!l.is_available());
    }
}
