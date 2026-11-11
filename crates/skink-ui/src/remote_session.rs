//! The editor's side of a `skink ssh` session (`skink --remote TOKEN`):
//! watch which ports the programs in terminal tabs listen on, tell the
//! local side (escape sequences, see remote.rs), and show what it answers
//! (forwards, offers, failures) as toasts and in `:ports`.
//!
//! Ports are checked only after terminal output or a child exit (a server
//! prints when it starts; Ctrl-C prints too): once ~0.4 s later, then at
//! most every 1.5 s while output continues, and once more after it stops.
//! An idle editor checks nothing.

use crate::App;
use crate::nav::Level;
use crate::remote::{Down, Up};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const FIRST_CHECK: Duration = Duration::from_millis(400);
const COOL_DOWN: Duration = Duration::from_millis(1500);

/// Is this a `skink ssh` session?
pub(crate) enum RemoteState {
    Local,
    /// `--remote TOKEN`; the notify socket starts with the event loop.
    Pending {
        token: String,
    },
    Live(Box<Live>),
    /// The notify socket could not start: ports are not forwarded.
    Failed(String),
}

/// When the next port check runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortCheck {
    Idle,
    Due(Instant),
    /// Just checked; `dirty`: output since, so check again at `until`.
    Cooling {
        until: Instant,
        dirty: bool,
    },
}

impl PortCheck {
    pub(crate) fn on_output(self, now: Instant) -> Self {
        match self {
            PortCheck::Idle => PortCheck::Due(now + FIRST_CHECK),
            PortCheck::Due(t) => PortCheck::Due(t),
            PortCheck::Cooling { until, .. } => PortCheck::Cooling { until, dirty: true },
        }
    }

    pub(crate) fn deadline(self) -> Option<Instant> {
        match self {
            PortCheck::Idle => None,
            PortCheck::Due(t) | PortCheck::Cooling { until: t, .. } => Some(t),
        }
    }

    /// `(check now?, next state)`.
    pub(crate) fn tick(self, now: Instant) -> (bool, Self) {
        match self {
            PortCheck::Due(t) if now >= t => (
                true,
                PortCheck::Cooling {
                    until: now + COOL_DOWN,
                    dirty: false,
                },
            ),
            PortCheck::Cooling { until, dirty } if now >= until => match dirty {
                true => (
                    true,
                    PortCheck::Cooling {
                        until: now + COOL_DOWN,
                        dirty: false,
                    },
                ),
                // One last look after output stopped, then sleep.
                false => (true, PortCheck::Idle),
            },
            s => (false, s),
        }
    }
}

/// A forward as the editor knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fwd {
    Requested,
    Active {
        local: u16,
    },
    Failed(String),
    /// `"ssh.autoforward": "ask"` offered it.
    Offered,
}

pub(crate) struct Live {
    pub token: String,
    socket: PathBuf,
    rx: mpsc::Receiver<Down>,
    /// Listening ports seen at the last check.
    pub ports: BTreeSet<u16>,
    pub forwards: BTreeMap<u16, Fwd>,
    pub check: PortCheck,
    /// Where the escape sequences go (the terminal; tests: a buffer).
    emit: Box<dyn FnMut(&str) + Send>,
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// One `--remote-notify` message on the socket.
#[derive(serde::Serialize, serde::Deserialize)]
struct Note {
    token: String,
    args: Vec<String>,
}

impl Live {
    fn start(
        token: String,
        dir: &Path,
        wake: crate::WakeFn,
        emit: Box<dyn FnMut(&str) + Send>,
    ) -> std::io::Result<Self> {
        let socket = dir.join(format!("skink-remote-{}.sock", std::process::id()));
        let (tx, rx) = mpsc::channel();
        listen(&socket, token.clone(), tx, wake)?;
        let mut live = Live {
            token,
            socket,
            rx,
            ports: BTreeSet::new(),
            forwards: BTreeMap::new(),
            check: PortCheck::Idle,
            emit,
        };
        let hello = Up::Hello {
            version: crate::remote::VERSION,
            socket: live.socket.display().to_string(),
        };
        live.send(&hello);
        Ok(live)
    }

    fn send(&mut self, m: &Up) {
        let seq = m.encode(&self.token);
        (self.emit)(&seq);
    }
}

#[cfg(unix)]
fn listen(
    path: &Path,
    token: String,
    tx: mpsc::Sender<Down>,
    wake: crate::WakeFn,
) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    let l = std::os::unix::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut line = String::new();
            if BufReader::new(&s).read_line(&mut line).is_err() {
                continue;
            }
            let Ok(n) = serde_json::from_str::<Note>(&line) else {
                continue;
            };
            if n.token != token {
                continue;
            }
            if let Some(d) = Down::from_args(&n.args) {
                if tx.send(d).is_err() {
                    return;
                }
                wake();
            }
        }
    });
    Ok(())
}

#[cfg(not(unix))]
fn listen(_: &Path, _: String, _: mpsc::Sender<Down>, _: crate::WakeFn) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "skink ssh needs Linux or macOS on the remote",
    ))
}

/// `skink --remote-notify SOCKET TOKEN WORDS...` (run by the local side
/// over ssh): hand the words to the editor. Exit code 0 when delivered.
pub fn notify_main(args: &[String]) -> i32 {
    let [socket, token, words @ ..] = args else {
        eprintln!("usage: skink --remote-notify SOCKET TOKEN MESSAGE...");
        return 2;
    };
    let line = serde_json::to_string(&Note {
        token: token.clone(),
        args: words.to_vec(),
    })
    .unwrap_or_default();
    match deliver(socket, &line) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("skink: {socket}: {e}");
            1
        }
    }
}

#[cfg(unix)]
fn deliver(socket: &str, line: &str) -> std::io::Result<()> {
    let mut s = std::os::unix::net::UnixStream::connect(socket)?;
    s.write_all(line.as_bytes())?;
    s.write_all(b"\n")
}

#[cfg(not(unix))]
fn deliver(_: &str, _: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "only on Linux and macOS",
    ))
}

/// Raw bytes to the user's terminal (through ssh to the local side).
fn to_terminal(seq: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

impl App {
    /// `--remote TOKEN`: this editor runs under `skink ssh`.
    pub fn set_remote(&mut self, token: String) {
        self.remote = RemoteState::Pending { token };
    }

    /// Start the notify socket once there is an event loop to wake.
    pub(crate) fn start_remote(&mut self) {
        let (RemoteState::Pending { .. }, Some(wake)) = (&self.remote, &self.wake) else {
            return;
        };
        let wake = wake.clone();
        self.start_remote_with(wake, Box::new(to_terminal));
    }

    pub(crate) fn start_remote_with(
        &mut self,
        wake: crate::WakeFn,
        emit: Box<dyn FnMut(&str) + Send>,
    ) {
        let RemoteState::Pending { token } =
            std::mem::replace(&mut self.remote, RemoteState::Local)
        else {
            return;
        };
        let preferred = self
            .state_dir
            .clone()
            .unwrap_or_else(std::env::temp_dir)
            .join("remote");
        let dir = crate::remote::socket_dir(&preferred, "skink-remote", 30);
        self.remote = match Live::start(token, &dir, wake, emit) {
            Ok(l) => RemoteState::Live(Box::new(l)),
            Err(e) => RemoteState::Failed(e.to_string()),
        };
    }

    /// Terminal output arrived (or a child exited).
    pub(crate) fn remote_output(&mut self) {
        if let RemoteState::Live(l) = &mut self.remote {
            l.check = l.check.on_output(Instant::now());
        }
    }

    pub(crate) fn remote_timeout(&self) -> Option<Duration> {
        let RemoteState::Live(l) = &self.remote else {
            return None;
        };
        let t = l.check.deadline()?;
        Some(t.saturating_duration_since(Instant::now()))
    }

    /// Run a due port check; read what the local side answered.
    pub(crate) fn remote_tick(&mut self) {
        let RemoteState::Live(l) = &mut self.remote else {
            return;
        };
        let (check, next) = l.check.tick(Instant::now());
        l.check = next;
        if check {
            let roots = self.terminal_pids();
            let now = crate::ports::listening(&roots);
            self.ports_changed(now);
        }
        self.poll_remote();
    }

    fn terminal_pids(&self) -> Vec<u32> {
        self.tabs
            .iter()
            .filter_map(|t| match t {
                crate::Tab::Term(p) => p.term.process_id(),
                crate::Tab::Doc(_) => None,
            })
            .collect()
    }

    /// Tell the local side about ports that opened or closed.
    pub(crate) fn ports_changed(&mut self, now: BTreeSet<u16>) {
        let RemoteState::Live(l) = &mut self.remote else {
            return;
        };
        let opened: Vec<u16> = now.difference(&l.ports).copied().collect();
        let closed: Vec<u16> = l.ports.difference(&now).copied().collect();
        for p in opened {
            l.send(&Up::Opened(p));
        }
        for p in closed {
            l.send(&Up::Closed(p));
            if matches!(l.forwards.get(&p), Some(Fwd::Offered)) {
                l.forwards.remove(&p);
            }
        }
        l.ports = now;
    }

    fn poll_remote(&mut self) {
        let RemoteState::Live(l) = &mut self.remote else {
            return;
        };
        let got: Vec<Down> = l.rx.try_iter().collect();
        for d in got {
            self.on_down(d);
        }
    }

    pub(crate) fn on_down(&mut self, d: Down) {
        let RemoteState::Live(l) = &mut self.remote else {
            return;
        };
        let (level, text) = match d {
            Down::Forwarded { remote, local } => {
                let fresh = l.forwards.get(&remote) != Some(&Fwd::Active { local });
                l.forwards.insert(remote, Fwd::Active { local });
                if !fresh {
                    return;
                }
                let text = match remote == local {
                    true => {
                        format!("port {remote} forwarded: http://localhost:{local} on your machine")
                    }
                    false => format!(
                        "port {remote} forwarded: http://localhost:{local} on your machine ({remote} was taken there)"
                    ),
                };
                (Level::Info, text)
            }
            Down::Failed { remote, reason } => {
                l.forwards.insert(remote, Fwd::Failed(reason.clone()));
                (
                    Level::Warn,
                    format!("port {remote} not forwarded: {reason}"),
                )
            }
            Down::Stopped(p) => {
                l.forwards.remove(&p);
                (Level::Info, format!("port {p} no longer forwarded"))
            }
            Down::Offer(p) => {
                l.forwards.insert(p, Fwd::Offered);
                (
                    Level::Info,
                    format!("port {p} opened: :forward {p} to reach it from your machine"),
                )
            }
        };
        self.toast(level, text);
    }

    /// `:forward R[:L]`.
    pub(crate) fn forward_port(&mut self, arg: Option<&str>) {
        let Some(spec) = arg.and_then(skink_config::ForwardSpec::parse) else {
            self.doc.editor.message =
                Some("usage: :forward PORT[:LOCAL]  (e.g. :forward 3000)".into());
            return;
        };
        let local = match arg.is_some_and(|a| a.contains(':')) {
            true => spec.local,
            false => 0,
        };
        let RemoteState::Live(l) = &mut self.remote else {
            return self.not_remote();
        };
        l.forwards.insert(spec.remote, Fwd::Requested);
        l.send(&Up::Forward {
            remote: spec.remote,
            local,
        });
        self.doc.editor.message = Some(format!("forwarding port {}...", spec.remote));
    }

    /// `:ports stop R`.
    pub(crate) fn unforward_port(&mut self, arg: Option<&str>) {
        let Some(p) = arg.and_then(|a| a.parse::<u16>().ok()) else {
            self.doc.editor.message = Some("usage: :ports stop PORT".into());
            return;
        };
        let RemoteState::Live(l) = &mut self.remote else {
            return self.not_remote();
        };
        l.send(&Up::Unforward(p));
    }

    fn not_remote(&mut self) {
        let why = match &self.remote {
            RemoteState::Failed(e) => format!("port forwarding is off: {e}"),
            _ => "ports are forwarded only in a `skink ssh` session".into(),
        };
        self.doc.editor.message = Some(why);
    }

    /// `:ports`: listening ports and forwards.
    pub(crate) fn show_ports(&mut self) {
        let RemoteState::Live(l) = &self.remote else {
            return self.not_remote();
        };
        let mut all: BTreeSet<u16> = l.ports.clone();
        all.extend(l.forwards.keys());
        let mut text =
            String::from("Ports on this machine, as seen from your local terminal (skink ssh)\n\n");
        if all.is_empty() {
            text.push_str("  Nothing is listening in the terminal tabs yet.\n");
        }
        for p in all {
            let listening = if l.ports.contains(&p) {
                "listening"
            } else {
                "         "
            };
            let state = match l.forwards.get(&p) {
                Some(Fwd::Active { local }) => {
                    format!("-> http://localhost:{local} on your machine")
                }
                Some(Fwd::Requested) => "forwarding...".into(),
                Some(Fwd::Failed(why)) => format!("not forwarded: {why}"),
                Some(Fwd::Offered) | None => format!("not forwarded (:forward {p})"),
            };
            text.push_str(&format!("  {p:>5}  {listening}  {state}\n"));
        }
        text.push_str(
            "\n:forward PORT[:LOCAL] adds one, :ports stop PORT removes one.\nForwards bind to 127.0.0.1 on your machine only.\n",
        );
        self.open_view("Ports", &text);
    }

    /// `:remotes`: the saved `skink ssh` targets.
    pub(crate) fn show_remotes(&mut self) {
        let mut text = String::from("Saved remotes (\"remotes\" in settings.json)\n\n");
        if self.settings.remotes.is_empty() {
            text.push_str(
                "  None yet. Add one to settings.json:\n    \"remotes\": {\"pi\": {\"host\": \"pi@10.0.0.5\", \"path\": \"~/app\", \"forwards\": [\"3000\"]}}\n",
            );
        }
        for (name, r) in &self.settings.remotes {
            let path = if r.path.is_empty() { "~" } else { &r.path };
            let fwd: Vec<String> = r
                .forwards
                .iter()
                .map(|f| match f.remote == f.local {
                    true => f.remote.to_string(),
                    false => format!("{}:{}", f.remote, f.local),
                })
                .collect();
            let auto = r
                .autoforward
                .unwrap_or(self.settings.ssh_autoforward)
                .name();
            text.push_str(&format!(
                "  {name:<12} {}:{path}  forwards [{}]  autoforward {auto}\n",
                r.host,
                fwd.join(", ")
            ));
        }
        text.push_str(&format!(
            "\nOpen one with: skink ssh NAME   (any host works too: skink ssh user@host:~/path)\n\"ssh.autoforward\": {}\n",
            self.settings.ssh_autoforward.name()
        ));
        self.open_view("Remotes", &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::sync::{Arc, Mutex};

    #[test]
    fn checks_run_after_output_and_stop_when_idle() {
        let t0 = Instant::now();
        let c = PortCheck::Idle;
        assert_eq!(c.deadline(), None);
        let c = c.on_output(t0);
        let (run, c) = c.tick(t0);
        assert!(!run);
        let (run, c) = c.tick(t0 + FIRST_CHECK);
        assert!(run);
        // Output during the cool-down: one more check at its end.
        let c = c.on_output(t0 + FIRST_CHECK);
        let (run, c) = c.tick(t0 + FIRST_CHECK + COOL_DOWN);
        assert!(run);
        assert!(matches!(c, PortCheck::Cooling { dirty: false, .. }));
        // Quiet: the last look, then nothing until the next output.
        let (run, c) = c.tick(t0 + FIRST_CHECK + 3 * COOL_DOWN);
        assert!(run);
        assert_eq!(c, PortCheck::Idle);
    }

    #[cfg(unix)]
    #[test]
    fn ports_go_up_and_answers_come_back_through_the_socket() {
        let dir = std::env::temp_dir().join(format!("skink-remote-t-{}", std::process::id()));
        let mut app = App::new(
            skink_core::Editor::new(skink_core::Buffer::from_text("")),
            skink_config::Keymap::defaults(),
            skink_core::Backend::Local,
        );
        app.state_dir = Some(dir.clone());
        let sent = Arc::new(Mutex::new(String::new()));
        let s2 = sent.clone();
        app.set_remote("tok".into());
        let (wtx, wrx) = mpsc::channel::<()>();
        let wtx = Mutex::new(wtx);
        app.start_remote_with(
            Arc::new(move || {
                let _ = wtx.lock().unwrap().send(());
            }),
            Box::new(move |s| s2.lock().unwrap().push_str(s)),
        );
        let RemoteState::Live(l) = &app.remote else {
            panic!("not live");
        };
        let socket = l.socket.clone();
        // hello, then open/close through the local filter.
        app.ports_changed([3000, 5432].into());
        app.ports_changed([3000].into());
        let mut f = crate::remote::Filter::new("tok");
        let mut out = Vec::new();
        let ups = f.feed(sent.lock().unwrap().as_bytes(), &mut out);
        assert!(out.is_empty());
        assert_eq!(
            ups[1..],
            [Up::Opened(3000), Up::Opened(5432), Up::Closed(5432)]
        );
        assert!(
            matches!(&ups[0], Up::Hello { socket: s, .. } if *s == socket.display().to_string())
        );
        // The local side answers with --remote-notify.
        let bad = notify_main(&[
            socket.display().to_string(),
            "wrong".into(),
            "forwarded".into(),
            "3000".into(),
            "3000".into(),
        ]);
        assert_eq!(bad, 0, "delivered, but ignored");
        let code = notify_main(&[
            socket.display().to_string(),
            "tok".into(),
            "forwarded".into(),
            "3000".into(),
            "3001".into(),
        ]);
        assert_eq!(code, 0);
        wrx.recv_timeout(Duration::from_secs(10)).expect("woken");
        app.remote_tick();
        let RemoteState::Live(l) = &app.remote else {
            panic!()
        };
        assert_eq!(l.forwards.get(&3000), Some(&Fwd::Active { local: 3001 }));
        assert!(app.toasts.iter().any(|t| t.text.contains("localhost:3001")));
        drop(app);
        assert!(!socket.exists(), "socket removed on exit");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
