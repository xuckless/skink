//! Integration tests against real children on a real pty.
//! Portable tests run everywhere; `sh`-based ones are `#[cfg(unix)]`.

#![cfg_attr(not(unix), allow(unused_imports, dead_code))]
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use skink_term::{CommandSpec, KeyInput, MouseEvent, MouseKind, Term, TermEvent, TermKey};

const T: Duration = Duration::from_secs(10);

#[cfg(unix)]
fn sh(script: &str) -> CommandSpec {
    CommandSpec::new("sh").args(["-c", script])
}

#[cfg(unix)]
fn spawn_sh(script: &str, rows: u16, cols: u16) -> Term {
    Term::spawn(sh(script), rows, cols, 1000).unwrap()
}

fn echo_spec(text: &str) -> CommandSpec {
    if cfg!(windows) {
        CommandSpec::new("cmd").args(["/C", &format!("echo {text}")])
    } else {
        CommandSpec::new("echo").args([text])
    }
}

#[test]
fn spawn_echo_and_read_screen() {
    let t = Term::spawn(echo_spec("hello-skink"), 24, 80, 100).unwrap();
    assert!(t.wait_for(T, |s| s.text().contains("hello-skink")));
    let st = t.wait_exit(T).expect("exits");
    assert!(st.success(), "{st:?}");
    assert!(!t.is_alive());
}

#[cfg(unix)]
#[test]
fn printf_text_and_sgr_colors() {
    let t = spawn_sh(
        r"printf 'plain \033[1;31mred\033[0m \033[38;2;10;20;30mrgb\033[0m\n'",
        5,
        40,
    );
    assert!(t.wait_for(T, |s| s.text().contains("rgb")));
    let s = t.screen();
    let row = s.row_text(0);
    assert_eq!(row, "plain red rgb");
    let red = s.cell(0, 6).unwrap();
    assert_eq!(red.ch, 'r');
    assert_eq!(red.fg, skink_term::Color::Indexed(1));
    assert!(red.attrs.bold());
    assert_eq!(
        s.cell(0, 10).unwrap().fg,
        skink_term::Color::Rgb(10, 20, 30)
    );
    assert_eq!(s.cell(0, 0).unwrap().fg, skink_term::Color::Default);
}

#[cfg(unix)]
#[test]
fn env_cwd_and_term_vars() {
    let dir = std::env::temp_dir();
    let spec = sh("echo \"$TERM|$COLORTERM|$SKINK_HOST_SOCKET|$PWD\"")
        .env("SKINK_HOST_SOCKET", "/tmp/x.sock")
        .cwd(&dir);
    let t = Term::spawn(spec, 5, 200, 0).unwrap();
    assert!(t.wait_for(T, |s| {
        s.text().contains("xterm-256color|truecolor|/tmp/x.sock|")
    }));
    let want = dir.canonicalize().unwrap();
    let text = t.screen().text();
    let pwd = text.rsplit('|').next().unwrap().trim();
    assert_eq!(std::path::Path::new(pwd).canonicalize().unwrap(), want);
}

#[cfg(unix)]
#[test]
fn caller_env_can_override_term() {
    let t = Term::spawn(sh("echo T=$TERM").env("TERM", "vt100"), 3, 30, 0).unwrap();
    assert!(t.wait_for(T, |s| s.text().contains("T=vt100")));
}

#[cfg(unix)]
#[test]
fn resize_reaches_the_child() {
    let t = spawn_sh("read x; stty size", 10, 40);
    t.resize(33, 101).unwrap();
    assert_eq!(t.size(), (33, 101));
    t.write_input(b"\n").unwrap();
    assert!(
        t.wait_for(T, |s| s.text().contains("33 101")),
        "{}",
        t.screen().text()
    );
    let s = t.screen();
    assert_eq!((s.rows, s.cols), (33, 101));
}

#[cfg(unix)]
#[test]
fn alt_screen_flag_and_restore() {
    let t = spawn_sh(
        r"printf 'main'; printf '\033[?1049h\033[2Jalt'; sleep 0.4; printf '\033[?1049l'; sleep 5",
        5,
        20,
    );
    assert!(t.wait_for(T, |s| s.alt_screen && s.text() == "alt"));
    assert!(t.wait_for(T, |s| !s.alt_screen && s.text() == "main"));
}

#[cfg(unix)]
#[test]
fn cursor_movement_and_title() {
    let t = spawn_sh(
        r"printf '\033]2;hi there\007abc\033[2;5HX\033[?25l'; sleep 5",
        5,
        20,
    );
    assert!(t.wait_for(T, |s| s.title == "hi there" && !s.cursor.visible));
    let s = t.screen();
    assert_eq!(s.row_text(0), "abc");
    assert_eq!(s.cell(1, 4).unwrap().ch, 'X');
    assert_eq!((s.cursor.row, s.cursor.col), (1, 5));
}

#[cfg(unix)]
#[test]
fn wide_chars_through_pty() {
    let t = spawn_sh("printf 'a世界b'; sleep 5", 3, 20);
    assert!(t.wait_for(T, |s| s.text().contains('b')));
    let s = t.screen();
    assert_eq!(s.row_text(0), "a世界b");
    assert_eq!(s.cell(0, 1).unwrap().width, 2);
    assert_eq!(s.cell(0, 2).unwrap().width, 0);
    assert_eq!(s.cell(0, 3).unwrap().width, 2);
    assert_eq!(s.cell(0, 5).unwrap().ch, 'b');
}

#[cfg(unix)]
#[test]
fn child_exit_code_and_event() {
    let t = spawn_sh("exit 7", 3, 20);
    let st = t.wait_exit(T).expect("exited");
    assert_eq!(st.code, 7);
    assert!(!st.success());
    assert!(!t.is_alive());
    assert_eq!(t.exit_status().unwrap().code, 7);
    // The Exited event is delivered.
    let deadline = Instant::now() + T;
    loop {
        match t.events().recv_timeout(Duration::from_millis(100)) {
            Ok(TermEvent::Exited) => break,
            Ok(_) => {}
            Err(_) => assert!(Instant::now() < deadline, "no Exited event"),
        }
    }
}

#[cfg(unix)]
#[test]
fn kill_terminates() {
    let t = spawn_sh("sleep 60", 3, 20);
    assert!(t.is_alive());
    t.kill().unwrap();
    let st = t.wait_exit(T).expect("died");
    assert!(!st.success());
}

#[cfg(target_os = "linux")]
fn pid_gone(pid: u32) -> bool {
    // Reaped means /proc entry is gone (a zombie would still be listed).
    !std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(target_os = "linux")]
#[test]
fn drop_kills_and_reaps_no_zombie() {
    let t = spawn_sh("sleep 60", 3, 20);
    let pid = t.process_id().unwrap();
    let start = Instant::now();
    drop(t);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "drop took {:?}",
        start.elapsed()
    );
    assert!(pid_gone(pid), "child {pid} still present after drop");
}

#[cfg(target_os = "linux")]
#[test]
fn drop_escalates_when_sighup_is_ignored() {
    let t = spawn_sh("trap '' HUP; while :; do sleep 1; done", 3, 20);
    std::thread::sleep(Duration::from_millis(300)); // let the trap install
    let pid = t.process_id().unwrap();
    let start = Instant::now();
    drop(t);
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "drop took {:?}",
        start.elapsed()
    );
    assert!(pid_gone(pid), "child {pid} survived drop");
}

#[cfg(unix)]
#[test]
fn large_output_is_fast_and_scrollback_is_capped() {
    let t = Term::spawn(sh("seq 1 300000"), 24, 80, 500).unwrap();
    let start = Instant::now();
    assert!(t.wait_for(Duration::from_secs(30), |s| s.text().contains("300000")));
    let took = start.elapsed();
    let s = t.screen();
    assert_eq!(s.scrollback_len, 500, "scrollback must be capped");
    assert!(took < Duration::from_secs(20), "took {took:?}");
    // Scrolling back works and is clamped to the cap.
    t.scroll_up(10_000);
    let s = t.screen();
    assert_eq!(s.scroll_offset, 500);
    assert!(!s.cursor.visible);
    let first: u32 = s.row_text(0).trim().parse().unwrap();
    assert!(first > 299_000 - 500 - 24 && first < 300_000, "{first}");
    t.scroll_to_bottom();
    assert_eq!(t.screen().scroll_offset, 0);
}

#[cfg(unix)]
#[test]
fn keys_reach_the_child_encoded() {
    // `cat -v` shows control bytes in caret notation; tty echo shows them too.
    let t = spawn_sh("cat -v", 5, 60);
    t.send_key(KeyInput::new(TermKey::Up)).unwrap();
    t.send_key(KeyInput::new(TermKey::Right).ctrl()).unwrap();
    t.send_key(KeyInput::new(TermKey::F(5))).unwrap();
    t.send_key(KeyInput::new(TermKey::Char('x')).alt()).unwrap();
    assert!(
        t.wait_for(T, |s| s.text().contains("^[[A^[[1;5C^[[15~^[x")),
        "{}",
        t.screen().text()
    );
    t.send_key(skink_config::parse_key("Enter").unwrap())
        .unwrap();
    // Ctrl-D ends cat.
    t.send_key(KeyInput::new(TermKey::Char('d')).ctrl())
        .unwrap();
    t.send_key(KeyInput::new(TermKey::Char('d')).ctrl())
        .unwrap();
    assert!(t.wait_exit(T).is_some());
}

#[cfg(unix)]
#[test]
fn application_cursor_mode_changes_arrows() {
    let t = spawn_sh(r"printf '\033[?1h'; cat -v", 5, 60);
    assert!(t.wait_for(T, |s| s.application_cursor));
    t.send_key(KeyInput::new(TermKey::Up)).unwrap();
    assert!(
        t.wait_for(T, |s| s.text().contains("^[OA")),
        "{}",
        t.screen().text()
    );
}

#[cfg(unix)]
#[test]
fn bracketed_paste_wraps_only_when_enabled() {
    let t = spawn_sh(r"printf '\033[?2004h'; cat -v", 5, 80);
    assert!(t.wait_for(T, |s| s.bracketed_paste));
    t.paste("x\x1b[201~y").unwrap(); // embedded end marker is stripped
    assert!(
        t.wait_for(T, |s| s.text().contains("^[[200~xy^[[201~")),
        "{}",
        t.screen().text()
    );

    let t = spawn_sh("cat -v", 5, 80);
    t.paste("a\nb").unwrap();
    assert!(
        t.wait_for(T, |s| s.text().contains("a")),
        "{}",
        t.screen().text()
    );
    assert!(!t.screen().text().contains("200~"));
}

#[cfg(unix)]
#[test]
fn mouse_reports_only_when_requested() {
    let t = spawn_sh("cat -v", 5, 60);
    let ev = MouseEvent::new(MouseKind::Down(skink_term::MouseButton::Left), 2, 1);
    assert!(!t.send_mouse(ev).unwrap(), "no mouse mode: host handles it");

    let t = spawn_sh(r"printf '\033[?1000h\033[?1006h'; cat -v", 5, 60);
    assert!(t.wait_for(T, |s| s.mouse_mode == skink_term::MouseMode::PressRelease));
    assert!(t.send_mouse(ev).unwrap());
    assert!(
        t.wait_for(T, |s| s.text().contains("^[[<0;3;2M")),
        "{}",
        t.screen().text()
    );
}

#[cfg(unix)]
#[test]
fn wheel_on_alt_screen_without_mouse_becomes_arrows() {
    let t = spawn_sh(r"printf '\033[?1049h'; cat -v", 5, 60);
    assert!(t.wait_for(T, |s| s.alt_screen));
    assert!(
        t.send_mouse(MouseEvent::new(MouseKind::ScrollUp, 0, 0))
            .unwrap()
    );
    assert!(
        t.wait_for(T, |s| s.text().contains("^[[A^[[A^[[A")),
        "{}",
        t.screen().text()
    );
}

#[cfg(unix)]
#[test]
fn terminal_queries_are_answered() {
    // The cursor-position reply is injected as input and echoed by the tty.
    let t = spawn_sh(r"printf 'ab\033[6n'; cat -v", 5, 60);
    assert!(
        t.wait_for(T, |s| s.text().contains("^[[1;3R")),
        "{}",
        t.screen().text()
    );
}

#[cfg(unix)]
#[test]
fn wake_callback_and_events_are_coalesced_and_idle_is_quiet() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = Arc::clone(&wakes);
    let wake: skink_term::WakeFn = Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    });
    let t = Term::spawn_with_wake(
        sh("echo a; sleep 0.3; echo b; sleep 30"),
        5,
        20,
        0,
        Some(wake),
    )
    .unwrap();
    assert_eq!(t.events().recv_timeout(T), Ok(TermEvent::Output));
    let deadline = Instant::now() + T;
    while wakes.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "wake callback never ran");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Consume the screen: re-arms. The next output produces a new event.
    assert!(t.wait_for(T, |s| s.text().contains('b')));
    // Now idle: nothing more arrives (no busy polling / spurious wakes).
    std::thread::sleep(Duration::from_millis(100)); // let in-flight wakes land
    while t.events().try_recv().is_ok() {}
    let before = wakes.load(Ordering::SeqCst);
    assert!(t.events().recv_timeout(Duration::from_millis(500)).is_err());
    assert_eq!(wakes.load(Ordering::SeqCst), before);
}

#[test]
fn spawn_failure_is_an_error() {
    assert!(Term::spawn(CommandSpec::new("/definitely/not/a/program"), 5, 20, 0).is_err());
}

#[test]
fn default_shell_is_nonempty() {
    assert!(!CommandSpec::default_shell().program.is_empty());
}
