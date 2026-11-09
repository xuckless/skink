use super::*;
use ratatui::{
    Terminal,
    backend::TestBackend,
    crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
};
use skink_core::Buffer;

fn app(text: &str) -> App {
    let mut a = App::new(
        Editor::new(Buffer::from_text(text)),
        Keymap::defaults(),
        Backend::Local,
    );
    a.icons = crate::icons::IconSet::Ascii; // plain text so assertions are readable
    a
}

fn screen(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| app.render(f)).unwrap();
    let buf = t.backend().buffer();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// (revision, text) the live or parked doc's syntax tree was built from.
fn parsed(d: &Doc) -> Option<(u64, &str)> {
    match &d.syntax.as_ref()?.shown {
        doc::Shown::Tree { rev, text } => Some((*rev, text)),
        doc::Shown::Nothing => None,
    }
}

fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.handle_key(KeyEvent::new(code, mods));
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        press(app, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

#[test]
fn renders_gutter_text_and_status() {
    let mut a = app("hello\nworld");
    let s = screen(&mut a, 60, 5);
    assert!(s[0].contains("[scratch]"), "{s:?}");
    assert_eq!(s[1], " 1 hello");
    assert_eq!(s[2], " 2 world");
    assert!(s[4].contains("EDIT") && s[4].contains("1:1"), "{s:?}");
}

#[test]
fn typing_edits_and_marks_dirty() {
    let mut a = app("");
    type_str(&mut a, "hi");
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    type_str(&mut a, "x");
    let s = screen(&mut a, 40, 5);
    assert_eq!(s[1], " 1 hi");
    assert_eq!(s[2], " 2 x");
    assert!(s[0].contains("[scratch]") && s[0].contains('*'), "{s:?}");
}

#[test]
fn undo_redo_via_keys() {
    let mut a = app("");
    type_str(&mut a, "abc");
    press(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(a.doc.editor.buffer.text(), "");
    press(&mut a, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(a.doc.editor.buffer.text(), "abc");
}

#[test]
fn shift_arrows_select_and_typing_replaces() {
    let mut a = app("hello");
    press(&mut a, KeyCode::Right, KeyModifiers::SHIFT);
    press(&mut a, KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(a.doc.editor.selected_text().as_deref(), Some("he"));
    type_str(&mut a, "J");
    assert_eq!(a.doc.editor.buffer.text(), "Jllo");
}

#[test]
fn tab_indents_or_inserts_via_fallback_chain() {
    let mut a = app("x");
    press(&mut a, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(a.doc.editor.buffer.text(), "\tx");
    press(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(a.doc.editor.buffer.text(), "x");
}

#[test]
fn quit_clean_exits_dirty_prompts() {
    let mut a = app("x");
    press(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(a.should_quit());

    let mut a = app("x");
    type_str(&mut a, "y");
    press(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(!a.should_quit());
    assert!(
        screen(&mut a, 80, 4)[3]
            .trim_start()
            .starts_with("Save changes before quitting?")
    );
    press(&mut a, KeyCode::Esc, KeyModifiers::NONE); // cancel
    assert!(!a.should_quit());
    press(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Char('n'), KeyModifiers::NONE); // discard
    assert!(a.should_quit());
}

#[test]
fn save_as_prompt_writes_file() {
    let dir = std::env::temp_dir().join(format!("skink-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("out.txt");
    let mut a = app("");
    type_str(&mut a, "data");
    press(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL); // no path: asks
    assert!(
        screen(&mut a, 80, 4)[3]
            .trim_start()
            .starts_with("Save as:")
    );
    type_str(&mut a, path.to_str().unwrap());
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "data");
    assert!(!a.doc.editor.buffer.is_dirty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn find_prompt_selects_match() {
    let mut a = app("one two three");
    press(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut a, "two");
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(a.doc.editor.selected_text().as_deref(), Some("two"));
}

#[test]
fn goto_command_bar() {
    let mut a = app("a\nb\nc\nd");
    press(&mut a, KeyCode::Char('l'), KeyModifiers::CONTROL); // Micro: command-edit:goto
    assert!(screen(&mut a, 40, 4)[3].trim_start().starts_with(":goto"));
    type_str(&mut a, "3");
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(a.doc.editor.cursor.line, 2);
}

#[test]
fn page_down_scrolls() {
    let text: String = (1..=50).map(|i| format!("line{i}\n")).collect();
    let mut a = app(&text);
    let _ = screen(&mut a, 20, 12); // 10 text rows between title and status
    press(&mut a, KeyCode::PageDown, KeyModifiers::NONE);
    press(&mut a, KeyCode::PageDown, KeyModifiers::NONE);
    let s = screen(&mut a, 20, 12);
    assert!(s[10].ends_with("line19"), "{s:?}");
}

#[test]
fn release_events_are_ignored() {
    let mut a = app("x");
    let mut ev = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
    ev.kind = KeyEventKind::Release;
    a.handle_key(ev);
    assert!(!a.should_quit());
}

#[test]
fn horizontal_scroll_keeps_cursor_visible() {
    let mut a = app("0123456789abcdefghij");
    press(&mut a, KeyCode::End, KeyModifiers::NONE);
    let s = screen(&mut a, 13, 3); // gutter " 1 " + 10 text cols
    assert_eq!(s[1], " 1 bcdefghij");
}

#[test]
fn bracketed_paste_inserts_text() {
    let mut a = app("");
    a.handle_paste("a\r\nb");
    assert_eq!(a.doc.editor.buffer.text(), "a\nb");
}

#[test]
fn mouse_click_moves_cursor_and_wheel_scrolls() {
    let text: String = (1..=50).map(|i| format!("line{i}\n")).collect();
    let mut a = app(&text);
    let _ = screen(&mut a, 30, 12);
    let click = |kind, column, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    a.handle_mouse(click(MouseEventKind::Down(MouseButton::Left), 4 + 3, 5)); // title bar offsets rows by one; gutter is 4 wide (" 51 ")
    assert_eq!((a.doc.editor.cursor.line, a.doc.editor.cursor.col), (4, 3));
    a.handle_mouse(click(MouseEventKind::ScrollDown, 5, 5));
    assert_eq!(a.doc.editor.scroll, 3);
}

#[cfg(unix)]
#[test]
fn terminal_tab_runs_a_command_and_shows_output() {
    let mut a = app("text");
    a.open_terminal(CommandSpec::new("sh").args(["-c", "printf 'pane-ok\\n'; sleep 5"]))
        .unwrap();
    assert_eq!(a.tabs.len(), 2);
    assert!(a.active_is_term());
    let _ = screen(&mut a, 60, 20); // lay out the tab
    let got = a
        .active_term()
        .unwrap()
        .term
        .wait_for(std::time::Duration::from_secs(5), |s| {
            s.text().contains("pane-ok")
        });
    assert!(got);
    let s = screen(&mut a, 60, 20);
    assert!(s.iter().any(|l| l.trim() == "pane-ok"), "{s:?}");
    assert!(s[0].contains("sh"), "tab bar shows the terminal tab: {s:?}");
    // F12 returns to the file tab; typing edits the buffer.
    press(&mut a, KeyCode::F(12), KeyModifiers::NONE);
    assert!(!a.active_is_term());
    type_str(&mut a, "X");
    assert!(a.doc.editor.buffer.text().starts_with('X'));
    // F12 again goes back to the same terminal (no second one is spawned).
    press(&mut a, KeyCode::F(12), KeyModifiers::NONE);
    assert!(a.active_is_term());
    assert_eq!(a.tabs.len(), 2);
    a.close_terminal();
    assert_eq!(a.tabs.len(), 1);
    assert!(!a.active_is_term());
}

#[cfg(unix)]
#[test]
fn several_terminal_tabs_keep_their_own_titles() {
    let mut a = app("");
    a.open_terminal(
        CommandSpec::new("sh").args(["-c", "printf '\\033]0;first-title\\007'; sleep 5"]),
    )
    .unwrap();
    a.open_terminal(
        CommandSpec::new("sh").args(["-c", "printf '\\033]0;second-title\\007'; sleep 5"]),
    )
    .unwrap();
    for i in 1..=2 {
        let _ = screen(&mut a, 100, 10);
        let want = if i == 1 {
            "first-title"
        } else {
            "second-title"
        };
        a.switch_to(i);
        let ok = a
            .active_term()
            .unwrap()
            .term
            .wait_for(std::time::Duration::from_secs(5), |s| s.title == want);
        assert!(ok, "{want}");
    }
    let s = screen(&mut a, 100, 10);
    assert!(s[0].contains("second-title"), "{s:?}");
    a.switch_to(1);
    let s = screen(&mut a, 100, 10);
    assert!(
        s[0].contains("first-title") && s[0].contains("second-title"),
        "{s:?}"
    );
}

#[test]
fn file_tree_shows_and_opens_files() {
    let dir = std::env::temp_dir().join(format!("skink-ui-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.txt"), "from a\n").unwrap();
    std::fs::write(dir.join(".hidden"), "").unwrap();
    let mut a = app("");
    a.set_tree_root(dir.clone(), true);
    let s = screen(&mut a, 80, 12);
    assert!(s.iter().any(|l| l.contains("Files")), "{s:?}");
    assert!(s.iter().any(|l| l.contains("src")), "{s:?}");
    assert!(s.iter().any(|l| l.contains("1 hidden items")), "{s:?}");
    assert!(s[11].contains("TREE"), "{s:?}");
    // Enter expands src, Down selects a.txt, Enter opens it in the editor.
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    press(&mut a, KeyCode::Down, KeyModifiers::NONE);
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(a.doc.editor.buffer.text(), "from a\n");
    let s = screen(&mut a, 80, 12);
    assert!(s[11].contains("EDIT"), "{s:?}");
    assert!(s[0].contains("a.txt"), "{s:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn opening_from_tree_adds_tabs_and_keeps_unsaved_work() {
    let dir = std::env::temp_dir().join(format!("skink-ui-tree2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    let mut a = app("");
    a.set_tree_root(dir.clone(), true);
    press(&mut a, KeyCode::F(8), KeyModifiers::NONE); // tree -> editor
    type_str(&mut a, "x");
    press(&mut a, KeyCode::F(8), KeyModifiers::NONE); // editor -> tree
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE); // open b.txt
    assert_eq!(
        a.tabs.len(),
        2,
        "a second tab, the unsaved scratch stays open"
    );
    assert_eq!(a.doc.editor.buffer.text(), "b\n");
    let s = screen(&mut a, 80, 10);
    assert!(
        s[0].contains("[scratch]") && s[0].contains("b.txt"),
        "{s:?}"
    );
    // Back to the first tab: the unsaved text is still there.
    press(&mut a, KeyCode::Char('1'), KeyModifiers::ALT);
    assert_eq!(a.doc.editor.buffer.text(), "x");
    // Opening b.txt again just selects its tab.
    a.open_path(&dir.join("b.txt"));
    assert_eq!(a.tabs.len(), 2);
    assert_eq!(a.doc.editor.buffer.text(), "b\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tab_cycle_and_close() {
    let mut a = app("one");
    a.new_scratch_tab();
    type_str(&mut a, "two");
    a.new_scratch_tab();
    assert_eq!(a.tabs.len(), 3);
    press(&mut a, KeyCode::PageUp, KeyModifiers::CONTROL);
    assert_eq!(a.doc.editor.buffer.text(), "two");
    press(&mut a, KeyCode::PageDown, KeyModifiers::CONTROL);
    press(&mut a, KeyCode::PageDown, KeyModifiers::CONTROL); // wraps to the first
    assert_eq!(a.doc.editor.buffer.text(), "one");
    // Ctrl-Q closes the clean tab instead of quitting.
    press(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(!a.should_quit());
    assert_eq!(a.tabs.len(), 2);
    // Closing a dirty tab asks first.
    assert_eq!(a.doc.editor.buffer.text(), "two");
    press(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(screen(&mut a, 80, 5)[4].contains("closing"));
    press(&mut a, KeyCode::Char('n'), KeyModifiers::NONE);
    assert_eq!(a.tabs.len(), 1);
    press(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(a.should_quit(), "closing the last file tab quits");
}

fn cmd(a: &mut App, line: &str) {
    press(a, KeyCode::Char('e'), KeyModifiers::CONTROL);
    type_str(a, line);
    press(a, KeyCode::Enter, KeyModifiers::NONE);
}

#[test]
fn help_opens_as_a_tab_and_topics_filter() {
    let mut a = app("work");
    cmd(&mut a, "help");
    assert_eq!(a.tabs.len(), 2);
    assert!(a.doc.editor.buffer.is_read_only());
    let text = a.doc.editor.buffer.text();
    assert!(text.contains("## Tabs") && text.contains("## Commands") && text.contains("Alt-p"));
    let s = screen(&mut a, 90, 10);
    assert!(s[0].contains("Help"), "{s:?}");
    cmd(&mut a, "help conflict");
    assert_eq!(a.tabs.len(), 3);
    let t = a.doc.editor.buffer.text();
    assert!(
        t.starts_with("## Merge conflicts") && !t.contains("## Tabs"),
        "{t}"
    );
    // Views are read-only: typing does not change them.
    type_str(&mut a, "zzz");
    assert!(!a.doc.editor.buffer.text().contains("zzz"));
}

#[test]
fn every_documented_command_is_wired() {
    // Each command in the table must not report "Unknown command".
    let mut a = app("x");
    for d in commands::COMMANDS {
        if matches!(
            d.name,
            "run" | "git" | "term" | "qa" | "quit" | "reload" | "settings" | "bindings"
        ) {
            continue; // needs arguments or touches the user's config / quits
        }
        a.doc.editor.message = None;
        a.run_command(d.name);
        let m = a.doc.editor.message.clone().unwrap_or_default();
        assert!(!m.starts_with("Unknown command"), "{}: {m}", d.name);
    }
    a.run_command("nonsense");
    assert!(
        a.doc
            .editor
            .message
            .as_deref()
            .is_some_and(|m| m.contains("Unknown command") && m.contains(":help"))
    );
}

#[test]
fn command_line_runs_actions_by_name_with_history_and_completion() {
    let mut a = app("abc");
    cmd(&mut a, "SelectAll");
    assert_eq!(a.doc.editor.selected_text().as_deref(), Some("abc"));
    // Tab completes a unique prefix.
    press(&mut a, KeyCode::Char('e'), KeyModifiers::CONTROL);
    type_str(&mut a, "gotoo");
    press(&mut a, KeyCode::Backspace, KeyModifiers::NONE);
    press(&mut a, KeyCode::Backspace, KeyModifiers::NONE);
    press(&mut a, KeyCode::Tab, KeyModifiers::NONE);
    assert!(
        screen(&mut a, 60, 4)[3].contains(":goto"),
        "{:?}",
        screen(&mut a, 60, 4)
    );
    press(&mut a, KeyCode::Esc, KeyModifiers::NONE);
    // History: Up recalls the previous line.
    press(&mut a, KeyCode::Char('e'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Up, KeyModifiers::NONE);
    assert!(screen(&mut a, 60, 4)[3].contains("SelectAll"));
}

#[test]
fn run_output_opens_in_a_tab() {
    if cfg!(windows) {
        return;
    }
    let mut a = app("");
    cmd(&mut a, "run echo skink-out");
    assert_eq!(a.tabs.len(), 2);
    assert!(
        a.doc.editor.buffer.text().contains("running"),
        "placeholder first"
    );
    // The command runs in the background; deliver its result.
    for done in a.jobs.wait_all() {
        if let Some(i) = a.find_doc(|d| d.id == done.doc) {
            a.doc_at_mut(i)
                .unwrap()
                .replace_view(&done.title, &done.text);
        }
    }
    let t = a.doc.editor.buffer.text();
    assert!(
        t.starts_with("$ echo skink-out\n\nskink-out\n") && !t.contains("running"),
        "{t}"
    );
    assert!(screen(&mut a, 60, 6)[0].contains("$ echo skink-out"));
    // Running it again reuses the tab.
    cmd(&mut a, "run echo skink-out");
    assert_eq!(a.tabs.len(), 2);
    a.jobs.wait_all();
}

#[test]
fn conflict_commands_resolve_blocks() {
    let mut a = app("a\n<<<<<<< HEAD\nmine\n=======\nyours\n>>>>>>> b\nz\n");
    cmd(&mut a, "nextconflict");
    assert_eq!(a.doc.editor.cursor.line, 1);
    // Colors: ours/theirs rows differ from the plain background.
    let mut t = Terminal::new(TestBackend::new(40, 10)).unwrap();
    t.draw(|f| a.render(f)).unwrap();
    let buf = t.backend().buffer();
    let bg = |y: u16| buf[(10, y)].bg;
    assert_ne!(bg(3), bg(1), "ours row is tinted");
    assert_ne!(bg(5), bg(3), "theirs differs from ours");
    cmd(&mut a, "theirs");
    assert_eq!(a.doc.editor.buffer.text(), "a\nyours\nz\n");
    cmd(&mut a, "ours");
    assert!(
        a.doc
            .editor
            .message
            .as_deref()
            .is_some_and(|m| m.contains("not inside"))
    );
}

#[test]
fn quick_open_lists_tabs_and_selects() {
    let dir = std::env::temp_dir().join(format!("skink-ui-qo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/alpha.rs"), "alpha\n").unwrap();
    std::fs::write(dir.join("src/beta.rs"), "beta\n").unwrap();
    std::fs::write(dir.join("Cargo.toml"), "").unwrap();
    let mut a = app("");
    a.state_dir = None;
    a.open_path(&dir.join("src/alpha.rs"));
    press(&mut a, KeyCode::Char('p'), KeyModifiers::ALT);
    let s = screen(&mut a, 80, 20);
    assert!(s.iter().any(|l| l.contains("Quick open")), "{s:?}");
    assert!(
        s.iter()
            .any(|l| l.contains("alpha.rs") && l.contains("open")),
        "{s:?}"
    );
    type_str(&mut a, "beta");
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(a.doc.editor.buffer.text(), "beta\n");
    assert_eq!(a.tabs.len(), 2);
    // Esc closes it without doing anything.
    press(&mut a, KeyCode::Char('p'), KeyModifiers::ALT);
    press(&mut a, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!matches!(a.overlay, Overlay::Picker(_)));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn settings_open_as_a_file_and_apply() {
    let dir = std::env::temp_dir().join(format!("skink-ui-cfg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut a = app("");
    a.config_dir = Some(dir.clone());
    a.state_dir = None;
    cmd(&mut a, "settings");
    assert!(dir.join("settings.json").exists());
    assert!(a.doc.editor.buffer.text().contains("\"tabsize\""));
    // Edit, save, reload: the new tab size applies.
    std::fs::write(
        dir.join("settings.json"),
        "{\"tabsize\": 2, \"icons\": \"ascii\"}",
    )
    .unwrap();
    cmd(&mut a, "reload");
    assert_eq!(a.doc.editor.tab_width, 2);
    cmd(&mut a, "bindings");
    assert!(dir.join("bindings.json").exists());
    std::fs::write(dir.join("bindings.json"), "{\"Ctrl-j\": \"SelectAll\"}").unwrap();
    cmd(&mut a, "reload");
    press(&mut a, KeyCode::Char('j'), KeyModifiers::CONTROL);
    assert!(a.doc.editor.selection().is_some() || a.doc.editor.buffer.len_chars() == 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn syntax_highlighting_colors_keywords() {
    let dir = std::env::temp_dir().join(format!("skink-ui-hl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("a.rs");
    std::fs::write(&f, "fn main() { let x = \"hi\"; }\n").unwrap();
    let mut a = app("");
    a.open_path(&f);
    let mut t = Terminal::new(TestBackend::new(60, 6)).unwrap();
    t.draw(|f| a.render(f)).unwrap();
    let buf = t.backend().buffer();
    // gutter " 1 " (3 cells): `fn` starts at x = 3.
    let kw = buf[(3, 1)].fg;
    let plain = buf[(8, 1)].fg; // `main`
    let string = buf[(22, 1)].fg;
    assert_ne!(
        kw, plain,
        "keyword colored differently from the function name"
    );
    assert_ne!(string, kw);
    assert_eq!(buf[(3, 1)].symbol(), "f");
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- regressions from the review ----------------------------------------

#[cfg(unix)]
#[test]
fn keys_never_reach_a_hidden_document() {
    // Old bug: tree focus -> Esc while a terminal tab was active sent keys
    // to the (invisible) live document.
    let mut a = app("doc");
    a.open_terminal(CommandSpec::new("sh").args(["-c", "sleep 5"]))
        .unwrap();
    a.set_tree_root(std::env::temp_dir(), true);
    press(&mut a, KeyCode::Esc, KeyModifiers::NONE); // leave the tree
    type_str(&mut a, "zz");
    assert_eq!(a.doc.editor.buffer.text(), "doc");
    assert!(a.active_is_term());
}

#[cfg(unix)]
#[test]
fn f10_reaches_programs_in_a_terminal_but_f12_switches() {
    let mut a = app("doc");
    a.open_terminal(CommandSpec::new("sh").args(["-c", "sleep 5"]))
        .unwrap();
    press(&mut a, KeyCode::F(10), KeyModifiers::NONE); // Micro binds F10 to Quit
    assert!(!a.should_quit() && a.active_is_term());
    press(&mut a, KeyCode::F(12), KeyModifiers::NONE);
    assert!(!a.active_is_term());
}

#[cfg(unix)]
#[test]
fn terminal_status_line_is_about_the_terminal() {
    let mut a = app("x\ny\nz");
    a.doc.editor.goto(Some(3), Some(1));
    a.open_terminal(CommandSpec::new("sh").args(["-c", "sleep 5"]))
        .unwrap();
    let s = screen(&mut a, 80, 6);
    assert!(
        s[5].contains("TERM") && !s[5].contains("3:1") && !s[5].contains("LF"),
        "{s:?}"
    );
}

#[test]
fn switching_tabs_keeps_syntax_and_cursor_state() {
    let dir = std::env::temp_dir().join(format!("skink-ui-keep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    std::fs::write(dir.join("b.rs"), "fn c() {}\n").unwrap();
    let mut a = app("");
    a.open_path(&dir.join("a.rs"));
    a.doc.editor.goto(Some(2), Some(4));
    let _ = screen(&mut a, 60, 6); // parses a.rs
    let parsed_at = parsed(&a.doc).expect("parsed").0;
    a.open_path(&dir.join("b.rs"));
    a.open_path(&dir.join("a.rs"));
    assert_eq!(
        parsed(&a.doc).map(|p| p.0),
        Some(parsed_at),
        "no reparse after a tab switch"
    );
    assert_eq!((a.doc.editor.cursor.line, a.doc.editor.cursor.col), (1, 3));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn views_are_reused_by_title() {
    let mut a = app("");
    cmd(&mut a, "help");
    cmd(&mut a, "help");
    assert_eq!(a.tabs.len(), 2);
    cmd(&mut a, "help tabs");
    assert_eq!(a.tabs.len(), 3);
}

#[test]
fn closing_the_active_tab_selects_its_right_neighbour() {
    let mut a = app("one");
    a.new_scratch_tab();
    type_str(&mut a, "two");
    a.new_scratch_tab();
    type_str(&mut a, "three");
    a.switch_to(1);
    a.close_tab(1);
    assert_eq!(a.tabs.len(), 2);
    assert_eq!(a.active, 1);
    assert_eq!(a.doc.editor.buffer.text(), "three");
    a.close_tab(1);
    assert_eq!(a.active, 0);
    assert_eq!(a.doc.editor.buffer.text(), "one");
}

#[test]
fn save_on_close_asks_for_a_name_then_closes() {
    let dir = std::env::temp_dir().join(format!("skink-ui-saveclose-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut a = app("keep");
    a.new_scratch_tab();
    type_str(&mut a, "new text");
    press(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Char('y'), KeyModifiers::NONE);
    let target = dir.join("saved.txt");
    type_str(&mut a, target.to_str().unwrap());
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new text");
    assert_eq!(a.tabs.len(), 1, "the tab closed after saving");
    assert_eq!(a.doc.editor.buffer.text(), "keep");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reload_drops_removed_user_bindings() {
    let dir = std::env::temp_dir().join(format!("skink-ui-rebind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut a = app("abc");
    a.config_dir = Some(dir.clone());
    a.state_dir = None;
    std::fs::write(dir.join("bindings.json"), r#"{"Ctrl-a": "CursorEnd"}"#).unwrap();
    cmd(&mut a, "reload");
    press(&mut a, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(a.doc.editor.selection().is_none(), "override active");
    assert!(commands_help(&mut a).contains("CursorEnd   (bindings.json)"));
    std::fs::write(dir.join("bindings.json"), "{}").unwrap();
    cmd(&mut a, "reload");
    a.switch_to(0);
    press(&mut a, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(
        a.doc.editor.selection().is_some(),
        "default SelectAll is back"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

fn commands_help(a: &mut App) -> String {
    cmd(a, "help key bindings");
    a.doc.editor.buffer.text()
}

#[test]
fn project_root_prefers_the_repository_over_a_member_crate() {
    let d = std::env::temp_dir().join(format!("skink-ui-root-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join(".git")).unwrap();
    std::fs::create_dir_all(d.join("crates/a/src")).unwrap();
    std::fs::write(d.join("crates/a/Cargo.toml"), "").unwrap();
    assert_eq!(find_project_root(&d.join("crates/a/src")), d);
    // Without git, the nearest manifest wins.
    std::fs::remove_dir_all(d.join(".git")).unwrap();
    assert_eq!(
        find_project_root(&d.join("crates/a/src")),
        d.join("crates/a")
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn chords_work_while_the_tree_has_focus() {
    // Regression: after `skink .` the tree swallowed Ctrl-E / Ctrl-Q, so the
    // command line could not be opened and the editor could not be quit.
    let mut a = app("doc");
    a.set_tree_root(std::env::temp_dir(), true);
    assert!(a.focus() == Focus::Tree);
    press(&mut a, KeyCode::Char('e'), KeyModifiers::CONTROL);
    assert!(
        matches!(a.overlay, Overlay::Prompt(_)),
        "Ctrl-E opens the command line from the tree"
    );
    press(&mut a, KeyCode::Esc, KeyModifiers::NONE);
    // Plain letters still belong to the tree, not the document.
    type_str(&mut a, "zz");
    assert_eq!(a.doc.editor.buffer.text(), "doc");
    press(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(a.should_quit());
}

#[test]
fn background_tabs_drop_their_parse_tree() {
    let dir = std::env::temp_dir().join(format!("skink-ui-park-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (a_rs, b_rs) = (dir.join("a.rs"), dir.join("b.rs"));
    std::fs::write(&a_rs, "fn a() {}\n".repeat(8000)).unwrap(); // 80 KB: big enough to park
    std::fs::write(&b_rs, "fn b() {}\n").unwrap();
    let mut a = app("");
    a.open_path(&a_rs);
    screen(&mut a, 60, 10); // parses a.rs
    assert!(parsed(&a.doc).is_some());
    let first = a.active;
    a.open_path(&b_rs);
    screen(&mut a, 60, 10);
    let parked = a.doc_at(first).unwrap();
    assert!(parsed(parked).is_none(), "tree and text dropped");
    // Switching back re-parses and colours again.
    a.switch_to(first);
    screen(&mut a, 60, 10);
    assert!(parsed(&a.doc).unwrap().1.contains("fn a"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tree_creates_nested_file_opens_it_and_ctrl_b_hides_the_sidebar() {
    let dir = std::env::temp_dir().join(format!("skink-ui-new-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut a = app("");
    a.set_tree_root(dir.clone(), true);
    press(&mut a, KeyCode::Char('a'), KeyModifiers::NONE);
    type_str(&mut a, "src/new.rs");
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    let made = doc::canonical(&dir.join("src/new.rs"));
    assert!(made.is_file());
    assert_eq!(
        a.doc.canon.as_deref(),
        Some(made.as_path()),
        "new file is the live tab"
    );
    assert_eq!(a.tree().unwrap().selected_node().unwrap().name, "new.rs");
    press(&mut a, KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert_eq!(a.sidebar_view(), Some(SidebarView::Hidden));
    assert!(!screen(&mut a, 80, 8).iter().any(|l| l.contains("Files")));
    press(&mut a, KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert_eq!(a.sidebar_view(), Some(SidebarView::Shown));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn shift_wheel_scrolls_sideways_until_the_cursor_moves() {
    let long = format!("{}END\nshort", "x".repeat(100));
    let mut a = app(&long);
    let _ = screen(&mut a, 40, 4);
    let wheel = |a: &mut App, kind| {
        a.handle_mouse(MouseEvent {
            kind,
            column: 20,
            row: 1,
            modifiers: KeyModifiers::SHIFT,
        })
    };
    for _ in 0..20 {
        wheel(&mut a, MouseEventKind::ScrollDown);
    }
    let s = screen(&mut a, 40, 4);
    assert!(s.iter().any(|l| l.contains("END")), "{s:?}");
    assert_eq!(a.doc.editor.cursor.col, 0, "cursor did not move");
    // Clamped to the longest visible line, not unbounded.
    assert!(a.doc.hview.offset <= 103);
    // Moving the cursor brings the view back to it.
    press(&mut a, KeyCode::Right, KeyModifiers::NONE);
    let s = screen(&mut a, 40, 4);
    assert!(!s.iter().any(|l| l.contains("END")), "{s:?}");
}

#[test]
fn matching_bracket_skips_strings_with_syntax_and_counts_without() {
    let dir = std::env::temp_dir().join(format!("skink-ui-br-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rs = dir.join("a.rs");
    std::fs::write(&rs, "fn a() { let s = \"}\"; }\n").unwrap();
    let mut a = app("");
    a.open_path(&rs);
    let _ = screen(&mut a, 60, 6); // parse
    a.doc.editor.goto(Some(1), Some(8)); // on '{'
    press(&mut a, KeyCode::Char(']'), KeyModifiers::CONTROL);
    assert_eq!(
        a.doc.editor.cursor.col, 22,
        "tree ignores the '}}' in the string"
    );
    // Plain text: counting, cursor just after the closing bracket.
    let mut b = app("(a [b] c)");
    b.doc.editor.goto(Some(1), Some(10));
    press(&mut b, KeyCode::Char(']'), KeyModifiers::CONTROL);
    assert_eq!(b.doc.editor.cursor.col, 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn extra_cursors_are_drawn_and_typing_reaches_them() {
    let mut a = app("ab\ncd");
    a.doc.editor.toggle_cursor_at(1, 0); // Alt-click on line 2
    type_str(&mut a, "x");
    assert_eq!(a.doc.editor.buffer.text(), "xab\nxcd");
    let mut t = Terminal::new(TestBackend::new(40, 6)).unwrap();
    t.draw(|f| a.render(f)).unwrap();
    // One extra cursor block besides the terminal cursor.
    let reversed = t
        .backend()
        .buffer()
        .content()
        .iter()
        .filter(|c| c.modifier.contains(ratatui::style::Modifier::REVERSED))
        .count();
    assert_eq!(reversed, 1);
    assert!(screen(&mut a, 40, 6).iter().any(|l| l.contains("×2")));
}

#[test]
fn find_bar_searches_live_counts_and_esc_returns() {
    let mut a = app("one two\ntwo three\nTwo");
    a.doc.editor.goto(Some(1), Some(5));
    press(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut a, "two");
    // Smart case: "two" also matches "Two"; the first match after the cursor.
    assert_eq!(a.doc.editor.selected_text().as_deref(), Some("two"));
    assert_eq!(a.doc.editor.message.as_deref(), Some("1 of 3"));
    press(&mut a, KeyCode::Char('c'), KeyModifiers::ALT); // case-sensitive
    assert_eq!(a.doc.editor.message.as_deref(), Some("1 of 2"));
    press(&mut a, KeyCode::Char('r'), KeyModifiers::ALT); // regex
    type_str(&mut a, "+");
    assert_eq!(a.doc.editor.message.as_deref(), Some("1 of 2"));
    press(&mut a, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(a.doc.editor.selection(), None);
    assert_eq!((a.doc.editor.cursor.line, a.doc.editor.cursor.col), (0, 4));
}

fn disk_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skink-disk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Make sure the next write gets a different mtime on coarse filesystems.
fn touch_later(p: &Path, text: &str) {
    std::thread::sleep(std::time::Duration::from_millis(15));
    std::fs::write(p, text).unwrap();
}

#[test]
fn external_changes_reload_clean_files_and_ask_for_dirty_ones() {
    let d = disk_dir("watch");
    let f = d.join("a.txt");
    std::fs::write(&f, "one\n").unwrap();
    let mut a = app("");
    a.open_path(&f);
    touch_later(&f, "two\n");
    a.check_disk(true);
    assert_eq!(a.doc.editor.buffer.text(), "two\n", "clean buffer reloads");
    assert!(!a.doc.editor.buffer.is_dirty());

    type_str(&mut a, "X");
    touch_later(&f, "three\n");
    a.check_disk(true);
    assert!(
        matches!(&a.overlay, Overlay::Prompt(p) if p.kind == crate::prompt::PromptKind::Choice(crate::prompt::Choice::DiskChanged)),
        "dirty buffer asks"
    );
    press(&mut a, KeyCode::Char('t'), KeyModifiers::NONE);
    assert_eq!(a.doc.editor.buffer.text(), "three\n");
    press(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(
        a.doc.editor.buffer.text(),
        "Xtwo\n",
        "taking the disk version is undoable"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn save_asks_before_overwriting_a_file_changed_on_disk() {
    let d = disk_dir("overwrite");
    let f = d.join("a.txt");
    std::fs::write(&f, "one\n").unwrap();
    let mut a = app("");
    a.open_path(&f);
    type_str(&mut a, "X");
    touch_later(&f, "theirs\n");
    press(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "theirs\n",
        "not written yet"
    );
    press(&mut a, KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "Xone\n");
    assert!(!a.doc.editor.buffer.is_dirty());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn unsaved_edits_survive_a_crash() {
    let d = disk_dir("backup");
    let f = d.join("a.txt");
    std::fs::write(&f, "saved\n").unwrap();
    let mut a = app("");
    a.state_dir = Some(d.join("state"));
    a.open_path(&f);
    type_str(&mut a, "unsaved ");
    assert!(a.backup_due());
    a.write_backups();
    assert!(!a.backup_due(), "written once per revision");
    drop(a); // crash: no discard_backups

    let mut b = app("");
    b.state_dir = Some(d.join("state"));
    b.open_path(&f);
    assert!(
        matches!(&b.overlay, Overlay::Prompt(p) if matches!(p.kind, crate::prompt::PromptKind::Choice(crate::prompt::Choice::Recover { .. })))
    );
    press(&mut b, KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(b.doc.editor.buffer.text(), "unsaved saved\n");
    assert!(b.doc.editor.buffer.is_dirty());
    press(&mut b, KeyCode::Char('s'), KeyModifiers::CONTROL);
    b.write_backups(); // clean now: the backup goes away
    assert_eq!(
        std::fs::read_dir(d.join("state/backup")).unwrap().count(),
        0
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn large_files_open_without_syntax_or_lsp() {
    let d = disk_dir("large");
    let f = d.join("big.rs");
    std::fs::write(&f, "fn a() {}\n".repeat(crate::disk::LARGE_CHARS / 10 + 1)).unwrap();
    let mut a = app("");
    a.open_path(&f);
    assert_eq!(a.doc.size, crate::disk::SizeClass::Large);
    assert!(a.doc.syntax.is_none() && !a.doc.is_lsp_file());
    assert!(
        a.doc
            .editor
            .message
            .as_deref()
            .unwrap()
            .starts_with("Large file")
    );
    a.doc.editor.message = None;
    assert!(screen(&mut a, 100, 5)[4].contains("LARGE"));
    std::fs::remove_dir_all(&d).unwrap();
}

#[cfg(unix)]
#[test]
fn saving_a_read_only_file_offers_sudo() {
    use std::os::unix::fs::PermissionsExt;
    let d = disk_dir("sudo");
    let f = d.join("a.txt");
    std::fs::write(&f, "x\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o444)).unwrap();
    let mut a = app("");
    a.open_path(&f);
    assert!(a.doc.editor.message.as_deref().unwrap().contains("sudo"));
    type_str(&mut a, "y");
    press(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(
        matches!(&a.overlay, Overlay::Prompt(p) if p.kind == crate::prompt::PromptKind::Choice(crate::prompt::Choice::SudoSave))
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn big_files_parse_on_a_worker_and_keep_old_colours_meanwhile() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let woke = Arc::new(AtomicUsize::new(0));
    let w = woke.clone();
    let wake: skink_term::WakeFn = Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    });
    let mut syn = doc::Syntax::for_test(skink_syntax::Lang::Rust);
    let big = "fn a() {}\n".repeat(10_000);
    let mut buf = skink_core::Buffer::from_text("");
    buf.insert(0, &big);
    let r1 = buf.revision();
    syn.refresh(&buf, Some(&wake));
    assert!(matches!(syn.job, doc::ParseJob::Running { rev, .. } if rev == r1));
    while woke.load(Ordering::SeqCst) == 0 {
        std::thread::yield_now();
    }
    syn.refresh(&buf, Some(&wake));
    assert_eq!(syn.current(r1).map(str::len), Some(big.len()));
    // An edit: the old tree stays shown while the (incremental) parse runs.
    buf.insert(buf.len_chars(), "fn b() {}\n");
    syn.refresh(&buf, Some(&wake));
    assert!(matches!(syn.shown, doc::Shown::Tree { rev, .. } if rev == r1));
    assert!(matches!(syn.job, doc::ParseJob::Running { rev, .. } if rev == r1 + 1));
}

#[test]
fn incremental_parse_matches_a_fresh_parse() {
    let mut syn = doc::Syntax::for_test(skink_syntax::Lang::Rust);
    let mut buf = skink_core::Buffer::from_text("fn a() {\n    let x = 1;\n}\n");
    syn.refresh(&buf, None);
    buf.insert(13, "\"str // ");
    buf.remove(0, 3);
    buf.insert(0, "pub fn ");
    syn.refresh(&buf, None); // incremental from the journal
    let text = buf.text();
    let inc = syn.hl.highlights(&text, 0, 3);
    let mut fresh = skink_syntax::Highlighter::new(skink_syntax::Lang::Rust);
    fresh.update(&text);
    assert_eq!(inc, fresh.highlights(&text, 0, 3));
}

#[test]
fn renaming_in_the_tree_moves_open_tabs_and_folders_are_remembered() {
    let d = disk_dir("treeops");
    std::fs::create_dir_all(d.join("src/deep")).unwrap();
    std::fs::write(d.join("src/a.rs"), "fn a() {}\n").unwrap();
    let state = d.join("state");
    let mut a = app("");
    a.state_dir = Some(state.clone());
    a.open_path(&d.join("src/a.rs"));
    a.set_tree_root(d.clone(), true);
    // The open file is revealed and selected; rename it into a new folder.
    assert_eq!(a.tree().unwrap().selected_node().unwrap().name, "a.rs");
    press(&mut a, KeyCode::Char('r'), KeyModifiers::NONE);
    for _ in 0.."src/a.rs".len() {
        press(&mut a, KeyCode::Backspace, KeyModifiers::NONE);
    }
    type_str(&mut a, "lib/b.rs");
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    let moved = doc::canonical(&d.join("lib/b.rs"));
    assert!(moved.is_file());
    assert_eq!(a.doc.canon.as_ref(), Some(&moved), "the open tab follows");
    type_str(&mut a, "x"); // tree still focused: plain keys stay in the tree
    press(&mut a, KeyCode::Esc, KeyModifiers::NONE);
    a.doc.editor.insert_text("// moved\n");
    press(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(
        std::fs::read_to_string(&moved)
            .unwrap()
            .starts_with("// moved")
    );

    a.shutdown();
    let mut b = app("");
    b.state_dir = Some(state);
    b.set_tree_root(d.clone(), true);
    let names: Vec<_> = b
        .tree()
        .unwrap()
        .nodes
        .iter()
        .map(|n| n.name.clone())
        .collect();
    assert!(
        names.contains(&"b.rs".to_string()),
        "lib/ re-opened: {names:?}"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn expand_and_shrink_selection_by_syntax() {
    let d = disk_dir("expand");
    let f = d.join("a.rs");
    std::fs::write(&f, "fn main() { foo(1, 22); }\n").unwrap();
    let mut a = app("");
    a.open_path(&f);
    a.doc.editor.goto(Some(1), Some(21)); // inside 22
    let sel = |a: &App| a.doc.editor.selected_text().unwrap_or_default();
    press(&mut a, KeyCode::Char('o'), KeyModifiers::ALT);
    assert_eq!(sel(&a), "22");
    press(&mut a, KeyCode::Char('o'), KeyModifiers::ALT);
    assert_eq!(sel(&a), "(1, 22)");
    press(&mut a, KeyCode::Char('o'), KeyModifiers::ALT);
    assert_eq!(sel(&a), "foo(1, 22)");
    press(&mut a, KeyCode::Char('i'), KeyModifiers::ALT);
    assert_eq!(sel(&a), "(1, 22)");
    press(&mut a, KeyCode::Char('i'), KeyModifiers::ALT);
    press(&mut a, KeyCode::Char('i'), KeyModifiers::ALT);
    assert_eq!(sel(&a), "", "back to the cursor");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn find_bar_replaces_inside_the_selection() {
    let mut a = app("x1\nx2\nx3\n");
    a.doc.editor.goto(Some(2), Some(1));
    press(&mut a, KeyCode::Down, KeyModifiers::SHIFT);
    press(&mut a, KeyCode::End, KeyModifiers::SHIFT); // lines 2-3 selected
    press(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    for _ in 0..10 {
        press(&mut a, KeyCode::Backspace, KeyModifiers::NONE);
    }
    type_str(&mut a, "x");
    assert_eq!(
        a.doc.editor.message.as_deref(),
        Some("1 of 2"),
        "counts in the selection"
    );
    press(&mut a, KeyCode::Tab, KeyModifiers::NONE);
    type_str(&mut a, "y");
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(a.doc.editor.buffer.text(), "x1\ny2\ny3\n");
    assert_eq!(a.doc.editor.message.as_deref(), Some("replaced 2 matches"));
    // Alt-S switches to the whole file.
    press(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
    a.doc.editor.goto(Some(2), Some(1));
    press(&mut a, KeyCode::Down, KeyModifiers::SHIFT);
    press(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Char('s'), KeyModifiers::ALT);
    assert!(a.doc.editor.message.as_deref().unwrap().ends_with("of 3"));
}

#[test]
fn long_lines_show_a_horizontal_thumb() {
    let mut a = app(&format!("{}\nshort\n\n\n", "x".repeat(200)));
    let s = screen(&mut a, 60, 6);
    assert!(s[4].contains('\u{2581}'), "{s:#?}");
    let mut b = app("short\nlines\n\n\n");
    let s = screen(&mut b, 60, 6);
    assert!(!s.iter().any(|l| l.contains('\u{2581}')), "{s:#?}");
}

#[test]
fn the_tree_scrolls_sideways_for_long_names() {
    let d = disk_dir("treeh");
    std::fs::write(d.join(format!("{}END.txt", "long_".repeat(12))), "").unwrap();
    let mut a = app("");
    a.set_tree_root(d.clone(), true);
    let row = |a: &mut App| screen(a, 80, 6)[3].clone();
    assert!(!row(&mut a).contains("END"));
    let tree = a.areas.tree;
    for _ in 0..10 {
        a.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollRight,
            column: tree.x + 2,
            row: tree.y + 3,
            modifiers: KeyModifiers::NONE,
        });
    }
    assert!(row(&mut a).contains("END"), "{}", row(&mut a));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn zoom_pane_shows_only_the_text() {
    let mut a = app("one\ntwo");
    let normal = screen(&mut a, 40, 4);
    assert!(normal[3].contains("EDIT"));
    press(&mut a, KeyCode::Char('z'), KeyModifiers::ALT);
    let z = screen(&mut a, 40, 4);
    assert!(
        z[0].contains("one") && !z.iter().any(|l| l.contains("EDIT")),
        "{z:#?}"
    );
    press(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(
        screen(&mut a, 40, 4)[3].contains("Find"),
        "prompts still show"
    );
}

#[test]
fn clicking_the_tree_focuses_it_so_dot_toggles_dotfiles() {
    // Owner report: nothing showed which pane had the keys, and `.` meant
    // for the tree went into the document.
    let d = disk_dir("focus");
    std::fs::write(d.join(".env"), "x").unwrap();
    std::fs::write(d.join("a.rs"), "fn a() {}\n").unwrap();
    let mut a = app("doc");
    a.set_tree_root(d.clone(), true);
    a.focus_main();
    let before = screen(&mut a, 80, 10);
    assert!(before[1].contains(" Files") && !before[1].contains("(focused)"));
    // Click the empty space under the entries: the tree takes the keys.
    a.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row: 8,
        modifiers: KeyModifiers::NONE,
    });
    assert!(a.focus() == Focus::Tree);
    let after = screen(&mut a, 80, 10);
    assert!(after[1].contains("Files  (focused)"), "{after:?}");
    assert!(after[9].contains("TREE"));
    assert!(!after.iter().any(|l| l.contains(".env")));
    press(&mut a, KeyCode::Char('.'), KeyModifiers::NONE);
    assert_eq!(a.doc.editor.buffer.text(), "doc", "no dot in the document");
    assert!(screen(&mut a, 80, 10).iter().any(|l| l.contains(".env")));
    // Clicking a tab hands the keys back to the editor.
    a.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 30,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert!(a.focus() == Focus::Main);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn alt_dot_toggles_dotfiles_from_the_editor_even_with_the_tree_hidden() {
    let d = disk_dir("altdot");
    std::fs::write(d.join(".hidden"), "x").unwrap();
    let mut a = app("doc");
    a.set_tree_root(d.clone(), false);
    press(&mut a, KeyCode::Char('.'), KeyModifiers::ALT);
    assert_eq!(a.doc.editor.buffer.text(), "doc");
    assert_eq!(a.doc.editor.message.as_deref(), Some("Hidden files shown"));
    assert!(a.tree().unwrap().show_hidden);
    a.run_command("tree hidden");
    assert_eq!(a.doc.editor.message.as_deref(), Some("Hidden files hidden"));
    std::fs::remove_dir_all(&d).unwrap();
}

// ---- code intelligence (v0.3) against an in-process fake server ----------

mod intel_fake {
    use skink_core::Backend;
    use skink_lsp::*;
    use std::path::{Path, PathBuf};

    /// Canned answers; `other` is a second file for jumps.
    pub struct Fake {
        pub other: PathBuf,
    }

    fn loc(path: &Path, line: usize, col: usize) -> Location {
        Location {
            path: path.to_path_buf(),
            range: Range::new(Position::new(line, col), Position::new(line, col + 3)),
        }
    }

    impl LspProvider for Fake {
        fn backend(&self) -> Backend {
            Backend::Local
        }
        fn did_open(&self, _: &Path, _: &str) -> Result<()> {
            Ok(())
        }
        fn did_change(&self, _: &Path, _: &str) -> Result<()> {
            Ok(())
        }
        fn did_save(&self, _: &Path) -> Result<()> {
            Ok(())
        }
        fn did_close(&self, _: &Path) -> Result<()> {
            Ok(())
        }
        fn hover(&self, _: &Path, _: Position) -> Result<Option<Hover>> {
            Ok(Some(Hover {
                text: "```rust\nfn push(&mut self, value: T)\n```\nAppends".into(),
                range: None,
            }))
        }
        fn definition(&self, _: &Path, _: Position) -> Result<Vec<Location>> {
            Ok(vec![loc(&self.other, 2, 4)])
        }
        fn references(&self, p: &Path, _: Position, _: bool) -> Result<Vec<Location>> {
            Ok(vec![loc(p, 0, 0), loc(&self.other, 2, 4)])
        }
        fn completion(&self, _: &Path, _: Position) -> Result<Vec<CompletionItem>> {
            let item = |label: &str, text: &str, snippet| CompletionItem {
                label: label.into(),
                insert_text: text.into(),
                snippet,
                documentation: Some("Appends an element".into()),
                kind: ItemKind::Method,
                detail: Some("fn".into()),
                sort_text: None,
                edit: None,
                additional_edits: vec![],
            };
            Ok(vec![
                item("pop", "pop()", false),
                item("push", "push(${1:value}, ${2:n})$0", true),
            ])
        }
        fn rename(&self, _: &Path, _: Position, _: &str) -> Result<Option<WorkspaceEdit>> {
            Ok(None)
        }
        fn formatting(&self, _: &Path, _: FormattingOptions) -> Result<Vec<TextEdit>> {
            // "let  x=1;" -> "let x = 1;"
            Ok(vec![
                TextEdit {
                    range: Range::new(Position::new(0, 4), Position::new(0, 5)),
                    new_text: String::new(),
                },
                TextEdit {
                    range: Range::new(Position::new(0, 6), Position::new(0, 7)),
                    new_text: " = ".into(),
                },
            ])
        }
        fn signature_help(&self, _: &Path, _: Position) -> Result<Option<SignatureHelp>> {
            Ok(None)
        }
        fn document_symbols(&self, _: &Path) -> Result<Vec<DocumentSymbol>> {
            let r = Range::new(Position::new(1, 3), Position::new(1, 6));
            Ok(vec![DocumentSymbol {
                name: "main".into(),
                detail: None,
                kind: ItemKind::Function,
                range: r,
                selection_range: r,
                children: vec![],
            }])
        }
        fn code_actions(&self, p: &Path, _: Range, _: &[Diagnostic]) -> Result<Vec<CodeAction>> {
            Ok(vec![CodeAction {
                title: "Add semicolon".into(),
                kind: Some("quickfix".into()),
                preferred: true,
                edit: Some(WorkspaceEdit {
                    files: vec![FileEdit {
                        path: p.to_path_buf(),
                        edits: vec![TextEdit {
                            range: Range::new(Position::new(1, 0), Position::new(1, 0)),
                            new_text: "// fixed\n".into(),
                        }],
                    }],
                    ops: vec![],
                }),
                command: None,
            }])
        }
        fn diagnostics(&self, _: &Path) -> Vec<Diagnostic> {
            Vec::new()
        }
    }
}

/// An app with `main.rs` (and `other.rs`) open against the fake server.
fn intel_app(tag: &str, text: &str) -> (App, PathBuf) {
    let d = disk_dir(tag);
    std::fs::write(d.join("main.rs"), text).unwrap();
    std::fs::write(d.join("other.rs"), "\n\nfn target() {}\n").unwrap();
    let mut a = app("");
    a.lsp_enabled = true;
    a.set_wake(std::sync::Arc::new(|| {}));
    a.lsp = Some(crate::lsp::LspLink::with_provider(
        std::sync::Arc::new(intel_fake::Fake {
            other: doc::canonical(&d.join("other.rs")),
        }),
        || {},
    ));
    a.open_path(&d.join("main.rs"));
    assert!(a.doc.lsp.is_open());
    (a, d)
}

/// Run the event loop's background step until no question is pending.
fn settle(a: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !a.intel.waiting.is_empty() && std::time::Instant::now() < deadline {
        a.poll_background();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    a.poll_background();
}

#[test]
fn completion_filters_while_typing_and_snippet_tabstops_follow_tab() {
    let (mut a, d) = intel_app("complete", "fn main() {\n    v.\n}\n");
    a.doc.editor.goto(Some(2), Some(7)); // after "v."
    type_str(&mut a, "pu"); // two word chars: asks the server
    settle(&mut a);
    let s = screen(&mut a, 80, 12);
    assert!(s.iter().any(|l| l.contains("push")), "{s:?}");
    assert!(!s.iter().any(|l| l.contains(" pop")), "filtered by 'pu'");
    assert!(
        s.iter().any(|l| l.contains("Appends an element")),
        "docs panel"
    );
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(a.doc.editor.buffer.line_text(1), "    v.push(value, n)");
    assert_eq!(a.doc.editor.selected_text().as_deref(), Some("value"));
    type_str(&mut a, "item"); // replaces the selected placeholder
    press(&mut a, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(a.doc.editor.selected_text().as_deref(), Some("n"));
    press(&mut a, KeyCode::Tab, KeyModifiers::NONE); // $0: after the call
    assert_eq!(a.doc.editor.buffer.line_text(1), "    v.push(item, n)");
    assert_eq!(a.doc.editor.cursor.col, 19);
    press(&mut a, KeyCode::Tab, KeyModifiers::NONE); // session over: a real tab
    assert!(
        a.doc.editor.buffer.line_text(1).ends_with('\t')
            || a.doc.editor.buffer.line_text(1).ends_with("  ")
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn definition_references_and_results_tab_jumps() {
    let (mut a, d) = intel_app("goto", "fn main() {\n    target();\n}\n");
    press(&mut a, KeyCode::Char('g'), KeyModifiers::ALT);
    settle(&mut a);
    assert!(a.doc.canon.as_ref().unwrap().ends_with("other.rs"));
    assert_eq!((a.doc.editor.cursor.line, a.doc.editor.cursor.col), (2, 4));
    a.open_path(&d.join("main.rs"));
    press(&mut a, KeyCode::Char('r'), KeyModifiers::ALT);
    settle(&mut a);
    assert!(a.doc.title().starts_with("References"), "{}", a.doc.title());
    assert!(a.doc.editor.buffer.text().contains("other.rs:3:5"));
    press(&mut a, KeyCode::Down, KeyModifiers::NONE);
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert!(a.doc.canon.as_ref().unwrap().ends_with("other.rs"));
    assert_eq!(a.doc.editor.cursor.line, 2);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn format_on_save_formats_then_saves_and_code_action_applies() {
    let (mut a, d) = intel_app("format", "let  x=1;\nfoo\n");
    a.settings.format_on_save = true;
    type_str(&mut a, "");
    a.doc.editor.goto(Some(2), Some(4));
    type_str(&mut a, "d");
    press(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
    settle(&mut a);
    let disk = std::fs::read_to_string(d.join("main.rs")).unwrap();
    assert_eq!(disk, "let x = 1;\nfood\n");
    assert!(!a.doc.editor.buffer.is_dirty());
    // Alt-Enter: code actions in a list; Enter applies the edit.
    press(&mut a, KeyCode::Enter, KeyModifiers::ALT);
    settle(&mut a);
    assert!(
        matches!(&a.overlay, Overlay::Picker(p) if p.mode == crate::picker::Mode::Choices("Code actions".into()))
    );
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(a.doc.editor.buffer.line_text(1), "// fixed");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn quick_open_modes_outline_and_command_palette() {
    let (mut a, d) = intel_app("modes", "\nfn main() {}\n");
    press(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL); // outline
    settle(&mut a);
    let s = screen(&mut a, 80, 12);
    assert!(
        s.iter().any(|l| l.contains("Symbols in file")) && s.iter().any(|l| l.contains("main"))
    );
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!((a.doc.editor.cursor.line, a.doc.editor.cursor.col), (1, 3));
    // `>` switches quick open to commands; Enter runs one.
    a.open_picker();
    type_str(&mut a, ">problems");
    let s = screen(&mut a, 80, 12);
    assert!(s.iter().any(|l| l.contains("Commands")), "{s:?}");
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert!(a.doc.title().starts_with("Problems"), "{}", a.doc.title());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn markdown_preview_renders_live_and_toggles_back() {
    let dir = std::env::temp_dir().join(format!("skink-ui-md-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let md = dir.join("notes.md");
    std::fs::write(&md, "# Plan\n\n- [ ] ship\n").unwrap();
    let mut a = app("");
    a.open_path(&md);
    press(&mut a, KeyCode::Char('v'), KeyModifiers::ALT);
    let s = screen(&mut a, 60, 12);
    assert!(
        s.iter().any(|l| l.contains("Plan")) && s.iter().any(|l| l.contains("☐ ship")),
        "{s:?}"
    );
    assert!(
        !s.iter().any(|l| l.contains("# Plan")),
        "markup is rendered, not shown: {s:?}"
    );
    // Back to the source, edit, and the preview follows.
    press(&mut a, KeyCode::Char('v'), KeyModifiers::ALT);
    assert!(a.doc.editor.buffer.text().starts_with("# Plan"));
    a.doc.editor.insert_text("Intro *now*\n\n");
    press(&mut a, KeyCode::Char('v'), KeyModifiers::ALT);
    let s = screen(&mut a, 60, 12);
    assert!(s.iter().any(|l| l.contains("Intro now")), "{s:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(feature = "svg-preview")]
#[test]
fn svg_preview_draws_half_blocks() {
    let dir = std::env::temp_dir().join(format!("skink-ui-svg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let svg = dir.join("dot.svg");
    std::fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#ff0000"/></svg>"##,
    )
    .unwrap();
    let mut a = app("");
    a.open_path(&svg);
    a.toggle_preview();
    let mut t = Terminal::new(TestBackend::new(40, 12)).unwrap();
    t.draw(|f| a.render(f)).unwrap();
    let buf = t.backend().buffer();
    let red = (0..40)
        .flat_map(|x| (0..12).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            buf[(x, y)].symbol() == "▀" && buf[(x, y)].fg == ratatui::style::Color::Rgb(255, 0, 0)
        })
        .count();
    assert!(red > 10, "{red} red cells");
    std::fs::remove_dir_all(&dir).unwrap();
}
