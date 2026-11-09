//! Debug Adapter Protocol client for Skink.
//!
//! * [`client`]: one adapter connection (child process over stdio or a
//!   loopback TCP port), threads + channels, never blocking the caller.
//! * [`session`]: the start-up handshake, stop / step cycle and the data a
//!   debugger shows (frames, variables tree, watches, console), headless.
//! * [`adapters`]: gdb, lldb-dap, CodeLLDB, debugpy, delve (js-debug known
//!   but not runnable yet), found on `PATH`.
//!
//! Framing is skink-lsp's (adapted from Lapce, Apache-2.0). Message bodies
//! follow the protocol's JSON schema (MIT); see NOTICES.md.

pub mod adapters;
pub mod client;
#[doc(hidden)]
pub mod fake;
pub mod session;
pub mod types;

pub use adapters::Adapter;
pub use client::{AdapterProcess, Client, Transport};
pub use session::{
    BpResult, BreakKind, BreakpointSpec, Children, ConsoleLine, DebugConfig, LineKind, Notice,
    Phase, RequestKind, Session, State, Stop, VarNode, Watch, WatchValue,
};

/// Called from reader threads when something arrived (wakes the UI loop).
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

#[cfg(test)]
mod tests;
