//! End-to-end git features against a real temporary repository.

use super::*;
use crate::gitui::Marks;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use skink_core::Buffer;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("skink-git-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.name", "T"]);
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("f.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "first"]);
    crate::doc::canonical(&dir)
}

fn open(dir: &Path) -> App {
    let mut a = App::new(
        Editor::new(Buffer::open(&dir.join("f.txt")).unwrap()),
        Keymap::defaults(),
        Backend::Local,
    );
    a.icons = crate::icons::IconSet::Ascii;
    // No wake callback: git jobs answer synchronously.
    a.poll_git();
    a.poll_git();
    a
}

fn hunks(a: &mut App) -> (usize, usize) {
    let h = a.fresh_hunks().cloned().unwrap_or_default();
    (h.unstaged.len(), h.staged.len())
}

fn key(a: &mut App, c: char) {
    a.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

#[test]
fn marks_stage_unstage_and_revert_a_hunk() {
    let dir = repo("hunks");
    let mut a = open(&dir);
    assert!(matches!(a.doc.git.marks, Marks::Ready(_)));
    assert_eq!(hunks(&mut a), (0, 0));

    // Edit line 2: one modified hunk, drawn in the gutter.
    a.doc.editor.goto(Some(2), None);
    a.doc.editor.move_line_end(false);
    a.doc.editor.insert_text("!");
    assert_eq!(hunks(&mut a), (1, 0));
    assert_eq!(
        a.git_gutter(&[0, 1, 2])[1].map(|m| m.0),
        Some("\u{258c}"),
        "modified: thick bar"
    );

    a.stage_hunk();
    a.poll_git();
    a.poll_git();
    assert_eq!(
        git(&dir, &["diff", "--cached", "--numstat"]).trim(),
        "1\t1\tf.txt"
    );
    assert_eq!(hunks(&mut a), (0, 1));

    a.unstage_hunk();
    a.poll_git();
    a.poll_git();
    assert_eq!(git(&dir, &["diff", "--cached"]), "");
    assert_eq!(hunks(&mut a), (1, 0));

    // The peek's `r` reverts in the buffer, as one undo step.
    a.peek_hunk();
    key(&mut a, 'r');
    assert_eq!(a.doc.editor.buffer.text(), "one\ntwo\nthree\nfour\n");
    assert_eq!(hunks(&mut a), (0, 0));

    // Deleted lines and next / previous.
    a.doc.editor.goto(Some(4), None);
    a.run_chain("CutLine");
    a.doc.editor.goto(Some(1), None);
    a.doc.editor.insert_text("zero\n");
    assert_eq!(hunks(&mut a), (2, 0));
    a.doc.editor.goto(Some(1), None);
    a.next_hunk();
    assert_eq!(a.doc.editor.cursor.line, 4);
    a.next_hunk();
    assert_eq!(a.doc.editor.cursor.line, 0);
}

#[test]
fn panel_commit_draft_branches_and_tree_colours() {
    let dir = repo("panel");
    std::fs::write(dir.join("new.txt"), "n\n").unwrap();
    let mut a = open(&dir);
    a.doc.editor.insert_text("top\n");
    a.run_chain("Save");

    a.git_panel();
    a.poll_git();
    let text = a.doc.editor.buffer.text();
    assert!(text.contains("On main"), "{text}");
    assert!(
        text.contains("Changes (1)") && text.contains("Untracked (1)"),
        "{text}"
    );

    // `s` on the f.txt row stages it.
    let row = text.lines().position(|l| l.ends_with("f.txt")).unwrap();
    a.doc.editor.goto(Some(row + 1), None);
    key(&mut a, 's');
    a.poll_git();
    let text = a.doc.editor.buffer.text();
    assert!(text.contains("Staged (1)"), "{text}");

    // The message tab: saving it commits.
    key(&mut a, 'c');
    assert!(a.doc.editor.buffer.text().contains("Staged:"));
    a.doc.editor.insert_text("second commit");
    a.run_chain("Save");
    a.poll_git();
    assert_eq!(
        git(&dir, &["log", "-1", "--format=%s"]).trim(),
        "second commit"
    );
    assert!(matches!(a.git.draft, crate::gitui::Draft::None));

    // Branch from the command line, listed in the branches view.
    a.run_command("branch topic");
    a.poll_git();
    assert_eq!(git(&dir, &["branch", "--show-current"]).trim(), "topic");
    a.branches_cmd();
    assert!(a.doc.editor.buffer.text().contains("\u{25cf} topic"));

    // Tree colours: the untracked file.
    let st = crate::git::status(&dir, true).unwrap();
    let m = crate::git::TreeMarks::new(&dir, &st);
    assert_eq!(
        m.get(&dir.join("new.txt")),
        Some(crate::git::FileState::Untracked)
    );
    // ...and not by colour alone: the row ends in a status letter.
    a.git.tree = crate::gitui::TreeStatus::Ready(std::sync::Arc::new(m));
    a.set_tree_root(dir.clone(), true);
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
    t.draw(|f| a.render(f)).unwrap();
    let area = a.areas.tree;
    let buf = t.backend().buffer();
    let rows: Vec<String> = (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right().saturating_sub(1))
                .map(|x| buf[(x, y)].symbol())
                .collect()
        })
        .collect();
    assert!(
        rows.iter()
            .any(|r| r.contains("new.txt") && r.trim_end().ends_with('U')),
        "{rows:#?}"
    );
}

#[test]
fn side_by_side_diff_and_blame_view() {
    let dir = repo("diff");
    let mut a = open(&dir);
    a.doc.editor.goto(Some(3), None);
    a.doc.editor.insert_text("THREE ");
    a.diff_cmd(&[]);
    let text = a.doc.editor.buffer.text();
    let row = text.lines().find(|l| l.contains("THREE")).unwrap();
    assert!(row.contains("\u{2502}") && row.contains("three"), "{text}");

    a.switch_to(0);
    a.blame_view();
    a.poll_git();
    let text = a.doc.editor.buffer.text();
    assert!(text.lines().next().unwrap().contains(" T "), "{text}");
    assert!(text.contains("not committed"), "{text}");
}

#[test]
fn blame_of_a_crlf_file_finds_its_commits() {
    // v1.0 B: the LF buffer was blamed against the CRLF blob, so every line
    // said "not committed".
    let dir = repo("crlf");
    git(&dir, &["config", "core.autocrlf", "false"]);
    std::fs::write(dir.join("w.txt"), "one\r\ntwo\r\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "crlf"]);
    let mut a = open(&dir);
    a.open_path(&dir.join("w.txt"));
    a.poll_git();
    a.poll_git();
    a.blame_view();
    a.poll_git();
    let text = a.doc.editor.buffer.text();
    assert!(text.lines().next().unwrap().contains(" T "), "{text}");
    assert!(!text.contains("not committed"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conflicts_puts_every_tab_on_its_block_so_ours_works_in_each() {
    // v1.0 B: only the last-opened file's cursor was on its conflict; in the
    // other tabs :ours said "not inside a merge conflict".
    let dir = repo("conflicts");
    for f in ["a.txt", "b.txt"] {
        std::fs::write(dir.join(f), "x\nbase\ny\n").unwrap();
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "other"]);
    for f in ["a.txt", "b.txt"] {
        std::fs::write(dir.join(f), "x\ntheirs\ny\n").unwrap();
    }
    git(&dir, &["commit", "-q", "-am", "theirs"]);
    git(&dir, &["checkout", "-q", "main"]);
    for f in ["a.txt", "b.txt"] {
        std::fs::write(dir.join(f), "x\nours\ny\n").unwrap();
    }
    git(&dir, &["commit", "-q", "-am", "ours"]);
    let _ = Command::new("git")
        .args(["merge", "-q", "other"])
        .current_dir(&dir)
        .output();
    let mut a = open(&dir);
    a.run_command("conflicts");
    for f in ["a.txt", "b.txt"] {
        a.open_path(&dir.join(f)); // switch tabs; the cursor stays put
        a.run_command("ours");
        assert_eq!(
            a.doc.editor.buffer.text(),
            "x\nours\ny\n",
            "{f}: {:?}",
            a.doc.editor.message
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_read_only_tracked_file_says_why_blame_is_off() {
    // v1.0 B: a tracked binary (opened read-only) said "blame needs a file
    // tracked by git" / "not a file tracked by git".
    let dir = repo("ro");
    std::fs::write(dir.join("b.dat"), [0u8, 159, 146, 150, 0, 1]).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "bin"]);
    let mut a = open(&dir);
    a.open_path(&dir.join("b.dat"));
    a.poll_git();
    assert!(a.doc.editor.buffer.is_read_only());
    a.blame_view();
    let msg = a.doc.editor.message.clone().unwrap_or_default();
    assert!(msg.contains("read-only"), "{msg}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_corrupt_index_shows_gits_error_not_loading_or_a_bogus_file() {
    // v1.0 B: with a truncated .git/index the panel said "loading…" forever
    // and :conflicts opened git's error message as a file.
    let dir = repo("corrupt");
    let idx = dir.join(".git/index");
    let bytes = std::fs::read(&idx).unwrap();
    std::fs::write(&idx, &bytes[..20]).unwrap();
    let mut a = open(&dir);
    a.git_panel();
    a.poll_git();
    let text = a.doc.editor.buffer.text();
    assert!(text.contains("git status failed"), "{text}");
    a.switch_to(0);
    let tabs = a.tabs.len();
    a.run_command("conflicts");
    assert_eq!(a.tabs.len(), tabs, "no tab opened for git's error text");
    let msg = a.doc.editor.message.clone().unwrap_or_default();
    assert!(msg.starts_with("git: "), "{msg}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn review_comments_are_kept_resolved_exported_and_survive_a_restart() {
    // v1.1: :codereview over a diff, comments in .git/skink-review.
    let dir = repo("review");
    std::fs::write(dir.join("f.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    let mut a = open(&dir);
    a.run_command("codereview");
    assert!(a.doc.editor.buffer.text().contains("uncommitted changes"));
    // Comment on the added line.
    let row = a
        .doc
        .editor
        .buffer
        .text()
        .lines()
        .position(|l| l.ends_with("+ TWO"))
        .expect("the added line is shown");
    a.doc.editor.goto(Some(row + 1), Some(1));
    key(&mut a, 'c');
    for c in "why upper case?".chars() {
        key(&mut a, c);
    }
    a.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let shown = a.doc.editor.buffer.text();
    assert!(shown.contains("\u{25cf} why upper case?"), "{shown}");
    assert!(shown.contains("1 open, 0 resolved"), "{shown}");
    let store = dir.join(".git/skink-review/worktree.json");
    assert!(
        std::fs::read_to_string(&store)
            .unwrap()
            .contains("why upper case?")
    );

    // A new Skink finds it again; x resolves it.
    let mut b = open(&dir);
    b.run_command("codereview");
    let row = b
        .doc
        .editor
        .buffer
        .text()
        .lines()
        .position(|l| l.contains("why upper case?"))
        .expect("kept across restarts");
    b.doc.editor.goto(Some(row + 1), Some(1));
    key(&mut b, 'x');
    assert!(b.doc.editor.buffer.text().contains("0 open, 1 resolved"));
    // The line moves (a line added above): the comment follows it.
    std::fs::write(dir.join("f.txt"), "zero\none\nTWO\nthree\nfour\n").unwrap();
    key(&mut b, 'g');
    let shown = b.doc.editor.buffer.text();
    let at = shown.lines().position(|l| l.ends_with("+ TWO")).unwrap();
    assert!(
        shown
            .lines()
            .nth(at + 1)
            .is_some_and(|l| l.contains("why upper case?")),
        "{shown}"
    );
    // m writes Markdown next to the comments and opens it.
    key(&mut b, 'm');
    let md = std::fs::read_to_string(dir.join(".git/skink-review/worktree.md")).unwrap();
    assert!(
        md.contains("**f.txt:3** (resolved): why upper case?"),
        "{md}"
    );
    assert!(
        b.doc
            .canon
            .as_deref()
            .is_some_and(|p| p.ends_with("worktree.md"))
    );
    // The work tree only has the user's own change.
    assert_eq!(git(&dir, &["status", "--porcelain"]).trim(), "M f.txt");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_log_previews_the_commit_under_the_cursor_beside_it() {
    // v1.1: :log with a live diff preview in another pane.
    let dir = repo("log");
    std::fs::write(dir.join("f.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    git(&dir, &["commit", "-q", "-am", "add five"]);
    let mut a = open(&dir);
    a.run_command("log");
    assert!(a.panes.is_split(), "the preview sits beside the list");
    let list = a.doc.editor.buffer.text();
    assert!(
        list.lines().next().is_some_and(|l| l.contains("add five")),
        "{list}"
    );
    a.poll_git(); // tests answer synchronously
    let preview = |a: &App| {
        let i = a
            .find_doc(|d| d.editor.buffer.title().as_deref() == Some("commit preview"))
            .expect("preview tab");
        a.doc_at(i).unwrap().editor.buffer.text()
    };
    assert!(preview(&a).contains("+five"), "{}", preview(&a));
    // Down: the preview follows to the first commit.
    a.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    a.poll_git();
    a.poll_git();
    assert!(preview(&a).contains("first"), "{}", preview(&a));
    assert!(!preview(&a).contains("+five"));
    // c reviews that commit.
    key(&mut a, 'c');
    assert!(a.doc.editor.buffer.text().contains("Review: commit "));
    let _ = std::fs::remove_dir_all(&dir);
}
