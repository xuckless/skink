//! The `skink ssh` wire protocol between the remote Skink (started with
//! `--remote TOKEN`) and the local `skink ssh` process that sits between
//! the user's terminal and ssh.
//!
//! Remote -> local: private escape sequences in the terminal stream,
//! `ESC ] 7717 ; skink ; TOKEN ; VERB ; ARGS BEL`. The local side strips
//! them (and only them) before the bytes reach the user's terminal. The
//! token is random per session, so text a program prints cannot pose as
//! Skink.
//!
//! Local -> remote: a one-shot command over the same SSH connection,
//! `skink --remote-notify SOCKET TOKEN VERB ARGS`, which hands one line to
//! the remote Skink's notify socket (a Unix socket only its user can open).
//! A second channel, rather than bytes injected into the remote terminal's
//! input, because the input stream belongs to the user's keystrokes: any
//! sequence written there could interleave with a key press or be read by a
//! program in a terminal tab.

use std::path::{Path, PathBuf};

/// The private OSC number (unassigned; terminals ignore unknown ones).
pub const OSC: &str = "7717";
const PREFIX: &[u8] = b"\x1b]7717;skink;";
/// Longest body accepted; anything longer is not ours and passes through.
const MAX_BODY: usize = 512;
/// Protocol version in `hello`.
pub const VERSION: u32 = 1;

/// Remote -> local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Up {
    /// The remote Skink is up; its notify socket path.
    Hello { version: u32, socket: String },
    /// A program in a terminal tab started listening on `port`.
    Opened(u16),
    /// That port closed.
    Closed(u16),
    /// `:port R[:L]` (L 0: same port as the remote, or the next free).
    Forward { remote: u16, local: u16 },
    /// `:ports stop R`.
    Unforward(u16),
}

impl Up {
    fn verb(&self) -> String {
        match self {
            Up::Hello { version, socket } => format!("hello;{version};{socket}"),
            Up::Opened(p) => format!("open;{p}"),
            Up::Closed(p) => format!("close;{p}"),
            Up::Forward { remote, local } => format!("forward;{remote};{local}"),
            Up::Unforward(p) => format!("unforward;{p}"),
        }
    }

    /// The escape sequence for `token`.
    pub fn encode(&self, token: &str) -> String {
        format!("\x1b]{OSC};skink;{token};{}\x07", self.verb())
    }

    fn parse(body: &str) -> Option<Self> {
        let mut it = body.split(';');
        let verb = it.next()?;
        let port = |s: Option<&str>| s?.parse::<u16>().ok();
        Some(match verb {
            "hello" => Up::Hello {
                version: it.next()?.parse().ok()?,
                // A path may hold `;`: take the rest.
                socket: it.collect::<Vec<_>>().join(";"),
            },
            "open" => Up::Opened(port(it.next())?),
            "close" => Up::Closed(port(it.next())?),
            "forward" => Up::Forward {
                remote: port(it.next())?,
                local: port(it.next()).unwrap_or(0),
            },
            "unforward" => Up::Unforward(port(it.next())?),
            _ => return None,
        })
    }
}

/// Local -> remote (`--remote-notify`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Down {
    /// `remote` is reachable here on `local`.
    Forwarded { remote: u16, local: u16 },
    /// Forwarding `remote` failed.
    Failed { remote: u16, reason: String },
    /// The forward of `remote` stopped.
    Stopped(u16),
    /// `"ssh.autoforward": "ask"`: offer to forward `remote`.
    Offer(u16),
}

impl Down {
    /// As command-line words after the token.
    pub fn to_args(&self) -> Vec<String> {
        match self {
            Down::Forwarded { remote, local } => {
                vec!["forwarded".into(), remote.to_string(), local.to_string()]
            }
            Down::Failed { remote, reason } => {
                vec!["failed".into(), remote.to_string(), reason.clone()]
            }
            Down::Stopped(p) => vec!["stopped".into(), p.to_string()],
            Down::Offer(p) => vec!["offer".into(), p.to_string()],
        }
    }

    pub fn from_args(args: &[String]) -> Option<Self> {
        let port = |i: usize| args.get(i)?.parse::<u16>().ok();
        Some(match args.first()?.as_str() {
            "forwarded" => Down::Forwarded {
                remote: port(1)?,
                local: port(2)?,
            },
            "failed" => Down::Failed {
                remote: port(1)?,
                reason: args.get(2..).map(|r| r.join(" ")).unwrap_or_default(),
            },
            "stopped" => Down::Stopped(port(1)?),
            "offer" => Down::Offer(port(1)?),
            _ => return None,
        })
    }
}

/// Where the scanner is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scan {
    /// Plain output.
    Plain,
    /// `held` matches the first bytes of [`PREFIX`].
    Prefix,
    /// After the prefix: collecting `TOKEN;VERB;ARGS` until BEL or ST.
    Body { esc: bool },
}

/// Strips Skink's sequences from the remote's output. Bytes go out as soon
/// as they cannot start one; only a possible prefix (at most 14 bytes) or a
/// sequence being read is held.
pub struct Filter {
    token: String,
    scan: Scan,
    held: Vec<u8>,
}

impl Filter {
    pub fn new(token: &str) -> Self {
        Self {
            token: token.to_owned(),
            scan: Scan::Plain,
            held: Vec::new(),
        }
    }

    /// Feed a chunk: bytes for the user's terminal go to `out`, messages
    /// are returned.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Vec<Up> {
        let mut msgs = Vec::new();
        for &b in input {
            self.byte(b, out, &mut msgs);
        }
        msgs
    }

    /// Bytes held when no more input is coming soon (a lone ESC at the end
    /// of a write): release them.
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        if matches!(self.scan, Scan::Prefix) {
            out.append(&mut self.held);
            self.scan = Scan::Plain;
        }
    }

    /// Something is held (the caller flushes it after a short pause).
    pub fn holding(&self) -> bool {
        !self.held.is_empty()
    }

    fn byte(&mut self, b: u8, out: &mut Vec<u8>, msgs: &mut Vec<Up>) {
        match self.scan {
            Scan::Plain => {
                if b == PREFIX[0] {
                    self.held.push(b);
                    self.scan = Scan::Prefix;
                } else {
                    out.push(b);
                }
            }
            Scan::Prefix => {
                if b == PREFIX[self.held.len()] {
                    self.held.push(b);
                    if self.held.len() == PREFIX.len() {
                        self.scan = Scan::Body { esc: false };
                    }
                } else {
                    out.append(&mut self.held);
                    self.scan = Scan::Plain;
                    // This byte may start a sequence itself.
                    self.byte(b, out, msgs);
                }
            }
            Scan::Body { esc } => {
                let end = b == 0x07 || (esc && b == b'\\');
                self.held.push(b);
                if end {
                    let cut = if b == 0x07 { 1 } else { 2 };
                    let body = &self.held[PREFIX.len()..self.held.len() - cut];
                    match self.message(body) {
                        Some(m) => {
                            msgs.push(m);
                            self.held.clear();
                        }
                        // Not ours (wrong token, unknown verb): untouched.
                        None => out.append(&mut self.held),
                    }
                    self.scan = Scan::Plain;
                } else if self.held.len() > PREFIX.len() + MAX_BODY {
                    out.append(&mut self.held);
                    self.scan = Scan::Plain;
                } else {
                    self.scan = Scan::Body { esc: b == 0x1b };
                }
            }
        }
    }

    fn message(&self, body: &[u8]) -> Option<Up> {
        let body = std::str::from_utf8(body).ok()?;
        let (token, rest) = body.split_once(';')?;
        (token == self.token).then(|| Up::parse(rest)).flatten()
    }
}

/// A folder for Unix sockets whose paths stay under the ~104-byte limit
/// (ssh adds a 17-byte suffix while binding): `preferred` when short
/// enough, else `$XDG_RUNTIME_DIR/<sub>`, else `<tmp>/<sub>-<user>`. Made
/// private to the user.
pub fn socket_dir(preferred: &Path, sub: &str, name_len: usize) -> PathBuf {
    let fits = |d: &Path| d.as_os_str().len() + 1 + name_len + 17 <= 100;
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let candidates = [
        Some(preferred.to_path_buf()),
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
            .map(|d| d.join(sub)),
        Some(std::env::temp_dir().join(format!("{sub}-{user}"))),
        Some(PathBuf::from("/tmp").join(format!("{sub}-{user}"))),
    ];
    let dir = candidates
        .into_iter()
        .flatten()
        .find(|d| fits(d))
        .unwrap_or_else(|| preferred.to_path_buf());
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

/// A random session token (hex, 128 bits).
pub fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(f: &mut Filter, chunks: &[&[u8]]) -> (Vec<u8>, Vec<Up>) {
        let mut out = Vec::new();
        let mut msgs = Vec::new();
        for c in chunks {
            msgs.extend(f.feed(c, &mut out));
        }
        (out, msgs)
    }

    #[test]
    fn our_sequences_are_stripped_even_split_across_reads() {
        let seq = Up::Opened(3000).encode("tok");
        let all = format!("before\x1b[1mbold{seq}after");
        let bytes = all.as_bytes();
        // Every split point, including inside the prefix and the body.
        for cut in 0..bytes.len() {
            let mut f = Filter::new("tok");
            let (out, msgs) = run(&mut f, &[&bytes[..cut], &bytes[cut..]]);
            assert_eq!(out, b"before\x1b[1mboldafter", "cut at {cut}");
            assert_eq!(msgs, [Up::Opened(3000)], "cut at {cut}");
            assert!(!f.holding());
        }
        // ST terminator too.
        let mut f = Filter::new("tok");
        let (out, msgs) = run(&mut f, &[b"\x1b]7717;skink;tok;close;80\x1b\\x"]);
        assert_eq!((out.as_slice(), msgs), (&b"x"[..], vec![Up::Closed(80)]));
    }

    #[test]
    fn foreign_and_faked_sequences_pass_through_untouched() {
        let mut f = Filter::new("tok");
        let fake = Up::Opened(22).encode("guess");
        let title = "\x1b]0;my title\x07";
        let binary: Vec<u8> = (0u8..=255).collect();
        let mut input = Vec::new();
        input.extend_from_slice(fake.as_bytes());
        input.extend_from_slice(title.as_bytes());
        input.extend_from_slice(&binary);
        input.extend_from_slice(b"\x1b]7717;skink;tok;nonsense\x07");
        let (mut out, msgs) = run(&mut f, &[&input]);
        f.flush(&mut out);
        assert!(msgs.is_empty());
        assert_eq!(out, input);
        // A lone trailing ESC is held until flushed.
        let mut f = Filter::new("tok");
        let (mut out, _) = run(&mut f, &[b"ab\x1b"]);
        assert_eq!(out, b"ab");
        assert!(f.holding());
        f.flush(&mut out);
        assert_eq!(out, b"ab\x1b");
    }

    #[test]
    fn messages_round_trip() {
        for m in [
            Up::Hello {
                version: 1,
                socket: "/tmp/a;b.sock".into(),
            },
            Up::Opened(1),
            Up::Closed(65535),
            Up::Forward {
                remote: 5432,
                local: 15432,
            },
            Up::Unforward(3000),
        ] {
            let mut f = Filter::new("t");
            let (_, got) = run(&mut f, &[m.encode("t").as_bytes()]);
            assert_eq!(got, [m]);
        }
        for d in [
            Down::Forwarded {
                remote: 3000,
                local: 3001,
            },
            Down::Failed {
                remote: 1,
                reason: "port in use".into(),
            },
            Down::Stopped(2),
            Down::Offer(3),
        ] {
            assert_eq!(Down::from_args(&d.to_args()), Some(d));
        }
        assert_ne!(new_token(), new_token());
    }
}
