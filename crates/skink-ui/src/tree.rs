//! A lazy file tree: only expanded directories are read from disk.
//! `.gitignore`d entries are listed but dimmed (via ripgrep's `ignore`).

use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub kind: NodeKind,
    /// Matched by a `.gitignore` (shown dimmed).
    pub ignored: bool,
}

/// What the tree lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeMode {
    /// Folders the user opened, read lazily.
    Browse,
    /// Every file under the root whose path contains `query` (case
    /// insensitive), with its folders; ignored files are left out.
    Filter { query: String },
}

/// Upper bound on files walked for a filter.
const FILTER_MAX_FILES: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir { open: bool },
}

impl Node {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, NodeKind::Dir { .. })
    }
}

/// What `a` / `A` in the tree (and `:new` / `:mkdir`) create.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewEntry {
    File,
    Folder,
}

/// Upper bound for "expand recursively", so `*` on a huge folder stays cheap.
const EXPAND_MAX_NODES: usize = 5000;

pub struct FileTree {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    pub selected: usize,
    pub scroll: usize,
    /// Display columns scrolled sideways (long names).
    pub hscroll: usize,
    pub show_hidden: bool,
    /// Hidden entries at the root level that are currently not shown.
    pub hidden_at_root: usize,
    pub mode: TreeMode,
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
            hscroll: 0,
            show_hidden: false,
            hidden_at_root: 0,
            mode: TreeMode::Browse,
            expanded: HashSet::new(),
        };
        t.rebuild();
        t
    }

    /// Entries of `dir`, folders first: (path, name, is_dir, ignored), plus
    /// the number of hidden entries left out.
    fn list(&self, dir: &Path) -> (Vec<(PathBuf, String, bool, bool)>, usize) {
        let mut out = Vec::new();
        let mut hidden = 0;
        let Ok(rd) = fs::read_dir(dir) else {
            return (out, 0);
        };
        let kept = not_ignored(dir);
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
            let path = e.path();
            let ignored = !name.starts_with('.') && !kept.contains(&path);
            out.push((path, name, is_dir, ignored));
        }
        out.sort_by_key(|(_, name, is_dir, _)| (!*is_dir, name.to_lowercase()));
        (out, hidden)
    }

    /// Re-read every expanded directory (or re-run the filter), keeping the
    /// selection by path.
    pub fn rebuild(&mut self) {
        let keep = self.nodes.get(self.selected).map(|n| n.path.clone());
        let mut nodes = Vec::new();
        let root = self.root.clone();
        match &self.mode {
            TreeMode::Browse => {
                let (_, hidden) = self.walk(&root, 0, &mut nodes);
                self.hidden_at_root = hidden;
            }
            TreeMode::Filter { query } => {
                nodes = filtered(&root, &query.to_lowercase(), self.show_hidden);
                self.hidden_at_root = 0;
            }
        }
        self.nodes = nodes;
        self.selected = keep
            .and_then(|p| self.nodes.iter().position(|n| n.path == p))
            .unwrap_or(0);
        self.selected = self.selected.min(self.nodes.len().saturating_sub(1));
    }

    fn walk(&self, dir: &Path, depth: usize, out: &mut Vec<Node>) -> (usize, usize) {
        let (entries, hidden) = self.list(dir);
        let n = entries.len();
        for (path, name, is_dir, ignored) in entries {
            let open = is_dir && self.expanded.contains(&path);
            out.push(Node {
                path: path.clone(),
                name,
                depth,
                kind: if is_dir {
                    NodeKind::Dir { open }
                } else {
                    NodeKind::File
                },
                ignored,
            });
            if open {
                self.walk(&path, depth + 1, out);
            }
        }
        (n, hidden)
    }

    /// Start, change or (with `None`) end filtering.
    pub fn set_filter(&mut self, query: Option<&str>) {
        self.mode = match query {
            Some(q) if !q.is_empty() => TreeMode::Filter {
                query: q.to_owned(),
            },
            _ => TreeMode::Browse,
        };
        self.scroll = 0;
        self.rebuild();
        if let TreeMode::Filter { .. } = self.mode {
            // Land on the first match, not on a folder above it.
            self.selected = self.nodes.iter().position(|n| !n.is_dir()).unwrap_or(0);
        }
    }

    /// Folders currently open, for remembering per project.
    pub fn expanded_paths(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        v.sort();
        v
    }

    /// Re-open remembered folders (ones that still exist under the root).
    pub fn restore_expanded(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.expanded.extend(
            paths
                .into_iter()
                .filter(|p| p.starts_with(&self.root) && p.is_dir()),
        );
        self.rebuild();
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
        match n.kind {
            NodeKind::Dir { open: true } => {
                self.expanded.remove(&n.path);
                self.rebuild();
                None
            }
            NodeKind::Dir { open: false } => {
                self.expanded.insert(n.path);
                self.rebuild();
                None
            }
            NodeKind::File => Some(n.path),
        }
    }

    /// Left: collapse the directory, or jump to the parent.
    pub fn collapse_or_parent(&mut self) {
        let Some(n) = self.selected_node().cloned() else {
            return;
        };
        if n.kind == (NodeKind::Dir { open: true }) {
            self.expanded.remove(&n.path);
            self.rebuild();
        } else if let Some(i) = self.nodes[..self.selected]
            .iter()
            .rposition(|p| p.is_dir() && p.depth < n.depth)
        {
            self.selected = i;
        }
    }

    /// `-`: fold every folder; the selection moves to its top-level ancestor.
    pub fn collapse_all(&mut self) {
        let top = self.selected_node().and_then(|n| {
            let rel = n.path.strip_prefix(&self.root).ok()?;
            Some(self.root.join(rel.components().next()?))
        });
        self.expanded.clear();
        self.rebuild();
        if let Some(i) = top.and_then(|p| self.nodes.iter().position(|n| n.path == p)) {
            self.selected = i;
        }
    }

    /// `*`: open the selected folder and everything below it (bounded).
    pub fn expand_recursive(&mut self) {
        let Some(n) = self.selected_node().filter(|n| n.is_dir()) else {
            return;
        };
        let mut stack = vec![n.path.clone()];
        let mut seen = 0;
        while let Some(dir) = stack.pop() {
            if seen >= EXPAND_MAX_NODES {
                break;
            }
            let (entries, _) = self.list(&dir);
            seen += entries.len();
            // Ignored folders (target/, node_modules/) stay closed.
            stack.extend(entries.into_iter().filter(|e| e.2 && !e.3).map(|e| e.0));
            self.expanded.insert(dir);
        }
        self.rebuild();
    }

    /// The folder new entries go into: the selected folder, or the selected
    /// file's folder, or the root.
    pub fn target_dir(&self) -> PathBuf {
        match self.selected_node() {
            Some(n) if n.is_dir() => n.path.clone(),
            Some(n) => n.path.parent().map_or(self.root.clone(), Path::to_path_buf),
            None => self.root.clone(),
        }
    }

    /// Show `path` after it was created outside the tree's knowledge.
    pub fn refresh_and_reveal(&mut self, path: &Path) {
        self.rebuild();
        self.reveal(path);
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

/// Paths in `dir` that no `.gitignore` excludes (one level, hidden included).
fn not_ignored(dir: &Path) -> HashSet<PathBuf> {
    ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .map(ignore::DirEntry::into_path)
        .collect()
}

/// Files under `root` whose relative path contains `query` (lowercase),
/// plus their folders, in tree order (folders first, by name).
fn filtered(root: &Path, query: &str, show_hidden: bool) -> Vec<Node> {
    let mut keep: BTreeSet<Vec<(bool, String, String)>> = BTreeSet::new();
    let files = ignore::WalkBuilder::new(root)
        .hidden(!show_hidden)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| !t.is_dir()))
        .take(FILTER_MAX_FILES);
    for e in files {
        let Ok(rel) = e.path().strip_prefix(root) else {
            continue;
        };
        if !rel.to_string_lossy().to_lowercase().contains(query) {
            continue;
        }
        // Sort key per prefix: (is_file, lowercase name, name) at each level,
        // so folders come first and every folder precedes its children.
        let comps: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        for i in 1..=comps.len() {
            keep.insert(
                comps[..i]
                    .iter()
                    .enumerate()
                    .map(|(j, c)| (j + 1 == comps.len(), c.to_lowercase(), c.clone()))
                    .collect(),
            );
        }
    }
    keep.into_iter()
        .map(|key| {
            let path = key
                .iter()
                .fold(root.to_path_buf(), |p, (_, _, c)| p.join(c));
            let (is_file, _, name) = key.last().cloned().expect("non-empty key");
            Node {
                path,
                name,
                depth: key.len() - 1,
                kind: if is_file {
                    NodeKind::File
                } else {
                    NodeKind::Dir { open: true }
                },
                ignored: false,
            }
        })
        .collect()
}

/// Rename or move `from` to `to_rel` (relative to `root`; folders are
/// created). Never replaces an existing entry.
pub fn rename(from: &Path, root: &Path, to_rel: &str) -> std::io::Result<PathBuf> {
    let to_rel = to_rel.trim();
    if to_rel.is_empty() || Path::new(to_rel).is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "give a path relative to the project root",
        ));
    }
    let to = root.join(to_rel);
    if to.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{to_rel} already exists"),
        ));
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(from, &to)?;
    Ok(to)
}

/// Create `name` (may contain `/`, parents are created) inside `dir`.
/// Files must not exist yet; folders may (like `mkdir -p`).
pub fn create(dir: &Path, name: &str, what: NewEntry) -> std::io::Result<PathBuf> {
    let name = name.trim();
    let rel = Path::new(name);
    if name.is_empty() || rel.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "give a name relative to the folder",
        ));
    }
    let path = dir.join(rel);
    match what {
        NewEntry::Folder => fs::create_dir_all(&path)?,
        NewEntry::File => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
        }
    }
    Ok(path)
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
    fn create_nested_then_collapse_and_expand_all() {
        let d = make();
        let mut t = FileTree::new(d.clone());
        assert_eq!(
            t.target_dir(),
            d.join("src"),
            "selected folder receives new entries"
        );
        let f = create(&d, "src/new/deep.rs", NewEntry::File).unwrap();
        assert!(f.is_file());
        assert!(
            create(&d, "src/new/deep.rs", NewEntry::File).is_err(),
            "never overwrites"
        );
        assert!(
            create(&d, "src/new", NewEntry::Folder).is_ok(),
            "folders like mkdir -p"
        );
        t.refresh_and_reveal(&f);
        assert_eq!(t.selected_node().unwrap().name, "deep.rs");
        t.collapse_all();
        assert_eq!(names(&t), ["src", "Cargo.toml"]);
        assert_eq!(t.selected_node().unwrap().name, "src");
        t.expand_recursive();
        assert!(names(&t).contains(&"    deep.rs".to_string()));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn gitignored_entries_are_dimmed_and_filter_finds_files() {
        let d = make();
        fs::write(d.join(".gitignore"), "target/\n").unwrap();
        fs::create_dir_all(d.join("target/debug")).unwrap();
        fs::write(d.join("target/debug/code.rs"), "").unwrap();
        let mut t = FileTree::new(d.clone());
        let target = t.nodes.iter().find(|n| n.name == "target").unwrap();
        assert!(target.ignored);
        assert!(!t.nodes.iter().find(|n| n.name == "src").unwrap().ignored);
        t.set_filter(Some("code"));
        // Ignored target/debug/code.rs is not listed; src/code/a.rs is.
        assert_eq!(names(&t), ["src", "  code", "    a.rs"]);
        assert_eq!(t.selected_node().unwrap().name, "a.rs", "lands on a file");
        t.set_filter(None);
        assert_eq!(t.mode, TreeMode::Browse);
        let moved = rename(&d.join("src/main.rs"), &d, "src/bin/main.rs").unwrap();
        assert!(moved.is_file() && !d.join("src/main.rs").exists());
        assert!(rename(&moved, &d, "Cargo.toml").is_err(), "never replaces");
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
