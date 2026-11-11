//! AI in the editor, all through the `skink-ai` [`AgentController`] seam
//! (a local ACP agent, or EveryAI when delegated):
//!
//! - the **prompt box** (Ctrl-G), modelled on ThePrimeagen/99's idea (no
//!   license, so ideas only): select code, write an instruction with `@file`,
//!   `#rule`, `@selection`, `@diagnostics`, `@terminal`, pick a mode
//!   (rewrite / ask / search / implement / chat) and a model, review the answer
//!   as a diff with per-hunk accept / reject;
//! - the **agent panel** (`:agent`): a long-lived session whose transcript
//!   (messages, tool calls, plan) is shown in a read-only tab;
//! - **permission prompts** and **staged agent edits**, reviewed in the
//!   refactor preview (`crate::refactor`).
//!
//! Every lifecycle is an explicit enum: [`Conn`] (the agent process),
//! [`AskBox`] (the prompt box), [`Stage`] (one request before its turn runs),
//! [`PanelSession`] (the panel's session).

use crate::{
    App,
    doc::DocId,
    nav::Level,
    refactor::{FilePatch, Hunk, Preview, PreviewState, Source},
};
use skink_ai::{
    AgentController, AgentEvent, AgentId, ErrorInfo, LocalAgents, ModelInfo, PermissionKind,
    PermissionRequest, PlanEntry, PlanStatus, PromptBlock, RequestId, SessionId, StopReason,
    ToolCallInfo, ToolStatus, TurnId,
    acp::BufferSource,
    prompter::{
        self, BuiltInsert, BuiltRewrite, CharPos, CharRange, ContextConfig, DiskFiles,
        EditProposal, FileSnapshot, SearchRequest, TextIndex,
    },
};
use skink_config::{AiMode, ModelChoice};
use skink_host::DelegatedAgents;
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Instant,
};

/// Title of the agent panel's tab.
pub const PANEL_TITLE: &str = "Agent";
const HISTORY_MAX: usize = 100;

// ---- shared with the agent's fs/* handler -----------------------------------------

/// What the agent sees through `fs/read_text_file` / `fs/write_text_file`:
/// open buffers (snapshotted when a request starts) and the edits it staged.
/// Writes never touch the disk or a buffer directly; they wait in `staged`
/// until the user applies them in the refactor preview.
#[derive(Default)]
pub struct Shared {
    open: Mutex<HashMap<PathBuf, String>>,
    staged: Mutex<Vec<(PathBuf, String)>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    pub fn set_open(&self, files: HashMap<PathBuf, String>) {
        *lock(&self.open) = files;
    }
    pub fn staged(&self) -> Vec<(PathBuf, String)> {
        lock(&self.staged).clone()
    }
    pub fn clear_staged(&self) {
        lock(&self.staged).clear();
    }
}

impl BufferSource for Shared {
    fn read(&self, path: &Path) -> Option<String> {
        if let Some((_, t)) = lock(&self.staged).iter().rev().find(|(p, _)| p == path) {
            return Some(t.clone());
        }
        lock(&self.open).get(path).cloned()
    }

    fn write(&self, path: &Path, content: &str) -> Option<Result<(), String>> {
        let mut st = lock(&self.staged);
        st.retain(|(p, _)| p != path);
        st.push((path.to_path_buf(), content.to_owned()));
        Some(Ok(()))
    }
}

// ---- state --------------------------------------------------------------------------

/// The controller is built on first use (it needs the project root and the
/// event loop's waker).
pub enum Controller {
    Unbuilt,
    /// Skink runs the agents (local ACP processes, or one a test attached).
    Built(Box<dyn AgentController>),
    /// Inside EveryAI: requests go to its host (`prompt.run`, `models.list`).
    Delegated(Box<DelegatedAgents>),
}

/// The agent label while EveryAI runs the prompts.
const EVERYAI: &str = "EveryAI";

/// The agent process.
pub enum Conn {
    Off,
    /// Handshake running.
    Starting {
        agent: AgentId,
        label: String,
    },
    Ready {
        agent: AgentId,
        label: String,
        /// The agent can reopen a past session (ACP `loadSession`).
        resume: bool,
    },
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxMode {
    Rewrite,
    Ask,
    Search,
    Implement,
    /// A message for the agent panel's session.
    Chat,
}

impl BoxMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rewrite => "rewrite",
            Self::Ask => "ask",
            Self::Search => "search",
            Self::Implement => "implement",
            Self::Chat => "chat",
        }
    }

    /// Shift-Tab order (chat is only reachable from the agent panel).
    fn next(self) -> Self {
        match self {
            Self::Rewrite => Self::Implement,
            Self::Implement => Self::Ask,
            Self::Ask => Self::Search,
            Self::Search => Self::Rewrite,
            Self::Chat => Self::Chat,
        }
    }

    fn settings_mode(self) -> AiMode {
        match self {
            Self::Rewrite => AiMode::Rewrite,
            Self::Ask | Self::Chat => AiMode::Ask,
            Self::Search => AiMode::Search,
            Self::Implement => AiMode::Implement,
        }
    }

    fn from_word(w: &str) -> Option<Self> {
        Some(match w {
            "rewrite" | "r" => Self::Rewrite,
            "ask" | "a" => Self::Ask,
            "search" | "s" => Self::Search,
            "implement" | "impl" | "i" => Self::Implement,
            _ => return None,
        })
    }
}

/// Where a new prompt box takes its model from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelSource {
    /// `ai.model` / `ai.model.<mode>` (per-mode overrides).
    Settings,
    /// `:model NAME` for the rest of this run, every mode.
    Run(Pick),
}

/// The model chosen in the box (starts from the settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    AgentDefault,
    Named(String),
}

impl Pick {
    fn from_choice(c: &ModelChoice) -> Self {
        match c {
            ModelChoice::AgentDefault => Pick::AgentDefault,
            ModelChoice::Named(m) => Pick::Named(m.clone()),
        }
    }
    pub fn label(&self) -> &str {
        match self {
            Pick::AgentDefault => "default model",
            Pick::Named(m) => m,
        }
    }
}

/// What the request is about.
#[derive(Clone, Debug)]
pub struct Target {
    pub doc: DocId,
    pub path: Option<PathBuf>,
    /// Char range (the selection, or the syntax scope at the cursor).
    pub range: (usize, usize),
    pub cursor: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hist {
    /// Editing a new line.
    Fresh,
    /// Showing history entry `n`.
    At(usize),
}

/// Completion list under the input for the `@` / `#` word at the cursor.
#[derive(Clone, Debug)]
pub enum Suggest {
    None,
    List {
        /// Char index of the `@` / `#`.
        start: usize,
        items: Vec<String>,
        selected: usize,
    },
}

#[derive(Clone, Debug)]
pub struct Draft {
    pub mode: BoxMode,
    pub input: String,
    /// Char index into `input`.
    pub cursor: usize,
    pub model: Pick,
    pub target: Option<Target>,
    pub hist: Hist,
    pub suggest: Suggest,
}

/// One request before and while its turn runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The agent process is still starting.
    WaitAgent,
    OpenSession {
        request: RequestId,
    },
    SetModel {
        session: SessionId,
        request: RequestId,
    },
    Running {
        session: SessionId,
        turn: TurnId,
    },
}

pub enum Work {
    Rewrite(Box<BuiltRewrite>),
    Implement(Box<BuiltInsert>),
    /// The answer streams into a read-only tab.
    Ask {
        title: String,
        prompt: String,
    },
    Search {
        root: PathBuf,
        prompt: String,
    },
}

pub struct Job {
    pub draft: Draft,
    pub work: Work,
    pub stage: Stage,
    pub blocks: Vec<PromptBlock>,
    pub reply: String,
    pub started: Instant,
    /// Latest tool call title, shown while working.
    pub activity: String,
}

pub struct Review {
    pub doc: DocId,
    pub proposal: EditProposal,
    pub hunks: Vec<Hunk>,
    pub current: usize,
}

/// The prompt box.
pub enum AskBox {
    Closed,
    Editing(Draft),
    /// Waiting for the agent, a session or a model switch.
    Sending(Box<Job>),
    /// The turn is running; the reply streams in.
    Streaming(Box<Job>),
    /// The answer as hunks to accept or reject.
    Reviewing(Box<Review>),
    Failed {
        draft: Draft,
        message: String,
    },
}

/// The agent panel's session.
pub enum PanelSession {
    None,
    /// The agent exited mid-conversation: the next message reopens this
    /// session when the restarted agent can load it, else starts anew.
    Ended {
        session: SessionId,
    },
    Opening {
        request: RequestId,
    },
    Idle {
        session: SessionId,
    },
    Busy {
        session: SessionId,
    },
}

pub enum Entry {
    User(String),
    Agent(String),
    Thought(String),
    Tool(ToolCallInfo),
    Plan(Vec<PlanEntry>),
    Note(String),
}

pub struct Panel {
    pub session: PanelSession,
    pub transcript: Vec<Entry>,
    /// Messages typed before the session was open.
    pub queued: Vec<String>,
}

pub struct Ai {
    pub controller: Controller,
    pub conn: Conn,
    pub ask: AskBox,
    pub panel: Panel,
    /// Permission requests waiting for an answer, oldest first, and the
    /// option highlighted for the first one.
    pub permissions: VecDeque<PermissionRequest>,
    pub permission_choice: usize,
    pub preview: PreviewState,
    /// A tree rename waiting for `workspace/willRenameFiles`.
    pub file_move: crate::refactor::FileMove,
    pub shared: Arc<Shared>,
    pub history: Vec<String>,
    /// Models the agent reported for its last session.
    pub models: Vec<ModelInfo>,
    /// Where the box's starting model comes from.
    pub model_source: ModelSource,
    /// Health check of the configured agent (run when settings.json is saved).
    pub check: crate::agents::AgentCheck,
    /// settings.json's modification time when last seen (save detection).
    pub settings_seen: Option<crate::agents::FileStamp>,
}

impl Default for Ai {
    fn default() -> Self {
        Ai {
            controller: Controller::Unbuilt,
            conn: Conn::Off,
            ask: AskBox::Closed,
            panel: Panel {
                session: PanelSession::None,
                transcript: Vec::new(),
                queued: Vec::new(),
            },
            permissions: VecDeque::new(),
            permission_choice: 0,
            preview: PreviewState::Closed,
            file_move: crate::refactor::FileMove::Idle,
            shared: Arc::new(Shared::default()),
            history: Vec::new(),
            models: Vec::new(),
            model_source: ModelSource::Settings,
            check: crate::agents::AgentCheck::Idle,
            settings_seen: None,
        }
    }
}

impl Ai {
    /// Something is running that the clock on screen should follow.
    pub fn busy(&self) -> bool {
        self.working() || matches!(self.check, crate::agents::AgentCheck::Running { .. })
    }

    /// A request or panel turn is in flight.
    pub fn working(&self) -> bool {
        matches!(self.ask, AskBox::Sending(_) | AskBox::Streaming(_))
            || matches!(self.panel.session, PanelSession::Busy { .. })
    }
}

// ---- agents ---------------------------------------------------------------------------

pub use crate::agents::resolve_agent;

/// Pick the model id for `want` among what the agent offers.
pub fn resolve_model(want: &Pick, offered: &[ModelInfo]) -> Result<Option<String>, String> {
    let Pick::Named(w) = want else {
        return Ok(None);
    };
    if offered.is_empty() {
        // The agent does not let clients choose; use its default.
        return Ok(None);
    }
    skink_ai::pick_model(w, offered)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn error_text(e: &ErrorInfo) -> String {
    if e.auth_required {
        format!(
            "{} (the agent needs a login: run it once in a terminal tab to sign in)",
            e.message
        )
    } else {
        e.message.clone()
    }
}

// ---- the prompt input -------------------------------------------------------------------

/// The `@`/`#` word ending at char `cursor` of `input`: (start, text after the sigil).
fn ref_word(input: &str, cursor: usize) -> Option<(usize, char, String)> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = cursor.min(chars.len());
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    let sigil = *chars.get(i)?;
    if (sigil == '@' || sigil == '#') && i < cursor {
        Some((i, sigil, chars[i + 1..cursor].iter().collect()))
    } else {
        None
    }
}

/// Context references in a prompt, for the chips under the input.
pub fn chips(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .filter(|w| (w.starts_with('@') || w.starts_with('#')) && w.len() > 1)
        .map(str::to_owned)
        .collect()
}

const SPECIAL_REFS: &[&str] = &["@selection", "@diagnostics", "@terminal"];

/// Remove `@selection` / `@diagnostics` / `@terminal`; returns the rest and
/// which were present.
fn take_special(input: &str) -> (String, [bool; 3]) {
    let mut found = [false; 3];
    let rest: Vec<&str> = input
        .split(' ')
        .filter(|w| match SPECIAL_REFS.iter().position(|s| s == w) {
            Some(i) => {
                found[i] = true;
                false
            }
            None => true,
        })
        .collect();
    (rest.join(" ").trim().to_owned(), found)
}

fn insert_at(s: &mut String, char_idx: usize, text: &str) {
    let b = s.char_indices().nth(char_idx).map_or(s.len(), |(b, _)| b);
    s.insert_str(b, text);
}

fn remove_range(s: &mut String, from: usize, to: usize) {
    let b0 = s.char_indices().nth(from).map_or(s.len(), |(b, _)| b);
    let b1 = s.char_indices().nth(to).map_or(s.len(), |(b, _)| b);
    s.replace_range(b0..b1, "");
}

impl App {
    // ---- connection ---------------------------------------------------------------

    /// Use `controller` with an agent it already started or attached (tests,
    /// EveryAI handing over its own agents).
    pub fn use_agent(&mut self, controller: Box<dyn AgentController>, agent: AgentId, label: &str) {
        self.ai.controller = Controller::Built(controller);
        self.ai.conn = Conn::Starting {
            agent,
            label: label.to_owned(),
        };
    }

    /// Hand the AI layer to EveryAI's host (normally found through
    /// `SKINK_HOST_URL`; tests pass a fake host's controller).
    pub fn use_host(&mut self, mut agents: DelegatedAgents) {
        agents.set_cwd(Some(self.project_root()));
        self.ai.controller = Controller::Delegated(Box::new(agents));
        self.ai.conn = Conn::Off;
    }

    fn controller(&mut self) -> &mut dyn AgentController {
        if matches!(self.ai.controller, Controller::Unbuilt) {
            let waker = self
                .wake
                .clone()
                .map(|w| -> skink_ai::local::Waker { Arc::new(move || w()) });
            match skink_host::connect_if_present() {
                // Inside EveryAI: its host runs the prompts.
                Some(client) => {
                    let mut d = DelegatedAgents::new(Arc::new(skink_host::HostLink::new(client)));
                    if let Some(w) = waker {
                        d = d.with_waker(w);
                    }
                    self.use_host(d);
                }
                None => {
                    let shared: Arc<dyn BufferSource> = self.ai.shared.clone();
                    let mut local = LocalAgents::for_workspace(self.project_root(), shared);
                    if let Some(w) = waker {
                        local = local.with_waker(w);
                    }
                    self.ai.controller = Controller::Built(Box::new(local));
                }
            }
        }
        match &mut self.ai.controller {
            Controller::Built(c) => c.as_mut(),
            Controller::Delegated(d) => d.as_mut(),
            Controller::Unbuilt => unreachable!("built above"),
        }
    }

    /// Start the configured agent unless it runs. Returns false (with the
    /// reason in `Conn::Failed`) when there is none.
    fn ensure_agent(&mut self) -> bool {
        match &self.ai.conn {
            Conn::Starting { .. } | Conn::Ready { .. } => return true,
            Conn::Off | Conn::Failed(_) => {}
        }
        self.controller();
        if let Controller::Delegated(d) = &mut self.ai.controller {
            // EveryAI's own AI route (`prompt.run`): ready at once.
            let agent = d.attach_prompt_agent();
            self.ai.conn = Conn::Starting {
                agent,
                label: EVERYAI.into(),
            };
            return true;
        }
        let (label, spec) = match resolve_agent(&self.settings.ai.agent) {
            Ok(x) => x,
            Err(e) => {
                self.ai.conn = Conn::Failed(e);
                return false;
            }
        };
        // A stray key must never fetch an agent from the internet: ask
        // first, once per command (remembered in the state folder).
        if crate::agents::downloads(&spec) {
            let cmd = crate::agents::command_line(&spec);
            if !self.download_allowed(&cmd) {
                self.ai.conn = Conn::Failed(format!(
                    "{label} needs a download first ({cmd}); answer the question below, then send again"
                ));
                self.open_choice(crate::prompt::Choice::AgentDownload {
                    label,
                    command: cmd,
                });
                return false;
            }
        }
        let spec = spec.cwd(self.project_root());
        match self.controller().start_agent(&label, spec) {
            Ok(agent) => {
                self.ai.conn = Conn::Starting { agent, label };
                true
            }
            Err(e) => {
                self.ai.conn = Conn::Failed(e.to_string());
                false
            }
        }
    }

    fn download_file(&self) -> Option<PathBuf> {
        self.state_dir
            .as_ref()
            .map(|d| d.join("allowed-agent-downloads.json"))
    }

    fn allowed_downloads(&self) -> Vec<String> {
        self.download_file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) fn download_allowed(&self, cmd: &str) -> bool {
        self.allowed_downloads().iter().any(|c| c == cmd)
    }

    /// The answer to "download and run this agent?".
    pub(crate) fn answer_agent_download(&mut self, label: &str, cmd: &str, yes: bool) {
        if !yes {
            self.ai.conn = Conn::Failed(format!("{label} not started (its download was declined)"));
            return self.toast(Level::Info, format!("{label} not started"));
        }
        let mut all = self.allowed_downloads();
        all.push(cmd.to_owned());
        if let Some(f) = self.download_file() {
            let _ = std::fs::create_dir_all(f.parent().unwrap_or(std::path::Path::new(".")));
            let _ = std::fs::write(f, serde_json::to_string_pretty(&all).unwrap_or_default());
        }
        self.ai.conn = Conn::Off;
        self.toast(
            Level::Info,
            format!("{label} allowed: send your message again"),
        );
    }

    pub(crate) fn ready_agent(&self) -> Option<AgentId> {
        match &self.ai.conn {
            Conn::Ready { agent, .. } => Some(*agent),
            _ => None,
        }
    }

    pub(crate) fn agent_label(&self) -> String {
        match &self.ai.conn {
            Conn::Starting { label, .. } | Conn::Ready { label, .. } => label.clone(),
            Conn::Off | Conn::Failed(_)
                if matches!(self.ai.controller, Controller::Delegated(_)) =>
            {
                EVERYAI.into()
            }
            Conn::Off | Conn::Failed(_) => match resolve_agent(&self.settings.ai.agent) {
                Ok((l, _)) => l,
                Err(_) => "no agent".into(),
            },
        }
    }

    /// Give the agent the current text of every open file.
    fn snapshot_buffers(&self) {
        let mut m = HashMap::new();
        for i in 0..self.tabs.len() {
            if let Some(d) = self.doc_at(i)
                && let Some(p) = &d.canon
            {
                m.insert(p.clone(), d.editor.buffer.text());
            }
        }
        self.ai.shared.set_open(m);
    }

    // ---- the prompt box ----------------------------------------------------------

    /// Ctrl-G: open the prompt box for the selection (or the syntax scope at
    /// the cursor); in the agent panel, a chat message.
    pub(crate) fn open_ai_prompt(&mut self) {
        self.open_ai_prompt_with("");
    }

    pub(crate) fn open_ai_prompt_with(&mut self, text: &str) {
        match &self.ai.ask {
            AskBox::Sending(_) | AskBox::Streaming(_) => {
                self.doc.editor.message = Some("An AI request is running (Esc cancels it)".into());
                return;
            }
            AskBox::Reviewing(_) => return,
            AskBox::Closed | AskBox::Editing(_) | AskBox::Failed { .. } => {}
        }
        if self.active_is_term() {
            self.doc.editor.message =
                Some("Open a file to use the AI prompt (:agent for a chat)".into());
            return;
        }
        let in_panel = self.doc.editor.buffer.title().as_deref() == Some(PANEL_TITLE)
            && self.doc.canon.is_none();
        let (mode, target) = if in_panel {
            (BoxMode::Chat, None)
        } else {
            if self.doc.editor.selection().is_none() {
                self.select_scope();
            }
            let ed = &self.doc.editor;
            let range = ed.selection().unwrap_or((ed.cursor_idx(), ed.cursor_idx()));
            (
                BoxMode::Rewrite,
                Some(Target {
                    doc: self.doc.id,
                    path: self.doc.canon.clone(),
                    range,
                    cursor: ed.cursor_idx(),
                }),
            )
        };
        let model = self.default_pick(mode);
        self.ai.ask = AskBox::Editing(Draft {
            mode,
            input: text.to_owned(),
            cursor: text.chars().count(),
            model,
            target,
            hist: Hist::Fresh,
            suggest: Suggest::None,
        });
    }

    fn default_pick(&self, mode: BoxMode) -> Pick {
        match &self.ai.model_source {
            ModelSource::Run(p) => p.clone(),
            ModelSource::Settings => {
                Pick::from_choice(self.settings.ai.model_for(mode.settings_mode()))
            }
        }
    }

    /// Nothing selected: grow the selection to the smallest syntax node that
    /// spans whole lines (a statement, a function), else the current line.
    fn select_scope(&mut self) {
        for _ in 0..16 {
            if !self.doc.expand_selection() {
                break;
            }
            if let Some((s, e)) = self.doc.editor.selection() {
                let b = &self.doc.editor.buffer;
                if b.char_to_line(s) != b.char_to_line(e.saturating_sub(1).max(s)) {
                    return;
                }
            }
        }
        if self.doc.editor.selection().is_none() {
            let ed = &mut self.doc.editor;
            let line = ed.cursor.line;
            let start = ed.idx(skink_core::Cursor { line, col: 0 });
            let end = start + ed.buffer.line_len(line);
            if end > start {
                self.doc.select_range((start, end));
            }
        }
    }

    /// Keys for the AI layers (permission dialog, preview, prompt box,
    /// review). True when consumed.
    pub(crate) fn ai_key(&mut self, spec: &skink_config::KeySpec) -> bool {
        use skink_config::Key;
        if !self.ai.permissions.is_empty() {
            self.permission_key(spec);
            return true;
        }
        if matches!(self.ai.preview, PreviewState::Open(_)) {
            self.refactor_key(spec);
            return true;
        }
        match &mut self.ai.ask {
            AskBox::Closed => false,
            AskBox::Editing(_) => {
                self.draft_key(spec);
                true
            }
            AskBox::Sending(_) | AskBox::Streaming(_) => {
                if spec.key == Key::Escape && !spec.ctrl && !spec.alt {
                    self.cancel_ai();
                    true
                } else {
                    false
                }
            }
            AskBox::Reviewing(_) => {
                self.review_key(spec);
                true
            }
            AskBox::Failed { .. } => {
                match spec.key {
                    Key::Enter => {
                        if let AskBox::Failed { draft, .. } =
                            std::mem::replace(&mut self.ai.ask, AskBox::Closed)
                        {
                            self.ai.ask = AskBox::Editing(draft);
                        }
                    }
                    _ => self.ai.ask = AskBox::Closed,
                }
                true
            }
        }
    }

    pub(crate) fn ai_paste(&mut self, s: &str) -> bool {
        let AskBox::Editing(d) = &mut self.ai.ask else {
            return false;
        };
        insert_at(&mut d.input, d.cursor, s);
        d.cursor += s.chars().count();
        true
    }

    fn draft_key(&mut self, spec: &skink_config::KeySpec) {
        use skink_config::Key;
        let root = self.project_root();
        let AskBox::Editing(d) = &mut self.ai.ask else {
            return;
        };
        let len = d.input.chars().count();
        let suggesting = matches!(d.suggest, Suggest::List { .. });
        match spec.key {
            Key::Escape if suggesting => d.suggest = Suggest::None,
            Key::Escape => {
                self.ai.ask = AskBox::Closed;
                return;
            }
            Key::Enter if spec.alt || spec.shift => {
                insert_at(&mut d.input, d.cursor, "\n");
                d.cursor += 1;
            }
            Key::Enter if suggesting => {
                accept_suggestion(d);
            }
            Key::Enter => return self.submit_ai(),
            Key::Tab if suggesting => accept_suggestion(d),
            Key::Tab => {
                // Cycle the model: default, then what the agent offers.
                let mut all = vec![Pick::AgentDefault];
                all.extend(self.ai.models.iter().map(|m| Pick::Named(m.id.clone())));
                if let Pick::Named(n) = &d.model
                    && !all.contains(&d.model)
                {
                    all.push(Pick::Named(n.clone()));
                }
                let i = all.iter().position(|p| *p == d.model).unwrap_or(0);
                d.model = all[(i + 1) % all.len()].clone();
                if self.ai.models.is_empty() {
                    self.doc.editor.message = Some(
                        "The agent lists its models after the first request (or type /model NAME)"
                            .into(),
                    );
                }
                return;
            }
            Key::BackTab => {
                d.mode = d.mode.next();
                return;
            }
            Key::Up | Key::Down if suggesting => {
                if let Suggest::List {
                    items, selected, ..
                } = &mut d.suggest
                {
                    let n = items.len().max(1);
                    *selected = if spec.key == Key::Up {
                        (*selected + n - 1) % n
                    } else {
                        (*selected + 1) % n
                    };
                }
                return;
            }
            Key::Up | Key::Down => {
                let h = &self.ai.history;
                if h.is_empty() {
                    return;
                }
                let next = match (d.hist, spec.key) {
                    (Hist::Fresh, Key::Up) => Hist::At(h.len() - 1),
                    (Hist::At(0), Key::Up) => Hist::At(0),
                    (Hist::At(n), Key::Up) => Hist::At(n - 1),
                    (Hist::At(n), _) if n + 1 < h.len() => Hist::At(n + 1),
                    _ => Hist::Fresh,
                };
                d.input = match next {
                    Hist::At(n) => h[n].clone(),
                    Hist::Fresh => String::new(),
                };
                d.cursor = d.input.chars().count();
                d.hist = next;
                return;
            }
            Key::Left => d.cursor = d.cursor.saturating_sub(1),
            Key::Right => d.cursor = (d.cursor + 1).min(len),
            Key::Home => d.cursor = 0,
            Key::End => d.cursor = len,
            Key::Backspace if d.cursor > 0 => {
                remove_range(&mut d.input, d.cursor - 1, d.cursor);
                d.cursor -= 1;
            }
            Key::Delete if d.cursor < len => remove_range(&mut d.input, d.cursor, d.cursor + 1),
            Key::Char('u') if spec.ctrl => {
                remove_range(&mut d.input, 0, d.cursor);
                d.cursor = 0;
            }
            Key::Char(c) if !spec.ctrl && !spec.alt => {
                insert_at(&mut d.input, d.cursor, &c.to_string());
                d.cursor += 1;
            }
            _ => return,
        }
        refresh_suggestions(d, &root);
    }

    /// Enter in the box.
    fn submit_ai(&mut self) {
        let AskBox::Editing(mut d) = std::mem::replace(&mut self.ai.ask, AskBox::Closed) else {
            return;
        };
        let line = d.input.trim().to_owned();
        // `/model NAME`, `/ask ...`, `/search ...`
        if let Some(rest) = line.strip_prefix('/') {
            let (word, arg) = rest.split_once(' ').unwrap_or((rest, ""));
            if word == "model" {
                d.model = match arg.trim() {
                    "" | "default" => Pick::AgentDefault,
                    m => Pick::Named(m.to_owned()),
                };
                d.input.clear();
                d.cursor = 0;
                self.ai.ask = AskBox::Editing(d);
                return;
            }
            if let Some(m) = BoxMode::from_word(word)
                && d.mode != BoxMode::Chat
            {
                d.mode = m;
                d.input = arg.to_owned();
                d.cursor = d.input.chars().count();
                if arg.trim().is_empty() {
                    self.ai.ask = AskBox::Editing(d);
                    return;
                }
            }
        }
        let text = d.input.trim().to_owned();
        if text.is_empty() {
            self.ai.ask = AskBox::Editing(d);
            return;
        }
        if self.ai.history.last() != Some(&text) {
            self.ai.history.push(text.clone());
            if self.ai.history.len() > HISTORY_MAX {
                self.ai.history.remove(0);
            }
        }
        if d.mode == BoxMode::Chat {
            let full = self.chat_message(&text);
            self.panel_send_as(&text, &full);
            return;
        }
        let (instruction, [want_sel, want_diags, want_term]) = take_special(&text);
        let work = match self.build_work(&d, &instruction, want_sel, want_diags) {
            Ok(w) => w,
            Err(message) => {
                self.ai.ask = AskBox::Failed { draft: d, message };
                return;
            }
        };
        let mut blocks = vec![PromptBlock::text(work_prompt(&work))];
        if want_term && let Some(t) = self.terminal_text() {
            blocks.push(PromptBlock::text(format!(
                "## Terminal output (most recent terminal tab)\n```\n{t}\n```"
            )));
        }
        if let Work::Ask { title, .. } = &work {
            let here = self.active;
            self.open_view(title, "_Waiting for the agent..._");
            self.switch_to(here);
        }
        self.snapshot_buffers();
        let job = Job {
            draft: d,
            work,
            stage: Stage::WaitAgent,
            blocks,
            reply: String::new(),
            started: Instant::now(),
            activity: String::new(),
        };
        if !self.ensure_agent() {
            let message = match &self.ai.conn {
                Conn::Failed(m) => m.clone(),
                _ => "No agent".into(),
            };
            self.ai.ask = AskBox::Failed {
                draft: job.draft,
                message,
            };
            return;
        }
        self.ai.ask = AskBox::Sending(Box::new(job));
        self.advance_job();
    }

    fn build_work(
        &self,
        d: &Draft,
        instruction: &str,
        want_sel: bool,
        want_diags: bool,
    ) -> Result<Work, String> {
        let root = self.project_root();
        let cfg = ContextConfig::new(&root);
        let t = d.target.as_ref().ok_or("No file for this request")?;
        let doc = self
            .find_doc(|x| x.id == t.doc)
            .and_then(|i| self.doc_at(i))
            .ok_or("The file was closed")?;
        let text = doc.editor.buffer.text();
        let path = t.path.clone().unwrap_or_else(|| root.join(doc.title()));
        let file = FileSnapshot {
            path: path.clone(),
            text: text.clone(),
        };
        let ix = TextIndex::new(&text);
        let range = CharRange::new(ix.char_idx_to_pos(t.range.0), ix.char_idx_to_pos(t.range.1));
        let diags = if want_diags || matches!(d.mode, BoxMode::Rewrite | BoxMode::Implement) {
            self.prompter_diagnostics(&path)
        } else {
            Vec::new()
        };
        Ok(match d.mode {
            BoxMode::Rewrite => {
                let req = prompter::RewriteRequest::new(instruction, file, range);
                Work::Rewrite(Box::new(prompter::build_rewrite(
                    &cfg, &DiskFiles, &req, &diags,
                )))
            }
            BoxMode::Implement => Work::Implement(Box::new(prompter::build_implement(
                &cfg,
                &DiskFiles,
                &file,
                ix.char_idx_to_pos(t.cursor),
                &diags,
                instruction,
            ))),
            BoxMode::Ask => {
                let sel = (want_sel || !range.is_empty()).then_some(range);
                let p = prompter::build_ask(&cfg, &DiskFiles, &file, sel, &diags, instruction);
                let short: String = instruction.chars().take(32).collect();
                Work::Ask {
                    title: format!("AI: {short}"),
                    prompt: p.text,
                }
            }
            BoxMode::Search => {
                let mut req = SearchRequest::new(instruction);
                req.file = Some(file);
                let b = prompter::build_search(&cfg, &DiskFiles, &req);
                Work::Search {
                    root,
                    prompt: b.prompt.text,
                }
            }
            BoxMode::Chat => return Err("chat goes to the agent panel".into()),
        })
    }

    fn prompter_diagnostics(&self, path: &Path) -> Vec<prompter::Diagnostic> {
        self.all_diags(path)
            .into_iter()
            .map(|d| prompter::Diagnostic {
                range: CharRange::new(
                    CharPos::new(d.range.start.line, d.range.start.col),
                    CharPos::new(d.range.end.line, d.range.end.col),
                ),
                severity: match d.severity {
                    skink_lsp::Severity::Error => prompter::Severity::Error,
                    skink_lsp::Severity::Warning => prompter::Severity::Warning,
                    skink_lsp::Severity::Info => prompter::Severity::Info,
                    skink_lsp::Severity::Hint => prompter::Severity::Hint,
                },
                message: d.message.clone(),
                source: d.source.clone(),
            })
            .collect()
    }

    /// Visible text of the most recent terminal tab (`@terminal`).
    pub(crate) fn terminal_text(&mut self) -> Option<String> {
        let i = (0..self.tabs.len())
            .rev()
            .find(|&i| matches!(self.tabs[i], crate::Tab::Term(_)))?;
        let crate::Tab::Term(p) = &mut self.tabs[i] else {
            return None;
        };
        p.term.snapshot_into(&mut p.screen);
        Some(p.screen.text())
    }

    /// Move the request forward: agent ready -> session -> model -> turn.
    fn advance_job(&mut self) {
        let Some(agent) = self.ready_agent() else {
            return; // still WaitAgent; `Ready` calls back
        };
        let stage = match &self.ai.ask {
            AskBox::Sending(j) => j.stage.clone(),
            _ => return,
        };
        if stage == Stage::WaitAgent {
            let cwd = self.project_root();
            let mode = match &self.ai.ask {
                AskBox::Sending(j) => j.draft.mode.settings_mode(),
                _ => AiMode::Ask,
            };
            let opts = crate::agents::session_options(self.settings.ai.effort_for(mode));
            match self.controller().new_session_opts(agent, &cwd, opts) {
                Ok(request) => self.set_stage(Stage::OpenSession { request }),
                Err(e) => self.fail_job(e.to_string()),
            }
        }
    }

    fn set_stage(&mut self, stage: Stage) {
        match std::mem::replace(&mut self.ai.ask, AskBox::Closed) {
            AskBox::Sending(mut j) | AskBox::Streaming(mut j) => {
                let running = matches!(stage, Stage::Running { .. });
                j.stage = stage;
                self.ai.ask = if running {
                    AskBox::Streaming(j)
                } else {
                    AskBox::Sending(j)
                };
            }
            other => self.ai.ask = other,
        }
    }

    fn fail_job(&mut self, message: String) {
        match std::mem::replace(&mut self.ai.ask, AskBox::Closed) {
            AskBox::Sending(j) | AskBox::Streaming(j) => {
                self.ai.ask = AskBox::Failed {
                    draft: j.draft,
                    message,
                }
            }
            other => self.ai.ask = other,
        }
    }

    /// The session for the request is open: switch model if asked, then send.
    fn job_session_open(&mut self, session: SessionId, offered: &[ModelInfo]) {
        let Some(agent) = self.ready_agent() else {
            return;
        };
        let want = match &self.ai.ask {
            AskBox::Sending(j) => j.draft.model.clone(),
            _ => return,
        };
        match resolve_model(&want, offered) {
            Err(e) => self.fail_job(e),
            Ok(Some(id)) => match self.controller().set_model(agent, &session, &id) {
                Ok(request) => self.set_stage(Stage::SetModel { session, request }),
                Err(skink_ai::AgentError::Unsupported(_)) => self.start_turn(session),
                Err(e) => self.fail_job(e.to_string()),
            },
            Ok(None) => self.start_turn(session),
        }
    }

    fn start_turn(&mut self, session: SessionId) {
        let Some(agent) = self.ready_agent() else {
            return;
        };
        let blocks = match &self.ai.ask {
            AskBox::Sending(j) => j.blocks.clone(),
            _ => return,
        };
        match self.controller().send_prompt(agent, &session, blocks) {
            Ok(turn) => self.set_stage(Stage::Running { session, turn }),
            Err(e) => self.fail_job(e.to_string()),
        }
    }

    /// Esc while a request runs.
    pub(crate) fn cancel_ai(&mut self) {
        let job = match std::mem::replace(&mut self.ai.ask, AskBox::Closed) {
            AskBox::Sending(j) | AskBox::Streaming(j) => j,
            other => {
                self.ai.ask = other;
                return;
            }
        };
        if let (Stage::Running { session, .. }, Some(agent)) = (&job.stage, self.ready_agent()) {
            let _ = self.controller().cancel(agent, session);
        }
        self.doc.editor.message = Some("AI request cancelled".into());
    }

    /// The turn ended: turn the reply into a review, an answer or results.
    fn finish_job(&mut self, stop: &StopReason) {
        let AskBox::Streaming(job) = std::mem::replace(&mut self.ai.ask, AskBox::Closed) else {
            return;
        };
        if *stop == StopReason::Cancelled {
            return;
        }
        let Job {
            draft, work, reply, ..
        } = *job;
        match work {
            Work::Rewrite(b) => {
                let r = b.finish(&reply);
                self.open_review(draft, r.ok(), &reply);
            }
            Work::Implement(b) => {
                let text = draft
                    .target
                    .as_ref()
                    .and_then(|t| self.find_doc(|d| d.id == t.doc))
                    .and_then(|i| self.doc_at(i))
                    .map(|d| d.editor.buffer.text())
                    .unwrap_or_default();
                let r = b.finish(&reply, &text);
                self.open_review(draft, r.ok(), &reply);
            }
            Work::Ask { title, .. } => {
                self.update_view(&title, &reply, false);
                self.doc.editor.message = Some(format!("Answer in the \"{title}\" tab"));
            }
            Work::Search { root, .. } => {
                let res = prompter::parse_search_results(&reply, &root, Some(&DiskFiles));
                if res.hits.is_empty() {
                    self.ai.ask = AskBox::Failed {
                        draft,
                        message: "The agent found nothing".into(),
                    };
                    return;
                }
                let rows = res
                    .hits
                    .iter()
                    .map(|h| {
                        let rel = h.path.strip_prefix(&root).unwrap_or(&h.path);
                        (
                            format!("{}:{}  {}", rel.display(), h.line, h.note),
                            Some(crate::doc::Jump {
                                path: h.path.clone(),
                                line: h.line.saturating_sub(1),
                                col: h.col.saturating_sub(1),
                            }),
                        )
                    })
                    .collect();
                self.open_results(&format!("AI search: {}", draft.input.trim()), rows);
            }
        }
    }

    fn open_review(&mut self, draft: Draft, r: Option<EditProposal>, reply: &str) {
        let Some(p) = r else {
            // Never a dead end: say what came back, and show any prose.
            let message = if reply.trim().is_empty() {
                format!(
                    "{} sent an empty answer (check its login and model: run it once in a terminal tab)",
                    self.agent_label()
                )
            } else {
                self.update_view("AI answer", reply, false);
                "The answer had no code to apply; it is open in the \"AI answer\" tab".to_owned()
            };
            self.ai.ask = AskBox::Failed { draft, message };
            return;
        };
        let Some(t) = draft.target.as_ref() else {
            return;
        };
        if p.is_noop() {
            self.doc.editor.message = Some("The agent suggested no change".into());
            return;
        }
        let n = p.diff.block_count();
        self.ai.ask = AskBox::Reviewing(Box::new(Review {
            doc: t.doc,
            proposal: p,
            hunks: vec![Hunk::Pending; n],
            current: 0,
        }));
        if let Some(i) = self.find_doc(|d| d.id == t.doc)
            && i != self.active
        {
            self.switch_to(i);
        }
    }

    /// The active document is the one under inline review.
    pub(crate) fn reviewing_here(&self) -> bool {
        matches!(&self.ai.ask, AskBox::Reviewing(r) if r.doc == self.doc.id)
    }

    /// The status-line worker: what the running request is doing.
    pub(crate) fn ai_status(&self) -> Option<String> {
        let j = match &self.ai.ask {
            AskBox::Sending(j) | AskBox::Streaming(j) => j,
            _ => {
                return matches!(self.ai.panel.session, PanelSession::Busy { .. })
                    .then(|| format!("✦ {} · chat · working (:agent cancel)", self.agent_label()));
            }
        };
        let what = match &j.stage {
            Stage::WaitAgent => "starting".to_owned(),
            Stage::OpenSession { .. } => "opening a session".to_owned(),
            Stage::SetModel { .. } => format!("switching to {}", j.draft.model.label()),
            Stage::Running { .. } if j.reply.is_empty() => "thinking".to_owned(),
            Stage::Running { .. } => format!("streaming {} chars", j.reply.chars().count()),
        };
        let activity = if j.activity.is_empty() {
            String::new()
        } else {
            format!(" · {}", j.activity)
        };
        Some(format!(
            "✦ {} · {} · {}s · {what}{activity} (Esc cancels)",
            self.agent_label(),
            j.draft.mode.label(),
            j.started.elapsed().as_secs(),
        ))
    }

    fn review_key(&mut self, spec: &skink_config::KeySpec) {
        use skink_config::Key;
        let AskBox::Reviewing(r) = &mut self.ai.ask else {
            return;
        };
        let n = r.hunks.len();
        match spec.key {
            Key::Enter if spec.ctrl => {
                r.hunks.iter_mut().for_each(|h| *h = Hunk::Accepted);
                self.apply_review();
            }
            Key::Down | Key::Tab | Key::Char('j' | ']') => r.current = (r.current + 1).min(n - 1),
            Key::Up | Key::BackTab | Key::Char('k' | '[') => {
                r.current = r.current.saturating_sub(1)
            }
            Key::Char('u') => r.hunks[r.current] = Hunk::Pending,
            Key::Char('y') => {
                r.hunks[r.current] = Hunk::Accepted;
                r.current = (r.current + 1).min(n - 1);
            }
            Key::Char('n') => {
                r.hunks[r.current] = Hunk::Rejected;
                r.current = (r.current + 1).min(n - 1);
            }
            Key::Char('a') => {
                r.hunks.iter_mut().for_each(|h| *h = Hunk::Accepted);
                self.apply_review();
            }
            Key::Enter => self.apply_review(),
            Key::Escape => {
                self.ai.ask = AskBox::Closed;
                self.doc.editor.message = Some("AI suggestion discarded".into());
            }
            _ => {}
        }
    }

    /// Write the accepted (and undecided) hunks into the document as one
    /// undo step.
    fn apply_review(&mut self) {
        let AskBox::Reviewing(r) = std::mem::replace(&mut self.ai.ask, AskBox::Closed) else {
            return;
        };
        let Some(i) = self.find_doc(|d| d.id == r.doc) else {
            return;
        };
        if i != self.active {
            self.switch_to(i);
        }
        let text = self.doc.editor.buffer.text();
        let hunks = r.hunks.clone();
        match r.proposal.apply_to(&text, |h| hunks[h].applies()) {
            Ok(new) => {
                self.doc.editor.set_text(&new);
                let kept = hunks.iter().filter(|h| h.applies()).count();
                self.doc.editor.message =
                    Some(format!("Applied {kept} of {} change(s)", hunks.len()));
            }
            Err(e) => self.doc.editor.message = Some(format!("Not applied: {e}")),
        }
    }

    // ---- events ---------------------------------------------------------------------

    /// Drain agent events (called from the event loop).
    pub(crate) fn poll_ai(&mut self) {
        self.poll_agent_check();
        let events = match &mut self.ai.controller {
            Controller::Built(c) => c.poll_events(),
            Controller::Delegated(d) => d.poll_events(),
            Controller::Unbuilt => return,
        };
        for (agent, ev) in events {
            self.on_agent_event(agent, ev);
        }
    }

    pub(crate) fn on_agent_event(&mut self, agent: AgentId, ev: AgentEvent) {
        match ev {
            AgentEvent::Ready { info } => {
                if let Conn::Starting { agent: a, label } = &self.ai.conn
                    && *a == agent
                {
                    self.ai.conn = Conn::Ready {
                        agent,
                        label: label.clone(),
                        resume: info.load_session,
                    };
                    self.advance_job();
                    self.advance_panel();
                }
            }
            AgentEvent::StartFailed { message } => {
                self.ai.conn = Conn::Failed(message.clone());
                self.fail_job(message.clone());
                self.panel_note(&format!("Agent failed to start: {message}"));
                self.ai.panel.session = PanelSession::None;
            }
            AgentEvent::Exited { stderr_tail, .. } => {
                let tail = stderr_tail.last().cloned().unwrap_or_default();
                let msg = format!("The agent exited. {tail}");
                self.ai.conn = Conn::Failed(msg.clone());
                if matches!(self.ai.ask, AskBox::Sending(_) | AskBox::Streaming(_)) {
                    self.fail_job(msg.clone());
                }
                match std::mem::replace(&mut self.ai.panel.session, PanelSession::None) {
                    PanelSession::Idle { session } | PanelSession::Busy { session } => {
                        self.panel_note(&format!(
                            "{msg} Your next message restarts it and resumes this conversation when the agent can."
                        ));
                        self.ai.panel.session = PanelSession::Ended { session };
                    }
                    PanelSession::Opening { .. } => self.panel_note(&msg),
                    keep @ (PanelSession::None | PanelSession::Ended { .. }) => {
                        self.ai.panel.session = keep;
                    }
                }
                for p in self.ai.permissions.drain(..) {
                    p.reply.cancel();
                }
            }
            AgentEvent::SessionStarted { request, session } => {
                if !session.models.is_empty() {
                    self.ai.models = session.models.clone();
                }
                let for_job = matches!(&self.ai.ask, AskBox::Sending(j)
                    if j.stage == Stage::OpenSession { request });
                if for_job {
                    self.job_session_open(session.id, &session.models);
                } else if matches!(&self.ai.panel.session,
                    PanelSession::Opening { request: r } if *r == request)
                {
                    self.ai.panel.session = PanelSession::Idle {
                        session: session.id,
                    };
                    self.advance_panel();
                }
            }
            AgentEvent::RequestFailed { request, error } => {
                let for_job = matches!(&self.ai.ask, AskBox::Sending(j)
                    if matches!(&j.stage, Stage::OpenSession { request: r }
                        | Stage::SetModel { request: r, .. } if *r == request));
                if for_job {
                    self.fail_job(error_text(&error));
                } else if matches!(&self.ai.panel.session,
                    PanelSession::Opening { request: r } if *r == request)
                {
                    self.ai.panel.session = PanelSession::None;
                    self.panel_note(&format!("Could not open a session: {}", error_text(&error)));
                }
            }
            AgentEvent::ModelChanged { session, .. } => {
                let for_job = matches!(&self.ai.ask, AskBox::Sending(j)
                    if matches!(&j.stage, Stage::SetModel { session: s, .. } if *s == session));
                if for_job {
                    self.start_turn(session);
                }
            }
            AgentEvent::MessageChunk { session, text } => {
                if let Some(j) = self.job_for(&session) {
                    j.reply.push_str(&text);
                    if let Work::Ask { title, .. } = &j.work {
                        let (title, reply) = (title.clone(), j.reply.clone());
                        self.update_view(&title, &reply, false);
                    }
                } else if self.is_panel(&session) {
                    match self.ai.panel.transcript.last_mut() {
                        Some(Entry::Agent(t)) => t.push_str(&text),
                        _ => self.ai.panel.transcript.push(Entry::Agent(text)),
                    }
                    self.render_panel();
                }
            }
            AgentEvent::ThoughtChunk { session, text } => {
                if self.is_panel(&session) {
                    match self.ai.panel.transcript.last_mut() {
                        Some(Entry::Thought(t)) => t.push_str(&text),
                        _ => self.ai.panel.transcript.push(Entry::Thought(text)),
                    }
                    self.render_panel();
                }
            }
            AgentEvent::ToolCall { session, call } => {
                if let Some(j) = self.job_for(&session) {
                    j.activity = call.title.clone();
                } else if self.is_panel(&session) {
                    self.ai.panel.transcript.push(Entry::Tool(call));
                    self.render_panel();
                }
            }
            AgentEvent::ToolCallUpdate { session, patch } => {
                if self.is_panel(&session) {
                    if let Some(Entry::Tool(t)) = self
                        .ai
                        .panel
                        .transcript
                        .iter_mut()
                        .rev()
                        .find(|e| matches!(e, Entry::Tool(t) if t.id == patch.id))
                    {
                        t.apply(&patch);
                    }
                    self.render_panel();
                }
            }
            AgentEvent::Plan { session, entries } => {
                if self.is_panel(&session) {
                    match self.ai.panel.transcript.last_mut() {
                        Some(Entry::Plan(p)) => *p = entries,
                        _ => self.ai.panel.transcript.push(Entry::Plan(entries)),
                    }
                    self.render_panel();
                }
            }
            AgentEvent::Permission(req) => {
                if self.ai.permissions.is_empty() {
                    self.ai.permission_choice = 0;
                }
                self.ai.permissions.push_back(req);
            }
            AgentEvent::TurnFinished { session, stop, .. } => {
                if self.job_for(&session).is_some() {
                    self.finish_job(&stop);
                } else if self.is_panel(&session) {
                    self.ai.panel.session = PanelSession::Idle { session };
                    if stop == StopReason::Cancelled {
                        self.panel_note("Cancelled");
                    }
                    self.render_panel();
                    self.advance_panel();
                    self.review_staged();
                }
            }
            AgentEvent::TurnFailed { session, error, .. } => {
                if self.job_for(&session).is_some() {
                    self.fail_job(error_text(&error));
                } else if self.is_panel(&session) {
                    self.ai.panel.session = PanelSession::Idle { session };
                    self.panel_note(&format!("Error: {}", error_text(&error)));
                    self.advance_panel();
                }
            }
            AgentEvent::Authenticated { .. }
            | AgentEvent::UserChunk { .. }
            | AgentEvent::Commands { .. }
            | AgentEvent::ModeChanged { .. }
            | AgentEvent::ProtocolError { .. } => {}
        }
    }

    fn job_for(&mut self, session: &SessionId) -> Option<&mut Job> {
        match &mut self.ai.ask {
            AskBox::Streaming(j) => match &j.stage {
                Stage::Running { session: s, .. } if s == session => Some(j),
                _ => None,
            },
            _ => None,
        }
    }

    // ---- permissions ----------------------------------------------------------------

    fn permission_key(&mut self, spec: &skink_config::KeySpec) {
        use skink_config::Key;
        let Some(req) = self.ai.permissions.front() else {
            return;
        };
        let n = req.options.len();
        let pick = match spec.key {
            Key::Up => {
                self.ai.permission_choice = self.ai.permission_choice.saturating_sub(1);
                None
            }
            Key::Down | Key::Tab => {
                self.ai.permission_choice =
                    (self.ai.permission_choice + 1).min(n.saturating_sub(1));
                None
            }
            Key::Char(c @ '1'..='9') => Some(c as usize - '1' as usize),
            Key::Enter => Some(self.ai.permission_choice),
            // Esc: the first "reject" option, else cancel.
            Key::Escape => Some(
                req.options
                    .iter()
                    .position(|o| matches!(o.kind, PermissionKind::RejectOnce))
                    .unwrap_or(usize::MAX),
            ),
            _ => None,
        };
        let Some(i) = pick else {
            return;
        };
        let Some(req) = self.ai.permissions.pop_front() else {
            return;
        };
        match req.options.get(i) {
            Some(o) => {
                req.reply.select(o.id.clone());
            }
            None => {
                req.reply.cancel();
            }
        }
        self.ai.permission_choice = 0;
    }

    // ---- agent panel ----------------------------------------------------------------

    fn is_panel(&self, session: &SessionId) -> bool {
        matches!(&self.ai.panel.session,
            PanelSession::Idle { session: s } | PanelSession::Busy { session: s } if s == session)
    }

    /// `:agent [message]`: show the panel; with a message, send it.
    pub(crate) fn open_agent_panel(&mut self, message: &str) {
        if self.ai.panel.transcript.is_empty() {
            let label = self.agent_label();
            self.ai.panel.transcript.push(Entry::Note(format!(
                "{label}. Ctrl-G here types a message; :agent cancel stops a turn."
            )));
        }
        self.render_panel_switch(true);
        if !message.trim().is_empty() {
            self.panel_send(message.trim());
        }
    }

    pub(crate) fn panel_send(&mut self, text: &str) {
        self.panel_send_as(text, text);
    }

    /// `shown` in the transcript, `sent` (with the context chips expanded)
    /// to the agent.
    fn panel_send_as(&mut self, shown: &str, sent: &str) {
        self.ai.panel.transcript.push(Entry::User(shown.to_owned()));
        self.ai.panel.queued.push(sent.to_owned());
        self.snapshot_buffers();
        if !self.ensure_agent() {
            let m = match &self.ai.conn {
                Conn::Failed(m) => m.clone(),
                _ => "No agent".into(),
            };
            self.ai.panel.queued.clear();
            self.panel_note(&m);
        }
        self.advance_panel();
        self.render_panel_switch(true);
    }

    /// Open the panel session, or send the next queued message.
    fn advance_panel(&mut self) {
        let Some(agent) = self.ready_agent() else {
            return;
        };
        if self.ai.panel.queued.is_empty() {
            return;
        }
        match &self.ai.panel.session {
            PanelSession::Ended { session }
                if matches!(self.ai.conn, Conn::Ready { resume: true, .. }) =>
            {
                let (session, cwd) = (session.clone(), self.project_root());
                match self.controller().load_session(agent, &session, &cwd) {
                    Ok(request) => {
                        self.panel_note("Resuming the previous conversation");
                        self.ai.panel.session = PanelSession::Opening { request };
                    }
                    Err(e) => {
                        self.panel_note(&format!("Could not resume ({e}): starting a new one"));
                        self.ai.panel.session = PanelSession::None;
                        self.advance_panel();
                    }
                }
            }
            PanelSession::None | PanelSession::Ended { .. } => {
                if matches!(self.ai.panel.session, PanelSession::Ended { .. }) {
                    self.panel_note(
                        "This agent cannot reopen past conversations: starting a new one",
                    );
                }
                let cwd = self.project_root();
                let opts = crate::agents::session_options(self.settings.ai.effort_for(AiMode::Ask));
                match self.controller().new_session_opts(agent, &cwd, opts) {
                    Ok(request) => self.ai.panel.session = PanelSession::Opening { request },
                    Err(e) => self.panel_note(&e.to_string()),
                }
            }
            PanelSession::Idle { session } => {
                let session = session.clone();
                let text = self.ai.panel.queued.remove(0);
                match self
                    .controller()
                    .send_prompt(agent, &session, vec![PromptBlock::text(text)])
                {
                    Ok(_) => self.ai.panel.session = PanelSession::Busy { session },
                    Err(e) => self.panel_note(&e.to_string()),
                }
            }
            PanelSession::Opening { .. } | PanelSession::Busy { .. } => {}
        }
    }

    /// `:agent cancel`
    pub(crate) fn cancel_panel(&mut self) {
        if let (PanelSession::Busy { session }, Some(agent)) =
            (&self.ai.panel.session, self.ready_agent())
        {
            let session = session.clone();
            let _ = self.controller().cancel(agent, &session);
        } else {
            self.doc.editor.message = Some("The agent is not working on anything".into());
        }
    }

    /// End the agent process without a word (the agent setting changed).
    pub(crate) fn stop_agent_quietly(&mut self) {
        if let Conn::Starting { agent, .. } | Conn::Ready { agent, .. } = &self.ai.conn {
            let a = *agent;
            self.controller().stop_agent(a);
        }
        self.ai.conn = Conn::Off;
        self.ai.panel.session = PanelSession::None;
    }

    /// `:agent stop`: end the agent process.
    pub(crate) fn stop_agent(&mut self) {
        let agent = match &self.ai.conn {
            Conn::Starting { agent, .. } | Conn::Ready { agent, .. } => Some(*agent),
            Conn::Off | Conn::Failed(_) => None,
        };
        if let Some(a) = agent {
            self.controller().stop_agent(a);
        }
        self.ai.conn = Conn::Off;
        self.ai.panel.session = PanelSession::None;
        self.fail_job("The agent was stopped".into());
        self.doc.editor.message = Some("Agent stopped".into());
    }

    /// `:agent send`: the selection, or the terminal's screen, as a message.
    pub(crate) fn send_context_to_agent(&mut self) {
        let text = if self.active_is_term() {
            self.terminal_text()
                .map(|t| format!("Terminal output:\n```\n{t}\n```"))
        } else {
            self.doc.editor.selected_text().map(|s| {
                let name = self.doc.title();
                format!("From {name}:\n```\n{s}\n```")
            })
        };
        match text {
            Some(t) => self.open_agent_panel(&t),
            None => self.doc.editor.message = Some("Select some text first".into()),
        }
    }

    /// A chat message with its chips resolved: `@file` / `#rule` contents,
    /// `@selection` (of the file tab with one), `@diagnostics`, `@terminal`.
    fn chat_message(&mut self, text: &str) -> String {
        let (rest, [want_sel, want_diags, want_term]) = take_special(text);
        // The panel is no file: chips resolve against the file tab with a
        // selection (else any file tab) and its project.
        let src = self
            .find_doc(|d| d.canon.is_some() && d.editor.selection().is_some())
            .or_else(|| self.find_doc(|d| d.canon.is_some()));
        let src = src.and_then(|i| self.doc_at(i));
        let root = src
            .and_then(|d| d.canon.as_deref()?.parent())
            .map_or_else(|| self.project_root(), crate::find_project_root);
        let cfg = ContextConfig::new(&root);
        let current = src
            .and_then(|d| d.canon.clone())
            .unwrap_or_else(|| root.join("_"));
        let (refs, _) = prompter::expand_references(&cfg, &DiskFiles, &current, &rest);
        let mut out = rest.clone();
        for r in refs {
            let what = match r.kind {
                prompter::RefKind::File => "File",
                prompter::RefKind::Rule => "Rule",
            };
            let lang = prompter::language_for(&r.path).unwrap_or("");
            out.push_str(&format!(
                "\n\n## {what} {}\n```{lang}\n{}\n```",
                skink_ai::prompter::context::display_path(&root, &r.path),
                r.content.trim_end()
            ));
        }
        if want_sel
            && let Some((name, s)) = src.and_then(|d| Some((d.title(), d.editor.selected_text()?)))
        {
            out.push_str(&format!("\n\n## Selection from {name}\n```\n{s}\n```"));
        }
        if want_diags && let Some(e) = self.problems_text() {
            out.push_str(&format!("\n\n## Diagnostics\n{e}"));
        }
        if want_term && let Some(t) = self.terminal_text() {
            out.push_str(&format!("\n\n## Terminal output\n```\n{t}\n```"));
        }
        out
    }

    /// Errors and warnings of the open files, one per line with the code
    /// line under it (for the agent).
    fn problems_text(&self) -> Option<String> {
        let root = self.project_root();
        let mut out = Vec::new();
        for i in 0..self.tabs.len() {
            let Some(d) = self.doc_at(i) else { continue };
            let Some(p) = &d.canon else { continue };
            for g in self.all_diags(p) {
                let sev = match g.severity {
                    skink_lsp::Severity::Error => "error",
                    skink_lsp::Severity::Warning => "warning",
                    _ => continue,
                };
                let (l, c) = (g.range.start.line, g.range.start.col);
                let rel = p.strip_prefix(&root).unwrap_or(p).display();
                let code = d
                    .editor
                    .buffer
                    .line_text(l.min(d.editor.buffer.len_lines() - 1));
                out.push(format!(
                    "{rel}:{}:{}: {sev}: {}\n    {}",
                    l + 1,
                    c + 1,
                    g.message,
                    code.trim()
                ));
                if out.len() >= 50 {
                    break;
                }
            }
        }
        (!out.is_empty()).then(|| out.join("\n"))
    }

    /// The last command and its output in a terminal tab (the active one,
    /// else the most recent).
    fn last_command_output(&mut self) -> Option<String> {
        let i = if self.active_is_term() {
            self.active
        } else {
            (0..self.tabs.len())
                .rev()
                .find(|&i| matches!(self.tabs[i], crate::Tab::Term(_)))?
        };
        let crate::Tab::Term(p) = &mut self.tabs[i] else {
            return None;
        };
        // Exact with shell-integration marks; else guessed from the screen.
        if let Some(t) = crate::termtools::exact_last_output(&p.term) {
            return Some(t);
        }
        p.term.snapshot_into(&mut p.screen);
        last_command(&p.screen.text())
    }

    /// Alt-E / `:agent errors`: the errors to the agent panel. In a
    /// terminal, its last command's output; elsewhere the open files'
    /// diagnostics, else the last terminal command.
    pub(crate) fn send_errors_to_agent(&mut self) {
        let body = if self.active_is_term() {
            self.last_command_output()
                .map(|t| format!("Terminal output:\n```\n{t}\n```"))
        } else {
            match self.problems_text() {
                Some(p) => Some(format!("Diagnostics:\n{p}")),
                None => self
                    .last_command_output()
                    .map(|t| format!("Terminal output:\n```\n{t}\n```")),
            }
        };
        match body {
            Some(b) => self.open_agent_panel(&format!(
                "Fix these errors (say briefly what caused them):\n{b}"
            )),
            None => {
                self.doc.editor.message =
                    Some("No errors: no diagnostics and no terminal output".into())
            }
        }
    }

    fn panel_note(&mut self, s: &str) {
        self.ai.panel.transcript.push(Entry::Note(s.to_owned()));
        self.render_panel();
    }

    fn render_panel(&mut self) {
        self.render_panel_switch(false);
    }

    fn render_panel_switch(&mut self, show: bool) {
        let text = panel_text(&self.ai.panel, &self.ai.conn);
        if show {
            let here_is_panel = self.doc.editor.buffer.title().as_deref() == Some(PANEL_TITLE);
            if !here_is_panel {
                self.open_view(PANEL_TITLE, &text);
            }
        }
        self.update_view(PANEL_TITLE, &text, true);
    }

    /// Replace the text of the read-only tab `title` (if open) without
    /// switching to it; `follow` keeps the cursor at the end.
    pub(crate) fn update_view(&mut self, title: &str, text: &str, follow: bool) {
        let Some(i) = self.find_doc(|d| {
            d.canon.is_none()
                && d.editor.buffer.is_read_only()
                && d.editor.buffer.title().as_deref() == Some(title)
        }) else {
            return;
        };
        if let Some(d) = self.doc_at_mut(i) {
            d.replace_view(title, text);
            if follow {
                d.editor.move_file_end(false);
            }
        }
    }

    /// The agent finished a turn with staged edits: open the preview.
    pub(crate) fn review_staged(&mut self) {
        let staged = self.ai.shared.staged();
        if staged.is_empty() {
            return;
        }
        let files: Vec<FilePatch> = staged
            .into_iter()
            .map(|(p, new)| {
                let base = self.text_of(&p).unwrap_or_default();
                FilePatch::new(p, base, &new)
            })
            .collect();
        self.open_refactor(Preview::new("Agent edits", Source::Agent, files));
    }

    /// `:model [name]`
    pub(crate) fn set_model_command(&mut self, arg: &str) {
        let arg = arg.trim();
        if arg.is_empty() {
            let list: Vec<&str> = self.ai.models.iter().map(|m| m.id.as_str()).collect();
            let cur = self.default_pick(BoxMode::Rewrite);
            self.doc.editor.message = Some(if list.is_empty() {
                format!(
                    "Model: {} (the agent lists its models after the first request)",
                    cur.label()
                )
            } else {
                format!("Model: {} · available: {}", cur.label(), list.join(", "))
            });
            return;
        }
        let p = match arg {
            "default" => Pick::AgentDefault,
            m => Pick::Named(m.to_owned()),
        };
        self.doc.editor.message = Some(format!("AI requests now use {}", p.label()));
        self.ai.model_source = ModelSource::Run(p);
    }
}

/// The last command block of a terminal screen: from the previous line that
/// starts with the current prompt to just before that prompt. Without shell
/// integration the prompt is guessed as the text of the last non-empty line
/// (where the cursor waits); falls back to the screen's last 40 lines.
pub(crate) fn last_command(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    let end = lines.iter().rposition(|l| !l.trim().is_empty())?;
    let prompt = lines[end].trim();
    let from = (!prompt.is_empty())
        .then(|| {
            lines[..end]
                .iter()
                .rposition(|l| l.trim_start().starts_with(prompt))
        })
        .flatten()
        .unwrap_or(end.saturating_sub(40));
    let block = lines[from..end].join("\n");
    (!block.trim().is_empty()).then_some(block)
}

fn work_prompt(w: &Work) -> String {
    match w {
        Work::Rewrite(b) => b.prompt.text.clone(),
        Work::Implement(b) => b.prompt.text.clone(),
        Work::Ask { prompt, .. } | Work::Search { prompt, .. } => prompt.clone(),
    }
}

fn accept_suggestion(d: &mut Draft) {
    let Suggest::List {
        start,
        items,
        selected,
    } = std::mem::replace(&mut d.suggest, Suggest::None)
    else {
        return;
    };
    let Some(item) = items.get(selected) else {
        return;
    };
    let sigil = d.input.chars().nth(start).unwrap_or('@');
    remove_range(&mut d.input, start, d.cursor);
    let text = format!("{sigil}{item} ");
    insert_at(&mut d.input, start, &text);
    d.cursor = start + text.chars().count();
}

/// Fill the completion list for the `@` / `#` word at the cursor.
fn refresh_suggestions(d: &mut Draft, root: &Path) {
    d.suggest = Suggest::None;
    let Some((start, sigil, typed)) = ref_word(&d.input, d.cursor) else {
        return;
    };
    let mut items: Vec<(i32, String)> = Vec::new();
    let q = crate::picker::prepare_query(&typed);
    let mut consider = |s: String| {
        if let Some(sc) = crate::picker::score_prepared(&q, &s.to_lowercase()) {
            items.push((sc, s));
        }
    };
    if sigil == '@' {
        for s in SPECIAL_REFS {
            consider(s[1..].to_owned());
        }
        for p in crate::picker::project_files(root, 5000) {
            if let Ok(rel) = p.strip_prefix(root) {
                consider(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    } else {
        for dir in [".skink/rules", ".cursor/rules"] {
            if let Ok(rd) = std::fs::read_dir(root.join(dir)) {
                for e in rd.flatten() {
                    let p = e.path();
                    if let Some(stem) = p.file_stem() {
                        consider(stem.to_string_lossy().into_owned());
                    }
                }
            }
        }
    }
    items.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
    items.truncate(6);
    if !items.is_empty() {
        d.suggest = Suggest::List {
            start,
            items: items.into_iter().map(|(_, s)| s).collect(),
            selected: 0,
        };
    }
}

/// The panel's transcript as text.
fn panel_text(p: &Panel, conn: &Conn) -> String {
    let mut out = String::new();
    for e in &p.transcript {
        match e {
            Entry::User(t) => {
                out.push_str("▍ You\n");
                out.push_str(t);
                out.push_str("\n\n");
            }
            Entry::Agent(t) => {
                out.push_str("▍ Agent\n");
                out.push_str(t.trim_end());
                out.push_str("\n\n");
            }
            Entry::Thought(t) => {
                let short: String = t.trim().chars().take(400).collect();
                out.push_str(&format!("  (thinking) {}\n\n", short.replace('\n', " ")));
            }
            Entry::Tool(t) => {
                let mark = match t.status {
                    ToolStatus::Pending => "○",
                    ToolStatus::InProgress => "◐",
                    ToolStatus::Completed => "●",
                    ToolStatus::Failed => "✗",
                };
                out.push_str(&format!("  {mark} {}\n", t.title));
                for l in &t.locations {
                    out.push_str(&format!("      {}\n", l.path.display()));
                }
                out.push('\n');
            }
            Entry::Plan(entries) => {
                out.push_str("  Plan\n");
                for e in entries {
                    let mark = match e.status {
                        PlanStatus::Pending => "[ ]",
                        PlanStatus::InProgress => "[~]",
                        PlanStatus::Completed => "[x]",
                    };
                    out.push_str(&format!("  {mark} {}\n", e.content));
                }
                out.push('\n');
            }
            Entry::Note(t) => {
                out.push_str(&format!("  — {t}\n\n"));
            }
        }
    }
    let status = match (&p.session, conn) {
        (_, Conn::Starting { label, .. }) => format!("starting {label}..."),
        (PanelSession::Opening { .. }, _) => "opening a session...".into(),
        (PanelSession::Busy { .. }, _) => "working... (:agent cancel)".into(),
        (_, Conn::Failed(m)) => format!("agent unavailable: {m}"),
        _ => "ready".into(),
    };
    out.push_str(&format!("── {status}\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_and_special_words() {
        assert_eq!(ref_word("fix @src/ma", 11), Some((4, '@', "src/ma".into())));
        assert_eq!(ref_word("fix it", 6), None);
        let (rest, f) = take_special("explain @selection with @diagnostics please");
        assert_eq!(rest, "explain with please");
        assert_eq!(f, [true, true, false]);
        assert_eq!(chips("use @a.rs and #style ok"), ["@a.rs", "#style"]);
    }

    #[test]
    fn model_resolution() {
        let m = |id: &str, name: &str| ModelInfo {
            id: id.into(),
            name: name.into(),
            description: None,
        };
        let offered = [m("claude-sonnet", "Sonnet"), m("claude-opus", "Opus")];
        assert_eq!(resolve_model(&Pick::AgentDefault, &offered), Ok(None));
        assert_eq!(
            resolve_model(&Pick::Named("opus".into()), &offered),
            Ok(Some("claude-opus".into()))
        );
        assert!(resolve_model(&Pick::Named("gpt".into()), &offered).is_err());
        // No list from the agent: keep its default.
        assert_eq!(resolve_model(&Pick::Named("x".into()), &[]), Ok(None));
    }
}
