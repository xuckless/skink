//! [`DelegatedLsp`]: Skink's [`LspProvider`] served by EveryAI's `lsp.*` tools.
//!
//! The contract has no document-sync tools, so `did_open` / `did_change` /
//! `did_save` / `did_close` only update a local mirror of the buffers. Every
//! query then carries the buffer as a [`tools::Doc`] (`text` while it differs
//! from disk, omitted after `did_save`), which is exactly "unsaved content as
//! `didChange`" in `docs/HANDOVER.md`. The mirror also gives the text needed
//! to convert between Skink's char columns and the contract's UTF-16 columns.
//!
//! Diagnostics are pull-only in the contract (`lsp.diagnostics`), so they are
//! served from a cache that [`DelegatedLsp::refresh_diagnostics`] (blocking)
//! or [`DelegatedLsp::refresh_diagnostics_async`] (background, coalescing)
//! fills; changes also surface as [`LspEvent::Diagnostics`] from
//! `poll_events`.

use super::{HostLink, LinkError, lock};
use crate::client::Timeouts;
use crate::tools::{self, PositionArgs};
use serde::Serialize;
use serde::de::DeserializeOwned;
use skink_core::Backend;
use skink_lsp::{
    CompletionItem, Diagnostic, Doc, DocumentSymbol, FileEdit, FormattingOptions, Hover, ItemKind,
    Location, LspError, LspEvent, LspProvider, Position, Range, Result, ServerStatus, Severity,
    SignatureHelp, TextEdit, WorkspaceEdit,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Deadlines suited to editor-interactive LSP calls (hover, completion, ...):
/// each call is cut off after 30 s overall and 10 s of silence.
pub fn lsp_timeouts() -> Timeouts {
    Timeouts {
        connect: crate::client::LOOPBACK_CONNECT,
        request: Duration::from_secs(10),
        call_idle: Duration::from_secs(10),
        call_total: Some(Duration::from_secs(30)),
    }
}

type Waker = Arc<dyn Fn() + Send + Sync>;

/// `server` name used in the events this provider emits.
const SERVER: &str = "everyai";

fn link_err(e: LinkError) -> LspError {
    use crate::Error;
    match e {
        LinkError::Unavailable => LspError::Unsupported("EveryAI host unavailable"),
        LinkError::ToolMissing(_) => LspError::Unsupported("tool not offered by the EveryAI host"),
        LinkError::Host(e) => match e {
            Error::Io(_) | Error::Unauthorized => LspError::Disconnected,
            Error::Timeout | Error::Cancelled => LspError::Timeout,
            Error::Rpc { code, message } => LspError::Server { code, message },
            Error::Http { status, message } => LspError::Server {
                code: i64::from(status),
                message,
            },
            // "isError": the host has no server for this file / refused it;
            // the contract says Skink then uses its own LSP for the call.
            Error::Tool(m) => LspError::NoServer(m),
            other => LspError::Protocol(other.to_string()),
        },
    }
}

fn key(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

struct Open {
    doc: Doc,
    /// The host can read this exact content from disk (just saved): `text`
    /// is then left out of requests.
    synced: bool,
}

struct Inner {
    link: Arc<HostLink>,
    docs: Mutex<HashMap<PathBuf, Open>>,
    diags: Mutex<HashMap<PathBuf, Vec<Diagnostic>>>,
    events: Mutex<Vec<LspEvent>>,
    /// Paths with a background refresh running; the value asks for a rerun.
    refreshing: Mutex<HashMap<PathBuf, bool>>,
    waker: Option<Waker>,
    auto_diagnostics: bool,
    server: Arc<str>,
}

/// Column conversion against open buffers, falling back to the file on disk
/// (read at most once per call) and finally to the identity.
struct Cols<'a> {
    inner: &'a Inner,
    disk: HashMap<PathBuf, Option<Doc>>,
}

impl<'a> Cols<'a> {
    fn new(inner: &'a Inner) -> Self {
        Cols {
            inner,
            disk: HashMap::new(),
        }
    }

    fn with_doc<T>(&mut self, path: &Path, f: impl FnOnce(&Doc) -> T) -> Option<T> {
        let k = key(path);
        if let Some(o) = lock(&self.inner.docs).get(&k) {
            return Some(f(&o.doc));
        }
        self.disk
            .entry(k)
            .or_insert_with_key(|k| Doc::from_disk(k))
            .as_ref()
            .map(f)
    }

    fn utf16_col(&mut self, path: &Path, p: Position) -> u32 {
        self.with_doc(path, |d| d.utf16_col(p.line, p.col))
            .unwrap_or(p.col as u32)
    }

    fn char_col(&mut self, path: &Path, line: u32, col16: u32) -> usize {
        if col16 == 0 {
            return 0;
        }
        self.with_doc(path, |d| d.char_col(line, col16))
            .unwrap_or(col16 as usize)
    }

    fn position(&mut self, path: &Path, p: tools::Position) -> Position {
        Position::new(p.line as usize, self.char_col(path, p.line, p.character))
    }

    fn range(&mut self, path: &Path, r: tools::Range) -> Range {
        Range::new(self.position(path, r.start), self.position(path, r.end))
    }

    fn edit(&mut self, path: &Path, e: tools::TextEdit) -> TextEdit {
        TextEdit {
            range: self.range(path, e.range),
            new_text: e.new_text,
        }
    }

    fn location(&mut self, l: tools::Location) -> Location {
        let path = PathBuf::from(l.path);
        let range = self.range(&path, l.range);
        Location { path, range }
    }
}

fn severity(s: tools::Severity) -> Severity {
    match s {
        tools::Severity::Error => Severity::Error,
        tools::Severity::Warning => Severity::Warning,
        tools::Severity::Info => Severity::Info,
        tools::Severity::Hint => Severity::Hint,
    }
}

impl Inner {
    fn call<A: Serialize, R: DeserializeOwned>(&self, tool: &'static str, args: &A) -> Result<R> {
        self.link.call(tool, args).map_err(link_err)
    }

    fn wake(&self) {
        if let Some(w) = &self.waker {
            w();
        }
    }

    /// The buffer as the contract wants it.
    fn doc_arg(&self, k: &Path) -> tools::Doc {
        let path = k.to_string_lossy().into_owned();
        match lock(&self.docs).get(k) {
            Some(o) => tools::Doc {
                path,
                text: (!o.synced).then(|| o.doc.text().to_owned()),
                version: Some(o.doc.version),
            },
            None => tools::Doc {
                path,
                text: None,
                version: None,
            },
        }
    }

    fn pos_args(&self, path: &Path, pos: Position, cols: &mut Cols<'_>) -> PositionArgs {
        let k = key(path);
        let character = cols.utf16_col(&k, pos);
        PositionArgs {
            doc: self.doc_arg(&k),
            line: pos.line as u32,
            character,
        }
    }

    fn open_version(&self, k: &Path) -> Option<i64> {
        lock(&self.docs).get(k).map(|o| o.doc.version)
    }

    // ---------------------------------------------------------------- sync

    fn set_text(&self, path: &Path, text: &str, bump: bool) {
        let k = key(path);
        let mut docs = lock(&self.docs);
        match docs.get_mut(&k) {
            Some(o) => {
                o.doc.set_text(text.to_owned());
                if bump {
                    o.doc.version += 1;
                }
                o.synced = false;
            }
            None => {
                docs.insert(
                    k,
                    Open {
                        doc: Doc::new(1, text.to_owned()),
                        synced: false,
                    },
                );
            }
        }
    }

    // --------------------------------------------------------- diagnostics

    fn refresh(&self, path: &Path) -> Result<Vec<Diagnostic>> {
        let k = key(path);
        let before = self.open_version(&k);
        let args = tools::DiagnosticsArgs {
            doc: self.doc_arg(&k),
        };
        let raw: tools::DiagnosticsResult = self.call(tools::LSP_DIAGNOSTICS, &args)?;
        if self.open_version(&k) != before {
            // Edited while the host was answering: its ranges no longer fit.
            return Err(LspError::Server {
                code: -32801,
                message: "document changed during request".into(),
            });
        }
        let mut cols = Cols::new(self);
        let diags: Vec<Diagnostic> = raw
            .into_iter()
            .map(|d| Diagnostic {
                range: cols.range(&k, d.range),
                severity: severity(d.severity),
                message: d.message,
                source: d.source,
                code: d.code,
            })
            .collect();
        let changed = {
            let mut cache = lock(&self.diags);
            let prev = cache.get(&k).map(Vec::as_slice).unwrap_or_default();
            let changed = prev != diags.as_slice();
            if changed {
                cache.insert(k.clone(), diags.clone());
            }
            changed
        };
        if changed {
            lock(&self.events).push(LspEvent::Diagnostics {
                server: Arc::clone(&self.server),
                path: k,
                version: None,
                diagnostics: diags.clone(),
            });
            self.wake();
        }
        Ok(diags)
    }

    fn spawn_refresh(self: &Arc<Self>, k: PathBuf) {
        {
            let mut running = lock(&self.refreshing);
            if let Some(rerun) = running.get_mut(&k) {
                *rerun = true;
                return;
            }
            running.insert(k.clone(), false);
        }
        let me = Arc::clone(self);
        let kk = k.clone();
        let spawned = thread::Builder::new()
            .name("host-diagnostics".into())
            .spawn(move || {
                loop {
                    let _ = me.refresh(&kk);
                    let mut running = lock(&me.refreshing);
                    match running.get_mut(&kk) {
                        Some(rerun) if *rerun => *rerun = false,
                        _ => {
                            running.remove(&kk);
                            break;
                        }
                    }
                }
            });
        if spawned.is_err() {
            lock(&self.refreshing).remove(&k);
        }
    }
}

/// The delegated [`LspProvider`]. Cheap to clone-share behind an `Arc`
/// (`Send + Sync`, object safe). See the module docs for how sync and
/// diagnostics work.
pub struct DelegatedLsp {
    inner: Arc<Inner>,
}

impl DelegatedLsp {
    /// Use `link` (give it a client with [`lsp_timeouts`]-style deadlines;
    /// `HostLink::new(client.fork_with_timeouts(lsp_timeouts()))`).
    pub fn new(link: Arc<HostLink>) -> Self {
        DelegatedLsp {
            inner: Arc::new(Inner {
                link,
                docs: Mutex::new(HashMap::new()),
                diags: Mutex::new(HashMap::new()),
                events: Mutex::new(Vec::new()),
                refreshing: Mutex::new(HashMap::new()),
                waker: None,
                auto_diagnostics: true,
                server: Arc::from(SERVER),
            }),
        }
    }

    fn configure(&mut self, f: impl FnOnce(&mut Inner)) {
        if let Some(i) = Arc::get_mut(&mut self.inner) {
            f(i);
        }
    }

    /// Called from a background thread whenever an event is queued.
    #[must_use]
    pub fn with_waker(mut self, waker: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.configure(|i| i.waker = Some(waker));
        self
    }

    /// Refresh diagnostics in the background after `did_open` and `did_save`
    /// (default on). Off: call [`refresh_diagnostics_async`](Self::refresh_diagnostics_async) yourself.
    #[must_use]
    pub fn with_auto_diagnostics(mut self, on: bool) -> Self {
        self.configure(|i| i.auto_diagnostics = on);
        self
    }

    pub fn link(&self) -> &Arc<HostLink> {
        &self.inner.link
    }

    /// Ask the host for the current diagnostics of `path` (blocking, with
    /// the client's deadlines), update the cache and emit a
    /// [`LspEvent::Diagnostics`] if they changed. If the buffer was edited
    /// meanwhile the answer is discarded with a transient (`-32801`) error.
    pub fn refresh_diagnostics(&self, path: &Path) -> Result<Vec<Diagnostic>> {
        self.inner.refresh(path)
    }

    /// [`refresh_diagnostics`](Self::refresh_diagnostics) on a worker thread.
    /// Requests for a path that is already refreshing are coalesced into one
    /// rerun. Failures are dropped (the link tracks host health).
    pub fn refresh_diagnostics_async(&self, path: &Path) {
        self.inner.spawn_refresh(key(path));
    }

    fn auto_refresh(&self, k: PathBuf) {
        if self.inner.auto_diagnostics && self.inner.link.is_available() {
            self.inner.spawn_refresh(k);
        }
    }
}

impl LspProvider for DelegatedLsp {
    fn backend(&self) -> Backend {
        Backend::Delegated
    }

    fn status(&self, _path: &Path) -> ServerStatus {
        let l = &self.inner.link;
        if !l.is_available() {
            return ServerStatus::Failed(
                l.last_error()
                    .unwrap_or_else(|| "EveryAI host unavailable".into()),
            );
        }
        if l.is_tool_missing(tools::LSP_HOVER) {
            return ServerStatus::Unsupported;
        }
        ServerStatus::Ready("EveryAI".into())
    }

    fn did_open(&self, path: &Path, text: &str) -> Result<()> {
        let k = key(path);
        {
            let mut docs = lock(&self.inner.docs);
            let version = docs.get(&k).map_or(1, |o| o.doc.version + 1);
            docs.insert(
                k.clone(),
                Open {
                    doc: Doc::new(version, text.to_owned()),
                    synced: false,
                },
            );
        }
        self.auto_refresh(k);
        Ok(())
    }

    fn did_change(&self, path: &Path, text: &str) -> Result<()> {
        self.inner.set_text(path, text, true);
        Ok(())
    }

    fn did_change_incremental(&self, path: &Path, edits: &[TextEdit]) -> Result<()> {
        let k = key(path);
        let mut docs = lock(&self.inner.docs);
        let o = docs.get_mut(&k).ok_or(LspError::NotOpen(k))?;
        if edits.is_empty() {
            return Ok(());
        }
        for e in edits {
            o.doc.replace(e.range, &e.new_text);
        }
        o.doc.version += 1;
        o.synced = false;
        Ok(())
    }

    fn did_save(&self, path: &Path) -> Result<()> {
        let k = key(path);
        if let Some(o) = lock(&self.inner.docs).get_mut(&k) {
            o.synced = true;
        }
        self.auto_refresh(k);
        Ok(())
    }

    fn did_close(&self, path: &Path) -> Result<()> {
        let k = key(path);
        lock(&self.inner.docs).remove(&k);
        lock(&self.inner.diags).remove(&k);
        Ok(())
    }

    fn hover(&self, path: &Path, pos: Position) -> Result<Option<Hover>> {
        let i = &*self.inner;
        let mut cols = Cols::new(i);
        let args = i.pos_args(path, pos, &mut cols);
        let r: Option<tools::HoverResult> = i.call(tools::LSP_HOVER, &args)?;
        Ok(r.map(|h| Hover {
            text: h.contents,
            range: h.range.map(|r| cols.range(&key(path), r)),
        }))
    }

    fn definition(&self, path: &Path, pos: Position) -> Result<Vec<Location>> {
        let i = &*self.inner;
        let mut cols = Cols::new(i);
        let args = i.pos_args(path, pos, &mut cols);
        let r: tools::DefinitionResult = i.call(tools::LSP_DEFINITION, &args)?;
        Ok(r.into_iter().map(|l| cols.location(l)).collect())
    }

    fn references(
        &self,
        path: &Path,
        pos: Position,
        include_declaration: bool,
    ) -> Result<Vec<Location>> {
        let i = &*self.inner;
        let mut cols = Cols::new(i);
        let args = tools::ReferencesArgs {
            pos: i.pos_args(path, pos, &mut cols),
            include_declaration,
        };
        let r: tools::ReferencesResult = i.call(tools::LSP_REFERENCES, &args)?;
        Ok(r.into_iter().map(|l| cols.location(l)).collect())
    }

    fn completion(&self, path: &Path, pos: Position) -> Result<Vec<CompletionItem>> {
        let i = &*self.inner;
        let mut cols = Cols::new(i);
        let args = tools::CompletionArgs {
            pos: i.pos_args(path, pos, &mut cols),
            trigger_character: None,
        };
        let r: tools::CompletionResult = i.call(tools::LSP_COMPLETION, &args)?;
        let k = key(path);
        Ok(r.items
            .into_iter()
            .map(|c| {
                let edit = c.text_edit.map(|e| cols.edit(&k, e));
                // `text_edit` wins over `insert_text`, which wins over the label.
                let insert_text = match (&edit, c.insert_text) {
                    (Some(e), _) => e.new_text.clone(),
                    (None, Some(t)) => t,
                    (None, None) => c.label.clone(),
                };
                CompletionItem {
                    label: c.label,
                    insert_text,
                    // The host contract carries plain text only.
                    snippet: false,
                    documentation: None,
                    kind: c.kind.map_or(ItemKind::Other, ItemKind::from_completion),
                    detail: c.detail,
                    sort_text: c.sort_text,
                    edit,
                    additional_edits: Vec::new(),
                    resolve: skink_lsp::Resolve::Complete,
                }
            })
            .collect())
    }

    fn rename(&self, path: &Path, pos: Position, new_name: &str) -> Result<Option<WorkspaceEdit>> {
        let i = &*self.inner;
        let mut cols = Cols::new(i);
        let args = tools::RenameArgs {
            pos: i.pos_args(path, pos, &mut cols),
            new_name: new_name.to_owned(),
        };
        let r: Option<tools::WorkspaceEdit> = i.call(tools::LSP_RENAME, &args)?;
        let edit = WorkspaceEdit {
            files: r
                .map(|w| w.files)
                .unwrap_or_default()
                .into_iter()
                .map(|f| {
                    let p = PathBuf::from(f.path);
                    let edits = f.edits.into_iter().map(|e| cols.edit(&p, e)).collect();
                    FileEdit { path: p, edits }
                })
                .collect(),
            ops: Vec::new(),
        };
        Ok((!edit.is_empty()).then_some(edit))
    }

    fn formatting(&self, path: &Path, opts: FormattingOptions) -> Result<Vec<TextEdit>> {
        let i = &*self.inner;
        let k = key(path);
        let args = tools::FormatArgs {
            doc: i.doc_arg(&k),
            range: None,
            tab_size: Some(opts.tab_size),
            insert_spaces: Some(opts.insert_spaces),
        };
        let r: tools::FormatResult = i.call(tools::LSP_FORMAT, &args)?;
        let mut cols = Cols::new(i);
        Ok(r.into_iter().map(|e| cols.edit(&k, e)).collect())
    }

    fn signature_help(&self, _: &Path, _: Position) -> Result<Option<SignatureHelp>> {
        Err(LspError::Unsupported(
            "signature help (not in the host contract)",
        ))
    }

    fn document_symbols(&self, _: &Path) -> Result<Vec<DocumentSymbol>> {
        Err(LspError::Unsupported(
            "document symbols (not in the host contract)",
        ))
    }

    fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        lock(&self.inner.diags)
            .get(&key(path))
            .cloned()
            .unwrap_or_default()
    }

    fn poll_events(&self) -> Vec<LspEvent> {
        std::mem::take(&mut *lock(&self.inner.events))
    }
}
