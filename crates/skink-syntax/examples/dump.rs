//! Prints highlight classes for a file: `cargo run -p skink-syntax --example dump -- FILE`.
use skink_syntax::{Highlighter, language_for_path};
fn main() {
    let path = std::env::args().nth(1).expect("file");
    let text = std::fs::read_to_string(&path).unwrap();
    let lang = language_for_path(path.as_ref()).expect("unknown language");
    let mut h = Highlighter::new(lang);
    h.update(&text);
    let n = text.lines().count();
    for (i, spans) in h.highlights(&text, 0, n).iter().enumerate() {
        let line: Vec<char> = text.lines().nth(i).unwrap().chars().collect();
        let mut s = String::new();
        for sp in spans {
            let t: String = line[sp.start..sp.end].iter().collect();
            s.push_str(&format!("[{:?} {:?}] ", sp.hl, t));
        }
        println!("{:>3}: {}", i + 1, s);
    }
}
