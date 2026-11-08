//! `Term`: pty + reader thread + emulator.
//!
//! Threading model (pump vs. mutex): a reader thread blocks on the pty, and
//! for each chunk (<= 32 KiB) locks the emulator `Mutex`, feeds `vt100`,
//! unlocks, then (if the UI has not yet consumed the last wake-up) sends one
//! coalesced [`TermEvent::Output`] and calls the optional wake callback. The UI
//! thread takes the same mutex only to copy a snapshot. Chosen over a
//! channel + `pump()` because (a) parsing happens off the UI thread, so a
//! flood of output never blocks input/redraw, (b) there is no unbounded queue
//! (backpressure is the pty buffer itself), and (c) latency is one lock handoff.
//! Idea from tui-term's examples (`Arc<RwLock<Parser>>` fed by a reader
//! thread), MIT, Copyright (c) tui-term authors; no code copied.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::emu::{Emu, empty_screen};
use crate::keys::{KeyInput, TermKey, encode_key};
use crate::mouse::{MouseEvent, MouseKind, encode_mouse};
use crate::screen::Screen;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What to run in the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Extra environment, applied after `TERM=xterm-256color` and
    /// `COLORTERM=truecolor` (so callers may override those too).
    pub env: Vec<(String, String)>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        CommandSpec {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The user's shell: `$SHELL` else `/bin/sh` on Unix; `pwsh`, then
    /// `powershell`, then `cmd` on Windows (first found on `PATH`).
    pub fn default_shell() -> Self {
        CommandSpec::new(default_shell_program())
    }
}

#[cfg(unix)]
fn default_shell_program() -> String {
    match std::env::var("SHELL") {
        Ok(s) if !s.is_empty() => s,
        _ => "/bin/sh".to_string(),
    }
}

#[cfg(windows)]
fn default_shell_program() -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in ["pwsh.exe", "powershell.exe"] {
        if std::env::split_paths(&path).any(|d| d.join(name).is_file()) {
            return name.to_string();
        }
    }
    "cmd.exe".to_string()
}

/// Coalesced notifications sent on [`Term::events`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermEvent {
    /// New output arrived since the last snapshot. At most one is pending.
    Output,
    /// The child process exited (read [`Term::exit_status`]).
    Exited,
}

/// How the child ended. On Unix, death by signal reports the signal name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitStatus {
    pub code: u32,
    pub signal: Option<String>,
}

impl ExitStatus {
    pub fn success(&self) -> bool {
        self.code == 0 && self.signal.is_none()
    }
}

/// Callback invoked from the reader thread when [`TermEvent`]s are queued.
/// Keep it tiny and non-blocking (e.g. post a message to the UI loop).
pub type WakeFn = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    emu: Mutex<Emu>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    dirty: AtomicBool,
    tx: Sender<TermEvent>,
    wake: Option<WakeFn>,
    exit: Mutex<Option<ExitStatus>>,
    exit_cv: Condvar,
}

impl Shared {
    fn notify(&self, ev: TermEvent) {
        let send = match ev {
            TermEvent::Output => !self.dirty.swap(true, Ordering::AcqRel),
            TermEvent::Exited => true,
        };
        if send {
            let _ = self.tx.send(ev);
            if let Some(w) = &self.wake {
                w();
            }
        }
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut g = lock(&self.writer);
        match g.as_mut() {
            Some(w) => {
                w.write_all(bytes)?;
                w.flush()
            }
            None => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
        }
    }
}

/// An embedded terminal: a child on a pty plus a screen model of its output.
pub struct Term {
    shared: Arc<Shared>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pid: Option<u32>,
    events: Receiver<TermEvent>,
    reader: Option<JoinHandle<()>>,
    waiter: Option<JoinHandle<()>>,
}

fn io_err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}

impl Term {
    /// Spawn `spec` on a new pty of `rows` x `cols`, keeping at most
    /// `scrollback_lines` lines of history.
    pub fn spawn(
        spec: CommandSpec,
        rows: u16,
        cols: u16,
        scrollback_lines: usize,
    ) -> io::Result<Term> {
        Self::spawn_with_wake(spec, rows, cols, scrollback_lines, None)
    }

    /// Like [`Term::spawn`], with a callback fired (from a background thread)
    /// whenever [`TermEvent`]s are queued, so a UI loop can sleep until needed.
    pub fn spawn_with_wake(
        spec: CommandSpec,
        rows: u16,
        cols: u16,
        scrollback_lines: usize,
        wake: Option<WakeFn>,
    ) -> io::Result<Term> {
        let (rows, cols) = (rows.max(1), cols.max(1));
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io_err)?;

        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let mut child = pair.slave.spawn_command(cmd).map_err(io_err)?;
        // Drop our copy of the slave so EOF reaches the reader when the child
        // (and its descendants) close theirs.
        drop(pair.slave);
        let pid = child.process_id();
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().map_err(io_err)?;
        let writer = pair.master.take_writer().map_err(io_err)?;

        let (tx, events) = mpsc::channel();
        let shared = Arc::new(Shared {
            emu: Mutex::new(Emu::new(rows, cols, scrollback_lines)),
            writer: Mutex::new(Some(writer)),
            dirty: AtomicBool::new(false),
            tx,
            wake,
            exit: Mutex::new(None),
            exit_cv: Condvar::new(),
        });

        let sh = Arc::clone(&shared);
        let reader_h = std::thread::Builder::new()
            .name("skink-term-read".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut buf = vec![0u8; 32 * 1024];
                let mut replies = Vec::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            replies.clear();
                            lock(&sh.emu).feed(&buf[..n], &mut replies);
                            if !replies.is_empty() {
                                let _ = sh.write(&replies);
                            }
                            sh.notify(TermEvent::Output);
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            })?;

        // Blocks in wait() so exit is noticed immediately with zero polling,
        // and the child is always reaped (no zombies).
        let sh = Arc::clone(&shared);
        let waiter_h = std::thread::Builder::new()
            .name("skink-term-wait".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let st = match child.wait() {
                    Ok(s) => ExitStatus {
                        code: s.exit_code(),
                        signal: s.signal().map(str::to_owned),
                    },
                    Err(_) => ExitStatus {
                        code: 1,
                        signal: None,
                    },
                };
                *lock(&sh.exit) = Some(st);
                sh.exit_cv.notify_all();
                sh.notify(TermEvent::Exited);
            })?;

        Ok(Term {
            shared,
            master: Mutex::new(Some(pair.master)),
            killer: Mutex::new(killer),
            pid,
            events,
            reader: Some(reader_h),
            waiter: Some(waiter_h),
        })
    }

    /// Receiver of coalesced wake-up events. Block on it (or poll
    /// `try_recv`) from the UI loop instead of polling the screen.
    /// After an `Output` event, call [`Term::screen`] / [`Term::snapshot_into`]
    /// to re-arm it (they clear the pending flag).
    pub fn events(&self) -> &Receiver<TermEvent> {
        &self.events
    }

    pub fn process_id(&self) -> Option<u32> {
        self.pid
    }

    /// Current (rows, cols).
    pub fn size(&self) -> (u16, u16) {
        lock(&self.shared.emu).size()
    }

    /// Resize the pty (child gets SIGWINCH / ConPTY resize) and the screen.
    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if self.size() == (rows, cols) {
            return Ok(());
        }
        lock(&self.shared.emu).resize(rows, cols);
        match lock(&self.master).as_ref() {
            Some(m) => m
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(io_err),
            None => Ok(()),
        }
    }

    /// Write raw bytes to the child's stdin (the pty).
    ///
    /// Blocks only if the child is not reading and the kernel pty buffer is
    /// full; send big pastes from a context that tolerates that.
    pub fn write_input(&self, bytes: &[u8]) -> io::Result<()> {
        self.shared.write(bytes)
    }

    /// Encode and send a key (honours application-cursor mode) and snap the
    /// view back to the live screen.
    pub fn send_key(&self, key: impl Into<KeyInput>) -> io::Result<()> {
        let app = {
            let mut emu = lock(&self.shared.emu);
            emu.set_scroll_offset(0);
            emu.app_cursor()
        };
        let bytes = encode_key(key.into(), app);
        if bytes.is_empty() {
            return Ok(());
        }
        self.shared.write(&bytes)
    }

    /// Paste text, wrapped in bracketed-paste markers if the app enabled
    /// them (embedded end markers are stripped); otherwise `\n` becomes `\r`
    /// like xterm.
    pub fn paste(&self, text: &str) -> io::Result<()> {
        let bracketed = {
            let mut emu = lock(&self.shared.emu);
            emu.set_scroll_offset(0);
            emu.bracketed_paste()
        };
        if bracketed {
            let clean = text.replace("\x1b[201~", "").replace("\x1b[200~", "");
            let mut out = Vec::with_capacity(clean.len() + 12);
            out.extend_from_slice(b"\x1b[200~");
            out.extend_from_slice(clean.as_bytes());
            out.extend_from_slice(b"\x1b[201~");
            self.shared.write(&out)
        } else {
            let out = text.replace("\r\n", "\r").replace('\n', "\r");
            self.shared.write(out.as_bytes())
        }
    }

    /// Report a mouse event to the app if it asked for mouse input.
    /// Returns `Ok(true)` if consumed (sent, or deliberately swallowed
    /// because its mode doesn't want it); `Ok(false)` means no mouse mode is
    /// active and the host should handle it (selection, scrollback...).
    /// On the alternate screen without a mouse mode, wheel events become
    /// arrow keys (xterm "alternate scroll"), so `less`/man paging works.
    pub fn send_mouse(&self, ev: MouseEvent) -> io::Result<bool> {
        let (mode, enc, alt, app) = {
            let emu = lock(&self.shared.emu);
            let (m, e) = emu.mouse();
            (m, e, emu.alt_screen(), emu.app_cursor())
        };
        if mode == crate::vt::MouseProtocolMode::None {
            if alt && matches!(ev.kind, MouseKind::ScrollUp | MouseKind::ScrollDown) {
                let key = if ev.kind == MouseKind::ScrollUp {
                    TermKey::Up
                } else {
                    TermKey::Down
                };
                let one = encode_key(KeyInput::new(key), app);
                self.shared.write(&one.repeat(3))?;
                return Ok(true);
            }
            return Ok(false);
        }
        if let Some(bytes) = encode_mouse(ev, mode, enc) {
            self.shared.write(&bytes)?;
        }
        Ok(true)
    }

    /// Tell a focus-reporting app (DECSET 1004) the pane gained/lost focus.
    /// No-op unless the app enabled it.
    pub fn send_focus(&self, focused: bool) -> io::Result<()> {
        if lock(&self.shared.emu).focus_reporting {
            self.shared
                .write(if focused { b"\x1b[I" } else { b"\x1b[O" })?;
        }
        Ok(())
    }

    /// Scroll the view back (toward older output) by `lines`.
    pub fn scroll_up(&self, lines: usize) {
        let mut emu = lock(&self.shared.emu);
        let n = emu.scroll_offset().saturating_add(lines);
        emu.set_scroll_offset(n);
    }

    /// Scroll the view forward (toward live output) by `lines`.
    pub fn scroll_down(&self, lines: usize) {
        let mut emu = lock(&self.shared.emu);
        let n = emu.scroll_offset().saturating_sub(lines);
        emu.set_scroll_offset(n);
    }

    pub fn scroll_to_bottom(&self) {
        lock(&self.shared.emu).set_scroll_offset(0);
    }

    /// Drain OSC 52 clipboard-set requests the child made (at most 8 queued,
    /// 1 MiB each). The host decides whether to honour them (it's a
    /// privileged action: an agent writing your clipboard).
    pub fn take_clipboard_requests(&self) -> Vec<crate::ClipboardRequest> {
        lock(&self.shared.emu).take_clipboard()
    }

    /// Copy the visible screen into a fresh [`Screen`].
    pub fn screen(&self) -> Screen {
        let mut s = empty_screen();
        self.snapshot_into(&mut s);
        s
    }

    /// Like [`Term::screen`] but reuses `out`'s allocations (per-frame use).
    pub fn snapshot_into(&self, out: &mut Screen) {
        // Clear before reading so output arriving mid-copy re-arms a wake-up.
        self.shared.dirty.store(false, Ordering::Release);
        lock(&self.shared.emu).snapshot_into(out);
    }

    /// Block until `pred(&screen)` holds or `timeout` elapses, waking on
    /// output events (no busy polling). Consumes queued [`TermEvent`]s.
    pub fn wait_for(&self, timeout: Duration, mut pred: impl FnMut(&Screen) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        let mut s = empty_screen();
        loop {
            self.snapshot_into(&mut s);
            if pred(&s) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let slice = (deadline - now).min(Duration::from_millis(100));
            let _ = self.events.recv_timeout(slice);
        }
    }

    /// True until the child has exited.
    pub fn is_alive(&self) -> bool {
        lock(&self.shared.exit).is_none()
    }

    /// `Some` once the child has exited.
    pub fn exit_status(&self) -> Option<ExitStatus> {
        lock(&self.shared.exit).clone()
    }

    /// Wait up to `timeout` for the child to exit.
    pub fn wait_exit(&self, timeout: Duration) -> Option<ExitStatus> {
        let g = lock(&self.shared.exit);
        let (g, _) = self
            .shared
            .exit_cv
            .wait_timeout_while(g, timeout, |s| s.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        g.clone()
    }

    /// Ask the child to terminate (SIGHUP on Unix, TerminateProcess on
    /// Windows). Escalation to SIGKILL happens in `Drop` if it lingers.
    pub fn kill(&self) -> io::Result<()> {
        if !self.is_alive() {
            return Ok(());
        }
        lock(&self.killer).kill()
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        if self.is_alive() {
            let _ = lock(&self.killer).kill();
        }
        // Close our pty ends: hangs up the session and unblocks the reader.
        lock(&self.shared.writer).take();
        lock(&self.master).take();

        #[cfg(unix)]
        if self.wait_exit(Duration::from_millis(250)).is_none() {
            // Ignored SIGHUP: hard kill without `unsafe` (std only).
            if let Some(pid) = self.pid {
                let _ = std::process::Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
        let exited = self.wait_exit(Duration::from_millis(500)).is_some();
        // Join only threads known to finish; never hang the UI on a stuck one.
        if exited && let Some(h) = self.waiter.take() {
            let _ = h.join();
        }
        if let Some(h) = self.reader.take() {
            let deadline = Instant::now() + Duration::from_millis(300);
            while !h.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            if h.is_finished() {
                let _ = h.join();
            }
            // else: detached; it exits on its own at pty EOF.
        }
    }
}
