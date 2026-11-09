//! Lua scripts against a real `App` (temp dirs only; no processes).

use super::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};

fn project(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skink-script-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("cfg")).unwrap();
    std::fs::create_dir_all(d.join("proj/.skink")).unwrap();
    std::fs::write(d.join("proj/go.mod"), "module x\n").unwrap();
    std::fs::write(d.join("proj/a.txt"), "hello\n").unwrap();
    d
}

fn app_in(d: &Path, user_script: &str) -> App {
    std::fs::write(d.join("cfg/init.lua"), user_script).unwrap();
    let mut a = App::new(
        Editor::new(Buffer::empty()),
        Keymap::defaults(),
        Backend::Local,
    );
    a.state_dir = Some(d.join("state"));
    a.open_path(&d.join("proj/a.txt"));
    a.load_scripts(Some(d.join("cfg")));
    a
}

fn toasts(a: &App) -> String {
    a.toasts
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn commands_edit_as_one_undo_step_and_events_fire() {
    let d = project("cmd");
    let mut a = app_in(
        &d,
        r#"
        skink.on("open", function(path) skink.set_status("words", "opened") end)
        skink.add_command("shout", function(word)
            skink.set_cursor(1, 1)
            skink.insert(word or "HEY")
            skink.insert(" ")
            print("shouted", skink.lang(), select(2, skink.cursor()))
        end)
        skink.on("save", function(path)
            skink.set_status("words", tostring(#skink.text()) .. " bytes")
        end)
        "#,
    );
    assert_eq!(toasts(&a), "", "loads cleanly");
    assert_eq!(a.script_status, vec![("words".into(), "opened".into())]);
    a.run_command("shout YO");
    assert_eq!(a.doc.editor.buffer.text(), "YO hello\n");
    assert!(
        toasts(&a).contains("shouted"),
        "print is a toast: {}",
        toasts(&a)
    );
    a.doc.editor.undo();
    assert_eq!(
        a.doc.editor.buffer.text(),
        "hello\n",
        "two inserts, one undo"
    );
    a.run_command("shout");
    a.run_chain("Save");
    assert_eq!(
        std::fs::read_to_string(d.join("proj/a.txt")).unwrap(),
        "HEY hello\n"
    );
    assert_eq!(a.script_status[0].1, "10 bytes");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn runaway_scripts_are_stopped_and_errors_are_toasts() {
    let d = project("limits");
    let mut a = app_in(
        &d,
        r#"
        skink.add_command("spin", function() while true do end end)
        skink.add_command("hog", function()
            local t = {}
            for i = 1, 1e9 do t[i] = string.rep("x", 1024) .. i end
        end)
        skink.add_command("boom", function() error("no such thing") end)
        skink.add_command("io", function() return io.open("/etc/passwd") end)
        "#,
    );
    let t = std::time::Instant::now();
    a.run_command("spin");
    assert!(t.elapsed() < std::time::Duration::from_secs(2));
    assert!(
        toasts(&a).contains("stopped after 250 ms"),
        "{}",
        toasts(&a)
    );
    a.run_command("hog");
    assert!(toasts(&a).contains("memory"), "{}", toasts(&a));
    a.run_command("boom");
    assert!(toasts(&a).contains("no such thing"), "{}", toasts(&a));
    a.run_command("io");
    assert!(toasts(&a).contains("io"), "no io library: {}", toasts(&a));
    // Still usable afterwards.
    a.run_script("later", r#"skink.insert("ok ")"#);
    assert_eq!(a.doc.editor.buffer.text(), "ok hello\n");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn project_scripts_run_only_once_trusted() {
    let d = project("trust");
    let script = d.join("proj/.skink/init.lua");
    std::fs::write(&script, r#"skink.set_status("p", "project ran")"#).unwrap();
    let mut a = app_in(&d, "");
    assert!(a.script_status.is_empty(), "not run before it is trusted");
    assert!(matches!(&a.overlay, Overlay::Prompt(p)
        if matches!(p.kind, prompt::PromptKind::Choice(prompt::Choice::TrustScript { .. }))));
    a.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
    assert_eq!(a.script_status[0].1, "project ran");

    // Remembered: the next start runs it without asking...
    let b = app_in(&d, "");
    assert_eq!(b.script_status[0].1, "project ran");
    assert!(matches!(b.overlay, Overlay::None));
    // ...until it changes.
    std::fs::write(&script, r#"skink.set_status("p", "changed")"#).unwrap();
    let mut c = app_in(&d, "");
    assert!(c.script_status.is_empty());
    c.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(c.script_status.is_empty(), "declined");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn unbound_keys_pickers_and_debounced_change() {
    let d = project("keys");
    let mut a = app_in(
        &d,
        r#"
        changes = 0
        skink.on("change", function() changes = changes + 1
            skink.set_status("c", "changes " .. changes) end)
        skink.on("key", function(key)
            if key == "Ctrl-Alt-J" then
                skink.pick("Insert", {"alpha", "beta"}, function(label, i)
                    skink.insert(label .. i)
                end)
                return true
            end
        end)
        "#,
    );
    a.handle_key(KeyEvent::new(
        KeyCode::Char('j'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ));
    assert!(
        matches!(a.overlay, Overlay::Picker(_)),
        "the script opened a list"
    );
    a.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    a.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(a.doc.editor.buffer.text(), "beta2hello\n");
    // Typing arms `change`; it fires once, after the pause.
    a.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    a.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(a.next_timeout().is_some());
    a.on_timeout();
    assert!(a.script_status.is_empty(), "not yet");
    std::thread::sleep(crate::script::CHANGE_DEBOUNCE);
    a.on_timeout();
    a.on_timeout();
    assert_eq!(a.script_status[0].1, "changes 1");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_example_scripts_in_docs_work() {
    let d = project("examples");
    std::fs::write(d.join("proj/a.txt"), "one two  \n// TODO three\n").unwrap();
    let all = [
        include_str!("../../../docs/scripts/word-count.lua"),
        include_str!("../../../docs/scripts/trim-on-save.lua"),
        include_str!("../../../docs/scripts/todos.lua"),
    ]
    .join("\n");
    let mut a = app_in(&d, &all);
    assert_eq!(toasts(&a), "");
    assert_eq!(a.script_status[0].1, "5 words");
    a.handle_key(KeyEvent::new(
        KeyCode::Char('j'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ));
    assert!(matches!(a.overlay, Overlay::Picker(_)), "{}", toasts(&a));
    a.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(a.doc.editor.cursor.line, 1, "jumped to the TODO");
    a.doc.editor.insert_text("x");
    a.run_chain("Save");
    assert_eq!(
        std::fs::read_to_string(d.join("proj/a.txt")).unwrap(),
        "one two\nx// TODO three\n",
        "trimmed and saved"
    );
    assert!(!a.doc.editor.buffer.is_dirty());
    assert_eq!(toasts(&a), "");
    let _ = std::fs::remove_dir_all(&d);
}
