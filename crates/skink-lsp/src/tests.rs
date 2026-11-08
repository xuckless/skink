//! Integration-style tests against the in-process fake server (and, for the
//! process plumbing, this very test binary re-run as a fake server).

use std::{
    io::{self, BufReader},
    path::PathBuf,
    sync::mpsc::Receiver,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    LocalLsp, LspProvider,
    client::{LspClient, ServerSpec},
    registry::{Language, ServerRegistry, ServerStatus},
    testutil::*,
    types::*,
    uri::path_to_uri,
};

const T: Duration = Duration::from_secs(5);

fn file(name: &str) -> PathBuf {
    std::env::temp_dir().join("skink-lsp-test").join(name)
}

fn method(m: &Value) -> &str {
    m.get("method").and_then(Value::as_str).unwrap_or("")
}

fn rng(l1: u32, c1: u32, l2: u32, c2: u32) -> Value {
    json!({"start":{"line":l1,"character":c1},"end":{"line":l2,"character":c2}})
}

fn p(line: usize, col: usize) -> Position {
    Position::new(line, col)
}

fn is_response(id: Value) -> impl Fn(&Value) -> bool {
    move |m| m.get("method").is_none() && m.get("id") == Some(&id)
}

#[test]
fn handshake_sends_valid_initialize_then_initialized() {
    let spec = ServerSpec {
        init_options: Some(json!({"opt": 1})),
        ..ServerSpec::default()
    };
    let h = harness_with(spec, json!({"hoverProvider": true}), silent);
    h.wait_method("initialized");
    let log = h.log.lock().unwrap().clone();
    assert_eq!(method(&log[0]), "initialize");
    assert_eq!(method(&log[1]), "initialized");
    let init = &log[0];
    assert_eq!(init["params"]["rootUri"], ROOT);
    assert_eq!(init["params"]["workspaceFolders"][0]["uri"], ROOT);
    assert_eq!(init["params"]["initializationOptions"], json!({"opt": 1}));
    assert_eq!(init["params"]["clientInfo"]["name"], "skink");
    assert_eq!(init["params"]["processId"], std::process::id());
    assert_eq!(
        init["params"]["capabilities"]["general"]["positionEncodings"],
        json!(["utf-16"])
    );
    // The capabilities (and the whole request) must be valid per the LSP spec types.
    let caps = init["params"]["capabilities"].clone();
    let parsed: lsp_types::ClientCapabilities =
        serde_json::from_value(caps).expect("capabilities valid");
    assert_eq!(
        parsed
            .text_document
            .unwrap()
            .completion
            .unwrap()
            .completion_item
            .unwrap()
            .snippet_support,
        Some(false)
    );
    serde_json::from_value::<lsp_types::InitializeParams>(init["params"].clone())
        .expect("params valid");
    assert!(h.client.supports("hoverProvider"));
    assert!(!h.client.supports("renameProvider"));
    assert!(h.client.is_alive());
}

#[test]
fn handshake_times_out_when_server_is_silent() {
    let (client_r, _server_w) = pipe();
    let (_server_r, client_w) = pipe();
    let spec = ServerSpec {
        init_timeout: Some(Duration::from_millis(80)),
        ..ServerSpec::default()
    };
    let start = Instant::now();
    let r = LspClient::from_io("mute", Box::new(client_r), Box::new(client_w), &spec, ROOT);
    assert!(matches!(r, Err(LspError::Timeout)));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "failed handshake must not wait for a polite shutdown"
    );
}

#[test]
fn hover_converts_columns_both_ways() {
    let path = file("hover.rs");
    let h = harness(json!({}), |c, m| {
        if method(m) == "textDocument/hover" {
            c.reply(
                &m["id"],
                json!({"contents": {"kind": "markdown", "value": "T"}, "range": rng(0, 3, 0, 4)}),
            );
        }
        Flow::Continue
    });
    h.client.did_open(&path, "rust", "a😀b").unwrap();
    let hv = h
        .client
        .hover(&path, p(0, 2))
        .unwrap()
        .wait(T)
        .unwrap()
        .unwrap();
    assert_eq!(
        h.wait_method("textDocument/hover")["params"]["position"],
        json!({"line": 0, "character": 3})
    );
    assert_eq!(hv.text, "T");
    assert_eq!(hv.range.unwrap(), Range::new(p(0, 2), p(0, 3)));
    let sent = h.wait_method("textDocument/didOpen");
    assert_eq!(sent["params"]["textDocument"]["uri"], path_to_uri(&path));
    assert_eq!(sent["params"]["textDocument"]["languageId"], "rust");
    assert_eq!(sent["params"]["textDocument"]["text"], "a😀b");
}

#[test]
fn out_of_order_replies_reach_the_right_caller() {
    let path = file("ooo.rs");
    let mut stashed: Option<Value> = None;
    let h = harness(json!({}), move |c, m| {
        let answer = |c: &mut Conn, m: &Value| match method(m) {
            "textDocument/hover" => c.reply(&m["id"], json!({"contents": "hover-answer"})),
            _ => c.reply(
                &m["id"],
                json!({"uri": path_to_uri(&file("target.rs")), "range": rng(4, 1, 4, 2)}),
            ),
        };
        if matches!(method(m), "textDocument/hover" | "textDocument/definition") {
            match stashed.take() {
                None => stashed = Some(m.clone()),
                Some(first) => {
                    answer(c, m); // second request answered first
                    answer(c, &first);
                }
            }
        }
        Flow::Continue
    });
    h.client.did_open(&path, "rust", "fn main() {}").unwrap();
    let hover = h.client.hover(&path, p(0, 1)).unwrap();
    let def = h.client.definition(&path, p(0, 1)).unwrap();
    let locs = def.wait(T).unwrap();
    let hov = hover.wait(T).unwrap().unwrap();
    assert_eq!(hov.text, "hover-answer");
    assert_eq!(locs.len(), 1);
    assert_eq!(locs[0].path, file("target.rs"));
    assert_eq!((locs[0].line(), locs[0].col()), (4, 1));
}

#[test]
fn many_concurrent_requests_all_resolve() {
    let path = file("many.rs");
    let h = harness(json!({}), |c, m| {
        if method(m) == "textDocument/hover" {
            let line = m["params"]["position"]["line"].as_u64().unwrap();
            c.reply(&m["id"], json!({"contents": format!("L{line}")}));
        }
        Flow::Continue
    });
    let text = "x\n".repeat(50);
    h.client.did_open(&path, "rust", &text).unwrap();
    let pend: Vec<_> = (0..50)
        .map(|i| (i, h.client.hover(&path, p(i, 0)).unwrap()))
        .collect();
    for (i, pd) in pend.into_iter().rev() {
        assert_eq!(pd.wait(T).unwrap().unwrap().text, format!("L{i}"));
    }
}

#[test]
fn diagnostics_are_converted_cached_and_cleared() {
    let path = file("diag.rs");
    let uri = path_to_uri(&path);
    let u2 = uri.clone();
    let h = harness(json!({}), move |c, m| {
        if method(m) == "textDocument/didOpen" {
            c.notify(
                "textDocument/publishDiagnostics",
                json!({"uri": u2, "version": 1, "diagnostics": [
                    {"range": rng(0, 3, 0, 4), "severity": 2, "code": "W1", "source": "fake", "message": "careful"},
                    {"range": rng(0, 0, 0, 1), "message": "bad"}
                ]}),
            );
        }
        if method(m) == "textDocument/didChange" {
            c.notify(
                "textDocument/publishDiagnostics",
                json!({"uri": u2, "diagnostics": []}),
            );
        }
        Flow::Continue
    });
    h.client.did_open(&path, "rust", "a😀b").unwrap();
    let LspEvent::Diagnostics {
        path: ep,
        version,
        diagnostics,
        ..
    } = h.recv_event()
    else {
        panic!("expected diagnostics")
    };
    assert_eq!(ep, path);
    assert_eq!(version, Some(1));
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(
        diagnostics[0].range,
        Range::new(p(0, 2), p(0, 3)),
        "UTF-16 -> char columns"
    );
    assert_eq!(diagnostics[0].severity, Severity::Warning);
    assert_eq!(diagnostics[0].source.as_deref(), Some("fake"));
    assert_eq!(diagnostics[0].code.as_deref(), Some("W1"));
    assert_eq!(diagnostics[1].severity, Severity::Error);
    assert_eq!(h.client.diagnostics(&path), diagnostics);
    assert_eq!(h.client.all_diagnostics().len(), 1);

    h.client.did_change(&path, "fixed").unwrap();
    let LspEvent::Diagnostics { diagnostics, .. } = h.recv_event() else {
        panic!()
    };
    assert!(diagnostics.is_empty());
    assert!(h.client.diagnostics(&path).is_empty());
}

#[test]
fn server_requests_get_correct_replies() {
    let path = file("srv.rs");
    let uri = path_to_uri(&path);
    let spec = ServerSpec {
        settings: Some(json!({"rust": {"x": {"a": 1}}, "top": true})),
        ..ServerSpec::default()
    };
    let h = harness_with(spec, json!({}), move |c, m| {
        if method(m) == "initialized" {
            c.request(
                json!("cfg"),
                "workspace/configuration",
                json!({"items": [{"section": "rust.x"}, {"section": "nope"}, {}]}),
            );
            c.request(
                json!(100),
                "client/registerCapability",
                json!({"registrations": []}),
            );
            c.request(
                json!(101),
                "client/unregisterCapability",
                json!({"unregisterations": []}),
            );
            c.request(
                json!("p"),
                "window/workDoneProgress/create",
                json!({"token": "t"}),
            );
            c.request(
                json!(7),
                "workspace/applyEdit",
                json!({"label": "L", "edit": {"changes": {uri.clone(): [
                {"range": rng(0, 0, 0, 1), "newText": "X"}]}}}),
            );
            c.request(json!(8), "unknown/method", json!({}));
            c.request(json!(9), "workspace/workspaceFolders", Value::Null);
            c.request(json!(10), "workspace/inlayHint/refresh", Value::Null);
            c.request(
                json!(11),
                "window/showMessageRequest",
                json!({"type": 2, "message": "pick", "actions": []}),
            );
            c.request(
                json!(12),
                "window/showDocument",
                json!({"uri": "file:///x"}),
            );
        }
        Flow::Continue
    });
    let r = |id: Value| h.wait_for(is_response(id));
    assert_eq!(
        r(json!("cfg"))["result"],
        json!([{"a": 1}, null, {"rust": {"x": {"a": 1}}, "top": true}])
    );
    assert_eq!(r(json!(100))["result"], Value::Null);
    assert!(r(json!(100)).get("error").is_none());
    assert_eq!(r(json!(101))["result"], Value::Null);
    assert_eq!(r(json!("p"))["result"], Value::Null);
    assert_eq!(r(json!(7))["result"], json!({"applied": true}));
    assert_eq!(r(json!(8))["error"]["code"], -32601);
    assert_eq!(r(json!(9))["result"][0]["uri"], ROOT);
    assert_eq!(r(json!(10))["result"], Value::Null);
    assert_eq!(r(json!(11))["result"], Value::Null);
    assert_eq!(r(json!(12))["result"], json!({"success": false}));

    let mut got_edit = false;
    let mut got_show = false;
    while !(got_edit && got_show) {
        match h.recv_event() {
            LspEvent::ApplyEdit { label, edit, .. } => {
                assert_eq!(label.as_deref(), Some("L"));
                assert_eq!(edit.files[0].path, path);
                assert_eq!(edit.files[0].edits[0].new_text, "X");
                got_edit = true;
            }
            LspEvent::Message {
                text,
                show: true,
                level: LogLevel::Warning,
                ..
            } if text == "pick" => got_show = true,
            _ => {}
        }
    }
}

#[test]
fn messages_and_progress_become_events() {
    let h = harness(json!({}), |c, m| {
        if method(m) == "initialized" {
            c.notify("window/logMessage", json!({"type": 3, "message": "hello"}));
            c.notify("window/showMessage", json!({"type": 1, "message": "boom"}));
            c.notify("$/progress", json!({"token": "idx", "value": {"kind": "begin", "title": "Indexing", "percentage": 0}}));
            c.notify("$/progress", json!({"token": 5, "value": {"kind": "report", "message": "3/10", "percentage": 30}}));
            c.notify(
                "$/progress",
                json!({"token": "idx", "value": {"kind": "end"}}),
            );
            c.notify("telemetry/event", json!({}));
        }
        Flow::Continue
    });
    let ev: Vec<LspEvent> = (0..5).map(|_| h.recv_event()).collect();
    assert!(
        matches!(&ev[0], LspEvent::Message { level: LogLevel::Info, text, show: false, .. } if text == "hello")
    );
    assert!(
        matches!(&ev[1], LspEvent::Message { level: LogLevel::Error, text, show: true, .. } if text == "boom")
    );
    assert!(
        matches!(&ev[2], LspEvent::Progress { token, title: Some(t), done: false, percentage: Some(0), .. } if token == "idx" && t == "Indexing")
    );
    assert!(
        matches!(&ev[3], LspEvent::Progress { token, message: Some(m), percentage: Some(30), .. } if token == "5" && m == "3/10")
    );
    assert!(matches!(&ev[4], LspEvent::Progress { done: true, .. }));
}

#[test]
fn server_crash_fails_pending_and_reports_exit() {
    let path = file("crash.rs");
    let h = harness(json!({}), |_, m| {
        if method(m) == "textDocument/hover" {
            Flow::Crash
        } else {
            Flow::Continue
        }
    });
    h.client.did_open(&path, "rust", "x").unwrap();
    let pending = h.client.hover(&path, p(0, 0)).unwrap();
    let start = Instant::now();
    assert_eq!(pending.wait(T).unwrap_err(), LspError::Disconnected);
    assert!(start.elapsed() < Duration::from_secs(2));
    loop {
        if let LspEvent::Exited { expected, .. } = h.recv_event() {
            assert!(!expected);
            break;
        }
    }
    assert!(!h.client.is_alive());
    let again = h.client.hover(&path, p(0, 0)).and_then(|p| p.wait(T));
    assert_eq!(again.unwrap_err(), LspError::Disconnected);
    assert_eq!(
        h.client.did_change(&path, "y").unwrap_err(),
        LspError::Disconnected
    );
}

#[test]
fn timeout_cancels_request_and_client_stays_usable() {
    let path = file("timeout.rs");
    let h = harness(json!({}), |c, m| {
        if method(m) == "textDocument/formatting" {
            c.reply(
                &m["id"],
                json!([{"range": rng(0, 0, 0, 1), "newText": "Z"}]),
            );
        }
        Flow::Continue // hover is never answered
    });
    h.client.did_open(&path, "rust", "abc").unwrap();
    let pending = h.client.hover(&path, p(0, 0)).unwrap();
    assert_eq!(
        pending.wait(Duration::from_millis(50)).unwrap_err(),
        LspError::Timeout
    );
    let hover_id = h.wait_method("textDocument/hover")["id"].clone();
    let cancel = h.wait_method("$/cancelRequest");
    assert_eq!(cancel["params"]["id"], hover_id);
    let edits = h
        .client
        .formatting(&path, FormattingOptions::default())
        .unwrap()
        .wait(T)
        .unwrap();
    assert_eq!(edits[0].new_text, "Z");
    assert!(h.client.is_alive());
}

#[test]
fn dropping_a_pending_request_cancels_it() {
    let path = file("dropcancel.rs");
    let h = harness(json!({}), silent);
    h.client.did_open(&path, "rust", "abc").unwrap();
    let pending = h.client.hover(&path, p(0, 0)).unwrap();
    let id = h.wait_method("textDocument/hover")["id"].clone();
    drop(pending);
    assert_eq!(h.wait_method("$/cancelRequest")["params"]["id"], id);
}

#[test]
fn try_get_polls_without_blocking() {
    let path = file("poll.rs");
    let h = harness(json!({}), |c, m| {
        if method(m) == "textDocument/hover" {
            thread::sleep(Duration::from_millis(60));
            c.reply(&m["id"], json!({"contents": "late"}));
        }
        Flow::Continue
    });
    h.client.did_open(&path, "rust", "abc").unwrap();
    let mut pending = h.client.hover(&path, p(0, 0)).unwrap();
    assert!(pending.try_get().is_none());
    let end = Instant::now() + T;
    let got = loop {
        if let Some(r) = pending.try_get() {
            break r;
        }
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(got.unwrap().unwrap().text, "late");
}

#[test]
fn server_error_response_is_typed() {
    let path = file("err.rs");
    let h = harness(json!({}), |c, m| {
        match method(m) {
            "textDocument/hover" => c.error(&m["id"], -32801, "content modified"),
            "textDocument/rename" => c.error(&m["id"], -32602, "bad name"),
            _ => {}
        }
        Flow::Continue
    });
    h.client.did_open(&path, "rust", "abc").unwrap();
    let e = h.client.hover(&path, p(0, 0)).unwrap().wait(T).unwrap_err();
    assert_eq!(
        e,
        LspError::Server {
            code: -32801,
            message: "content modified".into()
        }
    );
    assert!(e.is_transient());
    let e = h
        .client
        .rename(&path, p(0, 0), "x")
        .unwrap()
        .wait(T)
        .unwrap_err();
    assert!(!e.is_transient());
}

#[test]
fn requests_on_unopened_documents_fail_fast() {
    let h = harness(json!({}), silent);
    let path = file("closed.rs");
    assert!(matches!(
        h.client.hover(&path, p(0, 0)),
        Err(LspError::NotOpen(_))
    ));
    assert!(matches!(
        h.client.did_change(&path, "x"),
        Err(LspError::NotOpen(_))
    ));
    h.client.did_open(&path, "rust", "x").unwrap();
    h.client.did_close(&path).unwrap();
    assert!(matches!(
        h.client.definition(&path, p(0, 0)),
        Err(LspError::NotOpen(_))
    ));
    h.wait_method("textDocument/didClose");
}

#[test]
fn reopen_acts_as_change_and_versions_increase() {
    let path = file("versions.rs");
    let h = harness(json!({}), silent);
    h.client.did_open(&path, "rust", "one").unwrap();
    h.client.did_open(&path, "rust", "two").unwrap();
    h.client.did_change(&path, "three").unwrap();
    h.wait_for(|m| {
        method(m) == "textDocument/didChange" && m["params"]["textDocument"]["version"] == 3
    });
    assert_eq!(h.count_method("textDocument/didOpen"), 1);
    assert!(h.client.is_open(&path));
}

#[test]
fn incremental_sync_sends_utf16_ranges_in_order() {
    let path = file("inc.rs");
    let h = harness(
        json!({"textDocumentSync": {"change": 2, "save": {"includeText": true}}}),
        silent,
    );
    h.client.did_open(&path, "rust", "a😀b\nxyz").unwrap();
    h.client
        .did_change_incremental(
            &path,
            &[
                TextEdit {
                    range: Range::new(p(0, 2), p(0, 3)),
                    new_text: "Q".into(),
                },
                TextEdit {
                    range: Range::new(p(1, 0), p(1, 0)),
                    new_text: "!".into(),
                },
            ],
        )
        .unwrap();
    let m = h.wait_method("textDocument/didChange");
    assert_eq!(
        m["params"]["contentChanges"],
        json!([{"range": rng(0, 3, 0, 4), "text": "Q"}, {"range": rng(1, 0, 1, 0), "text": "!"}])
    );
    // our copy of the text advanced: positions after the edit use it
    let sent = h.client.hover(&path, p(0, 3)).unwrap();
    let hov = h.wait_method("textDocument/hover");
    assert_eq!(
        hov["params"]["position"],
        json!({"line": 0, "character": 4})
    );
    drop(sent);
    h.client.did_save(&path).unwrap();
    assert_eq!(
        h.wait_method("textDocument/didSave")["params"]["text"],
        "a😀Q\n!xyz"
    );
}

#[test]
fn full_sync_server_gets_whole_text_for_incremental_edits() {
    let path = file("full.rs");
    let h = harness(json!({"textDocumentSync": 1}), silent);
    h.client.did_open(&path, "rust", "a😀b\nxyz").unwrap();
    h.client
        .did_change_incremental(
            &path,
            &[TextEdit {
                range: Range::new(p(0, 2), p(0, 3)),
                new_text: "Q".into(),
            }],
        )
        .unwrap();
    let m = h.wait_method("textDocument/didChange");
    assert_eq!(
        m["params"]["contentChanges"],
        json!([{"text": "a😀Q\nxyz"}])
    );
    // no includeText: didSave carries no text
    h.client.did_save(&path).unwrap();
    assert!(
        h.wait_method("textDocument/didSave")["params"]
            .get("text")
            .is_none()
    );
}

#[test]
fn sync_none_server_gets_no_changes() {
    let path = file("none.rs");
    let h = harness(json!({"textDocumentSync": 0}), |c, m| {
        if method(m) == "textDocument/formatting" {
            c.reply(&m["id"], json!([]));
        }
        Flow::Continue
    });
    h.client.did_open(&path, "rust", "x").unwrap();
    h.client.did_change(&path, "y").unwrap();
    h.client
        .formatting(&path, FormattingOptions::default())
        .unwrap()
        .wait(T)
        .unwrap(); // barrier
    assert_eq!(h.count_method("textDocument/didChange"), 0);
}

#[test]
fn typed_requests_roundtrip() {
    let path = file("typed.rs");
    let target = path_to_uri(&file("other.rs"));
    let t2 = target.clone();
    let h = harness(json!({}), move |c, m| {
        let id = &m["id"];
        match method(m) {
            "textDocument/references" => c.reply(
                id,
                json!([{"uri": t2, "range": rng(1, 0, 1, 3)}, {"uri": t2, "range": rng(2, 0, 2, 3)}]),
            ),
            "textDocument/completion" => c.reply(
                id,
                json!({"isIncomplete": false, "items": [{"label": "println", "kind": 3, "textEdit":
                    {"range": rng(0, 0, 0, 2), "newText": "println!"}}]}),
            ),
            "textDocument/rename" => c.reply(id, json!({"changes": {t2.clone(): [{"range": rng(0, 0, 0, 1), "newText": "new"}]}})),
            "textDocument/formatting" => c.reply(id, json!([{"range": rng(0, 0, 0, 0), "newText": "// hi\n"}])),
            "textDocument/signatureHelp" => c.reply(
                id,
                json!({"signatures": [{"label": "f(a, b)", "parameters": [{"label": "a"}, {"label": [5, 6]}]}], "activeParameter": 1}),
            ),
            "textDocument/documentSymbol" => c.reply(
                id,
                json!([{"name": "main", "kind": 12, "range": rng(0, 0, 2, 1), "selectionRange": rng(0, 3, 0, 7)}]),
            ),
            "textDocument/typeDefinition" | "textDocument/implementation" => c.reply(id, Value::Null),
            _ => {}
        }
        Flow::Continue
    });
    let c = &h.client;
    c.did_open(&path, "rust", "pr\nfn main() {}\n").unwrap();
    let refs = c.references(&path, p(0, 1), true).unwrap().wait(T).unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(
        h.wait_method("textDocument/references")["params"]["context"],
        json!({"includeDeclaration": true})
    );
    let comp = c.completion(&path, p(0, 2)).unwrap().wait(T).unwrap();
    assert_eq!(comp[0].label, "println");
    assert_eq!(comp[0].insert_text, "println!");
    assert_eq!(comp[0].kind, ItemKind::Function);
    assert_eq!(
        comp[0].edit.as_ref().unwrap().range,
        Range::new(p(0, 0), p(0, 2))
    );
    let ren = c
        .rename(&path, p(0, 0), "new")
        .unwrap()
        .wait(T)
        .unwrap()
        .unwrap();
    assert_eq!(
        h.wait_method("textDocument/rename")["params"]["newName"],
        "new"
    );
    assert_eq!(ren.files[0].path, file("other.rs"));
    let fmt = c
        .formatting(
            &path,
            FormattingOptions {
                tab_size: 2,
                insert_spaces: false,
            },
        )
        .unwrap()
        .wait(T)
        .unwrap();
    assert_eq!(fmt[0].new_text, "// hi\n");
    assert_eq!(
        h.wait_method("textDocument/formatting")["params"]["options"],
        json!({"tabSize": 2, "insertSpaces": false})
    );
    let sig = c
        .signature_help(&path, p(0, 1))
        .unwrap()
        .wait(T)
        .unwrap()
        .unwrap();
    assert_eq!(sig.signatures[0].parameters[1].label, "b");
    assert_eq!(sig.active_parameter, Some(1));
    let syms = c.document_symbols(&path).unwrap().wait(T).unwrap();
    assert_eq!(syms[0].name, "main");
    assert_eq!(syms[0].kind, ItemKind::Function);
    assert!(
        c.type_definition(&path, p(0, 0))
            .unwrap()
            .wait(T)
            .unwrap()
            .is_empty()
    );
    assert!(
        c.implementation(&path, p(0, 0))
            .unwrap()
            .wait(T)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn drop_runs_shutdown_then_exit() {
    let mut h = harness(json!({}), silent);
    let join = h.join.take().unwrap();
    let log = h.log.clone();
    drop(h.client);
    join.join().unwrap(); // the fake only returns after `exit`
    let methods: Vec<String> = log
        .lock()
        .unwrap()
        .iter()
        .map(|m| method(m).to_string())
        .collect();
    let tail = &methods[methods.len() - 2..];
    assert_eq!(tail, ["shutdown", "exit"]);
}

#[test]
fn raw_request_and_notification_escape_hatch() {
    let h = harness(json!({}), |c, m| {
        if method(m) == "custom/echo" {
            c.reply(&m["id"], m["params"].clone());
        }
        Flow::Continue
    });
    let v = h
        .client
        .request_raw("custom/echo", json!({"a": [1, 2]}))
        .unwrap()
        .wait(T)
        .unwrap();
    assert_eq!(v, json!({"a": [1, 2]}));
    h.client.notify_raw("custom/note", json!({})).unwrap();
    h.wait_method("custom/note");
}

// ---- real child process (this test binary re-run as a fake server) -------------------

/// Not a test: when `SKINK_FAKE_LSP` is set this runs a fake server on
/// stdin/stdout. The harness chatter libtest prints is skipped by the framing
/// reader (it only recognises `Content-Length` headers).
#[test]
fn child_server_entry() {
    if std::env::var_os("SKINK_FAKE_LSP").is_none() {
        return;
    }
    let log: Log = Default::default();
    let stdin = io::stdin();
    let mut r = BufReader::new(stdin.lock());
    let mut w = io::stdout().lock();
    // libtest leaves `test ... ` unterminated on this line; end it so our header starts a fresh line.
    io::Write::write_all(&mut w, b"\n").unwrap();
    serve(
        &mut r,
        &mut w,
        &json!({"hoverProvider": true}),
        &log,
        &mut |c, m| {
            match method(m) {
            "textDocument/hover" => {
                eprintln!("fake stderr line");
                c.reply(&m["id"], json!({"contents": "from-child"}));
            }
            "textDocument/definition" => std::process::exit(3),
            "textDocument/didOpen" => c.notify(
                "textDocument/publishDiagnostics",
                json!({"uri": m["params"]["textDocument"]["uri"], "diagnostics": [{"range": rng(0, 0, 0, 1), "message": "from-child"}]}),
            ),
            _ => {}
        }
            Flow::Continue
        },
    );
}

fn child_spec() -> ServerSpec {
    ServerSpec {
        program: std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        args: [
            "--exact",
            "tests::child_server_entry",
            "--nocapture",
            "--test-threads=1",
        ]
        .map(String::from)
        .to_vec(),
        env: vec![("SKINK_FAKE_LSP".into(), "1".into())],
        ..ServerSpec::default()
    }
}

fn next_exit(rx: &Receiver<LspEvent>) -> (Option<i32>, bool) {
    loop {
        if let LspEvent::Exited { code, expected, .. } = rx.recv_timeout(T).expect("exit event") {
            return (code, expected);
        }
    }
}

#[test]
fn child_process_roundtrip_stderr_and_crash_code() {
    let path = file("child.rs");
    let c = LspClient::spawn(child_spec(), ROOT).expect("spawn");
    let rx = c.take_events().unwrap();
    c.did_open(&path, "rust", "x").unwrap();
    assert_eq!(
        c.hover(&path, p(0, 0))
            .unwrap()
            .wait(T)
            .unwrap()
            .unwrap()
            .text,
        "from-child"
    );
    let mut saw_stderr = false;
    let mut saw_diag = false;
    while !(saw_stderr && saw_diag) {
        match rx.recv_timeout(T).expect("event") {
            LspEvent::Message {
                level: LogLevel::Stderr,
                text,
                ..
            } if text == "fake stderr line" => saw_stderr = true,
            LspEvent::Diagnostics { diagnostics, .. } if diagnostics[0].message == "from-child" => {
                saw_diag = true
            }
            _ => {}
        }
    }
    let err = c.definition(&path, p(0, 0)).unwrap().wait(T).unwrap_err();
    assert_eq!(err, LspError::Disconnected);
    assert_eq!(next_exit(&rx), (Some(3), false));
}

#[test]
fn child_process_graceful_shutdown_on_drop() {
    let c = LspClient::spawn(child_spec(), ROOT).expect("spawn");
    let rx = c.take_events().unwrap();
    drop(c);
    assert_eq!(next_exit(&rx), (Some(0), true));
}

#[test]
fn spawning_a_missing_binary_reports_not_installed() {
    let spec = ServerSpec::new("skink-no-such-language-server-binary", &[]);
    assert!(matches!(
        LspClient::spawn(spec, ROOT),
        Err(LspError::NotInstalled(_))
    ));
}

// ---- registry + LocalLsp end to end ----------------------------------------------------

fn fake_language() -> Language {
    Language {
        id: "fake".into(),
        extensions: vec![("fk".into(), "fakelang".into())],
        root_markers: vec![],
        workspace_marker: None,
        servers: vec![
            ServerSpec::new("skink-missing-first-choice", &[]), // falls back to the next installed one
            child_spec(),
        ],
    }
}

#[test]
fn local_lsp_spawns_lazily_routes_and_shuts_down_on_drop() {
    let dir = std::env::temp_dir().join(format!("skink-lsp-reg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.fk");
    let mut reg = ServerRegistry::new(&dir);
    reg.register(fake_language());
    let lsp = LocalLsp::with_registry(reg);

    assert_eq!(lsp.status(&path), ServerStatus::NotStarted);
    assert!(lsp.registry().running_client(&path).is_none());
    lsp.did_open(&path, "let x").unwrap(); // starts the server
    let client = lsp.registry().running_client(&path).expect("running");
    assert!(matches!(lsp.status(&path), ServerStatus::Ready(_)));
    assert_eq!(client.root_uri(), path_to_uri(&dir));

    let h = lsp.hover(&path, p(0, 0)).unwrap().unwrap();
    assert_eq!(h.text, "from-child");

    // diagnostics pushed by the child reach the cache and the event queue
    let end = Instant::now() + T;
    while lsp.diagnostics(&path).is_empty() {
        assert!(Instant::now() < end, "no diagnostics");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(lsp.diagnostics(&path)[0].message, "from-child");
    let evs = lsp.poll_events();
    assert!(
        evs.iter()
            .any(|e| matches!(e, LspEvent::Diagnostics { .. }))
    );

    // a second file of the same language and root shares the server
    let other = dir.join("n.fk");
    lsp.did_open(&other, "y").unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &client,
        &lsp.registry().running_client(&other).unwrap()
    ));

    // unsupported files are quiet
    assert!(lsp.did_open(&dir.join("x.unknown"), "z").is_ok());

    drop(lsp);
    let end = Instant::now() + T;
    while client.is_alive() {
        assert!(
            Instant::now() < end,
            "server still alive after registry drop"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn registry_restart_starts_a_fresh_server() {
    let dir = std::env::temp_dir().join(format!("skink-lsp-restart-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.fk");
    let mut reg = ServerRegistry::new(&dir);
    reg.register(fake_language());
    let first = reg.client_for(&path).unwrap();
    reg.restart(&path);
    let end = Instant::now() + T;
    while first.is_alive() {
        assert!(Instant::now() < end, "old server still alive after restart");
        thread::sleep(Duration::from_millis(5));
    }
    let second = reg.client_for(&path).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first, &second));
    assert!(second.is_alive());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- real servers (opt-in) -------------------------------------------------------------

/// `cargo test -p skink-lsp -- --ignored` with rust-analyzer on PATH.
#[test]
#[ignore = "needs rust-analyzer on PATH"]
fn rust_analyzer_smoke() {
    if crate::registry::find_in_path("rust-analyzer").is_none() {
        eprintln!("rust-analyzer not installed; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("skink-lsp-ra-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"ra_smoke\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let src = "fn answer() -> u32 {\n    42\n}\n\nfn main() {\n    let x: u32 = answer();\n    let s: String = 1;\n}\n";
    let path = dir.join("src/main.rs");
    std::fs::write(&path, src).unwrap();
    let lsp = LocalLsp::new(&dir);
    lsp.did_open(&path, src).expect("start rust-analyzer");
    // definition of `answer` at line 5 col 17 -> line 0
    let end = Instant::now() + Duration::from_secs(60);
    let defs = loop {
        match lsp.definition(&path, p(5, 18)) {
            Ok(d) if !d.is_empty() => break d,
            _ if Instant::now() < end => thread::sleep(Duration::from_millis(500)),
            other => panic!("no definition: {other:?}"),
        }
    };
    assert_eq!(defs[0].line(), 0);
    let hov = lsp.hover(&path, p(5, 18)).unwrap();
    assert!(hov.is_some());
    // the type error eventually shows up as a diagnostic
    while lsp.diagnostics(&path).is_empty() {
        assert!(Instant::now() < end, "no diagnostics from rust-analyzer");
        thread::sleep(Duration::from_millis(500));
    }
    drop(lsp);
    let _ = std::fs::remove_dir_all(&dir);
}
