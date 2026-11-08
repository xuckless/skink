//! Handover to EveryAI. When Skink runs inside EveryAI's terminal, EveryAI's
//! local MCP host is reachable: EveryAI sets `SKINK_HOST_URL` (for example
//! `http://127.0.0.1:41234/mcp`) and `SKINK_HOST_TOKEN` (`eai_host_...`).
//! Skink then becomes an MCP client of it and can delegate LSP and agent work
//! (see [`tools`] and `docs/HANDOVER.md`). Without the variables, or if the
//! host is unreachable, Skink stays standalone ([`Backend::Local`]) and never
//! fails because of it.
//!
//! Security: the URL must be plain `http://` on `127.0.0.1`, `localhost` or
//! `[::1]`; anything else is refused. The token is never logged: it is
//! redacted from every `Debug`/`Display` output in this crate.

mod client;
mod error;
mod http;
pub mod tools;

pub use client::{CancelHandle, Image, McpClient, Progress, Timeouts, ToolInfo, ToolResult};
pub use error::{Error, Result};

use skink_core::Backend;
use std::fmt;

/// Env var holding the host's MCP endpoint, e.g. `http://127.0.0.1:41234/mcp`.
pub const ENV_URL: &str = "SKINK_HOST_URL";
/// Env var holding the bearer token (`eai_host_...`).
pub const ENV_TOKEN: &str = "SKINK_HOST_TOKEN";

/// Where the host is and the credential for it.
#[derive(Clone, PartialEq, Eq)]
pub struct HostEndpoint {
    /// The MCP endpoint URL. Validated (loopback, http) when connecting.
    pub url: String,
    token: String,
}

impl HostEndpoint {
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: token.into(),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Read the endpoint from the process environment.
    pub fn from_env() -> Option<Self> {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let url = get(ENV_URL).filter(|s| !s.trim().is_empty())?;
        let token = get(ENV_TOKEN).filter(|s| !s.is_empty())?;
        Some(Self { url, token })
    }
}

// Never print the token.
impl fmt::Debug for HostEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostEndpoint")
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Which backend should serve LSP and agents, given only whether a host was
/// configured (no I/O). Use [`detect`] to also verify the host answers.
pub fn backend(host: Option<&HostEndpoint>) -> Backend {
    if host.is_some() {
        Backend::Delegated
    } else {
        Backend::Local
    }
}

/// Cheap, lazy detection: `Delegated` if the env vars are present, without
/// touching the network. A later failed call should fall back to `Local`.
pub fn backend_from_env() -> Backend {
    backend(HostEndpoint::from_env().as_ref())
}

/// Connect to the host described by `get` (an env-like lookup), if any.
/// Uses short [`Timeouts::probe`] deadlines for the handshake so a dead host
/// delays start-up by well under two seconds, then switches to the normal
/// ones. `None` means "stay standalone": no variables, non-loopback URL,
/// refused connection, bad token, or a broken host.
pub fn connect_from_vars(get: impl Fn(&str) -> Option<String>) -> Option<McpClient> {
    let ep = HostEndpoint::from_vars(get)?;
    McpClient::connect_with(&ep, Timeouts::probe())
        .ok()
        .map(|c| c.with_timeouts(Timeouts::default()))
}

/// [`connect_from_vars`] on the process environment.
pub fn connect_if_present() -> Option<McpClient> {
    connect_from_vars(|k| std::env::var(k).ok())
}

/// Delegated only if the env is present *and* the host answers the MCP
/// handshake; otherwise `Local`. Drops the connection; call
/// [`connect_if_present`] instead to keep it.
pub fn detect() -> Backend {
    if connect_if_present().is_some() {
        Backend::Delegated
    } else {
        Backend::Local
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn needs_both_vars() {
        assert!(HostEndpoint::from_vars(vars(&[])).is_none());
        assert!(HostEndpoint::from_vars(vars(&[(ENV_URL, "http://127.0.0.1:1/mcp")])).is_none());
        assert!(HostEndpoint::from_vars(vars(&[(ENV_URL, ""), (ENV_TOKEN, "t")])).is_none());
        let h = HostEndpoint::from_vars(vars(&[
            (ENV_URL, "http://127.0.0.1:1/mcp"),
            (ENV_TOKEN, "t"),
        ]))
        .unwrap();
        assert_eq!(backend(Some(&h)), Backend::Delegated);
        assert_eq!(backend(None), Backend::Local);
    }

    #[test]
    fn debug_hides_token() {
        let h = HostEndpoint::from_vars(vars(&[
            (ENV_URL, "http://127.0.0.1:1/mcp"),
            (ENV_TOKEN, "secret"),
        ]))
        .unwrap();
        assert!(!format!("{h:?}").contains("secret"));
    }

    #[test]
    fn unreachable_or_hostile_host_means_standalone() {
        // Nothing listens on port 1 (connection refused), non-loopback is
        // refused without any network access, and absent vars are Local.
        assert!(connect_from_vars(vars(&[])).is_none());
        assert!(
            connect_from_vars(vars(&[
                (ENV_URL, "http://127.0.0.1:1/mcp"),
                (ENV_TOKEN, "t")
            ]))
            .is_none()
        );
        assert!(
            connect_from_vars(vars(&[
                (ENV_URL, "http://example.com/mcp"),
                (ENV_TOKEN, "t")
            ]))
            .is_none()
        );
    }
}
