//! The tool contract Skink expects EveryAI's host to serve. Names and the JSON
//! shapes of arguments and results; the prose spec is `docs/HANDOVER.md`.
//!
//! Conventions (all tools):
//! * Arguments are the MCP `arguments` object, serialised from the `*Args`
//!   structs below. Field names are `snake_case`.
//! * A successful result is a single `text` content block holding one JSON
//!   document (the `*Result` type). `structuredContent`, if present, wins.
//!   `isError: true` means the text is a plain human readable message.
//! * Positions are LSP positions: zero-based `line`, `character` in UTF-16
//!   code units. Paths are absolute, native file system paths.
//! * `text` on a [`Doc`] is the unsaved buffer content; when present the host
//!   must treat it as the document's current content (`didChange`).

use serde::{Deserialize, Serialize};

pub const LSP_HOVER: &str = "lsp.hover";
pub const LSP_DEFINITION: &str = "lsp.definition";
pub const LSP_REFERENCES: &str = "lsp.references";
pub const LSP_DIAGNOSTICS: &str = "lsp.diagnostics";
pub const LSP_COMPLETION: &str = "lsp.completion";
pub const LSP_RENAME: &str = "lsp.rename";
pub const LSP_FORMAT: &str = "lsp.format";
pub const PROMPT_RUN: &str = "prompt.run";
pub const PROMPT_CANCEL: &str = "prompt.cancel";
pub const MODELS_LIST: &str = "models.list";
pub const AGENTS_LIST: &str = "agents.list";
pub const AGENTS_START: &str = "agents.start";

/// Every tool in the contract.
pub const ALL: &[&str] = &[
    LSP_HOVER,
    LSP_DEFINITION,
    LSP_REFERENCES,
    LSP_DIAGNOSTICS,
    LSP_COMPLETION,
    LSP_RENAME,
    LSP_FORMAT,
    PROMPT_RUN,
    PROMPT_CANCEL,
    MODELS_LIST,
    AGENTS_LIST,
    AGENTS_START,
];

/// Zero-based LSP position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    pub range: Range,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

/// Edits to one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEdit {
    pub path: String,
    pub edits: Vec<TextEdit>,
}

/// A document, optionally with its unsaved content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doc {
    pub path: String,
    /// Current buffer content if it differs from disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Monotonic buffer version, if the client tracks one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
}

/// A cursor position in a document: the argument shared by most `lsp.*` tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionArgs {
    #[serde(flatten)]
    pub doc: Doc,
    pub line: u32,
    pub character: u32,
}

// ------------------------------------------------------------------ lsp

/// `lsp.hover` args: [`PositionArgs`]. Result: `null` or [`HoverResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoverResult {
    /// Markdown.
    pub contents: String,
    #[serde(default)]
    pub range: Option<Range>,
}

/// `lsp.definition` args: [`PositionArgs`]. Result: `Vec<Location>`.
pub type DefinitionResult = Vec<Location>;

/// `lsp.references` args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferencesArgs {
    #[serde(flatten)]
    pub pos: PositionArgs,
    #[serde(default)]
    pub include_declaration: bool,
}
/// `lsp.references` result.
pub type ReferencesResult = Vec<Location>;

/// `lsp.diagnostics` args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsArgs {
    #[serde(flatten)]
    pub doc: Doc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: Severity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}
/// `lsp.diagnostics` result.
pub type DiagnosticsResult = Vec<Diagnostic>;

/// `lsp.completion` args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionArgs {
    #[serde(flatten)]
    pub pos: PositionArgs,
    /// The character that triggered completion, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_character: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionItem {
    pub label: String,
    /// LSP `CompletionItemKind` number, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_text: Option<String>,
    /// If set, replaces `insert_text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_edit: Option<TextEdit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_text: Option<String>,
}

/// `lsp.completion` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionResult {
    #[serde(default)]
    pub is_incomplete: bool,
    pub items: Vec<CompletionItem>,
}

/// `lsp.rename` args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameArgs {
    #[serde(flatten)]
    pub pos: PositionArgs,
    pub new_name: String,
}
/// `lsp.rename` result: edits for every affected file (possibly many).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEdit {
    pub files: Vec<FileEdit>,
}

/// `lsp.format` args. With `range` set, format only that range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatArgs {
    #[serde(flatten)]
    pub doc: Doc,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_spaces: Option<bool>,
}
/// `lsp.format` result.
pub type FormatResult = Vec<TextEdit>;

// --------------------------------------------------------------- prompt

/// What `prompt.run` should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptMode {
    /// Rewrite the selection / file: result carries `files` edits.
    Edit,
    /// Project search: result carries `locations`.
    Search,
    /// Free form question: result carries `text`.
    Ask,
}

/// An `@file` / `#rule` / literal context item attached to the prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRef {
    pub kind: ContextKind,
    /// File path for `file`, rule name for `rule`, free label for `text`.
    pub name: String,
    /// Inline content; if absent the host resolves `name` itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextKind {
    /// `@path`
    File,
    /// `#rule`
    Rule,
    Text,
}

/// The selection the instruction applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub range: Range,
    pub text: String,
}

/// `prompt.run` args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRunArgs {
    /// Client generated id, used by [`PROMPT_CANCEL`] and echoed in events.
    pub task_id: String,
    pub mode: PromptMode,
    pub instruction: String,
    /// The file being edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<Doc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Selection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<ContextRef>,
    /// Model id from [`MODELS_LIST`]; host default if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Project root, for `@`/`#` resolution and search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// `prompt.run` final result. Exactly the field matching the mode is filled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRunResult {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<Location>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A streamed event, carried as JSON in the `message` of a
/// `notifications/progress` while `prompt.run` is running. Purely for live
/// display; the final [`PromptRunResult`] is authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PromptEvent {
    Status { text: String },
    TextDelta { text: String },
    Edit { file: FileEdit },
    Location { location: Location },
}

impl PromptEvent {
    /// Parse a progress `message`; `None` if it is not a prompt event.
    pub fn parse(message: &str) -> Option<PromptEvent> {
        serde_json::from_str(message).ok()
    }
}

/// `prompt.cancel` args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCancelArgs {
    pub task_id: String,
}
/// `prompt.cancel` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCancelResult {
    /// False if no such task was running.
    pub cancelled: bool,
}

// ---------------------------------------------------------------- models

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default)]
    pub default: bool,
}
/// `models.list` result (args: `{}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsResult {
    pub models: Vec<Model>,
}

// ---------------------------------------------------------------- agents

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    /// Stable id, e.g. `claude-code`, `gemini`.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The agent binary is available on this machine.
    #[serde(default)]
    pub installed: bool,
    /// A session of it is currently running.
    #[serde(default)]
    pub running: bool,
}
/// `agents.list` result (args: `{}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsListResult {
    pub agents: Vec<AgentInfo>,
}

/// `agents.start` args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsStartArgs {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Initial user message, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}
/// `agents.start` result: EveryAI owns the ACP session and shows it in its
/// own UI; Skink only gets an id to refer to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsStartResult {
    pub session_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_are_unique_and_dotted() {
        let mut v = ALL.to_vec();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), ALL.len());
        assert!(ALL.iter().all(|n| n.contains('.')));
    }

    #[test]
    fn position_args_flatten() {
        let a = PositionArgs {
            doc: Doc {
                path: "/a.rs".into(),
                text: None,
                version: Some(3),
            },
            line: 1,
            character: 2,
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(
            v,
            json!({"path":"/a.rs","version":3,"line":1,"character":2})
        );
        assert_eq!(serde_json::from_value::<PositionArgs>(v).unwrap(), a);
        let r = ReferencesArgs {
            pos: a,
            include_declaration: true,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["include_declaration"], true);
        assert_eq!(v["line"], 1);
        assert_eq!(serde_json::from_value::<ReferencesArgs>(v).unwrap(), r);
    }

    #[test]
    fn prompt_event_roundtrip() {
        let e = PromptEvent::Edit {
            file: FileEdit {
                path: "/a".into(),
                edits: vec![],
            },
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"edit\""));
        assert_eq!(PromptEvent::parse(&s), Some(e));
        assert_eq!(PromptEvent::parse("working..."), None);
        assert_eq!(
            PromptEvent::parse(r#"{"type":"text_delta","text":"x"}"#),
            Some(PromptEvent::TextDelta { text: "x".into() })
        );
    }

    #[test]
    fn severity_and_mode_wire_names() {
        assert_eq!(
            serde_json::to_string(&Severity::Warning).unwrap(),
            "\"warning\""
        );
        assert_eq!(
            serde_json::to_string(&PromptMode::Search).unwrap(),
            "\"search\""
        );
        let r: PromptRunResult = serde_json::from_str(r#"{"task_id":"t"}"#).unwrap();
        assert!(r.files.is_empty() && r.text.is_none());
    }
}
