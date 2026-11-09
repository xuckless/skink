//! One debug adapter connection: a child process spoken to over its stdio
//! or over a loopback TCP port it listens on (codelldb, delve, js-debug).
//! Threads and channels, no async runtime. Nothing here blocks the caller:
//! connecting (TCP retries) happens on the writer thread, and messages
//! queue until the connection is up.

use crate::Wake;
use serde_json::{Value, json};
use skink_lsp::transport::{frame, read_message};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// How Skink talks to the adapter process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Stdio,
    /// The adapter listens on `127.0.0.1:port` (the port is already in its
    /// arguments).
    Tcp {
        port: u16,
    },
}

/// How to start a debug adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterProcess {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub transport: Transport,
}

/// Something the adapter sent (or the connection's end).
#[derive(Debug, Clone)]
pub enum Incoming {
    Response {
        request_seq: i64,
        success: bool,
        message: Option<String>,
        body: Value,
    },
    Event {
        event: String,
        body: Value,
    },
    /// A reverse request (`runInTerminal`, `startDebugging`).
    Request {
        seq: i64,
        command: String,
        arguments: Value,
    },
    /// A line the adapter printed on stderr.
    Stderr(String),
    /// The connection is gone (adapter exited, could not connect, bad data).
    Closed(String),
}

/// What the writer thread starts from.
enum Connect {
    Ready {
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
    },
    Tcp {
        port: u16,
    },
}

const TCP_CONNECT_FOR: Duration = Duration::from_secs(10);

pub struct Client {
    out: Sender<Vec<u8>>,
    rx: Receiver<Incoming>,
    seq: i64,
    child: Option<Child>,
}

impl Client {
    /// Start the adapter process. Returns at once; a TCP adapter is
    /// connected to in the background.
    pub fn spawn(p: &AdapterProcess, wake: Wake) -> io::Result<Client> {
        let mut cmd = Command::new(&p.program);
        cmd.args(&p.args)
            .envs(p.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &p.cwd {
            cmd.current_dir(cwd);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", p.program.display())))?;
        let (tx, rx) = mpsc::channel();
        if let Some(err) = child.stderr.take() {
            let (tx, wake) = (tx.clone(), wake.clone());
            thread::Builder::new()
                .name("dap-stderr".into())
                .spawn(move || {
                    let mut r = BufReader::new(err);
                    let mut line = String::new();
                    while matches!(r.read_line(&mut line), Ok(n) if n > 0) {
                        if tx.send(Incoming::Stderr(line.trim_end().into())).is_err() {
                            return;
                        }
                        wake();
                        line.clear();
                    }
                })?;
        }
        let connect = match p.transport {
            Transport::Stdio => {
                let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
                    let _ = child.kill();
                    return Err(io::Error::other("adapter stdio not piped"));
                };
                Connect::Ready {
                    reader: Box::new(stdout),
                    writer: Box::new(stdin),
                }
            }
            // Its stdout carries logs, not the protocol: drained as stderr is.
            Transport::Tcp { port } => {
                if let Some(out) = child.stdout.take() {
                    let (tx, wake) = (tx.clone(), wake.clone());
                    thread::spawn(move || {
                        for line in BufReader::new(out).lines().map_while(Result::ok) {
                            if tx.send(Incoming::Stderr(line)).is_err() {
                                return;
                            }
                            wake();
                        }
                    });
                }
                Connect::Tcp { port }
            }
        };
        let out = start_io(connect, tx, wake)?;
        Ok(Client {
            out,
            rx,
            seq: 0,
            child: Some(child),
        })
    }

    /// Run over arbitrary streams (tests, a fake adapter): `reader` is the
    /// adapter's output, `writer` its input.
    pub fn from_io(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        wake: Wake,
    ) -> io::Result<Client> {
        let (tx, rx) = mpsc::channel();
        let out = start_io(Connect::Ready { reader, writer }, tx, wake)?;
        Ok(Client {
            out,
            rx,
            seq: 0,
            child: None,
        })
    }

    /// Send a request; returns its `seq` (matched by responses).
    pub fn request(&mut self, command: &str, arguments: Value) -> i64 {
        self.seq += 1;
        let msg = json!({
            "seq": self.seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
        let _ = self.out.send(msg.to_string().into_bytes());
        self.seq
    }

    /// Answer a reverse request.
    pub fn respond(&mut self, request_seq: i64, command: &str, result: Result<Value, String>) {
        self.seq += 1;
        let (success, message, body) = match result {
            Ok(b) => (true, Value::Null, b),
            Err(m) => (false, Value::String(m), Value::Null),
        };
        let msg = json!({
            "seq": self.seq,
            "type": "response",
            "request_seq": request_seq,
            "success": success,
            "command": command,
            "message": message,
            "body": body,
        });
        let _ = self.out.send(msg.to_string().into_bytes());
    }

    /// Everything received since the last call, without blocking.
    pub fn drain(&self) -> Vec<Incoming> {
        self.rx.try_iter().collect()
    }

    /// Stop the adapter process now.
    pub fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The writer thread: connect, start the reader, then write queued
/// messages in order.
fn start_io(connect: Connect, tx: Sender<Incoming>, wake: Wake) -> io::Result<Sender<Vec<u8>>> {
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
    thread::Builder::new()
        .name("dap-writer".into())
        .spawn(move || {
            let closed = |why: String| {
                let _ = tx.send(Incoming::Closed(why));
                wake();
            };
            let (reader, mut writer): (Box<dyn Read + Send>, Box<dyn Write + Send>) = match connect
            {
                Connect::Ready { reader, writer } => (reader, writer),
                Connect::Tcp { port } => match connect_tcp(port) {
                    Ok(s) => match s.try_clone() {
                        Ok(r) => (Box::new(r), Box::new(s)),
                        Err(e) => return closed(format!("adapter socket: {e}")),
                    },
                    Err(e) => return closed(e),
                },
            };
            let (rtx, rwake) = (tx.clone(), wake.clone());
            if let Err(e) = thread::Builder::new()
                .name("dap-reader".into())
                .spawn(move || read_loop(reader, rtx, rwake))
            {
                return closed(format!("reader thread: {e}"));
            }
            for msg in out_rx {
                if writer
                    .write_all(&frame(&msg))
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    return; // the reader reports the closed pipe
                }
            }
        })?;
    Ok(out_tx)
}

fn connect_tcp(port: u16) -> Result<TcpStream, String> {
    let end = Instant::now() + TCP_CONNECT_FOR;
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) if Instant::now() >= end => {
                return Err(format!(
                    "could not connect to the adapter on port {port}: {e}"
                ));
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn read_loop(reader: Box<dyn Read + Send>, tx: Sender<Incoming>, wake: Wake) {
    let mut r = BufReader::new(reader);
    loop {
        let msg = match read_message(&mut r) {
            Ok(body) => match serde_json::from_slice::<Value>(&body) {
                Ok(v) => classify(v),
                Err(e) => Some(Incoming::Closed(format!("adapter sent bad JSON: {e}"))),
            },
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                Some(Incoming::Closed("the debug adapter exited".into()))
            }
            Err(e) => Some(Incoming::Closed(format!("adapter connection: {e}"))),
        };
        let Some(msg) = msg else { continue };
        let done = matches!(msg, Incoming::Closed(_));
        if tx.send(msg).is_err() {
            return;
        }
        wake();
        if done {
            return;
        }
    }
}

fn classify(mut v: Value) -> Option<Incoming> {
    let take = |v: &mut Value, k: &str| v.get_mut(k).map(Value::take).unwrap_or(Value::Null);
    match v.get("type")?.as_str()? {
        "response" => Some(Incoming::Response {
            request_seq: v.get("request_seq")?.as_i64()?,
            success: v.get("success").and_then(Value::as_bool).unwrap_or(false),
            message: v.get("message").and_then(Value::as_str).map(str::to_owned),
            body: take(&mut v, "body"),
        }),
        "event" => Some(Incoming::Event {
            event: v.get("event")?.as_str()?.to_owned(),
            body: take(&mut v, "body"),
        }),
        "request" => Some(Incoming::Request {
            seq: v.get("seq")?.as_i64()?,
            command: v.get("command")?.as_str()?.to_owned(),
            arguments: take(&mut v, "arguments"),
        }),
        _ => None,
    }
}
