//! Dev tool: render the UI for a path into a static HTML page (colors and all),
//! so the look can be reviewed without a terminal. Usage:
//!   cargo run -p skink-ui --example snapshot -- <path> <out.html> [cols rows]
use ratatui::{Terminal, backend::TestBackend, style::Color};
use skink_config::Keymap;
use skink_core::{Backend, Buffer, Editor};
use skink_ui::App;
use std::{fmt::Write, path::Path};

fn css(c: Color, default: &str) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => {
            let (r, g, b) = xterm(i);
            format!("#{r:02x}{g:02x}{b:02x}")
        }
        _ => default.to_owned(),
    }
}

fn xterm(i: u8) -> (u8, u8, u8) {
    match i {
        0..=15 => [
            (0, 0, 0),
            (205, 0, 0),
            (0, 205, 0),
            (205, 205, 0),
            (0, 0, 238),
            (205, 0, 205),
            (0, 205, 205),
            (229, 229, 229),
            (127, 127, 127),
            (255, 0, 0),
            (0, 255, 0),
            (255, 255, 0),
            (92, 92, 255),
            (255, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ][i as usize],
        16..=231 => {
            let n = i - 16;
            let l = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (l(n / 36), l(n / 6 % 6), l(n % 6))
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            (v, v, v)
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = Path::new(args.get(1).expect("path"));
    let out = args.get(2).expect("out.html");
    let w: u16 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(120);
    let h: u16 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(36);

    let (editor, tree) = if path.is_dir() {
        (Editor::new(Buffer::empty()), Some(path.to_path_buf()))
    } else {
        (Editor::new(Buffer::open(path).expect("open")), None)
    };
    let mut app = App::new(editor, Keymap::defaults(), Backend::Local);
    app.startup();
    let root = tree.unwrap_or_else(|| app.project_root());
    app.set_tree_root(std::fs::canonicalize(&root).unwrap_or(root), true);
    if !path.is_dir() {
        app.editor.goto(Some(18), Some(12));
    }
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| app.render(f)).unwrap();
    let buf = t.backend().buffer();

    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><body style='margin:0;background:#101018'><pre style=\"margin:0;padding:12px;font:15px/1.25 'JetBrainsMono Nerd Font','JetBrains Mono','Symbols Nerd Font Mono','Fira Code',monospace;color:#c0caf5;background:#1a1b26;display:inline-block\">",
    );
    for y in 0..h {
        for x in 0..w {
            let c = &buf[(x, y)];
            let fg = css(c.fg, "#c0caf5");
            let bg = css(c.bg, "#1a1b26");
            let bold = c.modifier.contains(ratatui::style::Modifier::BOLD);
            let rev = c.modifier.contains(ratatui::style::Modifier::REVERSED);
            let (fg, bg) = if rev { (bg, fg) } else { (fg, bg) };
            let sym = c.symbol().replace('&', "&amp;").replace('<', "&lt;");
            let _ = write!(
                html,
                "<span style='color:{fg};background:{bg};{}'>{}</span>",
                if bold { "font-weight:700" } else { "" },
                if sym.is_empty() { " " } else { &sym }
            );
        }
        html.push('\n');
    }
    html.push_str("</pre>");
    std::fs::write(out, html).unwrap();
}
