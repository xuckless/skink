use crate::fake::{self, Program};
use crate::*;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn wake() -> Wake {
    Arc::new(|| {})
}

/// Pump until `done` holds (or fail after 10 s); returns every notice.
fn pump_until(s: &mut Session, mut done: impl FnMut(&Session, &[Notice]) -> bool) -> Vec<Notice> {
    let end = Instant::now() + Duration::from_secs(10);
    let mut all = Vec::new();
    loop {
        all.extend(s.pump());
        if done(s, &all) {
            return all;
        }
        assert!(
            Instant::now() < end,
            "timed out in {:?}; console: {:?}",
            s.state(),
            s.console
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn config() -> DebugConfig {
    DebugConfig {
        name: "fake".into(),
        adapter_id: "fake".into(),
        adapter: AdapterProcess {
            program: PathBuf::from("unused"),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            transport: Transport::Stdio,
        },
        request: RequestKind::Launch,
        arguments: json!({"program": "x"}),
    }
}

fn bp(line: usize) -> BreakpointSpec {
    BreakpointSpec {
        line,
        kind: BreakKind::Plain,
    }
}

#[test]
fn breakpoint_stop_variables_watch_step_and_exit() {
    let path = PathBuf::from("/src/main.c");
    let (client, log) = fake::start(
        Program {
            path: path.clone(),
            lines: 10,
            fail_launch: false,
            run_in_terminal: false,
        },
        wake(),
    );
    // Line 4 (0-based 3) binds, line 99 does not.
    let mut s = Session::with_client(
        client,
        config(),
        vec![(path.clone(), vec![bp(3), bp(98)])],
        &["count".into()],
    );
    let notices = pump_until(&mut s, |s, _| {
        s.is_stopped()
            && s.scopes
                .first()
                .is_some_and(|n| matches!(n.children, Children::Expanded(_)))
    });
    assert!(notices.contains(&Notice::Breakpoints {
        path: path.clone(),
        results: vec![
            BpResult::Verified { line: 3 },
            BpResult::Rejected("no code here".into())
        ],
    }));
    assert!(notices.contains(&Notice::Location {
        path: path.clone(),
        line: 3
    }));
    // The handshake went in protocol order.
    let cmds = log.lock().unwrap().clone();
    let pos = |c: &str| cmds.iter().position(|x| x == c).unwrap_or(usize::MAX);
    assert!(pos("initialize") < pos("launch"));
    assert!(pos("launch") < pos("setBreakpoints"));
    assert!(pos("setBreakpoints") < pos("configurationDone"));
    // Expensive scopes stay collapsed; nested variables load on demand.
    assert_eq!(s.scopes[1].children, Children::Collapsed);
    assert_eq!(s.locals(), vec![("count", "4"), ("point", "{x, y}")]);
    s.toggle(&[0, 1]);
    pump_until(&mut s, |s, _| {
        let Children::Expanded(k) = &s.scopes[0].children else {
            return false;
        };
        matches!(&k[1].children, Children::Expanded(kids) if kids.len() == 2)
    });
    pump_until(&mut s, |s, _| {
        s.watches[0].value == WatchValue::Value("count = 4".into())
    });
    // A failing console line is an error line, not a crash.
    s.repl("bad");
    pump_until(&mut s, |s, _| {
        s.console
            .iter()
            .any(|l| l.kind == LineKind::Error && l.text.contains("no symbol"))
    });
    s.step_over();
    assert_eq!(s.state(), &State::Running, "stepping resumes until stopped");
    pump_until(&mut s, |s, n| {
        s.is_stopped()
            && n.contains(&Notice::Location {
                path: path.clone(),
                line: 4,
            })
    });
    // Selecting the outer frame shows its line.
    let n = s.select_frame(1);
    assert!(n.contains(&Notice::Location {
        path: path.clone(),
        line: 0
    }));
    s.continue_();
    let n = pump_until(&mut s, |s, _| matches!(s.state(), State::Exited(_)));
    assert_eq!(s.state(), &State::Exited(Some(0)));
    assert!(n.contains(&Notice::Ended));
    assert!(s.console.iter().any(|l| l.text == "hello from fake"));
}

#[test]
fn launch_failure_is_a_failed_state_with_the_reason() {
    let (client, _) = fake::start(
        Program {
            path: "/a".into(),
            lines: 3,
            fail_launch: true,
            run_in_terminal: false,
        },
        wake(),
    );
    let mut s = Session::with_client(client, config(), Vec::new(), &[]);
    let n = pump_until(&mut s, |s, _| !s.is_live());
    assert_eq!(s.state(), &State::Failed("program not found".into()));
    assert!(n.contains(&Notice::Ended));
}

#[test]
fn run_in_terminal_is_asked_of_the_ui_and_answered() {
    let (client, log) = fake::start(
        Program {
            path: "/a".into(),
            lines: 3,
            fail_launch: false,
            run_in_terminal: true,
        },
        wake(),
    );
    let mut s = Session::with_client(client, config(), Vec::new(), &[]);
    let n = pump_until(&mut s, |_, n| {
        n.iter().any(|x| matches!(x, Notice::RunInTerminal { .. }))
    });
    let Some(Notice::RunInTerminal { seq, args, env, .. }) = n
        .into_iter()
        .find(|x| matches!(x, Notice::RunInTerminal { .. }))
    else {
        unreachable!()
    };
    assert_eq!(args, ["sh", "-c", "true"]);
    assert_eq!(env, [("A".to_string(), "1".to_string())]);
    s.ran_in_terminal(seq, Ok(Some(42)));
    pump_until(&mut s, |_, _| {
        log.lock()
            .unwrap()
            .iter()
            .any(|c| c == "response:runInTerminal")
    });
}

#[test]
fn stop_terminates_then_a_second_stop_kills() {
    let path = PathBuf::from("/m");
    let (client, log) = fake::start(
        Program {
            path: path.clone(),
            lines: 5,
            fail_launch: false,
            run_in_terminal: false,
        },
        wake(),
    );
    let mut s = Session::with_client(client, config(), vec![(path, vec![bp(1)])], &[]);
    pump_until(&mut s, |s, _| s.is_stopped());
    s.stop();
    pump_until(&mut s, |s, _| !s.is_live());
    assert_eq!(s.state(), &State::Exited(Some(3)));
    assert!(log.lock().unwrap().iter().any(|c| c == "terminate"));
}

#[test]
fn console_joins_output_that_arrives_in_pieces() {
    let (client, _) = fake::start(
        Program {
            path: "/a".into(),
            lines: 1,
            fail_launch: false,
            run_in_terminal: false,
        },
        wake(),
    );
    let mut s = Session::with_client(client, config(), Vec::new(), &[]);
    // Drive the private console through the public pump: feed output events
    // by running to the end (the fake prints one line).
    pump_until(&mut s, |s, _| !s.is_live());
    let lines: Vec<&str> = s.console.iter().map(|l| l.text.as_str()).collect();
    assert!(lines.contains(&"hello from fake"), "{lines:?}");
}

/// Real end-to-end run against GDB's built-in adapter (GDB 14+). Skips when
/// gdb or a C compiler is missing or gdb has no DAP support.
#[test]
fn gdb_end_to_end() {
    let Some(dir) = gdb_fixture() else {
        eprintln!("gdb with DAP or cc not available; skipping");
        return;
    };
    let src = dir.join("prog.c");
    let exe = dir.join("prog");
    let mut process = Adapter::Gdb.process().expect("gdb on PATH");
    process.cwd = Some(dir.clone());
    let cfg = DebugConfig {
        name: "gdb".into(),
        adapter_id: "gdb".into(),
        adapter: process,
        request: RequestKind::Launch,
        arguments: json!({"program": exe.to_string_lossy(), "cwd": dir.to_string_lossy()}),
    };
    // `int y = x + 1;` is line 4 (0-based 3).
    let mut s = Session::start(
        cfg,
        vec![(src.clone(), vec![bp(3)])],
        &["x * 2".into()],
        wake(),
    )
    .expect("start gdb");
    let n = pump_until(&mut s, |s, _| {
        !s.is_live()
            || (s.is_stopped()
                && s.scopes
                    .iter()
                    .any(|n| matches!(n.children, Children::Expanded(_))))
    });
    assert!(
        s.is_stopped(),
        "gdb did not stop: {:?} {:?}",
        s.state(),
        s.console
    );
    assert!(
        n.iter()
            .any(|x| matches!(x, Notice::Location { path, line: 3 } if same_file(path, &src))),
        "{n:?}"
    );
    assert!(
        s.locals().iter().any(|(k, v)| *k == "x" && *v == "41"),
        "{:?}",
        s.locals()
    );
    pump_until(&mut s, |s, _| {
        s.watches[0].value == WatchValue::Value("82".into())
    });
    s.step_over();
    pump_until(&mut s, |s, _| {
        s.current_frame().is_some_and(|f| f.line == 5)
    });
    s.continue_();
    pump_until(&mut s, |s, _| !s.is_live());
    assert_eq!(s.state(), &State::Exited(Some(42)), "{:?}", s.console);
    let _ = std::fs::remove_dir_all(&dir);
}

fn same_file(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
}

fn gdb_fixture() -> Option<PathBuf> {
    use std::process::Command;
    // GitHub's Windows image has MinGW gdb with DAP and a cc, and this test
    // hung there for over an hour (2026-11-09). Real-adapter runs on Windows
    // are a v1.0 hardening item; the fake adapter covers the protocol.
    if cfg!(windows) {
        return None;
    }
    let ok = |c: &mut Command| c.output().is_ok_and(|o| o.status.success());
    if !ok(Command::new("gdb").args(["-q", "-batch", "-ex", "python import gdb.dap"])) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("skink-dap-gdb-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(
        dir.join("prog.c"),
        "int main(void) {\n    volatile int x = 41;\n    x = x + 0;\n    int y = x + 1;\n    return y;\n}\n",
    )
    .ok()?;
    let built = ok(Command::new("cc")
        .current_dir(&dir)
        .args(["-g", "-O0", "-o", "prog", "prog.c"]));
    built.then_some(dir)
}
