//! Timing benches (ignored; numbers go to TODO.md):
//! `cargo test --release -p skink-ui --lib bench_ -- --ignored --nocapture --test-threads=1`
use super::*;
use ratatui::{
    Terminal,
    backend::TestBackend,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
};
use std::time::{Duration, Instant};

fn app() -> App {
    let mut a = App::new(
        Editor::new(Buffer::empty()),
        Keymap::defaults(),
        Backend::Local,
    );
    a.state_dir = None;
    a
}

fn draw(t: &mut Terminal<TestBackend>, a: &mut App) {
    t.draw(|f| a.render(f)).unwrap();
}

fn press(a: &mut App, code: KeyCode, mods: KeyModifiers) {
    a.handle_key(KeyEvent::new(code, mods));
}

fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skink-bench-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

#[test]
#[ignore = "bench"]
fn bench_100mb_file_open_and_scroll() {
    let d = dir("big");
    let file = d.join("big.txt");
    let line =
        "the quick brown fox jumps over the lazy dog 0123456789 abcdefghijklmnopqrstuvwxyz\n";
    let n = 100 * 1024 * 1024 / line.len();
    std::fs::write(&file, line.repeat(n)).unwrap();
    let rss0 = rss_mb();
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut a = app();
    let t0 = Instant::now();
    a.open_path(&file);
    draw(&mut t, &mut a);
    let open = t0.elapsed();
    let t1 = Instant::now();
    for _ in 0..200 {
        press(&mut a, KeyCode::PageDown, KeyModifiers::NONE);
        draw(&mut t, &mut a);
    }
    let pages = t1.elapsed() / 200;
    let t2 = Instant::now();
    press(&mut a, KeyCode::End, KeyModifiers::CONTROL);
    draw(&mut t, &mut a);
    let end = t2.elapsed();
    eprintln!(
        "BENCH 100 MB file ({n} lines): open + first frame {}, PageDown + frame {}, Ctrl-End + frame {}, RSS +{:.0} MB",
        ms(open),
        ms(pages),
        ms(end),
        rss_mb() - rss0
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
#[ignore = "bench"]
fn bench_1m_line_scroll() {
    let d = dir("lines");
    let file = d.join("lines.rs");
    let mut s = String::new();
    for i in 0..1_000_000 {
        s.push_str(&format!("let v{i} = {i};\n"));
    }
    std::fs::write(&file, &s).unwrap();
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut a = app();
    let t0 = Instant::now();
    a.open_path(&file);
    draw(&mut t, &mut a);
    let open = t0.elapsed();
    let t1 = Instant::now();
    for _ in 0..1000 {
        press(&mut a, KeyCode::PageDown, KeyModifiers::NONE);
        draw(&mut t, &mut a);
    }
    let pages = t1.elapsed() / 1000;
    let t2 = Instant::now();
    for _ in 0..1000 {
        a.doc.editor.scroll_by(3);
        draw(&mut t, &mut a);
    }
    let wheel = t2.elapsed() / 1000;
    eprintln!(
        "BENCH 1 M lines ({} MB, .rs): open + first frame {}, PageDown + frame {}, wheel + frame {}",
        s.len() >> 20,
        ms(open),
        ms(pages),
        ms(wheel)
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
#[ignore = "bench"]
fn bench_10k_file_picker() {
    let d = dir("picker");
    std::fs::create_dir_all(d.join(".git")).unwrap();
    for i in 0..10_000 {
        let sub = d.join(format!("src/m{}", i / 100));
        if i % 100 == 0 {
            std::fs::create_dir_all(&sub).unwrap();
        }
        std::fs::write(sub.join(format!("file{i}.rs")), "").unwrap();
    }
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut a = app();
    a.set_wake(std::sync::Arc::new(|| {})); // stream the walk, as the TUI does
    a.open_path(&d.join("src/m0/file0.rs"));
    let t0 = Instant::now();
    press(&mut a, KeyCode::Char('p'), KeyModifiers::ALT);
    draw(&mut t, &mut a);
    let first = t0.elapsed();
    let deadline = Instant::now() + Duration::from_secs(30);
    let files = loop {
        a.poll_background();
        if let Overlay::Picker(p) = &a.overlay
            && let crate::picker::Walk::Done { files } = p.walk
        {
            break files;
        }
        assert!(Instant::now() < deadline, "walk never finished");
        std::thread::sleep(Duration::from_millis(1));
    };
    let walked = t0.elapsed();
    let t1 = Instant::now();
    for c in "f9999".chars() {
        press(&mut a, KeyCode::Char(c), KeyModifiers::NONE);
        draw(&mut t, &mut a);
    }
    let typed = t1.elapsed() / 5;
    eprintln!(
        "BENCH picker over {files} files: open + frame {}, walk done {}, keystroke + rank + frame {}",
        ms(first),
        ms(walked),
        ms(typed)
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
#[ignore = "bench"]
#[cfg(unix)]
fn bench_50_terminal_tabs() {
    let d = dir("terms");
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut a = app();
    a.set_tree_root(d.clone(), false);
    a.set_wake(std::sync::Arc::new(|| {}));
    let rss0 = rss_mb();
    let t0 = Instant::now();
    for _ in 0..50 {
        a.run_chain("NewTerminal");
    }
    draw(&mut t, &mut a);
    let opened = t0.elapsed();
    std::thread::sleep(Duration::from_millis(500));
    a.poll_background();
    let t1 = Instant::now();
    for _ in 0..50 {
        a.run_chain("NextTab");
        draw(&mut t, &mut a);
    }
    let switch = t1.elapsed() / 50;
    eprintln!(
        "BENCH 50 terminal tabs: open all + frame {}, tab switch + frame {}, RSS +{:.0} MB (own process; shells are separate)",
        ms(opened),
        ms(switch),
        rss_mb() - rss0
    );
    let _ = std::fs::remove_dir_all(&d);
}
