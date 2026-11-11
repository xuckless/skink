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
