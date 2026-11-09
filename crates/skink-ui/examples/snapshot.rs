//! Dev tool: render the real UI (`App::render`) into a static HTML page, so
//! the look can be reviewed without a terminal and README screenshots are
//! made from actual output. Usage:
//!
//!   cargo run -p skink-ui --example snapshot -- <path> <out.html> [options]
//!
//! Options:
//!   --size COLSxROWS     terminal size (default 120x36)
//!   --open a,b,c         open these files as tabs first (the main path last)
//!   --goto LINE:COL      cursor position in the main file
//!   --type TEXT          type TEXT at the cursor (an unsaved edit)
//!   --dirty FILE         make the --open tab FILE unsaved (tab marker)
//!   --term CMD           also open a terminal tab running CMD (via `sh -c`)
//!   --show-term          make the terminal tab the active one
//!   --keys K1,K2         press keys at the end (e.g. `Alt-p`, `Ctrl-e`, `Ctrl-Home`, `F8`, `x`)
//!   --title TEXT         window title in the frame
//!   --frame              wrap the grid in a window frame (for screenshots)
use ratatui::{
    Terminal,
    backend::TestBackend,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    style::{Color, Modifier},
};
use skink_config::Keymap;
use skink_core::{Backend, Buffer, Editor};
use skink_term::CommandSpec;
use skink_ui::App;
use std::{fmt::Write, path::Path, time::Duration};

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

/// `Alt-p`, `Ctrl-e`, `Esc`, `x` -> a key event.
fn key(spec: &str) -> KeyEvent {
    let (mods, name) = match spec.rsplit_once('-') {
        Some((m, k)) if !k.is_empty() => (m, k),
        _ => ("", spec),
    };
    let mut m = KeyModifiers::NONE;
    if mods.contains("Ctrl") {
        m |= KeyModifiers::CONTROL;
    }
    if mods.contains("Alt") {
        m |= KeyModifiers::ALT;
    }
    let code = match name {
        "Esc" => KeyCode::Esc,
        "Enter" => KeyCode::Enter,
        "Down" => KeyCode::Down,
        "Up" => KeyCode::Up,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Tab" => KeyCode::Tab,
        f if f.len() > 1 && f.starts_with('F') && f[1..].parse::<u8>().is_ok() => {
            KeyCode::F(f[1..].parse().unwrap_or(1))
        }
        _ => KeyCode::Char(name.chars().next().expect("key")),
    };
    KeyEvent::new(code, m)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = Path::new(args.get(1).expect("path"));
    let out = args.get(2).expect("out.html");
    let opt = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let flag = |name: &str| args.iter().any(|a| a == name);
    let (w, h) = opt("--size")
        .and_then(|s| {
            let (a, b) = s.split_once('x')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .unwrap_or((120u16, 36u16));

    let mut app = App::new(
        Editor::new(Buffer::empty()),
        Keymap::defaults(),
        Backend::Local,
    );
    app.startup();
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| app.render(f)).unwrap(); // lay out first (terminal size)

    if let Some(cmd) = opt("--term") {
        app.open_terminal(CommandSpec::new("sh").args(["-c", cmd.as_str()]))
            .expect("terminal");
        // Let the program draw, then pull its output in.
        std::thread::sleep(Duration::from_millis(1200));
        app.poll_terminals();
    }
    for f in opt("--open").iter().flat_map(|s| s.split(',')) {
        app.open_path(Path::new(f));
        if opt("--dirty").as_deref() == Some(f) {
            // An unsaved edit, so the tab shows the modified marker.
            app.editor_mut().insert_text(" ");
        }
    }
    if path.is_dir() {
        app.set_tree_root(path.to_path_buf(), true);
    } else {
        app.open_path(path);
        let root = app.project_root();
        app.set_tree_root(root, true);
        // The tree took focus; give it back to the editor (F8 toggles).
        app.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
    }
    if let Some((l, c)) = opt("--goto").and_then(|s| {
        let (l, c) = s.split_once(':')?;
        Some((l.parse().ok()?, c.parse().ok()?))
    }) {
        app.editor_mut().goto(Some(l), Some(c));
    }
    if let Some(text) = opt("--type") {
        app.editor_mut().insert_text(&text);
    }
    if flag("--show-term") {
        app.handle_key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
    }
    for k in opt("--keys").iter().flat_map(|s| s.split(',')) {
        app.handle_key(key(k));
    }
    t.draw(|f| app.render(f)).unwrap();
    // A headless backend has no visible cursor; draw a block like a terminal.
    let cursor = t.get_cursor_position().ok();
    let buf = t.backend().buffer();

    // One <span> per run of same-styled cells; one fixed-height row per line
    // so cell backgrounds join without gaps.
    let mut grid = String::new();
    for y in 0..h {
        grid.push_str("<div class=r>");
        let mut run = String::new();
        let mut style = String::new();
        for x in 0..w {
            let c = &buf[(x, y)];
            let (mut fg, mut bg) = (css(c.fg, "#c0caf5"), css(c.bg, "#1a1b26"));
            if c.modifier.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cursor.is_some_and(|p| (p.x, p.y) == (x, y)) {
                (fg, bg) = (bg, "#c0caf5".to_owned());
            }
            let mut s = format!("color:{fg};background:{bg}");
            if c.modifier.contains(Modifier::BOLD) {
                s.push_str(";font-weight:700");
            }
            if c.modifier.contains(Modifier::ITALIC) {
                s.push_str(";font-style:italic");
            }
            if s != style && !run.is_empty() {
                let _ = write!(grid, "<span style='{style}'>{run}</span>");
                run.clear();
            }
            style = s;
            let sym = c.symbol();
            run.push_str(&match sym {
                "" => " ".to_owned(),
                s => s.replace('&', "&amp;").replace('<', "&lt;"),
            });
        }
        let _ = write!(grid, "<span style='{style}'>{run}</span></div>");
    }

    let font = "'JetBrainsMono Nerd Font Mono','JetBrainsMono NF','JetBrains Mono',monospace";
    let page = if flag("--frame") {
        let title = opt("--title").unwrap_or_else(|| "skink".into());
        format!(
            "<!doctype html><meta charset=utf-8><style>
html,body{{margin:0;background:transparent}}
.wrap{{display:inline-block;padding:28px 40px 56px}}
.win{{border-radius:12px;overflow:hidden;background:#1a1b26;
  box-shadow:0 24px 60px rgba(0,0,0,.45),0 0 0 1px rgba(122,162,247,.18)}}
.bar{{height:34px;background:#16161e;display:flex;align-items:center;padding:0 14px;
  font:600 12.5px/1 system-ui,'Inter','Segoe UI',sans-serif;color:#565f89;position:relative}}
.dot{{width:12px;height:12px;border-radius:50%;margin-right:8px}}
.t{{position:absolute;left:0;right:0;text-align:center;pointer-events:none}}
.g{{padding:6px 8px 8px;font:14px/1 {font};white-space:pre}}
.r{{height:19px;line-height:19px}}
</style><div class=wrap><div class=win><div class=bar>
<span class=dot style='background:#f7768e'></span><span class=dot style='background:#e0af68'></span>
<span class=dot style='background:#9ece6a'></span><span class=t>{title}</span></div>
<div class=g>{grid}</div></div></div>"
        )
    } else {
        format!(
            "<!doctype html><meta charset=utf-8><body style='margin:0;background:#101018'>\
             <div style=\"display:inline-block;padding:12px;background:#1a1b26;font:15px/1 {font};white-space:pre\">\
             <style>.r{{height:19px;line-height:19px}}</style>{grid}</div>"
        )
    };
    std::fs::write(out, page).unwrap();
}
