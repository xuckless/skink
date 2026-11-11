//! Drive a TUI program in a real pty and read its screen back: the v1.0
//! abuse harness. `cargo run --release -p skink-term --example drive --
//! SCRIPT`. Environment: DRIVE_CMD (program), DRIVE_ARGS (space separated),
//! DRIVE_CWD, plus anything else passed through.
//!
//! Script lines (`#` comments):
//!   keys TEXT      bytes with escapes: \e \r \n \t \xNN \\ and <name> keys
//!   type TEXT      literal text
//!   paste FILE     bracketed paste of a file's bytes
//!   rawpaste FILE  the file's bytes without paste brackets
//!   sleep MS       wait
//!   settle         wait until the screen stops changing (max 3 s)
//!   resize R C     resize the pty
//!   dump [TAG]     print the screen (cursor marked) to stdout
//!   expect TEXT    the screen must contain TEXT (within 3 s), else FAIL
//!   absent TEXT    the screen must not contain TEXT
//!   alive          responsiveness check (resize-redraw within 2 s)
//!   fuzz N SEED [nosave]  N random input events, alive checks every 50
//!   idlecpu        CPU ms over 2 s of idle, FAIL if > 100
//!   quit           Ctrl-Q x3, then wait for exit (5 s)
//!   kill SIG       send a signal to the program (TERM, HUP, KILL, INT)
//! Prints FAIL lines for violations and a final summary; exit code 1 if any.

use skink_term::{CommandSpec, Screen, Term};
use std::time::{Duration, Instant};

fn main() {
    let script = std::env::args().nth(1).expect("script path");
    let text = std::fs::read_to_string(&script).expect("read script");
    let cmd = std::env::var("DRIVE_CMD").unwrap_or_else(|_| "skink".into());
    let args: Vec<String> = std::env::var("DRIVE_ARGS")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let mut spec = CommandSpec::new(cmd).args(args);
    if let Ok(cwd) = std::env::var("DRIVE_CWD") {
        spec = spec.cwd(cwd);
    }
    for (k, v) in std::env::vars() {
        spec = spec.env(k, v);
    }
    let (mut rows, mut cols) = (30u16, 100u16);
    let t = Term::spawn(spec, rows, cols, 2000).expect("spawn");
    let pid = child_pid();
    let mut fails = 0usize;
    let mut fail = |msg: String, t: &Term| {
        fails += 1;
        println!("FAIL: {msg}");
        println!("{}", render(&t.screen()));
    };
    // First frame.
    if !t.wait_for(Duration::from_secs(5), |s| !s.text().trim().is_empty()) {
        fail("no first frame in 5 s".into(), &t);
    }
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let (op, rest) = line.split_once(' ').unwrap_or((line, ""));
        if !t.is_alive() && op != "dump" {
            fail(
                format!("line {}: program exited before `{line}`", n + 1),
                &t,
            );
            break;
        }
        match op {
            "keys" => send(&t, &unescape(rest)),
            "type" => send(&t, rest.as_bytes()),
            "paste" | "rawpaste" => {
                let data = std::fs::read(rest).expect("paste file");
                let mut b = Vec::new();
                if op == "paste" {
                    b.extend_from_slice(b"\x1b[200~");
                }
                if op == "paste" {
                    b.extend_from_slice(&data);
                } else {
                    // A terminal pastes newlines as CR (the Enter key).
                    b.extend(data.iter().map(|&c| if c == b'\n' { b'\r' } else { c }));
                }
                if op == "paste" {
                    b.extend_from_slice(b"\x1b[201~");
                }
                send(&t, &b);
            }
            "sh" => {
                let mut c = std::process::Command::new("sh");
                if let Ok(d) = std::env::var("DRIVE_CWD") {
                    c.current_dir(d);
                }
                let st = c.args(["-c", rest]).status();
                println!("sh {rest:?}: {st:?}");
            }
            "sleep" => std::thread::sleep(Duration::from_millis(rest.parse().unwrap_or(100))),
            "settle" => settle(&t),
            "resize" => {
                let mut it = rest
                    .split_whitespace()
                    .map(|x| x.parse::<u16>().unwrap_or(1));
                rows = it.next().unwrap_or(24);
                cols = it.next().unwrap_or(80);
                let _ = t.resize(rows, cols);
            }
            "dump" => {
                settle(&t);
                println!("--- dump {rest} ({rows}x{cols})");
                println!("{}", render(&t.screen()));
            }
            "expect" => {
                if !t.wait_for(Duration::from_secs(3), |s| s.text().contains(rest)) {
                    fail(format!("line {}: expected {rest:?}", n + 1), &t);
                }
            }
            "absent" => {
                settle(&t);
                if t.screen().text().contains(rest) {
                    fail(format!("line {}: unexpected {rest:?}", n + 1), &t);
                }
            }
            "alive" => {
                if let Some(m) = alive(&t, rows, cols) {
                    fail(format!("line {}: {m}", n + 1), &t);
                }
            }
            "fuzz" => {
                let mut it = rest.split_whitespace();
                let count: usize = it.next().and_then(|x| x.parse().ok()).unwrap_or(500);
                let seed: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
                let nosave = it.next() == Some("nosave");
                let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
                let mut log: Vec<String> = Vec::new();
                for i in 0..count {
                    let (desc, bytes, resize) = event(&mut rng, nosave, rows, cols);
                    log.push(desc);
                    if log.len() > 40 {
                        log.remove(0);
                    }
                    match resize {
                        Some((r, c)) => {
                            rows = r;
                            cols = c;
                            let _ = t.resize(r, c);
                        }
                        None => send(&t, &bytes),
                    }
                    if rng.below(8) == 0 {
                        std::thread::sleep(Duration::from_millis(rng.below(30)));
                    }
                    if !t.is_alive() {
                        fail(
                            format!(
                                "fuzz seed {seed} event {i}: program exited; last events: {log:?}"
                            ),
                            &t,
                        );
                        break;
                    }
                    if i % 50 == 49 {
                        if rows < 4 || cols < 20 {
                            rows = 30;
                            cols = 100;
                            let _ = t.resize(rows, cols);
                        }
                        if let Some(m) = alive(&t, rows, cols) {
                            fail(
                                format!("fuzz seed {seed} event {i}: {m}; last events: {log:?}"),
                                &t,
                            );
                            break;
                        }
                        if t.screen().text().contains("panicked") {
                            fail(format!("fuzz seed {seed} event {i}: panic on screen"), &t);
                            break;
                        }
                    }
                }
                println!("fuzz seed {seed}: {count} events done");
            }
            "idlecpu" => {
                settle(&t);
                std::thread::sleep(Duration::from_millis(500));
                let a = cpu_ms(pid);
                std::thread::sleep(Duration::from_secs(2));
                let b = cpu_ms(pid);
                let used = b.saturating_sub(a);
                println!("idle cpu {used} ms / 2 s, rss {} MB", rss_mb(pid));
                if used > 100 {
                    fail(format!("line {}: idle CPU {used} ms over 2 s", n + 1), &t);
                }
            }
            "quit" => {
                for _ in 0..3 {
                    send(&t, b"\x11");
                    std::thread::sleep(Duration::from_millis(150));
                    // Answer a save prompt with "no".
                    send(&t, b"n");
                    std::thread::sleep(Duration::from_millis(150));
                    if !t.is_alive() {
                        break;
                    }
                }
                let end = Instant::now() + Duration::from_secs(5);
                while t.is_alive() && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(50));
                }
                if t.is_alive() {
                    fail(format!("line {}: still running after quit", n + 1), &t);
                }
            }
            "kill" => {
                let sig = match rest {
                    "KILL" => "-KILL",
                    "HUP" => "-HUP",
                    "INT" => "-INT",
                    _ => "-TERM",
                };
                let _ = std::process::Command::new("kill")
                    .args([sig, &pid.to_string()])
                    .status();
                std::thread::sleep(Duration::from_millis(500));
            }
            other => panic!("line {}: unknown op {other}", n + 1),
        }
    }
    let status = t.exit_status();
    println!(
        "SUMMARY fails={fails} alive={} exit={status:?} rss={} MB",
        t.is_alive(),
        rss_mb(pid)
    );
    if t.is_alive() {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
    std::process::exit(i32::from(fails > 0));
}

fn send(t: &Term, b: &[u8]) {
    // Large writes in chunks so the pty buffer never stalls the harness.
    for c in b.chunks(4096) {
        let _ = t.write_input(c);
    }
    // A lone Esc followed at once by the next key reads as Alt+key.
    if b.last() == Some(&0x1b) {
        std::thread::sleep(Duration::from_millis(60));
    }
}

fn settle(t: &Term) {
    let end = Instant::now() + Duration::from_secs(3);
    let mut last = t.screen().text();
    loop {
        std::thread::sleep(Duration::from_millis(120));
        let now = t.screen().text();
        if now == last || Instant::now() > end {
            return;
        }
        last = now;
    }
}

/// Resize by one column and back: a live program redraws within 2 s.
fn alive(t: &Term, rows: u16, cols: u16) -> Option<String> {
    let before = t.screen();
    let start = Instant::now();
    let _ = t.resize(rows, cols + 1);
    let changed = t.wait_for(Duration::from_secs(10), |s| {
        s.cols != before.cols && s.text() != before.text()
    });
    let took = start.elapsed();
    let wide = t.screen().text();
    let _ = t.resize(rows, cols);
    // Let the redraw at the old size land so the next check compares fresh.
    let _ = t.wait_for(Duration::from_secs(10), |s| s.text() != wide);
    if !t.is_alive() {
        return Some("program exited during alive check".into());
    }
    if !changed {
        return Some("no redraw within 10 s of a resize (hang)".into());
    }
    if took > Duration::from_secs(2) {
        return Some(format!(
            "redraw took {:.1} s after a resize (stall)",
            took.as_secs_f64()
        ));
    }
    None
}

/// Screen text with the cursor shown as a reverse block marker `▮`.
fn render(s: &Screen) -> String {
    let mut out = String::new();
    for r in 0..s.rows {
        let mut line = String::new();
        for c in 0..s.cols {
            let cell = &s.cells[r as usize * s.cols as usize + c as usize];
            if cell.is_wide_continuation() {
                continue;
            }
            if s.cursor.visible && s.cursor.row == r && s.cursor.col == c {
                line.push('▮');
            }
            cell.push_text(&mut line);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out
}

fn unescape(s: &str) -> Vec<u8> {
    let named = |n: &str| -> Option<&'static [u8]> {
        Some(match n {
            "up" => b"\x1b[A",
            "down" => b"\x1b[B",
            "right" => b"\x1b[C",
            "left" => b"\x1b[D",
            "home" => b"\x1b[H",
            "end" => b"\x1b[F",
            "pgup" => b"\x1b[5~",
            "pgdn" => b"\x1b[6~",
            "del" => b"\x1b[3~",
            "esc" => b"\x1b",
            "enter" => b"\r",
            "tab" => b"\t",
            "stab" => b"\x1b[Z",
            "bs" => b"\x7f",
            "f1" => b"\x1bOP",
            "f2" => b"\x1bOQ",
            "f3" => b"\x1bOR",
            "f4" => b"\x1bOS",
            "f5" => b"\x1b[15~",
            "f6" => b"\x1b[17~",
            "f7" => b"\x1b[18~",
            "f8" => b"\x1b[19~",
            "f9" => b"\x1b[20~",
            "f10" => b"\x1b[21~",
            "f11" => b"\x1b[23~",
            "f12" => b"\x1b[24~",
            "sup" => b"\x1b[1;2A",
            "sdown" => b"\x1b[1;2B",
            "sleft" => b"\x1b[1;2D",
            "sright" => b"\x1b[1;2C",
            "cup" => b"\x1b[1;5A",
            "cdown" => b"\x1b[1;5B",
            "cleft" => b"\x1b[1;5D",
            "cright" => b"\x1b[1;5C",
            "aup" => b"\x1b[1;3A",
            "adown" => b"\x1b[1;3B",
            _ => return None,
        })
    };
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'<' => {
                if let Some(j) = s[i + 1..].find('>')
                    && let Some(k) = named(&s[i + 1..i + 1 + j])
                {
                    out.extend_from_slice(k);
                    i += j + 2;
                    continue;
                }
                // ^X: control key
                out.push(b'<');
            }
            b'^' if i + 1 < b.len() && b[i + 1].is_ascii_alphabetic() => {
                out.push(b[i + 1].to_ascii_lowercase() - b'a' + 1);
                i += 1;
            }
            b'\\' if i + 1 < b.len() => {
                i += 1;
                match b[i] {
                    b'e' => out.push(0x1b),
                    b'r' => out.push(b'\r'),
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b's' => out.push(b' '),
                    b'x' if i + 2 < b.len() => {
                        out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
                        i += 2;
                    }
                    c => out.push(c),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// One random input event: (description, bytes, resize).
fn event(r: &mut Rng, nosave: bool, rows: u16, cols: u16) -> (String, Vec<u8>, Option<(u16, u16)>) {
    const TEXT: &[&str] = &[
        "a",
        "Z",
        " ",
        "fn ",
        "{",
        "}",
        "(",
        ")",
        "\"",
        "'",
        "`",
        "é",
        "日本",
        "👨‍👩‍👧",
        "e\u{301}",
        "\t",
        "שלום",
        "\u{200b}",
        "\u{fe0f}",
        "😀",
        "ｗ",
        "\\",
        "/",
        "$",
        "%",
    ];
    const SEQ: &[&str] = &[
        "\x1b[A",
        "\x1b[B",
        "\x1b[C",
        "\x1b[D",
        "\x1b[H",
        "\x1b[F",
        "\x1b[5~",
        "\x1b[6~",
        "\x1b[3~",
        "\x7f",
        "\r",
        "\x1b",
        "\x1b[Z",
        "\x1b[1;2A",
        "\x1b[1;2B",
        "\x1b[1;2C",
        "\x1b[1;2D",
        "\x1b[1;5C",
        "\x1b[1;5D",
        "\x1b[1;3A",
        "\x1b[1;3B",
        "\x1bOP",
        "\x1b[15~",
        "\x1b[17~",
        "\x1b[18~",
        "\x1b[19~",
        "\x1b[20~",
        "\x1b[21~",
        "\x1b[23~",
        "\x1b[24~",
        "\x1b[1;2P",
        "\x1b[17;2~",
        "\x1b[19;2~",
        "\x1b[23;2~",
    ];
    match r.below(100) {
        0..=34 => {
            let t = TEXT[r.below(TEXT.len() as u64) as usize];
            (format!("text {t:?}"), t.as_bytes().to_vec(), None)
        }
        35..=59 => {
            let s = SEQ[r.below(SEQ.len() as u64) as usize];
            (format!("seq {s:?}"), s.as_bytes().to_vec(), None)
        }
        60..=74 => {
            // Ctrl-letter, skipping Ctrl-S (save) when asked and Ctrl-Q
            // (quit) and Ctrl-Z (suspend: SIGTSTP stops the job) always.
            let mut c = b'a' + r.below(26) as u8;
            // Ctrl-G (AI prompt) would start an agent over npx.
            while (nosave && c == b's') || c == b'q' || c == b'z' || c == b'g' {
                c = b'a' + r.below(26) as u8;
            }
            (format!("ctrl-{}", c as char), vec![c - b'a' + 1], None)
        }
        75..=84 => {
            const ALT: &[u8] = b"abcdefghijklmnopqrstuvwxyz.,ABCDEFGHIJKLMNOPRSTUVWXYZ";
            let c = ALT[r.below(ALT.len() as u64) as usize];
            (format!("alt-{}", c as char), vec![0x1b, c], None)
        }
        85..=92 => {
            // SGR mouse: press/drag/release/wheel anywhere, including off-screen.
            let x = r.below(u64::from(cols) + 20) + 1;
            let y = r.below(u64::from(rows) + 10) + 1;
            let b = [0, 1, 2, 32, 64, 65, 66, 67, 4, 8, 16][r.below(11) as usize];
            let up = if r.below(2) == 0 { 'M' } else { 'm' };
            let s = format!("\x1b[<{b};{x};{y}{up}");
            (format!("mouse {s:?}"), s.into_bytes(), None)
        }
        93..=96 => {
            let n = r.below(300) as usize;
            let mut p = b"\x1b[200~".to_vec();
            for i in 0..n {
                p.extend_from_slice(TEXT[(i * 7 + n) % TEXT.len()].as_bytes());
                if i % 13 == 0 {
                    p.extend_from_slice(b"\r\n");
                }
            }
            p.extend_from_slice(b"\x1b[201~");
            (format!("paste {n}"), p, None)
        }
        _ => {
            let (rr, cc) = match r.below(6) {
                0 => (1, 1),
                1 => (2, 80),
                2 => (200, 5),
                3 => (5, 200),
                4 => (3 + r.below(60) as u16, 10 + r.below(250) as u16),
                _ => (30, 100),
            };
            (format!("resize {rr}x{cc}"), Vec::new(), Some((rr, cc)))
        }
    }
}

/// The newest child of this process (the program in the pty).
fn child_pid() -> u32 {
    let me = std::process::id();
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        let mut best = 0;
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
                    continue;
                };
                let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                    continue;
                };
                let Some((_, rest)) = stat.rsplit_once(')') else {
                    continue;
                };
                let ppid: u32 = rest
                    .split_whitespace()
                    .nth(1)
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(0);
                if ppid == me && pid > best {
                    best = pid;
                }
            }
        }
        if best != 0 || Instant::now() > end {
            return best;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn cpu_ms(pid: u32) -> u64 {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return 0;
    };
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return 0;
    };
    let f: Vec<u64> = rest
        .split_whitespace()
        .map(|x| x.parse().unwrap_or(0))
        .collect();
    (f[11] + f[12]) * 10
}

fn rss_mb(pid: u32) -> f64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}
