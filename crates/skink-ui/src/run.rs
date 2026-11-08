//! Shell commands whose output is shown in a view tab (`:run`, `:diff`,
//! `:git`). They run on a background thread so a slow command never
//! freezes the editor; the result is delivered through [`Jobs`].

use crate::doc::DocId;
use skink_term::WakeFn;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

/// Longest a background command may run before it is killed.
const JOB_TIMEOUT: Duration = Duration::from_secs(600);

/// A finished command: the text for the view tab `doc`.
pub struct JobDone {
    pub doc: DocId,
    pub title: String,
    pub text: String,
}

pub struct Jobs {
    tx: Sender<JobDone>,
    rx: Receiver<JobDone>,
    running: usize,
}

impl Default for Jobs {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { tx, rx, running: 0 }
    }
}

impl Jobs {
    /// Run `cmdline` in `cwd` on a worker thread; `wake` fires when it is done.
    pub fn spawn(
        &mut self,
        doc: DocId,
        title: &str,
        cmdline: &str,
        cwd: PathBuf,
        wake: Option<WakeFn>,
    ) {
        let tx = self.tx.clone();
        let (title, cmdline) = (title.to_owned(), cmdline.to_owned());
        self.running += 1;
        std::thread::spawn(move || {
            let (out, ok) = run_capture(&cmdline, Some(&cwd), JOB_TIMEOUT);
            let mut text = format!("$ {cmdline}\n\n{out}");
            if !ok && !out.contains("[killed") {
                text.push_str("\n[command failed]\n");
            }
            let _ = tx.send(JobDone { doc, title, text });
            if let Some(w) = wake {
                w();
            }
        });
    }

    pub fn try_recv(&mut self) -> Option<JobDone> {
        let done = self.rx.try_recv().ok()?;
        self.running -= 1;
        Some(done)
    }

    /// Wait for every running job (tests).
    #[cfg(test)]
    pub fn wait_all(&mut self) -> Vec<JobDone> {
        let mut out = Vec::new();
        while self.running > 0 {
            match self.rx.recv_timeout(Duration::from_secs(20)) {
                Ok(d) => {
                    self.running -= 1;
                    out.push(d);
                }
                Err(_) => break,
            }
        }
        out
    }
}

/// Output kept per command; more is discarded and the command stopped.
const MAX_OUTPUT: usize = 4 << 20;
/// After the command exits, how long to keep reading output that a
/// background child (`cmd &`, a pipeline leftover) may still be holding.
const DRAIN_GRACE: Duration = Duration::from_millis(300);

enum Chunk {
    Data(Vec<u8>),
    Eof,
}

/// Forward everything read from `r` to `tx`, then `Eof`.
fn pump(mut r: impl Read + Send + 'static, tx: Sender<Chunk>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(Chunk::Data(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = tx.send(Chunk::Eof);
    });
}

/// Run `cmdline` through the platform shell. Returns stdout and stderr
/// interleaved in arrival order, and whether it succeeded. A command that
/// outlives `timeout` or floods more than [`MAX_OUTPUT`] is killed. Never
/// blocks on a pipe held open by a background grandchild.
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
    let (tx, rx) = mpsc::channel();
    let mut open_pipes = 0;
    if let Some(s) = child.stdout.take() {
        pump(s, tx.clone());
        open_pipes += 1;
    }
    if let Some(s) = child.stderr.take() {
        pump(s, tx.clone());
        open_pipes += 1;
    }
    drop(tx);

    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut status: Option<bool> = None;
    let mut note = None;
    let mut drain_until = None;
    loop {
        if status.is_none() {
            let now = Instant::now();
            let stop = if out.len() >= MAX_OUTPUT {
                Some(format!("[output over {} MB, stopped]", MAX_OUTPUT >> 20))
            } else if now >= deadline {
                Some(format!("[killed after {}s]", timeout.as_secs()))
            } else {
                None
            };
            match child.try_wait() {
                Ok(Some(st)) => status = Some(st.success()),
                Ok(None) if stop.is_some() => {
                    let _ = child.kill();
                    let _ = child.wait();
                    status = Some(false);
                    note = stop;
                }
                Ok(None) => {}
                Err(_) => status = Some(false),
            }
            if status.is_some() {
                drain_until = Some(now + DRAIN_GRACE);
            }
        }
        if status.is_some() && (open_pipes == 0 || drain_until.is_some_and(|t| Instant::now() >= t))
        {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(Chunk::Data(d)) => {
                let room = MAX_OUTPUT.saturating_sub(out.len());
                out.extend_from_slice(&d[..d.len().min(room)]);
            }
            Ok(Chunk::Eof) => open_pipes -= 1,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => open_pipes = 0,
        }
    }
    let mut text = String::from_utf8_lossy(&out).into_owned();
    if let Some(n) = note {
        text.push_str(&format!("\n{n}\n"));
    }
    (text, status == Some(true))
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
    fn background_child_holding_the_pipe_does_not_hang() {
        let start = Instant::now();
        let (t, ok) = run_capture("echo done; sleep 30 &", None, Duration::from_secs(10));
        assert!(ok && t.contains("done"), "{t}");
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn flooding_output_is_capped_and_stopped() {
        let (t, ok) = run_capture("yes", None, Duration::from_secs(30));
        assert!(!ok);
        assert!(t.len() <= MAX_OUTPUT + 64 && t.contains("stopped"));
    }

    #[test]
    fn kills_on_timeout() {
        let (t, ok) = run_capture("echo hi; sleep 30", None, Duration::from_millis(200));
        assert!(!ok && t.contains("hi") && t.contains("killed"));
    }
}
