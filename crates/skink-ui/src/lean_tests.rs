//! Lean mode ($EDITOR for git, Claude Code / Codex Ctrl-G) and the nested
//! hand-off, driven through the app.
use super::*;
use ratatui::{
    Terminal,
    backend::TestBackend,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
};

fn app(text: &str) -> App {
    let mut a = App::new(
        Editor::new(Buffer::from_text(text)),
        Keymap::defaults(),
        Backend::Local,
    );
    a.icons = crate::icons::IconSet::Ascii;
    a
}

fn screen(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| app.render(f)).unwrap();
    let buf = t.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect()
}

fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.handle_key(KeyEvent::new(code, mods));
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        press(app, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

fn cmd(a: &mut App, line: &str) {
    press(a, KeyCode::Char('e'), KeyModifiers::CONTROL);
    type_str(a, line);
    press(a, KeyCode::Enter, KeyModifiers::NONE);
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skink-lean-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn commit_lean() -> lean::Lean {
    lean::Lean {
        kind: lean::LeanKind::GitMessage(lean::GitMsg::Commit),
        why: lean::Why::GitFile,
    }
}

#[test]
fn lean_process_cq_exits_1_and_ctrl_q_on_a_clean_file_exits_0() {
    let mut a = app("\n# Please enter the commit message\n");
    a.set_launch(lean::LaunchMode::Lean(commit_lean()));
    let s = screen(&mut a, 80, 10);
    assert!(
        s[9].contains(":cq cancel") && s[9].contains("commit message"),
        "{s:?}"
    );
    assert_eq!(a.rulers(), vec![51, 73], "commit rulers by default");
    press(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(a.should_quit());
    assert_eq!(a.exit_status(), Exit::Done);

    let mut a = app("msg");
    a.set_launch(lean::LaunchMode::Lean(commit_lean()));
    type_str(&mut a, "edited");
    cmd(&mut a, "cq");
    assert!(a.should_quit(), "no save prompt");
    assert_eq!(a.exit_status(), Exit::Cancelled);
}

#[test]
fn lean_process_leaves_the_state_folder_alone() {
    // No session, recents or crash backups: the user's real ones stay put.
    let dir = temp_dir("state");
    let file = dir.join("COMMIT_EDITMSG");
    std::fs::write(&file, "\n").unwrap();
    let mut a = App::new(
        Editor::new(Buffer::open(&file).unwrap()),
        Keymap::defaults(),
        Backend::Local,
    );
    let sd = dir.join("state");
    a.state_dir = Some(sd.clone());
    a.set_launch(lean::LaunchMode::Lean(commit_lean()));
    type_str(&mut a, "fix: a thing");
    assert!(!a.backup_due(), "no crash backup of a commit message");
    a.write_backups();
    press(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
    a.shutdown();
    assert!(!sd.exists(), "the state folder was written");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "fix: a thing\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rebase_list_cycles_the_verb_under_the_cursor() {
    let mut a = app("pick 1a2b3c first\npick 4d5e6f second\n# Commands:\n");
    a.set_launch(lean::LaunchMode::Lean(lean::Lean {
        kind: lean::LeanKind::RebaseTodo,
        why: lean::Why::GitFile,
    }));
    press(
        &mut a,
        KeyCode::Char('K'),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    );
    assert_eq!(a.doc.editor.buffer.line_text(0), "reword 1a2b3c first");
    cmd(&mut a, "rebase f");
    assert_eq!(a.doc.editor.buffer.line_text(0), "fixup 1a2b3c first");
    press(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(
        a.doc.editor.buffer.line_text(0),
        "reword 1a2b3c first",
        "one undo step each"
    );
}

#[test]
fn nested_skink_hands_its_file_to_the_parent_and_waits_for_the_tab() {
    let dir = temp_dir("handoff");
    let msg = dir.join("COMMIT_EDITMSG");
    std::fs::write(&msg, "\n# comment\n").unwrap();
    let mut a = app("");
    let server = handoff::Server::start_tcp(Arc::new(|| {})).unwrap();
    let (ep, token) = (server.endpoint.clone(), server.token.clone());
    a.handoff = handoff::ServerState::Listening(server);
    let ask = |path: PathBuf| {
        let (ep, token) = (ep.clone(), token.clone());
        std::thread::spawn(move || {
            handoff::ask(
                &ep,
                &handoff::Request {
                    token,
                    path,
                    line: None,
                    col: None,
                    mode: lean::LaunchMode::Lean(commit_lean()),
                },
            )
            .unwrap()
        })
    };
    let wait_open = |a: &mut App| {
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while a.live_lean().is_none() && std::time::Instant::now() < end {
            a.poll_background();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(a.live_lean().is_some(), "the handed-off file did not open");
    };

    // :cq answers Cancelled (the child exits 1, git aborts).
    let child = ask(msg.clone());
    wait_open(&mut a);
    assert_eq!(a.doc.title(), "COMMIT_EDITMSG");
    type_str(&mut a, "unsaved");
    cmd(&mut a, "cq");
    assert_eq!(child.join().unwrap(), handoff::Reply::Cancelled);
    assert!(!a.should_quit(), "the parent keeps running");
    assert!(a.live_lean().is_none());

    // Saving and closing the tab answers Done, even as the last document.
    let child = ask(msg.clone());
    wait_open(&mut a);
    while a.doc_count() > 1 {
        let other = (0..a.tabs.len())
            .find(|&i| i != a.cur_doc && a.is_doc(i))
            .unwrap();
        a.close_tab(other);
    }
    type_str(&mut a, "feat: x");
    press(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert_eq!(child.join().unwrap(), handoff::Reply::Done);
    assert!(
        !a.should_quit(),
        "closing the handed-off tab never quits the parent"
    );
    assert!(
        std::fs::read_to_string(&msg)
            .unwrap()
            .starts_with("feat: x")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
