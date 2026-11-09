//! The settings health check against a scripted in-process ACP agent.

use serde_json::{Value, json};
use skink_ai::acp::{Transport, memory_pipe};
use skink_ai::check::{
    CheckFailure, CheckOutcome, CheckRequest, claude_project_dir_name, run_check,
};
use skink_ai::{Effort, EffortOutcome, EffortRequest, ModelInfo, ModelPickError, pick_model};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How the fake agent behaves.
#[derive(Clone)]
struct Fake {
    name: &'static str,
    /// `agentCapabilities` in the initialize answer.
    caps: Value,
    /// Extra fields of the session/new result (models, configOptions).
    session: Value,
    /// `session/new` fails with this JSON-RPC error.
    session_error: Option<Value>,
    /// Text streamed for the prompt (`None`: never answer).
    answer: Option<&'static str>,
    /// Write a transcript here on session/new, like Claude Code does.
    transcript_dir: Option<PathBuf>,
}

impl Default for Fake {
    fn default() -> Self {
        Self {
            name: "fake",
            caps: json!({}),
            session: json!({}),
            session_error: None,
            answer: Some("OK"),
            transcript_dir: None,
        }
    }
}

type Log = Arc<Mutex<Vec<Value>>>;

fn start(f: Fake) -> (Transport, Log) {
    let (c2a_w, c2a_r) = memory_pipe();
    let (a2c_w, a2c_r) = memory_pipe();
    let log: Log = Arc::default();
    let l = log.clone();
    std::thread::spawn(move || {
        let mut rd = BufReader::new(c2a_r);
        let mut wr = a2c_w;
        let mut line = String::new();
        let send = |wr: &mut skink_ai::acp::transport::PipeWriter, v: Value| {
            let _ = wr.write_all(format!("{v}\n").as_bytes());
        };
        loop {
            line.clear();
            if rd.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let m: Value = serde_json::from_str(&line).unwrap();
            l.lock().unwrap().push(m.clone());
            let id = m["id"].clone();
            let ok = |r: Value| json!({"jsonrpc": "2.0", "id": id, "result": r});
            match m["method"].as_str().unwrap_or("") {
                "initialize" => send(
                    &mut wr,
                    ok(json!({"protocolVersion": 1, "agentCapabilities": f.caps,
                              "agentInfo": {"name": f.name, "version": "1"}})),
                ),
                "session/new" => {
                    if let Some(e) = &f.session_error {
                        send(&mut wr, json!({"jsonrpc": "2.0", "id": id, "error": e}));
                        continue;
                    }
                    if let Some(dir) = &f.transcript_dir {
                        let cwd = m["params"]["cwd"].as_str().unwrap();
                        let d = dir.join("projects").join(claude_project_dir_name(cwd));
                        std::fs::create_dir_all(&d).unwrap();
                        std::fs::write(d.join("s1.jsonl"), "{}\n").unwrap();
                    }
                    let mut r = f.session.clone();
                    r["sessionId"] = json!("s1");
                    send(&mut wr, ok(r));
                }
                "session/set_model"
                | "session/set_config_option"
                | "session/close"
                | "session/delete" => send(&mut wr, ok(json!({}))),
                "session/prompt" => {
                    let Some(text) = f.answer else { continue };
                    send(
                        &mut wr,
                        json!({"jsonrpc": "2.0", "method": "session/update", "params": {
                            "sessionId": "s1", "update": {"sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": text}}}}),
                    );
                    send(&mut wr, ok(json!({"stopReason": "end_turn"})));
                }
                _ => {}
            }
        }
    });
    (Transport::new(a2c_r, c2a_w), log)
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skink-check-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn request(work: &Path, model: Option<&str>, effort: EffortRequest) -> CheckRequest {
    std::fs::create_dir_all(work).unwrap();
    CheckRequest {
        model: model.map(str::to_owned),
        effort,
        workdir: work.to_path_buf(),
        timeout: Duration::from_secs(5),
        claude_config_dir: None,
        cancel: Arc::new(AtomicBool::new(false)),
    }
}

fn methods(log: &Log) -> Vec<String> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|m| m["method"].as_str().map(str::to_owned))
        .collect()
}

fn models() -> Value {
    json!({"models": {"currentModelId": "default", "availableModels": [
        {"modelId": "default", "name": "Default"},
        {"modelId": "claude-opus-5-5", "name": "Opus 5.5"},
        {"modelId": "claude-opus-4-1", "name": "Opus 4.1"},
        {"modelId": "claude-sonnet-5-5", "name": "Sonnet 5.5"}]}})
}

#[test]
fn passes_with_loose_model_and_effort_through_config_option_then_closes() {
    let root = temp("pass");
    let mut session = models();
    session["configOptions"] = json!([{"id": "reasoning", "name": "Reasoning", "type": "select",
        "category": "thought_level", "currentValue": "medium",
        "options": [{"value": "low", "name": "Low"}, {"value": "medium", "name": "Medium"},
                    {"value": "xhigh", "name": "Extra high"}]}]);
    let (t, log) = start(Fake {
        caps: json!({"sessionCapabilities": {"close": {}}}),
        session,
        ..Fake::default()
    });
    let work = root.join("work");
    let r = run_check(
        t,
        &request(&work, Some("opus5.5"), EffortRequest::Level(Effort::Max)),
    );
    assert_eq!(
        r.outcome,
        CheckOutcome::Passed {
            model: Some("claude-opus-5-5".into()),
            effort: EffortOutcome::ConfigOption {
                config_id: "reasoning".into(),
                value: "xhigh".into()
            },
            reply: "OK".into()
        }
    );
    assert!(r.session_removed);
    assert!(!work.exists(), "throwaway dir removed");
    let m = methods(&log);
    assert!(m.contains(&"session/set_config_option".to_owned()), "{m:?}");
    assert!(m.contains(&"session/close".to_owned()), "{m:?}");
    // The session ran in the throwaway dir, never the project.
    let cwd = log
        .lock()
        .unwrap()
        .iter()
        .find(|v| v["method"] == "session/new")
        .unwrap()["params"]["cwd"]
        .clone();
    assert!(cwd.as_str().unwrap().ends_with("work"), "{cwd}");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn claude_code_gets_a_thinking_budget_and_its_transcript_is_deleted() {
    let root = temp("claude");
    let config = root.join("claude-config");
    // Another chat of the user's that must survive.
    let other = config.join("projects").join("-home-me-proj");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("keep.jsonl"), "{}\n").unwrap();
    let (t, log) = start(Fake {
        name: "@zed-industries/claude-code-acp",
        session: models(),
        transcript_dir: Some(config.clone()),
        ..Fake::default()
    });
    let work = root.join("work");
    let mut req = request(&work, None, EffortRequest::Level(Effort::High));
    req.claude_config_dir = Some(config.clone());
    let r = run_check(t, &req);
    assert!(
        matches!(
            r.outcome,
            CheckOutcome::Passed {
                effort: EffortOutcome::ClaudeThinkingTokens(20_000),
                ..
            }
        ),
        "{:?}",
        r.outcome
    );
    let new = log
        .lock()
        .unwrap()
        .iter()
        .find(|v| v["method"] == "session/new")
        .unwrap()
        .clone();
    assert_eq!(
        new["params"]["_meta"]["claudeCode"]["options"]["maxThinkingTokens"],
        json!(20_000)
    );
    let transcript_folder =
        config
            .join("projects")
            .join(claude_project_dir_name(&skink_ai::acp::abs_path_string(
                &work,
            )));
    assert!(
        !transcript_folder.exists(),
        "transcript and its folder removed"
    );
    assert!(
        r.removed.contains(&transcript_folder.join("s1.jsonl")),
        "{:?}",
        r.removed
    );
    assert!(other.join("keep.jsonl").exists(), "other chats untouched");
    assert!(
        !r.session_removed,
        "claude-code-acp 0.16 has no session/close"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn failures_are_named() {
    let root = temp("fail");
    // Login needed.
    let (t, _) = start(Fake {
        session_error: Some(json!({"code": -32000, "message": "Authentication required"})),
        ..Fake::default()
    });
    let r = run_check(
        t,
        &request(&root.join("a"), None, EffortRequest::AgentDefault),
    );
    assert!(
        matches!(
            r.outcome,
            CheckOutcome::Failed(CheckFailure::LoginNeeded(_))
        ),
        "{:?}",
        r.outcome
    );
    assert!(!root.join("a").exists());
    // Ambiguous and unknown models list the candidates.
    let (t, _) = start(Fake {
        session: models(),
        ..Fake::default()
    });
    let r = run_check(
        t,
        &request(&root.join("b"), Some("opus"), EffortRequest::AgentDefault),
    );
    let CheckOutcome::Failed(CheckFailure::Model(m)) = &r.outcome else {
        panic!("{:?}", r.outcome)
    };
    assert!(
        m.contains("claude-opus-5-5") && m.contains("claude-opus-4-1"),
        "{m}"
    );
    // No answer within the budget.
    let (t, _) = start(Fake {
        answer: None,
        ..Fake::default()
    });
    let mut req = request(&root.join("c"), None, EffortRequest::AgentDefault);
    req.timeout = Duration::from_millis(400);
    let r = run_check(t, &req);
    assert_eq!(
        r.outcome,
        CheckOutcome::Failed(CheckFailure::Timeout("waiting for the reply"))
    );
    assert!(!root.join("c").exists());
    // Effort the agent cannot take is reported, not ignored; the check passes.
    let (t, _) = start(Fake::default());
    let r = run_check(
        t,
        &request(&root.join("d"), None, EffortRequest::Level(Effort::Low)),
    );
    assert!(
        matches!(
            &r.outcome,
            CheckOutcome::Passed {
                effort: EffortOutcome::Unsupported(_),
                ..
            }
        ),
        "{:?}",
        r.outcome
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn model_names_are_matched_loosely() {
    let offered: Vec<ModelInfo> = [
        ("default", "Default", None),
        ("claude-opus-5-5", "Opus 5.5", Some("Most capable")),
        ("claude-opus-4-1", "Opus 4.1", None),
        ("claude-sonnet-5-5", "Sonnet 5.5", None),
    ]
    .into_iter()
    .map(|(id, name, d)| ModelInfo {
        id: id.into(),
        name: name.into(),
        description: d.map(str::to_owned),
    })
    .collect();
    for want in [
        "opus5.5",
        "opus 5.5",
        "Opus 5.5",
        "claude-opus-5-5",
        "OPUS-5-5",
    ] {
        assert_eq!(
            pick_model(want, &offered).as_deref(),
            Ok("claude-opus-5-5"),
            "{want}"
        );
    }
    assert!(matches!(
        pick_model("opus", &offered),
        Err(ModelPickError::Ambiguous { .. })
    ));
    assert!(matches!(
        pick_model("gpt-9", &offered),
        Err(ModelPickError::Unknown { .. })
    ));
}
