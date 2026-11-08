//! Integration tests against a fake in-process EveryAI-style MCP host: a
//! `std::net::TcpListener` thread that checks the Bearer token and the exact
//! `Host` header, answers JSON-RPC as plain JSON (Content-Length or chunked)
//! and `tools/call` as SSE with keep-alives and progress. Loopback only.

use serde_json::{Value, json};
use skink_host::tools::{self, HoverResult, PositionArgs};
use skink_host::{Error, HostEndpoint, McpClient, Timeouts};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const TOKEN: &str = "eai_host_test_token_0123456789";

#[derive(Clone, Copy, PartialEq)]
enum Style {
    Length,
    Chunked,
}

#[derive(Clone, Debug)]
struct Logged {
    headers: HashMap<String, String>,
    body: Value,
}

struct State {
    port: u16,
    style: Style,
    log: Mutex<Vec<Logged>>,
    hang_started: AtomicBool,
    stop: AtomicBool,
}

struct Fake {
    state: Arc<State>,
    join: Option<thread::JoinHandle<()>>,
}

impl Fake {
    fn start(style: Style) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(State {
            port,
            style,
            log: Mutex::new(Vec::new()),
            hang_started: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let st = state.clone();
        let join = thread::spawn(move || {
            for conn in listener.incoming() {
                if st.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let st = st.clone();
                thread::spawn(move || {
                    let _ = serve(&st, conn);
                });
            }
        });
        Fake {
            state,
            join: Some(join),
        }
    }

    fn endpoint(&self) -> HostEndpoint {
        HostEndpoint::new(format!("http://127.0.0.1:{}/mcp", self.state.port), TOKEN)
    }

    fn log(&self) -> Vec<Logged> {
        self.state.log.lock().unwrap().clone()
    }

    fn methods(&self) -> Vec<String> {
        self.log()
            .iter()
            .map(|l| l.body["method"].as_str().unwrap_or("").to_string())
            .collect()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.state.port));
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timed out waiting for {what}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn respond(
    conn: &mut TcpStream,
    status: &str,
    extra: &str,
    body: &str,
    style: Style,
) -> std::io::Result<()> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\n{extra}"
    );
    match style {
        Style::Length => {
            out.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        }
        Style::Chunked => {
            out.push_str("Transfer-Encoding: chunked\r\n\r\n");
            for piece in body.as_bytes().chunks(7) {
                out.push_str(&format!(
                    "{:x}\r\n{}\r\n",
                    piece.len(),
                    String::from_utf8_lossy(piece)
                ));
            }
            out.push_str("0\r\n\r\n");
        }
    }
    conn.write_all(out.as_bytes())?;
    conn.flush()
}

fn rpc_error(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}).to_string()
}

fn sse_event(conn: &mut TcpStream, v: &Value) -> std::io::Result<()> {
    conn.write_all(format!("event: message\ndata: {v}\n\n").as_bytes())?;
    conn.flush()
}

fn serve(st: &State, mut conn: TcpStream) -> std::io::Result<()> {
    let mut r = BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Ok(()); // wake-up connection from Drop
    }
    let mut headers = HashMap::new();
    loop {
        line.clear();
        r.read_line(&mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;

    // DNS-rebinding guard: exactly 127.0.0.1:port.
    if headers.get("host").map(String::as_str) != Some(format!("127.0.0.1:{}", st.port).as_str()) {
        return respond(
            &mut conn,
            "403 Forbidden",
            "",
            &rpc_error(&Value::Null, -32600, "Forbidden host"),
            st.style,
        );
    }
    if headers.get("authorization").map(String::as_str) != Some(format!("Bearer {TOKEN}").as_str())
    {
        return respond(
            &mut conn,
            "401 Unauthorized",
            "WWW-Authenticate: Bearer error=\"invalid_token\"\r\n",
            &rpc_error(&Value::Null, -32600, "Missing, expired or revoked token"),
            st.style,
        );
    }
    let msg: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    st.log.lock().unwrap().push(Logged {
        headers: headers.clone(),
        body: msg.clone(),
    });
    let id = msg["id"].clone();
    let method = msg["method"].as_str().unwrap_or("");
    if id.is_null() {
        return conn
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
    let ok = |result: Value| json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
    match method {
        "initialize" => {
            let asked = msg["params"]["protocolVersion"].clone();
            respond(
                &mut conn,
                "200 OK",
                "",
                &ok(
                    json!({"protocolVersion":asked,"capabilities":{"tools":{}},"serverInfo":{"name":"EveryAI","version":"1.0"}}),
                ),
                st.style,
            )
        }
        "tools/list" => {
            let page2 = msg["params"]["cursor"] == "p2";
            let result = if page2 {
                json!({"tools":[{"name":"lsp.hover","inputSchema":{"type":"object"}}]})
            } else {
                json!({"tools":[{"name":"echo","description":"echoes","inputSchema":{"type":"object"}}],"nextCursor":"p2"})
            };
            respond(&mut conn, "200 OK", "", &ok(result), st.style)
        }
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            if name == "unknown" {
                return respond(
                    &mut conn,
                    "200 OK",
                    "",
                    &rpc_error(&id, -32602, "Unknown tool: unknown"),
                    st.style,
                );
            }
            if name == "boom" {
                return respond(&mut conn, "500 Internal Server Error", "", "oops", st.style);
            }
            conn.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
            )?;
            conn.write_all(b":\n\n")?;
            let progress = msg["params"]["_meta"]["progressToken"].clone();
            let tick = |conn: &mut TcpStream, n: u32| -> std::io::Result<()> {
                if progress.is_null() {
                    return Ok(());
                }
                sse_event(
                    conn,
                    &json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":progress,"progress":n,"message":format!("step {n}")}}),
                )
            };
            let text = |t: &str| json!({"content":[{"type":"text","text":t}],"isError":false});
            let result = match name {
                "echo" => {
                    tick(&mut conn, 1)?;
                    conn.write_all(b":\n\n")?;
                    tick(&mut conn, 2)?;
                    // A progress event for somebody else's token must be ignored.
                    sse_event(
                        &mut conn,
                        &json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":"other","progress":99}}),
                    )?;
                    text(&msg["params"]["arguments"].to_string())
                }
                "image" => {
                    json!({"content":[{"type":"text","text":"a"},{"type":"image","data":"QUJD","mimeType":"image/png"},{"type":"text","text":"b"}],"isError":false})
                }
                "fail" => json!({"content":[{"type":"text","text":"nope"}],"isError":true}),
                "lsp.hover" => text(r#"{"contents":"fn main()","range":null}"#),
                "primed" => {
                    // MCP 2025-11-25 priming event (id + empty data), then a
                    // host -> client request whose id equals ours.
                    conn.write_all(b"id: 0\ndata:\n\n")?;
                    sse_event(&mut conn, &json!({"jsonrpc":"2.0","id":id,"method":"ping"}))?;
                    text("ok")
                }
                "hang" => {
                    st.hang_started.store(true, Ordering::SeqCst);
                    let start = Instant::now();
                    while start.elapsed() < Duration::from_secs(5) {
                        thread::sleep(Duration::from_millis(30));
                        if conn.write_all(b":\n\n").and_then(|_| conn.flush()).is_err() {
                            return Ok(()); // client left: call is over
                        }
                    }
                    return Ok(());
                }
                "silent" => {
                    thread::sleep(Duration::from_millis(1500));
                    text("late")
                }
                other => text(&format!("unhandled {other}")),
            };
            sse_event(&mut conn, &json!({"jsonrpc":"2.0","id":id,"result":result}))?;
            let _ = conn.shutdown(Shutdown::Both);
            Ok(())
        }
        other => respond(
            &mut conn,
            "200 OK",
            "",
            &rpc_error(&id, -32601, &format!("Method not found: {other}")),
            st.style,
        ),
    }
}

// ----------------------------------------------------------------- tests

#[test]
fn handshake_list_and_headers() {
    let fake = Fake::start(Style::Length);
    let c = McpClient::connect(&fake.endpoint()).unwrap();
    assert_eq!(c.protocol_version(), "2025-11-25");
    assert_eq!(c.server_name(), Some("EveryAI"));
    let tools = c.list_tools().unwrap();
    assert_eq!(
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["echo", "lsp.hover"]
    );
    assert_eq!(tools[0].description.as_deref(), Some("echoes"));

    assert_eq!(
        fake.methods(),
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/list"
        ]
    );
    let log = fake.log();
    assert_eq!(log[0].body["params"]["clientInfo"]["name"], "skink");
    // Version header only after the handshake.
    assert!(!log[0].headers.contains_key("mcp-protocol-version"));
    assert_eq!(log[2].headers["mcp-protocol-version"], "2025-11-25");
    assert_eq!(log[2].headers["content-type"], "application/json");
    assert!(log[2].headers["accept"].contains("text/event-stream"));
}

#[test]
fn chunked_json_replies() {
    let fake = Fake::start(Style::Chunked);
    let c = McpClient::connect(&fake.endpoint()).unwrap();
    assert_eq!(c.list_tools().unwrap().len(), 2);
}

#[test]
fn sse_call_with_keepalive_and_progress() {
    let fake = Fake::start(Style::Length);
    let c = McpClient::connect(&fake.endpoint()).unwrap();

    let mut seen = Vec::new();
    let r = c
        .call_tool_progress("echo", &json!({"a": 1}), &mut |p| {
            seen.push((p.progress, p.message.clone()));
        })
        .unwrap();
    assert!(!r.is_error);
    assert_eq!(r.text, r#"{"a":1}"#);
    assert_eq!(
        seen,
        [
            (1.0, Some("step 1".to_string())),
            (2.0, Some("step 2".to_string()))
        ]
    );

    // Without a callback no progress token is sent and keep-alives are just skipped.
    let r = c.call_tool("echo", &json!({})).unwrap();
    assert_eq!(r.text, "{}");
    let log = fake.log();
    assert!(log[log.len() - 1].body["params"].get("_meta").is_none());
}

#[test]
fn images_errors_and_rpc_errors() {
    let fake = Fake::start(Style::Length);
    let c = McpClient::connect(&fake.endpoint()).unwrap();

    let r = c.call_tool("image", &json!({})).unwrap();
    assert_eq!(r.text, "a\nb");
    assert_eq!(r.images.len(), 1);
    assert_eq!(r.images[0].mime_type, "image/png");
    assert_eq!(r.images[0].data, "QUJD");

    let r = c.call_tool("fail", &json!({})).unwrap();
    assert!(r.is_error);
    assert_eq!(r.text, "nope");
    assert!(matches!(r.parse::<Value>(), Err(Error::Tool(m)) if m == "nope"));

    match c.call_tool("unknown", &json!({})) {
        Err(Error::Rpc { code, message }) => {
            assert_eq!(code, -32602);
            assert!(message.contains("Unknown tool"));
        }
        other => panic!("{other:?}"),
    }
    match c.call_tool("boom", &json!({})) {
        Err(Error::Http {
            status: 500,
            message,
        }) => assert_eq!(message, "oops"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn typed_contract_call() {
    let fake = Fake::start(Style::Length);
    let c = McpClient::connect(&fake.endpoint()).unwrap();
    let args = PositionArgs {
        doc: tools::Doc {
            path: "/a.rs".into(),
            text: None,
            version: None,
        },
        line: 0,
        character: 3,
    };
    let h: HoverResult = c.call_typed(tools::LSP_HOVER, &args).unwrap();
    assert_eq!(h.contents, "fn main()");
    let log = fake.log();
    assert_eq!(
        log[log.len() - 1].body["params"]["arguments"]["character"],
        3
    );
}

#[test]
fn unauthorized_and_token_never_leaks() {
    let fake = Fake::start(Style::Length);
    let ep = HostEndpoint::new(fake.endpoint().url, "eai_host_wrong_secret");
    let err = McpClient::connect(&ep).unwrap_err();
    assert!(matches!(err, Error::Unauthorized), "{err:?}");
    let shown = format!("{err} {err:?} {ep:?}");
    assert!(!shown.contains("wrong_secret"));

    let c = McpClient::connect(&fake.endpoint()).unwrap();
    assert!(!format!("{c:?} {:?}", c.cancel_handle()).contains(TOKEN));
}

#[test]
fn rejects_bad_urls_and_tokens_without_connecting() {
    for url in [
        "http://example.com/mcp",
        "http://10.1.2.3:8080/mcp",
        "http://127.0.0.1.nip.io:80/mcp",
        "https://127.0.0.1:443/mcp",
        "http://user:pw@127.0.0.1:1/mcp",
    ] {
        let err = McpClient::connect(&HostEndpoint::new(url, TOKEN)).unwrap_err();
        assert!(
            matches!(err, Error::NotLoopback(_) | Error::InvalidUrl(_)),
            "{url}: {err:?}"
        );
    }
    let err = McpClient::connect(&HostEndpoint::new(
        "http://127.0.0.1:1/mcp",
        "bad\r\nX-Evil: 1",
    ))
    .unwrap_err();
    assert!(matches!(err, Error::InvalidToken));
}

#[test]
fn connection_refused_is_an_error_not_a_panic() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let start = Instant::now();
    let err = McpClient::connect(&HostEndpoint::new(
        format!("http://127.0.0.1:{port}/mcp"),
        TOKEN,
    ))
    .unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    // Windows retries a refused loopback connect until the connect timeout.
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn silent_host_times_out() {
    // Accepts, never answers.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let _t = thread::spawn(move || {
        let held: Vec<_> = l.incoming().take(1).flatten().collect();
        thread::sleep(Duration::from_secs(3));
        drop(held);
    });
    let t = Timeouts {
        request: Duration::from_millis(300),
        ..Timeouts::default()
    };
    let start = Instant::now();
    let err = McpClient::connect_with(
        &HostEndpoint::new(format!("http://127.0.0.1:{port}/mcp"), TOKEN),
        t,
    )
    .unwrap_err();
    assert!(matches!(err, Error::Timeout), "{err:?}");
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn call_idle_and_total_timeouts() {
    let fake = Fake::start(Style::Length);
    let c = McpClient::connect(&fake.endpoint())
        .unwrap()
        .with_timeouts(Timeouts {
            call_idle: Duration::from_millis(200),
            ..Timeouts::default()
        });
    let start = Instant::now();
    assert!(matches!(
        c.call_tool("silent", &json!({})),
        Err(Error::Timeout)
    ));
    assert!(start.elapsed() < Duration::from_millis(1200));

    // Keep-alives defeat the idle timeout but not the total cap.
    let c = c.with_timeouts(Timeouts {
        call_idle: Duration::from_secs(5),
        call_total: Some(Duration::from_millis(300)),
        ..Timeouts::default()
    });
    let start = Instant::now();
    assert!(matches!(
        c.call_tool("hang", &json!({})),
        Err(Error::Timeout)
    ));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn cancel_sends_notification_and_drops_the_call() {
    let fake = Fake::start(Style::Length);
    let c = Arc::new(McpClient::connect(&fake.endpoint()).unwrap());
    let handle = c.cancel_handle();
    assert_eq!(handle.cancel(), 0, "nothing in flight");

    let worker = {
        let c = c.clone();
        thread::spawn(move || c.call_tool("hang", &json!({})))
    };
    wait_until("hang to start", || {
        fake.state.hang_started.load(Ordering::SeqCst)
    });
    let start = Instant::now();
    assert_eq!(handle.cancel(), 1);
    let out = worker.join().unwrap();
    assert!(matches!(out, Err(Error::Cancelled)), "{out:?}");
    assert!(start.elapsed() < Duration::from_secs(2));

    wait_until("cancelled notification", || {
        fake.methods()
            .iter()
            .any(|m| m == "notifications/cancelled")
    });
    let log = fake.log();
    let call_id = log
        .iter()
        .find(|l| l.body["params"]["name"] == "hang")
        .unwrap()
        .body["id"]
        .clone();
    let cancel = log
        .iter()
        .find(|l| l.body["method"] == "notifications/cancelled")
        .unwrap();
    assert_eq!(cancel.body["params"]["requestId"], call_id);
    assert_eq!(cancel.headers["authorization"], format!("Bearer {TOKEN}"));

    // The client is still usable afterwards.
    assert_eq!(c.call_tool("echo", &json!(1)).unwrap().text, "1");
}

#[test]
fn priming_events_and_host_requests_on_the_stream_are_skipped() {
    // Regression: an empty `data:` event failed the call with a JSON error,
    // and a host request reusing our id was taken for the reply.
    let fake = Fake::start(Style::Length);
    let c = McpClient::connect(&fake.endpoint()).unwrap();
    assert_eq!(c.call_tool("primed", &json!({})).unwrap().text, "ok");
}

#[test]
fn request_ids_are_unique_across_forks() {
    // Regression: each fork counted from 1, so a cancel for one fork's call
    // could name another fork's request.
    let fake = Fake::start(Style::Length);
    let c = McpClient::connect(&fake.endpoint()).unwrap();
    let f = c.fork();
    for _ in 0..3 {
        c.call_tool("echo", &json!({})).unwrap();
        f.call_tool("echo", &json!({})).unwrap();
    }
    let mut ids: Vec<u64> = fake
        .log()
        .iter()
        .filter_map(|l| l.body["id"].as_u64())
        .collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "duplicate JSON-RPC ids");
}
