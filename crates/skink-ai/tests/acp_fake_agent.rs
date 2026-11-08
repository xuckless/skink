//! End-to-end tests of the ACP client against a scripted in-process agent
//! connected through in-memory pipes (no processes, no network).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};
use skink_ai::acp::transport::{PipeReader, PipeWriter};
use skink_ai::acp::{
    AcpClient, AcpError, AcpEvent, BufferSource, McpServer, NoHandler, Transport, WorkspaceFs,
    memory_pipe,
};
use skink_ai::{
    AgentController, AgentEvent, AgentState, LocalAgents, PermissionKind, PlanStatus, PromptBlock,
    SessionId, StopReason, ToolKind, ToolStatus,
};

const T: Duration = Duration::from_secs(5);

/// The agent's side of the wire.
struct Fake {
    rd: BufReader<PipeReader>,
    wr: PipeWriter,
}

impl Fake {
    fn recv(&mut self) -> Value {
        let mut l = String::new();
        let n = self.rd.read_line(&mut l).expect("read");
        assert!(n > 0, "client closed the pipe");
        serde_json::from_str(&l).unwrap_or_else(|e| panic!("client sent bad json {l:?}: {e}"))
    }
    fn try_recv(&mut self) -> Option<Value> {
        let mut l = String::new();
        (self.rd.read_line(&mut l).ok()? > 0).then(|| serde_json::from_str(&l).unwrap())
    }
    fn send(&mut self, v: Value) {
        self.raw(&format!("{v}\n"));
    }
    fn raw(&mut self, s: &str) {
        self.wr.write_all(s.as_bytes()).unwrap();
    }
    fn reply(&mut self, req: &Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": req["id"], "result": result}));
    }
    fn reply_err(&mut self, req: &Value, code: i64, msg: &str) {
        self.send(
            json!({"jsonrpc": "2.0", "id": req["id"], "error": {"code": code, "message": msg}}),
        );
    }
    fn notify_update(&mut self, session: &str, update: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": "session/update",
                          "params": {"sessionId": session, "update": update}}));
    }
    fn chunk(&mut self, session: &str, text: &str) {
        self.notify_update(
            session,
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}),
        );
    }
    /// Answer `initialize`.
    fn handshake(&mut self, extra_caps: Value, auth: Value) -> Value {
        let init = self.recv();
        assert_eq!(init["method"], "initialize");
        self.reply(
            &init,
            json!({"protocolVersion": 1, "agentCapabilities": extra_caps, "authMethods": auth,
                   "agentInfo": {"name": "fake-agent", "version": "9.9"}}),
        );
        init
    }
}

fn connect(script: impl FnOnce(Fake) + Send + 'static) -> (Transport, JoinHandle<()>) {
    let (c2a_w, c2a_r) = memory_pipe();
    let (a2c_w, a2c_r) = memory_pipe();
    let h = thread::spawn(move || {
        script(Fake {
            rd: BufReader::new(c2a_r),
            wr: a2c_w,
        });
    });
    (Transport::new(a2c_r, c2a_w), h)
}

fn next(rx: &Receiver<AcpEvent>) -> AcpEvent {
    rx.recv_timeout(T).expect("event expected")
}

fn next_non_protocol(rx: &Receiver<AcpEvent>) -> AcpEvent {
    loop {
        let ev = next(rx);
        if !matches!(ev, AgentEvent::ProtocolError { .. }) {
            return ev;
        }
    }
}

struct Bufs(Mutex<HashMap<PathBuf, String>>);
impl BufferSource for Bufs {
    fn read(&self, p: &Path) -> Option<String> {
        self.0.lock().unwrap().get(p).cloned()
    }
    fn write(&self, p: &Path, c: &str) -> Option<Result<(), String>> {
        let mut g = self.0.lock().unwrap();
        g.get_mut(p).map(|b| {
            *b = c.to_owned();
            Ok(())
        })
    }
}

fn temp_root(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skink-ai-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(d).unwrap()
}

#[test]
fn full_turn_with_streaming_permission_fs_and_noise() {
    let root = temp_root("full");
    std::fs::write(root.join("disk.txt"), "on disk\n").unwrap();
    let bufs = Arc::new(Bufs(Mutex::new(HashMap::new())));
    bufs.0
        .lock()
        .unwrap()
        .insert(root.join("open.rs"), "unsaved text\n".into());
    bufs.0
        .lock()
        .unwrap()
        .insert(root.join("w.rs"), "old".into());

    let r2 = root.clone();
    let (transport, agent) = connect(move |mut a| {
        let init = a.handshake(
            json!({"loadSession": false, "promptCapabilities": {"embeddedContext": true}}),
            json!([{"id": "oauth", "name": "Log in with browser"}]),
        );
        assert_eq!(init["params"]["protocolVersion"], 1);
        assert_eq!(
            init["params"]["clientCapabilities"]["fs"]["readTextFile"],
            true
        );
        assert_eq!(
            init["params"]["clientCapabilities"]["fs"]["writeTextFile"],
            true
        );
        assert_eq!(init["params"]["clientCapabilities"]["terminal"], false);

        let ns = a.recv();
        assert_eq!(ns["method"], "session/new");
        assert!(Path::new(ns["params"]["cwd"].as_str().unwrap()).is_absolute());
        assert_eq!(ns["params"]["mcpServers"][0]["name"], "tools");
        assert_eq!(ns["params"]["mcpServers"][0]["command"], "mcp-tool");
        a.reply(
            &ns,
            json!({"sessionId": "sess-1", "modes": {"currentModeId": "ask",
            "availableModes": [{"id": "ask", "name": "Ask"}, {"id": "code", "name": "Code"}]}}),
        );

        let pr = a.recv();
        assert_eq!(pr["method"], "session/prompt");
        assert_eq!(pr["params"]["sessionId"], "sess-1");
        assert_eq!(
            pr["params"]["prompt"][0],
            json!({"type": "text", "text": "do it"})
        );
        assert_eq!(pr["params"]["prompt"][1]["type"], "resource_link");
        assert_eq!(pr["params"]["prompt"][2]["type"], "resource");

        // noise the client must survive
        a.raw("this is not json\n");
        a.raw("\n");
        a.raw("[1,2,3]\n");
        a.raw("{\"jsonrpc\":\"2.0\",\"id\":424242,\"result\":{}}\n");
        a.raw(&format!(
            "{}\n",
            json!({"jsonrpc":"2.0","method":"$/unknown","params":{}})
        ));
        a.notify_update(
            "sess-1",
            json!({"sessionUpdate": "usage_update", "used": 1}),
        );

        a.chunk("sess-1", "Hel");
        a.chunk("sess-1", "lo");
        a.notify_update(
            "sess-1",
            json!({"sessionUpdate": "agent_thought_chunk",
            "content": {"type": "text", "text": "hmm"}}),
        );
        a.notify_update(
            "sess-1",
            json!({"sessionUpdate": "plan", "entries": [
            {"content": "step one", "priority": "high", "status": "in_progress"}]}),
        );
        a.notify_update(
            "sess-1",
            json!({"sessionUpdate": "available_commands_update",
            "availableCommands": [{"name": "review", "description": "Review code"}]}),
        );
        a.notify_update(
            "sess-1",
            json!({"sessionUpdate": "current_mode_update", "currentModeId": "code"}),
        );
        a.notify_update(
            "sess-1",
            json!({"sessionUpdate": "tool_call", "toolCallId": "tc1",
            "title": "Write open.rs", "kind": "edit", "status": "pending"}),
        );

        // permission request
        a.send(json!({"jsonrpc": "2.0", "id": "perm-1", "method": "session/request_permission",
            "params": {"sessionId": "sess-1", "toolCall": {"toolCallId": "tc1", "title": "Write open.rs"},
                       "options": [{"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                                   {"optionId": "no", "name": "Reject", "kind": "reject_once"}]}}));
        let perm = a.recv();
        assert_eq!(perm["id"], "perm-1", "string ids are echoed verbatim");
        assert_eq!(
            perm["result"]["outcome"],
            json!({"outcome": "selected", "optionId": "yes"})
        );

        // fs: unsaved buffer beats disk, disk fallback, escape refused
        a.send(
            json!({"jsonrpc":"2.0","id":100,"method":"fs/read_text_file",
            "params":{"sessionId":"sess-1","path": r2.join("open.rs")}}),
        );
        let r = a.recv();
        assert_eq!(r["id"], 100);
        assert_eq!(r["result"]["content"], "unsaved text\n");
        a.send(
            json!({"jsonrpc":"2.0","id":101,"method":"fs/read_text_file",
            "params":{"sessionId":"sess-1","path": r2.join("disk.txt"),"line":1,"limit":1}}),
        );
        assert_eq!(a.recv()["result"]["content"], "on disk\n");
        a.send(
            json!({"jsonrpc":"2.0","id":102,"method":"fs/read_text_file",
            "params":{"sessionId":"sess-1","path": "/etc/passwd"}}),
        );
        let r = a.recv();
        assert_eq!(r["error"]["code"], -32602);
        a.send(
            json!({"jsonrpc":"2.0","id":103,"method":"fs/write_text_file",
            "params":{"sessionId":"sess-1","path": r2.join("w.rs"),"content":"agent wrote"}}),
        );
        assert!(a.recv().get("error").is_none());
        a.send(
            json!({"jsonrpc":"2.0","id":104,"method":"fs/read_text_file",
            "params":{"sessionId":"sess-1","path": r2.join("missing.txt")}}),
        );
        assert_eq!(a.recv()["error"]["code"], -32002);

        // terminals are not supported by this handler; unknown methods too
        a.send(json!({"jsonrpc":"2.0","id":105,"method":"terminal/create",
            "params":{"sessionId":"sess-1","command":"ls"}}));
        assert_eq!(a.recv()["error"]["code"], -32601);
        a.send(json!({"jsonrpc":"2.0","id":106,"method":"weird/thing","params":{}}));
        assert_eq!(a.recv()["error"]["code"], -32601);

        a.notify_update(
            "sess-1",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "tc1",
            "status": "completed", "content": [{"type": "diff", "path": r2.join("w.rs"),
            "oldText": "old", "newText": "agent wrote"}]}),
        );
        a.chunk("sess-1", "!");
        a.reply(&pr, json!({"stopReason": "end_turn"}));
        // client closes stdin on drop
        while a.try_recv().is_some() {}
    });

    let handler = WorkspaceFs::new(&root, bufs.clone());
    let (client, rx) = AcpClient::start(transport, Box::new(handler)).expect("start");
    let info = client.info();
    assert_eq!(info.name, "fake-agent");
    assert!(info.embedded_context && !info.load_session);
    assert_eq!(info.auth_methods[0].id, "oauth");

    let mcp = [McpServer::Stdio {
        name: "tools".into(),
        command: "mcp-tool".into(),
        args: vec![],
        env: vec![],
    }];
    let session = client.new_session_with(&root, &mcp).unwrap();
    assert_eq!(session.id, SessionId("sess-1".into()));
    assert_eq!(session.modes.len(), 2);
    assert_eq!(session.current_mode.as_deref(), Some("ask"));

    let mut turn = client
        .prompt(
            &session.id,
            &[
                PromptBlock::text("do it"),
                PromptBlock::file_link(&root.join("open.rs")),
                PromptBlock::Resource {
                    uri: "file:///x".into(),
                    text: "body".into(),
                    mime_type: None,
                },
            ],
        )
        .unwrap();
    assert!(matches!(
        client.prompt(&session.id, &[PromptBlock::text("again")]),
        Err(AcpError::TurnInProgress)
    ));

    let mut text = String::new();
    let mut seen_perm = false;
    let mut kinds = Vec::new();
    loop {
        match next_non_protocol(&rx) {
            AgentEvent::MessageChunk { text: t, .. } => text.push_str(&t),
            AgentEvent::ThoughtChunk { text: t, .. } => kinds.push(format!("thought:{t}")),
            AgentEvent::Plan { entries, .. } => {
                assert_eq!(entries[0].status, PlanStatus::InProgress);
                kinds.push("plan".into());
            }
            AgentEvent::Commands { commands, .. } => {
                assert_eq!(commands[0].name, "review");
                kinds.push("commands".into());
            }
            AgentEvent::ModeChanged { mode_id, .. } => kinds.push(format!("mode:{mode_id}")),
            AgentEvent::ToolCall { call, .. } => {
                assert_eq!(
                    (call.kind, call.status),
                    (ToolKind::Edit, ToolStatus::Pending)
                );
                kinds.push("tool".into());
            }
            AgentEvent::Permission(p) => {
                assert_eq!(p.options[0].kind, PermissionKind::AllowOnce);
                assert_eq!(p.tool.title.as_deref(), Some("Write open.rs"));
                assert!(p.reply.select("yes"));
                assert!(!p.reply.select("no"), "first answer wins");
                seen_perm = true;
            }
            AgentEvent::ToolCallUpdate { patch, .. } => {
                assert_eq!(patch.status, Some(ToolStatus::Completed));
                kinds.push("tool-update".into());
            }
            AgentEvent::TurnFinished { stop, turn: t, .. } => {
                assert_eq!(stop, StopReason::EndTurn);
                assert_eq!(t, turn.turn);
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(text, "Hello!");
    assert!(seen_perm);
    assert_eq!(
        kinds,
        [
            "thought:hmm",
            "plan",
            "commands",
            "mode:code",
            "tool",
            "tool-update"
        ]
    );
    assert_eq!(turn.wait_timeout(T), Some(Ok(StopReason::EndTurn)));
    assert!(!client.turn_active(&session.id));
    assert_eq!(bufs.0.lock().unwrap()[&root.join("w.rs")], "agent wrote");
    assert!(!root.join("open.rs").exists());
    drop(client);
    agent.join().unwrap();
}

#[test]
fn protocol_errors_are_reported_not_fatal() {
    let (transport, agent) = connect(|mut a| {
        a.handshake(json!({}), json!([]));
        a.raw("garbage {\n");
        let ns = a.recv();
        a.reply(&ns, json!({"sessionId": "s"}));
        while a.try_recv().is_some() {}
    });
    let (client, rx) = AcpClient::start(transport, Box::new(NoHandler)).unwrap();
    let s = client.new_session(Path::new("/tmp")).unwrap();
    assert_eq!(s.id.0, "s");
    let mut got_error = false;
    while let Ok(ev) = rx.try_recv() {
        got_error |= matches!(ev, AgentEvent::ProtocolError { .. });
    }
    assert!(got_error);
    drop(client);
    agent.join().unwrap();
}

#[test]
fn out_of_order_responses_reach_the_right_caller() {
    let (transport, agent) = connect(|mut a| {
        a.handshake(json!({}), json!([]));
        let first = a.recv();
        let second = a.recv();
        // answer in reverse order, deriving the session id from cwd
        for req in [&second, &first] {
            let cwd = req["params"]["cwd"].as_str().unwrap().to_owned();
            a.reply(req, json!({"sessionId": format!("sid-for-{cwd}")}));
        }
        while a.try_recv().is_some() {}
    });
    let (client, _rx) = AcpClient::start(transport, Box::new(NoHandler)).unwrap();
    let client = Arc::new(client);
    let mut hs = Vec::new();
    for cwd in ["/a", "/b"] {
        let c = Arc::clone(&client);
        hs.push(thread::spawn(move || {
            (cwd, c.new_session(Path::new(cwd)).unwrap())
        }));
        thread::sleep(Duration::from_millis(50)); // make the arrival order deterministic
    }
    for h in hs {
        let (cwd, s) = h.join().unwrap();
        // The client absolutizes cwd (Windows prefixes a drive letter).
        let abs = std::path::absolute(cwd).unwrap();
        assert_eq!(s.id.0, format!("sid-for-{}", abs.display()));
    }
    drop(client);
    agent.join().unwrap();
}

#[test]
fn cancel_answers_pending_permission_and_ends_cancelled() {
    let (transport, agent) = connect(|mut a| {
        a.handshake(json!({}), json!([]));
        let ns = a.recv();
        a.reply(&ns, json!({"sessionId": "s"}));
        let pr = a.recv();
        a.send(
            json!({"jsonrpc":"2.0","id":7,"method":"session/request_permission",
            "params":{"sessionId":"s","toolCall":{"toolCallId":"t"},
                      "options":[{"optionId":"o","name":"ok","kind":"allow_once"}]}}),
        );
        // protocol: client MUST answer the pending request with `cancelled`
        let cancelled = a.recv();
        assert_eq!(cancelled["id"], 7);
        assert_eq!(
            cancelled["result"]["outcome"],
            json!({"outcome": "cancelled"})
        );
        let note = a.recv();
        assert_eq!(note["method"], "session/cancel");
        assert!(note.get("id").is_none(), "cancel is a notification");
        assert_eq!(note["params"]["sessionId"], "s");
        a.reply(&pr, json!({"stopReason": "cancelled"}));
        while a.try_recv().is_some() {}
    });
    let (client, rx) = AcpClient::start(transport, Box::new(NoHandler)).unwrap();
    let s = client.new_session(Path::new("/tmp")).unwrap();
    let mut turn = client.prompt(&s.id, &[PromptBlock::text("go")]).unwrap();
    let AgentEvent::Permission(p) = next_non_protocol(&rx) else {
        panic!("want permission")
    };
    client.cancel(&s.id).unwrap();
    assert!(p.reply.is_answered(), "cancel answered the permission");
    assert!(!p.reply.select("o"));
    assert!(matches!(
        next_non_protocol(&rx),
        AgentEvent::TurnFinished {
            stop: StopReason::Cancelled,
            ..
        }
    ));
    assert_eq!(turn.wait_timeout(T), Some(Ok(StopReason::Cancelled)));
    drop(client);
    agent.join().unwrap();
}

#[test]
fn dropped_permission_reply_cancels() {
    let (transport, agent) = connect(|mut a| {
        a.handshake(json!({}), json!([]));
        a.send(
            json!({"jsonrpc":"2.0","id":1,"method":"session/request_permission",
            "params":{"sessionId":"s","toolCall":{"toolCallId":"t"},"options":[]}}),
        );
        let r = a.recv();
        assert_eq!(r["result"]["outcome"]["outcome"], "cancelled");
        while a.try_recv().is_some() {}
    });
    let (client, rx) = AcpClient::start(transport, Box::new(NoHandler)).unwrap();
    let ev = next_non_protocol(&rx);
    drop(ev); // UI throws the request away
    thread::sleep(Duration::from_millis(100));
    drop(client);
    agent.join().unwrap();
}

#[test]
fn agent_crash_mid_turn() {
    let (transport, agent) = connect(|mut a| {
        a.handshake(json!({}), json!([]));
        let ns = a.recv();
        a.reply(&ns, json!({"sessionId": "s"}));
        let _pr = a.recv();
        a.chunk("s", "partial");
        // drop everything: simulates the process dying
    });
    let (client, rx) = AcpClient::start(transport, Box::new(NoHandler)).unwrap();
    let s = client.new_session(Path::new("/tmp")).unwrap();
    let turn = client.prompt(&s.id, &[PromptBlock::text("go")]).unwrap();
    assert!(matches!(
        next_non_protocol(&rx),
        AgentEvent::MessageChunk { .. }
    ));
    assert!(matches!(
        next_non_protocol(&rx),
        AgentEvent::TurnFailed { .. }
    ));
    assert!(matches!(next_non_protocol(&rx), AgentEvent::Exited { .. }));
    assert_eq!(turn.wait(), Err(AcpError::Closed));
    assert!(!client.is_alive());
    assert!(matches!(
        client.prompt(&s.id, &[PromptBlock::text("x")]),
        Err(AcpError::Closed)
    ));
    assert!(matches!(
        client.new_session(Path::new("/tmp")),
        Err(AcpError::Closed)
    ));
    assert!(client.cancel(&s.id).is_err());
    agent.join().unwrap();
}

#[test]
fn handshake_failures() {
    // wrong protocol version
    let (t, h) = connect(|mut a| {
        let init = a.recv();
        a.reply(&init, json!({"protocolVersion": 7}));
        while a.try_recv().is_some() {}
    });
    assert!(matches!(
        AcpClient::start(t, Box::new(NoHandler)),
        Err(AcpError::Protocol(_))
    ));
    h.join().unwrap();
    // agent dies before answering
    let (t, h) = connect(|mut a| {
        let _ = a.recv();
    });
    assert!(matches!(
        AcpClient::start(t, Box::new(NoHandler)),
        Err(AcpError::Closed)
    ));
    h.join().unwrap();
    // agent answers initialize with an error
    let (t, h) = connect(|mut a| {
        let init = a.recv();
        a.reply_err(&init, -32603, "boom");
        while a.try_recv().is_some() {}
    });
    assert!(matches!(
        AcpClient::start(t, Box::new(NoHandler)),
        Err(AcpError::Rpc { code: -32603, .. })
    ));
    h.join().unwrap();
}

#[test]
fn auth_required_and_authenticate() {
    let (transport, agent) = connect(|mut a| {
        a.handshake(json!({}), json!([{"id": "key", "name": "API key"}]));
        let ns = a.recv();
        a.reply_err(&ns, -32000, "auth required");
        let au = a.recv();
        assert_eq!(au["method"], "authenticate");
        assert_eq!(au["params"]["methodId"], "key");
        a.reply(&au, json!({}));
        let ns = a.recv();
        a.reply(&ns, json!({"sessionId": "ok"}));
        while a.try_recv().is_some() {}
    });
    let (client, _rx) = AcpClient::start(transport, Box::new(NoHandler)).unwrap();
    let err = client.new_session(Path::new("/tmp")).unwrap_err();
    assert!(err.info().auth_required);
    client
        .authenticate(&client.info().auth_methods[0].id)
        .unwrap();
    assert_eq!(client.new_session(Path::new("/tmp")).unwrap().id.0, "ok");
    // session/load without capability is refused locally
    assert!(matches!(
        client.load_session(&SessionId("x".into()), Path::new("/tmp"), &[]),
        Err(AcpError::Unsupported(_))
    ));
    drop(client);
    agent.join().unwrap();
}

#[test]
fn resource_blocks_degrade_without_embedded_context() {
    let (transport, agent) = connect(|mut a| {
        a.handshake(json!({"promptCapabilities": {}}), json!([]));
        let ns = a.recv();
        a.reply(&ns, json!({"sessionId": "s"}));
        let pr = a.recv();
        let b = &pr["params"]["prompt"][0];
        assert_eq!(b["type"], "text");
        let t = b["text"].as_str().unwrap();
        assert!(
            t.contains("file:///a.rs") && t.contains("fn main() {}") && t.contains("```"),
            "{t}"
        );
        a.reply(&pr, json!({"stopReason": "end_turn"}));
        while a.try_recv().is_some() {}
    });
    let (client, _rx) = AcpClient::start(transport, Box::new(NoHandler)).unwrap();
    let s = client.new_session(Path::new("/tmp")).unwrap();
    let turn = client
        .prompt(
            &s.id,
            &[PromptBlock::Resource {
                uri: "file:///a.rs".into(),
                text: "fn main() {}".into(),
                mime_type: None,
            }],
        )
        .unwrap();
    assert_eq!(turn.wait(), Ok(StopReason::EndTurn));
    drop(client);
    agent.join().unwrap();
}

fn pump(
    ctl: &mut dyn AgentController,
    id: skink_ai::AgentId,
    events: &mut Vec<AgentEvent>,
    until: &dyn Fn(&AgentEvent) -> bool,
) {
    let end = std::time::Instant::now() + T;
    loop {
        for (aid, ev) in ctl.poll_events() {
            assert_eq!(aid, id);
            let stop = until(&ev);
            events.push(ev);
            if stop {
                return;
            }
        }
        assert!(
            std::time::Instant::now() < end,
            "timeout; events so far: {events:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn local_agents_controller_end_to_end() {
    let (transport, agent) = connect(|mut a| {
        a.handshake(json!({"loadSession": true}), json!([]));
        let ns = a.recv();
        a.reply(&ns, json!({"sessionId": "S"}));
        let pr = a.recv();
        a.chunk("S", "hi");
        a.reply(&pr, json!({"stopReason": "end_turn"}));
        while a.try_recv().is_some() {}
    });
    let woke = Arc::new(Mutex::new(0u32));
    let w2 = Arc::clone(&woke);
    let mut agents = LocalAgents::for_workspace(
        std::env::temp_dir(),
        Arc::new(Bufs(Mutex::new(HashMap::new()))),
    )
    .with_waker(Arc::new(move || *w2.lock().unwrap() += 1));
    let ctl: &mut dyn AgentController = &mut agents;
    assert_eq!(ctl.backend(), skink_core::Backend::Local);
    let id = agents.attach("fake", transport);
    let ctl: &mut dyn AgentController = &mut agents;

    let mut events = Vec::new();
    pump(ctl, id, &mut events, &|e| {
        matches!(e, AgentEvent::Ready { .. })
    });
    let st = ctl.agents();
    assert_eq!(
        (st[0].state, st[0].label.as_str()),
        (AgentState::Ready, "fake")
    );
    assert!(st[0].info.as_ref().unwrap().load_session);

    let req = ctl.new_session(id, Path::new("/tmp")).unwrap();
    pump(ctl, id, &mut events, &|e| {
        matches!(e, AgentEvent::SessionStarted { .. })
    });
    let Some(AgentEvent::SessionStarted { request, session }) = events.last().cloned() else {
        panic!()
    };
    assert_eq!(request, req);
    let turn = ctl
        .send_prompt(id, &session.id, vec![PromptBlock::text("yo")])
        .unwrap();
    pump(ctl, id, &mut events, &|e| {
        matches!(e, AgentEvent::TurnFinished { .. })
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::MessageChunk { text, .. } if text == "hi"))
    );
    assert!(matches!(events.last(), Some(AgentEvent::TurnFinished { turn: t, .. }) if *t == turn));
    assert!(*woke.lock().unwrap() >= 3);

    ctl.stop_agent(id);
    assert!(ctl.agents().is_empty());
    assert!(matches!(
        ctl.cancel(id, &session.id),
        Err(skink_ai::AgentError::UnknownAgent(_))
    ));
    agent.join().unwrap();
}

#[test]
fn local_agents_reports_failed_handshake_and_spawn_errors() {
    let (transport, agent) = connect(|mut a| {
        let _ = a.recv(); // die without answering
    });
    let mut agents = LocalAgents::for_workspace(
        std::env::temp_dir(),
        Arc::new(Bufs(Mutex::new(HashMap::new()))),
    );
    let id = agents.attach("dead", transport);
    let end = std::time::Instant::now() + T;
    let mut failed = false;
    while !failed && std::time::Instant::now() < end {
        failed = agents
            .poll_events()
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::StartFailed { .. }));
        thread::sleep(Duration::from_millis(5));
    }
    assert!(failed);
    assert_eq!(agents.agents()[0].state, AgentState::Failed);
    assert!(agents.new_session(id, Path::new("/tmp")).is_err());
    agent.join().unwrap();
    assert!(matches!(
        agents.start_agent(
            "nope",
            skink_ai::AgentSpec::new("definitely-not-a-binary-xyz")
        ),
        Err(skink_ai::AgentError::Spawn(_))
    ));
}

#[cfg(unix)]
#[test]
fn real_process_over_stdio() {
    // A "agent" in sh: answers initialize (id 1) and exits when stdin closes.
    let script = r#"read line; printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"sh-agent","version":"0"}}}'; cat >/dev/null; echo bye >&2"#;
    let spec = skink_ai::AgentSpec::new("sh").args(["-c", script]);
    let transport = skink_ai::acp::spawn(&spec).unwrap();
    let (client, rx) = AcpClient::start(transport, Box::new(NoHandler)).unwrap();
    assert_eq!(client.info().name, "sh-agent");
    client.shutdown();
    let AgentEvent::Exited {
        status,
        stderr_tail,
    } = next_non_protocol(&rx)
    else {
        panic!()
    };
    assert_eq!(status, Some(0));
    assert!(stderr_tail.iter().any(|l| l == "bye") || stderr_tail.is_empty());
}

#[test]
fn prompter_rewrite_round_trip_through_a_fake_agent() {
    use skink_ai::prompter::{
        CharPos, CharRange, ContextConfig, DiskFiles, FileSnapshot, ReplyBuffer, RewriteRequest,
        build_rewrite,
    };
    let root = temp_root("rewrite");
    let file = FileSnapshot {
        path: root.join("m.rs"),
        text: "fn main() {\n    let x = 1;\n}\n".into(),
    };
    let req = RewriteRequest::new(
        "rename x to count",
        file.clone(),
        CharRange::new(CharPos::new(1, 2), CharPos::new(1, 6)),
    );
    let built = build_rewrite(&ContextConfig::new(&root), &DiskFiles, &req, &[]);

    let sent = built.prompt.text.clone();
    let (transport, agent) = connect(move |mut a| {
        a.handshake(json!({}), json!([]));
        let ns = a.recv();
        a.reply(&ns, json!({"sessionId": "s"}));
        let pr = a.recv();
        assert_eq!(pr["params"]["prompt"][0]["text"], sent.as_str());
        a.chunk("s", "Sure, here you go:\n\n```rust\n    let co");
        a.chunk("s", "unt = 1;\n```\n");
        a.reply(&pr, json!({"stopReason": "end_turn"}));
        while a.try_recv().is_some() {}
    });
    let (client, rx) = AcpClient::start(transport, Box::new(NoHandler)).unwrap();
    let s = client.new_session(&root).unwrap();
    client
        .prompt(&s.id, &built.prompt.clone().into_blocks())
        .unwrap();
    let mut buf = ReplyBuffer::default();
    loop {
        match next_non_protocol(&rx) {
            AgentEvent::MessageChunk { text, .. } => buf.push(&text),
            AgentEvent::TurnFinished { .. } => break,
            other => panic!("{other:?}"),
        }
    }
    let edit = built.finish(&buf.take()).unwrap();
    assert_eq!(edit.diff.stats().blocks, 1);
    assert_eq!(
        edit.accept_all_to(&file.text).unwrap(),
        "fn main() {\n    let count = 1;\n}\n"
    );
    drop(client);
    agent.join().unwrap();
}
