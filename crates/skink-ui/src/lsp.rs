//! Glue between the editor and `skink-lsp`: a worker thread owns the slow
//! notifications (the first `did_open` blocks on the server handshake), while
//! diagnostics are read straight from the client's cache.

use skink_core::Backend;
use skink_lsp::{Diagnostic, LspProvider, Severity};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        mpsc::{self, Receiver, Sender},
    },
};

enum Cmd {
    Open(PathBuf, String),
    Change(PathBuf, String),
    Save(PathBuf),
    Close(PathBuf),
}

pub struct LspLink {
    /// Set once the worker has chosen local or EveryAI-delegated servers
    /// (probing the host can take a moment, so it happens off the UI thread).
    provider: Arc<OnceLock<(Backend, Arc<dyn LspProvider>)>>,
    tx: Sender<Cmd>,
}

impl LspLink {
    /// `wake` is called when a server publishes something new.
    pub fn start(root: PathBuf, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let provider = Arc::new(OnceLock::new());
        let (tx, rx) = mpsc::channel();
        let slot = Arc::clone(&provider);
        std::thread::Builder::new()
            .name("skink-lsp-sync".into())
            .spawn(move || {
                let opts = skink_host::ProviderOptions {
                    waker: Some(Arc::new(wake)),
                    ..Default::default()
                };
                let p = skink_host::providers_with(&root, |k| std::env::var(k).ok(), &opts);
                let _ = slot.set((p.backend, Arc::clone(&p.lsp)));
                sync_loop(p.lsp.as_ref(), &rx);
            })
            .ok();
        Self { provider, tx }
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

    /// Discard queued server events (we only read the diagnostics cache).
    pub fn drain(&self) {
        if let Some((_, l)) = self.provider.get() {
            let _ = l.poll_events();
        }
    }
}

fn sync_loop(lsp: &dyn LspProvider, rx: &Receiver<Cmd>) {
    while let Ok(first) = rx.recv() {
        // Coalesce a burst of changes to the latest text per file.
        let mut batch = vec![first];
        batch.extend(rx.try_iter());
        let mut last_change: Vec<(PathBuf, String)> = Vec::new();
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
            }
        }
        for (p, t) in last_change {
            let _ = lsp.did_change(&p, &t);
        }
    }
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
