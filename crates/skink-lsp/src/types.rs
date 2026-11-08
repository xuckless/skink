//! Skink-owned LSP result types. All coordinates are 0-based lines and
//! Unicode-scalar ("char") columns, i.e. what `skink_core::Cursor` uses; the
//! UTF-16 conversion happens inside the client. The types are plain data with
//! serde derives so an alternative provider (EveryAI MCP tools) can fill them
//! straight from JSON.

use std::{fmt, path::PathBuf, sync::Arc};

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Position {
    pub line: usize,
    pub col: usize,
}

impl Position {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

impl From<skink_core::Cursor> for Position {
    fn from(c: skink_core::Cursor) -> Self {
        Self {
            line: c.line,
            col: c.col,
        }
    }
}

impl From<Position> for skink_core::Cursor {
    fn from(p: Position) -> Self {
        Self {
            line: p.line,
            col: p.col,
        }
    }
}

/// Half-open range, `end` exclusive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }
}

/// A place in a file (not the CLI `path:line:col` type in `skink_core`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Location {
    pub path: PathBuf,
    pub range: Range,
}

impl Location {
    pub fn line(&self) -> usize {
        self.range.start.line
    }
    pub fn col(&self) -> usize {
        self.range.start.col
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
    pub source: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hover {
    /// Markdown or plain text, as sent by the server.
    pub text: String,
    pub range: Option<Range>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

/// Union of the LSP `CompletionItemKind` and `SymbolKind` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ItemKind {
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Namespace,
    Package,
    Property,
    Unit,
    Value,
    Enum,
    Keyword,
    Snippet,
    Color,
    File,
    Reference,
    Folder,
    EnumMember,
    Constant,
    Struct,
    Event,
    Operator,
    TypeParameter,
    String,
    Number,
    Boolean,
    Array,
    Object,
    Key,
    Null,
    Other,
}

impl ItemKind {
    pub fn from_completion(k: u32) -> Self {
        use ItemKind::*;
        const T: [ItemKind; 25] = [
            Text,
            Method,
            Function,
            Constructor,
            Field,
            Variable,
            Class,
            Interface,
            Module,
            Property,
            Unit,
            Value,
            Enum,
            Keyword,
            Snippet,
            Color,
            File,
            Reference,
            Folder,
            EnumMember,
            Constant,
            Struct,
            Event,
            Operator,
            TypeParameter,
        ];
        k.checked_sub(1)
            .and_then(|i| T.get(i as usize))
            .copied()
            .unwrap_or(Other)
    }

    pub fn from_symbol(k: u32) -> Self {
        use ItemKind::*;
        const T: [ItemKind; 26] = [
            File,
            Module,
            Namespace,
            Package,
            Class,
            Method,
            Property,
            Field,
            Constructor,
            Enum,
            Interface,
            Function,
            Variable,
            Constant,
            String,
            Number,
            Boolean,
            Array,
            Object,
            Key,
            Null,
            EnumMember,
            Struct,
            Event,
            Operator,
            TypeParameter,
        ];
        k.checked_sub(1)
            .and_then(|i| T.get(i as usize))
            .copied()
            .unwrap_or(Other)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionItem {
    pub label: String,
    /// Text to insert when no `edit` is given (already plain text: Skink does
    /// not advertise snippet support).
    pub insert_text: String,
    pub kind: ItemKind,
    pub detail: Option<String>,
    pub sort_text: Option<String>,
    /// Range to replace (usually the identifier prefix) with the new text.
    pub edit: Option<TextEdit>,
    pub additional_edits: Vec<TextEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parameter {
    pub label: String,
    pub documentation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<Parameter>,
    pub active_parameter: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureHelp {
    pub signatures: Vec<Signature>,
    pub active_signature: usize,
    pub active_parameter: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSymbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: ItemKind,
    pub range: Range,
    pub selection_range: Range,
    pub children: Vec<DocumentSymbol>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEdit {
    pub path: PathBuf,
    pub edits: Vec<TextEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileOp {
    Create { path: PathBuf },
    Rename { from: PathBuf, to: PathBuf },
    Delete { path: PathBuf },
}

/// Multi-file edit (rename, code action, `workspace/applyEdit`). Apply `ops`
/// (create/rename/delete) first, then the text edits in `files`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEdit {
    pub files: Vec<FileEdit>,
    pub ops: Vec<FileOp>,
}

impl WorkspaceEdit {
    pub fn is_empty(&self) -> bool {
        self.files.iter().all(|f| f.edits.is_empty()) && self.ops.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormattingOptions {
    pub tab_size: u32,
    pub insert_spaces: bool,
}

impl Default for FormattingOptions {
    fn default() -> Self {
        Self {
            tab_size: 4,
            insert_spaces: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LogLevel {
    Error,
    Warning,
    Info,
    Log,
    /// Line written by the server process to stderr.
    Stderr,
}

/// Asynchronous server -> client traffic. `server` is the server name
/// (program stem, e.g. `rust-analyzer`).
#[derive(Debug, Clone, PartialEq)]
pub enum LspEvent {
    /// Full replacement of the diagnostics for `path` (empty = cleared).
    Diagnostics {
        server: Arc<str>,
        path: PathBuf,
        version: Option<i32>,
        diagnostics: Vec<Diagnostic>,
    },
    /// `window/logMessage` / `window/showMessage` (`show`) / stderr line.
    Message {
        server: Arc<str>,
        level: LogLevel,
        text: String,
        show: bool,
    },
    /// `$/progress` work-done progress (indexing, building, ...).
    Progress {
        server: Arc<str>,
        token: String,
        title: Option<String>,
        message: Option<String>,
        percentage: Option<u32>,
        done: bool,
    },
    /// `workspace/applyEdit`: the server asked Skink to apply `edit`. The
    /// client has already answered `applied: true`, so the receiver must
    /// apply it (or surface the failure itself).
    ApplyEdit {
        server: Arc<str>,
        label: Option<String>,
        edit: WorkspaceEdit,
    },
    /// The server process / connection is gone. `expected` is true for a
    /// requested shutdown.
    Exited {
        server: Arc<str>,
        code: Option<i32>,
        expected: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspError {
    /// No server is configured for this file type.
    NoServer(String),
    /// Server binary is not installed (program name).
    NotInstalled(String),
    /// The request did not complete in time (it was cancelled).
    Timeout,
    /// The server exited or the pipe closed.
    Disconnected,
    /// The server answered with a JSON-RPC error.
    Server { code: i64, message: String },
    /// Malformed message or unexpected result shape.
    Protocol(String),
    /// Spawn / pipe I/O failure.
    Io(String),
    /// Document is not open (call `did_open` first).
    NotOpen(PathBuf),
    /// Provider does not implement this operation.
    Unsupported(&'static str),
}

impl LspError {
    /// Servers answer `ContentModified` (-32801) / `ServerCancelled` (-32802)
    /// / `RequestCancelled` (-32800) when the document changed under a
    /// request; callers usually just retry or drop those.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            LspError::Server {
                code: -32802..=-32800,
                ..
            }
        )
    }
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LspError::NoServer(l) => write!(f, "no language server for {l}"),
            LspError::NotInstalled(p) => write!(f, "language server `{p}` is not installed"),
            LspError::Timeout => f.write_str("language server request timed out"),
            LspError::Disconnected => f.write_str("language server disconnected"),
            LspError::Server { code, message } => {
                write!(f, "language server error {code}: {message}")
            }
            LspError::Protocol(m) => write!(f, "protocol error: {m}"),
            LspError::Io(m) => write!(f, "i/o error: {m}"),
            LspError::NotOpen(p) => write!(f, "document not open: {}", p.display()),
            LspError::Unsupported(w) => write!(f, "unsupported: {w}"),
        }
    }
}

impl std::error::Error for LspError {}

impl From<std::io::Error> for LspError {
    fn from(e: std::io::Error) -> Self {
        LspError::Io(e.to_string())
    }
}

pub type Result<T, E = LspError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_map() {
        assert_eq!(ItemKind::from_completion(3), ItemKind::Function);
        assert_eq!(ItemKind::from_completion(25), ItemKind::TypeParameter);
        assert_eq!(ItemKind::from_completion(0), ItemKind::Other);
        assert_eq!(ItemKind::from_completion(99), ItemKind::Other);
        assert_eq!(ItemKind::from_symbol(12), ItemKind::Function);
        assert_eq!(ItemKind::from_symbol(23), ItemKind::Struct);
        assert_eq!(ItemKind::from_symbol(26), ItemKind::TypeParameter);
    }

    #[test]
    fn transient() {
        assert!(
            LspError::Server {
                code: -32801,
                message: String::new()
            }
            .is_transient()
        );
        assert!(
            !LspError::Server {
                code: -32601,
                message: String::new()
            }
            .is_transient()
        );
    }

    #[test]
    fn json_roundtrip() {
        let h = Hover {
            text: "x".into(),
            range: Some(Range::new(Position::new(1, 2), Position::new(1, 3))),
        };
        let s = serde_json::to_string(&h).unwrap();
        assert_eq!(serde_json::from_str::<Hover>(&s).unwrap(), h);
    }
}
