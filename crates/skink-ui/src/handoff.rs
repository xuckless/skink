//! Nested hand-off: a `skink FILE` started inside one of Skink's own
//! terminal tabs (by git, Claude Code, Codex, or typed) opens the file as a
//! tab of that Skink instead of drawing an editor inside the terminal, and
//! waits until the tab closes, like `code --wait`.
//!
//! Terminal tabs get `SKINK_IPC` (`unix:/path/to.sock`, or on Windows
//! `tcp:127.0.0.1:PORT`) and `SKINK_IPC_TOKEN`. The child sends one JSON
//! line ([`Request`]) and reads one back ([`Reply`]). Loopback only; the
//! token must match. The parent's accept thread blocks in `accept`, so an
//! idle Skink spends no CPU on it, and wakes the event loop on a request.
//!
//! Windows uses loopback TCP rather than a named pipe: std has no
//! named-pipe server, and the token makes the two equivalent here.

use crate::lean::LaunchMode;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

pub const ENV_ADDR: &str = "SKINK_IPC";
pub const ENV_TOKEN: &str = "SKINK_IPC_TOKEN";

/// What the child asks for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub token: String,
    /// Absolute path (the child resolves it against its own cwd).
    pub path: PathBuf,
    pub line: Option<usize>,
    pub col: Option<usize>,
    /// `Lean`: wait for the tab to close. `Project`: open a normal tab and
    /// answer at once.
    pub mode: LaunchMode,
}

/// The parent's answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Reply {
    /// A normal tab opened; nothing to wait for.
    Opened,
    /// The lean tab closed after `Ctrl-Q` / closing it.
    Done,
    /// `:cq`: the caller should abort (git does on exit code 1).
    Cancelled,
    /// Bad token or unreadable request.
    Refused(String),
}

/// Where the parent listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    #[cfg(unix)]
    Unix(PathBuf),
    Tcp(std::net::SocketAddr),
}

impl Endpoint {
    pub fn to_env(&self) -> String {
        match self {
            #[cfg(unix)]
            Endpoint::Unix(p) => format!("unix:{}", p.display()),
            Endpoint::Tcp(a) => format!("tcp:{a}"),
        }
    }

    pub fn parse(s: &str) -> Option<Endpoint> {
        if let Some(p) = s.strip_prefix("unix:") {
            #[cfg(unix)]
            return Some(Endpoint::Unix(PathBuf::from(p)));
            #[cfg(not(unix))]
            {
                let _ = p;
                return None;
            }
        }
        let addr: std::net::SocketAddr = s.strip_prefix("tcp:")?.parse().ok()?;
        addr.ip().is_loopback().then_some(Endpoint::Tcp(addr))
    }

    /// The parent's endpoint and token, when this process runs in one of
    /// Skink's terminal tabs.
    pub fn from_env() -> Option<(Endpoint, String)> {
        let addr = Endpoint::parse(&std::env::var(ENV_ADDR).ok()?)?;
        let token = std::env::var(ENV_TOKEN).ok()?;
        Some((addr, token))
    }
}

/// One accepted connection.
enum Stream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    Tcp(std::net::TcpStream),
}

impl Stream {
    fn connect(e: &Endpoint) -> io::Result<Stream> {
        Ok(match e {
            #[cfg(unix)]
            Endpoint::Unix(p) => Stream::Unix(std::os::unix::net::UnixStream::connect(p)?),
            Endpoint::Tcp(a) => Stream::Tcp(std::net::TcpStream::connect_timeout(
                a,
                Duration::from_secs(2),
            )?),
        })
    }

    fn try_clone(&self) -> io::Result<Stream> {
        Ok(match self {
            #[cfg(unix)]
            Stream::Unix(s) => Stream::Unix(s.try_clone()?),
            Stream::Tcp(s) => Stream::Tcp(s.try_clone()?),
        })
    }

    fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Stream::Unix(s) => s.set_read_timeout(t),
            Stream::Tcp(s) => s.set_read_timeout(t),
        }
    }
}

impl io::Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Stream::Unix(s) => s.read(buf),
            Stream::Tcp(s) => s.read(buf),
        }
    }
}

impl io::Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Stream::Unix(s) => s.write(buf),
            Stream::Tcp(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Stream::Unix(s) => s.flush(),
            Stream::Tcp(s) => s.flush(),
        }
    }
}

fn send_line<T: Serialize>(s: &mut Stream, v: &T) -> io::Result<()> {
    let mut line = serde_json::to_string(v).map_err(io::Error::other)?;
    line.push('\n');
    s.write_all(line.as_bytes())?;
    s.flush()
}

fn read_line<T: for<'de> Deserialize<'de>>(s: Stream) -> io::Result<T> {
    let mut line = String::new();
    if BufReader::new(s).read_line(&mut line)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "closed"));
    }
    serde_json::from_str(&line).map_err(io::Error::other)
}

/// A child waiting for its tab; answered exactly once.
pub struct Conn(Stream);

impl Conn {
    pub fn reply(mut self, r: Reply) {
        // The child may be gone (Ctrl-C in its terminal): nothing to do.
        let _ = send_line(&mut self.0, &r);
    }
}

/// Who learns how a lean document ended.
pub enum Waiter {
    /// This process: its exit code.
    Process,
    /// A child `skink` in a terminal tab.
    Remote(Conn),
}

/// A request that passed the token check.
pub struct Incoming {
    pub req: Request,
    pub conn: Conn,
}

/// The parent side, started with the first terminal tab.
pub enum ServerState {
    NotStarted,
    Listening(Server),
    /// Could not bind; tabs run without the hand-off.
    Failed,
}

pub struct Server {
    pub endpoint: Endpoint,
    pub token: String,
    rx: mpsc::Receiver<Incoming>,
}

impl Drop for Server {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Endpoint::Unix(p) = &self.endpoint {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// 128 random bits from std's per-process hash keys (seeded by the OS).
fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let part = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.finish()
    };
    format!("{:016x}{:016x}", part(), part())
}

impl Server {
    /// Listen in `dir` (Unix socket) or on loopback TCP.
    pub fn start(dir: &std::path::Path, wake: crate::WakeFn) -> io::Result<Server> {
        let token = new_token();
        let (tx, rx) = mpsc::channel();
        #[cfg(unix)]
        let endpoint = {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join(format!("skink-{}-{}.sock", std::process::id(), &token[..8]));
            let _ = std::fs::remove_file(&path);
            let l = std::os::unix::net::UnixListener::bind(&path)?;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            let (token, wake) = (token.clone(), wake.clone());
            std::thread::spawn(move || {
                for s in l.incoming().flatten() {
                    serve(Stream::Unix(s), &token, &tx, &wake);
                }
            });
            Endpoint::Unix(path)
        };
        #[cfg(not(unix))]
        let endpoint = {
            let _ = dir;
            let l = std::net::TcpListener::bind(("127.0.0.1", 0))?;
            let addr = l.local_addr()?;
            let (token, wake) = (token.clone(), wake.clone());
            std::thread::spawn(move || {
                for s in l.incoming().flatten() {
                    serve(Stream::Tcp(s), &token, &tx, &wake);
                }
            });
            Endpoint::Tcp(addr)
        };
        Ok(Server {
            endpoint,
            token,
            rx,
        })
    }

    /// Loopback TCP on any OS (tests exercise the Windows transport here).
    #[cfg(test)]
    pub fn start_tcp(wake: crate::WakeFn) -> io::Result<Server> {
        let token = new_token();
        let (tx, rx) = mpsc::channel();
        let l = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let addr = l.local_addr()?;
        let (t, w) = (token.clone(), wake.clone());
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                serve(Stream::Tcp(s), &t, &tx, &w);
            }
        });
        Ok(Server {
            endpoint: Endpoint::Tcp(addr),
            token,
            rx,
        })
    }

    pub fn try_recv(&self) -> Option<Incoming> {
        self.rx.try_recv().ok()
    }
}

/// Read one request (a short thread per connection, so a silent client
/// cannot block the next one), check the token, hand it to the app.
fn serve(s: Stream, token: &str, tx: &mpsc::Sender<Incoming>, wake: &crate::WakeFn) {
    let (token, tx, wake) = (token.to_owned(), tx.clone(), wake.clone());
    std::thread::spawn(move || {
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let Ok(reader) = s.try_clone() else {
            return;
        };
        let conn = Conn(s);
        let req: Request = match read_line(reader) {
            Ok(r) => r,
            Err(e) => return conn.reply(Reply::Refused(format!("bad request: {e}"))),
        };
        if !same(&req.token, &token) {
            return conn.reply(Reply::Refused("wrong token".into()));
        }
        let _ = conn.0.set_read_timeout(None);
        if tx.send(Incoming { req, conn }).is_ok() {
            wake();
        }
    });
}

/// Compare without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Child side: ask the parent and wait for its answer. `Err` when the
/// parent is not reachable (the caller then opens the file itself).
pub fn ask(endpoint: &Endpoint, req: &Request) -> Result<Reply, AskError> {
    let mut s = Stream::connect(endpoint).map_err(AskError::Unreachable)?;
    send_line(&mut s, req).map_err(AskError::Unreachable)?;
    read_line(s).map_err(AskError::Lost)
}

#[derive(Debug)]
pub enum AskError {
    /// Nobody listens: open the file here instead.
    Unreachable(io::Error),
    /// The parent went away while the tab was open.
    Lost(io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lean::{GitMsg, Lean, LeanKind, Why};
    use std::sync::Arc;

    fn wake() -> crate::WakeFn {
        Arc::new(|| {})
    }

    fn req(token: &str, mode: LaunchMode) -> Request {
        Request {
            token: token.into(),
            path: PathBuf::from("/x/.git/COMMIT_EDITMSG"),
            line: None,
            col: None,
            mode,
        }
    }

    fn lean() -> LaunchMode {
        LaunchMode::Lean(Lean {
            kind: LeanKind::GitMessage(GitMsg::Commit),
            why: Why::GitFile,
        })
    }

    /// Child asks, parent receives, answers when "the tab closes".
    fn round_trip(server: Server) {
        let ep = server.endpoint.clone();
        let tok = server.token.clone();
        let child = std::thread::spawn(move || ask(&ep, &req(&tok, lean())));
        let got = loop {
            if let Some(i) = server.try_recv() {
                break i;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(got.req.mode, lean());
        got.conn.reply(Reply::Cancelled);
        assert_eq!(child.join().unwrap().unwrap(), Reply::Cancelled);

        let bad = ask(&server.endpoint, &req("nope", lean())).unwrap();
        assert!(matches!(bad, Reply::Refused(_)), "{bad:?}");
        assert!(server.try_recv().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn handoff_round_trip_over_a_unix_socket() {
        let dir = std::env::temp_dir().join(format!("skink-ipc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = Server::start(&dir, wake()).unwrap();
        let Endpoint::Unix(p) = &s.endpoint else {
            panic!("unix endpoint")
        };
        let p = p.clone();
        assert_eq!(
            Endpoint::parse(&s.endpoint.to_env()),
            Some(s.endpoint.clone())
        );
        round_trip(s);
        assert!(!p.exists(), "socket removed when the server goes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handoff_round_trip_over_loopback_tcp() {
        let s = Server::start_tcp(wake()).unwrap();
        assert_eq!(
            Endpoint::parse(&s.endpoint.to_env()),
            Some(s.endpoint.clone())
        );
        round_trip(s);
        assert_eq!(Endpoint::parse("tcp:10.0.0.1:80"), None, "loopback only");
    }

    #[test]
    fn nobody_listening_is_unreachable() {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = l.local_addr().unwrap();
        drop(l);
        let r = ask(&Endpoint::Tcp(addr), &req("t", lean()));
        assert!(matches!(r, Err(AskError::Unreachable(_))), "{r:?}");
    }
}
