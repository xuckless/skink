//! Code review over a git diff (`:codereview`): the working tree, the
//! staged changes, a commit, a branch against its base, or everything since
//! a commit ("what the agent changed"). Comments sit on diff lines, are kept
//! in `<git dir>/skink-review/<target>.json` (so they survive restarts and
//! stay out of the work tree), can be resolved, exported as Markdown, and
//! sent to the AI agent to address.

use crate::{
    App,
    doc::DocKind,
    git,
    gitui::{GitView, Row, Tint, ViewWhat},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What is being reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Everything not committed (staged and unstaged) against HEAD.
    WorkTree,
    Staged,
    /// One commit (full hash).
    Commit(String),
    /// The work tree against a commit (full hash): what changed since.
    Since(String),
    /// The current branch against `base` (`base...HEAD`).
    Branch(String),
}

impl Target {
    /// File name of its comment store.
    fn key(&self) -> String {
        let safe = |s: &str| -> String {
            s.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect()
        };
        match self {
            Target::WorkTree => "worktree".into(),
            Target::Staged => "staged".into(),
            Target::Commit(h) => format!("commit-{h}"),
            Target::Since(h) => format!("since-{h}"),
            Target::Branch(b) => format!("branch-{}", safe(b)),
        }
    }

    pub fn describe(&self) -> String {
        let short = |h: &str| h[..8.min(h.len())].to_owned();
        match self {
            Target::WorkTree => "uncommitted changes".into(),
            Target::Staged => "staged changes".into(),
            Target::Commit(h) => format!("commit {}", short(h)),
            Target::Since(h) => format!("changes since {}", short(h)),
            Target::Branch(b) => format!("this branch against {b}"),
        }
    }

    fn diff_args(&self) -> Vec<String> {
        let mut a: Vec<String> = match self {
            Target::WorkTree => vec!["diff".into(), "HEAD".into()],
            Target::Staged => vec!["diff".into(), "--cached".into()],
            Target::Commit(h) => vec!["show".into(), "--format=".into(), h.clone()],
            Target::Since(h) => vec!["diff".into(), h.clone()],
            Target::Branch(b) => vec!["diff".into(), format!("{b}...HEAD")],
        };
        a.extend(
            ["--no-color", "--no-ext-diff", "-M", "--patch"]
                .iter()
                .map(|s| (*s).to_owned()),
        );
        a
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Open,
    Resolved,
}

/// One review comment, on a line of a file's diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    /// Path relative to the repository root.
    pub file: String,
    /// 1-based line on the new side (None: a removed line).
    pub new: Option<usize>,
    /// 1-based line on the old side (removed lines).
    pub old: Option<usize>,
    /// The line's text when the comment was made: finds the line again
    /// after the diff changed, else the comment is outdated.
    pub anchor: String,
    pub text: String,
    pub state: State,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Store {
    target: String,
    next: u64,
    comments: Vec<Comment>,
}

/// A review tab's identity: the repository and what is reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub target: Target,
}

impl Review {
    fn store_path(&self) -> PathBuf {
        self.git_dir
            .join("skink-review")
            .join(format!("{}.json", self.target.key()))
    }

    fn load(&self) -> Store {
        std::fs::read_to_string(self.store_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| Store {
                target: self.target.describe(),
                next: 1,
                comments: Vec::new(),
            })
    }

    fn save(&self, s: &Store) -> Result<(), String> {
        let p = self.store_path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
        std::fs::write(&p, json).map_err(|e| format!("{}: {e}", p.display()))
    }
}

/// The comment being written in the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Draft {
    New {
        review: Box<Review>,
        file: String,
        new: Option<usize>,
        old: Option<usize>,
        anchor: String,
    },
    Edit {
        review: Box<Review>,
        id: u64,
    },
}

/// One line of a parsed diff.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DLine {
    File(String),
    Meta(String),
    Hunk(String),
    Ctx {
        old: usize,
        new: usize,
        text: String,
    },
    Add {
        new: usize,
        text: String,
    },
    Del {
        old: usize,
        text: String,
    },
}

/// `git diff` output as lines that know their file and line numbers.
fn parse_diff(out: &str) -> Vec<DLine> {
    let mut v = Vec::new();
    let (mut o, mut n) = (0usize, 0usize);
    let mut in_hunk = false;
    for l in out.lines() {
        if let Some(rest) = l.strip_prefix("diff --git ") {
            in_hunk = false;
            // `a/x b/x`: the b side (the new path; renames too).
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, b)| b).to_owned();
            v.push(DLine::File(path));
        } else if let Some(h) = l.strip_prefix("@@ ") {
            in_hunk = true;
            // @@ -a,b +c,d @@
            let mut parts = h.split_whitespace();
            let num = |s: Option<&str>| {
                s.and_then(|s| s[1..].split(',').next()?.parse::<usize>().ok())
                    .unwrap_or(1)
            };
            o = num(parts.next());
            n = num(parts.next());
            v.push(DLine::Hunk(l.to_owned()));
        } else if in_hunk && let Some(t) = l.strip_prefix('+') {
            v.push(DLine::Add {
                new: n,
                text: t.to_owned(),
            });
            n += 1;
        } else if in_hunk && let Some(t) = l.strip_prefix('-') {
            v.push(DLine::Del {
                old: o,
                text: t.to_owned(),
            });
            o += 1;
        } else if in_hunk && let Some(t) = l.strip_prefix(' ') {
            v.push(DLine::Ctx {
                old: o,
                new: n,
                text: t.to_owned(),
            });
            o += 1;
            n += 1;
        } else if in_hunk && l.starts_with('\\') {
            // "\ No newline at end of file"
        } else if !l.starts_with("index ") && !l.starts_with("--- ") && !l.starts_with("+++ ") {
            in_hunk = false;
            v.push(DLine::Meta(l.to_owned()));
        }
    }
    v
}

/// Where each comment shows: the diff line index it hangs under, or
/// outdated (its line is gone).
fn place(lines: &[DLine], comments: &[Comment]) -> Vec<(u64, Option<usize>)> {
    let file_of = |i: usize| {
        lines[..=i].iter().rev().find_map(|l| match l {
            DLine::File(f) => Some(f.as_str()),
            _ => None,
        })
    };
    fn text_of(l: &DLine) -> Option<&str> {
        match l {
            DLine::Ctx { text, .. } | DLine::Add { text, .. } | DLine::Del { text, .. } => {
                Some(text.as_str())
            }
            _ => None,
        }
    }
    comments
        .iter()
        .map(|c| {
            let in_file: Vec<usize> = (0..lines.len())
                .filter(|&i| text_of(&lines[i]).is_some() && file_of(i) == Some(c.file.as_str()))
                .collect();
            let exact = in_file.iter().copied().find(|&i| {
                let at = match &lines[i] {
                    DLine::Ctx { new, .. } | DLine::Add { new, .. } => c.new == Some(*new),
                    DLine::Del { old, .. } => c.new.is_none() && c.old == Some(*old),
                    _ => false,
                };
                at && text_of(&lines[i]) == Some(c.anchor.as_str())
            });
            // Moved: the same text elsewhere in the file's diff (nearest).
            let moved = || {
                let want = c.new.or(c.old).unwrap_or(0);
                in_file
                    .iter()
                    .copied()
                    .filter(|&i| text_of(&lines[i]) == Some(c.anchor.as_str()))
                    .min_by_key(|&i| {
                        let at = match &lines[i] {
                            DLine::Ctx { new, .. } | DLine::Add { new, .. } => *new,
                            DLine::Del { old, .. } => *old,
                            _ => 0,
                        };
                        at.abs_diff(want)
                    })
            };
            (c.id, exact.or_else(moved))
        })
        .collect()
}

struct Builder {
    text: String,
    rows: Vec<Row>,
    tints: Vec<Vec<(usize, usize, Tint)>>,
}

impl Builder {
    fn line(&mut self, parts: &[(&str, Option<Tint>)], row: Row) {
        let mut col = 0;
        let mut tints = Vec::new();
        for (s, t) in parts {
            let n = s.chars().count();
            if let Some(t) = t
                && n > 0
            {
                tints.push((col, col + n, *t));
            }
            col += n;
            self.text.push_str(s);
        }
        self.text.push('\n');
        self.rows.push(row);
        self.tints.push(tints);
    }

    fn comment(&mut self, c: &Comment) {
        let (mark, tint) = match c.state {
            State::Open => ("\u{25cf}", Tint::Warn),
            State::Resolved => ("\u{2713}", Tint::Dim),
        };
        for (k, part) in c.text.lines().enumerate() {
            let lead = if k == 0 {
                format!("            \u{2503} {mark} ")
            } else {
                "            \u{2503}   ".into()
            };
            self.line(
                &[(&lead, Some(tint)), (part, Some(tint))],
                Row::Comment(c.id),
            );
        }
    }
}

/// Comments whose line moved take its new numbers (so exports and the
/// agent's prompt point at the right place). True when any changed.
fn reanchor(diff: &str, store: &mut Store) -> bool {
    let lines = parse_diff(diff);
    let placed = place(&lines, &store.comments);
    let mut changed = false;
    for (id, at) in placed {
        let Some(i) = at else { continue };
        let (new, old) = match &lines[i] {
            DLine::Ctx { new, .. } | DLine::Add { new, .. } => (Some(*new), None),
            DLine::Del { old, .. } => (None, Some(*old)),
            _ => continue,
        };
        if let Some(c) = store.comments.iter_mut().find(|c| c.id == id)
            && (c.new != new || (new.is_none() && c.old != old))
        {
            c.new = new;
            c.old = old;
            changed = true;
        }
    }
    changed
}

fn render(r: &Review, diff: &str, store: &Store) -> (String, GitView) {
    let lines = parse_diff(diff);
    let placed = place(&lines, &store.comments);
    let mut b = Builder {
        text: String::new(),
        rows: Vec::new(),
        tints: Vec::new(),
    };
    let open = store
        .comments
        .iter()
        .filter(|c| c.state == State::Open)
        .count();
    b.line(
        &[
            (&format!(" Review: {}", r.target.describe()), Some(Tint::Head)),
            (
                &format!(
                    "   {open} open, {} resolved · c comment · e edit · x resolve · D delete · ] [ next/prev · m Markdown · a ask the agent · g refresh",
                    store.comments.len() - open
                ),
                Some(Tint::Dim),
            ),
        ],
        Row::None,
    );
    if lines.is_empty() {
        b.line(&[("  No changes.", Some(Tint::Dim))], Row::None);
    }
    let mut file = String::new();
    for (i, l) in lines.iter().enumerate() {
        match l {
            DLine::File(f) => {
                file = f.clone();
                b.line(&[("", None)], Row::None);
                b.line(
                    &[(&format!("\u{258c} {f}"), Some(Tint::Head))],
                    Row::Rev {
                        file: f.clone(),
                        new: None,
                        old: None,
                    },
                );
            }
            DLine::Meta(m) => b.line(&[(&format!("  {m}"), Some(Tint::Dim))], Row::None),
            DLine::Hunk(h) => b.line(&[(&format!("  {h}"), Some(Tint::Accent))], Row::None),
            DLine::Ctx { old, new, text } => b.line(
                &[
                    (&format!("{old:>5} {new:>5}   "), Some(Tint::Dim)),
                    (text, None),
                ],
                Row::Rev {
                    file: file.clone(),
                    new: Some(*new),
                    old: Some(*old),
                },
            ),
            DLine::Add { new, text } => b.line(
                &[
                    (&format!("      {new:>5} + "), Some(Tint::AddNum)),
                    (text, Some(Tint::AddBg)),
                ],
                Row::Rev {
                    file: file.clone(),
                    new: Some(*new),
                    old: None,
                },
            ),
            DLine::Del { old, text } => b.line(
                &[
                    (&format!("{old:>5}       - "), Some(Tint::DelNum)),
                    (text, Some(Tint::DelBg)),
                ],
                Row::Rev {
                    file: file.clone(),
                    new: None,
                    old: Some(*old),
                },
            ),
        }
        for (id, at) in &placed {
            if *at == Some(i)
                && let Some(c) = store.comments.iter().find(|c| c.id == *id)
            {
                b.comment(c);
            }
        }
    }
    let outdated: Vec<&Comment> = placed
        .iter()
        .filter(|(_, at)| at.is_none())
        .filter_map(|(id, _)| store.comments.iter().find(|c| c.id == *id))
        .collect();
    if !outdated.is_empty() {
        b.line(&[("", None)], Row::None);
        b.line(
            &[(
                " Outdated (their lines are no longer in the diff)",
                Some(Tint::Head),
            )],
            Row::None,
        );
        for c in outdated {
            let at = c.new.or(c.old).unwrap_or(0);
            b.line(
                &[(
                    &format!("  {}:{at}  {}", c.file, c.anchor.trim()),
                    Some(Tint::Dim),
                )],
                Row::Comment(c.id),
            );
            b.comment(c);
        }
    }
    (
        b.text,
        GitView {
            what: ViewWhat::Review(Box::new(r.clone())),
            rows: b.rows,
            tints: b.tints,
        },
    )
}

/// The review as Markdown (export, and the agent's prompt).
fn markdown(r: &Review, store: &Store, only_open: bool) -> String {
    let mut s = format!(
        "# Review: {}\n\nRepository: `{}`\n\n",
        r.target.describe(),
        r.root.display()
    );
    let mut any = false;
    for c in &store.comments {
        if only_open && c.state == State::Resolved {
            continue;
        }
        any = true;
        let at = match (c.new, c.old) {
            (Some(n), _) => format!("{}:{n}", c.file),
            (None, Some(o)) => format!("{} (removed line {o})", c.file),
            (None, None) => c.file.clone(),
        };
        let state = match c.state {
            State::Open => "",
            State::Resolved => " (resolved)",
        };
        s.push_str(&format!(
            "- **{at}**{state}: {}\n",
            c.text.replace('\n', "\n  ")
        ));
        if !c.anchor.trim().is_empty() {
            s.push_str(&format!("  ```\n  {}\n  ```\n", c.anchor));
        }
    }
    if !any {
        s.push_str("No comments.\n");
    }
    s
}

impl App {
    /// `:codereview [staged | since REV | branch [BASE] | REV]`
    pub(crate) fn code_review_cmd(&mut self, args: &[&str]) {
        let Some(root) = self.git_root_for_review() else {
            return;
        };
        let rev = |r: &str| -> Result<String, String> {
            git::run(
                &root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{r}^{{commit}}"),
                ],
            )
            .map(|o| String::from_utf8_lossy(&o).trim().to_owned())
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("{r} is not a commit here"))
        };
        let target = match args {
            [] => Ok(Target::WorkTree),
            ["staged"] => Ok(Target::Staged),
            ["since", r] => rev(r).map(Target::Since),
            ["branch"] => {
                let base = ["main", "master", "trunk", "develop"]
                    .into_iter()
                    .find(|b| rev(b).is_ok());
                base.map(|b| Target::Branch(b.to_owned()))
                    .ok_or_else(|| "No main or master branch: :codereview branch BASE".to_owned())
            }
            ["branch", b] => rev(b).map(|_| Target::Branch((*b).to_owned())),
            [r] => rev(r).map(Target::Commit),
            _ => Err("usage: :codereview [staged | since REV | branch [BASE] | REV]".into()),
        };
        let target = match target {
            Ok(t) => t,
            Err(e) => {
                self.editor_mut().message = Some(e);
                return;
            }
        };
        let git_dir = match git::run(&root, &["rev-parse", "--absolute-git-dir"]) {
            Ok(o) => PathBuf::from(String::from_utf8_lossy(&o).trim()),
            Err(e) => {
                self.editor_mut().message = Some(format!("git: {e}"));
                return;
            }
        };
        self.show_review(&Review {
            root,
            git_dir,
            target,
        });
    }

    pub(crate) fn git_root_for_review(&mut self) -> Option<PathBuf> {
        if let DocKind::Git(v) = &self.doc.kind {
            match &v.what {
                ViewWhat::Review(r) => return Some(r.root.clone()),
                ViewWhat::Log { root, .. }
                | ViewWhat::Status { root }
                | ViewWhat::Branches { root }
                | ViewWhat::Stashes { root } => return Some(root.clone()),
                _ => {}
            }
        }
        let start = self
            .doc
            .canon
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.tree().map(|t| t.root.clone()))
            .unwrap_or_else(|| self.project_root());
        let root = git::toplevel(&start).map(|p| crate::doc::canonical(&p));
        if root.is_none() {
            self.editor_mut().message =
                Some("Not inside a git repository (open a file or folder in one)".into());
        }
        root
    }

    /// (Re)draw a review tab, running the diff now.
    fn show_review(&mut self, r: &Review) {
        let args = r.target.diff_args();
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let diff = match git::run(&r.root, &a) {
            Ok(o) => String::from_utf8_lossy(&o).into_owned(),
            Err(e) => {
                self.editor_mut().message =
                    Some(format!("git diff: {}", e.lines().next().unwrap_or("")));
                return;
            }
        };
        let mut store = r.load();
        if reanchor(&diff, &mut store)
            && let Err(e) = r.save(&store)
        {
            self.toast(
                crate::nav::Level::Warn,
                format!("Review comments not updated: {e}"),
            );
        }
        let (text, view) = render(r, &diff, &store);
        let title = format!("review {}", r.target.describe());
        let same = |d: &crate::doc::Doc| matches!(&d.kind, DocKind::Git(v) if matches!(&v.what, ViewWhat::Review(o) if **o == *r));
        match self.find_doc(same) {
            Some(i) => {
                if i != self.active {
                    self.switch_to(i);
                }
                self.replace_git_view_at(i, &title, &text, view);
            }
            None => self.open_git_view(&title, &text, view),
        }
    }

    fn live_review(&self) -> Option<Review> {
        match &self.doc.kind {
            DocKind::Git(v) => match &v.what {
                ViewWhat::Review(r) => Some((**r).clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Plain keys in a review tab. True when consumed.
    pub(crate) fn code_review_key(&mut self, c: char, row: &Row) -> bool {
        let Some(r) = self.live_review() else {
            return false;
        };
        let line_text = |a: &App| {
            a.doc
                .editor
                .buffer
                .line_text(a.doc.editor.cursor.line)
                .chars()
                .skip(14)
                .collect::<String>()
                .trim_end_matches('\n')
                .to_owned()
        };
        match (c, row) {
            ('c', Row::Rev { file, new, old }) if new.is_some() || old.is_some() => {
                let anchor = line_text(self);
                let at = new.or(*old).unwrap_or(0);
                self.open_prompt(
                    crate::prompt::PromptKind::ReviewComment(Draft::New {
                        review: Box::new(r),
                        file: file.clone(),
                        new: *new,
                        old: if new.is_some() { None } else { *old },
                        anchor,
                    }),
                    &format!("Comment on {file}:{at}: "),
                    "",
                );
            }
            ('c', _) => {
                self.editor_mut().message =
                    Some("Put the cursor on a changed or context line to comment".into());
            }
            ('e', Row::Comment(id)) => {
                let text = r
                    .load()
                    .comments
                    .iter()
                    .find(|c| c.id == *id)
                    .map(|c| c.text.clone())
                    .unwrap_or_default();
                self.open_prompt(
                    crate::prompt::PromptKind::ReviewComment(Draft::Edit {
                        review: Box::new(r),
                        id: *id,
                    }),
                    "Edit comment: ",
                    &text,
                );
            }
            ('x', Row::Comment(id)) => self.change_comment(&r, *id, |s, i| {
                let c = &mut s.comments[i];
                c.state = match c.state {
                    State::Open => State::Resolved,
                    State::Resolved => State::Open,
                };
                format!(
                    "Comment {}",
                    match c.state {
                        State::Open => "reopened",
                        State::Resolved => "resolved",
                    }
                )
            }),
            ('D', Row::Comment(id)) => self.change_comment(&r, *id, |s, i| {
                s.comments.remove(i);
                "Comment deleted".into()
            }),
            ('e' | 'x' | 'D', _) => {
                self.editor_mut().message =
                    Some("Put the cursor on a comment (] / [ go to the next / previous)".into());
            }
            (']', _) => self.step_comment(true),
            ('[', _) => self.step_comment(false),
            ('\n', Row::Rev { file, new, .. }) => {
                let path = r.root.join(file);
                self.jump(&path, new.unwrap_or(1).saturating_sub(1), 0);
            }
            ('\n', Row::Comment(id)) => {
                if let Some(c) = r.load().comments.into_iter().find(|c| c.id == *id) {
                    self.jump(
                        &r.root.join(&c.file),
                        c.new.unwrap_or(1).saturating_sub(1),
                        0,
                    );
                }
            }
            ('m', _) => self.export_review(&r),
            ('a', _) => self.review_to_agent(&r),
            ('g', _) => self.show_review(&r),
            _ => return false,
        }
        true
    }

    fn change_comment(&mut self, r: &Review, id: u64, f: impl FnOnce(&mut Store, usize) -> String) {
        let mut s = r.load();
        let Some(i) = s.comments.iter().position(|c| c.id == id) else {
            return;
        };
        let note = f(&mut s, i);
        match r.save(&s) {
            Ok(()) => {
                self.show_review(r);
                self.editor_mut().message = Some(note);
            }
            Err(e) => self.editor_mut().message = Some(format!("Could not save the review: {e}")),
        }
    }

    /// The prompt for a comment was answered.
    pub(crate) fn finish_review_comment(&mut self, d: Draft, input: &str) {
        let text = input.trim();
        let (r, note) = match d {
            Draft::New { .. } if text.is_empty() => {
                self.editor_mut().message = Some("Empty comment: nothing added".into());
                return;
            }
            Draft::New {
                review,
                file,
                new,
                old,
                anchor,
            } => {
                let mut s = review.load();
                let id = s.next.max(1);
                s.next = id + 1;
                s.comments.push(Comment {
                    id,
                    file,
                    new,
                    old,
                    anchor,
                    text: text.to_owned(),
                    state: State::Open,
                });
                (review, s)
            }
            Draft::Edit { review, id } => {
                let mut s = review.load();
                if text.is_empty() {
                    // Emptied: the comment goes.
                    s.comments.retain(|c| c.id != id);
                } else if let Some(c) = s.comments.iter_mut().find(|c| c.id == id) {
                    c.text = text.to_owned();
                } else {
                    return;
                }
                (review, s)
            }
        };
        match r.save(&note) {
            Ok(()) => {
                let line = self.doc.editor.cursor.line;
                self.show_review(&r);
                self.doc.editor.goto(Some(line + 1), None);
                self.editor_mut().message = Some("Comment saved".into());
            }
            Err(e) => self.editor_mut().message = Some(format!("Could not save the comment: {e}")),
        }
    }

    fn step_comment(&mut self, forward: bool) {
        let rows = match &self.doc.kind {
            DocKind::Git(v) => v.rows.clone(),
            _ => return,
        };
        let here = self.doc.editor.cursor.line;
        let is_start = |i: usize| matches!(rows.get(i), Some(Row::Comment(id)) if !matches!(rows.get(i.wrapping_sub(1)), Some(Row::Comment(p)) if p == id));
        let found = if forward {
            (here + 1..rows.len()).find(|&i| is_start(i))
        } else {
            (0..here).rev().find(|&i| is_start(i))
        };
        match found {
            Some(i) => self.doc.editor.goto(Some(i + 1), Some(1)),
            None => self.editor_mut().message = Some("No more comments that way".into()),
        }
    }

    /// `m`: write the review as Markdown next to its comments and open it.
    fn export_review(&mut self, r: &Review) {
        let md = markdown(r, &r.load(), false);
        let path = r.store_path().with_extension("md");
        match std::fs::write(&path, md) {
            Ok(()) => {
                self.open_path(&path);
                self.editor_mut().message = Some(format!("Review written to {}", path.display()));
            }
            Err(e) => self.editor_mut().message = Some(format!("{}: {e}", path.display())),
        }
    }

    /// `a`: ask the agent to address the open comments.
    fn review_to_agent(&mut self, r: &Review) {
        let s = r.load();
        if !s.comments.iter().any(|c| c.state == State::Open) {
            self.editor_mut().message = Some("No open comments to send".into());
            return;
        }
        let msg = format!(
            "Please address these code review comments, then say what you changed for each.\n\n{}",
            markdown(r, &s, true)
        );
        self.open_agent_panel(&msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs
index 1..2 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,3 @@
 fn a() {
-    old();
+    new();
 }
";

    #[test]
    fn diff_lines_know_their_numbers_and_comments_follow_moved_lines() {
        let l = parse_diff(DIFF);
        assert_eq!(l[0], DLine::File("src/a.rs".into()));
        assert!(l.contains(&DLine::Del {
            old: 2,
            text: "    old();".into()
        }));
        assert!(l.contains(&DLine::Add {
            new: 2,
            text: "    new();".into()
        }));
        let c = |new, anchor: &str| Comment {
            id: 1,
            file: "src/a.rs".into(),
            new,
            old: None,
            anchor: anchor.into(),
            text: "why?".into(),
            state: State::Open,
        };
        // Exact place, then the same text on another line, then outdated.
        let add = l.iter().position(|x| matches!(x, DLine::Add { .. }));
        assert_eq!(place(&l, &[c(Some(2), "    new();")])[0].1, add);
        assert_eq!(place(&l, &[c(Some(9), "    new();")])[0].1, add);
        assert_eq!(place(&l, &[c(Some(2), "gone")])[0].1, None);
    }
}
