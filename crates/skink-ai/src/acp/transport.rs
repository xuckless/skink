//! Byte transport under the ACP client: any `(Read, Write)` pair, optionally
//! with an owned child process, plus a `spawn` convenience for standalone use.
//!
//! Inside the Skink app the agent process is normally run by the host
//! (terminal pane / EveryAI); the host then builds a [`Transport`] from the
//! process' piped stdio, or from a socket, and never calls [`spawn`].

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::agent::AgentSpec;

/// A process the client owns and must reap. Implemented for `std::process::Child`
/// by [`spawn`]; hosts can implement it for their own process handles.
pub trait AgentProcess: Send {
    /// Ask the process to stop now (kill). Must not block.
    fn terminate(&mut self);
    /// Wait up to `grace` for exit, then kill; returns the exit code if any.
    fn reap(&mut self, grace: Duration) -> Option<i32>;
    /// Last lines the process wrote to stderr (diagnostics for `Exited`).
    fn stderr_tail(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Reader + writer (+ optional child) the client talks over.
pub struct Transport {
    /// Agent -> client stream (the agent's stdout).
    pub reader: Box<dyn Read + Send>,
    /// Client -> agent stream (the agent's stdin). Dropping it signals EOF.
    pub writer: Box<dyn Write + Send>,
    pub process: Option<Box<dyn AgentProcess>>,
}

impl Transport {
    pub fn new(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static) -> Self {
        Self {
            reader: Box::new(reader),
            writer: Box::new(writer),
            process: None,
        }
    }
    #[must_use]
    pub fn with_process(mut self, p: Box<dyn AgentProcess>) -> Self {
        self.process = Some(p);
        self
    }
}

const STDERR_KEEP: usize = 50;
const STDERR_LINE_MAX: usize = 500;

struct ChildProcess {
    child: Child,
    stderr: Arc<Mutex<VecDeque<String>>>,
    done: bool,
}

impl AgentProcess for ChildProcess {
    fn terminate(&mut self) {
        if !self.done {
            let _ = self.child.kill();
        }
    }
    fn reap(&mut self, grace: Duration) -> Option<i32> {
        let deadline = Instant::now() + grace;
        loop {
            match self.child.try_wait() {
                Ok(Some(st)) => {
                    self.done = true;
                    return st.code();
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                Ok(None) | Err(_) => break,
            }
        }
        let _ = self.child.kill();
        self.done = true;
        self.child.wait().ok().and_then(|s| s.code())
    }
    fn stderr_tail(&self) -> Vec<String> {
        self.stderr
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Start `spec` with piped stdio. stderr is drained on a thread (last
/// [`STDERR_KEEP`] lines kept) so a chatty agent can never block on it.
pub fn spawn(spec: &AgentSpec) -> io::Result<Transport> {
    let mut cmd = Command::new(resolve_program(&spec.program));
    cmd.args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn()?;
    let missing = || io::Error::other("child stdio not piped");
    let stdin = child.stdin.take().ok_or_else(missing)?;
    let stdout = child.stdout.take().ok_or_else(missing)?;
    let stderr = child.stderr.take().ok_or_else(missing)?;

    let tail = Arc::new(Mutex::new(VecDeque::new()));
    let t2 = Arc::clone(&tail);
    let _ = thread::Builder::new()
        .name("acp-stderr".into())
        .stack_size(64 * 1024)
        .spawn(move || drain_stderr(stderr, &t2));
    Ok(
        Transport::new(stdout, stdin).with_process(Box::new(ChildProcess {
            child,
            stderr: tail,
            done: false,
        })),
    )
}

/// Keep the last [`STDERR_KEEP`] lines of `r` in `tail`, each cut to
/// [`STDERR_LINE_MAX`] chars. Memory stays bounded even if the agent writes
/// an endless line without `\n` (a plain `read_until` would buffer it all).
fn drain_stderr(r: impl Read, tail: &Mutex<VecDeque<String>>) {
    let push = |line: &[u8]| {
        let line = String::from_utf8_lossy(line);
        let line: String = line.trim_end().chars().take(STDERR_LINE_MAX).collect();
        if line.is_empty() {
            return;
        }
        let mut q = tail.lock().unwrap_or_else(PoisonError::into_inner);
        if q.len() == STDERR_KEEP {
            q.pop_front();
        }
        q.push_back(line);
    };
    let mut r = BufReader::new(r);
    let mut line = Vec::new();
    loop {
        let buf = match r.fill_buf() {
            Ok([]) => break,
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let (chunk, eol) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..i], true),
            None => (buf, false),
        };
        // Up to 4 bytes per char: enough for STDERR_LINE_MAX chars.
        let room = (4 * STDERR_LINE_MAX).saturating_sub(line.len());
        line.extend_from_slice(&chunk[..chunk.len().min(room)]);
        let used = chunk.len() + usize::from(eol);
        r.consume(used);
        if eol {
            push(&line);
            line.clear();
        }
    }
    push(&line);
}

/// Rust's `Command` only finds `.exe` on Windows; tools like `npx` are `.cmd`.
#[cfg(windows)]
fn resolve_program(program: &str) -> std::ffi::OsString {
    use std::path::Path;
    if Path::new(program).extension().is_some() || program.contains(['\\', '/']) {
        return program.into();
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for ext in ["exe", "cmd", "bat"] {
                let cand = dir.join(format!("{program}.{ext}"));
                if cand.is_file() {
                    return cand.into_os_string();
                }
            }
        }
    }
    program.into()
}

#[cfg(not(windows))]
fn resolve_program(program: &str) -> std::ffi::OsString {
    program.into()
}

/// Write half of an in-memory pipe (see [`memory_pipe`]).
pub struct PipeWriter(Sender<Vec<u8>>);
/// Read half of an in-memory pipe. Returns EOF once the writer is dropped.
pub struct PipeReader {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

/// An in-process byte pipe (for tests and for hosts that run the agent as a
/// thread). Writes never block; `flush` is a no-op.
pub fn memory_pipe() -> (PipeWriter, PipeReader) {
    let (tx, rx) = mpsc::channel();
    (
        PipeWriter(tx),
        PipeReader {
            rx,
            buf: Vec::new(),
            pos: 0,
        },
    )
}

impl Write for PipeWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        self.0
            .send(data.to_vec())
            .map(|()| data.len())
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.pos >= self.buf.len() {
            match self.rx.recv() {
                Ok(b) => {
                    self.buf = b;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_roundtrip_and_eof() {
        let (mut w, mut r) = memory_pipe();
        w.write_all(b"hello ").unwrap();
        w.write_all(b"world").unwrap();
        drop(w);
        let mut s = String::new();
        r.read_to_string(&mut s).unwrap();
        assert_eq!(s, "hello world");
    }

    #[test]
    fn pipe_broken_when_reader_gone() {
        let (mut w, r) = memory_pipe();
        drop(r);
        assert!(w.write_all(b"x").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn spawn_echo_and_reap() {
        let spec = AgentSpec::new("sh").args(["-c", "echo '{\"a\":1}'; echo oops >&2"]);
        let mut t = spawn(&spec).unwrap();
        let mut s = String::new();
        t.reader.read_to_string(&mut s).unwrap();
        assert_eq!(s.trim(), "{\"a\":1}");
        let mut p = t.process.take().unwrap();
        assert_eq!(p.reap(Duration::from_secs(2)), Some(0));
        thread::sleep(Duration::from_millis(50));
        assert_eq!(p.stderr_tail(), vec!["oops".to_string()]);
    }

    #[test]
    fn stderr_tail_is_bounded_without_newlines() {
        // Regression: `read_until` buffered an endless stderr line in full.
        struct Endless(usize);
        impl Read for Endless {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if self.0 == 0 {
                    return Ok(0);
                }
                let n = out.len().min(self.0);
                out[..n].fill(b'x');
                self.0 -= n;
                Ok(n)
            }
        }
        let tail = Mutex::new(VecDeque::new());
        drain_stderr(Endless(64 << 20), &tail);
        let q = tail.into_inner().unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].len(), STDERR_LINE_MAX);

        let tail = Mutex::new(VecDeque::new());
        let many: String = (0..80).map(|i| format!("line {i}\n\n")).collect();
        drain_stderr(many.as_bytes(), &tail);
        let q = tail.into_inner().unwrap();
        assert_eq!(q.len(), STDERR_KEEP);
        assert_eq!(q.back().map(String::as_str), Some("line 79"));
    }

    #[test]
    fn spawn_missing_program_errors() {
        assert!(spawn(&AgentSpec::new("definitely-not-a-real-binary-xyz")).is_err());
    }
}
