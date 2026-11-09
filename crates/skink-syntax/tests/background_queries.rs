//! Queries compile on a worker once the TUI asks for it (its own test
//! binary: the hook is process-wide).
use skink_syntax::{Highlighter, Lang};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn first_highlight_is_uncoloured_then_the_hook_fires_and_colours_arrive() {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    skink_syntax::compile_queries_in_background(move || {
        let _ = tx.lock().unwrap().send(());
    });
    let src = "fn main() { let x = 1; }\n";
    let mut h = Highlighter::new(Lang::Rust);
    h.update(src);
    let first = h.highlights(src, 0, 1);
    assert!(first.iter().all(Vec::is_empty), "{first:?}");
    rx.recv_timeout(Duration::from_secs(10))
        .expect("ready hook");
    let later = h.highlights(src, 0, 1);
    assert!(!later[0].is_empty(), "coloured after the wake");

    // A kept render (picker preview) never waits: it compiles inline.
    let mut p = Highlighter::new_inline(Lang::Python);
    let py = "def f():\n    return 1\n";
    p.update(py);
    assert!(!p.highlights(py, 0, 1)[0].is_empty());
}
