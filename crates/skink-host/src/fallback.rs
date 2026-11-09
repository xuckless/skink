//! Delegated language features with a local fallback, per file.
//!
//! The handover contract says: when the EveryAI host has no server for a file
//! (a tool error, `isError`) or is unavailable, Skink uses its own LSP for
//! that call. [`FallbackLsp`] does exactly that and remembers the outcome:
//! once a file needed the local servers, it stays with them.

use skink_core::Backend;
use skink_lsp::{
    CodeAction, CompletionItem, Diagnostic, DocumentSymbol, FormattingOptions, GotoKind, Hover,
    InlayHint, Location, LspError, LspEvent, LspProvider, Position, Range, Result, ServerCommand,
    ServerStatus, SignatureHelp, SymbolInfo, TextEdit, WorkspaceEdit,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

/// Which provider serves a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// The host (EveryAI) serves it.
    Primary,
    /// The host could not; Skink's own servers do.
    Local,
}

struct File {
    /// Latest full text, so the file can be opened locally on fallback.
    text: String,
    route: Route,
}

pub struct FallbackLsp {
    primary: Arc<dyn LspProvider>,
    local: Arc<dyn LspProvider>,
    files: Mutex<HashMap<PathBuf, File>>,
}

/// Errors that mean "the host cannot serve this", not "the query failed".
fn falls_back(e: &LspError) -> bool {
    matches!(
        e,
        LspError::NoServer(_) | LspError::NotInstalled(_) | LspError::Unsupported(_)
    )
}

fn key(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

impl FallbackLsp {
    pub fn new(primary: Arc<dyn LspProvider>, local: Arc<dyn LspProvider>) -> Self {
        FallbackLsp {
            primary,
            local,
            files: Mutex::new(HashMap::new()),
        }
    }

    fn files(&self) -> MutexGuard<'_, HashMap<PathBuf, File>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn route(&self, path: &Path) -> Route {
        self.files()
            .get(&key(path))
            .map_or(Route::Primary, |f| f.route)
    }

    fn provider(&self, route: Route) -> &dyn LspProvider {
        match route {
            Route::Primary => self.primary.as_ref(),
            Route::Local => self.local.as_ref(),
        }
    }

    /// Move `path` to the local servers (opening it there with its text).
    fn switch_to_local(&self, path: &Path) -> Result<()> {
        let text = {
            let mut files = self.files();
            let f = files.entry(key(path)).or_insert(File {
                text: String::new(),
                route: Route::Primary,
            });
            if f.route == Route::Local {
                return Ok(());
            }
            f.route = Route::Local;
            f.text.clone()
        };
        let _ = self.primary.did_close(path);
        self.local.did_open(path, &text)
    }

    /// Run `f` on the file's provider; if the host cannot serve it, switch the
    /// file to the local servers and run it there.
    fn call<T>(&self, path: &Path, f: impl Fn(&dyn LspProvider) -> Result<T>) -> Result<T> {
        match self.route(path) {
            Route::Local => f(self.local.as_ref()),
            Route::Primary => match f(self.primary.as_ref()) {
                Err(e) if falls_back(&e) => {
                    self.switch_to_local(path)?;
                    f(self.local.as_ref())
                }
                r => r,
            },
        }
    }
}

impl LspProvider for FallbackLsp {
    fn backend(&self) -> Backend {
        self.primary.backend()
    }

    fn status(&self, path: &Path) -> ServerStatus {
        match self.route(path) {
            Route::Local => self.local.status(path),
            Route::Primary => match self.primary.status(path) {
                ServerStatus::Ready(s) => ServerStatus::Ready(s),
                _ => self.local.status(path),
            },
        }
    }

    fn did_open(&self, path: &Path, text: &str) -> Result<()> {
        let route = {
            let mut files = self.files();
            let f = files.entry(key(path)).or_insert(File {
                text: String::new(),
                route: Route::Primary,
            });
            f.text = text.to_owned();
            f.route
        };
        match route {
            Route::Local => self.local.did_open(path, text),
            Route::Primary => match self.primary.did_open(path, text) {
                Err(e) if falls_back(&e) => self.switch_to_local(path),
                r => r,
            },
        }
    }

    fn did_change(&self, path: &Path, text: &str) -> Result<()> {
        if let Some(f) = self.files().get_mut(&key(path)) {
            f.text = text.to_owned();
        }
        self.provider(self.route(path)).did_change(path, text)
    }

    fn did_save(&self, path: &Path) -> Result<()> {
        self.provider(self.route(path)).did_save(path)
    }

    fn did_close(&self, path: &Path) -> Result<()> {
        let route = self.route(path);
        self.files().remove(&key(path));
        self.provider(route).did_close(path)
    }

    fn hover(&self, path: &Path, pos: Position) -> Result<Option<Hover>> {
        self.call(path, |p| p.hover(path, pos))
    }

    fn definition(&self, path: &Path, pos: Position) -> Result<Vec<Location>> {
        self.call(path, |p| p.definition(path, pos))
    }

    fn references(&self, path: &Path, pos: Position, decl: bool) -> Result<Vec<Location>> {
        self.call(path, |p| p.references(path, pos, decl))
    }

    fn completion(&self, path: &Path, pos: Position) -> Result<Vec<CompletionItem>> {
        self.call(path, |p| p.completion(path, pos))
    }

    fn rename(&self, path: &Path, pos: Position, new_name: &str) -> Result<Option<WorkspaceEdit>> {
        self.call(path, |p| p.rename(path, pos, new_name))
    }

    fn formatting(&self, path: &Path, opts: FormattingOptions) -> Result<Vec<TextEdit>> {
        self.call(path, |p| p.formatting(path, opts))
    }

    fn signature_help(&self, path: &Path, pos: Position) -> Result<Option<SignatureHelp>> {
        self.call(path, |p| p.signature_help(path, pos))
    }

    fn document_symbols(&self, path: &Path) -> Result<Vec<DocumentSymbol>> {
        self.call(path, |p| p.document_symbols(path))
    }

    fn goto(&self, kind: GotoKind, path: &Path, pos: Position) -> Result<Vec<Location>> {
        self.call(path, |p| p.goto(kind, path, pos))
    }

    fn range_formatting(
        &self,
        path: &Path,
        range: Range,
        opts: FormattingOptions,
    ) -> Result<Vec<TextEdit>> {
        self.call(path, |p| p.range_formatting(path, range, opts))
    }

    fn code_actions(
        &self,
        path: &Path,
        range: Range,
        diags: &[Diagnostic],
    ) -> Result<Vec<CodeAction>> {
        self.call(path, |p| p.code_actions(path, range, diags))
    }

    fn execute_command(&self, path: &Path, cmd: &ServerCommand) -> Result<()> {
        // A command runs on the server that offered the action.
        self.provider(self.route(path)).execute_command(path, cmd)
    }

    fn workspace_symbols(&self, path: &Path, query: &str) -> Result<Vec<SymbolInfo>> {
        self.call(path, |p| p.workspace_symbols(path, query))
    }

    fn inlay_hints(&self, path: &Path, range: Range) -> Result<Vec<InlayHint>> {
        self.call(path, |p| p.inlay_hints(path, range))
    }

    fn restart(&self, path: &Path) {
        self.provider(self.route(path)).restart(path);
    }

    fn server_pids(&self) -> Vec<u32> {
        self.local.server_pids()
    }

    fn resolve_completion(&self, path: &Path, item: &CompletionItem) -> Result<Option<String>> {
        self.provider(self.route(path))
            .resolve_completion(path, item)
    }

    fn stop_idle(&self, after: std::time::Duration) -> Vec<String> {
        self.local.stop_idle(after)
    }

    fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        self.provider(self.route(path)).diagnostics(path)
    }

    fn poll_events(&self) -> Vec<LspEvent> {
        let mut ev = self.primary.poll_events();
        ev.extend(self.local.poll_events());
        ev
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A provider that answers hover with its name, or fails with `err`.
    struct P {
        name: &'static str,
        err: Option<fn() -> LspError>,
        opened: AtomicUsize,
    }

    impl LspProvider for P {
        fn backend(&self) -> Backend {
            Backend::Local
        }
        fn did_open(&self, _: &Path, _: &str) -> Result<()> {
            self.opened.fetch_add(1, Ordering::SeqCst);
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
            match self.err {
                Some(e) => Err(e()),
                None => Ok(Some(Hover {
                    text: self.name.into(),
                    range: None,
                })),
            }
        }
        fn definition(&self, _: &Path, _: Position) -> Result<Vec<Location>> {
            Ok(vec![])
        }
        fn references(&self, _: &Path, _: Position, _: bool) -> Result<Vec<Location>> {
            Ok(vec![])
        }
        fn completion(&self, _: &Path, _: Position) -> Result<Vec<CompletionItem>> {
            Ok(vec![])
        }
        fn rename(&self, _: &Path, _: Position, _: &str) -> Result<Option<WorkspaceEdit>> {
            Ok(None)
        }
        fn formatting(&self, _: &Path, _: FormattingOptions) -> Result<Vec<TextEdit>> {
            Ok(vec![])
        }
        fn signature_help(&self, _: &Path, _: Position) -> Result<Option<SignatureHelp>> {
            Ok(None)
        }
        fn document_symbols(&self, _: &Path) -> Result<Vec<DocumentSymbol>> {
            Ok(vec![])
        }
        fn diagnostics(&self, _: &Path) -> Vec<Diagnostic> {
            vec![]
        }
    }

    fn p(name: &'static str, err: Option<fn() -> LspError>) -> Arc<P> {
        Arc::new(P {
            name,
            err,
            opened: AtomicUsize::new(0),
        })
    }

    fn hover(f: &FallbackLsp) -> Result<String> {
        f.hover(Path::new("/x/a.rs"), Position::new(0, 0))
            .map(|h| h.unwrap().text)
    }

    #[test]
    fn host_without_a_server_falls_back_to_local_and_stays_there() {
        let host = p("host", Some(|| LspError::NoServer("rust".into())));
        let local = p("local", None);
        let f = FallbackLsp::new(host.clone(), local.clone());
        f.did_open(Path::new("/x/a.rs"), "fn a() {}").unwrap();
        assert_eq!(hover(&f).unwrap(), "local");
        assert_eq!(
            local.opened.load(Ordering::SeqCst),
            1,
            "opened locally once"
        );
        assert_eq!(hover(&f).unwrap(), "local");
        assert_eq!(local.opened.load(Ordering::SeqCst), 1, "route remembered");
    }

    #[test]
    fn real_query_errors_do_not_fall_back() {
        let host = p("host", Some(|| LspError::Timeout));
        let local = p("local", None);
        let f = FallbackLsp::new(host, local.clone());
        f.did_open(Path::new("/x/a.rs"), "").unwrap();
        assert!(matches!(hover(&f), Err(LspError::Timeout)));
        assert_eq!(local.opened.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_working_host_serves_the_file() {
        let f = FallbackLsp::new(p("host", None), p("local", None));
        f.did_open(Path::new("/x/a.rs"), "").unwrap();
        assert_eq!(hover(&f).unwrap(), "host");
    }
}
