//! A lazy file tree: only expanded directories are read from disk.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

pub struct FileTree {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    pub selected: usize,
    pub scroll: usize,
    pub show_hidden: bool,
    /// Hidden entries at the root level that are currently not shown.
    pub hidden_at_root: usize,
    expanded: HashSet<PathBuf>,
}

/// Directory names never listed (still reachable via the command bar).
const SKIP: &[&str] = &[".git"];

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut t = Self {
            root,
            nodes: Vec::new(),
            selected: 0,
            scroll: 0,
            show_hidden: false,
            hidden_at_root: 0,
            expanded: HashSet::new(),
        };
        t.rebuild();
        t
    }

    fn list(&self, dir: &Path) -> (Vec<(PathBuf, String, bool)>, usize) {
        let mut out = Vec::new();
        let mut hidden = 0;
        let Ok(rd) = fs::read_dir(dir) else {
            return (out, 0);
        };
        for e in rd.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            if SKIP.contains(&name.as_str()) {
                continue;
            }
            if name.starts_with('.') && !self.show_hidden {
                hidden += 1;
                continue;
            }
            let is_dir = e
                .file_type()
                .map(|t| t.is_dir() || (t.is_symlink() && e.path().is_dir()))
                .unwrap_or(false);
            out.push((e.path(), name, is_dir));
        }
        out.sort_by_key(|(_, name, is_dir)| (!*is_dir, name.to_lowercase()));
        (out, hidden)
    }

    /// Re-read every expanded directory, keeping the selection by path.
    pub fn rebuild(&mut self) {
        let keep = self.nodes.get(self.selected).map(|n| n.path.clone());
        let mut nodes = Vec::new();
        let root = self.root.clone();
        let (_, hidden) = self.walk(&root, 0, &mut nodes);
        self.hidden_at_root = hidden;
        self.nodes = nodes;
        self.selected = keep
            .and_then(|p| self.nodes.iter().position(|n| n.path == p))
            .unwrap_or(0);
        self.selected = self.selected.min(self.nodes.len().saturating_sub(1));
    }

    fn walk(&self, dir: &Path, depth: usize, out: &mut Vec<Node>) -> (usize, usize) {
        let (entries, hidden) = self.list(dir);
        let n = entries.len();
        for (path, name, is_dir) in entries {
            let expanded = is_dir && self.expanded.contains(&path);
            out.push(Node {
                path: path.clone(),
                name,
                depth,
                is_dir,
                expanded,
            });
            if expanded {
                self.walk(&path, depth + 1, out);
            }
        }
        (n, hidden)
    }

    pub fn selected_node(&self) -> Option<&Node> {
        self.nodes.get(self.selected)
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.nodes.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.nodes.len() - 1);
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.nodes.len() {
            self.selected = idx;
        }
    }

    /// Enter/Right: toggle a directory, or return the file to open.
    pub fn activate(&mut self) -> Option<PathBuf> {
        let n = self.selected_node()?.clone();
        if n.is_dir {
            if n.expanded {
                self.expanded.remove(&n.path);
            } else {
                self.expanded.insert(n.path);
            }
            self.rebuild();
            None
        } else {
            Some(n.path)
        }
    }

    /// Left: collapse the directory, or jump to the parent.
    pub fn collapse_or_parent(&mut self) {
        let Some(n) = self.selected_node().cloned() else {
            return;
        };
        if n.is_dir && n.expanded {
            self.expanded.remove(&n.path);
            self.rebuild();
        } else if let Some(i) = self.nodes[..self.selected]
            .iter()
            .rposition(|p| p.is_dir && p.depth < n.depth)
        {
            self.selected = i;
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.rebuild();
    }

    /// Expand parents of `path` and select it (if it is under the root).
    pub fn reveal(&mut self, path: &Path) {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return;
        };
        let mut cur = self.root.clone();
        for comp in rel.parent().into_iter().flat_map(Path::components) {
            cur.push(comp);
            self.expanded.insert(cur.clone());
        }
        self.rebuild();
        if let Some(i) = self.nodes.iter().position(|n| n.path == path) {
            self.selected = i;
        }
    }

    pub fn keep_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "skink-tree-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src/code")).unwrap();
        fs::create_dir_all(d.join(".git")).unwrap();
        fs::write(d.join("src/main.rs"), "").unwrap();
        fs::write(d.join("src/code/a.rs"), "").unwrap();
        fs::write(d.join("Cargo.toml"), "").unwrap();
        fs::write(d.join(".env"), "").unwrap();
        d
    }

    fn names(t: &FileTree) -> Vec<String> {
        t.nodes
            .iter()
            .map(|n| format!("{}{}", "  ".repeat(n.depth), n.name))
            .collect()
    }

    #[test]
    fn dirs_first_hidden_counted_git_skipped() {
        let d = make();
        let t = FileTree::new(d.clone());
        assert_eq!(names(&t), ["src", "Cargo.toml"]);
        assert_eq!(t.hidden_at_root, 1); // .env (the .git dir is skipped outright)
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn expand_collapse_and_open() {
        let d = make();
        let mut t = FileTree::new(d.clone());
        assert!(t.activate().is_none()); // expand src
        assert_eq!(names(&t), ["src", "  code", "  main.rs", "Cargo.toml"]);
        t.move_selection(2);
        assert_eq!(t.activate().unwrap(), d.join("src/main.rs"));
        t.collapse_or_parent(); // file: jump to parent dir
        assert_eq!(t.selected_node().unwrap().name, "src");
        t.collapse_or_parent();
        assert_eq!(names(&t), ["src", "Cargo.toml"]);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn reveal_expands_ancestors() {
        let d = make();
        let mut t = FileTree::new(d.clone());
        t.reveal(&d.join("src/code/a.rs"));
        assert_eq!(t.selected_node().unwrap().name, "a.rs");
        t.toggle_hidden();
        assert!(names(&t).iter().any(|n| n == ".env"));
        fs::remove_dir_all(d).unwrap();
    }
}
