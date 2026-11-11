//! Thin LSP client for Skink.
//!
//! * [`LspProvider`] is the interface the editor calls (object safe, no
//!   `lsp-types` in sight, Skink char coordinates). [`LocalLsp`] implements it
//!   with real language servers; `NoLsp` is the do-nothing placeholder; a
//!   `Delegated` implementation (LSP-backed MCP tools served by EveryAI)
//!   lives elsewhere and fills the same plain structs from JSON.
//! * [`LspClient`] is one server connection (threads + `std::sync::mpsc`,
//!   no async runtime): framing, handshake, id -> response routing,
//!   server->client requests, diagnostics/log events, shutdown.
//! * [`ServerRegistry`] maps file extensions to servers (rust-analyzer,
//!   typescript-language-server, basedpyright/pyright, gopls, zls), spawns
//!   them lazily per (language, project root) if installed, and shuts them
//!   down on drop.
//! * [`uri`] and [`pos`] hold the `file://` URI and UTF-16 column helpers.
//!
//! Transport and lifecycle are adapted from Lapce (Apache-2.0); see NOTICES.md.

mod client;
mod doc;
pub mod pos;
mod registry;
pub mod transport;
mod types;
pub mod uri;
mod wire;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod testutil;

use std::path::{Path, PathBuf};

use skink_core::Backend;

pub use client::{EventSink, LspClient, Pending, ServerSpec};
pub use doc::Doc;
pub use registry::{
    Language, Override, ServerRegistry, ServerStatus, Source, default_languages, find_in_path,
    find_server, language_in, merge_overrides,
};
pub use types::*;

/// What the editor asks of a language-intelligence backend. All methods are
/// blocking (with the provider's own timeout) so call them from a worker
/// thread; `poll_events` is the non-blocking side. Coordinates are 0-based
/// lines and char columns. `Send + Sync` so one provider can be shared.
pub trait LspProvider: Send + Sync {
    fn backend(&self) -> Backend;

    /// Is there (or could there be) a server for `path`? Never starts one.
    fn status(&self, _path: &Path) -> ServerStatus {
        ServerStatus::Unsupported
    }

    // -- document sync (no-ops when no server applies to `path`) --
    /// May start the server for this file's language (blocks for the handshake).
    fn did_open(&self, path: &Path, text: &str) -> Result<()>;
    /// Full-text replacement.
    fn did_change(&self, path: &Path, text: &str) -> Result<()>;
    /// Ordered edits in char coordinates; falls back to full sync on servers
    /// that do not support incremental updates.
    fn did_change_incremental(&self, _path: &Path, _edits: &[TextEdit]) -> Result<()> {
        Err(LspError::Unsupported("incremental sync"))
    }
    fn did_save(&self, path: &Path) -> Result<()>;
    fn did_close(&self, path: &Path) -> Result<()>;

    // -- queries --
    fn hover(&self, path: &Path, pos: Position) -> Result<Option<Hover>>;
    fn definition(&self, path: &Path, pos: Position) -> Result<Vec<Location>>;
    fn references(
        &self,
        path: &Path,
        pos: Position,
        include_declaration: bool,
    ) -> Result<Vec<Location>>;
    fn completion(&self, path: &Path, pos: Position) -> Result<Vec<CompletionItem>>;
    fn rename(&self, path: &Path, pos: Position, new_name: &str) -> Result<Option<WorkspaceEdit>>;
    fn formatting(&self, path: &Path, opts: FormattingOptions) -> Result<Vec<TextEdit>>;
    fn signature_help(&self, path: &Path, pos: Position) -> Result<Option<SignatureHelp>>;
    fn document_symbols(&self, path: &Path) -> Result<Vec<DocumentSymbol>>;

    /// Go to declaration / type definition / implementation.
    fn goto(&self, kind: GotoKind, path: &Path, pos: Position) -> Result<Vec<Location>> {
        match kind {
            GotoKind::Definition => self.definition(path, pos),
            _ => Err(LspError::Unsupported("goto kind")),
        }
    }
    fn range_formatting(
        &self,
        _path: &Path,
        _range: Range,
        _opts: FormattingOptions,
    ) -> Result<Vec<TextEdit>> {
        Err(LspError::Unsupported("range formatting"))
    }
    fn code_actions(
        &self,
        _path: &Path,
        _range: Range,
        _diagnostics: &[Diagnostic],
    ) -> Result<Vec<CodeAction>> {
        Err(LspError::Unsupported("code actions"))
    }
    /// Run a code action's command on the server that offered it.
    fn execute_command(&self, _path: &Path, _cmd: &ServerCommand) -> Result<()> {
        Err(LspError::Unsupported("execute command"))
    }
    /// `query` across the project of `path`'s server.
    fn workspace_symbols(&self, _path: &Path, _query: &str) -> Result<Vec<SymbolInfo>> {
        Err(LspError::Unsupported("workspace symbols"))
    }
    fn inlay_hints(&self, _path: &Path, _range: Range) -> Result<Vec<InlayHint>> {
        Err(LspError::Unsupported("inlay hints"))
    }
    /// Foldable line spans from the server (`textDocument/foldingRange`).
    fn folding_ranges(&self, _path: &Path) -> Result<Vec<FoldingRange>> {
        Err(LspError::Unsupported("folding ranges"))
    }
    /// Status of each server for `path`, primary first, with its command
    /// (`:lsp` lists them). One entry per configured server.
    fn server_statuses(&self, path: &Path) -> Vec<ServerStatus> {
        vec![self.status(path)]
    }

    /// Before moving files or folders (`(from, to)`): edits that keep the
    /// project consistent (imports, module declarations), merged from every
    /// server that asked for `willRename`. `Ok(None)` when none has any.
    fn will_rename_files(&self, _renames: &[(PathBuf, PathBuf)]) -> Result<Option<WorkspaceEdit>> {
        Ok(None)
    }

    /// After the move: tell the servers that asked for `didRename`.
    fn did_rename_files(&self, _renames: &[(PathBuf, PathBuf)]) -> Result<()> {
        Ok(())
    }

    /// Shut down the server for `path`; the next `did_open` starts a new one.
    fn restart(&self, _path: &Path) {}

    /// Process ids of local servers (for memory reporting).
    fn server_pids(&self) -> Vec<u32> {
        Vec::new()
    }

    /// Documentation for a completion item that left it out (lazy resolve).
    fn resolve_completion(&self, _path: &Path, _item: &CompletionItem) -> Result<Option<String>> {
        Ok(None)
    }

    /// Stop servers with no open documents for `after`; returns their names.
    fn stop_idle(&self, _after: std::time::Duration) -> Vec<String> {
        Vec::new()
    }

    /// Latest diagnostics for `path` (cached; never blocks).
    fn diagnostics(&self, path: &Path) -> Vec<Diagnostic>;

    /// Drain pending asynchronous events (diagnostics pushes, logs, progress,
    /// `applyEdit` requests, server exits). Never blocks.
    fn poll_events(&self) -> Vec<LspEvent> {
        Vec::new()
    }
}

/// Placeholder provider: accepts notifications, answers every query with
/// `Unsupported`.
pub struct NoLsp(pub Backend);

const NONE: LspError = LspError::Unsupported("no language server");

impl LspProvider for NoLsp {
    fn backend(&self) -> Backend {
        self.0
    }
    fn did_open(&self, _: &Path, _: &str) -> Result<()> {
        Ok(())
    }
    fn did_change(&self, _: &Path, _: &str) -> Result<()> {
        Ok(())
    }
    fn did_save(&self, _: &Path) -> Result<()> {
        Ok(())
    }
    fn did_close(&self, _: &Path) -> Result<()> {
        Ok(())
    }
    fn hover(&self, _: &Path, _: Position) -> Result<Option<Hover>> {
        Err(NONE)
    }
    fn definition(&self, _: &Path, _: Position) -> Result<Vec<Location>> {
        Err(NONE)
    }
    fn references(&self, _: &Path, _: Position, _: bool) -> Result<Vec<Location>> {
        Err(NONE)
    }
    fn completion(&self, _: &Path, _: Position) -> Result<Vec<CompletionItem>> {
        Err(NONE)
    }
    fn rename(&self, _: &Path, _: Position, _: &str) -> Result<Option<WorkspaceEdit>> {
        Err(NONE)
    }
    fn formatting(&self, _: &Path, _: FormattingOptions) -> Result<Vec<TextEdit>> {
        Err(NONE)
    }
    fn signature_help(&self, _: &Path, _: Position) -> Result<Option<SignatureHelp>> {
        Err(NONE)
    }
    fn document_symbols(&self, _: &Path) -> Result<Vec<DocumentSymbol>> {
        Err(NONE)
    }
    fn diagnostics(&self, _: &Path) -> Vec<Diagnostic> {
        Vec::new()
    }
}

/// Real language servers, started lazily through a [`ServerRegistry`].
pub struct LocalLsp {
    registry: ServerRegistry,
}

impl LocalLsp {
    /// Built-in languages; `root` bounds project-root detection.
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            registry: ServerRegistry::new(root),
        }
    }

    pub fn with_registry(registry: ServerRegistry) -> Self {
        Self { registry }
    }

    pub fn registry(&self) -> &ServerRegistry {
        &self.registry
    }

    /// Run `f` on every running server for `path` (document sync). The
    /// first error is returned after all have been tried.
    fn on_running(&self, path: &Path, f: impl Fn(&LspClient) -> Result<()>) -> Result<()> {
        let mut first = Ok(());
        for c in self.registry.running_clients_for(path) {
            if let Err(e) = f(&c)
                && first.is_ok()
            {
                first = Err(e);
            }
        }
        first
    }

    /// The server a single-answer request goes to: the first (primary
    /// first) that advertises `capability`, else the primary, so a lone
    /// server keeps answering as before.
    fn pick(&self, path: &Path, capability: &str) -> Result<std::sync::Arc<LspClient>> {
        if self.registry.server_count(path) > 1 {
            let found = self
                .registry
                .clients_for(path)
                .into_iter()
                .flatten()
                .find(|c| c.supports(capability));
            if let Some(c) = found {
                return Ok(c);
            }
        }
        self.registry.client_for(path)
    }

    fn query<T>(
        &self,
        path: &Path,
        capability: &str,
        f: impl FnOnce(&LspClient) -> Result<Pending<T>>,
    ) -> Result<T> {
        let c = self.pick(path, capability)?;
        f(&c)?.wait(c.timeout())
    }

    /// Ask every server that advertises `capability` and merge the answers
    /// (completions, code actions). One server failing does not lose the
    /// others' answers; with none supporting it, the primary is asked.
    fn query_all<T>(
        &self,
        path: &Path,
        capability: &str,
        f: impl Fn(&LspClient) -> Result<Pending<Vec<T>>>,
    ) -> Result<Vec<T>> {
        if self.registry.server_count(path) <= 1 {
            return self.query(path, capability, f);
        }
        let clients: Vec<_> = self
            .registry
            .clients_for(path)
            .into_iter()
            .flatten()
            .filter(|c| c.supports(capability))
            .collect();
        if clients.is_empty() {
            return self.query(path, capability, f);
        }
        // Send all first, then wait: the servers work in parallel.
        let pending: Vec<_> = clients.iter().map(|c| (c, f(c))).collect();
        let mut out = Vec::new();
        let mut err = None;
        for (c, p) in pending {
            match p.and_then(|p| p.wait(c.timeout())) {
                Ok(v) => out.extend(v),
                Err(e) => err = Some(e),
            }
        }
        match err {
            Some(e) if out.is_empty() => Err(e),
            _ => Ok(out),
        }
    }
}

impl LspProvider for LocalLsp {
    fn backend(&self) -> Backend {
        Backend::Local
    }

    fn status(&self, path: &Path) -> ServerStatus {
        self.registry.status(path)
    }

    fn did_open(&self, path: &Path, text: &str) -> Result<()> {
        let Some(lang_id) = self.registry.language_id(path) else {
            return Ok(());
        };
        let mut first = Ok(());
        for c in self.registry.clients_for(path) {
            let r = match c {
                Ok(c) => c.did_open(path, lang_id, text),
                // Missing server binary is a normal situation, not an error to shout about.
                Err(LspError::NotInstalled(_)) => Ok(()),
                Err(e) => Err(e),
            };
            if let Err(e) = r
                && first.is_ok()
            {
                first = Err(e);
            }
        }
        first
    }

    fn did_change(&self, path: &Path, text: &str) -> Result<()> {
        self.on_running(path, |c| c.did_change(path, text))
    }

    fn did_change_incremental(&self, path: &Path, edits: &[TextEdit]) -> Result<()> {
        self.on_running(path, |c| c.did_change_incremental(path, edits))
    }

    fn did_save(&self, path: &Path) -> Result<()> {
        self.on_running(path, |c| c.did_save(path))
    }

    fn did_close(&self, path: &Path) -> Result<()> {
        self.on_running(path, |c| c.did_close(path))
    }

    fn hover(&self, path: &Path, pos: Position) -> Result<Option<Hover>> {
        self.query(path, "hoverProvider", |c| c.hover(path, pos))
    }

    fn definition(&self, path: &Path, pos: Position) -> Result<Vec<Location>> {
        self.query(path, "definitionProvider", |c| c.definition(path, pos))
    }

    fn references(
        &self,
        path: &Path,
        pos: Position,
        include_declaration: bool,
    ) -> Result<Vec<Location>> {
        self.query(path, "referencesProvider", |c| {
            c.references(path, pos, include_declaration)
        })
    }

    fn completion(&self, path: &Path, pos: Position) -> Result<Vec<CompletionItem>> {
        self.query_all(path, "completionProvider", |c| c.completion(path, pos))
    }

    fn rename(&self, path: &Path, pos: Position, new_name: &str) -> Result<Option<WorkspaceEdit>> {
        self.query(path, "renameProvider", |c| c.rename(path, pos, new_name))
    }

    fn formatting(&self, path: &Path, opts: FormattingOptions) -> Result<Vec<TextEdit>> {
        self.query(path, "documentFormattingProvider", |c| {
            c.formatting(path, opts)
        })
    }

    fn signature_help(&self, path: &Path, pos: Position) -> Result<Option<SignatureHelp>> {
        self.query(path, "signatureHelpProvider", |c| {
            c.signature_help(path, pos)
        })
    }

    fn document_symbols(&self, path: &Path) -> Result<Vec<DocumentSymbol>> {
        self.query(path, "documentSymbolProvider", |c| c.document_symbols(path))
    }

    fn goto(&self, kind: GotoKind, path: &Path, pos: Position) -> Result<Vec<Location>> {
        let cap = match kind {
            GotoKind::Definition => "definitionProvider",
            GotoKind::Declaration => "declarationProvider",
            GotoKind::TypeDefinition => "typeDefinitionProvider",
            GotoKind::Implementation => "implementationProvider",
        };
        self.query(path, cap, |c| match kind {
            GotoKind::Definition => c.definition(path, pos),
            GotoKind::Declaration => c.declaration(path, pos),
            GotoKind::TypeDefinition => c.type_definition(path, pos),
            GotoKind::Implementation => c.implementation(path, pos),
        })
    }

    fn range_formatting(
        &self,
        path: &Path,
        range: Range,
        opts: FormattingOptions,
    ) -> Result<Vec<TextEdit>> {
        self.query(path, "documentRangeFormattingProvider", |c| {
            c.range_formatting(path, range, opts)
        })
    }

    fn code_actions(
        &self,
        path: &Path,
        range: Range,
        diagnostics: &[Diagnostic],
    ) -> Result<Vec<CodeAction>> {
        self.query_all(path, "codeActionProvider", |c| {
            c.code_actions(path, range, diagnostics)
        })
    }

    fn execute_command(&self, path: &Path, cmd: &ServerCommand) -> Result<()> {
        // The server that registered this command (code actions from
        // several servers are mixed in one list).
        if self.registry.server_count(path) > 1 {
            let owner = self
                .registry
                .clients_for(path)
                .into_iter()
                .flatten()
                .find(|c| {
                    c.capabilities()
                        .pointer("/executeCommandProvider/commands")
                        .and_then(|v| v.as_array())
                        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(&cmd.command)))
                });
            if let Some(c) = owner {
                return c.execute_command(cmd)?.wait(c.timeout()).map(drop);
            }
        }
        self.query(path, "executeCommandProvider", |c| c.execute_command(cmd))
            .map(drop)
    }

    fn workspace_symbols(&self, path: &Path, query: &str) -> Result<Vec<SymbolInfo>> {
        self.query(path, "workspaceSymbolProvider", |c| {
            c.workspace_symbols(query)
        })
    }

    fn inlay_hints(&self, path: &Path, range: Range) -> Result<Vec<InlayHint>> {
        self.query(path, "inlayHintProvider", |c| c.inlay_hints(path, range))
    }

    fn folding_ranges(&self, path: &Path) -> Result<Vec<FoldingRange>> {
        let c = self.pick(path, "foldingRangeProvider")?;
        if !c.supports("foldingRangeProvider") {
            return Err(LspError::Unsupported("folding ranges"));
        }
        c.folding_ranges(path)?.wait(c.timeout())
    }

    fn server_statuses(&self, path: &Path) -> Vec<ServerStatus> {
        self.registry.statuses(path)
    }

    fn will_rename_files(&self, renames: &[(PathBuf, PathBuf)]) -> Result<Option<WorkspaceEdit>> {
        let mut out = WorkspaceEdit::default();
        for c in self.registry.running_clients() {
            let Ok(pending) = c.will_rename_files(renames) else {
                continue; // the server did not ask for it
            };
            if let Some(e) = pending.wait(c.timeout())? {
                out.files.extend(e.files);
                out.ops.extend(e.ops);
            }
        }
        Ok((!out.is_empty()).then_some(out))
    }

    fn did_rename_files(&self, renames: &[(PathBuf, PathBuf)]) -> Result<()> {
        for c in self.registry.running_clients() {
            c.did_rename_files(renames)?;
        }
        Ok(())
    }

    fn restart(&self, path: &Path) {
        self.registry.restart(path);
    }

    fn server_pids(&self) -> Vec<u32> {
        self.registry.server_pids()
    }

    fn stop_idle(&self, after: std::time::Duration) -> Vec<String> {
        self.registry.stop_idle(std::time::Instant::now(), after)
    }

    fn resolve_completion(&self, path: &Path, item: &CompletionItem) -> Result<Option<String>> {
        let Resolve::Available(raw) = &item.resolve else {
            return Ok(None);
        };
        // The item came from one of the servers; the one that resolves it
        // (others reject an item they did not make) gives the docs.
        let mut last = Ok(None);
        for c in self.registry.running_clients_for(path) {
            let Ok(p) = c.resolve_completion(raw) else {
                continue;
            };
            last = p.wait(c.timeout());
            if matches!(last, Ok(Some(_))) {
                break;
            }
        }
        last
    }

    /// Every server's diagnostics for `path`, merged.
    fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        self.registry
            .running_clients_for(path)
            .iter()
            .flat_map(|c| c.diagnostics(path))
            .collect()
    }

    fn poll_events(&self) -> Vec<LspEvent> {
        self.registry.drain_events()
    }
}

#[cfg(test)]
mod trait_tests {
    use super::*;

    #[test]
    fn trait_is_object_safe_and_nolsp_reports_backend() {
        let p: Box<dyn LspProvider> = Box::new(NoLsp(Backend::Delegated));
        assert_eq!(p.backend(), Backend::Delegated);
        assert!(p.did_open(Path::new("a.rs"), "").is_ok());
        assert!(matches!(
            p.hover(Path::new("a.rs"), Position::default()),
            Err(LspError::Unsupported(_))
        ));
        assert!(p.diagnostics(Path::new("a.rs")).is_empty());
        assert!(p.poll_events().is_empty());
        assert_eq!(p.status(Path::new("a.rs")), ServerStatus::Unsupported);
    }

    #[test]
    fn local_lsp_without_server_is_quiet() {
        let l = LocalLsp::new("/nonexistent-skink-root");
        assert_eq!(l.backend(), Backend::Local);
        // unsupported file type: notifications are no-ops, queries error
        assert!(l.did_open(Path::new("x.txt"), "hi").is_ok());
        assert!(l.did_change(Path::new("x.txt"), "ho").is_ok());
        assert!(matches!(
            l.hover(Path::new("x.txt"), Position::default()),
            Err(LspError::NoServer(_))
        ));
        assert!(l.diagnostics(Path::new("x.txt")).is_empty());
    }
}
