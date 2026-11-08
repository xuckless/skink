//! The "99-style" prompter: a UI-free layer that turns editor state plus a
//! user instruction into an agent prompt, and the agent's answer back into
//! something the editor can show and apply.
//!
//! Concepts follow ThePrimeagen/99 (README/docs only; that repository has no
//! license, so nothing was copied):
//! * [`context`]: `@file` / `#rule` references, AGENTS.md walk-up, selection,
//!   surrounding lines, diagnostics.
//! * [`rewrite`]: replace the selection; strict "only the code" template and a
//!   forgiving extractor ([`extract_code`]).
//! * [`search`]: project search, answer parsed into [`SearchHit`]s.
//! * [`worker`]: tracks one multi-round task and decides whether to prompt again.
//! * [`edit`]: line [`Diff`] with per-block accept/reject for inline review.
//! * [`coords`]: Skink's char-based coordinates vs plain text.
//!
//! Everything is pure (the only I/O is through [`ProjectFiles`]), so it is
//! unit-tested with canned agent output.

pub mod context;
pub mod coords;
pub mod edit;
pub mod rewrite;
pub mod search;
pub mod worker;

pub use context::{
    AgentDoc, Assembled, ContextConfig, Diagnostic, DiskFiles, FileSnapshot, ProjectFiles, RefKind,
    Reference, SelectionContext, Severity, assemble, collect_agent_docs, expand_references,
    extract_references, language_for,
};
pub use coords::{CharPos, CharRange, TextIndex};
pub use edit::{ApplyError, Diff, DiffStats, EditProposal, Segment};
pub use rewrite::{
    BuiltRewrite, ExtractError, ExtractMethod, Extracted, RewriteRequest, build_rewrite,
    extract_code,
};
pub use search::{
    BuiltSearch, SearchHit, SearchRequest, SearchResults, build_search, parse_search_results,
};
pub use worker::{ReplyStatus, StepReport, Worker, WorkerState, parse_tracker};

use crate::agent::PromptBlock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    RewriteSelection,
    Search,
    Worker,
}

/// A fully rendered prompt, ready for `AgentController::send_prompt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub kind: RequestKind,
    pub text: String,
}

impl Prompt {
    /// The single text block to send.
    pub fn into_blocks(self) -> Vec<PromptBlock> {
        vec![PromptBlock::Text(self.text)]
    }
}

/// Accumulates streamed `MessageChunk` text of one turn so the final answer
/// can be handed to the extractors once the turn finishes.
#[derive(Debug, Default, Clone)]
pub struct ReplyBuffer {
    text: String,
}

impl ReplyBuffer {
    pub fn push(&mut self, chunk: &str) {
        self.text.push_str(chunk);
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.text)
    }
}
