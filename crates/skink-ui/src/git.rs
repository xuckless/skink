//! Git through the `git` command line: the index and HEAD versions of a file,
//! line hunks against them, patches to stage or unstage one hunk, blame and
//! status. Plain `git` keeps the binary small (gix would add megabytes) and
//! matches whatever the user's git config does. Everything here is blocking:
//! callers run it on worker threads. The async split (a job per question,
//! results polled by the UI) follows gitui's asyncgit idea (MIT); no code is
//! taken from it.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Files above this size get no change marks (like large-file mode).
pub const MAX_BYTES: usize = 2 << 20;

/// Run git in `cwd`; stdout on success, stderr (trimmed) on failure.
pub fn run(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    run_with_input(cwd, args, None)
}

/// [`run`] with `input` on stdin.
pub fn run_with_input(cwd: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, String> {
    use std::io::Write;
    test_guard(cwd)?;
    if !in_work_tree(cwd) {
        return Err("not a git repository".into());
    }
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        // Never block on index.lock for read-only questions, never prompt.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("git: {e}"))?;
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(data).map_err(|e| format!("git: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        Err(if err.is_empty() {
            format!("git {} failed", args.first().unwrap_or(&""))
        } else {
            err
        })
    }
}

/// Tests may only run git inside the system temp directory, so no test can
/// ever touch the checkout it runs from (a test once stashed it).
#[cfg(test)]
pub fn test_guard(cwd: &Path) -> Result<(), String> {
    let tmp = crate::doc::canonical(&std::env::temp_dir());
    if crate::doc::canonical(cwd).starts_with(&tmp) {
        Ok(())
    } else {
        Err(format!("test refused to run git outside {}", tmp.display()))
    }
}

#[cfg(not(test))]
pub fn test_guard(_cwd: &Path) -> Result<(), String> {
    Ok(())
}

/// A file inside a work tree: the top directory and its path from there
/// (forward slashes, as git prints and expects it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    pub root: PathBuf,
    pub rel: String,
}

/// Whether `dir` or a parent holds a `.git` entry (a folder, or the file a
/// linked worktree uses). Checked before spawning git, so files outside any
/// repository cost no process (and on Windows no process holds the folder
/// open, which blocks deleting it).
pub fn in_work_tree(dir: &Path) -> bool {
    dir.ancestors().any(|d| d.join(".git").exists())
}

/// Where `path` sits in a git work tree, if it does.
pub fn locate(path: &Path) -> Option<FileRef> {
    let dir = path.parent()?;
    if !in_work_tree(dir) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let out = run(dir, &["rev-parse", "--show-toplevel", "--show-prefix"]).ok()?;
    let text = String::from_utf8(out).ok()?;
    let mut lines = text.lines();
    let root = PathBuf::from(lines.next()?.trim());
    let prefix = lines.next().unwrap_or("").trim();
    Some(FileRef {
        root,
        rel: format!("{prefix}{name}"),
    })
}

/// The work tree containing `dir`.
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    if !in_work_tree(dir) {
        return None;
    }
    let out = run(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    let s = String::from_utf8(out).ok()?;
    Some(PathBuf::from(s.trim()))
}

/// What a file is compared with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// In the index; `head` is the committed version (`None`: added since).
    Tracked { index: String, head: Option<String> },
    /// Not in the index and not ignored: every line is new.
    Untracked,
    /// Ignored, binary or too large: no marks.
    Unmarked,
}

/// Read the index and HEAD versions of `f`.
pub fn load_base(f: &FileRef) -> Base {
    let index_spec = format!(":{}", f.rel);
    match run(&f.root, &["show", &index_spec]) {
        Ok(bytes) => {
            if bytes.len() > MAX_BYTES || bytes.contains(&0) {
                return Base::Unmarked;
            }
            let Ok(index) = String::from_utf8(bytes) else {
                return Base::Unmarked;
            };
            let head = run(&f.root, &["show", &format!("HEAD:{}", f.rel)])
                .ok()
                .and_then(|b| String::from_utf8(b).ok());
            Base::Tracked { index, head }
        }
        Err(_) => {
            let ignored = run(&f.root, &["check-ignore", "-q", "--", &f.rel]).is_ok();
            if ignored {
                Base::Unmarked
            } else {
                Base::Untracked
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkKind {
    Added,
    Modified,
    /// Lines removed before buffer line `new.start` (`new` is empty).
    Deleted,
}

/// One changed region: `new` in the buffer, `old` in the base (0-based,
/// half-open line ranges).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub kind: HunkKind,
    pub new: Range<usize>,
    pub old: Range<usize>,
}

impl Hunk {
    /// Whether buffer line `line` is part of (or marks) this hunk.
    pub fn touches(&self, line: usize) -> bool {
        match self.kind {
            HunkKind::Deleted => line == self.new.start || line + 1 == self.new.start,
            _ => self.new.contains(&line),
        }
    }

    /// The buffer line the hunk is shown at (first line; for deletions the
    /// line after the gap, or the last line at the end of the file).
    pub fn anchor(&self) -> usize {
        self.new.start
    }
}

/// Unstaged changes (buffer against the index) and staged ones (index
/// against HEAD, moved to buffer lines), both sorted by line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hunks {
    pub unstaged: Vec<Hunk>,
    pub staged: Vec<Hunk>,
}

/// Text split into lines without line endings (`\r\n` and `\n` alike), so
/// CRLF work trees compare cleanly with LF blobs.
pub fn lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let t = text.strip_suffix('\n').unwrap_or(text);
    t.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

/// Line hunks turning `old` into `new`.
pub fn diff(old: &[&str], new: &[&str]) -> Vec<Hunk> {
    let ops = similar::capture_diff_slices(similar::Algorithm::Myers, old, new);
    let mut out: Vec<Hunk> = Vec::new();
    let mut cur: Option<(Range<usize>, Range<usize>)> = None;
    let flush = |cur: &mut Option<(Range<usize>, Range<usize>)>, out: &mut Vec<Hunk>| {
        if let Some((o, n)) = cur.take() {
            let kind = match (o.is_empty(), n.is_empty()) {
                (true, _) => HunkKind::Added,
                (false, true) => HunkKind::Deleted,
                (false, false) => HunkKind::Modified,
            };
            out.push(Hunk {
                kind,
                new: n,
                old: o,
            });
        }
    };
    for op in &ops {
        let (tag, o, n) = op.as_tag_tuple();
        if tag == similar::DiffTag::Equal {
            flush(&mut cur, &mut out);
            continue;
        }
        cur = Some(match cur.take() {
            Some((co, cn)) => (co.start..o.end, cn.start..n.end),
            None => (o, n),
        });
    }
    flush(&mut cur, &mut out);
    out
}

/// Map a line of `old` to the matching line of `new` through `hunks`
/// (lines inside a changed region go to its start).
fn map_line(hunks: &[Hunk], line: usize) -> usize {
    let mut delta: isize = 0;
    for h in hunks {
        if line < h.old.start {
            break;
        }
        if line < h.old.end {
            return h.new.start;
        }
        delta += h.new.len() as isize - h.old.len() as isize;
    }
    line.saturating_add_signed(delta)
}

/// Both kinds of hunks for a buffer's `text` against `base`.
pub fn compute(base: &Base, text: &str) -> Hunks {
    let new = lines(text);
    match base {
        Base::Unmarked => Hunks::default(),
        Base::Untracked if new.is_empty() => Hunks::default(),
        Base::Untracked => Hunks {
            unstaged: vec![Hunk {
                kind: HunkKind::Added,
                new: 0..new.len(),
                old: 0..0,
            }],
            staged: Vec::new(),
        },
        Base::Tracked { index, head } => {
            let idx = lines(index);
            let unstaged = diff(&idx, &new);
            let head_lines = head.as_deref().map(lines).unwrap_or_default();
            let staged = diff(&head_lines, &idx)
                .into_iter()
                .map(|h| {
                    let start = map_line(&unstaged, h.new.start);
                    let end = if h.new.is_empty() {
                        start
                    } else {
                        map_line(&unstaged, h.new.end - 1) + 1
                    };
                    Hunk {
                        new: start..end.max(start),
                        ..h
                    }
                })
                .collect();
            Hunks { unstaged, staged }
        }
    }
}

/// Whether the text ends without a final newline.
fn no_final_newline(text: &str) -> bool {
    !text.is_empty() && !text.ends_with('\n')
}

/// A zero-context patch for one hunk, turning `old_text` lines `h.old` into
/// `new_text` lines `h.new`, for `git apply --unidiff-zero`. New lines take
/// the line ending `old_text` uses, so staging from a CRLF buffer into an LF
/// index does not churn every line.
pub fn hunk_patch(rel: &str, old_text: &str, new_text: &str, h: &Hunk) -> String {
    let old = lines(old_text);
    let new = lines(new_text);
    let eol = if old_text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let start = |r: &Range<usize>| if r.is_empty() { r.start } else { r.start + 1 };
    let mut p = format!(
        "diff --git a/{rel} b/{rel}\n--- a/{rel}\n+++ b/{rel}\n@@ -{},{} +{},{} @@\n",
        start(&h.old),
        h.old.len(),
        start(&h.new),
        h.new.len()
    );
    let push = |p: &mut String, sign: char, lines: &[&str], r: &Range<usize>, last_open: bool| {
        for (k, i) in r.clone().enumerate() {
            p.push(sign);
            p.push_str(lines[i]);
            p.push_str(eol);
            if last_open && i + 1 == lines.len() && k + 1 == r.len() {
                // Replace the final eol with git's marker.
                p.truncate(p.len() - eol.len());
                p.push_str("\n\\ No newline at end of file\n");
            }
        }
    };
    push(&mut p, '-', &old, &h.old, no_final_newline(old_text));
    push(&mut p, '+', &new, &h.new, no_final_newline(new_text));
    p
}

/// Stage hunk `h` of `buffer` (an unstaged hunk against `index`).
pub fn stage_hunk(f: &FileRef, index: &str, buffer: &str, h: &Hunk) -> Result<(), String> {
    let patch = hunk_patch(&f.rel, index, buffer, h);
    run_with_input(
        &f.root,
        &[
            "apply",
            "--cached",
            "--unidiff-zero",
            "--whitespace=nowarn",
            "-",
        ],
        Some(patch.as_bytes()),
    )
    .map(|_| ())
}

/// Unstage staged hunk `h` (`h.old` in HEAD, its index lines found again
/// by diffing HEAD against the index).
pub fn unstage_hunk(f: &FileRef, head: &str, index: &str, h: &Hunk) -> Result<(), String> {
    // `h.new` was moved to buffer lines; recover the index range.
    let found = diff(&lines(head), &lines(index))
        .into_iter()
        .find(|s| s.old == h.old)
        .ok_or("the staged hunk changed; refresh and try again")?;
    let patch = hunk_patch(&f.rel, head, index, &found);
    run_with_input(
        &f.root,
        &[
            "apply",
            "--cached",
            "--reverse",
            "--unidiff-zero",
            "--whitespace=nowarn",
            "-",
        ],
        Some(patch.as_bytes()),
    )
    .map(|_| ())
}

// ---- status ---------------------------------------------------------------

/// How a path differs, most important first (folders show their worst).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileState {
    Conflicted,
    Modified,
    Added,
    Untracked,
    Ignored,
}

/// One line of `git status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Path from the work tree top, forward slashes (a trailing `/` for a
    /// whole untracked or ignored folder).
    pub path: String,
    /// Index and work-tree columns of `git status --short` (`.` = clean).
    pub x: char,
    pub y: char,
    pub state: FileState,
    /// Renames: the old path.
    pub from: Option<String>,
}

impl Entry {
    pub fn staged(&self) -> bool {
        self.state != FileState::Untracked
            && self.state != FileState::Ignored
            && self.state != FileState::Conflicted
            && self.x != '.'
    }
    pub fn unstaged(&self) -> bool {
        self.state == FileState::Untracked
            || (self.state != FileState::Ignored
                && self.state != FileState::Conflicted
                && self.y != '.')
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Branch name; `None` on a detached HEAD.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    /// Commits ahead of / behind the upstream.
    pub ahead_behind: Option<(u32, u32)>,
    pub entries: Vec<Entry>,
}

/// `git status --porcelain=v2 --branch -z` (with `--ignored` for the tree).
pub fn status(root: &Path, ignored: bool) -> Result<Status, String> {
    let mut args = vec!["status", "--porcelain=v2", "--branch", "-z"];
    if ignored {
        args.push("--ignored");
    }
    let out = run(root, &args)?;
    Ok(parse_status(&String::from_utf8_lossy(&out)))
}

pub fn parse_status(out: &str) -> Status {
    let mut s = Status::default();
    let mut fields = out.split('\0');
    while let Some(rec) = fields.next() {
        if let Some(h) = rec.strip_prefix("# ") {
            let (k, v) = h.split_once(' ').unwrap_or((h, ""));
            match k {
                "branch.head" if v != "(detached)" => s.branch = Some(v.to_owned()),
                "branch.upstream" => s.upstream = Some(v.to_owned()),
                "branch.ab" => {
                    let mut it = v
                        .split(' ')
                        .map(|n| n.trim_start_matches(['+', '-']).parse::<u32>().unwrap_or(0));
                    s.ahead_behind = Some((it.next().unwrap_or(0), it.next().unwrap_or(0)));
                }
                _ => {}
            }
            continue;
        }
        let mut parts = rec.splitn(2, ' ');
        let (tag, rest) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let xy = |r: &str| {
            let mut c = r.chars();
            (c.next().unwrap_or('.'), c.next().unwrap_or('.'))
        };
        let entry = match tag {
            "1" | "2" => {
                // XY sub mH mI mW hH hI [score] path
                let n = if tag == "1" { 8 } else { 9 };
                let f: Vec<&str> = rest.splitn(n, ' ').collect();
                let (x, y) = xy(f.first().copied().unwrap_or(""));
                let path = f.get(n - 1).copied().unwrap_or("").to_owned();
                let from = (tag == "2").then(|| fields.next().unwrap_or("").to_owned());
                let state = if x == 'A' {
                    FileState::Added
                } else {
                    FileState::Modified
                };
                Entry {
                    path,
                    x,
                    y,
                    state,
                    from,
                }
            }
            "u" => {
                let f: Vec<&str> = rest.splitn(10, ' ').collect();
                let (x, y) = xy(f.first().copied().unwrap_or(""));
                Entry {
                    path: f.get(9).copied().unwrap_or("").to_owned(),
                    x,
                    y,
                    state: FileState::Conflicted,
                    from: None,
                }
            }
            "?" | "!" => Entry {
                path: rest.to_owned(),
                x: tag.chars().next().unwrap_or('?'),
                y: tag.chars().next().unwrap_or('?'),
                state: if tag == "?" {
                    FileState::Untracked
                } else {
                    FileState::Ignored
                },
                from: None,
            },
            _ => continue,
        };
        if !entry.path.is_empty() {
            s.entries.push(entry);
        }
    }
    s
}

/// `rel` (forward slashes) under `root`, built component by component so
/// it compares equal to tree paths on Windows too.
pub fn abs(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    p.extend(rel.split('/').filter(|c| !c.is_empty()));
    p
}

/// Status of every path for the file tree, folders included.
#[derive(Debug, Default)]
pub struct TreeMarks {
    files: std::collections::HashMap<PathBuf, FileState>,
    /// Untracked or ignored folders listed as a whole.
    whole: Vec<(PathBuf, FileState)>,
    folders: std::collections::HashMap<PathBuf, FileState>,
}

impl TreeMarks {
    pub fn new(root: &Path, st: &Status) -> Self {
        let mut m = TreeMarks::default();
        for e in &st.entries {
            let p = abs(root, &e.path);
            if e.path.ends_with('/') {
                m.whole.push((p.clone(), e.state));
            } else {
                m.files.insert(p.clone(), e.state);
            }
            if e.state == FileState::Ignored {
                continue;
            }
            for dir in p.ancestors().skip(1) {
                if !dir.starts_with(root) || dir == root {
                    break;
                }
                let cur = m.folders.entry(dir.to_path_buf()).or_insert(e.state);
                *cur = (*cur).min(e.state);
            }
        }
        m
    }

    pub fn get(&self, path: &Path) -> Option<FileState> {
        self.files
            .get(path)
            .or_else(|| self.folders.get(path))
            .copied()
            .or_else(|| {
                self.whole
                    .iter()
                    .find(|(d, _)| path.starts_with(d))
                    .map(|(_, s)| *s)
            })
    }
}

// ---- blame ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub author: String,
    /// Unix seconds.
    pub time: i64,
    pub summary: String,
}

impl Commit {
    /// The all-zero hash git uses for lines not committed yet.
    pub fn uncommitted(&self) -> bool {
        self.hash.bytes().all(|b| b == b'0')
    }
}

/// Last commit of each line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Blame {
    pub commits: Vec<Commit>,
    /// Per buffer line, an index into `commits`.
    pub lines: Vec<usize>,
}

impl Blame {
    pub fn at(&self, line: usize) -> Option<&Commit> {
        self.lines.get(line).and_then(|&i| self.commits.get(i))
    }
}

/// Blame `contents` (the buffer) as file `f`.
pub fn blame(f: &FileRef, contents: &str) -> Result<Blame, String> {
    let out = run_with_input(
        &f.root,
        &["blame", "--porcelain", "--contents", "-", "--", &f.rel],
        Some(contents.as_bytes()),
    )?;
    Ok(parse_blame(&String::from_utf8_lossy(&out)))
}

pub fn parse_blame(out: &str) -> Blame {
    let mut b = Blame::default();
    let mut by_hash: std::collections::HashMap<String, usize> = Default::default();
    let mut cur: Option<usize> = None;
    for line in out.lines() {
        if line.starts_with('\t') {
            if let Some(c) = cur {
                b.lines.push(c);
            }
            continue;
        }
        let mut words = line.splitn(2, ' ');
        let key = words.next().unwrap_or("");
        let val = words.next().unwrap_or("");
        if key.len() == 40 && key.bytes().all(|c| c.is_ascii_hexdigit()) {
            let idx = *by_hash.entry(key.to_owned()).or_insert_with(|| {
                b.commits.push(Commit {
                    hash: key.to_owned(),
                    author: String::new(),
                    time: 0,
                    summary: String::new(),
                });
                b.commits.len() - 1
            });
            cur = Some(idx);
            continue;
        }
        let Some(c) = cur.and_then(|i| b.commits.get_mut(i)) else {
            continue;
        };
        match key {
            "author" => c.author = val.to_owned(),
            "author-time" => c.time = val.parse().unwrap_or(0),
            "summary" => c.summary = val.to_owned(),
            _ => {}
        }
    }
    b
}

/// "3 days ago" for a time `secs` seconds in the past.
pub fn relative(secs: i64) -> String {
    let s = secs.max(0);
    let (n, unit) = match s {
        0..60 => return "just now".into(),
        60..3600 => (s / 60, "minute"),
        3600..86_400 => (s / 3600, "hour"),
        86_400..2_592_000 => (s / 86_400, "day"),
        2_592_000..31_536_000 => (s / 2_592_000, "month"),
        _ => (s / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// ---- branches & stashes ---------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub current: bool,
    pub upstream: String,
    pub when: String,
    pub subject: String,
}

pub fn branches(root: &Path) -> Result<Vec<Branch>, String> {
    let out = run(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(HEAD)%00%(refname:short)%00%(upstream:short)%00%(committerdate:relative)%00%(subject)",
            "refs/heads",
        ],
    )?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\0').collect();
            Some(Branch {
                current: *f.first()? == "*",
                name: (*f.get(1)?).to_owned(),
                upstream: f.get(2).copied().unwrap_or("").to_owned(),
                when: f.get(3).copied().unwrap_or("").to_owned(),
                subject: f.get(4).copied().unwrap_or("").to_owned(),
            })
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    /// `stash@{0}`.
    pub name: String,
    pub when: String,
    pub subject: String,
}

pub fn stashes(root: &Path) -> Result<Vec<Stash>, String> {
    let out = run(root, &["stash", "list", "--format=%gd%x00%cr%x00%gs"])?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\0').collect();
            Some(Stash {
                name: (*f.first()?).to_owned(),
                when: f.get(1).copied().unwrap_or("").to_owned(),
                subject: f.get(2).copied().unwrap_or("").to_owned(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(h: &[Hunk]) -> Vec<(HunkKind, Range<usize>, Range<usize>)> {
        h.iter()
            .map(|h| (h.kind, h.new.clone(), h.old.clone()))
            .collect()
    }

    #[test]
    fn diff_finds_added_modified_and_deleted_lines() {
        let old = ["a", "b", "c", "d", "e"];
        let new = ["a", "B", "c", "x", "y", "e"];
        // b->B modified; d->x,y modified (replace grows).
        assert_eq!(
            kinds(&diff(&old, &new)),
            vec![
                (HunkKind::Modified, 1..2, 1..2),
                (HunkKind::Modified, 3..5, 3..4)
            ]
        );
        assert_eq!(
            kinds(&diff(&["a", "b", "c"], &["a", "c"])),
            vec![(HunkKind::Deleted, 1..1, 1..2)]
        );
        assert_eq!(
            kinds(&diff(&["a"], &["a", "n"])),
            vec![(HunkKind::Added, 1..2, 1..1)]
        );
    }

    #[test]
    fn crlf_buffers_compare_cleanly_with_lf_blobs() {
        assert!(diff(&lines("a\nb\n"), &lines("a\r\nb\r\n")).is_empty());
    }

    #[test]
    fn staged_hunks_move_to_buffer_lines() {
        // HEAD: a b c d; index: a B c d (staged b->B); buffer adds 2 lines
        // at the top, so the staged hunk shows at buffer line 3.
        let base = Base::Tracked {
            index: "a\nB\nc\nd\n".into(),
            head: Some("a\nb\nc\nd\n".into()),
        };
        let h = compute(&base, "n1\nn2\na\nB\nc\nd\n");
        assert_eq!(kinds(&h.unstaged), vec![(HunkKind::Added, 0..2, 0..0)]);
        assert_eq!(kinds(&h.staged), vec![(HunkKind::Modified, 3..4, 1..2)]);
    }

    #[test]
    fn hunk_patches_use_unidiff_zero_numbering() {
        let h = &diff(&lines("a\nb\n"), &lines("a\nb\nc\n"))[0];
        assert_eq!(
            hunk_patch("f.txt", "a\nb\n", "a\nb\nc\n", h),
            "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -2,0 +3,1 @@\n+c\n"
        );
        let h = &diff(&lines("a\nb"), &lines("a\nB"))[0];
        assert_eq!(
            hunk_patch("f", "a\nb", "a\nB", h),
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -2,1 +2,1 @@\n-b\n\\ No newline at end of file\n+B\n\\ No newline at end of file\n"
        );
    }

    #[test]
    fn status_v2_records_parse() {
        let out = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 aaa bbb src/a.rs\0\
1 A. N... 000000 100644 100644 000 ccc new.rs\0\
2 R. N... 100644 100644 100644 ddd eee R100 b.rs\0old b.rs\0\
u UU N... 100644 100644 100644 100644 f1 f2 f3 c.rs\0? notes.txt\0! target/\0";
        let s = parse_status(out);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.ahead_behind, Some((2, 1)));
        let got: Vec<_> = s
            .entries
            .iter()
            .map(|e| (e.path.as_str(), e.state, e.from.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("src/a.rs", FileState::Modified, None),
                ("new.rs", FileState::Added, None),
                ("b.rs", FileState::Modified, Some("old b.rs")),
                ("c.rs", FileState::Conflicted, None),
                ("notes.txt", FileState::Untracked, None),
                ("target/", FileState::Ignored, None),
            ]
        );
        let root = Path::new("/r");
        let m = TreeMarks::new(root, &s);
        assert_eq!(m.get(&abs(root, "src")), Some(FileState::Modified));
        assert_eq!(
            m.get(&abs(root, "target/debug/x")),
            Some(FileState::Ignored)
        );
        assert_eq!(m.get(&abs(root, "src/a.rs")), Some(FileState::Modified));
        assert_eq!(m.get(&abs(root, "zzz")), None);
    }

    #[test]
    fn blame_porcelain_parses_repeated_commits() {
        let h = "a".repeat(40);
        let out = format!(
            "{h} 1 1 2\nauthor Ann\nauthor-time 100\nsummary first\nfilename f\n\tx\n{h} 2 2\n\ty\n{z} 3 3 1\nauthor Not Committed Yet\nsummary Version of f from f\n\tz\n",
            z = "0".repeat(40)
        );
        let b = parse_blame(&out);
        assert_eq!(b.lines, vec![0, 0, 1]);
        assert_eq!(b.at(1).map(|c| c.author.as_str()), Some("Ann"));
        assert!(b.at(2).is_some_and(Commit::uncommitted));
    }
}
