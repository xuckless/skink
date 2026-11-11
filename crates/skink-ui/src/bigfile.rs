//! Opening big files without freezing: files from [`background_bytes`] up
//! load on a worker thread (the tab shows "Loading 120 MB..." meanwhile),
//! and files from [`huge_bytes`] up open in the large-file view, read
//! straight from disk ([`skink_core::Buffer::open_disk`]).

use crate::App;
use crate::disk::SizeClass;
use skink_core::{Buffer, Editor};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// How a file is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Read now (small files: no visible pause).
    Now,
    /// Read on a worker thread; the tab says it is loading.
    Background { bytes: u64 },
    /// The large-file view: read-only, from disk, any size.
    Disk { bytes: u64 },
}

fn env_bytes(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// Files this big load in the background (`SKINK_BG_LOAD_BYTES`).
pub fn background_bytes() -> u64 {
    env_bytes("SKINK_BG_LOAD_BYTES", 16 << 20)
}

/// Files this big open in the large-file view (`SKINK_HUGE_BYTES`).
pub fn huge_bytes() -> u64 {
    env_bytes("SKINK_HUGE_BYTES", 256 << 20)
}

pub fn plan(path: &Path) -> Plan {
    let Ok(meta) = std::fs::metadata(path) else {
        return Plan::Now;
    };
    if !meta.is_file() {
        return Plan::Now;
    }
    plan_for(meta.len(), background_bytes(), huge_bytes())
}

/// The plan for a file of `bytes` with these limits.
pub fn plan_for(bytes: u64, background: u64, huge: u64) -> Plan {
    if bytes >= huge {
        Plan::Disk { bytes }
    } else if bytes >= background {
        Plan::Background { bytes }
    } else {
        Plan::Now
    }
}

/// "1.2 GB", "340 MB", "12 KB".
pub fn human(bytes: u64) -> String {
    const GB: f64 = (1u64 << 30) as f64;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if bytes >= 1 << 20 {
        format!("{} MB", bytes >> 20)
    } else {
        format!("{} KB", (bytes >> 10).max(1))
    }
}

/// A document's text: there, or still being read by a worker.
#[derive(Default)]
pub enum Load {
    #[default]
    Ready,
    Loading {
        rx: mpsc::Receiver<io::Result<Buffer>>,
        bytes: u64,
        /// Where to put the cursor once loaded (`file:line:col`).
        goto: (Option<usize>, Option<usize>),
    },
}

/// Read `path` on a worker thread; `wake` nudges the event loop when done.
pub fn spawn_load(
    path: PathBuf,
    wake: Option<skink_term::WakeFn>,
) -> mpsc::Receiver<io::Result<Buffer>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(Buffer::open(&path));
        if let Some(w) = wake {
            w();
        }
    });
    rx
}

impl App {
    /// Load the live document's file (opened as a placeholder) on a worker.
    pub fn load_live_in_background(&mut self, goto: (Option<usize>, Option<usize>)) {
        let Some(path) = self.doc.editor.buffer.path().map(Path::to_path_buf) else {
            return;
        };
        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
        let rx = spawn_load(path, self.wake.clone());
        self.doc.load = Load::Loading { rx, bytes, goto };
        self.doc.editor.message = Some(format!("Loading {}...", human(bytes)));
    }

    /// Swap in the text of documents whose worker has finished.
    pub(crate) fn poll_loads(&mut self) {
        for i in 0..self.tabs.len() {
            let done = match self.doc_at_mut(i).map(|d| &d.load) {
                Some(Load::Loading { rx, .. }) => match rx.try_recv() {
                    Ok(r) => Some(r),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err(io::Error::other("the loader stopped")))
                    }
                },
                _ => None,
            };
            let Some(result) = done else { continue };
            let path = self
                .doc_at(i)
                .and_then(|d| d.editor.buffer.path().map(Path::to_path_buf));
            let settings = self.settings_for(path.as_deref()).clone();
            let syntax = settings.syntax;
            let Some(d) = self.doc_at_mut(i) else {
                continue;
            };
            let Load::Loading { bytes, goto, .. } = std::mem::take(&mut d.load) else {
                continue;
            };
            match result {
                Ok(buffer) => {
                    let mut ed = Editor::new(buffer);
                    crate::doc::apply_edit_settings(&mut ed, &settings);
                    ed.viewport_height = d.editor.viewport_height;
                    if goto.0.is_some() {
                        ed.goto(goto.0, goto.1);
                    }
                    d.size = SizeClass::of(ed.buffer.len_chars());
                    ed.message = Some(match d.size {
                        SizeClass::Normal => format!("Loaded {}", human(bytes)),
                        _ => format!(
                            "Loaded {}: large file, syntax colours and language server are off",
                            human(bytes)
                        ),
                    });
                    d.editor = ed;
                    d.reset_syntax(syntax);
                }
                Err(e) => {
                    d.editor.message = Some(format!("Could not load the file: {e}"));
                }
            }
        }
    }

    /// Wake up to show progress while a file loads or is indexed.
    pub(crate) fn load_timeout(&self) -> Option<Duration> {
        let busy = (0..self.tabs.len()).any(|i| {
            self.doc_at(i).is_some_and(|d| {
                matches!(d.load, Load::Loading { .. })
                    || d.editor.buffer.disk_text().is_some_and(|t| {
                        matches!(t.scan(), skink_core::disktext::Scan::Running { .. })
                    })
            })
        });
        busy.then_some(Duration::from_millis(250))
    }

    /// Status text for a loading or indexing document, if any.
    pub(crate) fn load_status(&self) -> Option<String> {
        if let Load::Loading { bytes, .. } = &self.doc.load {
            return Some(format!("Loading {}...", human(*bytes)));
        }
        let t = self.doc.editor.buffer.disk_text()?;
        Some(match t.scan() {
            skink_core::disktext::Scan::Running { bytes, total } => format!(
                "Indexing {}%",
                (bytes * 100).checked_div(total).unwrap_or(100)
            ),
            skink_core::disktext::Scan::Done => format!("{} from disk", human(t.total_bytes())),
            skink_core::disktext::Scan::Failed(e) => format!("Index failed: {e}"),
        })
    }
}
