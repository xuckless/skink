//! Agent Client Protocol (ACP) client: Skink is the *client*, agents such as
//! `claude-code-acp`, `gemini --experimental-acp`, `agy acp` or `goose acp`
//! are the servers, speaking newline-delimited JSON-RPC 2.0 over stdio.
//!
//! See `docs/ACP.md` for the supported subset and how a host launches agents.

mod client;
pub mod handler;
pub mod transport;
pub mod wire;

pub use client::{
    AcpClient, AcpError, AcpEvent, ClientOptions, EventSink, McpServer, TurnHandle, abs_path_string,
};
pub use handler::{
    AcpHandler, BufferSource, HandlerError, NoBuffers, NoHandler, TerminalExit, TerminalHost,
    TerminalOutput, TerminalSpec, WorkspaceFs,
};
pub use transport::{AgentProcess, Transport, memory_pipe, spawn};
