//! Skink core: text buffer, cursor, command registry. No UI or I/O beyond
//! reading files, so everything here is testable headless.

pub mod buffer;
pub mod command;
pub mod editor;
mod grapheme;
pub mod history;
pub mod lineops;
pub mod location;
pub mod multi;
pub mod search;
pub mod smart;

pub use buffer::{Access, Buffer, DiskChange, Encoding, LineEnding, ReadOnly};
pub use command::{Command, CommandRegistry};
pub use editor::{Conflict, Cursor, Editor, Request, Resolution};
pub use lineops::TextCase;
pub use location::{Location, parse_location};
pub use multi::{Order, Scope, Sel};
pub use search::{Case, MatchCount, SearchOptions, Within};
pub use smart::{BlockStart, Indent};

/// Who provides an external capability (LSP, ACP agents, terminal).
///
/// Every such seam is a trait with a `Local` implementation (Skink does the
/// work itself) and a `Delegated` one (EveryAI does it and Skink asks over the
/// host channel). See `skink-host`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Local,
    Delegated,
}
