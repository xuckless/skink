//! ACP-free vocabulary shared by the editor-facing [`crate::AgentController`]
//! trait, the local ACP client and the future EveryAI-delegated controller.
//!
//! Nothing in here mentions JSON-RPC or ACP wire shapes, so a controller that
//! talks MCP to EveryAI can produce and consume exactly the same values.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

/// Handle of a running (or starting) agent process, assigned by the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentId(pub u64);

/// Agent-assigned conversation id (ACP `sessionId`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub String);

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One prompt turn (a `session/prompt` call and everything it streams).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TurnId(pub u64);

/// Correlates an asynchronous controller call with its result event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// How to launch an agent process. Tests and hosts may also run the process
/// themselves (terminal pane, EveryAI) and hand a [`crate::acp::Transport`] over.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Working directory of the process (usually the workspace root).
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl AgentSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// A well-known ACP agent and the command line that starts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownAgent {
    pub label: &'static str,
    pub spec: AgentSpec,
}

/// Command lines of common ACP agents (idea: the same list Zed ships). The
/// host decides which are installed; Skink does not probe the machine.
pub fn known_agents() -> Vec<KnownAgent> {
    let mk = |label, program: &str, args: &[&str]| KnownAgent {
        label,
        spec: AgentSpec::new(program).args(args.iter().copied()),
    };
    vec![
        mk(
            "Claude Code",
            "npx",
            &["-y", "@zed-industries/claude-code-acp"],
        ),
        mk("Gemini CLI", "gemini", &["--experimental-acp"]),
        mk("Antigravity", "agy", &["acp"]),
        mk("Goose", "goose", &["acp"]),
        mk("Codex", "npx", &["-y", "@zed-industries/codex-acp"]),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    /// A reason this client does not know (forward compatibility).
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    #[default]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLocation {
    pub path: PathBuf,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolContent {
    Text(String),
    /// A file change proposed or made by the tool (`old == None`: new file).
    Diff {
        path: PathBuf,
        old: Option<String>,
        new: String,
    },
    /// Live output of a terminal the agent created.
    Terminal(String),
}

/// A tool call the agent started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallInfo {
    pub id: String,
    pub title: String,
    pub kind: ToolKind,
    pub status: ToolStatus,
    pub content: Vec<ToolContent>,
    pub locations: Vec<ToolLocation>,
}

/// Partial update of a tool call; `None` means "unchanged".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolCallPatch {
    pub id: String,
    pub title: Option<String>,
    pub kind: Option<ToolKind>,
    pub status: Option<ToolStatus>,
    pub content: Option<Vec<ToolContent>>,
    pub locations: Option<Vec<ToolLocation>>,
}

impl ToolCallInfo {
    /// Apply a patch (what a UI does to its stored copy of the call).
    pub fn apply(&mut self, p: &ToolCallPatch) {
        if let Some(t) = &p.title {
            self.title.clone_from(t);
        }
        if let Some(k) = p.kind {
            self.kind = k;
        }
        if let Some(s) = p.status {
            self.status = s;
        }
        if let Some(c) = &p.content {
            self.content.clone_from(c);
        }
        if let Some(l) = &p.locations {
            self.locations.clone_from(l);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    /// `"high" | "medium" | "low"` (other values are passed through).
    pub priority: String,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
    /// Hint shown when the command takes free-form input.
    pub input_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthMethodInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// What the agent said about itself in `initialize`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentInfo {
    pub name: String,
    pub title: Option<String>,
    pub version: String,
    pub protocol_version: u32,
    pub load_session: bool,
    pub image_prompts: bool,
    pub embedded_context: bool,
    pub auth_methods: Vec<AuthMethodInfo>,
}

/// Result of creating or loading a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: SessionId,
    pub modes: Vec<ModeInfo>,
    pub current_mode: Option<String>,
}

/// One piece of a user prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptBlock {
    Text(String),
    /// Reference to a file/URI the agent may read itself.
    ResourceLink {
        uri: String,
        name: String,
        mime_type: Option<String>,
    },
    /// File contents inlined (unsaved editor text). Falls back to a fenced
    /// text block when the agent does not advertise `embeddedContext`.
    Resource {
        uri: String,
        text: String,
        mime_type: Option<String>,
    },
}

impl PromptBlock {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }
    /// `file://` link for a local path.
    pub fn file_link(path: &std::path::Path) -> Self {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        Self::ResourceLink {
            uri: file_uri(path),
            name,
            mime_type: None,
        }
    }
}

/// `file:///abs/path` (percent-encodes the few characters that matter;
/// Windows `C:\x` becomes `file:///C:/x`, `\\srv\share` becomes
/// `file://srv/share`, and `fs::canonicalize`'s `\\?\` prefix is dropped).
pub fn file_uri(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let s = strip_verbatim(&s).unwrap_or_else(|| s.into_owned());
    let s = s.replace('\\', "/");
    let mut out = String::from("file:");
    if !s.starts_with("//") {
        out.push_str(if s.starts_with('/') { "//" } else { "///" });
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Windows verbatim paths (what `fs::canonicalize` returns there) in their
/// ordinary form: `\\?\C:\x` -> `C:\x`, `\\?\UNC\srv\x` -> `\\srv\x`.
/// `None` if `p` is not such a path (or a verbatim form without a plain one).
pub(crate) fn strip_verbatim(p: &str) -> Option<String> {
    if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    let rest = p.strip_prefix(r"\\?\")?;
    let b = rest.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':').then(|| rest.to_owned())
}

/// Wrap `text` in a Markdown code fence long enough to contain any backtick
/// run inside it.
pub fn markdown_fence(lang: &str, text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    let nl = if text.ends_with('\n') { "" } else { "\n" };
    format!("{fence}{lang}\n{text}{nl}{fence}")
}

/// Failure details of an asynchronous controller call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorInfo {
    pub message: String,
    /// JSON-RPC error code when the agent returned one.
    pub code: Option<i64>,
    /// The agent wants `authenticate` first (ACP `auth_required`, -32000).
    pub auth_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    Other,
}

impl PermissionKind {
    pub fn is_allow(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    pub kind: PermissionKind,
}

/// The user's answer to a permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionChoice {
    Selected(String),
    Cancelled,
}

type ReplyFn = Box<dyn FnOnce(PermissionChoice) + Send>;

struct ReplyInner {
    f: Mutex<Option<ReplyFn>>,
}

impl ReplyInner {
    fn answer(&self, choice: PermissionChoice) -> bool {
        let f = self.f.lock().unwrap_or_else(PoisonError::into_inner).take();
        match f {
            Some(f) => {
                f(choice);
                true
            }
            None => false,
        }
    }
}

impl Drop for ReplyInner {
    /// A reply dropped unanswered tells the agent "cancelled" rather than
    /// leaving it blocked forever.
    fn drop(&mut self) {
        self.answer(PermissionChoice::Cancelled);
    }
}

/// One-shot reply channel carried by a [`PermissionRequest`]. Cloning shares
/// the same slot: the first answer wins. Dropping every clone unanswered
/// answers `Cancelled`.
#[derive(Clone)]
pub struct PermissionReply {
    inner: Arc<ReplyInner>,
}

impl PermissionReply {
    /// Build a reply from a closure (the local client writes a JSON-RPC
    /// response, a delegated controller calls an MCP tool, ...).
    pub fn new(f: impl FnOnce(PermissionChoice) + Send + 'static) -> Self {
        Self {
            inner: Arc::new(ReplyInner {
                f: Mutex::new(Some(Box::new(f))),
            }),
        }
    }
    /// Answer; returns false if it was already answered.
    pub fn answer(&self, choice: PermissionChoice) -> bool {
        self.inner.answer(choice)
    }
    pub fn select(&self, option_id: impl Into<String>) -> bool {
        self.answer(PermissionChoice::Selected(option_id.into()))
    }
    pub fn cancel(&self) -> bool {
        self.answer(PermissionChoice::Cancelled)
    }
    pub fn is_answered(&self) -> bool {
        self.inner
            .f
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
    }
    /// Weak handle so the client can cancel pending replies on `session/cancel`
    /// without keeping them alive.
    pub(crate) fn downgrade(&self) -> WeakReply {
        WeakReply(Arc::downgrade(&self.inner))
    }
}

impl fmt::Debug for PermissionReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PermissionReply")
            .field("answered", &self.is_answered())
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct WeakReply(std::sync::Weak<ReplyInner>);

impl WeakReply {
    /// Cancel if still alive and unanswered. Returns false when gone/answered.
    pub(crate) fn cancel(&self) -> bool {
        self.0
            .upgrade()
            .is_some_and(|i| i.answer(PermissionChoice::Cancelled))
    }
    pub(crate) fn is_live(&self) -> bool {
        self.0.strong_count() > 0
    }
}

/// The agent asks the user to approve something.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub session: SessionId,
    /// The tool call being approved (title/kind/content/locations as known).
    pub tool: ToolCallPatch,
    pub options: Vec<PermissionOption>,
    pub reply: PermissionReply,
}

/// Everything an agent can tell the editor. `ProtocolError` and `Exited` are
/// non-fatal / terminal notices respectively; the stream never panics.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Handshake done (emitted by controllers that start agents asynchronously).
    Ready {
        info: AgentInfo,
    },
    /// Spawn or handshake failed.
    StartFailed {
        message: String,
    },
    SessionStarted {
        request: RequestId,
        session: SessionInfo,
    },
    Authenticated {
        request: RequestId,
    },
    RequestFailed {
        request: RequestId,
        error: ErrorInfo,
    },
    /// Replayed history while loading a session.
    UserChunk {
        session: SessionId,
        text: String,
    },
    MessageChunk {
        session: SessionId,
        text: String,
    },
    ThoughtChunk {
        session: SessionId,
        text: String,
    },
    ToolCall {
        session: SessionId,
        call: ToolCallInfo,
    },
    ToolCallUpdate {
        session: SessionId,
        patch: ToolCallPatch,
    },
    Plan {
        session: SessionId,
        entries: Vec<PlanEntry>,
    },
    Commands {
        session: SessionId,
        commands: Vec<CommandInfo>,
    },
    ModeChanged {
        session: SessionId,
        mode_id: String,
    },
    Permission(PermissionRequest),
    TurnFinished {
        session: SessionId,
        turn: TurnId,
        stop: StopReason,
    },
    TurnFailed {
        session: SessionId,
        turn: TurnId,
        error: ErrorInfo,
    },
    /// Unparseable or unexpected traffic (malformed JSON, unknown response id...).
    ProtocolError {
        message: String,
    },
    /// The agent process / connection is gone. No further events follow.
    Exited {
        status: Option<i32>,
        stderr_tail: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn file_uris() {
        assert_eq!(file_uri(Path::new("/a b/c.rs")), "file:///a%20b/c.rs");
        assert_eq!(file_uri(Path::new(r"C:\x\y.rs")), "file:///C:/x/y.rs");
        // Regression: canonicalized Windows paths became `file:////%3F/C:/...`.
        assert_eq!(file_uri(Path::new(r"\\?\C:\x\y.rs")), "file:///C:/x/y.rs");
        assert_eq!(file_uri(Path::new(r"\\srv\share\a")), "file://srv/share/a");
        assert_eq!(
            file_uri(Path::new(r"\\?\UNC\srv\share\a")),
            "file://srv/share/a"
        );
    }

    #[test]
    fn verbatim_prefixes() {
        assert_eq!(strip_verbatim(r"\\?\C:\x").as_deref(), Some(r"C:\x"));
        assert_eq!(strip_verbatim(r"\\?\UNC\s\x").as_deref(), Some(r"\\s\x"));
        assert_eq!(strip_verbatim(r"\\?\Volume{1}\x"), None);
        assert_eq!(strip_verbatim("/usr/bin"), None);
    }
}
