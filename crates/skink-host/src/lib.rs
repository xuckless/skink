//! Handover to EveryAI. When Skink runs inside EveryAI's terminal, EveryAI
//! sets `SKINK_HOST_SOCKET` (unix socket / Windows named pipe) and
//! `SKINK_HOST_TOKEN`; Skink then talks MCP over that channel and can
//! delegate LSP and agent management. Without them Skink is standalone.
//! The MCP client itself lands with the AI/LSP phases.

use skink_core::Backend;
use std::fmt;

pub const ENV_SOCKET: &str = "SKINK_HOST_SOCKET";
pub const ENV_TOKEN: &str = "SKINK_HOST_TOKEN";

#[derive(Clone, PartialEq, Eq)]
pub struct HostEndpoint {
    pub socket: String,
    token: String,
}

impl HostEndpoint {
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Read the endpoint from the process environment.
    pub fn from_env() -> Option<Self> {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let socket = get(ENV_SOCKET).filter(|s| !s.is_empty())?;
        let token = get(ENV_TOKEN).filter(|s| !s.is_empty())?;
        Some(Self { socket, token })
    }
}

// Never print the token.
impl fmt::Debug for HostEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostEndpoint")
            .field("socket", &self.socket)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Which backend should serve LSP and agents.
pub fn backend(host: Option<&HostEndpoint>) -> Backend {
    if host.is_some() {
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
        assert!(HostEndpoint::from_vars(vars(&[(ENV_SOCKET, "/s")])).is_none());
        assert!(HostEndpoint::from_vars(vars(&[(ENV_SOCKET, ""), (ENV_TOKEN, "t")])).is_none());
        let h = HostEndpoint::from_vars(vars(&[(ENV_SOCKET, "/s"), (ENV_TOKEN, "t")])).unwrap();
        assert_eq!(backend(Some(&h)), Backend::Delegated);
        assert_eq!(backend(None), Backend::Local);
    }

    #[test]
    fn debug_hides_token() {
        let h =
            HostEndpoint::from_vars(vars(&[(ENV_SOCKET, "/s"), (ENV_TOKEN, "secret")])).unwrap();
        assert!(!format!("{h:?}").contains("secret"));
    }
}
