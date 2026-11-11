//! `skink ssh [user@]host[:port][:path]`: open a project on another
//! machine. The local process only connects, installs, forwards ports and
//! passes the terminal through; the editor runs on the remote, next to the
//! code (language servers, git, tasks, terminals).
//!
//! One OpenSSH ControlMaster connection carries everything (your keys,
//! agent, `~/.ssh/config` and jump hosts work as usual): the probe, the
//! binary upload, the session itself, `-O forward` for ports, and the
//! one-shot `--remote-notify` commands back to the editor. OpenSSH for
//! Windows has no ControlMaster: each command opens its own connection and
//! each forward is its own `ssh -N -L` process.

use crate::remote::{Down, Filter, Up};
use crate::update;
use skink_config::{AutoForward, ForwardSpec};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Where the binary lives on the remote (a shell expression).
pub const REMOTE_BIN: &str = "$HOME/.skink/bin/skink";

/// `[user@]host[:port][:path]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// What ssh connects to: `user@host`, `host`, or a `~/.ssh/config` alias.
    pub dest: String,
    pub port: Option<u16>,
    /// The folder to open (empty: the remote home).
    pub path: String,
}

/// Parse a target; `[::1]` style hosts keep their brackets for ssh.
pub fn parse_target(arg: &str) -> Result<Target, String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Err("skink ssh needs a host, e.g. skink ssh pi@10.0.0.5:~/app".into());
    }
    let (user, rest) = match arg.split_once('@') {
        Some((u, r)) if !u.is_empty() && !u.contains(':') => (Some(u), r),
        _ => (None, arg),
    };
    let (host, after) = if let Some(r) = rest.strip_prefix('[') {
        let (h, a) = r
            .split_once(']')
            .ok_or_else(|| format!("{arg}: missing ] after the IPv6 address"))?;
        (h.to_owned(), a)
    } else {
        match rest.find(':') {
            Some(i) => (rest[..i].to_owned(), &rest[i..]),
            None => (rest.to_owned(), ""),
        }
    };
    if host.is_empty() {
        return Err(format!("{arg}: no host"));
    }
    let after = after.strip_prefix(':').unwrap_or(after);
    // A first segment of digits is the port: host:2222:~/app, host:22.
    let (port, path) = match after.split_once(':') {
        Some((p, rest)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
            (Some(p), rest)
        }
        None if !after.is_empty() && after.bytes().all(|b| b.is_ascii_digit()) => (Some(after), ""),
        _ => (None, after),
    };
    let port = match port {
        Some(p) => Some(
            p.parse::<u16>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| format!("{arg}: bad port {p}"))?,
        ),
        None => None,
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let dest = match user {
        Some(u) => format!("{u}@{host}"),
        None => host,
    };
    Ok(Target {
        dest,
        port,
        path: path.to_owned(),
    })
}

/// How commands reach the remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mux {
    /// One ControlMaster connection (Linux, macOS): `-S socket`.
    Control(PathBuf),
    /// A new connection per command (OpenSSH for Windows).
    PerCommand,
}

/// Where the connection is; [`describe`](Conn::describe) is what the user
/// reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conn {
    Connecting,
    Installing(Step),
    Running,
    Lost(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Probing,
    Downloading { target: String },
    Uploading { target: String },
}

impl Conn {
    pub fn describe(&self, host: &str) -> String {
        match self {
            Conn::Connecting => format!("connecting to {host}..."),
            Conn::Installing(Step::Probing) => format!("checking skink on {host}..."),
            Conn::Installing(Step::Downloading { target }) => {
                format!(
                    "downloading skink {} for {target}...",
                    update::Version::current()
                )
            }
            Conn::Installing(Step::Uploading { target }) => {
                format!(
                    "installing skink {} on {host} ({target})...",
                    update::Version::current()
                )
            }
            Conn::Running => format!("connected to {host}"),
            Conn::Lost(why) => format!("{host}: {why}"),
        }
    }
}

/// The system `ssh` for one target.
#[derive(Debug, Clone)]
pub struct Ssh {
    /// `ssh` (or `SKINK_SSH`, tests: a fake).
    pub program: String,
    pub dest: String,
    pub port: Option<u16>,
    pub mux: Mux,
    /// Extra options before the destination (tests: `-o BatchMode=yes`).
    pub options: Vec<String>,
}

/// Quote for a POSIX shell (and fish): `'...'` with `'` spelled `'"'"'`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// A remote path for the shell: a leading `~/` becomes `"$HOME"/`.
fn remote_path(p: &str) -> String {
    match p {
        "" | "~" => "\"$HOME\"".into(),
        p if p.starts_with("~/") => format!("\"$HOME\"/{}", sh_quote(&p[2..])),
        p => sh_quote(p),
    }
}

impl Ssh {
    pub fn new(target: &Target, state_dir: &Path) -> Self {
        let program = std::env::var("SKINK_SSH")
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "ssh".into());
        let mux = if cfg!(windows) {
            Mux::PerCommand
        } else {
            Mux::Control(control_socket(state_dir, target))
        };
        Self {
            program,
            dest: target.dest.clone(),
            port: target.port,
            mux,
            options: Vec::new(),
        }
    }

    fn base(&self) -> Command {
        let mut c = Command::new(&self.program);
        if let Some(p) = self.port {
            c.args(["-p", &p.to_string()]);
        }
        if let Mux::Control(sock) = &self.mux {
            c.arg("-S").arg(sock);
        }
        c.args(&self.options);
        c
    }

    /// Open (or reuse) the connection. Prompts (host key, password) go to
    /// the user's terminal.
    pub fn connect(&self) -> Result<(), String> {
        match &self.mux {
            Mux::Control(_) => {
                if self.control_alive() {
                    return Ok(());
                }
                let status = self
                    .base()
                    .args(["-M", "-o", "ControlPersist=60", "-f", "-N"])
                    .arg(&self.dest)
                    .status()
                    .map_err(|e| format!("could not run {}: {e}", self.program))?;
                connect_result(status)
            }
            Mux::PerCommand => {
                let status = self
                    .base()
                    .arg(&self.dest)
                    .arg("exit 0")
                    .status()
                    .map_err(|e| format!("could not run {}: {e}", self.program))?;
                connect_result(status)
            }
        }
    }

    fn control_alive(&self) -> bool {
        self.base()
            .args(["-O", "check"])
            .arg(&self.dest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Run `script` with `sh` on the remote; stdin from `input` if given.
    pub fn run(&self, script: &str, input: Option<&Path>) -> Result<Output, String> {
        let mut c = self.base();
        if matches!(self.mux, Mux::Control(_)) {
            // The master holds the authentication: never prompt here.
            c.args(["-o", "BatchMode=yes"]);
        }
        c.arg(&self.dest).arg(format!("sh -c {}", sh_quote(script)));
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        match input {
            Some(p) => {
                let f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
                c.stdin(f);
            }
            None => {
                c.stdin(Stdio::null());
            }
        }
        c.output()
            .map_err(|e| format!("could not run {}: {e}", self.program))
    }

    /// The interactive session: the editor on the remote in a terminal.
    pub fn session(&self, script: &str) -> std::io::Result<Child> {
        let mut c = self.base();
        c.arg("-t")
            .arg(&self.dest)
            .arg(format!("sh -c {}", sh_quote(script)));
        c.stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        c.spawn()
    }

    /// Start forwarding `127.0.0.1:local` here to `localhost:remote` there.
    pub fn forward(&self, local: u16, remote: u16) -> Result<Tunnel, String> {
        let spec = format!("127.0.0.1:{local}:localhost:{remote}");
        match &self.mux {
            Mux::Control(_) => {
                let out = self
                    .base()
                    .args(["-O", "forward", "-L", &spec])
                    .arg(&self.dest)
                    .stdin(Stdio::null())
                    .output()
                    .map_err(|e| e.to_string())?;
                match out.status.success() {
                    true => Ok(Tunnel::Mux),
                    false => Err(first_line(&out.stderr, "ssh refused the forward")),
                }
            }
            Mux::PerCommand => {
                let child = self
                    .base()
                    .args(["-N", "-o", "ExitOnForwardFailure=yes", "-L", &spec])
                    .arg(&self.dest)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| e.to_string())?;
                Ok(Tunnel::Child(child))
            }
        }
    }

    pub fn cancel(&self, local: u16, remote: u16, tunnel: Tunnel) {
        match tunnel {
            Tunnel::Mux => {
                let _ = self
                    .base()
                    .args(["-O", "cancel", "-L"])
                    .arg(format!("127.0.0.1:{local}:localhost:{remote}"))
                    .arg(&self.dest)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            Tunnel::Child(mut c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
}

/// A live forward: on the shared connection, or its own `ssh -N`.
#[derive(Debug)]
pub enum Tunnel {
    Mux,
    Child(Child),
}

fn connect_result(status: ExitStatus) -> Result<(), String> {
    match status.code() {
        Some(0) => Ok(()),
        // ssh prints its own reason (host key, auth, DNS) above.
        Some(255) => Err("ssh could not connect (its message is above)".into()),
        Some(c) => Err(format!("ssh exited with {c}")),
        None => Err("ssh was stopped".into()),
    }
}

fn first_line(bytes: &[u8], fallback: &str) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

/// A short socket path (Unix sockets allow ~104 bytes).
fn control_socket(state_dir: &Path, t: &Target) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (&t.dest, t.port).hash(&mut h);
    let name = format!("{:012x}.sock", h.finish() & 0xffff_ffff_ffff);
    crate::remote::socket_dir(&state_dir.join("ssh"), "skink-ssh", name.len()).join(name)
}

/// The release target for `uname -sm` output.
pub fn remote_target(uname: &str) -> Result<&'static str, String> {
    let mut it = uname.split_whitespace();
    let (os, arch) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
    Ok(match (os, arch) {
        ("Linux", "x86_64" | "amd64") => "x86_64-unknown-linux-musl",
        ("Linux", "aarch64" | "arm64") => "aarch64-unknown-linux-musl",
        ("Darwin", "x86_64") => "x86_64-apple-darwin",
        ("Darwin", "arm64" | "aarch64") => "aarch64-apple-darwin",
        _ => {
            return Err(format!(
                "no skink build for {} (Linux and macOS on x86_64 and arm64 are supported)",
                uname.trim()
            ));
        }
    })
}

/// What the probe found: the remote's target and its skink version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub target: &'static str,
    pub installed: Option<update::Version>,
}

impl Probe {
    /// Install when missing or older than this skink.
    pub fn needs_install(&self) -> bool {
        match &self.installed {
            Some(v) => *v < update::Version::current(),
            None => true,
        }
    }
}

pub fn probe(ssh: &Ssh) -> Result<Probe, String> {
    let out = ssh.run(
        &format!("uname -sm; \"{REMOTE_BIN}\" --version 2>/dev/null; exit 0"),
        None,
    )?;
    if !out.status.success() {
        return Err(first_line(&out.stderr, "the remote shell failed"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let target = remote_target(lines.next().unwrap_or(""))?;
    let installed = lines
        .find_map(|l| l.trim().strip_prefix("skink "))
        .and_then(update::Version::parse);
    Ok(Probe { target, installed })
}

/// The binary to copy: this one when the remote is the same platform,
/// otherwise this version's release for it (downloaded here, cached).
pub fn binary_for(target: &str, cache: &Path) -> Result<PathBuf, String> {
    if update::target() == Some(target)
        && let Ok(me) = std::env::current_exe()
    {
        return Ok(me);
    }
    update::binary_for(target, cache).map_err(|e| e.to_string())
}

/// Copy `bin` to [`REMOTE_BIN`] over the connection (atomic rename).
pub fn upload(ssh: &Ssh, bin: &Path) -> Result<(), String> {
    let script = "set -e; d=\"$HOME/.skink/bin\"; mkdir -p \"$d\"; cat > \"$d/.skink.new\"; chmod 755 \"$d/.skink.new\"; mv -f \"$d/.skink.new\" \"$d/skink\"";
    let out = ssh.run(script, Some(bin))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("No space left") || err.contains("Disk quota") {
        return Err("no space left on the remote for skink (about 20 MB)".into());
    }
    Err(first_line(
        &out.stderr,
        "copying skink to the remote failed",
    ))
}

/// The remote side of the session: cd, then the editor.
pub fn session_script(token: &str, path: &str, env: &[(String, String)]) -> String {
    let vars: String = env
        .iter()
        .map(|(k, v)| format!("{k}={} ", sh_quote(v)))
        .collect();
    let dir = remote_path(path);
    format!(
        "cd {dir} 2>/dev/null || cd \"$HOME\"; exec env {vars}\"{REMOTE_BIN}\" --remote {token} ."
    )
}

/// Terminal facts the remote should know (truecolor, kitty keys).
pub fn passed_env() -> Vec<(String, String)> {
    ["COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"]
        .iter()
        .filter_map(|k| Some((k.to_string(), std::env::var(k).ok()?)))
        .collect()
}

// ---- port forwarding (local side) -------------------------------------------

/// Why a port is forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Listed in the remote's `"forwards"`: kept for the whole session.
    Configured,
    /// A program started listening (`"ssh.autoforward": true`).
    Auto,
    /// `:port`.
    Manual,
}

/// The first free loopback port from `want` (the OS picks when none of the
/// next 50 is free).
pub fn free_local_port(want: u16) -> u16 {
    let ok = |p: u16| std::net::TcpListener::bind(("127.0.0.1", p)).is_ok();
    let start = want.max(1);
    (start..start.saturating_add(50))
        .find(|&p| ok(p))
        .or_else(|| {
            std::net::TcpListener::bind(("127.0.0.1", 0))
                .and_then(|l| l.local_addr())
                .map(|a| a.port())
                .ok()
        })
        .unwrap_or(want)
}

/// Work for the forwarding thread, so the terminal never waits on ssh.
#[derive(Debug)]
pub enum Job {
    Hello {
        socket: String,
    },
    Opened(u16),
    Closed(u16),
    Forward {
        remote: u16,
        local: u16,
        origin: Origin,
    },
    Unforward(u16),
    Stop,
}

/// The forwarding thread's state.
pub struct Forwarder {
    pub ssh: Ssh,
    pub token: String,
    pub policy: AutoForward,
    /// The remote's notify socket, once it said hello.
    notify: Notify,
    active: BTreeMap<u16, (u16, Origin, Tunnel)>,
    /// Messages for the editor that came before its hello.
    queued: Vec<Down>,
}

/// Where `--remote-notify` messages go.
enum Notify {
    WaitingForHello,
    Socket(String),
}

impl Forwarder {
    pub fn new(ssh: Ssh, token: String, policy: AutoForward) -> Self {
        Self {
            ssh,
            token,
            policy,
            notify: Notify::WaitingForHello,
            active: BTreeMap::new(),
            queued: Vec::new(),
        }
    }

    /// Ports forwarded now: remote -> local.
    pub fn active(&self) -> Vec<(u16, u16)> {
        self.active.iter().map(|(r, (l, ..))| (*r, *l)).collect()
    }

    pub fn handle(&mut self, job: Job) {
        match job {
            Job::Hello { socket } => {
                self.notify = Notify::Socket(socket);
                // Forwards are resent from the table below (the editor may
                // also have restarted); failures and offers from the queue.
                for d in std::mem::take(&mut self.queued) {
                    if !matches!(d, Down::Forwarded { .. } | Down::Stopped(_)) {
                        self.tell(d);
                    }
                }
                for (r, l) in self.active() {
                    self.tell(Down::Forwarded {
                        remote: r,
                        local: l,
                    });
                }
            }
            Job::Opened(p) => match self.policy {
                AutoForward::On => self.handle(Job::Forward {
                    remote: p,
                    local: p,
                    origin: Origin::Auto,
                }),
                AutoForward::Ask if !self.active.contains_key(&p) => self.tell(Down::Offer(p)),
                AutoForward::Ask | AutoForward::Off => {}
            },
            Job::Closed(p) => {
                if matches!(self.active.get(&p), Some((_, Origin::Auto, _))) {
                    self.stop(p);
                }
            }
            Job::Forward {
                remote,
                local,
                origin,
            } => {
                if let Some((l, ..)) = self.active.get(&remote) {
                    let l = *l;
                    return self.tell(Down::Forwarded { remote, local: l });
                }
                let local = free_local_port(if local == 0 { remote } else { local });
                match self.ssh.forward(local, remote) {
                    Ok(t) => {
                        self.active.insert(remote, (local, origin, t));
                        self.tell(Down::Forwarded { remote, local });
                    }
                    Err(reason) => self.tell(Down::Failed { remote, reason }),
                }
            }
            Job::Unforward(p) => self.stop(p),
            Job::Stop => {
                for r in self.active.keys().copied().collect::<Vec<_>>() {
                    if let Some((l, _, t)) = self.active.remove(&r) {
                        self.ssh.cancel(l, r, t);
                    }
                }
            }
        }
    }

    fn stop(&mut self, remote: u16) {
        if let Some((l, _, t)) = self.active.remove(&remote) {
            self.ssh.cancel(l, remote, t);
        }
        self.tell(Down::Stopped(remote));
    }

    /// Tell the editor (a one-shot command over the same connection).
    fn tell(&mut self, d: Down) {
        let Notify::Socket(sock) = &self.notify else {
            return self.queued.push(d);
        };
        let words: Vec<String> = d.to_args().iter().map(|a| sh_quote(a)).collect();
        let script = format!(
            "exec \"{REMOTE_BIN}\" --remote-notify {} {} {}",
            sh_quote(sock),
            sh_quote(&self.token),
            words.join(" ")
        );
        let _ = self.ssh.run(&script, None);
    }
}

/// Copy the remote's output to `out`, minus Skink's sequences, until it
/// ends; messages go to `jobs`. A held partial sequence is released after
/// 20 ms without more bytes.
pub fn pump(
    mut from: impl Read + Send + 'static,
    out: &mut impl Write,
    token: &str,
    jobs: &mpsc::Sender<Job>,
) {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match from.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let mut filter = Filter::new(token);
    let mut bytes = Vec::with_capacity(16 * 1024);
    loop {
        let chunk = if filter.holding() {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            }
        };
        match chunk {
            Some(c) => {
                for m in filter.feed(&c, &mut bytes) {
                    let _ = jobs.send(job_for(m));
                }
            }
            None => filter.flush(&mut bytes),
        }
        if !bytes.is_empty() {
            let _ = out.write_all(&bytes);
            let _ = out.flush();
            bytes.clear();
        }
    }
    filter.flush(&mut bytes);
    let _ = out.write_all(&bytes);
    let _ = out.flush();
}

fn job_for(m: Up) -> Job {
    match m {
        Up::Hello { socket, .. } => Job::Hello { socket },
        Up::Opened(p) => Job::Opened(p),
        Up::Closed(p) => Job::Closed(p),
        Up::Forward { remote, local } => Job::Forward {
            remote,
            local,
            origin: Origin::Manual,
        },
        Up::Unforward(p) => Job::Unforward(p),
    }
}

/// What `skink ssh` was asked for, settings applied.
#[derive(Debug, Clone)]
pub struct Plan {
    pub target: Target,
    pub forwards: Vec<ForwardSpec>,
    pub policy: AutoForward,
    /// A saved remote's name, for messages.
    pub name: Option<String>,
}

/// `skink ssh NAME` (a saved remote) or `skink ssh TARGET`.
pub fn plan(arg: &str, settings: &skink_config::Settings) -> Result<Plan, String> {
    if let Some((name, r)) = settings.remotes.iter().find(|(n, _)| n == arg) {
        let mut target = parse_target(&r.host)?;
        if !r.path.is_empty() {
            target.path = r.path.clone();
        }
        return Ok(Plan {
            target,
            forwards: r.forwards.clone(),
            policy: r.autoforward.unwrap_or(settings.ssh_autoforward),
            name: Some(name.clone()),
        });
    }
    Ok(Plan {
        target: parse_target(arg)?,
        forwards: Vec::new(),
        policy: settings.ssh_autoforward,
        name: None,
    })
}

/// The whole `skink ssh` run; the exit code of the remote editor.
pub fn main(args: &[String]) -> i32 {
    let Some(arg) = args.iter().find(|a| !a.starts_with('-')) else {
        eprintln!(
            "usage: skink ssh [user@]host[:port][:path] | NAME   (NAME: a \"remotes\" entry in settings.json)"
        );
        return 2;
    };
    let config = skink_config::config_dir();
    let (settings, errs) = match &config {
        Some(dir) => skink_config::Settings::load(dir),
        None => Default::default(),
    };
    for e in errs {
        eprintln!("skink: settings.json: {e}");
    }
    let plan = match plan(arg, &settings) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("skink: {e}");
            return 2;
        }
    };
    let state = skink_config::state_dir().unwrap_or_else(std::env::temp_dir);
    let ssh = Ssh::new(&plan.target, &state);
    let host = plan
        .name
        .clone()
        .unwrap_or_else(|| plan.target.dest.clone());
    let say = |c: &Conn| eprintln!("skink: {}", c.describe(&host));
    match run(&plan, ssh, &state, &say) {
        Ok(code) => code,
        Err(why) => {
            say(&Conn::Lost(why));
            1
        }
    }
}

/// Connect, install if needed, forward, run; `say` reports each state.
pub fn run(plan: &Plan, ssh: Ssh, state: &Path, say: &dyn Fn(&Conn)) -> Result<i32, String> {
    say(&Conn::Connecting);
    ssh.connect()?;
    say(&Conn::Installing(Step::Probing));
    let p = probe(&ssh)?;
    if p.needs_install() {
        let target = p.target.to_owned();
        if update::target() != Some(p.target) {
            say(&Conn::Installing(Step::Downloading {
                target: target.clone(),
            }));
        }
        let bin = binary_for(p.target, &state.join("remote-bin"))?;
        say(&Conn::Installing(Step::Uploading { target }));
        upload(&ssh, &bin)?;
    }
    let token = crate::remote::new_token();
    let mut fwd = Forwarder::new(ssh.clone(), token.clone(), plan.policy);
    for f in &plan.forwards {
        fwd.handle(Job::Forward {
            remote: f.remote,
            local: f.local,
            origin: Origin::Configured,
        });
    }
    for (r, l) in fwd.active() {
        if r != l {
            eprintln!("skink: port {r} on the remote is localhost:{l} here");
        }
    }
    let mut child = ssh
        .session(&session_script(&token, &plan.target.path, &passed_env()))
        .map_err(|e| format!("could not start the session: {e}"))?;
    say(&Conn::Running);
    #[cfg(windows)]
    let _ = ratatui::crossterm::ansi_support::supports_ansi();
    let (jobs, rx) = mpsc::channel::<Job>();
    let worker = std::thread::spawn(move || {
        for job in rx {
            let stop = matches!(job, Job::Stop);
            fwd.handle(job);
            if stop {
                return;
            }
        }
    });
    let stdout = child.stdout.take().ok_or("no session output")?;
    pump(stdout, &mut std::io::stdout(), &token, &jobs);
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = jobs.send(Job::Stop);
    let _ = worker.join();
    match status.code() {
        Some(255) => Err("the connection dropped".into()),
        Some(c) => Ok(c),
        None => Ok(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse_ports_paths_users_and_ipv6() {
        let t = |s| parse_target(s).unwrap();
        let ok = |dest: &str, port, path: &str| Target {
            dest: dest.into(),
            port,
            path: path.into(),
        };
        assert_eq!(t("pi"), ok("pi", None, ""));
        assert_eq!(t("pi@10.0.0.5:~/app"), ok("pi@10.0.0.5", None, "~/app"));
        assert_eq!(t("me@h:2222:/srv/x"), ok("me@h", Some(2222), "/srv/x"));
        assert_eq!(t("h:22"), ok("h", Some(22), ""));
        assert_eq!(t("h:3000app"), ok("h", None, "3000app"));
        assert_eq!(t("[::1]:2200:~/a"), ok("[::1]", Some(2200), "~/a"));
        assert_eq!(t("u@[fe80::1]"), ok("u@[fe80::1]", None, ""));
        assert!(parse_target("").is_err());
        assert!(parse_target(":22").is_err());
        assert!(parse_target("h:70000:x").is_err());
    }

    #[test]
    fn uname_maps_to_release_targets() {
        assert_eq!(
            remote_target("Linux x86_64\n"),
            Ok("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            remote_target("Linux aarch64"),
            Ok("aarch64-unknown-linux-musl")
        );
        assert_eq!(remote_target("Darwin arm64"), Ok("aarch64-apple-darwin"));
        let e = remote_target("FreeBSD amd64").unwrap_err();
        assert!(e.contains("FreeBSD"), "{e}");
    }

    #[test]
    fn quoting_survives_quotes_and_home() {
        assert_eq!(sh_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(remote_path("~/my app"), "\"$HOME\"/'my app'");
        let s = session_script("tok", "~", &[("COLORTERM".into(), "truecolor".into())]);
        assert!(s.contains("--remote tok"), "{s}");
        assert!(s.contains("COLORTERM='truecolor'"), "{s}");
    }

    /// A fake `ssh` that appends its arguments (one call per line) to a log.
    #[cfg(unix)]
    fn fake_ssh(dir: &Path) -> (Ssh, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("calls.log");
        let bin = dir.join("ssh");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ssh = Ssh {
            program: bin.display().to_string(),
            dest: "me@box".into(),
            port: None,
            mux: Mux::Control(dir.join("c.sock")),
            options: Vec::new(),
        };
        (ssh, log)
    }

    #[cfg(unix)]
    fn calls(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn opened_ports_are_forwarded_on_the_master_and_reported_back() {
        let dir = std::env::temp_dir().join(format!("skink-ssh-fwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ssh, log) = fake_ssh(&dir);
        // Hold a local port: its forward must move to a free one.
        let taken = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let busy = taken.local_addr().unwrap().port();
        let mut f = Forwarder::new(ssh.clone(), "tok".into(), AutoForward::On);
        f.handle(Job::Opened(busy));
        let (_, local) = f.active()[0];
        assert_ne!(local, busy);
        let c = calls(&log);
        assert_eq!(
            c[0],
            format!(
                "-S {} -O forward -L 127.0.0.1:{local}:localhost:{busy} me@box",
                dir.join("c.sock").display()
            )
        );
        // Nothing told yet: the editor has not said hello.
        assert_eq!(c.len(), 1);
        f.handle(Job::Hello {
            socket: "/run/s.sock".into(),
        });
        let c = calls(&log);
        assert_eq!(c.len(), 2, "{c:?}");
        let told = c[1].replace("'\"'\"'", "'");
        assert!(
            told.contains("--remote-notify '/run/s.sock' 'tok' 'forwarded'")
                && told.contains(&format!("'{busy}' '{local}'")),
            "{told}"
        );
        // The program stops listening: an automatic forward goes away.
        f.handle(Job::Closed(busy));
        let c = calls(&log);
        assert!(
            c[2].contains("-O cancel -L") && c[2].contains(&format!(":{local}:localhost:{busy}"))
        );
        assert!(c[3].contains("stopped"));
        assert!(f.active().is_empty());
        // "ask": offer, do not forward.
        let mut ask = Forwarder::new(ssh, "tok".into(), AutoForward::Ask);
        ask.handle(Job::Hello {
            socket: "/run/s.sock".into(),
        });
        ask.handle(Job::Opened(8080));
        let c = calls(&log);
        assert!(
            c.last()
                .unwrap()
                .replace("'\"'\"'", "'")
                .contains("'offer' '8080'")
        );
        assert!(ask.active().is_empty());
        drop(taken);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pump_strips_messages_and_passes_the_rest() {
        let seq = Up::Opened(3000).encode("tok");
        let fake = Up::Opened(22).encode("nope");
        let input = format!("hi\x1b[31m{seq}there{fake}\x1b");
        let (tx, rx) = mpsc::channel();
        let mut out = Vec::new();
        pump(
            std::io::Cursor::new(input.into_bytes()),
            &mut out,
            "tok",
            &tx,
        );
        assert_eq!(out, format!("hi\x1b[31mthere{fake}\x1b").into_bytes());
        let jobs: Vec<Job> = rx.try_iter().collect();
        assert!(matches!(jobs[..], [Job::Opened(3000)]), "{jobs:?}");
    }

    #[test]
    fn saved_remotes_win_over_hosts_of_the_same_text() {
        let (s, errs) = skink_config::Settings::parse(
            r#"{"remotes": {"pi": {"host": "pi@10.0.0.5:2222", "path": "~/app", "forwards": ["3000"], "autoforward": false}}, "ssh.autoforward": "ask"}"#,
        );
        assert!(errs.is_empty(), "{errs:?}");
        let p = plan("pi", &s).unwrap();
        assert_eq!(p.target.dest, "pi@10.0.0.5");
        assert_eq!(p.target.port, Some(2222));
        assert_eq!(p.target.path, "~/app");
        assert_eq!(p.policy, AutoForward::Off);
        let q = plan("other@host", &s).unwrap();
        assert_eq!(q.policy, AutoForward::Ask);
        assert!(q.forwards.is_empty());
    }
}
