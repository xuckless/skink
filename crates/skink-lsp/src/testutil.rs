//! In-process fake language server for tests: byte pipes built on mpsc and a
//! scripted request loop, so no OS pipes or processes are needed.

use std::{
    io::{self, BufRead, Read, Write},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    client::{LspClient, ServerSpec},
    transport::{read_message, write_message},
    types::LspEvent,
};

pub struct PipeReader {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

pub struct PipeWriter(Sender<Vec<u8>>);

pub fn pipe() -> (PipeReader, PipeWriter) {
    let (tx, rx) = mpsc::channel();
    (
        PipeReader {
            rx,
            buf: Vec::new(),
            pos: 0,
        },
        PipeWriter(tx),
    )
}

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.buf.len() {
            match self.rx.recv() {
                Ok(b) => {
                    self.buf = b;
                    self.pos = 0;
                }
                Err(_) => return Ok(0), // all writers dropped: EOF
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for PipeWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0
            .send(b.to_vec())
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub type Log = Arc<Mutex<Vec<Value>>>;

pub enum Flow {
    Continue,
    /// Stop serving without answering (simulates a crash / closed pipe).
    Crash,
}

pub struct Conn<'a> {
    w: &'a mut dyn Write,
}

impl Conn<'_> {
    pub fn send(&mut self, v: Value) {
        let _ = write_message(self.w, &serde_json::to_vec(&v).unwrap());
    }
    pub fn reply(&mut self, id: &Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }
    pub fn error(&mut self, id: &Value, code: i64, message: &str) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}));
    }
    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
    pub fn request(&mut self, id: Value, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    }
}

/// Serve the protocol: answers `initialize` with `caps`, `shutdown` with null,
/// stops at `exit`, hands everything else to `handler`. Every received
/// message is appended to `log`.
pub fn serve(
    r: &mut dyn BufRead,
    w: &mut dyn Write,
    caps: &Value,
    log: &Log,
    handler: &mut dyn FnMut(&mut Conn, &Value) -> Flow,
) {
    let mut conn = Conn { w };
    while let Ok(body) = read_message(r) {
        let msg: Value = serde_json::from_slice(&body).unwrap();
        log.lock().unwrap().push(msg.clone());
        let id = msg.get("id").cloned();
        match (msg.get("method").and_then(Value::as_str), id) {
            (Some("initialize"), Some(id)) => {
                conn.reply(
                    &id,
                    json!({"capabilities": caps, "serverInfo": {"name": "fake"}}),
                );
            }
            (Some("shutdown"), Some(id)) => conn.reply(&id, Value::Null),
            (Some("exit"), _) => return,
            _ => {
                if let Flow::Crash = handler(&mut conn, &msg) {
                    return;
                }
            }
        }
    }
}

pub struct Harness {
    pub client: LspClient,
    pub log: Log,
    pub events: Receiver<LspEvent>,
    pub join: Option<JoinHandle<()>>,
}

impl Harness {
    /// Wait until the fake received a message matching `pred`.
    pub fn wait_for(&self, pred: impl Fn(&Value) -> bool) -> Value {
        wait_for(&self.log, pred)
    }

    pub fn wait_method(&self, method: &str) -> Value {
        self.wait_for(|m| m.get("method").and_then(Value::as_str) == Some(method))
    }

    pub fn count_method(&self, method: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.get("method").and_then(Value::as_str) == Some(method))
            .count()
    }

    pub fn recv_event(&self) -> LspEvent {
        self.events
            .recv_timeout(Duration::from_secs(5))
            .expect("event")
    }
}

pub fn wait_for(log: &Log, pred: impl Fn(&Value) -> bool) -> Value {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(m) = log.lock().unwrap().iter().find(|m| pred(m)) {
            return m.clone();
        }
        assert!(
            Instant::now() < end,
            "timed out waiting for message; log = {:#?}",
            log.lock().unwrap()
        );
        thread::sleep(Duration::from_millis(2));
    }
}

pub const ROOT: &str = "file:///tmp/skink-test-root";

pub fn harness_with(
    spec: ServerSpec,
    caps: Value,
    mut handler: impl FnMut(&mut Conn, &Value) -> Flow + Send + 'static,
) -> Harness {
    let (client_r, mut server_w) = pipe(); // server -> client
    let (server_r, client_w) = pipe(); // client -> server
    let log: Log = Arc::default();
    let slog = log.clone();
    let join = thread::spawn(move || {
        let mut r = io::BufReader::new(server_r);
        serve(&mut r, &mut server_w, &caps, &slog, &mut handler);
    });
    let client = LspClient::from_io("fake", Box::new(client_r), Box::new(client_w), &spec, ROOT)
        .expect("handshake");
    let events = client.take_events().unwrap();
    Harness {
        client,
        log,
        events,
        join: Some(join),
    }
}

pub fn harness(
    caps: Value,
    handler: impl FnMut(&mut Conn, &Value) -> Flow + Send + 'static,
) -> Harness {
    harness_with(ServerSpec::default(), caps, handler)
}

/// A handler that never answers anything.
pub fn silent(_: &mut Conn, _: &Value) -> Flow {
    Flow::Continue
}
