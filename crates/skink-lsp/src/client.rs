//! One language-server connection: spawn, handshake, request/response
//! routing, notifications and shutdown. Plain threads and `std::sync::mpsc`,
//! no async runtime.
//!
//! Threads per client: a **reader** (parses frames, routes responses to the
//! waiting `Pending`, answers server->client requests, publishes events), a
//! **writer** (drains an outgoing queue so a stalled server can never block
//! the editor) and, for child processes, a **stderr** forwarder.
//!
//! Structure (spawn / initialize+initialized / shutdown+exit, id -> response
//! routing, `workspace/configuration` etc. replies) follows
//! lapce-proxy/src/plugin/{lsp,psp}.rs; see NOTICES.md.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    doc::Doc,
    transport::{frame, read_message},
    types::*,
    uri::{path_to_uri, uri_key, uri_to_path},
    wire::{self, Docs, Resolver},
};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_INIT_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const EXIT_GRACE: Duration = Duration::from_secs(1);
/// Bytes queued for a server that does not read its stdin. Past this the
/// server is considered wedged and killed instead of buffering without bound
/// (every full-sync `didChange` carries the whole document).
const MAX_QUEUED: usize = if cfg!(test) { 1 << 20 } else { 128 << 20 };

/// How to start a server process.
#[derive(Debug, Clone, Default)]
pub struct ServerSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// `initializationOptions` sent in `initialize`.
    pub init_options: Option<Value>,
    /// Answers for `workspace/configuration` (looked up by dotted section).
    pub settings: Option<Value>,
    /// Handshake budget (default 30 s).
    pub init_timeout: Option<Duration>,
}

impl ServerSpec {
    pub fn new(program: impl Into<String>, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            ..Self::default()
        }
    }

    /// Short display name: the program's file stem.
    pub fn name(&self) -> String {
        Path::new(&self.program).file_stem().map_or_else(
            || self.program.clone(),
            |s| s.to_string_lossy().into_owned(),
        )
    }
}

/// Where a client delivers [`LspEvent`]s. Cloneable; a registry shares one
/// sink between all its servers. The optional waker is called after every
/// event so a UI loop blocked on its own queue can wake up and drain.
#[derive(Clone)]
pub struct EventSink {
    tx: Sender<LspEvent>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl EventSink {
    pub fn new() -> (Self, Receiver<LspEvent>) {
        let (tx, rx) = mpsc::channel();
        (Self { tx, waker: None }, rx)
    }

    pub fn with_waker(mut self, waker: impl Fn() + Send + Sync + 'static) -> Self {
        self.waker = Some(Arc::new(waker));
        self
    }

    fn send(&self, e: LspEvent) {
        if self.tx.send(e).is_ok()
            && let Some(w) = &self.waker
        {
            w();
        }
    }
}

type Reply = std::result::Result<Value, (i64, String)>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SyncKind {
    None,
    Full,
    Incremental,
}

struct Caps {
    raw: Value,
    sync: SyncKind,
    save_text: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Shared {
    name: Arc<str>,
    out: Sender<Vec<u8>>,
    /// Bytes handed to the writer thread and not yet written.
    queued: Arc<AtomicUsize>,
    pending: Mutex<HashMap<i64, Sender<Reply>>>,
    next_id: AtomicI64,
    alive: AtomicBool,
    closing: AtomicBool,
    docs: Docs,
    /// Keyed by [`uri_key`] so any spelling of a path finds its entry.
    diagnostics: Mutex<HashMap<String, (PathBuf, Vec<Diagnostic>)>>,
    caps: OnceLock<Caps>,
    sink: EventSink,
    settings: Option<Value>,
    root_uri: String,
    child: Mutex<Option<Child>>,
    timeout_ms: AtomicU64,
}

impl Shared {
    fn send_value(&self, v: &Value) -> Result<()> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(LspError::Disconnected);
        }
        let body = serde_json::to_vec(v).map_err(|e| LspError::Protocol(e.to_string()))?;
        let bytes = frame(&body);
        let n = bytes.len();
        if self.queued.fetch_add(n, Ordering::AcqRel) + n > MAX_QUEUED {
            self.queued.fetch_sub(n, Ordering::AcqRel);
            self.wedged();
            return Err(LspError::Disconnected);
        }
        self.out.send(bytes).map_err(|_| LspError::Disconnected)
    }

    /// The server stopped reading its input: fail everything now and kill
    /// it (its reader then sees EOF and reports `Exited`).
    fn wedged(&self) {
        self.message(
            LogLevel::Error,
            "server stopped reading its input; killing it".into(),
            false,
        );
        self.disconnect();
        if let Some(child) = lock(&self.child).as_mut() {
            let _ = child.kill();
        }
    }

    /// Mark the connection dead and wake every waiter with `Disconnected`.
    fn disconnect(&self) {
        let mut p = lock(&self.pending);
        self.alive.store(false, Ordering::Release);
        p.clear(); // dropping the senders wakes every waiter
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send_value(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn request(&self, method: &str, params: Value) -> Result<(i64, Receiver<Reply>)> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        {
            let mut p = lock(&self.pending);
            if !self.alive.load(Ordering::Acquire) {
                return Err(LspError::Disconnected);
            }
            p.insert(id, tx);
        }
        if let Err(e) = self
            .send_value(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        {
            lock(&self.pending).remove(&id);
            return Err(e);
        }
        Ok((id, rx))
    }

    fn reply(&self, id: Value, result: std::result::Result<Value, (i64, &str)>) {
        let msg = match result {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
            Err((code, m)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": m}})
            }
        };
        let _ = self.send_value(&msg);
    }

    fn with_resolver<R>(&self, f: impl FnOnce(&mut Resolver) -> R) -> R {
        f(&mut Resolver::new(&self.docs))
    }

    fn event(&self, e: LspEvent) {
        self.sink.send(e);
    }

    fn message(&self, level: LogLevel, text: String, show: bool) {
        self.event(LspEvent::Message {
            server: self.name.clone(),
            level,
            text,
            show,
        });
    }

    /// `[{uri, name}]` for the root; the name is the decoded last path segment.
    fn workspace_folders(&self) -> Value {
        let name = uri_to_path(&self.root_uri)
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "workspace".to_string());
        json!([{"uri": self.root_uri, "name": name}])
    }

    fn sync(&self) -> SyncKind {
        self.caps.get().map_or(SyncKind::Full, |c| c.sync)
    }
}

/// An in-flight request. `wait` for the typed result, or poll with `try_get`.
/// Dropping an unfinished `Pending` (or a timeout) sends `$/cancelRequest`.
pub struct Pending<T> {
    shared: Arc<Shared>,
    id: i64,
    rx: Receiver<Reply>,
    conv: Option<Conv<T>>,
    done: bool,
}

type Conv<T> = Box<dyn FnOnce(Value, &Shared) -> Result<T> + Send>;

impl<T> Pending<T> {
    fn finish(&mut self, reply: Reply) -> Result<T> {
        self.done = true;
        match reply {
            Ok(v) => (self.conv.take().expect("pending finished twice"))(v, &self.shared),
            Err((code, message)) => Err(LspError::Server { code, message }),
        }
    }

    fn cancel(&mut self) {
        if !self.done {
            self.done = true;
            lock(&self.shared.pending).remove(&self.id);
            let _ = self
                .shared
                .notify("$/cancelRequest", json!({"id": self.id}));
        }
    }

    /// Block for the result; on timeout the request is cancelled.
    pub fn wait(mut self, timeout: Duration) -> Result<T> {
        match self.rx.recv_timeout(timeout) {
            Ok(reply) => self.finish(reply),
            Err(RecvTimeoutError::Timeout) => {
                self.cancel();
                Err(LspError::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.done = true;
                Err(LspError::Disconnected)
            }
        }
    }

    /// Non-blocking poll: `None` while the server has not answered yet.
    pub fn try_get(&mut self) -> Option<Result<T>> {
        if self.done {
            return Some(Err(LspError::Disconnected));
        }
        match self.rx.try_recv() {
            Ok(reply) => Some(self.finish(reply)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.done = true;
                Some(Err(LspError::Disconnected))
            }
        }
    }

    /// Cancel explicitly (also done on drop).
    pub fn abort(mut self) {
        self.cancel();
    }
}

impl<T> Drop for Pending<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Connection to one language server.
pub struct LspClient {
    sh: Arc<Shared>,
    events: Mutex<Option<Receiver<LspEvent>>>,
}

impl LspClient {
    /// Start `spec`, run the `initialize`/`initialized` handshake for
    /// `root_uri` and return a ready client. Events are read from the
    /// receiver returned by [`LspClient::take_events`]. Blocks until the handshake completes (up to
    /// `spec.init_timeout`): call it off the UI thread.
    pub fn spawn(spec: ServerSpec, root_uri: &str) -> Result<LspClient> {
        let (sink, rx) = EventSink::new();
        let c = Self::spawn_with(spec, root_uri, sink)?;
        *lock(&c.events) = Some(rx);
        Ok(c)
    }

    /// Like [`spawn`](Self::spawn) but delivering events to a shared `sink`
    /// (then [`take_events`](Self::take_events) on this client returns `None`).
    pub fn spawn_with(spec: ServerSpec, root_uri: &str, sink: EventSink) -> Result<LspClient> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        // Adapted from lapce/lapce-proxy/src/plugin/lsp.rs (LspClient::process) @ f66ffaa,
        // Apache-2.0, Copyright (c) Lapce Authors. Changes: only the no-console flag kept.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => LspError::NotInstalled(spec.program.clone()),
            _ => LspError::Io(format!("{}: {e}", spec.program)),
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err(LspError::Io("server stdio not piped".into()));
        };
        let name: Arc<str> = spec.name().into();
        let err_sink = sink.clone();
        let err_name = name.clone();
        thread::Builder::new()
            .name(format!("lsp-stderr-{name}"))
            .spawn(move || {
                let mut r = BufReader::new(stderr);
                let mut line = String::new();
                while matches!(r.read_line(&mut line), Ok(n) if n > 0) {
                    err_sink.send(LspEvent::Message {
                        server: err_name.clone(),
                        level: LogLevel::Stderr,
                        text: line.trim_end().to_string(),
                        show: false,
                    });
                    line.clear();
                }
            })?;
        Self::connect(
            name,
            Box::new(stdout),
            Box::new(stdin),
            Some(child),
            &spec,
            root_uri,
            sink,
        )
    }

    /// Run the protocol over arbitrary byte streams (used by tests and by
    /// anything that is not a child process). `reader` is the server's
    /// output, `writer` its input.
    pub fn from_io(
        name: &str,
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        spec: &ServerSpec,
        root_uri: &str,
    ) -> Result<LspClient> {
        let (sink, rx) = EventSink::new();
        let c = Self::connect(name.into(), reader, writer, None, spec, root_uri, sink)?;
        *lock(&c.events) = Some(rx);
        Ok(c)
    }

    fn connect(
        name: Arc<str>,
        reader: Box<dyn Read + Send>,
        mut writer: Box<dyn Write + Send>,
        child: Option<Child>,
        spec: &ServerSpec,
        root_uri: &str,
        sink: EventSink,
    ) -> Result<LspClient> {
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let shared = Arc::new(Shared {
            name: name.clone(),
            out: out_tx,
            queued: Arc::clone(&queued),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            alive: AtomicBool::new(true),
            closing: AtomicBool::new(false),
            docs: Mutex::new(HashMap::new()),
            diagnostics: Mutex::new(HashMap::new()),
            caps: OnceLock::new(),
            sink,
            settings: spec.settings.clone(),
            root_uri: root_uri.to_string(),
            child: Mutex::new(child),
            timeout_ms: AtomicU64::new(DEFAULT_REQUEST_TIMEOUT.as_millis() as u64),
        });
        // Writer: ends when every sender is gone or the pipe breaks.
        thread::Builder::new()
            .name(format!("lsp-writer-{name}"))
            .spawn(move || {
                use std::io::Write as _;
                for bytes in out_rx {
                    if writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        return;
                    }
                    queued.fetch_sub(bytes.len(), Ordering::AcqRel);
                }
            })?;
        let sh = shared.clone();
        thread::Builder::new()
            .name(format!("lsp-reader-{name}"))
            .spawn(move || reader_loop(&sh, &mut BufReader::new(reader)))?;

        let client = LspClient {
            sh: shared,
            events: Mutex::new(None),
        };
        if let Err(e) = client.handshake(spec) {
            client.abort();
            return Err(e);
        }
        Ok(client)
    }

    fn handshake(&self, spec: &ServerSpec) -> Result<()> {
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "skink", "version": env!("CARGO_PKG_VERSION")},
            "rootUri": self.sh.root_uri,
            "workspaceFolders": self.sh.workspace_folders(),
            "capabilities": client_capabilities(),
            "initializationOptions": spec.init_options,
        });
        let result = self
            .request_raw("initialize", params)?
            .wait(spec.init_timeout.unwrap_or(DEFAULT_INIT_TIMEOUT))?;
        let raw = result.get("capabilities").cloned().unwrap_or(Value::Null);
        let sync_val = raw.get("textDocumentSync");
        let sync = match sync_val.and_then(|v| {
            v.as_u64()
                .or_else(|| v.get("change").and_then(Value::as_u64))
        }) {
            Some(0) => SyncKind::None,
            Some(2) => SyncKind::Incremental,
            _ => SyncKind::Full,
        };
        let save_text = sync_val
            .and_then(|v| v.get("save"))
            .and_then(|s| s.get("includeText"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let _ = self.sh.caps.set(Caps {
            raw,
            sync,
            save_text,
        });
        self.sh.notify("initialized", json!({}))
    }

    /// The receiver of this client's events (diagnostics, logs, progress,
    /// exit) when created by [`spawn`](Self::spawn) / [`from_io`](Self::from_io).
    /// Returns `None` the second time or for sink-based clients.
    pub fn take_events(&self) -> Option<Receiver<LspEvent>> {
        lock(&self.events).take()
    }

    pub fn name(&self) -> &str {
        &self.sh.name
    }

    /// Process id of the server (None for in-process test servers).
    pub fn pid(&self) -> Option<u32> {
        lock(&self.sh.child).as_ref().map(Child::id)
    }

    pub fn is_alive(&self) -> bool {
        self.sh.alive.load(Ordering::Acquire)
    }

    pub fn root_uri(&self) -> &str {
        &self.sh.root_uri
    }

    /// Default timeout used by blocking callers.
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.sh.timeout_ms.load(Ordering::Relaxed))
    }

    pub fn set_timeout(&self, d: Duration) {
        self.sh
            .timeout_ms
            .store(d.as_millis() as u64, Ordering::Relaxed);
    }

    /// The server's raw `capabilities` object from `initialize`.
    pub fn capabilities(&self) -> &Value {
        static NULL: Value = Value::Null;
        self.sh.caps.get().map_or(&NULL, |c| &c.raw)
    }

    /// Does the server advertise `key` (e.g. `"hoverProvider"`) with a truthy value?
    pub fn supports(&self, key: &str) -> bool {
        self.capabilities()
            .get(key)
            .is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)))
    }

    // ---- raw access -------------------------------------------------------

    /// Send any request; the result is the raw JSON `result`.
    pub fn request_raw(&self, method: &str, params: Value) -> Result<Pending<Value>> {
        self.request_with(method, params, |v, _| Ok(v))
    }

    pub fn notify_raw(&self, method: &str, params: Value) -> Result<()> {
        self.sh.notify(method, params)
    }

    fn request_with<T>(
        &self,
        method: &str,
        params: Value,
        conv: impl FnOnce(Value, &Shared) -> Result<T> + Send + 'static,
    ) -> Result<Pending<T>> {
        let (id, rx) = self.sh.request(method, params)?;
        Ok(Pending {
            shared: self.sh.clone(),
            id,
            rx,
            conv: Some(Box::new(conv)),
            done: false,
        })
    }

    // ---- document sync ------------------------------------------------------

    /// `textDocument/didOpen` (an already-open document is re-synced instead).
    pub fn did_open(&self, path: &Path, language_id: &str, text: &str) -> Result<()> {
        let uri = path_to_uri(path);
        let key = uri_key(&uri);
        let mut docs = lock(&self.sh.docs);
        if docs.contains_key(&key) {
            drop(docs);
            return self.did_change(path, text);
        }
        docs.insert(key, Doc::new(1, text.to_string()));
        self.sh.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": language_id, "version": 1, "text": text}}),
        )
    }

    /// Replace the whole text (full sync; valid for every server).
    pub fn did_change(&self, path: &Path, text: &str) -> Result<()> {
        let uri = path_to_uri(path);
        let mut docs = lock(&self.sh.docs);
        let doc = docs
            .get_mut(&uri_key(&uri))
            .ok_or_else(|| LspError::NotOpen(path.to_path_buf()))?;
        doc.version += 1;
        doc.set_text(text.to_string());
        if self.sh.sync() == SyncKind::None {
            return Ok(());
        }
        self.sh.notify(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri, "version": doc.version}, "contentChanges": [{"text": text}]}),
        )
    }

    /// Apply `edits` in order (each relative to the result of the previous
    /// one, char coordinates). Sent as incremental changes if the server
    /// supports it, otherwise as one full-text change.
    pub fn did_change_incremental(&self, path: &Path, edits: &[TextEdit]) -> Result<()> {
        if edits.is_empty() {
            return Ok(());
        }
        let uri = path_to_uri(path);
        let mut docs = lock(&self.sh.docs);
        let doc = docs
            .get_mut(&uri_key(&uri))
            .ok_or_else(|| LspError::NotOpen(path.to_path_buf()))?;
        doc.version += 1;
        let sync = self.sh.sync();
        let mut changes = Vec::with_capacity(edits.len());
        for e in edits {
            if sync == SyncKind::Incremental {
                let lsp = |p: Position| json!({"line": p.line, "character": doc.utf16_col(p.line, p.col)});
                changes.push(json!({"range": {"start": lsp(e.range.start), "end": lsp(e.range.end)}, "text": e.new_text}));
            }
            doc.replace(e.range, &e.new_text);
        }
        match sync {
            SyncKind::None => return Ok(()),
            SyncKind::Full => changes = vec![json!({"text": doc.text()})],
            SyncKind::Incremental => {}
        }
        self.sh.notify(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri, "version": doc.version}, "contentChanges": changes}),
        )
    }

    /// `textDocument/didSave`. Includes the text only if the server asked for it.
    pub fn did_save(&self, path: &Path) -> Result<()> {
        let uri = path_to_uri(path);
        let docs = lock(&self.sh.docs);
        let doc = docs
            .get(&uri_key(&uri))
            .ok_or_else(|| LspError::NotOpen(path.to_path_buf()))?;
        let mut params = json!({"textDocument": {"uri": uri}});
        if self.sh.caps.get().is_some_and(|c| c.save_text) {
            params["text"] = json!(doc.text());
        }
        self.sh.notify("textDocument/didSave", params)
    }

    pub fn did_close(&self, path: &Path) -> Result<()> {
        let uri = path_to_uri(path);
        if lock(&self.sh.docs).remove(&uri_key(&uri)).is_none() {
            return Ok(());
        }
        self.sh.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": uri}}),
        )
    }

    pub fn is_open(&self, path: &Path) -> bool {
        lock(&self.sh.docs).contains_key(&uri_key(&path_to_uri(path)))
    }

    /// How many documents are open on this server.
    pub fn open_count(&self) -> usize {
        lock(&self.sh.docs).len()
    }

    // ---- queries --------------------------------------------------------------

    fn doc_params(&self, path: &Path) -> Result<(String, Value)> {
        let uri = path_to_uri(path);
        if !lock(&self.sh.docs).contains_key(&uri_key(&uri)) {
            return Err(LspError::NotOpen(path.to_path_buf()));
        }
        let td = json!({"uri": uri});
        Ok((uri, td))
    }

    fn pos_params(&self, path: &Path, pos: Position) -> Result<(String, Value)> {
        let (uri, td) = self.doc_params(path)?;
        let p = self.sh.with_resolver(|r| r.lsp_pos(&uri, pos));
        Ok((uri.clone(), json!({"textDocument": td, "position": p})))
    }

    pub fn hover(&self, path: &Path, pos: Position) -> Result<Pending<Option<Hover>>> {
        let (uri, params) = self.pos_params(path, pos)?;
        self.request_with("textDocument/hover", params, move |v, sh| {
            sh.with_resolver(|r| wire::hover(r, &uri, v))
        })
    }

    pub fn definition(&self, path: &Path, pos: Position) -> Result<Pending<Vec<Location>>> {
        self.goto("textDocument/definition", path, pos)
    }

    pub fn type_definition(&self, path: &Path, pos: Position) -> Result<Pending<Vec<Location>>> {
        self.goto("textDocument/typeDefinition", path, pos)
    }

    pub fn implementation(&self, path: &Path, pos: Position) -> Result<Pending<Vec<Location>>> {
        self.goto("textDocument/implementation", path, pos)
    }

    pub fn declaration(&self, path: &Path, pos: Position) -> Result<Pending<Vec<Location>>> {
        self.goto("textDocument/declaration", path, pos)
    }

    pub fn range_formatting(
        &self,
        path: &Path,
        range: Range,
        opts: FormattingOptions,
    ) -> Result<Pending<Vec<TextEdit>>> {
        let (uri, td) = self.doc_params(path)?;
        let (start, end) = self
            .sh
            .with_resolver(|r| (r.lsp_pos(&uri, range.start), r.lsp_pos(&uri, range.end)));
        let params = json!({"textDocument": td, "range": {"start": start, "end": end},
            "options": {"tabSize": opts.tab_size, "insertSpaces": opts.insert_spaces}});
        self.request_with("textDocument/rangeFormatting", params, move |v, sh| {
            sh.with_resolver(|r| wire::text_edits(r, &uri, v))
        })
    }

    pub fn code_actions(
        &self,
        path: &Path,
        range: Range,
        diagnostics: &[Diagnostic],
    ) -> Result<Pending<Vec<CodeAction>>> {
        let (uri, td) = self.doc_params(path)?;
        let (start, end, diags) = self.sh.with_resolver(|r| {
            let diags: Vec<Value> = diagnostics
                .iter()
                .map(|d| {
                    json!({
                        "range": {"start": r.lsp_pos(&uri, d.range.start), "end": r.lsp_pos(&uri, d.range.end)},
                        "severity": match d.severity { Severity::Error => 1, Severity::Warning => 2, Severity::Info => 3, Severity::Hint => 4 },
                        "message": d.message,
                        "source": d.source,
                        "code": d.code,
                    })
                })
                .collect();
            (r.lsp_pos(&uri, range.start), r.lsp_pos(&uri, range.end), diags)
        });
        let params = json!({"textDocument": td, "range": {"start": start, "end": end},
            "context": {"diagnostics": diags}});
        self.request_with("textDocument/codeAction", params, |v, sh| {
            sh.with_resolver(|r| wire::code_actions(r, v))
        })
    }

    pub fn execute_command(&self, cmd: &ServerCommand) -> Result<Pending<Value>> {
        self.request_raw(
            "workspace/executeCommand",
            json!({"command": cmd.command, "arguments": cmd.arguments}),
        )
    }

    /// Does the server want `workspace/{will,did}RenameFiles` (`which` is
    /// `willRename` or `didRename`)?
    fn wants_file_op(&self, which: &str) -> bool {
        self.capabilities()
            .pointer(&format!("/workspace/fileOperations/{which}"))
            .is_some_and(|v| !v.is_null())
    }

    /// `workspace/willRenameFiles`: edits (imports, module paths) to make
    /// before moving each `(from, to)`. Unsupported if the server did not ask
    /// for file operations.
    pub fn will_rename_files(
        &self,
        renames: &[(PathBuf, PathBuf)],
    ) -> Result<Pending<Option<WorkspaceEdit>>> {
        if !self.wants_file_op("willRename") {
            return Err(LspError::Unsupported("workspace/willRenameFiles"));
        }
        self.request_with(
            "workspace/willRenameFiles",
            rename_params(renames),
            |v, sh| sh.with_resolver(|r| wire::workspace_edit(r, v)),
        )
    }

    /// `workspace/didRenameFiles` (only to servers that asked for it).
    pub fn did_rename_files(&self, renames: &[(PathBuf, PathBuf)]) -> Result<()> {
        if !self.wants_file_op("didRename") {
            return Ok(());
        }
        self.sh
            .notify("workspace/didRenameFiles", rename_params(renames))
    }

    pub fn workspace_symbols(&self, query: &str) -> Result<Pending<Vec<SymbolInfo>>> {
        self.request_with("workspace/symbol", json!({"query": query}), |v, sh| {
            sh.with_resolver(|r| wire::workspace_symbols(r, v))
        })
    }

    pub fn inlay_hints(&self, path: &Path, range: Range) -> Result<Pending<Vec<InlayHint>>> {
        let (uri, td) = self.doc_params(path)?;
        let (start, end) = self
            .sh
            .with_resolver(|r| (r.lsp_pos(&uri, range.start), r.lsp_pos(&uri, range.end)));
        let params = json!({"textDocument": td, "range": {"start": start, "end": end}});
        self.request_with("textDocument/inlayHint", params, move |v, sh| {
            sh.with_resolver(|r| wire::inlay_hints(r, &uri, v))
        })
    }

    fn goto(&self, method: &str, path: &Path, pos: Position) -> Result<Pending<Vec<Location>>> {
        let (_, params) = self.pos_params(path, pos)?;
        self.request_with(method, params, |v, sh| {
            sh.with_resolver(|r| wire::locations(r, v))
        })
    }

    pub fn references(
        &self,
        path: &Path,
        pos: Position,
        include_declaration: bool,
    ) -> Result<Pending<Vec<Location>>> {
        let (_, mut params) = self.pos_params(path, pos)?;
        params["context"] = json!({"includeDeclaration": include_declaration});
        self.request_with("textDocument/references", params, |v, sh| {
            sh.with_resolver(|r| wire::locations(r, v))
        })
    }

    pub fn completion(&self, path: &Path, pos: Position) -> Result<Pending<Vec<CompletionItem>>> {
        let (uri, mut params) = self.pos_params(path, pos)?;
        params["context"] = json!({"triggerKind": 1});
        self.request_with("textDocument/completion", params, move |v, sh| {
            sh.with_resolver(|r| wire::completion(r, &uri, v))
        })
    }

    /// Fetch the documentation of an item that left it out
    /// (`completionItem/resolve`); `None` when the server has none.
    pub fn resolve_completion(&self, item: &Value) -> Result<Pending<Option<String>>> {
        let resolves = self
            .capabilities()
            .pointer("/completionProvider/resolveProvider")
            .is_some_and(|v| v == &Value::Bool(true));
        if !resolves {
            return Err(LspError::Unsupported("completionItem/resolve"));
        }
        self.request_with("completionItem/resolve", item.clone(), |v, _| {
            Ok(wire::resolved_documentation(&v))
        })
    }

    pub fn rename(
        &self,
        path: &Path,
        pos: Position,
        new_name: &str,
    ) -> Result<Pending<Option<WorkspaceEdit>>> {
        let (_, mut params) = self.pos_params(path, pos)?;
        params["newName"] = json!(new_name);
        self.request_with("textDocument/rename", params, |v, sh| {
            sh.with_resolver(|r| wire::workspace_edit(r, v))
        })
    }

    pub fn formatting(
        &self,
        path: &Path,
        opts: FormattingOptions,
    ) -> Result<Pending<Vec<TextEdit>>> {
        let (uri, td) = self.doc_params(path)?;
        let params = json!({"textDocument": td, "options": {"tabSize": opts.tab_size, "insertSpaces": opts.insert_spaces}});
        self.request_with("textDocument/formatting", params, move |v, sh| {
            sh.with_resolver(|r| wire::text_edits(r, &uri, v))
        })
    }

    pub fn signature_help(
        &self,
        path: &Path,
        pos: Position,
    ) -> Result<Pending<Option<SignatureHelp>>> {
        let (_, params) = self.pos_params(path, pos)?;
        self.request_with("textDocument/signatureHelp", params, |v, _| {
            wire::signature_help(v)
        })
    }

    pub fn document_symbols(&self, path: &Path) -> Result<Pending<Vec<DocumentSymbol>>> {
        let (uri, td) = self.doc_params(path)?;
        self.request_with(
            "textDocument/documentSymbol",
            json!({"textDocument": td}),
            move |v, sh| sh.with_resolver(|r| wire::document_symbols(r, &uri, v)),
        )
    }

    // ---- diagnostics -----------------------------------------------------------

    /// Latest diagnostics published for `path` (empty if none).
    pub fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        lock(&self.sh.diagnostics)
            .get(&uri_key(&path_to_uri(path)))
            .map(|(_, d)| d.clone())
            .unwrap_or_default()
    }

    /// Latest diagnostics for every file that has some.
    pub fn all_diagnostics(&self) -> Vec<(PathBuf, Vec<Diagnostic>)> {
        lock(&self.sh.diagnostics).values().cloned().collect()
    }

    // ---- shutdown ----------------------------------------------------------------

    /// Hard stop without the protocol dance (failed handshake).
    fn abort(&self) {
        self.sh.closing.store(true, Ordering::Release);
        if let Some(child) = lock(&self.sh.child).as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Polite shutdown: `shutdown` request (2 s), `exit` notification, then
    /// wait up to 1 s for the process before killing it. Idempotent.
    pub fn shutdown(&self) {
        if self.sh.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.is_alive() {
            if let Ok(p) = self.request_raw("shutdown", Value::Null) {
                let _ = p.wait(SHUTDOWN_REQUEST_TIMEOUT);
            }
            let _ = self.sh.notify("exit", Value::Null);
        }
        let deadline = Instant::now() + EXIT_GRACE;
        loop {
            let mut guard = lock(&self.sh.child);
            let Some(child) = guard.as_mut() else { break };
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {}
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
            drop(guard);
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// `RenameFilesParams`.
fn rename_params(renames: &[(PathBuf, PathBuf)]) -> Value {
    let files: Vec<Value> = renames
        .iter()
        .map(|(a, b)| json!({"oldUri": path_to_uri(a), "newUri": path_to_uri(b)}))
        .collect();
    json!({ "files": files })
}

/// Minimal capabilities: everything Skink uses, nothing dynamic (so servers
/// never need `client/registerCapability`), plain-text completion, UTF-16.
pub(crate) fn client_capabilities() -> Value {
    json!({
        "general": {"positionEncodings": ["utf-16"]},
        "textDocument": {
            "synchronization": {"dynamicRegistration": false, "didSave": true, "willSave": false, "willSaveWaitUntil": false},
            "publishDiagnostics": {"relatedInformation": false, "versionSupport": true},
            "hover": {"contentFormat": ["markdown", "plaintext"]},
            "completion": {
                "completionItem": {
                    "snippetSupport": true,
                    "insertReplaceSupport": false,
                    "resolveSupport": {"properties": ["documentation"]},
                    "documentationFormat": ["markdown", "plaintext"]
                },
                "contextSupport": true
            },
            "definition": {"linkSupport": true},
            "declaration": {"linkSupport": true},
            "rangeFormatting": {},
            "codeAction": {
                "codeActionLiteralSupport": {"codeActionKind": {"valueSet": [
                    "", "quickfix", "refactor", "refactor.extract", "refactor.inline",
                    "refactor.rewrite", "source", "source.organizeImports"
                ]}},
                "isPreferredSupport": true,
                "disabledSupport": true
            },
            "inlayHint": {},
            "typeDefinition": {"linkSupport": true},
            "implementation": {"linkSupport": true},
            "references": {},
            "rename": {"prepareSupport": false},
            "formatting": {},
            "signatureHelp": {
                "signatureInformation": {
                    "documentationFormat": ["markdown", "plaintext"],
                    "parameterInformation": {"labelOffsetSupport": true},
                    "activeParameterSupport": true
                }
            },
            "documentSymbol": {"hierarchicalDocumentSymbolSupport": true}
        },
        "window": {"workDoneProgress": true, "showMessage": {}},
        "workspace": {
            "symbol": {},
            "executeCommand": {},
            "workspaceFolders": true,
            "configuration": true,
            "applyEdit": true,
            "workspaceEdit": {"documentChanges": true, "resourceOperations": ["create", "rename", "delete"]},
            "fileOperations": {"willRename": true, "didRename": true}
        }
    })
}

// ---- reader thread ---------------------------------------------------------------------

fn reader_loop(sh: &Arc<Shared>, reader: &mut dyn BufRead) {
    loop {
        match read_message(reader) {
            Ok(body) => match serde_json::from_slice::<Value>(&body) {
                Ok(msg) => dispatch(sh, msg),
                Err(e) => sh.message(
                    LogLevel::Warning,
                    format!("invalid JSON from server: {e}"),
                    false,
                ),
            },
            Err(e) => {
                if e.kind() != std::io::ErrorKind::UnexpectedEof {
                    sh.message(LogLevel::Error, format!("transport error: {e}"), false);
                }
                break;
            }
        }
    }
    // Connection is gone: fail everything waiting, then report.
    sh.disconnect();
    let code = reap_code(sh);
    sh.event(LspEvent::Exited {
        server: sh.name.clone(),
        code,
        expected: sh.closing.load(Ordering::Acquire),
    });
}

/// Exit code of the child if it ends shortly after the pipe closed.
fn reap_code(sh: &Shared) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_millis(300);
    loop {
        match lock(&sh.child).as_mut().map(Child::try_wait) {
            None | Some(Err(_)) => return None,
            Some(Ok(Some(status))) => return status.code(),
            Some(Ok(None)) if Instant::now() >= deadline => return None,
            Some(Ok(None)) => {}
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn dispatch(sh: &Arc<Shared>, mut msg: Value) {
    let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);
    let id = msg.get("id").filter(|v| !v.is_null()).cloned();
    let params = msg
        .get_mut("params")
        .map(Value::take)
        .unwrap_or(Value::Null);
    match (method, id) {
        (Some(m), Some(id)) => handle_request(sh, &m, id, params),
        (Some(m), None) => handle_notification(sh, &m, params),
        (None, Some(id)) => {
            let Some(id) = id
                .as_i64()
                .or_else(|| id.as_str().and_then(|s| s.parse().ok()))
            else {
                return;
            };
            let reply = match msg.get_mut("error").map(Value::take) {
                Some(e) if !e.is_null() => Err((
                    e.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                    e.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                )),
                _ => Ok(msg
                    .get_mut("result")
                    .map(Value::take)
                    .unwrap_or(Value::Null)),
            };
            if let Some(tx) = lock(&sh.pending).remove(&id) {
                let _ = tx.send(reply); // receiver gone = request abandoned
            }
        }
        (None, None) => {}
    }
}

// Reply set follows lapce-proxy/src/plugin/psp.rs (PluginHostHandler::handle_request:
// RegisterCapability, WorkDoneProgressCreate, ...) @ f66ffaa95387794205c35864517de70958b9ea8d,
// Apache-2.0, Copyright (c) Lapce Authors. Changes: GUI-free; adds workspace/configuration,
// workspaceFolders, applyEdit, showMessageRequest, showDocument and refresh requests.
fn handle_request(sh: &Arc<Shared>, method: &str, id: Value, params: Value) {
    match method {
        "workspace/configuration" => {
            let items = params
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let out: Vec<Value> = items
                .iter()
                .map(|it| {
                    let section = it.get("section").and_then(Value::as_str);
                    config_lookup(sh.settings.as_ref(), section)
                })
                .collect();
            sh.reply(id, Ok(Value::Array(out)));
        }
        "workspace/workspaceFolders" => sh.reply(id, Ok(sh.workspace_folders())),
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create" => {
            sh.reply(id, Ok(Value::Null));
        }
        m if m.starts_with("workspace/") && m.ends_with("/refresh") => {
            sh.reply(id, Ok(Value::Null))
        }
        "window/showMessageRequest" => {
            let level = level_from(params.get("type"));
            let text = params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            sh.message(level, text, true);
            sh.reply(id, Ok(Value::Null)); // no action chosen
        }
        "window/showDocument" => sh.reply(id, Ok(json!({"success": false}))),
        "workspace/applyEdit" => {
            let label = params
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let edit = params.get("edit").cloned().unwrap_or(Value::Null);
            match sh.with_resolver(|r| wire::workspace_edit(r, edit)) {
                Ok(Some(edit)) => {
                    sh.event(LspEvent::ApplyEdit {
                        server: sh.name.clone(),
                        label,
                        edit,
                    });
                    sh.reply(id, Ok(json!({"applied": true})));
                }
                Ok(None) => sh.reply(
                    id,
                    Ok(json!({"applied": false, "failureReason": "empty edit"})),
                ),
                Err(e) => sh.reply(
                    id,
                    Ok(json!({"applied": false, "failureReason": e.to_string()})),
                ),
            }
        }
        _ => sh.reply(id, Err((-32601, "method not found"))),
    }
}

/// `settings["a"]["b"]` for section `"a.b"`; whole object for no section.
fn config_lookup(settings: Option<&Value>, section: Option<&str>) -> Value {
    let Some(mut cur) = settings else {
        return Value::Null;
    };
    if let Some(s) = section.filter(|s| !s.is_empty()) {
        for part in s.split('.') {
            match cur.get(part) {
                Some(v) => cur = v,
                None => return Value::Null,
            }
        }
    }
    cur.clone()
}

fn level_from(t: Option<&Value>) -> LogLevel {
    match t.and_then(Value::as_u64) {
        Some(1) => LogLevel::Error,
        Some(2) => LogLevel::Warning,
        Some(3) => LogLevel::Info,
        _ => LogLevel::Log,
    }
}

fn handle_notification(sh: &Arc<Shared>, method: &str, params: Value) {
    match method {
        "textDocument/publishDiagnostics" => {
            match sh.with_resolver(|r| wire::publish_diagnostics(r, params)) {
                Ok(p) => {
                    let Some(path) = uri_to_path(&p.uri) else {
                        return;
                    };
                    {
                        let mut store = lock(&sh.diagnostics);
                        let key = uri_key(&p.uri);
                        if p.diagnostics.is_empty() {
                            store.remove(&key);
                        } else {
                            store.insert(key, (path.clone(), p.diagnostics.clone()));
                        }
                    }
                    sh.event(LspEvent::Diagnostics {
                        server: sh.name.clone(),
                        path,
                        version: p.version,
                        diagnostics: p.diagnostics,
                    });
                }
                Err(e) => sh.message(LogLevel::Warning, e.to_string(), false),
            }
        }
        "window/logMessage" | "window/showMessage" => {
            let text = params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            sh.message(
                level_from(params.get("type")),
                text,
                method == "window/showMessage",
            );
        }
        "$/progress" => {
            let token = match params.get("token") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                _ => return,
            };
            let v = params.get("value").unwrap_or(&Value::Null);
            let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
            sh.event(LspEvent::Progress {
                server: sh.name.clone(),
                token,
                title: s("title"),
                message: s("message"),
                percentage: v
                    .get("percentage")
                    .and_then(Value::as_u64)
                    .map(|p| p as u32),
                done: v.get("kind").and_then(Value::as_str) == Some("end"),
            });
        }
        _ => {} // $/logTrace, telemetry/event, ...
    }
}
