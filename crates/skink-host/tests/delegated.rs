//! End-to-end tests of the takeover side (`DelegatedLsp`, `DelegatedAgents`,
//! `providers_with`) against a scripted fake EveryAI host: a loopback
//! `TcpListener` that checks the Bearer token and `Host` header, answers
//! `tools/list` as JSON and every `tools/call` as SSE (keep-alives, progress
//! notifications, final result) according to a handler closure.

use serde_json::{Value, json};
use skink_ai::agent::{
    AgentEvent, AgentId, AgentSpec, PromptBlock, SessionId, StopReason, ToolKind,
};
use skink_ai::{AgentController, AgentError, AgentState};
use skink_core::Backend;
use skink_host::tools::{self, PromptMode, PromptRunArgs};
use skink_host::{
    DelegatedAgents, DelegatedLsp, HostEndpoint, HostLink, LinkPolicy, McpClient, ProviderOptions,
    providers_with,
};
use skink_lsp::{
    FormattingOptions, LspError, LspEvent, LspProvider, Position, Range, ServerStatus, Severity,
    TextEdit,
};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const TOKEN: &str = "eai_host_test_token_0123456789";

// ------------------------------------------------------------ fake host

enum Reply {
    /// Success: the value is the JSON document in the text block.
    Ok(Value),
    /// `isError: true` with this message.
    ToolErr(&'static str),
    /// JSON-RPC error object.
    Rpc(i64, &'static str),
    /// Progress events (each a JSON `message`), then the result.
    Stream(Vec<Value>, Value),
    /// Progress events, then keep-alives until the client leaves.
    Hang(Vec<Value>),
    /// Progress events, then the connection just closes (host crash).
    Close(Vec<Value>),
}

type Handler = dyn Fn(&str, &Value) -> Reply + Send + Sync;

struct State {
    port: u16,
    handler: Arc<Handler>,
    log: Mutex<Vec<Value>>,
    hanging: AtomicUsize,
    stop: AtomicBool,
}

struct Fake {
    state: Arc<State>,
    join: Option<thread::JoinHandle<()>>,
}

impl Fake {
    fn start(handler: impl Fn(&str, &Value) -> Reply + Send + Sync + 'static) -> Fake {
        Self::start_on(0, Arc::new(handler))
    }

    fn start_on(port: u16, handler: Arc<Handler>) -> Fake {
        let listener = {
            let mut tries = 0;
            loop {
                match TcpListener::bind(("127.0.0.1", port)) {
                    Ok(l) => break l,
                    Err(e) if tries < 50 => {
                        tries += 1;
                        let _ = e;
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(e) => panic!("bind: {e}"),
                }
            }
        };
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(State {
            port,
            handler,
            log: Mutex::new(Vec::new()),
            hanging: AtomicUsize::new(0),
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

    fn client(&self) -> McpClient {
        McpClient::connect(&self.endpoint()).unwrap()
    }

    fn link(&self) -> Arc<HostLink> {
        Arc::new(HostLink::new(self.client()))
    }

    fn link_with(&self, policy: LinkPolicy) -> Arc<HostLink> {
        Arc::new(HostLink::with_policy(self.client(), policy))
    }

    /// Arguments of every `tools/call` of `tool`, in order.
    fn calls(&self, tool: &str) -> Vec<Value> {
        self.state
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m["method"] == "tools/call" && m["params"]["name"] == tool)
            .map(|m| m["params"]["arguments"].clone())
            .collect()
    }

    fn methods(&self) -> Vec<String> {
        self.state
            .log
            .lock()
            .unwrap()
            .iter()
            .map(|m| m["method"].as_str().unwrap_or("").to_string())
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

fn reply_json(conn: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    conn.write_all(out.as_bytes())?;
    conn.flush()
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
        return Ok(());
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
    if headers.get("host").map(String::as_str) != Some(format!("127.0.0.1:{}", st.port).as_str())
        || headers.get("authorization").map(String::as_str)
            != Some(format!("Bearer {TOKEN}").as_str())
    {
        return reply_json(&mut conn, "401 Unauthorized", "{}");
    }
    let msg: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    st.log.lock().unwrap().push(msg.clone());
    let id = msg["id"].clone();
    if id.is_null() {
        return conn
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
    let ok = |result: Value| json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
    match msg["method"].as_str().unwrap_or("") {
        "initialize" => reply_json(
            &mut conn,
            "200 OK",
            &ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "EveryAI", "version": "1.0"}
            })),
        ),
        "tools/list" => {
            let tools: Vec<Value> = tools::ALL
                .iter()
                .map(|n| json!({"name": n, "inputSchema": {"type": "object"}}))
                .collect();
            reply_json(&mut conn, "200 OK", &ok(json!({ "tools": tools })))
        }
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("").to_string();
            let args = msg["params"]["arguments"].clone();
            let token = msg["params"]["_meta"]["progressToken"].clone();
            let reply = (st.handler)(&name, &args);
            conn.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n:\n\n",
            )?;
            let progress = |conn: &mut TcpStream, evs: &[Value]| -> std::io::Result<()> {
                for (n, e) in evs.iter().enumerate() {
                    sse_event(
                        conn,
                        &json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":token,"progress":n + 1,"message":e.to_string()}}),
                    )?;
                    thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            };
            let result = |v: &Value| {
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":v.to_string()}],"isError":false}})
            };
            match reply {
                Reply::Ok(v) => sse_event(&mut conn, &result(&v))?,
                Reply::ToolErr(m) => sse_event(
                    &mut conn,
                    &json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":m}],"isError":true}}),
                )?,
                Reply::Rpc(code, m) => sse_event(
                    &mut conn,
                    &json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":m}}),
                )?,
                Reply::Stream(evs, v) => {
                    progress(&mut conn, &evs)?;
                    sse_event(&mut conn, &result(&v))?;
                }
                Reply::Hang(evs) => {
                    progress(&mut conn, &evs)?;
                    st.hanging.fetch_add(1, Ordering::SeqCst);
                    let start = Instant::now();
                    while start.elapsed() < Duration::from_secs(8) {
                        thread::sleep(Duration::from_millis(20));
                        if conn.write_all(b":\n\n").and_then(|_| conn.flush()).is_err() {
                            break;
                        }
                    }
                    return Ok(());
                }
                Reply::Close(evs) => progress(&mut conn, &evs)?,
            }
            let _ = conn.shutdown(Shutdown::Both);
            Ok(())
        }
        other => reply_json(
            &mut conn,
            "200 OK",
            &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Method not found: {other}")}}).to_string(),
        ),
    }
}

// ------------------------------------------------------------- helpers

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skink-host-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tpos(line: u32, character: u32) -> Value {
    json!({"line": line, "character": character})
}

fn trange(l1: u32, c1: u32, l2: u32, c2: u32) -> Value {
    json!({"start": tpos(l1, c1), "end": tpos(l2, c2)})
}

fn pos(line: usize, col: usize) -> Position {
    Position::new(line, col)
}

fn rng(l1: usize, c1: usize, l2: usize, c2: usize) -> Range {
    Range::new(pos(l1, c1), pos(l2, c2))
}

fn lsp_over(fake: &Fake) -> DelegatedLsp {
    DelegatedLsp::new(fake.link()).with_auto_diagnostics(false)
}

/// A canned host for the LSP tools.
fn lsp_host(doc_path: String, other_path: String) -> Fake {
    Fake::start(move |name, _args| match name {
        tools::LSP_HOVER => Reply::Ok(json!({"contents": "fn a()", "range": trange(0, 3, 0, 5)})),
        tools::LSP_DEFINITION => Reply::Ok(json!([
            {"path": doc_path, "range": trange(0, 3, 0, 4)},
            {"path": other_path, "range": trange(0, 2, 0, 3)},
        ])),
        tools::LSP_REFERENCES => Reply::Ok(json!([
            {"path": doc_path, "range": trange(1, 0, 1, 3)},
        ])),
        tools::LSP_COMPLETION => Reply::Ok(json!({
            "is_incomplete": false,
            "items": [
                {"label": "plain"},
                {"label": "ins", "insert_text": "ins()", "kind": 3, "detail": "fn()", "sort_text": "01"},
                {"label": "edit", "insert_text": "ignored", "text_edit": {"range": trange(0, 3, 0, 4), "new_text": "edited"}},
            ]
        })),
        tools::LSP_RENAME => Reply::Ok(json!({"files": [
            {"path": doc_path, "edits": [{"range": trange(0, 3, 0, 4), "new_text": "n"}]},
            {"path": other_path, "edits": [{"range": trange(0, 2, 0, 3), "new_text": "n"}]},
        ]})),
        tools::LSP_FORMAT => Reply::Ok(json!([
            {"range": trange(0, 5, 0, 6), "new_text": ""}
        ])),
        tools::LSP_DIAGNOSTICS => Reply::Ok(json!([
            {"range": trange(0, 3, 0, 5), "severity": "warning", "message": "unused", "code": "W1", "source": "rustc"},
            {"range": trange(1, 0, 1, 1), "severity": "hint", "message": "h"},
        ])),
        _ => Reply::ToolErr("unexpected"),
    })
}

// ------------------------------------------------------------------ LSP

#[test]
fn lsp_every_method_end_to_end() {
    let dir = tmp("lsp-methods");
    let doc = dir.join("a.rs");
    let other = dir.join("other.rs");
    // The second file is not open: its columns are converted from disk.
    std::fs::write(&other, "😀x\n").unwrap();
    let fake = lsp_host(
        doc.to_string_lossy().into_owned(),
        other.to_string_lossy().into_owned(),
    );
    let lsp = lsp_over(&fake);
    assert_eq!(lsp.backend(), Backend::Delegated);
    assert_eq!(lsp.status(&doc), ServerStatus::Ready("EveryAI".into()));

    // "b" is char 2 but UTF-16 unit 3 in this line.
    lsp.did_open(&doc, "a😀b = 1\nlet é = 2\n").unwrap();

    let h = lsp.hover(&doc, pos(0, 2)).unwrap().unwrap();
    assert_eq!(h.text, "fn a()");
    assert_eq!(h.range, Some(rng(0, 2, 0, 4)));
    let sent = &fake.calls(tools::LSP_HOVER)[0];
    assert_eq!(sent["path"], doc.to_string_lossy().as_ref());
    assert_eq!(sent["line"], 0);
    assert_eq!(sent["character"], 3, "char column sent as UTF-16");
    assert_eq!(
        sent["text"], "a😀b = 1\nlet é = 2\n",
        "unsaved text rides along"
    );
    assert_eq!(sent["version"], 1);

    let defs = lsp.definition(&doc, pos(0, 2)).unwrap();
    assert_eq!(defs.len(), 2);
    assert_eq!(defs[0].path, doc);
    assert_eq!(defs[0].range, rng(0, 2, 0, 3));
    assert_eq!(defs[1].path, other);
    assert_eq!(
        defs[1].range,
        rng(0, 1, 0, 2),
        "disk file: UTF-16 2 is char 1"
    );

    let refs = lsp.references(&doc, pos(0, 2), true).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].range, rng(1, 0, 1, 3));
    assert_eq!(
        fake.calls(tools::LSP_REFERENCES)[0]["include_declaration"],
        true
    );

    let items = lsp.completion(&doc, pos(0, 2)).unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].insert_text, "plain");
    assert_eq!(items[0].kind, skink_lsp::ItemKind::Other);
    assert_eq!(items[1].insert_text, "ins()");
    assert_eq!(items[1].kind, skink_lsp::ItemKind::Function);
    assert_eq!(items[1].detail.as_deref(), Some("fn()"));
    assert_eq!(items[1].sort_text.as_deref(), Some("01"));
    assert_eq!(items[2].insert_text, "edited", "text_edit wins");
    assert_eq!(
        items[2].edit,
        Some(TextEdit {
            range: rng(0, 2, 0, 3),
            new_text: "edited".into()
        })
    );

    let edit = lsp.rename(&doc, pos(0, 2), "n").unwrap().unwrap();
    assert_eq!(fake.calls(tools::LSP_RENAME)[0]["new_name"], "n");
    assert_eq!(edit.files.len(), 2);
    assert_eq!(edit.files[0].edits[0].range, rng(0, 2, 0, 3));
    assert_eq!(edit.files[1].path, other);
    assert_eq!(edit.files[1].edits[0].range, rng(0, 1, 0, 2));
    assert!(edit.ops.is_empty());

    let fmt = lsp
        .formatting(
            &doc,
            FormattingOptions {
                tab_size: 2,
                insert_spaces: false,
            },
        )
        .unwrap();
    assert_eq!(fmt.len(), 1);
    assert_eq!(fmt[0].range, rng(0, 4, 0, 5));
    let sent = &fake.calls(tools::LSP_FORMAT)[0];
    assert_eq!(
        (sent["tab_size"].clone(), sent["insert_spaces"].clone()),
        (json!(2), json!(false))
    );

    // Not in the host contract.
    assert!(matches!(
        lsp.signature_help(&doc, pos(0, 0)),
        Err(LspError::Unsupported(_))
    ));
    assert!(matches!(
        lsp.document_symbols(&doc),
        Err(LspError::Unsupported(_))
    ));
}

#[test]
fn lsp_document_sync_is_carried_on_each_query() {
    let dir = tmp("lsp-sync");
    let doc = dir.join("s.rs");
    let fake = Fake::start(|_, _| Reply::Ok(Value::Null));
    let lsp = lsp_over(&fake);
    let last = |n: usize| fake.calls(tools::LSP_HOVER)[n].clone();

    lsp.did_open(&doc, "abc\n").unwrap();
    assert_eq!(lsp.hover(&doc, pos(0, 1)).unwrap(), None);
    assert_eq!(last(0)["text"], "abc\n");

    lsp.did_save(&doc).unwrap();
    lsp.hover(&doc, pos(0, 1)).unwrap();
    assert!(last(1).get("text").is_none(), "saved: host reads disk");
    assert_eq!(last(1)["version"], 1);

    lsp.did_change(&doc, "abcd\n").unwrap();
    lsp.hover(&doc, pos(0, 1)).unwrap();
    assert_eq!(last(2)["text"], "abcd\n");
    assert_eq!(last(2)["version"], 2);

    lsp.did_change_incremental(
        &doc,
        &[TextEdit {
            range: rng(0, 0, 0, 1),
            new_text: "XY".into(),
        }],
    )
    .unwrap();
    lsp.hover(&doc, pos(0, 0)).unwrap();
    assert_eq!(last(3)["text"], "XYbcd\n");
    assert_eq!(last(3)["version"], 3);

    // A path that was never opened is just sent as a path.
    let other = dir.join("never.rs");
    lsp.hover(&other, pos(0, 0)).unwrap();
    let sent = last(4);
    assert!(sent.get("text").is_none() && sent.get("version").is_none());

    lsp.did_close(&doc).unwrap();
    lsp.hover(&doc, pos(0, 0)).unwrap();
    assert!(last(5).get("text").is_none());
    assert!(matches!(
        lsp.did_change_incremental(&doc, &[]),
        Err(LspError::NotOpen(_))
    ));
}

#[test]
fn lsp_relative_paths_are_sent_absolute() {
    let fake = Fake::start(|_, _| Reply::Ok(Value::Null));
    let lsp = lsp_over(&fake);
    lsp.hover(Path::new("rel/x.rs"), pos(0, 0)).unwrap();
    let sent = fake.calls(tools::LSP_HOVER)[0]["path"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(Path::new(&sent).is_absolute(), "{sent}");
}

#[test]
fn lsp_diagnostics_cache_events_and_waker() {
    let dir = tmp("lsp-diag");
    let doc = dir.join("d.rs");
    let fake = lsp_host(doc.to_string_lossy().into_owned(), String::new());
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let lsp = lsp_over(&fake).with_waker(Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }));
    lsp.did_open(&doc, "a😀b = 1\nxy\n").unwrap();
    assert!(
        lsp.diagnostics(&doc).is_empty(),
        "nothing cached before a refresh"
    );

    let d = lsp.refresh_diagnostics(&doc).unwrap();
    assert_eq!(d.len(), 2);
    assert_eq!(d[0].range, rng(0, 2, 0, 4));
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(d[0].message, "unused");
    assert_eq!(d[0].code.as_deref(), Some("W1"));
    assert_eq!(d[0].source.as_deref(), Some("rustc"));
    assert_eq!(d[1].severity, Severity::Hint);
    assert_eq!(lsp.diagnostics(&doc), d);
    assert_eq!(
        fake.calls(tools::LSP_DIAGNOSTICS)[0]["text"],
        "a😀b = 1\nxy\n"
    );

    let ev = lsp.poll_events();
    assert_eq!(ev.len(), 1);
    match &ev[0] {
        LspEvent::Diagnostics {
            path, diagnostics, ..
        } => {
            assert_eq!(path, &doc);
            assert_eq!(diagnostics, &d);
        }
        other => panic!("{other:?}"),
    }
    assert!(lsp.poll_events().is_empty());
    assert_eq!(wakes.load(Ordering::SeqCst), 1);

    // Unchanged answer: no new event.
    lsp.refresh_diagnostics(&doc).unwrap();
    assert!(lsp.poll_events().is_empty());
    assert_eq!(wakes.load(Ordering::SeqCst), 1);

    lsp.did_close(&doc).unwrap();
    assert!(lsp.diagnostics(&doc).is_empty());
}

#[test]
fn lsp_auto_and_coalesced_background_refresh() {
    let dir = tmp("lsp-bg");
    let doc = dir.join("b.rs");
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let fake = Fake::start(move |name, _| {
        assert_eq!(name, tools::LSP_DIAGNOSTICS);
        let n = c.fetch_add(1, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(120));
        Reply::Ok(
            json!([{"range": trange(0, 0, 0, 1), "severity": "error", "message": format!("e{n}")}]),
        )
    });
    // Auto: did_open triggers a refresh in the background.
    let lsp = DelegatedLsp::new(fake.link());
    lsp.did_open(&doc, "x\n").unwrap();
    wait_until("auto refresh after open", || {
        !lsp.diagnostics(&doc).is_empty()
    });
    assert_eq!(lsp.diagnostics(&doc)[0].message, "e0");

    // Many requests while one is running coalesce into at most one rerun.
    let before = count.load(Ordering::SeqCst);
    for _ in 0..10 {
        lsp.refresh_diagnostics_async(&doc);
    }
    wait_until("coalesced refresh", || {
        lsp.diagnostics(&doc)[0].message != "e0" && count.load(Ordering::SeqCst) > before
    });
    thread::sleep(Duration::from_millis(400));
    let ran = count.load(Ordering::SeqCst) - before;
    assert!(
        (1..=2).contains(&ran),
        "ran {ran} refreshes for 10 requests"
    );

    // did_save also refreshes.
    let before = count.load(Ordering::SeqCst);
    lsp.did_save(&doc).unwrap();
    wait_until("refresh after save", || {
        count.load(Ordering::SeqCst) > before
    });
}

#[test]
fn lsp_answer_for_an_edited_buffer_is_discarded() {
    let dir = tmp("lsp-stale");
    let doc = dir.join("st.rs");
    let slot: Arc<OnceLock<Arc<DelegatedLsp>>> = Arc::new(OnceLock::new());
    let s = slot.clone();
    let p = doc.clone();
    let fake = Fake::start(move |_, _| {
        // The user types while the host is still answering.
        s.get().unwrap().did_change(&p, "changed\n").unwrap();
        Reply::Ok(json!([{"range": trange(0, 0, 0, 1), "severity": "error", "message": "old"}]))
    });
    let lsp = Arc::new(lsp_over(&fake));
    slot.set(lsp.clone()).ok();
    lsp.did_open(&doc, "orig\n").unwrap();
    let err = lsp.refresh_diagnostics(&doc).unwrap_err();
    assert!(err.is_transient(), "{err:?}");
    assert!(lsp.diagnostics(&doc).is_empty());
    assert!(lsp.poll_events().is_empty());
}

#[test]
fn lsp_host_errors_map_to_lsp_errors() {
    let dir = tmp("lsp-errors");
    let doc = dir.join("e.rs");
    let fake = Fake::start(|name, _| match name {
        tools::LSP_HOVER => Reply::ToolErr("no language server for rust"),
        tools::LSP_RENAME => Reply::Rpc(-32602, "Unknown tool: lsp.rename"),
        tools::LSP_DEFINITION => Reply::Rpc(-32000, "boom"),
        tools::LSP_FORMAT => Reply::Ok(json!("not a list")),
        _ => Reply::Ok(json!([])),
    });
    let lsp = lsp_over(&fake);
    assert!(matches!(
        lsp.hover(&doc, pos(0, 0)),
        Err(LspError::NoServer(m)) if m.contains("rust")
    ));
    assert!(matches!(
        lsp.definition(&doc, pos(0, 0)),
        Err(LspError::Server { code: -32000, .. })
    ));
    assert!(matches!(
        lsp.formatting(&doc, FormattingOptions::default()),
        Err(LspError::Protocol(_))
    ));
    // The host does not know the tool: Unsupported, and never asked again.
    assert!(matches!(
        lsp.rename(&doc, pos(0, 0), "x"),
        Err(LspError::Unsupported(_))
    ));
    assert!(matches!(
        lsp.rename(&doc, pos(0, 0), "x"),
        Err(LspError::Unsupported(_))
    ));
    assert_eq!(fake.calls(tools::LSP_RENAME).len(), 1);
    // Other tools are unaffected, and none of this tripped the link.
    assert!(lsp.references(&doc, pos(0, 0), false).unwrap().is_empty());
    assert!(lsp.link().is_available());
}

#[test]
fn lsp_host_going_away_degrades_to_unsupported_and_recovers() {
    let dir = tmp("lsp-gone");
    let doc = dir.join("g.rs");
    let handler: Arc<Handler> = Arc::new(|name, _| match name {
        tools::LSP_DIAGNOSTICS => {
            Reply::Ok(json!([{"range": trange(0, 0, 0, 1), "severity": "info", "message": "m"}]))
        }
        _ => Reply::Ok(Value::Null),
    });
    let fake = Fake::start_on(0, handler.clone());
    let port = fake.state.port;
    let lsp = DelegatedLsp::new(fake.link_with(LinkPolicy {
        max_failures: 2,
        retry_after: Duration::from_millis(300),
    }))
    .with_auto_diagnostics(false);
    lsp.did_open(&doc, "x\n").unwrap();
    lsp.refresh_diagnostics(&doc).unwrap();
    assert_eq!(lsp.hover(&doc, pos(0, 0)).unwrap(), None);

    drop(fake); // the host disappears

    let start = Instant::now();
    assert!(matches!(
        lsp.hover(&doc, pos(0, 0)),
        Err(LspError::Disconnected)
    ));
    assert!(matches!(
        lsp.hover(&doc, pos(0, 0)),
        Err(LspError::Disconnected)
    ));
    // Tripped: no more network, just Unsupported, instantly.
    assert!(matches!(
        lsp.hover(&doc, pos(0, 0)),
        Err(LspError::Unsupported(_))
    ));
    assert!(matches!(
        lsp.definition(&doc, pos(0, 0)),
        Err(LspError::Unsupported(_))
    ));
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(matches!(lsp.status(&doc), ServerStatus::Failed(_)));
    assert_eq!(lsp.backend(), Backend::Delegated);
    // Notifications and cached diagnostics keep working without a host.
    lsp.did_change(&doc, "y\n").unwrap();
    lsp.did_save(&doc).unwrap();
    assert_eq!(lsp.diagnostics(&doc).len(), 1);
    lsp.refresh_diagnostics_async(&doc);

    // The host comes back (same port): the first call after the cool-down
    // is the probe, and service resumes.
    #[cfg(unix)]
    {
        let _back = Fake::start_on(port, handler);
        thread::sleep(Duration::from_millis(350));
        assert_eq!(lsp.hover(&doc, pos(0, 0)).unwrap(), None);
        assert_eq!(lsp.status(&doc), ServerStatus::Ready("EveryAI".into()));
    }
    #[cfg(not(unix))]
    let _ = port;
}

// --------------------------------------------------------------- agents

fn collect(
    agents: &mut dyn AgentController,
    until: impl Fn(&[(AgentId, AgentEvent)]) -> bool,
) -> Vec<(AgentId, AgentEvent)> {
    let start = Instant::now();
    let mut all = Vec::new();
    while !until(&all) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timed out; events so far: {all:?}"
        );
        all.extend(agents.poll_events());
        thread::sleep(Duration::from_millis(5));
    }
    all
}

fn has_turn_end(evs: &[(AgentId, AgentEvent)]) -> bool {
    evs.iter().any(|(_, e)| {
        matches!(
            e,
            AgentEvent::TurnFinished { .. } | AgentEvent::TurnFailed { .. }
        )
    })
}

fn edit_json(path: &str) -> Value {
    json!({"path": path, "edits": [{"range": trange(2, 0, 2, 4), "new_text": "x"}]})
}

fn agents_host(hang_first_prompt: bool) -> Fake {
    let prompts = AtomicUsize::new(0);
    Fake::start(move |name, args| match name {
        tools::AGENTS_LIST => Reply::Ok(json!({"agents": [
            {"id": "claude-code", "name": "Claude Code", "description": "Anthropic", "installed": true, "running": false},
            {"id": "goose", "name": "Goose", "installed": false},
        ]})),
        tools::AGENTS_START => Reply::Ok(json!({"session_id": "sess-1"})),
        tools::MODELS_LIST => Reply::Ok(json!({"models": [
            {"id": "m1", "name": "Model One", "provider": "p", "default": true},
            {"id": "m2", "name": "Model Two"},
        ]})),
        tools::PROMPT_CANCEL => Reply::Ok(json!({"cancelled": true})),
        tools::PROMPT_RUN => {
            let n = prompts.fetch_add(1, Ordering::SeqCst);
            if hang_first_prompt && n == 0 {
                return Reply::Hang(vec![json!({"type": "status", "text": "thinking"})]);
            }
            let task = args["task_id"].clone();
            match args["mode"].as_str() {
                Some("edit") => Reply::Stream(
                    vec![json!({"type": "edit", "file": edit_json("/w/a.rs")})],
                    json!({"task_id": task, "files": [edit_json("/w/a.rs"), edit_json("/w/b.rs")]}),
                ),
                Some("search") => Reply::Stream(
                    vec![
                        json!({"type": "location", "location": {"path": "/w/a.rs", "range": trange(4, 0, 4, 3)}}),
                    ],
                    json!({"task_id": task, "locations": [
                        {"path": "/w/a.rs", "range": trange(4, 0, 4, 3)},
                        {"path": "/w/c.rs", "range": trange(0, 0, 0, 1)},
                    ]}),
                ),
                _ => Reply::Stream(
                    vec![
                        json!({"type": "status", "text": "reading"}),
                        json!({"type": "text_delta", "text": "Hel"}),
                        json!({"type": "text_delta", "text": "lo"}),
                        json!({"type": "garbage"}),
                    ],
                    json!({"task_id": task, "text": "Hello"}),
                ),
            }
        }
        _ => Reply::ToolErr("unexpected"),
    })
}

fn ready_agent(agents: &mut DelegatedAgents, label: &str) -> AgentId {
    let id = agents.start_agent(label, AgentSpec::new("npx")).unwrap();
    let evs = collect(agents, |e| !e.is_empty());
    assert!(matches!(evs[0].1, AgentEvent::Ready { .. }), "{evs:?}");
    id
}

fn session_of(agents: &mut DelegatedAgents, agent: AgentId) -> SessionId {
    agents.new_session(agent, Path::new("/w")).unwrap();
    let evs = collect(agents, |e| !e.is_empty());
    match &evs[0].1 {
        AgentEvent::SessionStarted { session, .. } => session.id.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn agents_start_resolution_and_session() {
    let fake = agents_host(false);
    let mut agents = DelegatedAgents::new(fake.link());
    assert_eq!(agents.backend(), Backend::Delegated);

    // Resolved by slug of the label; info comes from the host.
    let a = agents
        .start_agent("Claude Code", AgentSpec::new("npx").cwd("/w"))
        .unwrap();
    assert_eq!(agents.agents()[0].state, AgentState::Starting);
    let evs = collect(&mut agents, |e| !e.is_empty());
    match &evs[0] {
        (id, AgentEvent::Ready { info }) => {
            assert_eq!(*id, a);
            assert_eq!(info.name, "Claude Code");
            assert_eq!(info.title.as_deref(), Some("Anthropic"));
        }
        other => panic!("{other:?}"),
    }
    let st = agents.agents();
    assert_eq!(
        (st[0].state, st[0].label.as_str()),
        (AgentState::Ready, "Claude Code")
    );

    // Not installed / unknown to the host.
    let g = agents
        .start_agent("Goose", AgentSpec::new("goose"))
        .unwrap();
    let n = agents.start_agent("Nope", AgentSpec::new("nope")).unwrap();
    let evs = collect(&mut agents, |e| e.len() == 2);
    for (id, ev) in &evs {
        match ev {
            AgentEvent::StartFailed { message } if *id == g => {
                assert!(message.contains("not installed"), "{message}");
            }
            AgentEvent::StartFailed { message } if *id == n => {
                assert!(message.contains("Nope"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(
        agents
            .agents()
            .iter()
            .filter(|s| s.id != a)
            .all(|s| s.state == AgentState::Failed)
    );

    // Sessions: agents.start with the resolved id and cwd.
    agents.set_model(Some("m2".into()));
    let req = agents.new_session(a, Path::new("/w")).unwrap();
    let evs = collect(&mut agents, |e| !e.is_empty());
    match &evs[0].1 {
        AgentEvent::SessionStarted { request, session } => {
            assert_eq!(*request, req);
            assert_eq!(session.id, SessionId("sess-1".into()));
        }
        other => panic!("{other:?}"),
    }
    let start = &fake.calls(tools::AGENTS_START)[0];
    assert_eq!(start["agent_id"], "claude-code");
    assert_eq!(start["cwd"], "/w");
    assert_eq!(start["model"], "m2");
    assert!(start.get("prompt").is_none());

    agents
        .new_session_with_prompt(a, Path::new("/w"), "hi there")
        .unwrap();
    collect(&mut agents, |e| !e.is_empty());
    assert_eq!(fake.calls(tools::AGENTS_START)[1]["prompt"], "hi there");

    // Not part of the contract.
    assert!(matches!(
        agents.authenticate(a, "m"),
        Err(AgentError::Unsupported(_))
    ));
    assert!(matches!(
        agents.load_session(a, &SessionId("x".into()), Path::new("/w")),
        Err(AgentError::Unsupported(_))
    ));
    assert!(matches!(
        agents.set_mode(a, &SessionId("x".into()), "m"),
        Err(AgentError::Unsupported(_))
    ));
    assert!(matches!(
        agents.new_session(AgentId(99), Path::new("/w")),
        Err(AgentError::UnknownAgent(_))
    ));

    // Catalogues.
    let list = agents.host_agents().unwrap();
    assert_eq!(list.len(), 2);
    let models = agents.models().unwrap();
    assert_eq!(models.len(), 2);
    assert!(models[0].default);

    agents.stop_agent(a);
    assert!(agents.agents().iter().all(|s| s.id != a));
    assert!(matches!(
        agents.new_session(a, Path::new("/w")),
        Err(AgentError::UnknownAgent(_))
    ));
}

#[test]
fn agents_prompt_stream_maps_to_events() {
    let fake = agents_host(false);
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let mut agents = DelegatedAgents::new(fake.link()).with_waker(Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }));
    agents.set_cwd(Some(PathBuf::from("/w")));
    let a = ready_agent(&mut agents, "claude-code");
    let s = session_of(&mut agents, a);

    let turn = agents
        .send_prompt(
            a,
            &s,
            vec![
                PromptBlock::text("say hello"),
                PromptBlock::Resource {
                    uri: "file:///w/x.rs".into(),
                    text: "fn x() {}".into(),
                    mime_type: None,
                },
            ],
        )
        .unwrap();
    let evs = collect(&mut agents, has_turn_end);

    let kinds: Vec<&str> = evs
        .iter()
        .map(|(_, e)| match e {
            AgentEvent::ThoughtChunk { .. } => "thought",
            AgentEvent::MessageChunk { .. } => "message",
            AgentEvent::TurnFinished { .. } => "finished",
            other => panic!("{other:?}"),
        })
        .collect();
    // The final `text` repeats the streamed deltas and must not be shown twice;
    // the unparseable progress message is ignored.
    assert_eq!(kinds, ["thought", "message", "message", "finished"]);
    match &evs[0].1 {
        AgentEvent::ThoughtChunk { session, text } => {
            assert_eq!(session, &s);
            assert_eq!(text, "reading\n");
        }
        other => panic!("{other:?}"),
    }
    match &evs[3].1 {
        AgentEvent::TurnFinished {
            turn: t,
            stop,
            session,
        } => {
            assert_eq!((*t, session), (turn, &s));
            assert_eq!(*stop, StopReason::EndTurn);
        }
        other => panic!("{other:?}"),
    }
    let sent = &fake.calls(tools::PROMPT_RUN)[0];
    assert_eq!(sent["mode"], "ask");
    assert_eq!(sent["instruction"], "say hello");
    assert_eq!(sent["cwd"], "/w");
    assert_eq!(sent["context"][0]["kind"], "file");
    assert_eq!(sent["context"][0]["text"], "fn x() {}");
    assert!(sent["task_id"].as_str().unwrap().starts_with("skink-"));
    let r = agents.take_result(turn).unwrap();
    assert_eq!(r.text.as_deref(), Some("Hello"));
    assert!(agents.take_result(turn).is_none());
    assert!(wakes.load(Ordering::SeqCst) >= 4);

    // The turn is over, so the next prompt is allowed straight away.
    agents
        .send_prompt(a, &s, vec![PromptBlock::text("again")])
        .unwrap();
    collect(&mut agents, has_turn_end);
    let task_ids: Vec<_> = fake
        .calls(tools::PROMPT_RUN)
        .iter()
        .map(|c| c["task_id"].as_str().unwrap().to_string())
        .collect();
    assert_ne!(task_ids[0], task_ids[1]);
    assert!(matches!(
        agents.send_prompt(a, &s, vec![PromptBlock::text(" ")]),
        Err(AgentError::Other(_))
    ));
}

#[test]
fn agents_edit_and_search_modes_return_structured_results() {
    let fake = agents_host(false);
    let mut agents = DelegatedAgents::new(fake.link());
    let a = agents.attach_prompt_agent();
    let evs = collect(&mut agents, |e| !e.is_empty());
    assert!(matches!(&evs[0].1, AgentEvent::Ready { info } if info.name == "EveryAI"));
    assert_eq!(agents.agents()[0].state, AgentState::Ready);
    let s = session_of(&mut agents, a);
    assert!(s.0.starts_with("everyai-"));
    assert!(
        fake.calls(tools::AGENTS_START).is_empty(),
        "the prompt agent opens no host session"
    );

    let mk = |mode| PromptRunArgs {
        task_id: "ignored".into(),
        mode,
        instruction: "rewrite".into(),
        file: Some(tools::Doc {
            path: "/w/a.rs".into(),
            text: Some("buffer".into()),
            version: Some(7),
        }),
        selection: Some(tools::Selection {
            range: skink_host::tools::Range {
                start: skink_host::tools::Position {
                    line: 2,
                    character: 0,
                },
                end: skink_host::tools::Position {
                    line: 2,
                    character: 4,
                },
            },
            text: "line".into(),
        }),
        context: vec![],
        model: None,
        cwd: None,
    };

    let turn = agents.run_prompt(a, &s, mk(PromptMode::Edit)).unwrap();
    let evs = collect(&mut agents, has_turn_end);
    // One edit streamed live, the second one only in the final result.
    let calls: Vec<_> = evs
        .iter()
        .filter_map(|(_, e)| match e {
            AgentEvent::ToolCall { call, .. } => Some(call.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| c.kind == ToolKind::Edit));
    assert_eq!(calls[0].locations[0].path, PathBuf::from("/w/a.rs"));
    assert_eq!(calls[0].locations[0].line, Some(3));
    assert_eq!(calls[1].locations[0].path, PathBuf::from("/w/b.rs"));
    assert_ne!(calls[0].id, calls[1].id);
    let r = agents.take_result(turn).unwrap();
    assert_eq!(r.files.len(), 2);
    let sent = &fake.calls(tools::PROMPT_RUN)[0];
    assert_eq!(sent["mode"], "edit");
    assert_ne!(sent["task_id"], "ignored");
    assert_eq!(sent["file"]["text"], "buffer");
    assert_eq!(sent["selection"]["text"], "line");

    let turn = agents.run_prompt(a, &s, mk(PromptMode::Search)).unwrap();
    let evs = collect(&mut agents, has_turn_end);
    let hits = evs
        .iter()
        .filter(|(_, e)| matches!(e, AgentEvent::ToolCall { call, .. } if call.kind == ToolKind::Search))
        .count();
    assert_eq!(hits, 2);
    assert_eq!(agents.take_result(turn).unwrap().locations.len(), 2);
}

#[test]
fn agents_cancel_stops_the_turn_and_tells_the_host() {
    let fake = agents_host(true);
    let mut agents = DelegatedAgents::new(fake.link());
    let a = agents.attach_prompt_agent();
    collect(&mut agents, |e| !e.is_empty());
    let s = session_of(&mut agents, a);
    let s2 = SessionId("another".into());

    let turn = agents
        .send_prompt(a, &s, vec![PromptBlock::text("long task")])
        .unwrap();
    wait_until("host to start the prompt", || {
        fake.state.hanging.load(Ordering::SeqCst) == 1
    });
    assert!(matches!(
        agents.send_prompt(a, &s, vec![PromptBlock::text("again")]),
        Err(AgentError::Busy)
    ));
    // Another session is independent; cancelling a quiet session is a no-op.
    agents.cancel(a, &s2).unwrap();
    assert!(matches!(
        agents.cancel(AgentId(77), &s),
        Err(AgentError::UnknownAgent(_))
    ));

    let start = Instant::now();
    agents.cancel(a, &s).unwrap();
    let evs = collect(&mut agents, has_turn_end);
    assert!(start.elapsed() < Duration::from_secs(3));
    match &evs.last().unwrap().1 {
        AgentEvent::TurnFinished { turn: t, stop, .. } => {
            assert_eq!(*t, turn);
            assert_eq!(*stop, StopReason::Cancelled);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        evs.iter().any(
            |(_, e)| matches!(e, AgentEvent::ThoughtChunk { text, .. } if text == "thinking\n")
        ),
        "progress before the cancel was delivered"
    );

    wait_until("prompt.cancel to reach the host", || {
        !fake.calls(tools::PROMPT_CANCEL).is_empty()
    });
    let task = fake.calls(tools::PROMPT_RUN)[0]["task_id"].clone();
    assert_eq!(fake.calls(tools::PROMPT_CANCEL)[0]["task_id"], task);
    assert!(
        fake.methods()
            .iter()
            .any(|m| m == "notifications/cancelled")
    );

    // The session is usable again.
    agents
        .send_prompt(a, &s, vec![PromptBlock::text("next")])
        .unwrap();
    let evs = collect(&mut agents, has_turn_end);
    assert!(matches!(
        evs.last().unwrap().1,
        AgentEvent::TurnFinished {
            stop: StopReason::EndTurn,
            ..
        }
    ));
}

#[test]
fn cancelling_one_turn_leaves_the_other_running() {
    let fake = agents_host(true);
    let mut agents = DelegatedAgents::new(fake.link());
    let a = agents.attach_prompt_agent();
    collect(&mut agents, |e| !e.is_empty());
    let (s1, s2) = (SessionId("one".into()), SessionId("two".into()));

    agents
        .send_prompt(a, &s1, vec![PromptBlock::text("hang")])
        .unwrap();
    wait_until("first prompt to hang", || {
        fake.state.hanging.load(Ordering::SeqCst) == 1
    });
    // The second prompt completes on its own; cancel the first meanwhile.
    agents.cancel(a, &s1).unwrap();
    agents
        .send_prompt(a, &s2, vec![PromptBlock::text("quick")])
        .unwrap();
    let evs = collect(&mut agents, |e| {
        e.iter()
            .filter(|(_, ev)| matches!(ev, AgentEvent::TurnFinished { .. }))
            .count()
            == 2
    });
    let stops: Vec<_> = evs
        .iter()
        .filter_map(|(_, e)| match e {
            AgentEvent::TurnFinished { session, stop, .. } => {
                Some((session.0.clone(), stop.clone()))
            }
            _ => None,
        })
        .collect();
    assert!(stops.contains(&("one".into(), StopReason::Cancelled)));
    assert!(stops.contains(&("two".into(), StopReason::EndTurn)));
}

#[test]
fn agents_host_errors_become_failure_events() {
    let fake = Fake::start(|name, _| match name {
        tools::AGENTS_LIST => {
            Reply::Ok(json!({"agents": [{"id": "a", "name": "A", "installed": true}]}))
        }
        tools::AGENTS_START => Reply::ToolErr("cannot start A"),
        tools::PROMPT_RUN => Reply::ToolErr("no model configured"),
        _ => Reply::Ok(Value::Null),
    });
    let mut agents = DelegatedAgents::new(fake.link());
    let a = ready_agent(&mut agents, "a");
    agents.new_session(a, Path::new("/w")).unwrap();
    let evs = collect(&mut agents, |e| !e.is_empty());
    match &evs[0].1 {
        AgentEvent::RequestFailed { error, .. } => {
            assert!(error.message.contains("cannot start A"), "{error:?}");
        }
        other => panic!("{other:?}"),
    }
    let s = SessionId("s".into());
    agents
        .send_prompt(a, &s, vec![PromptBlock::text("x")])
        .unwrap();
    let evs = collect(&mut agents, has_turn_end);
    match &evs.last().unwrap().1 {
        AgentEvent::TurnFailed { error, .. } => {
            assert!(error.message.contains("no model configured"));
        }
        other => panic!("{other:?}"),
    }
    // An answered error means the host is alive.
    assert!(agents.link().is_available());
}

#[test]
fn agents_host_going_away_mid_turn_and_after() {
    let fake = Fake::start(|name, _| match name {
        tools::AGENTS_LIST => {
            Reply::Ok(json!({"agents": [{"id": "a", "name": "A", "installed": true}]}))
        }
        tools::AGENTS_START => Reply::Ok(json!({"session_id": "s1"})),
        // The host crashes after one progress message.
        tools::PROMPT_RUN => Reply::Close(vec![json!({"type": "text_delta", "text": "par"})]),
        _ => Reply::Ok(Value::Null),
    });
    let link = fake.link_with(LinkPolicy {
        max_failures: 2,
        retry_after: Duration::from_secs(60),
    });
    let mut agents = DelegatedAgents::new(link);
    let a = ready_agent(&mut agents, "a");
    let s = session_of(&mut agents, a);

    agents
        .send_prompt(a, &s, vec![PromptBlock::text("go")])
        .unwrap();
    let evs = collect(&mut agents, has_turn_end);
    assert!(
        evs.iter()
            .any(|(_, e)| matches!(e, AgentEvent::MessageChunk { text, .. } if text == "par"))
    );
    assert!(matches!(
        evs.last().unwrap().1,
        AgentEvent::TurnFailed { .. }
    ));

    drop(fake); // and now the host is gone altogether

    // Failure number two arrives as an event, not a panic or a hang.
    agents.new_session(a, Path::new("/w")).unwrap();
    let evs = collect(&mut agents, |e| !e.is_empty());
    assert!(
        matches!(&evs[0].1, AgentEvent::RequestFailed { .. }),
        "{evs:?}"
    );

    // Tripped: everything answers Unsupported immediately.
    let t = Instant::now();
    assert!(matches!(
        agents.new_session(a, Path::new("/w")),
        Err(AgentError::Unsupported(_))
    ));
    assert!(matches!(
        agents.send_prompt(a, &s, vec![PromptBlock::text("x")]),
        Err(AgentError::Unsupported(_))
    ));
    assert!(matches!(
        agents.start_agent("a", AgentSpec::new("a")),
        Err(AgentError::Unsupported(_))
    ));
    assert!(matches!(agents.models(), Err(AgentError::Unsupported(_))));
    assert!(t.elapsed() < Duration::from_secs(1));
    assert!(agents.poll_events().is_empty());
    assert_eq!(agents.backend(), Backend::Delegated);
    // Housekeeping still works.
    agents.cancel(a, &s).unwrap();
    agents.stop_agent(a);
    assert!(agents.agents().is_empty());
}

// ------------------------------------------------------------ providers

fn vars_for(fake: &Fake) -> impl Fn(&str) -> Option<String> + use<> {
    let ep = fake.endpoint();
    move |k| match k {
        skink_host::ENV_URL => Some(ep.url.clone()),
        skink_host::ENV_TOKEN => Some(ep.token().to_string()),
        _ => None,
    }
}

#[test]
fn providers_pick_delegated_when_the_host_answers() {
    let dir = tmp("prov-delegated");
    let doc = dir.join("p.rs");
    let fake = lsp_host(doc.to_string_lossy().into_owned(), String::new());
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let opts = ProviderOptions {
        waker: Some(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        })),
        ..ProviderOptions::default()
    };
    let mut p = providers_with(&dir, vars_for(&fake), &opts);
    assert_eq!(p.backend, Backend::Delegated);
    assert_eq!(p.lsp.backend(), Backend::Delegated);
    assert_eq!(p.agents.backend(), Backend::Delegated);

    // The LSP side works through the shared trait object, from another thread.
    let lsp = p.lsp.clone();
    let d = doc.clone();
    let h = thread::spawn(move || lsp.hover(&d, pos(0, 0)).unwrap().unwrap().text);
    assert_eq!(h.join().unwrap(), "fn a()");
    // did_open auto-refreshes diagnostics in the background and wakes the UI.
    p.lsp.did_open(&doc, "a😀b = 1\nxy\n").unwrap();
    wait_until("diagnostics push", || !p.lsp.diagnostics(&doc).is_empty());
    assert!(!p.lsp.poll_events().is_empty());
    assert!(wakes.load(Ordering::SeqCst) >= 1);
    assert!(p.agents.poll_events().is_empty());
    // LSP and agents are separate cancel scopes (two connections' worth of
    // in-flight tracking), though one handshake.
    assert_eq!(
        fake.methods().iter().filter(|m| *m == "initialize").count(),
        1
    );
}

#[test]
fn providers_fall_back_to_local() {
    let dir = tmp("prov-local");
    // No variables.
    let mut p = providers_with(&dir, |_| None, &ProviderOptions::default());
    assert_eq!(p.backend, Backend::Local);
    assert_eq!(p.lsp.backend(), Backend::Local);
    assert_eq!(p.agents.backend(), Backend::Local);
    assert!(p.agents.poll_events().is_empty());
    // Unsupported file type: notifications are quiet no-ops.
    assert!(p.lsp.did_open(Path::new("x.txt"), "hi").is_ok());

    // Variables pointing at nothing, at a non-loopback host, and at a bad
    // token: all stay standalone, quickly and silently.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    for url in [
        format!("http://127.0.0.1:{dead}/mcp"),
        "http://example.com/mcp".to_string(),
    ] {
        let t = Instant::now();
        let vars = move |k: &str| match k {
            skink_host::ENV_URL => Some(url.clone()),
            skink_host::ENV_TOKEN => Some(TOKEN.to_string()),
            _ => None,
        };
        let p = providers_with(&dir, vars, &ProviderOptions::default());
        assert_eq!(p.backend, Backend::Local);
        assert_eq!(p.lsp.backend(), Backend::Local);
        assert!(t.elapsed() < Duration::from_secs(3));
    }
    let fake = Fake::start(|_, _| Reply::Ok(Value::Null));
    let ep = fake.endpoint();
    let p = providers_with(
        &dir,
        move |k| match k {
            skink_host::ENV_URL => Some(ep.url.clone()),
            skink_host::ENV_TOKEN => Some("wrong".into()),
            _ => None,
        },
        &ProviderOptions::default(),
    );
    assert_eq!(p.backend, Backend::Local);
}

#[test]
fn trait_objects_are_send_and_sync_where_promised() {
    fn send_sync<T: Send + Sync + ?Sized>() {}
    fn send<T: Send + ?Sized>() {}
    send_sync::<DelegatedLsp>();
    send_sync::<dyn LspProvider>();
    send_sync::<HostLink>();
    send::<DelegatedAgents>();
    send::<dyn AgentController>();
}
