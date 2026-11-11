//! Package grammars: a tiny WASM grammar registered at run time.
#![cfg(feature = "wasm")]

use skink_syntax::{
    DynSpec, Highlighter, Hl, Lang, LoadState, language_for_path, register_grammar,
};
use std::path::PathBuf;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skinktest/skinktest.wasm")
}

fn spec(name: &str, wasm: PathBuf) -> DynSpec {
    DynSpec {
        name: name.into(),
        aliases: vec![format!("{name}-alias")],
        wasm,
        extensions: vec![format!("{name}x")],
        file_names: vec![format!("{name}file")],
        highlights: "(keyword) @keyword\n(number) @number\n(comment) @comment\n".into(),
        injections: None,
    }
}

#[test]
fn a_package_grammar_loads_on_first_use_and_highlights() {
    let d = register_grammar(spec("skinktest", fixture()));
    assert_eq!(d.state(), LoadState::Unloaded, "registering reads nothing");
    let lang = language_for_path("a.skinktestx".as_ref()).expect("by extension");
    assert_eq!(lang, Lang::Dyn(d));
    assert_eq!(language_for_path("skinktestfile".as_ref()), Some(lang));
    assert_eq!(Lang::from_name("skinktest-alias"), Some(lang));
    assert_eq!(lang.name(), "skinktest");

    let text = "let x 42 # note\n";
    let mut h = Highlighter::new_inline(lang);
    h.update(text);
    assert_eq!(d.state(), LoadState::Ready);
    let spans = &h.highlights(text, 0, 1)[0];
    let kinds: Vec<Hl> = spans.iter().map(|s| s.hl).collect();
    assert!(
        kinds.contains(&Hl::Keyword) && kinds.contains(&Hl::Number),
        "{spans:?}"
    );
    assert!(kinds.contains(&Hl::Comment), "{spans:?}");

    // Another thread (the parse worker) gets its own store and parses too.
    let mut snap = h.snapshot();
    let t = std::thread::spawn(move || {
        snap.update("fn 7\n");
        snap.highlights("fn 7\n", 0, 1)[0].len()
    });
    let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !t.is_finished() {
        assert!(
            std::time::Instant::now() < end,
            "worker parse never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(t.join().unwrap(), 2);
}

#[test]
fn a_broken_package_grammar_fails_cleanly() {
    let d = register_grammar(spec("skinkmissing", PathBuf::from("/nonexistent/x.wasm")));
    let lang = Lang::Dyn(d);
    let mut h = Highlighter::new_inline(lang);
    h.update("let 1\n");
    assert!(h.highlights("let 1\n", 0, 1).iter().all(Vec::is_empty));
    assert!(matches!(d.state(), LoadState::Failed(_)), "{:?}", d.state());
    // A failed grammar no longer claims its files.
    assert_eq!(language_for_path("a.skinkmissingx".as_ref()), None);
}
