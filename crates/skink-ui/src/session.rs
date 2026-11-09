//! Per-project session: open tabs with their cursors, split panes, the file
//! tree and terminal folders, kept in the state dir. `skink` with no
//! arguments inside a project restores it; quitting saves it.

use crate::panes::{Panes, Shape};
use crate::{App, Purpose, SidebarView, Tab};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use skink_term::CommandSpec;
use std::path::{Path, PathBuf};

/// Whether this run saves a session on exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// Opened on a file or outside a project: nothing is saved.
    Off,
    /// Started as `skink` inside `root`: saved there on exit.
    Project { root: PathBuf },
}

const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub version: u32,
    pub tabs: Vec<SavedTab>,
    pub active: usize,
    /// Indices into `tabs`.
    pub panes: Shape,
    pub tree: SavedTree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SavedTab {
    /// 0-based cursor and first visible line.
    File {
        path: PathBuf,
        line: usize,
        col: usize,
        scroll: usize,
    },
    /// An interactive shell; reopened in the folder it was in.
    Shell { cwd: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SavedTree {
    Hidden,
    Shown {
        root: PathBuf,
        /// Roots added with `:addroot`.
        #[serde(default)]
        extra: Vec<PathBuf>,
    },
}

fn session_file(state_dir: &Path, root: &Path) -> PathBuf {
    let hash = Sha256::digest(root.to_string_lossy().as_bytes());
    let name: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    state_dir.join("session").join(format!("{name}.json"))
}

/// The project `start` is inside (a `.git` or a manifest above it), if any.
pub fn project_of(start: &Path) -> Option<PathBuf> {
    let root = crate::find_project_root(start);
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "build.zig",
    ];
    MARKERS
        .iter()
        .any(|m| root.join(m).exists())
        .then_some(root)
}

/// Where a shell is now: its working folder on Linux, else where it started.
fn shell_cwd(pid: Option<u32>, started: &Path) -> PathBuf {
    #[cfg(target_os = "linux")]
    if let Some(cwd) = pid.and_then(|p| std::fs::read_link(format!("/proc/{p}/cwd")).ok()) {
        return cwd;
    }
    let _ = pid;
    started.to_path_buf()
}

/// Rewrite pane tab indices through `map` (saved index -> new index).
fn remap(s: &Shape, map: &[Option<usize>], fallback: usize) -> Shape {
    match s {
        Shape::Pane { tab, focused } => Shape::Pane {
            tab: map.get(*tab).copied().flatten().unwrap_or(fallback),
            focused: *focused,
        },
        Shape::Split {
            axis,
            share,
            first,
            second,
        } => Shape::Split {
            axis: *axis,
            share: *share,
            first: Box::new(remap(first, map, fallback)),
            second: Box::new(remap(second, map, fallback)),
        },
    }
}

impl App {
    /// `skink` with no arguments: inside a project (and with the
    /// `restoresession` setting on), keep and restore its session.
    pub fn start_session_here(&mut self) {
        if !self.settings.restore_session {
            return;
        }
        if let Some(root) = std::env::current_dir().ok().and_then(|d| project_of(&d)) {
            self.restore_session(root);
        }
    }

    /// What to save: files with a path and plain shells; views, previews and
    /// agents are left out.
    pub(crate) fn session_snapshot(&self) -> Saved {
        let mut tabs = Vec::new();
        let mut map = vec![None; self.tabs.len()];
        for (i, slot) in map.iter_mut().enumerate() {
            let saved = match &self.tabs[i] {
                Tab::Term(p) => match (&p.purpose, &p.shell_cwd) {
                    (Purpose::Shell, Some(start)) => Some(SavedTab::Shell {
                        cwd: shell_cwd(p.term.process_id(), start),
                    }),
                    _ => None,
                },
                Tab::Doc(_) => self.doc_at(i).and_then(|d| {
                    let path = d.canon.clone()?;
                    let e = &d.editor;
                    Some(SavedTab::File {
                        path,
                        line: e.cursor.line,
                        col: e.cursor.col,
                        scroll: e.scroll,
                    })
                }),
            };
            if let Some(t) = saved {
                *slot = Some(tabs.len());
                tabs.push(t);
            }
        }
        let tree = match &self.sidebar {
            Some(s) if s.view != SidebarView::Hidden => SavedTree::Shown {
                root: s.tree.root.clone(),
                extra: s.tree.extra.clone(),
            },
            _ => SavedTree::Hidden,
        };
        Saved {
            version: VERSION,
            active: map.get(self.active).copied().flatten().unwrap_or(0),
            panes: remap(&self.panes.shape(), &map, 0),
            tabs,
            tree,
        }
    }

    /// Save the session if this run keeps one.
    pub(crate) fn save_session(&self) {
        let (SessionState::Project { root }, Some(dir)) = (&self.session, &self.state_dir) else {
            return;
        };
        let file = session_file(dir, root);
        let saved = self.session_snapshot();
        if saved.tabs.is_empty() {
            let _ = std::fs::remove_file(file);
            return;
        }
        if let (Ok(json), Some(parent)) = (serde_json::to_string_pretty(&saved), file.parent())
            && std::fs::create_dir_all(parent).is_ok()
        {
            let _ = std::fs::write(file, json);
        }
    }

    /// `skink` with no arguments in `root`: keep a session there and restore
    /// the last one. Returns whether anything was restored.
    pub fn restore_session(&mut self, root: PathBuf) -> bool {
        let saved = self
            .state_dir
            .as_ref()
            .and_then(|d| std::fs::read_to_string(session_file(d, &root)).ok())
            .and_then(|s| serde_json::from_str::<Saved>(&s).ok())
            .filter(|s| s.version == VERSION);
        self.session = SessionState::Project { root: root.clone() };
        let Some(saved) = saved else {
            return false;
        };
        self.apply_session(&saved);
        true
    }

    pub(crate) fn apply_session(&mut self, saved: &Saved) {
        let start_tabs = self.tabs.len();
        let mut map = Vec::with_capacity(saved.tabs.len());
        for t in &saved.tabs {
            let opened = match t {
                SavedTab::File {
                    path,
                    line,
                    col,
                    scroll,
                } if path.is_file() => {
                    // May reuse the empty start-up tab, so check by path.
                    self.open_path(path);
                    let ok = self.doc.canon.as_ref() == Some(&crate::doc::canonical(path));
                    if ok {
                        let e = &mut self.doc.editor;
                        e.goto(Some(line + 1), Some(col + 1));
                        e.scroll = (*scroll).min(e.cursor.line);
                    }
                    ok
                }
                SavedTab::File { .. } => false,
                SavedTab::Shell { cwd } => {
                    let dir = if cwd.is_dir() {
                        cwd.clone()
                    } else {
                        self.project_root()
                    };
                    let before = self.tabs.len();
                    self.open_terminal_or_report(CommandSpec::default_shell().cwd(dir));
                    self.tabs.len() > before
                }
            };
            map.push(opened.then_some(self.active));
        }
        let restored = map.iter().any(Option::is_some);
        // The empty start-up buffer goes once something was restored.
        let scratch_gone = restored
            && start_tabs == 1
            && !map.contains(&Some(0))
            && self
                .doc_at(0)
                .is_some_and(|d| d.canon.is_none() && !d.editor.buffer.is_dirty())
            && self.doc_count() > 1;
        if scratch_gone {
            self.close_tab(0);
            for m in map.iter_mut().flatten() {
                *m -= 1;
            }
        }
        if let SavedTree::Shown { root, extra } = &saved.tree
            && root.is_dir()
        {
            self.set_tree_root(root.clone(), true);
            if let Some(t) = self.tree_mut() {
                for r in extra.iter().filter(|r| r.is_dir()) {
                    t.add_root(r.clone());
                }
            }
            self.restore_tree_state();
            self.set_sidebar_view(SidebarView::Shown);
        }
        if !restored {
            return;
        }
        let fallback = self.tabs.len() - 1;
        let shape = remap(&saved.panes, &map, fallback);
        let active = map.get(saved.active).copied().flatten().unwrap_or(fallback);
        if let Some(p) = Panes::from_shape(&shape, self.tabs.len()) {
            self.panes = p;
        }
        let focused_tab = self
            .panes
            .pane(self.panes.focused())
            .map_or(active, |p| p.tab);
        self.switch_to(focused_tab);
        let missing = map.iter().filter(|m| m.is_none()).count();
        match missing {
            0 => self.toast(crate::nav::Level::Info, "Session restored"),
            n => self.toast(
                crate::nav::Level::Warn,
                format!("Session restored; {n} file(s) no longer exist"),
            ),
        }
    }
}
