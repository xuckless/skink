//! The AI layer end to end against a scripted in-process ACP agent (memory
//! pipes, no process, no network, no real model).

use crate::{App, ai::AskBox, refactor::PreviewState};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use skink_ai::{
    LocalAgents,
    acp::{BufferSource, Transport, memory_pipe, transport::PipeWriter},
};
use skink_config::{Keymap, ModelChoice};
use skink_core::{Backend, Buffer, Editor};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// What the fake agent does when a prompt arrives.
#[derive(Clone)]
enum Script {
    /// Stream this text, then end the turn.
    Answer(&'static str),
    /// Wait for `session/cancel`, then end the turn as cancelled.
    WaitForCancel,
    /// Write `content` to `path` through the client, then end the turn.
    Write {
        path: PathBuf,
        content: &'static str,
    },
}

type Log = Arc<Mutex<Vec<String>>>;

fn send(w: &mut PipeWriter, v: Value) {
    w.write_all(format!("{v}\n").as_bytes()).unwrap();
}

/// Start the fake agent; returns the client's transport and the method log.
fn fake_agent(script: Script) -> (Transport, Log) {
    let (c2a_w, c2a_r) = memory_pipe();
    let (a2c_w, a2c_r) = memory_pipe();
    let log: Log = Arc::default();
    let l = log.clone();
    std::thread::spawn(move || {
        let mut rd = BufReader::new(c2a_r);
        let mut wr = a2c_w;
        let mut sessions = 0;
        let mut pending_prompt: Option<Value> = None;
        let mut line = String::new();
        loop {
            line.clear();
            if rd.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let m: Value = serde_json::from_str(&line).unwrap();
            let method = m["method"].as_str().unwrap_or("").to_owned();
            if !method.is_empty() {
                l.lock().unwrap().push(
                    format!("{method} {}", m["params"]["modelId"].as_str().unwrap_or(""))
                        .trim()
                        .to_owned(),
                );
            }
            let reply = |wr: &mut PipeWriter, r: Value| {
                send(wr, json!({"jsonrpc": "2.0", "id": m["id"], "result": r}))
            };
            match method.as_str() {
                "initialize" => reply(
                    &mut wr,
                    json!({"protocolVersion": 1, "agentCapabilities": {}, "authMethods": [],
                           "agentInfo": {"name": "fake", "version": "1"}}),
                ),
                "session/new" => {
                    sessions += 1;
                    reply(
                        &mut wr,
                        json!({"sessionId": format!("s{sessions}"),
                               "models": {"currentModelId": "fast", "availableModels": [
                                   {"modelId": "fast", "name": "Fast"},
                                   {"modelId": "big", "name": "Big"}]}}),
                    )
                }
                "session/set_model" => reply(&mut wr, json!({})),
                "session/prompt" => {
                    let sid = m["params"]["sessionId"].clone();
                    let text = m["params"]["prompt"][0]["text"].as_str().unwrap_or("");
                    l.lock().unwrap().push(format!("text {text}"));
                    match &script {
                        Script::Answer(text) => {
                            for part in text.split_inclusive('\n') {
                                send(
                                    &mut wr,
                                    json!({"jsonrpc": "2.0", "method": "session/update",
                                           "params": {"sessionId": sid, "update": {
                                               "sessionUpdate": "agent_message_chunk",
                                               "content": {"type": "text", "text": part}}}}),
                                );
                            }
                            reply(&mut wr, json!({"stopReason": "end_turn"}));
                        }
                        Script::WaitForCancel => pending_prompt = Some(m.clone()),
                        Script::Write { path, content } => {
                            send(
                                &mut wr,
                                json!({"jsonrpc": "2.0", "id": 900, "method": "fs/write_text_file",
                                       "params": {"sessionId": sid, "path": path, "content": content}}),
                            );
                            // The client's answer to our request.
                            line.clear();
                            rd.read_line(&mut line).unwrap();
                            reply(&mut wr, json!({"stopReason": "end_turn"}));
                        }
                    }
                }
                "session/cancel" => {
                    if let Some(p) = pending_prompt.take() {
                        send(
                            &mut wr,
                            json!({"jsonrpc": "2.0", "id": p["id"], "result": {"stopReason": "cancelled"}}),
                        );
                    }
                }
                _ => {}
            }
        }
    });
    (Transport::new(a2c_r, c2a_w), log)
}

fn app_with(text: &str, path: Option<&Path>) -> App {
    let buf = match path {
        Some(p) => {
            std::fs::write(p, text).unwrap();
            Buffer::open(p).unwrap()
        }
        None => Buffer::from_text(text),
    };
    let mut a = App::new(Editor::new(buf), Keymap::defaults(), Backend::Local);
    if let Some(p) = path {
        a.doc.canon = Some(crate::doc::canonical(p));
    }
    a.icons = crate::icons::IconSet::Ascii;
    a
}

fn attach(a: &mut App, script: Script) -> Log {
    let (t, log) = fake_agent(script);
    let root = a.project_root();
    let shared: Arc<dyn BufferSource> = a.ai.shared.clone();
    let mut c = LocalAgents::for_workspace(root, shared);
    let id = c.attach("Fake", t);
    a.use_agent(Box::new(c), id, "Fake");
    log
}

fn key(a: &mut App, code: KeyCode) {
    a.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn type_str(a: &mut App, s: &str) {
    for c in s.chars() {
        key(a, KeyCode::Char(c));
    }
}

fn pump_until(a: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let t0 = Instant::now();
    while !done(a) {
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "timed out waiting for {what}"
        );
        a.poll_background();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The screen as text rows.
fn screen(a: &mut App, w: u16, h: u16) -> Vec<String> {
    use ratatui::{Terminal, backend::TestBackend};
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| a.render(f)).unwrap();
    let b = t.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| b[(x, y)].symbol()).collect::<String>())
        .collect()
}

#[test]
fn rewrite_switches_model_streams_and_applies_as_one_undo_step() {
    let mut a = app_with("fn a() {}\nfn z() {}\n", None);
    a.settings.ai.model = ModelChoice::Named("Big".into());
    let log = attach(
        &mut a,
        Script::Answer("Here is the updated code:\n```rust\nfn b() {}\n```\n"),
    );
    a.doc.select_range((0, 10));
    a.open_ai_prompt();
    type_str(&mut a, "rename to b");
    key(&mut a, KeyCode::Enter);
    pump_until(&mut a, "the review", |a| {
        matches!(a.ai.ask, AskBox::Reviewing(_))
    });
    // Picked by name, sent by id, before the prompt.
    let l = log.lock().unwrap().clone();
    let set = l
        .iter()
        .position(|m| m == "session/set_model big")
        .expect("model switched");
    let prompt = l.iter().position(|m| m == "session/prompt").unwrap();
    assert!(set < prompt, "{l:?}");
    key(&mut a, KeyCode::Char('a')); // accept all
    assert_eq!(a.doc.editor.buffer.text(), "fn b() {}\nfn z() {}\n");
    assert!(matches!(a.ai.ask, AskBox::Closed));
    a.doc.editor.undo();
    assert_eq!(a.doc.editor.buffer.text(), "fn a() {}\nfn z() {}\n");
}

#[test]
fn rejected_hunks_stay_and_esc_cancels_a_running_turn() {
    // Two hunks; reject the first.
    let mut a = app_with("one\ntwo\nthree\nfour\n", None);
    attach(&mut a, Script::Answer("```\nONE\ntwo\nthree\nFOUR\n```"));
    a.doc.select_range((0, 19));
    a.open_ai_prompt();
    type_str(&mut a, "shout");
    key(&mut a, KeyCode::Enter);
    pump_until(&mut a, "the review", |a| {
        matches!(a.ai.ask, AskBox::Reviewing(_))
    });
    // Inline: the old line struck through in place, the new one under it.
    let s = screen(&mut a, 80, 12);
    let at = |needle: &str| {
        s.iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} in {s:#?}"))
    };
    assert!(at("-1 one") < at("+  ONE"), "{s:#?}");
    assert!(at("+  ONE") < at(" 2 two"), "{s:#?}");
    assert!(s[s.len() - 2].contains("change 1/2"), "{s:#?}");
    key(&mut a, KeyCode::Char('n'));
    key(&mut a, KeyCode::Enter);
    assert_eq!(a.doc.editor.buffer.text(), "one\ntwo\nthree\nFOUR\n");

    // Cancel: the agent is told, the buffer is untouched, the box closes.
    let mut a = app_with("x\n", None);
    let log = attach(&mut a, Script::WaitForCancel);
    a.open_ai_prompt();
    type_str(&mut a, "slow");
    key(&mut a, KeyCode::Enter);
    pump_until(&mut a, "the turn", |a| {
        matches!(a.ai.ask, AskBox::Streaming(_))
    });
    // The status line is the worker: agent, mode, time, state.
    let s = screen(&mut a, 120, 6);
    assert!(
        s.last().unwrap().contains("Fake · rewrite · 0s · thinking"),
        "{s:#?}"
    );
    key(&mut a, KeyCode::Esc);
    assert!(matches!(a.ai.ask, AskBox::Closed));
    pump_until(&mut a, "the cancel", |_| {
        log.lock().unwrap().iter().any(|m| m == "session/cancel")
    });
    assert_eq!(a.doc.editor.buffer.text(), "x\n");
}

#[test]
fn agent_writes_are_staged_and_applied_through_the_preview() {
    let dir = std::env::temp_dir().join(format!("skink-ai-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = crate::doc::canonical(&dir).join("lib.rs");
    let mut a = app_with("fn old() {}\n", Some(&file));
    attach(
        &mut a,
        Script::Write {
            path: file.clone(),
            content: "fn new() {}\n",
        },
    );
    a.open_agent_panel("rename old to new");
    pump_until(&mut a, "the preview", |a| {
        matches!(a.ai.preview, PreviewState::Open(_))
    });
    // Nothing touched the disk or the buffer yet.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn old() {}\n");
    key(&mut a, KeyCode::Enter);
    let i = a
        .find_doc(|d| d.canon.as_deref() == Some(file.as_path()))
        .unwrap();
    a.switch_to(i);
    assert_eq!(a.doc.editor.buffer.text(), "fn new() {}\n");
    assert!(a.ai.shared.staged().is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn panel_chips_are_expanded_against_the_file_tab() {
    let dir = std::env::temp_dir().join(format!("skink-ai-chips-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    let dir = crate::doc::canonical(&dir);
    std::fs::write(dir.join("notes.md"), "the secret is 42\n").unwrap();
    let file = dir.join("lib.rs");
    let mut a = app_with("fn pick() {}\n", Some(&file));
    let log = attach(&mut a, Script::Answer("ok"));
    a.doc.select_range((3, 7));
    a.open_agent_panel("");
    // Ctrl-G in the panel is a chat message; the chips resolve against the
    // file tab.
    a.open_ai_prompt();
    type_str(&mut a, "explain @notes.md with @selection "); // space: no completion list
    key(&mut a, KeyCode::Enter);
    let sent = |log: &Log, needle: &str| {
        log.lock()
            .unwrap()
            .iter()
            .any(|m| m.starts_with("text ") && m.contains(needle))
    };
    pump_until(&mut a, "the chat turn", |_| sent(&log, "the secret is 42"));
    assert!(sent(&log, "## Selection from lib.rs\n```\npick\n```"));
    // The transcript shows what was typed, not the expansion.
    assert!(!a.doc.editor.buffer.text().contains("the secret"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn last_command_block_of_a_terminal_screen() {
    let screen = "me@box:~/p$ ls\na b\nme@box:~/p$ cargo build\nerror[E0425]: x\n  --> src/a.rs:1:1\nme@box:~/p$\n\n";
    assert_eq!(
        crate::ai::last_command(screen).unwrap(),
        "me@box:~/p$ cargo build\nerror[E0425]: x\n  --> src/a.rs:1:1"
    );
    // No recognisable prompt: the tail of the screen.
    assert_eq!(crate::ai::last_command("a\nb\n> ").unwrap(), "a\nb");
    assert_eq!(crate::ai::last_command("\n\n"), None);
}

#[test]
fn every_ai_layer_draws_at_tiny_and_normal_sizes() {
    use ratatui::{Terminal, backend::TestBackend};
    let draw = |a: &mut App| {
        for (w, h) in [(12, 4), (40, 10), (120, 30)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| a.render(f)).unwrap();
        }
    };
    let mut a = app_with("one\ntwo\nthree\nfour\n", None);
    attach(&mut a, Script::Answer("```\nONE\ntwo\nthree\nFOUR\n```"));
    a.doc.select_range((0, 19));
    a.open_ai_prompt();
    type_str(&mut a, "x @sel");
    draw(&mut a); // editing, with suggestions
    key(&mut a, KeyCode::Esc);
    type_str(&mut a, "ection make it loud");
    key(&mut a, KeyCode::Enter);
    draw(&mut a); // sending
    pump_until(&mut a, "the review", |a| {
        matches!(a.ai.ask, AskBox::Reviewing(_))
    });
    draw(&mut a);
    key(&mut a, KeyCode::Esc);
    let f = crate::refactor::FilePatch::new("a.rs".into(), "1\n2\n".into(), "1\nX\n");
    a.open_refactor(crate::refactor::Preview::new(
        "t",
        crate::refactor::Source::Lsp,
        vec![f],
    ));
    draw(&mut a);
    key(&mut a, KeyCode::Down);
    draw(&mut a);
}

// ---- through EveryAI (a scripted fake host on loopback) --------------------------

mod host {
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    pub const TOKEN: &str = "eai_host_test_token_0123456789";

    /// `tools/call` (name, arguments), in order.
    pub type Calls = Arc<Mutex<Vec<(String, Value)>>>;

    fn sse(c: &mut TcpStream, v: &Value) -> std::io::Result<()> {
        c.write_all(format!("event: message\ndata: {v}\n\n").as_bytes())?;
        c.flush()
    }

    fn serve(calls: &Calls, mut c: TcpStream) -> std::io::Result<()> {
        let mut r = BufReader::new(c.try_clone()?);
        let (mut line, mut len) = (String::new(), 0);
        r.read_line(&mut line)?;
        loop {
            line.clear();
            r.read_line(&mut line)?;
            if line.trim().is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; len];
        r.read_exact(&mut body)?;
        let m: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let id = m["id"].clone();
        if id.is_null() {
            return c.write_all(
                b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
        let json_reply = |c: &mut TcpStream, result: Value| {
            let b = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
            c.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{b}",
                    b.len()
                )
                .as_bytes(),
            )
        };
        match m["method"].as_str().unwrap_or("") {
            "initialize" => json_reply(
                &mut c,
                json!({"protocolVersion": m["params"]["protocolVersion"],
                       "capabilities": {"tools": {}},
                       "serverInfo": {"name": "EveryAI", "version": "1"}}),
            ),
            "tools/list" => json_reply(
                &mut c,
                json!({"tools": skink_host::tools::ALL.iter()
                    .map(|n| json!({"name": n, "inputSchema": {"type": "object"}}))
                    .collect::<Vec<_>>()}),
            ),
            _ => {
                let name = m["params"]["name"].as_str().unwrap_or("").to_owned();
                let args = m["params"]["arguments"].clone();
                let token = m["params"]["_meta"]["progressToken"].clone();
                calls.lock().unwrap().push((name.clone(), args.clone()));
                c.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )?;
                let result = |v: Value| {
                    json!({"jsonrpc": "2.0", "id": id, "result": {
                        "content": [{"type": "text", "text": v.to_string()}], "isError": false}})
                };
                let progress = |c: &mut TcpStream, ev: Value| {
                    sse(
                        c,
                        &json!({"jsonrpc": "2.0", "method": "notifications/progress",
                            "params": {"progressToken": token, "progress": 1,
                                       "message": ev.to_string()}}),
                    )
                };
                let instruction = args["instruction"].as_str().unwrap_or("");
                match name.as_str() {
                    "models.list" => sse(
                        &mut c,
                        &result(json!({"models": [
                            {"id": "eai-fast", "name": "Fast", "default": true},
                            {"id": "eai-big", "name": "Big"}]})),
                    ),
                    "prompt.run" if instruction.contains("slow") => {
                        progress(&mut c, json!({"type": "status", "text": "thinking"}))?;
                        // Keep-alives until Skink hangs up (the cancel).
                        for _ in 0..400 {
                            std::thread::sleep(std::time::Duration::from_millis(20));
                            if c.write_all(b":\n\n").and_then(|()| c.flush()).is_err() {
                                break;
                            }
                        }
                        Ok(())
                    }
                    "prompt.run" => {
                        let text = "```\nONE\n```\n";
                        progress(&mut c, json!({"type": "text_delta", "text": text}))?;
                        sse(
                            &mut c,
                            &result(json!({"task_id": args["task_id"], "text": text})),
                        )
                    }
                    _ => sse(&mut c, &result(json!({"cancelled": true}))),
                }
            }
        }
    }

    /// Start the fake host; returns its endpoint URL and the call log.
    pub fn start() -> (String, Calls) {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let calls: Calls = Arc::default();
        let log = calls.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let log = log.clone();
                std::thread::spawn(move || serve(&log, c));
            }
        });
        (format!("http://127.0.0.1:{port}/mcp"), calls)
    }
}

fn attach_host(a: &mut App) -> host::Calls {
    let (url, calls) = host::start();
    let ep = skink_host::HostEndpoint::new(url, host::TOKEN);
    let client = skink_host::McpClient::connect(&ep).unwrap();
    a.use_host(skink_host::DelegatedAgents::new(Arc::new(
        skink_host::HostLink::new(client),
    )));
    calls
}

#[test]
fn everyai_prompt_streams_to_review_switches_model_and_cancels() {
    let mut a = app_with("one\ntwo\n", None);
    a.settings.ai.model = ModelChoice::Named("big".into());
    let calls = attach_host(&mut a);
    a.doc.select_range((0, 4));
    a.open_ai_prompt();
    type_str(&mut a, "shout");
    key(&mut a, KeyCode::Enter);
    pump_until(&mut a, "the review", |a| {
        matches!(a.ai.ask, AskBox::Reviewing(_))
    });
    // Picked by name from models.list, sent by id with the prompt.
    let runs: Vec<Value> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(n, _)| n == "prompt.run")
        .map(|(_, v)| v.clone())
        .collect();
    assert_eq!(runs[0]["model"], "eai-big", "{runs:?}");
    assert!(a.ai.models.iter().any(|m| m.id == "eai-fast"));
    assert_eq!(a.agent_label(), "EveryAI");
    key(&mut a, KeyCode::Char('a'));
    assert_eq!(a.doc.editor.buffer.text(), "ONE\ntwo\n");

    // Cancel: the host is told and the buffer stays.
    a.open_ai_prompt();
    type_str(&mut a, "slow");
    key(&mut a, KeyCode::Enter);
    pump_until(&mut a, "the turn", |a| {
        matches!(a.ai.ask, AskBox::Streaming(_))
    });
    key(&mut a, KeyCode::Esc);
    assert!(matches!(a.ai.ask, AskBox::Closed));
    pump_until(&mut a, "prompt.cancel", |_| {
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|(n, _)| n == "prompt.cancel")
    });
    assert_eq!(a.doc.editor.buffer.text(), "ONE\ntwo\n");
}
