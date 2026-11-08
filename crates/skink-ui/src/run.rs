//! Run a shell command and capture its output, with a deadline.

use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Returns (combined stdout+stderr, success). A command that outlives
/// `timeout` is killed and its partial output returned with `false`.
pub fn run_capture(cmdline: &str, cwd: Option<&Path>, timeout: Duration) -> (String, bool) {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", cmdline]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", cmdline]);
        c
    };
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (format!("cannot run `{cmdline}`: {e}\n"), false),
    };
    let read = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            buf
        })
    };
    let out = child.stdout.take().map(|s| read(Box::new(s)));
    let err = child.stderr.take().map(|s| read(Box::new(s)));
    let deadline = Instant::now() + timeout;
    let mut ok = false;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                ok = st.success();
                break;
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(_) => break,
        }
    }
    let mut text = String::new();
    for h in [out, err].into_iter().flatten() {
        if let Ok(b) = h.join() {
            text.push_str(&String::from_utf8_lossy(&b));
        }
    }
    if timed_out {
        text.push_str(&format!("\n[killed after {}s]\n", timeout.as_secs()));
    }
    (text, ok)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn captures_both_streams_and_status() {
        let (t, ok) = run_capture(
            "echo out; echo err 1>&2; exit 3",
            None,
            Duration::from_secs(5),
        );
        assert!(t.contains("out") && t.contains("err"));
        assert!(!ok);
        assert!(run_capture("true", None, Duration::from_secs(5)).1);
    }

    #[test]
    fn kills_on_timeout() {
        let (t, ok) = run_capture("echo hi; sleep 30", None, Duration::from_millis(200));
        assert!(!ok && t.contains("hi") && t.contains("killed"));
    }
}
