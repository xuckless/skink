//! The blocking MCP client (Streamable HTTP, legacy handshake) for EveryAI's
//! loopback host. Every call opens its own short-lived connection, so the
//! client is `Send + Sync` and a [`CancelHandle`] can be used from any thread.
//!
//! Wire facts come from the public MCP spec and from observing EveryAI's host
//! behaviour; no EveryAI code is used.

use crate::HostEndpoint;
use crate::error::{Error, Result};
use crate::http::{
    BodyReader, SseReader, TimedStream, Url, read_all_limited, read_head, valid_header_value,
    write_post,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::fmt;
use std::io::BufReader;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// Protocol versions we speak, newest first (the host's legacy set).
const SUPPORTED: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
/// Cap for a plain JSON reply and for a single SSE message.
const MAX_BODY: usize = 16 * 1024 * 1024;
const MAX_PAGES: usize = 20;

/// Deadlines for talking to the host.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// TCP connect.
    pub connect: Duration,
    /// Whole round trip of a plain request (initialize, tools/list, ...).
    pub request: Duration,
    /// Longest silence tolerated on a `tools/call` stream. The host sends
    /// keep-alive comments, so silence means the host is gone.
    pub call_idle: Duration,
    /// Optional cap on a whole `tools/call`.
    pub call_total: Option<Duration>,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(2),
            request: Duration::from_secs(10),
            call_idle: Duration::from_secs(120),
            call_total: None,
        }
    }
}

impl Timeouts {
    /// Short deadlines for the start-up probe so a dead host never delays
    /// the editor.
    pub fn probe() -> Self {
        Timeouts {
            connect: Duration::from_millis(300),
            request: Duration::from_millis(1500),
            ..Timeouts::default()
        }
    }
}

/// A `notifications/progress` event for a running call.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub progress: f64,
    pub total: Option<f64>,
    /// Free text; `prompt.run` puts JSON here, see [`crate::tools::PromptEvent`].
    pub message: Option<String>,
}

/// One entry of `tools/list`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

/// An image content block (base64 data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub mime_type: String,
    pub data: String,
}

/// The result of `tools/call`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// All text blocks joined with newlines.
    pub text: String,
    pub is_error: bool,
    pub images: Vec<Image>,
    /// `structuredContent`, if the host sent it.
    pub structured: Option<Value>,
}

impl ToolResult {
    /// Decode the result as the tool's JSON output type (structured content
    /// first, else the text as JSON). A tool-level error becomes
    /// [`Error::Tool`].
    pub fn parse<T: DeserializeOwned>(&self) -> Result<T> {
        if self.is_error {
            return Err(Error::Tool(self.text.clone()));
        }
        match &self.structured {
            Some(v) => Ok(serde_json::from_value(v.clone())?),
            None => Ok(serde_json::from_str(&self.text)?),
        }
    }

    fn from_value(v: &Value) -> ToolResult {
        let mut text = String::new();
        let mut images = Vec::new();
        for block in v["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(block["text"].as_str().unwrap_or(""));
                }
                Some("image") => images.push(Image {
                    mime_type: block["mimeType"].as_str().unwrap_or("").to_string(),
                    data: block["data"].as_str().unwrap_or("").to_string(),
                }),
                _ => {}
            }
        }
        ToolResult {
            text,
            is_error: v["isError"].as_bool().unwrap_or(false),
            images,
            structured: v.get("structuredContent").filter(|s| !s.is_null()).cloned(),
        }
    }
}

struct InFlight {
    id: u64,
    stream: TcpStream,
    cancelled: Arc<AtomicBool>,
}

struct Shared {
    url: Url,
    token: String,
    timeouts: Timeouts,
    protocol: OnceLock<String>,
    server: OnceLock<String>,
    next_id: AtomicU64,
    inflight: Mutex<Vec<InFlight>>,
}

/// Removes a call from the in-flight list when it ends.
struct Registration<'a> {
    shared: &'a Shared,
    id: u64,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        lock(&self.shared.inflight).retain(|c| c.id != self.id);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

type ProgressFn<'a> = &'a mut dyn FnMut(&Progress);

struct CallCtx<'a> {
    idle: Duration,
    total: Option<Duration>,
    /// Track the connection so a [`CancelHandle`] can reach it.
    track: bool,
    progress: Option<(Value, ProgressFn<'a>)>,
}

impl Shared {
    fn fresh(url: Url, token: String, timeouts: Timeouts) -> Shared {
        Shared {
            url,
            token,
            timeouts,
            protocol: OnceLock::new(),
            server: OnceLock::new(),
            next_id: AtomicU64::new(1),
            inflight: Mutex::new(Vec::new()),
        }
    }

    fn plain_ctx(&self) -> CallCtx<'static> {
        CallCtx {
            idle: self.timeouts.request,
            total: Some(self.timeouts.request),
            track: false,
            progress: None,
        }
    }

    /// POST one JSON-RPC message. For a request (`id` is `Some`) returns its
    /// `result`; for a notification returns `Null`.
    fn post(&self, id: Option<u64>, body: &Value, mut ctx: CallCtx<'_>) -> Result<Value> {
        let stream = self.url.connect(self.timeouts.connect)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let _reg = match (id, ctx.track) {
            (Some(id), true) => {
                lock(&self.inflight).push(InFlight {
                    id,
                    stream: stream.try_clone()?,
                    cancelled: cancelled.clone(),
                });
                Some(Registration { shared: self, id })
            }
            _ => None,
        };
        let out = self.exchange(stream, id, body, &mut ctx);
        match out {
            Err(_) if cancelled.load(Ordering::SeqCst) => Err(Error::Cancelled),
            other => other,
        }
    }

    fn exchange(
        &self,
        stream: TcpStream,
        id: Option<u64>,
        body: &Value,
        ctx: &mut CallCtx<'_>,
    ) -> Result<Value> {
        let payload = serde_json::to_vec(body)?;
        let auth = format!("Bearer {}", self.token);
        let mut headers = vec![
            ("Authorization", auth.as_str()),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ];
        if let Some(p) = self.protocol.get() {
            headers.push(("MCP-Protocol-Version", p.as_str()));
        }
        let mut r = BufReader::new(TimedStream::new(stream, ctx.idle, ctx.total));
        write_post(r.get_mut(), &self.url, &headers, &payload)?;
        let head = read_head(&mut r)?;
        let mut reader = BodyReader::new(r, &head)?;

        if !(200..300).contains(&head.status) {
            if head.status == 401 || head.status == 403 {
                return Err(Error::Unauthorized);
            }
            let raw = read_all_limited(&mut reader, 4096).unwrap_or_default();
            return Err(Error::Http {
                status: head.status,
                message: error_message(&raw),
            });
        }
        let Some(id) = id else {
            // Notification: 202 Accepted, nothing to read.
            return Ok(Value::Null);
        };

        let is_sse = head
            .header("content-type")
            .is_some_and(|c| c.to_ascii_lowercase().contains("text/event-stream"));
        if is_sse {
            let mut sse = SseReader::new(BufReader::new(reader), MAX_BODY);
            while let Some(ev) = sse.next_event()? {
                let msg: Value = serde_json::from_str(&ev.data)?;
                if let Some(done) = handle_message(&msg, id, ctx) {
                    return done;
                }
            }
        } else {
            let raw = read_all_limited(&mut reader, MAX_BODY)?;
            let msg: Value = serde_json::from_slice(&raw)?;
            let items = match &msg {
                Value::Array(a) => a.as_slice(),
                one => std::slice::from_ref(one),
            };
            for m in items {
                if let Some(done) = handle_message(m, id, ctx) {
                    return done;
                }
            }
        }
        Err(Error::Protocol("reply ended without a result".into()))
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        self.post(Some(id), &body, self.plain_ctx())
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        let body = json!({"jsonrpc":"2.0","method":method,"params":params});
        self.post(None, &body, self.plain_ctx()).map(|_| ())
    }

    fn call(
        &self,
        name: &str,
        args: &Value,
        progress: Option<ProgressFn<'_>>,
    ) -> Result<ToolResult> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut params = json!({"name": name, "arguments": args});
        let progress = progress.map(|cb| {
            let token = Value::String(format!("skink-{id}"));
            params["_meta"] = json!({"progressToken": token});
            (token, cb)
        });
        let body = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":params});
        let ctx = CallCtx {
            idle: self.timeouts.call_idle,
            total: self.timeouts.call_total,
            track: true,
            progress,
        };
        let result = self.post(Some(id), &body, ctx)?;
        Ok(ToolResult::from_value(&result))
    }
}

/// `Some` when `msg` settles request `id`; progress events are forwarded.
fn handle_message(msg: &Value, id: u64, ctx: &mut CallCtx<'_>) -> Option<Result<Value>> {
    if msg["id"].as_u64() == Some(id) {
        if let Some(e) = msg.get("error") {
            return Some(Err(Error::Rpc {
                code: e["code"].as_i64().unwrap_or(0),
                message: e["message"].as_str().unwrap_or("").to_string(),
            }));
        }
        if let Some(r) = msg.get("result") {
            return Some(Ok(r.clone()));
        }
        return Some(Err(Error::Protocol(
            "reply has neither result nor error".into(),
        )));
    }
    if msg["method"] == "notifications/progress"
        && let Some((token, cb)) = ctx.progress.as_mut()
    {
        let p = &msg["params"];
        if p["progressToken"] == *token {
            cb(&Progress {
                progress: p["progress"].as_f64().unwrap_or(0.0),
                total: p["total"].as_f64(),
                message: p["message"].as_str().map(str::to_string),
            });
        }
    }
    None
}

/// A short, token-free description of an error body.
fn error_message(raw: &[u8]) -> String {
    if let Ok(v) = serde_json::from_slice::<Value>(raw)
        && let Some(m) = v["error"]["message"].as_str()
    {
        return m.chars().take(200).collect();
    }
    String::from_utf8_lossy(raw).chars().take(200).collect()
}

/// A blocking MCP client for EveryAI's local host. Cheap to share (`&self`
/// everywhere); run calls on your own thread.
pub struct McpClient {
    shared: Arc<Shared>,
}

impl fmt::Debug for McpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpClient")
            .field("host", &self.shared.url.host_header())
            .field("protocol", &self.shared.protocol.get())
            .field("server", &self.shared.server.get())
            .field("token", &"<redacted>")
            .finish()
    }
}

impl McpClient {
    /// Connect with default [`Timeouts`]: `initialize` then
    /// `notifications/initialized`. Refuses non-loopback URLs.
    pub fn connect(endpoint: &HostEndpoint) -> Result<McpClient> {
        Self::connect_with(endpoint, Timeouts::default())
    }

    /// As [`connect`](Self::connect) with explicit deadlines.
    pub fn connect_with(endpoint: &HostEndpoint, timeouts: Timeouts) -> Result<McpClient> {
        let url = Url::parse(&endpoint.url)?;
        let token = endpoint.token();
        if token.is_empty() || !valid_header_value(token) {
            return Err(Error::InvalidToken);
        }
        let shared = Arc::new(Shared::fresh(url, token.to_string(), timeouts));
        let init = shared.request(
            "initialize",
            json!({
                "protocolVersion": SUPPORTED[0],
                "capabilities": {},
                "clientInfo": {"name": "skink", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        let version = init["protocolVersion"].as_str().unwrap_or("");
        if !SUPPORTED.contains(&version) {
            return Err(Error::Protocol(format!(
                "host chose unsupported protocol version {version:?}"
            )));
        }
        let _ = shared.protocol.set(version.to_string());
        if let Some(n) = init["serverInfo"]["name"].as_str() {
            let _ = shared.server.set(n.to_string());
        }
        shared.notify("notifications/initialized", json!({}))?;
        Ok(McpClient { shared })
    }

    /// The same client with different deadlines (call before any call is in
    /// flight; used to relax the short start-up probe).
    pub fn with_timeouts(self, timeouts: Timeouts) -> McpClient {
        let s = &self.shared;
        let n = Shared::fresh(s.url.clone(), s.token.clone(), timeouts);
        if let Some(p) = s.protocol.get() {
            let _ = n.protocol.set(p.clone());
        }
        if let Some(p) = s.server.get() {
            let _ = n.server.set(p.clone());
        }
        McpClient {
            shared: Arc::new(n),
        }
    }

    /// Negotiated protocol version.
    pub fn protocol_version(&self) -> &str {
        self.shared.protocol.get().map_or("", String::as_str)
    }

    /// `serverInfo.name` from the handshake.
    pub fn server_name(&self) -> Option<&str> {
        self.shared.server.get().map(String::as_str)
    }

    /// `tools/list`, following pagination cursors.
    pub fn list_tools(&self) -> Result<Vec<ToolInfo>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let r = self.shared.request("tools/list", params)?;
            for t in r["tools"].as_array().into_iter().flatten() {
                if let Some(name) = t["name"].as_str() {
                    out.push(ToolInfo {
                        name: name.to_string(),
                        description: t["description"].as_str().map(str::to_string),
                        input_schema: t["inputSchema"].clone(),
                    });
                }
            }
            match r["nextCursor"].as_str() {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }

    /// `tools/call`, blocking until the host's SSE stream delivers the result.
    pub fn call_tool(&self, name: &str, args: &Value) -> Result<ToolResult> {
        self.shared.call(name, args, None)
    }

    /// As [`call_tool`](Self::call_tool), forwarding `notifications/progress`
    /// events to `on_progress` (on the calling thread).
    pub fn call_tool_progress(
        &self,
        name: &str,
        args: &Value,
        on_progress: &mut dyn FnMut(&Progress),
    ) -> Result<ToolResult> {
        self.shared.call(name, args, Some(on_progress))
    }

    /// Typed call for the [`crate::tools`] contract: serialise `args`, parse
    /// the JSON result. A tool-level error is [`Error::Tool`].
    pub fn call_typed<A: Serialize, R: DeserializeOwned>(&self, name: &str, args: &A) -> Result<R> {
        self.call_tool(name, &serde_json::to_value(args)?)?.parse()
    }

    /// A handle that cancels this client's in-flight `tools/call`s from any
    /// thread.
    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle {
            shared: self.shared.clone(),
        }
    }
}

/// Cancels in-flight tool calls: sends `notifications/cancelled` for each on a
/// fresh connection, then drops the call's socket. The blocked
/// `call_tool` returns [`Error::Cancelled`].
#[derive(Clone)]
pub struct CancelHandle {
    shared: Arc<Shared>,
}

impl fmt::Debug for CancelHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancelHandle").finish_non_exhaustive()
    }
}

impl CancelHandle {
    /// Cancel everything in flight; returns how many calls were cancelled.
    /// Never blocks longer than about two seconds. Best effort: failures to
    /// reach the host are ignored (the socket is dropped regardless).
    pub fn cancel(&self) -> usize {
        let targets: Vec<(u64, TcpStream, Arc<AtomicBool>)> = lock(&self.shared.inflight)
            .iter()
            .filter_map(|c| Some((c.id, c.stream.try_clone().ok()?, c.cancelled.clone())))
            .collect();
        let quick = Shared {
            timeouts: Timeouts {
                connect: Duration::from_secs(1),
                request: Duration::from_secs(1),
                ..self.shared.timeouts
            },
            url: self.shared.url.clone(),
            token: self.shared.token.clone(),
            protocol: OnceLock::new(),
            server: OnceLock::new(),
            next_id: AtomicU64::new(0),
            inflight: Mutex::new(Vec::new()),
        };
        if let Some(p) = self.shared.protocol.get() {
            let _ = quick.protocol.set(p.clone());
        }
        for (id, stream, flag) in &targets {
            flag.store(true, Ordering::SeqCst);
            let _ = quick.notify(
                "notifications/cancelled",
                json!({"requestId": id, "reason": "cancelled by Skink"}),
            );
            let _ = stream.shutdown(Shutdown::Both);
        }
        targets.len()
    }
}
