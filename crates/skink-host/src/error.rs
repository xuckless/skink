//! The error type for the whole crate. Nothing here ever contains the bearer
//! token.

use std::fmt;
use std::io;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong talking to the EveryAI host.
#[derive(Debug)]
pub enum Error {
    /// The URL is malformed or not plain `http://`.
    InvalidUrl(String),
    /// The URL (or what it resolves to) is not loopback. Refused.
    NotLoopback(String),
    /// The token contains characters that cannot go in a header.
    InvalidToken,
    /// HTTP 401/403: the host rejected the token or the Host header.
    Unauthorized,
    /// Any other non-2xx HTTP status.
    Http { status: u16, message: String },
    /// The host answered with a JSON-RPC error object.
    Rpc { code: i64, message: String },
    /// The tool ran but reported `isError: true`; the text is its message.
    Tool(String),
    /// The tool succeeded but its result does not match the contract type.
    /// The host is alive (this never trips a [`HostLink`](crate::HostLink)).
    BadResult(String),
    /// A connect/read deadline passed.
    Timeout,
    /// The call was cancelled through a [`CancelHandle`](crate::CancelHandle).
    Cancelled,
    /// Socket-level failure.
    Io(io::Error),
    /// The host said something that is not valid HTTP / MCP / JSON.
    Protocol(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidUrl(m) => write!(f, "invalid host url: {m}"),
            Error::NotLoopback(u) => write!(f, "refusing non-loopback host url: {u}"),
            Error::InvalidToken => f.write_str("host token contains invalid characters"),
            Error::Unauthorized => f.write_str("host rejected the token (unauthorized)"),
            Error::Http { status, message } => write!(f, "host returned HTTP {status}: {message}"),
            Error::Rpc { code, message } => write!(f, "host error {code}: {message}"),
            Error::Tool(m) => write!(f, "host tool failed: {m}"),
            Error::BadResult(m) => write!(f, "host tool returned an unexpected result: {m}"),
            Error::Timeout => f.write_str("timed out talking to the host"),
            Error::Cancelled => f.write_str("call cancelled"),
            Error::Io(e) => write!(f, "host i/o error: {e}"),
            Error::Protocol(m) => write!(f, "host protocol error: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Error::Timeout,
            io::ErrorKind::InvalidData => Error::Protocol(e.to_string()),
            _ => Error::Io(e),
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Protocol(format!("bad json: {e}"))
    }
}
