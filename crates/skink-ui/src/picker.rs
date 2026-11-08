//! Quick open: one list for open tabs, recent files and project files, with
//! a small subsequence matcher (kept in-house: it is ~30 lines and avoids a
//! dependency; frizbee/skim remain options if ranking quality needs it).

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Tab(usize),
    File(PathBuf),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub label: String,
    pub detail: String,
    pub target: Target,
    /// Where it came from, for the right-hand tag.
    pub tag: &'static str,
}

pub struct Picker {
    pub input: String,
    pub items: Vec<Item>,
    pub selected: usize,
}

/// Higher is better; `None` when `query` is not a subsequence of `text`.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let name_start = t
        .iter()
        .rposition(|c| *c == '/' || *c == '\\')
        .map_or(0, |i| i + 1);
    let (mut qi, mut score, mut prev) = (0, 0i32, None::<usize>);
    for (i, c) in t.iter().enumerate() {
        if qi < q.len() && *c == q[qi] {
            score += 1;
            if prev == Some(i.wrapping_sub(1)) {
                score += 4; // contiguous run
            }
            if i == name_start || (i > 0 && !t[i - 1].is_alphanumeric()) {
                score += 3; // word / file-name start
            }
            if i >= name_start {
                score += 2; // inside the file name
            }
            prev = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then(|| score * 10 - t.len() as i32 / 4)
}

/// Files under `root` (bounded), skipping VCS, build and hidden directories.
pub fn project_files(root: &Path, limit: usize) -> Vec<PathBuf> {
    const SKIP: &[&str] = &[
        "target",
        "node_modules",
        "dist",
        "build",
        "__pycache__",
        "venv",
    ];
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if depth < 8 && !SKIP.contains(&name.as_str()) {
                    stack.push((e.path(), depth + 1));
                }
            } else if ft.is_file() {
                out.push(e.path());
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

impl Picker {
    pub fn new(tabs: Vec<Item>, recent: &[PathBuf], project: Vec<PathBuf>, root: &Path) -> Self {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut items = Vec::new();
        for t in tabs {
            if let Target::File(p) = &t.target {
                seen.insert(p.clone());
            }
            items.push(t);
        }
        let rel = |p: &Path| {
            p.strip_prefix(root)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned()
        };
        let mut add = |p: &PathBuf, tag: &'static str, items: &mut Vec<Item>| {
            if seen.insert(p.clone()) {
                let name = p
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                items.push(Item {
                    label: name,
                    detail: rel(p),
                    target: Target::File(p.clone()),
                    tag,
                });
            }
        };
        for p in recent {
            if p.is_file() {
                add(p, "recent", &mut items);
            }
        }
        for p in &project {
            add(p, "", &mut items);
        }
        Self {
            input: String::new(),
            items,
            selected: 0,
        }
    }

    /// Items matching the input, best first (stable order when empty).
    pub fn matches(&self) -> Vec<&Item> {
        if self.input.trim().is_empty() {
            return self.items.iter().take(200).collect();
        }
        let mut scored: Vec<(i32, &Item)> = self
            .items
            .iter()
            .filter_map(|i| {
                fuzzy_score(&self.input, &format!("{}/{}", i.detail, i.label)).map(|s| (s, i))
            })
            .collect();
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        scored.into_iter().take(200).map(|(_, i)| i).collect()
    }

    pub fn current(&self) -> Option<Target> {
        self.matches().get(self.selected).map(|i| i.target.clone())
    }

    pub fn move_selection(&mut self, delta: isize) {
        let n = self.matches().len();
        if n > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        }
    }

    pub fn type_char(&mut self, c: char) {
        self.input.push(c);
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        self.input.pop();
        self.selected = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_ranking() {
        assert!(fuzzy_score("xyz", "abc").is_none());
        let a = fuzzy_score("edrs", "src/editor.rs").unwrap();
        let b = fuzzy_score("edrs", "src/some/deeper/rs_stuff.txt").unwrap();
        assert!(a > b);
        // File-name hits beat directory hits.
        assert!(
            fuzzy_score("main", "src/main.rs").unwrap()
                > fuzzy_score("main", "main_dir/other.rs").unwrap()
        );
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn picker_orders_tabs_recent_project_without_duplicates() {
        let root = Path::new("/p");
        let tabs = vec![Item {
            label: "a.rs".into(),
            detail: "a.rs".into(),
            target: Target::File("/p/a.rs".into()),
            tag: "open",
        }];
        let dir = std::env::temp_dir().join(format!("skink-picker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rf = dir.join("r.txt");
        std::fs::write(&rf, "").unwrap();
        let p = Picker::new(
            tabs,
            &[rf.clone(), "/gone/missing".into()],
            vec!["/p/a.rs".into(), "/p/b.rs".into()],
            root,
        );
        let labels: Vec<_> = p.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["a.rs", "r.txt", "b.rs"]);
        let mut p = p;
        p.type_char('b');
        assert_eq!(p.current(), Some(Target::File("/p/b.rs".into())));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn project_walk_skips_hidden_and_build_dirs() {
        let d = std::env::temp_dir().join(format!("skink-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for sub in ["src", ".git", "target", "node_modules"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
            std::fs::write(d.join(sub).join("f.txt"), "").unwrap();
        }
        std::fs::write(d.join("top.txt"), "").unwrap();
        let mut names: Vec<_> = project_files(&d, 100)
            .iter()
            .map(|p| {
                p.strip_prefix(&d)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        names.sort();
        assert_eq!(names, ["src/f.txt", "top.txt"]);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
