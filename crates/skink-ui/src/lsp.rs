//! Glue between the editor and `skink-lsp`: a worker thread owns the slow
//! notifications (the first `did_open` blocks on the server handshake), while
//! diagnostics are read straight from the client's cache.

use skink_lsp::{Diagnostic, LocalLsp, LspProvider, Severity};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
};

enum Cmd {
    Open(PathBuf, String),
    Change(PathBuf, String),
    Save(PathBuf),
}

pub struct LspLink {
    lsp: Arc<LocalLsp>,
    tx: Sender<Cmd>,
}

impl LspLink {
    /// `wake` is called when a server publishes something new.
    pub fn start(root: PathBuf, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let mut registry = skink_lsp::ServerRegistry::new(root);
        registry.set_waker(wake);
        let lsp = Arc::new(LocalLsp::with_registry(registry));
        let (tx, rx) = mpsc::channel();
        let worker = Arc::clone(&lsp);
        std::thread::Builder::new()
            .name("skink-lsp-sync".into())
            .spawn(move || sync_loop(&worker, &rx))
            .ok();
        Self { lsp, tx }
    }

    pub fn open(&self, path: &Path, text: String) {
        let _ = self.tx.send(Cmd::Open(path.to_path_buf(), text));
    }

    pub fn change(&self, path: &Path, text: String) {
        let _ = self.tx.send(Cmd::Change(path.to_path_buf(), text));
    }

    pub fn save(&self, path: &Path) {
        let _ = self.tx.send(Cmd::Save(path.to_path_buf()));
    }

    /// Cached diagnostics; never blocks on the server.
    pub fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        self.lsp.diagnostics(path)
    }

    /// Discard queued server events (we only read the diagnostics cache).
    pub fn drain(&self) {
        let _ = self.lsp.poll_events();
    }
}

fn sync_loop(lsp: &LocalLsp, rx: &Receiver<Cmd>) {
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
