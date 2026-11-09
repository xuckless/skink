//! Glue between the editor and `skink-lsp`. One worker thread owns every
//! call into the provider, in order: document sync (the first `did_open`
//! blocks on the server handshake) and queries (completion, hover, ...).
//! Queries carry a [`Ticket`]; their [`Reply`] comes back on a channel and
//! wakes the event loop. Diagnostics are read straight from the cache.

use skink_core::Backend;
use skink_lsp::{
    CodeAction, CompletionItem, Diagnostic, DocumentSymbol, FormattingOptions, GotoKind, Hover,
    InlayHint, Location, LspEvent, LspProvider, Position, Range, ServerCommand, Severity,
    SignatureHelp, SymbolInfo, TextEdit,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        mpsc::{self, Receiver, Sender},
    },
};

/// Identifies one query; replies to superseded tickets are ignored.
pub type Ticket = u64;

/// A question for the language server about one file.
#[derive(Debug, Clone)]
pub enum Ask {
    Completion(Position),
    Hover(Position),
    Signature(Position),
    Goto(GotoKind, Position),
    References(Position),
    Format(FormattingOptions),
    FormatRange(Range, FormattingOptions),
    CodeActions(Range, Vec<Diagnostic>),
    Execute(ServerCommand),
    DocumentSymbols,
    WorkspaceSymbols(String),
    InlayHints(Range),
    /// Documentation for a completion item that left it out.
    Resolve(Box<CompletionItem>),
}

#[derive(Debug)]
pub enum Answer {
    Completion(Vec<CompletionItem>),
    Hover(Option<Hover>),
    Signature(Option<SignatureHelp>),
    Locations(Vec<Location>),
    Edits(Vec<TextEdit>),
    Actions(Vec<CodeAction>),
    Done,
    DocumentSymbols(Vec<DocumentSymbol>),
    Symbols(Vec<SymbolInfo>),
    Inlay(Vec<InlayHint>),
    Resolved(Option<String>),
}

#[derive(Debug)]
pub struct Reply {
    pub ticket: Ticket,
    pub result: Result<Answer, String>,
}

enum Cmd {
    Open(PathBuf, String),
    Change(PathBuf, String),
    Save(PathBuf),
    Close(PathBuf),
    Ask(Ticket, PathBuf, Ask),
    /// Shut the file's server down; the next open starts a fresh one.
    Restart(PathBuf),
}

/// What the servers have been doing, for the status line and `:lsp log`.
#[derive(Debug, Default)]
pub struct Activity {
    /// Latest progress title per server ("indexing 40%"), cleared when done.
    pub progress: Vec<(String, String)>,
    /// Bounded log of messages, stderr lines and exits.
    pub log: std::collections::VecDeque<String>,
    /// Servers that exited unexpectedly since the last look.
    pub crashed: Vec<String>,
}

const LOG_MAX: usize = 2000;

pub struct LspLink {
    /// Set once the worker has chosen local or EveryAI-delegated servers
    /// (probing the host can take a moment, so it happens off the UI thread).
    provider: Arc<OnceLock<(Backend, Arc<dyn LspProvider>)>>,
    tx: Sender<Cmd>,
    replies: Receiver<Reply>,
    next_ticket: Ticket,
    pub activity: Activity,
}

impl LspLink {
    /// `wake` is called when a server publishes something new.
    pub fn start(root: PathBuf, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let w = Arc::clone(&wake);
        Self::spawn(wake, move || {
            let opts = skink_host::ProviderOptions {
                waker: Some(Arc::new(move || w())),
                ..Default::default()
            };
            let p = skink_host::providers_with(&root, |k| std::env::var(k).ok(), &opts);
            (p.backend, p.lsp)
        })
    }

    /// A link over a given provider (tests, embedders).
    #[cfg(test)]
    pub fn with_provider(p: Arc<dyn LspProvider>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self::spawn(Arc::new(wake), move || (Backend::Local, p))
    }

    fn spawn(
        wake: Arc<dyn Fn() + Send + Sync>,
        make: impl FnOnce() -> (Backend, Arc<dyn LspProvider>) + Send + 'static,
    ) -> Self {
        let provider = Arc::new(OnceLock::new());
        let (tx, rx) = mpsc::channel();
        let (rtx, replies) = mpsc::channel();
        let slot = Arc::clone(&provider);
        std::thread::Builder::new()
            .name("skink-lsp".into())
            .spawn(move || {
                let (backend, lsp) = make();
                let _ = slot.set((backend, Arc::clone(&lsp)));
                work_loop(lsp.as_ref(), &rx, &rtx, wake.as_ref());
            })
            .ok();
        Self {
            provider,
            tx,
            replies,
            next_ticket: 1,
            activity: Activity::default(),
        }
    }

    /// Queue `ask` about `path`; edits already sent are seen first.
    pub fn ask(&mut self, path: &Path, ask: Ask) -> Ticket {
        let t = self.next_ticket;
        self.next_ticket += 1;
        let _ = self.tx.send(Cmd::Ask(t, path.to_path_buf(), ask));
        t
    }

    /// Replies that arrived since the last call.
    pub fn replies(&self) -> Vec<Reply> {
        self.replies.try_iter().collect()
    }

    pub fn restart(&self, path: &Path) {
        let _ = self.tx.send(Cmd::Restart(path.to_path_buf()));
    }

    /// Stop servers with no open documents for `after` (they restart on
    /// next use). Returns the names of those stopped.
    pub fn stop_idle(&self, after: std::time::Duration) -> Vec<String> {
        self.provider
            .get()
            .map_or_else(Vec::new, |(_, l)| l.stop_idle(after))
    }

    pub fn server_pids(&self) -> Vec<u32> {
        self.provider
            .get()
            .map_or_else(Vec::new, |(_, l)| l.server_pids())
    }

    /// Server status for `path` (never starts one).
    pub fn status(&self, path: &Path) -> Option<skink_lsp::ServerStatus> {
        self.provider.get().map(|(_, l)| l.status(path))
    }

    /// Which side serves language features (None until the worker decided).
    pub fn backend(&self) -> Option<Backend> {
        self.provider.get().map(|(b, _)| *b)
    }

    pub fn open(&self, path: &Path, text: String) {
        let _ = self.tx.send(Cmd::Open(path.to_path_buf(), text));
    }

    pub fn change(&self, path: &Path, text: String) {
        let _ = self.tx.send(Cmd::Change(path.to_path_buf(), text));
    }

    pub fn close(&self, path: &Path) {
        let _ = self.tx.send(Cmd::Close(path.to_path_buf()));
    }

    pub fn save(&self, path: &Path) {
        let _ = self.tx.send(Cmd::Save(path.to_path_buf()));
    }

    /// Cached diagnostics; never blocks on the server.
    pub fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        self.provider
            .get()
            .map_or_else(Vec::new, |(_, l)| l.diagnostics(path))
    }

    /// Fold queued server events into [`Activity`] (diagnostics are read
    /// from the cache). Returns edits servers asked to apply.
    pub fn drain(&mut self) -> Vec<skink_lsp::WorkspaceEdit> {
        let Some((_, l)) = self.provider.get() else {
            return Vec::new();
        };
        let mut edits = Vec::new();
        let a = &mut self.activity;
        let mut log = |line: String| {
            if a.log.len() == LOG_MAX {
                a.log.pop_front();
            }
            a.log.push_back(line);
        };
        for ev in l.poll_events() {
            match ev {
                LspEvent::Diagnostics { .. } => {}
                LspEvent::Message {
                    server,
                    level,
                    text,
                    ..
                } => log(format!("[{server}] {level:?}: {text}")),
                LspEvent::Progress {
                    server,
                    title,
                    message,
                    percentage,
                    done,
                    ..
                } => {
                    a.progress.retain(|(s, _)| **s != *server);
                    if !done {
                        let mut t = title.unwrap_or_default();
                        if let Some(m) = message {
                            t = format!("{t} {m}");
                        }
                        if let Some(p) = percentage {
                            t = format!("{t} {p}%");
                        }
                        a.progress.push((server.to_string(), t.trim().to_owned()));
                    }
                }
                LspEvent::ApplyEdit { edit, .. } => edits.push(edit),
                LspEvent::Exited {
                    server,
                    code,
                    expected,
                } => {
                    log(format!("[{server}] exited (code {code:?})"));
                    a.progress.retain(|(s, _)| **s != *server);
                    if !expected {
                        a.crashed.push(server.to_string());
                    }
                }
            }
        }
        edits
    }
}

fn answer(lsp: &dyn LspProvider, path: &Path, ask: Ask) -> Result<Answer, skink_lsp::LspError> {
    Ok(match ask {
        Ask::Completion(p) => Answer::Completion(lsp.completion(path, p)?),
        Ask::Hover(p) => Answer::Hover(lsp.hover(path, p)?),
        Ask::Signature(p) => Answer::Signature(lsp.signature_help(path, p)?),
        Ask::Goto(k, p) => Answer::Locations(lsp.goto(k, path, p)?),
        Ask::References(p) => Answer::Locations(lsp.references(path, p, true)?),
        Ask::Format(o) => Answer::Edits(lsp.formatting(path, o)?),
        Ask::FormatRange(r, o) => Answer::Edits(lsp.range_formatting(path, r, o)?),
        Ask::CodeActions(r, d) => Answer::Actions(lsp.code_actions(path, r, &d)?),
        Ask::Execute(c) => {
            lsp.execute_command(path, &c)?;
            Answer::Done
        }
        Ask::DocumentSymbols => Answer::DocumentSymbols(lsp.document_symbols(path)?),
        Ask::WorkspaceSymbols(q) => Answer::Symbols(lsp.workspace_symbols(path, &q)?),
        Ask::InlayHints(r) => Answer::Inlay(lsp.inlay_hints(path, r)?),
        Ask::Resolve(item) => Answer::Resolved(lsp.resolve_completion(path, &item)?),
    })
}

/// Same kind of question about the same file: the newer one replaces it.
fn same_slot(a: &(PathBuf, Ask), b: &(PathBuf, Ask)) -> bool {
    a.0 == b.0 && std::mem::discriminant(&a.1) == std::mem::discriminant(&b.1)
}

fn work_loop(
    lsp: &dyn LspProvider,
    rx: &Receiver<Cmd>,
    replies: &Sender<Reply>,
    wake: &(dyn Fn() + Send + Sync),
) {
    while let Ok(first) = rx.recv() {
        // Coalesce a burst: the latest text per file, the latest ask per kind.
        let mut batch = vec![first];
        batch.extend(rx.try_iter());
        let asks: Vec<(PathBuf, Ask)> = batch
            .iter()
            .filter_map(|c| match c {
                Cmd::Ask(_, p, a) => Some((p.clone(), a.clone())),
                _ => None,
            })
            .collect();
        let mut last_change: Vec<(PathBuf, String)> = Vec::new();
        let flush = |p: &Path, last_change: &mut Vec<(PathBuf, String)>| {
            if let Some(i) = last_change.iter().position(|(q, _)| q == p) {
                let (q, t) = last_change.remove(i);
                let _ = lsp.did_change(&q, &t);
            }
        };
        let mut seen = 0;
        for cmd in batch {
            match cmd {
                Cmd::Open(p, t) => {
                    last_change.retain(|(q, _)| *q != p);
                    let _ = lsp.did_open(&p, &t);
                }
                Cmd::Change(p, t) => {
                    last_change.retain(|(q, _)| *q != p);
                    last_change.push((p, t));
                }
                Cmd::Close(p) => {
                    last_change.retain(|(q, _)| *q != p);
                    let _ = lsp.did_close(&p);
                }
                Cmd::Save(p) => {
                    for (q, t) in last_change.drain(..) {
                        let _ = lsp.did_change(&q, &t);
                    }
                    let _ = lsp.did_save(&p);
                }
                Cmd::Restart(p) => {
                    last_change.retain(|(q, _)| *q != p);
                    lsp.restart(&p);
                }
                Cmd::Ask(ticket, p, a) => {
                    let me = (p, a);
                    seen += 1;
                    let superseded = asks[seen..].iter().any(|later| same_slot(&me, later));
                    let result = if superseded {
                        Err("superseded".to_owned())
                    } else {
                        flush(&me.0, &mut last_change);
                        answer(lsp, &me.0, me.1).map_err(|e| e.to_string())
                    };
                    if replies.send(Reply { ticket, result }).is_ok() {
                        wake();
                    }
                }
            }
        }
        for (p, t) in last_change {
            let _ = lsp.did_change(&p, &t);
        }
    }
}

/// Resident memory of process `pid` in MB (Linux; None elsewhere).
pub fn rss_mb(pid: u32) -> Option<f64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb: f64 = s
        .lines()
        .find(|l| l.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kb / 1024.0)
}

/// Worst severity per 0-based line and error/warning totals.
pub fn summarize(
    diags: &[Diagnostic],
) -> (std::collections::HashMap<usize, Severity>, usize, usize) {
    let mut by_line = std::collections::HashMap::new();
    let (mut errors, mut warnings) = (0, 0);
    for d in diags {
        match d.severity {
            Severity::Error => errors += 1,
            Severity::Warning => warnings += 1,
            _ => {}
        }
        let rank = |s: &Severity| match s {
            Severity::Error => 0,
            Severity::Warning => 1,
            Severity::Info => 2,
            Severity::Hint => 3,
        };
        let line = d.range.start.line;
        by_line
            .entry(line)
            .and_modify(|s: &mut Severity| {
                if rank(&d.severity) < rank(s) {
                    *s = d.severity;
                }
            })
            .or_insert_with(|| d.severity);
    }
    (by_line, errors, warnings)
}
