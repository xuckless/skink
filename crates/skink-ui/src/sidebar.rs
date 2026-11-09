//! File-tree operations that involve the rest of the app: rename/move and
//! trash (open documents follow), copy path, live filter, sidebar width and
//! remembering open folders per project.

use crate::prompt::{Choice, PromptKind};
use crate::{App, SidebarView, doc, tree};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Sidebar width: automatic (a quarter of the window, 22..34 columns) until
/// the user resizes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarWidth {
    Auto,
    Fixed(u16),
}

/// Mouse drag on the sidebar's right border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderDrag {
    Idle,
    Resizing,
}

/// Narrowest and widest the sidebar can be made.
const MIN_W: u16 = 12;
const MAX_W: u16 = 80;
/// Below this window width the sidebar is not drawn.
pub const HIDE_BELOW: u16 = 50;

impl SidebarWidth {
    /// Columns for a body `body_w` wide.
    pub fn columns(self, body_w: u16) -> u16 {
        match self {
            SidebarWidth::Auto => (body_w / 4).clamp(22, 34),
            SidebarWidth::Fixed(w) => {
                w.clamp(MIN_W, MAX_W.min(body_w.saturating_sub(20).max(MIN_W)))
            }
        }
    }
}

/// Where a project's open folders are remembered.
fn state_file(state_dir: &Path, root: &Path) -> PathBuf {
    let hash = Sha256::digest(root.to_string_lossy().as_bytes());
    let name: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    state_dir.join("tree").join(format!("{name}.txt"))
}

impl App {
    /// Every project root: the tree's root first, then added ones.
    pub(crate) fn roots(&self) -> Vec<PathBuf> {
        match self.tree() {
            Some(t) => std::iter::once(t.root.clone())
                .chain(t.extra.iter().cloned())
                .collect(),
            None => vec![self.project_root()],
        }
    }

    /// `:addroot DIR`: another project root in the tree, quick open and
    /// project search.
    pub fn add_root(&mut self, dir: &str) {
        let path = crate::doc::canonical(Path::new(dir));
        if !path.is_dir() {
            self.doc.editor.message = Some(format!("{dir}: not a folder"));
            return;
        }
        if self.tree().is_none() {
            let root = self.project_root();
            self.set_tree_root(root, true);
        }
        let added = self.tree_mut().is_some_and(|t| t.add_root(path.clone()));
        self.doc.editor.message = Some(if added {
            format!("Added root {}", crate::view::home_relative(&path))
        } else {
            "Already a root".into()
        });
    }

    /// `:removeroot DIR` (no argument: the root of the selected tree entry).
    pub fn remove_root(&mut self, dir: &str) {
        let path = if dir.is_empty() {
            let sel = self
                .tree()
                .and_then(|t| t.selected_node().map(|n| n.path.clone()));
            let roots = self.roots();
            match sel.and_then(|p| roots.into_iter().skip(1).find(|r| p.starts_with(r))) {
                Some(r) => r,
                None => {
                    self.doc.editor.message = Some("Select an added root in the tree".into());
                    return;
                }
            }
        } else {
            crate::doc::canonical(Path::new(dir))
        };
        let removed = self.tree_mut().is_some_and(|t| t.remove_root(&path));
        if !removed {
            self.doc.editor.message = Some("Not an added root (the main root stays)".into());
        }
    }

    /// Remembered open folders for the tree's root.
    pub(crate) fn restore_tree_state(&mut self) {
        let (Some(state), Some(s)) = (&self.state_dir, &mut self.sidebar) else {
            return;
        };
        if let Ok(text) = std::fs::read_to_string(state_file(state, &s.tree.root)) {
            s.tree.restore_expanded(text.lines().map(PathBuf::from));
        }
    }

    pub(crate) fn save_tree_state(&self) {
        let (Some(state), Some(s)) = (&self.state_dir, &self.sidebar) else {
            return;
        };
        let p = state_file(state, &s.tree.root);
        let body: Vec<String> = s
            .tree
            .expanded_paths()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if std::fs::create_dir_all(p.parent().expect("has parent")).is_ok() {
            let _ = std::fs::write(p, body.join("\n"));
        }
    }

    /// Select the live document's file in the tree (when it is on screen
    /// and not filtered).
    pub(crate) fn follow_active_file(&mut self) {
        let Some(path) = self.doc.canon.clone() else {
            return;
        };
        if let Some(s) = &mut self.sidebar
            && s.view != SidebarView::Hidden
            && s.tree.mode == tree::TreeMode::Browse
            && (path.starts_with(&s.tree.root) || s.tree.extra.iter().any(|r| path.starts_with(r)))
            && s.tree.selected_node().map(|n| &n.path) != Some(&path)
        {
            s.tree.reveal(&path);
        }
    }

    // ---- rename / move ---------------------------------------------------

    /// `r` in the tree: ask for the new path (relative to the root).
    pub(crate) fn prompt_rename(&mut self) {
        let Some(t) = self.tree() else { return };
        let Some(n) = t.selected_node() else { return };
        let rel = n
            .path
            .strip_prefix(&t.root)
            .unwrap_or(&n.path)
            .to_string_lossy()
            .into_owned();
        let from = n.path.clone();
        self.open_prompt(PromptKind::Rename { from }, "Rename / move to: ", &rel);
    }

    pub(crate) fn finish_rename(&mut self, from: &Path, to_rel: &str) {
        let Some(root) = self.tree().map(|t| t.root.clone()) else {
            return;
        };
        match tree::rename(from, &root, to_rel) {
            Ok(to) => {
                let to = doc::canonical(&to);
                self.paths_moved(from, &to);
                if let Some(t) = self.tree_mut() {
                    t.refresh_and_reveal(&to);
                }
                self.doc.editor.message = Some(format!("moved to {to_rel}"));
            }
            Err(e) => self.doc.editor.message = Some(format!("rename: {e}")),
        }
    }

    /// Open documents at or under `from` now live under `to`.
    fn paths_moved(&mut self, from: &Path, to: &Path) {
        let syntax = self.settings.syntax;
        for i in 0..self.tabs.len() {
            let lsp = self.lsp.as_ref();
            let Some(d) = (match self.tabs.get_mut(i) {
                Some(crate::Tab::Doc(None)) => Some(&mut self.doc),
                Some(crate::Tab::Doc(Some(d))) => Some(&mut **d),
                _ => None,
            }) else {
                continue;
            };
            let Some(rest) = d.canon.as_ref().and_then(|c| c.strip_prefix(from).ok()) else {
                continue;
            };
            let new = if rest.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rest)
            };
            if let (Some(link), Some(old)) = (lsp, &d.canon)
                && d.lsp.is_open()
            {
                link.close(old);
                d.lsp = doc::LspSync::Closed;
            }
            d.editor.buffer.set_path(new);
            d.editor.buffer.acknowledge_disk();
            d.path_changed(syntax);
        }
        self.lsp_attach_live();
    }

    // ---- trash -------------------------------------------------------------

    /// `d` / Delete in the tree: confirm, then move to the OS trash.
    pub(crate) fn prompt_trash(&mut self) {
        let Some(path) = self
            .tree()
            .and_then(|t| t.selected_node())
            .map(|n| n.path.clone())
        else {
            return;
        };
        self.open_choice(Choice::Trash { path });
    }

    pub(crate) fn finish_trash(&mut self, path: &Path) {
        match crate::trash::trash(path) {
            Ok(()) => {
                if let Some(t) = self.tree_mut() {
                    t.rebuild();
                }
                self.doc.editor.message = Some(format!(
                    "moved {} to the trash",
                    path.file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                ));
            }
            Err(e) => self.doc.editor.message = Some(format!("trash: {e}")),
        }
    }

    // ---- copy path, filter, width -------------------------------------------

    /// `y` absolute path, `Y` path relative to the tree root.
    pub(crate) fn copy_tree_path(&mut self, relative: bool) {
        let Some(t) = self.tree() else { return };
        let Some(n) = t.selected_node() else { return };
        let p = if relative {
            n.path
                .strip_prefix(&t.root)
                .unwrap_or(&n.path)
                .to_path_buf()
        } else {
            n.path.clone()
        };
        let text = p.to_string_lossy().into_owned();
        self.clip.set(&text, self.osc52);
        self.doc.editor.message = Some(format!("copied {text}"));
    }

    /// `/` in the tree: filter as you type.
    pub(crate) fn prompt_tree_filter(&mut self) {
        let q = match self.tree().map(|t| &t.mode) {
            Some(tree::TreeMode::Filter { query }) => query.clone(),
            _ => String::new(),
        };
        self.open_prompt(PromptKind::TreeFilter, "Filter files: ", &q);
    }

    pub(crate) fn set_tree_filter(&mut self, query: Option<&str>) {
        if let Some(t) = self.tree_mut() {
            t.set_filter(query);
        }
    }

    /// `<` / `>` in the tree, TreeNarrower / TreeWider actions.
    pub(crate) fn resize_sidebar(&mut self, delta: i16) {
        let body = self.areas.main.width + self.areas.tree.width;
        if let Some(s) = &mut self.sidebar {
            let now = s.width.columns(body);
            s.width = SidebarWidth::Fixed(now.saturating_add_signed(delta).clamp(MIN_W, MAX_W));
        }
    }

    pub(crate) fn set_sidebar_columns(&mut self, cols: u16) {
        if let Some(s) = &mut self.sidebar {
            s.width = SidebarWidth::Fixed(cols.clamp(MIN_W, MAX_W));
        }
    }
}
