use super::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use skink_core::Buffer;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skink-pkgui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn app(root: &Path) -> App {
    let mut a = App::new(
        Editor::new(Buffer::empty()),
        Keymap::defaults(),
        Backend::Local,
    );
    a.data_dir = Some(root.join("data"));
    a.state_dir = None;
    a.config_dir = None;
    a
}

fn cmd(a: &mut App, line: &str) {
    a.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    for c in line.chars() {
        a.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    a.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
}

/// Poll until the background package work is done (20 s at most).
fn settle(a: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !a.pkgs.work.is_empty() {
        assert!(Instant::now() < deadline, "package work did not finish");
        a.poll_packages();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn toasts(a: &App) -> Vec<String> {
    a.toasts.iter().map(|t| t.text.clone()).collect()
}

/// A package with the skink-syntax test grammar (an original grammar).
fn package(dir: &Path) {
    let wasm = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../skink-syntax/tests/fixtures/skinktest/skinktest.wasm");
    std::fs::create_dir_all(dir.join("grammars")).unwrap();
    std::fs::copy(wasm, dir.join("grammars/skinkui.wasm")).unwrap();
    std::fs::write(dir.join("hl.scm"), "\"let\" @keyword\n(number) @number\n").unwrap();
    std::fs::write(
        dir.join("skink-package.toml"),
        r#"[package]
name = "skinkui"
version = "0.1.0"

[[grammar]]
# The test grammar's C name is skinktest; the file can be called anything.
name = "skinktest"
wasm = "grammars/skinkui.wasm"
highlights = "hl.scm"
extensions = ["skui"]
"#,
    )
    .unwrap();
}

#[test]
fn an_installed_package_colours_files_at_once_and_disabling_it_stops() {
    let root = temp("install");
    package(&root.join("pkg"));
    std::fs::write(root.join("a.skui"), "let x = 42\n").unwrap();
    let mut a = app(&root);
    a.open_path(&root.join("a.skui"));
    assert!(a.doc.syntax.is_none(), "no grammar before the install");

    cmd(&mut a, &format!("install {}", root.join("pkg").display()));
    settle(&mut a);
    assert!(
        toasts(&a).iter().any(|t| t == "installed skinkui 0.1.0"),
        "{:?}",
        toasts(&a)
    );
    // The open file picked the grammar up without reopening.
    let syn = a.doc.syntax.as_ref().expect("coloured after the install");
    assert_eq!(syn.hl.lang().name(), "skinktest");

    cmd(&mut a, "packages");
    assert!(a.doc.editor.buffer.text().contains("skinkui"));
    assert!(a.doc.editor.buffer.text().contains(" on "));

    cmd(&mut a, "packages disable skinkui");
    a.open_path(&root.join("a.skui"));
    assert!(a.doc.syntax.is_none(), "disabled: no colours");
    cmd(&mut a, "packages enable skinkui");
    assert!(a.doc.syntax.is_some());

    cmd(&mut a, "uninstall skinkui");
    assert!(a.doc.syntax.is_none());
    assert!(!root.join("data/packages/skinkui").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_file_without_a_grammar_suggests_its_package_once() {
    let root = temp("hint");
    for f in ["a.kt", "b.kt", "c.unknownext"] {
        std::fs::write(root.join(f), "").unwrap();
    }
    let mut a = app(&root);
    a.open_path(&root.join("a.kt"));
    a.open_path(&root.join("b.kt"));
    a.open_path(&root.join("c.unknownext"));
    let hints: Vec<String> = toasts(&a)
        .into_iter()
        .filter(|t| t.starts_with("No grammar"))
        .collect();
    assert_eq!(hints, ["No grammar for .kt: :install kotlin"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn install_dot_asks_before_installing_what_the_project_needs() {
    let root = temp("dot");
    let p = root.join("proj");
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::write(p.join("src/Main.kt"), "").unwrap();
    std::fs::write(p.join("Cargo.toml"), "").unwrap();
    let mut a = app(&root);
    a.open_path(&p.join("Cargo.toml"));
    a.set_tree_root(p.clone(), false);
    cmd(&mut a, "install .");
    settle(&mut a);
    let Overlay::Prompt(pr) = &a.overlay else {
        panic!("expected the plan question, toasts: {:?}", toasts(&a));
    };
    assert!(
        pr.label.starts_with("Install kotlin (1 file (.kt))?"),
        "{}",
        pr.label
    );
    // No: nothing is installed.
    a.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(a.pkgs.work.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}
