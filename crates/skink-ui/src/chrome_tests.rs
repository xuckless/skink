//! The settings screen, the key-binding editor and the menu bar, driven
//! through keys and the mouse like a user would. Config lives in temp
//! folders only.

use super::*;
use ratatui::{
    Terminal,
    backend::TestBackend,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
};
use skink_core::Buffer;
use std::path::PathBuf;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skink-chrome-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An app whose config folder is `dir/cfg` (never the real one).
fn app(dir: &std::path::Path) -> App {
    let mut a = App::new(
        Editor::new(Buffer::from_text("one\ntwo\n")),
        Keymap::defaults(),
        Backend::Local,
    );
    a.icons = crate::icons::IconSet::Ascii;
    a.config_dir = Some(dir.join("cfg"));
    a.state_dir = None;
    a
}

fn press(a: &mut App, code: KeyCode, mods: KeyModifiers) {
    a.handle_key(KeyEvent::new(code, mods));
}

fn key(a: &mut App, code: KeyCode) {
    press(a, code, KeyModifiers::NONE);
}

fn type_str(a: &mut App, s: &str) {
    for c in s.chars() {
        key(a, KeyCode::Char(c));
    }
}

fn cmd(a: &mut App, line: &str) {
    press(a, KeyCode::Char('e'), KeyModifiers::CONTROL);
    type_str(a, line);
    key(a, KeyCode::Enter);
}

fn screen(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| a.render(f)).unwrap();
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

fn click(a: &mut App, x: u16, y: u16) {
    let ev = |kind| MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    a.handle_mouse(ev(MouseEventKind::Down(MouseButton::Left)));
    a.handle_mouse(ev(MouseEventKind::Up(MouseButton::Left)));
}

fn msg(a: &App) -> String {
    a.doc.editor.message.clone().unwrap_or_default()
}

fn read(p: PathBuf) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

#[test]
fn the_settings_screen_changes_settings_in_place_and_keeps_comments() {
    let d = temp("settings");
    let cfg = d.join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("settings.json"),
        "{\n  // my tabs\n  \"tabsize\": 4,\n  \"syntax\": true\n}\n",
    )
    .unwrap();
    let mut a = app(&d);
    assert!(a.load_user_config().is_empty());
    cmd(&mut a, "settings");
    assert!(matches!(a.overlay, Overlay::Settings(_)));
    let s = screen(&mut a, 110, 30);
    assert!(s.iter().any(|l| l.contains("Settings")), "{s:#?}");
    assert!(s.iter().any(|l| l.contains("Appearance")), "{s:#?}");
    // Search, then step the number up twice.
    type_str(&mut a, "tabsize");
    key(&mut a, KeyCode::Right);
    key(&mut a, KeyCode::Right);
    let text = read(cfg.join("settings.json"));
    assert!(
        text.contains("// my tabs") && text.contains("\"tabsize\": 6"),
        "{text}"
    );
    assert_eq!(a.settings.tabsize, 6, "applied at once");
    assert!(msg(&a).contains("tabsize = 6"), "{}", msg(&a));
    // Typing a value: a bad one stays in the editor and says why.
    key(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Char('u'), KeyModifiers::CONTROL);
    type_str(&mut a, "99");
    key(&mut a, KeyCode::Enter);
    assert!(msg(&a).contains("from 1 to 16"), "{}", msg(&a));
    press(&mut a, KeyCode::Char('u'), KeyModifiers::CONTROL);
    type_str(&mut a, "3");
    key(&mut a, KeyCode::Enter);
    assert_eq!(a.settings.tabsize, 3);
    // Delete takes it out of the file: the default is back.
    key(&mut a, KeyCode::Delete);
    assert!(!read(cfg.join("settings.json")).contains("tabsize"));
    assert_eq!(a.settings.tabsize, 4);
    // A toggle: Enter flips it.
    key(&mut a, KeyCode::Esc); // clears the search
    type_str(&mut a, "menubar");
    key(&mut a, KeyCode::Enter);
    assert!(a.settings.menubar);
    // A list: keymap goes to vscode.
    key(&mut a, KeyCode::Esc);
    type_str(&mut a, "keymap");
    key(&mut a, KeyCode::Enter);
    key(&mut a, KeyCode::Down);
    key(&mut a, KeyCode::Enter);
    assert_eq!(a.settings.keymap.name(), "vscode");
    assert!(read(cfg.join("settings.json")).contains("\"keymap\": \"vscode\""));
    // Esc twice: search cleared, then closed.
    key(&mut a, KeyCode::Esc);
    key(&mut a, KeyCode::Esc);
    assert!(matches!(a.overlay, Overlay::None));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_theme_setting_previews_and_esc_puts_the_old_one_back() {
    let d = temp("theme");
    let mut a = app(&d);
    a.load_user_config();
    let before = a.theme;
    cmd(&mut a, "settings colorscheme");
    key(&mut a, KeyCode::Enter);
    key(&mut a, KeyCode::Down);
    key(&mut a, KeyCode::Down);
    assert_ne!(a.theme, before, "previewed");
    key(&mut a, KeyCode::Esc);
    assert_eq!(a.theme, before, "put back");
    assert!(!d.join("cfg/settings.json").exists(), "nothing written");
    // Enter keeps one: written and applied.
    key(&mut a, KeyCode::Enter);
    for _ in 0..12 {
        key(&mut a, KeyCode::Down);
    }
    key(&mut a, KeyCode::Enter);
    let text = read(d.join("cfg/settings.json"));
    assert!(text.contains("\"colorscheme\""), "{text}");
    assert_ne!(a.settings.colorscheme, "auto");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_settings_screen_refuses_broken_or_unsaved_files_and_writes_project_settings() {
    let d = temp("refuse");
    let cfg = d.join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    let broken = "{\n  \"tabsize\": 99\n}\n";
    std::fs::write(cfg.join("settings.json"), broken).unwrap();
    let mut a = app(&d);
    let proj = d.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    a.set_tree_root(proj.clone(), false);
    cmd(&mut a, "settings tabsize");
    key(&mut a, KeyCode::Right);
    assert_eq!(read(cfg.join("settings.json")), broken, "untouched");
    assert!(msg(&a).contains("fix the file first"), "{}", msg(&a));
    // Tab: the project's file instead (created).
    key(&mut a, KeyCode::Tab);
    key(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Char('u'), KeyModifiers::CONTROL);
    type_str(&mut a, "2");
    key(&mut a, KeyCode::Enter);
    let p = read(proj.join(".skink/settings.json"));
    assert!(p.contains("\"tabsize\": 2"), "{p}");
    let s = screen(&mut a, 120, 30);
    assert!(s.iter().any(|l| l.contains("project")), "{s:#?}");
    // An open settings.json with unsaved edits is not overwritten.
    std::fs::write(cfg.join("settings.json"), "{}\n").unwrap();
    key(&mut a, KeyCode::Esc);
    cmd(&mut a, "settings json");
    type_str(&mut a, " ");
    cmd(&mut a, "settings tabsize");
    key(&mut a, KeyCode::Right);
    assert!(msg(&a).contains("unsaved edits"), "{}", msg(&a));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_key_editor_rebinds_unbinds_and_resets() {
    let d = temp("keys");
    let cfg = d.join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("bindings.json"), "{\n  // mine\n}\n").unwrap();
    let mut a = app(&d);
    assert!(a.load_user_config().is_empty());
    let spec = |k: &str| skink_config::parse_key(k).unwrap();
    cmd(&mut a, "keys ToggleComment");
    let s = screen(&mut a, 120, 30);
    assert!(
        s.iter()
            .any(|l| l.contains("ToggleComment") && l.contains("Alt-/")),
        "{s:#?}"
    );
    // Rebind to Alt-Y: free, so it applies at once; the old keys go.
    key(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Char('y'), KeyModifiers::ALT);
    assert_eq!(
        a.keymap.command(&spec("Alt-y")),
        Some("ToggleComment"),
        "{}",
        msg(&a)
    );
    assert_eq!(a.keymap.command(&spec("Alt-/")), Some(keyed::UNBOUND));
    let text = read(cfg.join("bindings.json"));
    assert!(
        text.contains("// mine") && text.contains("\"Alt-y\": \"ToggleComment\""),
        "{text}"
    );
    // A key that runs something else asks first (Alt-J is PeekDefinition).
    press(&mut a, KeyCode::Char('n'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Char('j'), KeyModifiers::ALT);
    let s = screen(&mut a, 160, 30);
    assert!(
        s.iter().any(|l| l.contains("runs PeekDefinition now")),
        "{s:#?}"
    );
    key(&mut a, KeyCode::Enter);
    assert_eq!(a.keymap.command(&spec("Alt-j")), Some("ToggleComment"));
    assert_eq!(
        a.keymap.command(&spec("Alt-y")),
        Some("ToggleComment"),
        "added"
    );
    // Esc cancels a pending one.
    key(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Char('u'), KeyModifiers::CONTROL);
    key(&mut a, KeyCode::Esc);
    assert_eq!(
        a.keymap.command(&spec("Ctrl-u")),
        Some("ToggleMacro"),
        "cancelled"
    );
    // Ctrl-R: back to the defaults.
    press(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(a.keymap.command(&spec("Alt-/")), Some("ToggleComment"));
    assert_eq!(a.keymap.command(&spec("Alt-j")), Some("PeekDefinition"));
    assert_eq!(a.keymap.command(&spec("Alt-y")), None);
    // Delete: no key runs it.
    key(&mut a, KeyCode::Delete);
    assert!(
        a.keymap
            .bindings()
            .iter()
            .all(|(_, c)| !c.split('|').any(|n| n == "ToggleComment")),
        "unbound"
    );
    // Esc closes it (nothing typed); search by key.
    key(&mut a, KeyCode::Esc);
    assert!(matches!(a.overlay, Overlay::None));
    cmd(&mut a, "keys");
    type_str(&mut a, "ctrl-s");
    let s = screen(&mut a, 120, 30);
    assert!(s.iter().any(|l| l.contains("Save")), "{s:#?}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn f10_opens_the_menus_and_the_menubar_setting_costs_a_row_only_when_on() {
    let d = temp("menu");
    let mut a = app(&d);
    a.load_user_config();
    let s = screen(&mut a, 100, 20);
    assert!(!s[0].contains("Selection"), "no bar by default: {s:#?}");
    key(&mut a, KeyCode::F(10));
    assert!(matches!(a.overlay, Overlay::Menu(_)));
    let s = screen(&mut a, 100, 20);
    assert!(
        s[0].contains("File") && s[0].contains("Selection"),
        "{s:#?}"
    );
    assert!(
        s.iter().any(|l| l.contains("Save") && l.contains("Ctrl-S")),
        "{s:#?}"
    );
    // Edit > ... > Select all, by keys.
    key(&mut a, KeyCode::Right);
    key(&mut a, KeyCode::Right);
    key(&mut a, KeyCode::Enter); // Selection > Select all
    assert!(matches!(a.overlay, Overlay::None));
    assert!(a.doc.editor.selection().is_some(), "ran the entry");
    // The bar on: its own row above the tabs, and a click opens a menu.
    std::fs::create_dir_all(d.join("cfg")).unwrap();
    std::fs::write(d.join("cfg/settings.json"), "{\"menubar\": true}").unwrap();
    a.load_user_config();
    let s = screen(&mut a, 100, 20);
    assert!(s[0].contains("File") && s[0].contains("Help"), "{s:#?}");
    assert!(!s[1].contains("Selection"), "tabs below: {s:#?}");
    let (x0, _, _) = menu::titles(0)[0];
    click(&mut a, x0 + 1, 0);
    assert!(matches!(&a.overlay, Overlay::Menu(m) if m.menu == 0));
    let s = screen(&mut a, 100, 20);
    let row = s.iter().position(|l| l.contains("Quick open")).unwrap() as u16;
    click(&mut a, x0 + 2, row);
    assert!(matches!(a.overlay, Overlay::Picker(_)), "Quick open ran");
    let _ = std::fs::remove_dir_all(&d);
}
