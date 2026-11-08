use super::*;
use ratatui::{
    Terminal,
    backend::TestBackend,
    crossterm::event::{KeyCode, KeyModifiers},
};
use skink_core::Buffer;

fn app(text: &str) -> App {
    App::new(
        Editor::new(Buffer::from_text(text)),
        Keymap::defaults(),
        Backend::Local,
    )
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
    let s = screen(&mut a, 50, 4);
    assert_eq!(s[0], " 1 hello");
    assert_eq!(s[1], " 2 world");
    assert_eq!(s[3], " [scratch]  Ln 1, Col 1  LF  F12 terminal");
}

#[test]
fn typing_edits_and_marks_dirty() {
    let mut a = app("");
    type_str(&mut a, "hi");
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    type_str(&mut a, "x");
    let s = screen(&mut a, 40, 4);
    assert_eq!(s[0], " 1 hi");
    assert_eq!(s[1], " 2 x");
    assert!(s[3].contains("[scratch] *"));
}

#[test]
fn undo_redo_via_keys() {
    let mut a = app("");
    type_str(&mut a, "abc");
    press(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(a.editor.buffer.text(), "");
    press(&mut a, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(a.editor.buffer.text(), "abc");
}

#[test]
fn shift_arrows_select_and_typing_replaces() {
    let mut a = app("hello");
    press(&mut a, KeyCode::Right, KeyModifiers::SHIFT);
    press(&mut a, KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(a.editor.selected_text().as_deref(), Some("he"));
    type_str(&mut a, "J");
    assert_eq!(a.editor.buffer.text(), "Jllo");
}

#[test]
fn tab_indents_or_inserts_via_fallback_chain() {
    let mut a = app("x");
    press(&mut a, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(a.editor.buffer.text(), "\tx");
    press(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(a.editor.buffer.text(), "x");
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
    assert!(screen(&mut a, 80, 3)[2].starts_with("Save changes before quitting?"));
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
    assert!(screen(&mut a, 80, 3)[2].starts_with("Save as:"));
    type_str(&mut a, path.to_str().unwrap());
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "data");
    assert!(!a.editor.buffer.is_dirty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn find_prompt_selects_match() {
    let mut a = app("one two three");
    press(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut a, "two");
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(a.editor.selected_text().as_deref(), Some("two"));
}

#[test]
fn goto_command_bar() {
    let mut a = app("a\nb\nc\nd");
    press(&mut a, KeyCode::Char('l'), KeyModifiers::CONTROL); // Micro: command-edit:goto
    assert!(screen(&mut a, 40, 3)[2].starts_with(":goto"));
    type_str(&mut a, "3");
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(a.editor.cursor.line, 2);
}

#[test]
fn page_down_scrolls() {
    let text: String = (1..=50).map(|i| format!("line{i}\n")).collect();
    let mut a = app(&text);
    let _ = screen(&mut a, 20, 11); // 10 text rows
    press(&mut a, KeyCode::PageDown, KeyModifiers::NONE);
    press(&mut a, KeyCode::PageDown, KeyModifiers::NONE);
    let s = screen(&mut a, 20, 11);
    assert!(s[9].ends_with("line19"), "{s:?}");
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
    let s = screen(&mut a, 13, 2); // gutter " 1 " + 10 text cols
    assert_eq!(s[0], " 1 bcdefghij");
}

#[test]
fn bracketed_paste_inserts_text() {
    let mut a = app("");
    a.handle_paste("a\r\nb");
    assert_eq!(a.editor.buffer.text(), "a\nb");
}

#[test]
fn mouse_click_moves_cursor_and_wheel_scrolls() {
    let text: String = (1..=50).map(|i| format!("line{i}\n")).collect();
    let mut a = app(&text);
    let _ = screen(&mut a, 30, 11);
    let click = |kind, column, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    a.handle_mouse(click(MouseEventKind::Down(MouseButton::Left), 4 + 3, 4)); // gutter is 4 wide (" 51 ")
    assert_eq!((a.editor.cursor.line, a.editor.cursor.col), (4, 3));
    a.handle_mouse(click(MouseEventKind::ScrollDown, 5, 5));
    assert_eq!(a.editor.scroll, 3);
}

#[cfg(unix)]
#[test]
fn terminal_pane_runs_a_command_and_shows_output() {
    let mut a = app("text");
    a.open_terminal(CommandSpec::new("sh").args(["-c", "printf 'pane-ok\\n'; sleep 5"]))
        .unwrap();
    let _ = screen(&mut a, 60, 20); // lay out the pane
    let got = a
        .pane
        .as_ref()
        .unwrap()
        .term
        .wait_for(std::time::Duration::from_secs(5), |s| {
            s.text().contains("pane-ok")
        });
    assert!(got);
    let s = screen(&mut a, 60, 20);
    assert!(s.iter().any(|l| l.trim() == "pane-ok"), "{s:?}");
    assert!(s.iter().any(|l| l.contains("terminal")));
    // Focus switching: F12 goes back to the editor, typing edits the buffer.
    press(&mut a, KeyCode::F(12), KeyModifiers::NONE);
    type_str(&mut a, "X");
    assert!(a.editor.buffer.text().starts_with('X'));
    a.close_terminal();
    assert!(a.pane.is_none());
}
