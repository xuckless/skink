//! `:log [PATH]`: the commit list with a live preview. Moving through the
//! list shows the selected commit's diff in a pane beside it; Enter opens
//! the commit in its own tab, c reviews it (`:codereview`).

use crate::{
    App,
    doc::DocKind,
    git,
    gitui::{GitView, Row, Tint, ViewWhat},
    panes::Axis,
};
use std::path::{Path, PathBuf};

const PREVIEW_TITLE: &str = "commit preview";
const LIMIT: &str = "2000";

/// The preview pane beside a log tab.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LogPreview {
    #[default]
    Off,
    /// `git show` for this commit is running.
    Loading {
        sha: String,
    },
    Showing {
        sha: String,
    },
}

/// Colours for `git show` output.
pub(crate) fn diff_tints(text: &str) -> Vec<Vec<(usize, usize, Tint)>> {
    let mut in_body = false;
    text.lines()
        .map(|l| {
            let n = l.chars().count();
            let t = if l.starts_with("diff --git") {
                in_body = true;
                Some(Tint::Head)
            } else if (l.starts_with("commit ") && !in_body) || l.starts_with("@@") {
                Some(Tint::Accent)
            } else if in_body && l.starts_with('+') && !l.starts_with("+++") {
                Some(Tint::Add)
            } else if in_body && l.starts_with('-') && !l.starts_with("---") {
                Some(Tint::Del)
            } else if !in_body
                && (l.starts_with("Author") || l.starts_with("Date") || l.starts_with("Commit"))
            {
                Some(Tint::Dim)
            } else {
                None
            };
            t.map(|t| vec![(0, n, t)]).unwrap_or_default()
        })
        .collect()
}

impl App {
    /// `:log [PATH]`
    pub(crate) fn log_cmd(&mut self, args: &[&str]) {
        let Some(root) = self.git_root_for_review() else {
            return;
        };
        let path = (!args.is_empty()).then(|| PathBuf::from(args.join(" ")));
        let mut a = vec![
            "log".to_owned(),
            "--no-color".into(),
            "-n".into(),
            LIMIT.into(),
            "--format=%H%x1f%h%x1f%ar%x1f%an%x1f%s".into(),
        ];
        if let Some(p) = &path {
            a.push("--".into());
            a.push(p.display().to_string());
        }
        let a: Vec<&str> = a.iter().map(String::as_str).collect();
        let out = match git::run(&root, &a) {
            Ok(o) => String::from_utf8_lossy(&o).into_owned(),
            Err(e) => {
                self.editor_mut().message =
                    Some(format!("git log: {}", e.lines().next().unwrap_or("")));
                return;
            }
        };
        let (text, view) = log_view(&out, &root, path.as_deref());
        if view.rows.iter().all(|r| !matches!(r, Row::Commit(_))) {
            self.editor_mut().message = Some("No commits".into());
            return;
        }
        let title = match &path {
            Some(p) => format!("log {}", p.display()),
            None => "log".into(),
        };
        self.open_git_view(&title, &text, view);
        let log_tab = self.active;
        // The preview beside it: its own tab, in another pane.
        let preview = self.open_view(
            PREVIEW_TITLE,
            "Move through the log to see a commit here.\n",
        );
        let preview_tab = self.find_doc(|d| d.id == preview).unwrap_or(log_tab);
        self.switch_to(log_tab);
        let log_pane = self.panes.focused();
        if self.panes.is_split() {
            let other = self.panes.cycle(1);
            self.focus_pane(other);
        } else {
            self.split(Axis::Columns);
        }
        self.switch_to(preview_tab);
        self.focus_pane(log_pane);
        self.switch_to(log_tab);
        self.git_log_preview = LogPreview::Off;
        self.follow_log_preview();
        self.editor_mut().message = Some(
            "Up/Down show each commit beside the list · Enter opens it · c reviews it · Ctrl-W switches pane".into(),
        );
    }

    /// Between events: keep the preview on the commit under the cursor.
    pub(crate) fn follow_log_preview(&mut self) {
        let DocKind::Git(v) = &self.doc.kind else {
            return;
        };
        let ViewWhat::Log { root, .. } = &v.what else {
            return;
        };
        let Some(Row::Commit(sha)) = v.rows.get(self.doc.editor.cursor.line) else {
            return;
        };
        let current = match &self.git_log_preview {
            LogPreview::Loading { sha } | LogPreview::Showing { sha } => Some(sha.as_str()),
            LogPreview::Off => None,
        };
        if current == Some(sha.as_str()) {
            return;
        }
        // Only while the preview tab is still open.
        if self
            .find_doc(|d| d.editor.buffer.title().as_deref() == Some(PREVIEW_TITLE))
            .is_none()
        {
            return;
        }
        let (root, sha) = (root.clone(), sha.clone());
        self.git_log_preview = LogPreview::Loading { sha: sha.clone() };
        self.git_spawn_apply(move || {
            let got = git::run(
                &root,
                &[
                    "show",
                    "--no-color",
                    "--stat",
                    "--patch",
                    "--format=fuller",
                    &sha,
                ],
            )
            .map(|o| String::from_utf8_lossy(&o).into_owned());
            Box::new(move |app: &mut App| app.show_log_preview(&sha, got))
        });
    }

    fn show_log_preview(&mut self, sha: &str, got: Result<String, String>) {
        // A newer commit was selected meanwhile: that answer comes later.
        if !matches!(&self.git_log_preview, LogPreview::Loading { sha: s } if s == sha) {
            return;
        }
        let text = got.unwrap_or_else(|e| format!("git show {sha} failed:\n{e}"));
        let Some(i) = self.find_doc(|d| d.editor.buffer.title().as_deref() == Some(PREVIEW_TITLE))
        else {
            self.git_log_preview = LogPreview::Off;
            return;
        };
        let tints = diff_tints(&text);
        let rows = vec![Row::None; tints.len()];
        if let Some(d) = self.doc_at_mut(i) {
            d.replace_view(PREVIEW_TITLE, &text);
            d.kind = DocKind::Git(Box::new(GitView {
                what: ViewWhat::Diff,
                rows,
                tints,
            }));
        }
        self.git_log_preview = LogPreview::Showing {
            sha: sha.to_owned(),
        };
    }

    /// Keys of a log tab beyond Enter (which opens the commit).
    pub(crate) fn log_key(&mut self, c: char, row: &Row) -> bool {
        match (c, row) {
            ('c', Row::Commit(sha)) => {
                let sha = sha.clone();
                self.code_review_cmd(&[&sha]);
                true
            }
            _ => false,
        }
    }
}

fn log_view(out: &str, root: &Path, path: Option<&Path>) -> (String, GitView) {
    let mut text = String::new();
    let mut rows = Vec::new();
    let mut tints = Vec::new();
    let mut push = |parts: &[(&str, Option<Tint>)], row: Row| {
        let mut col = 0;
        let mut t = Vec::new();
        for (s, tint) in parts {
            let n = s.chars().count();
            if let Some(tint) = tint
                && n > 0
            {
                t.push((col, col + n, *tint));
            }
            col += n;
            text.push_str(s);
        }
        text.push('\n');
        rows.push(row);
        tints.push(t);
    };
    for l in out.lines() {
        let f: Vec<&str> = l.split('\u{1f}').collect();
        let [full, short, when, who, subject] = f[..] else {
            continue;
        };
        let who: String = who.chars().take(16).collect();
        push(
            &[
                (&format!(" {short} "), Some(Tint::Accent)),
                (&format!("{when:<15} "), Some(Tint::Dim)),
                (&format!("{who:<16} "), Some(Tint::Mod)),
                (subject, None),
            ],
            Row::Commit(full.to_owned()),
        );
    }
    (
        text,
        GitView {
            what: ViewWhat::Log {
                root: root.to_path_buf(),
                path: path.map(Path::to_path_buf),
            },
            rows,
            tints,
        },
    )
}
