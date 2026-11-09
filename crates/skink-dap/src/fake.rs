//! An in-process fake debug adapter for tests: it "runs" a program of
//! `lines` lines in one file, stopping at breakpoints, stepping one line at
//! a time, with a small variable tree and `evaluate` that echoes. Commands
//! it received are logged.

use crate::Wake;
use crate::client::Client;
use serde_json::{Value, json};
use skink_lsp::transport::{frame, read_message};
use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub type Log = Arc<Mutex<Vec<String>>>;

#[derive(Clone)]
pub struct Program {
    pub path: PathBuf,
    pub lines: usize,
    /// Answer `launch` with an error.
    pub fail_launch: bool,
    /// Ask the client to `runInTerminal` before running.
    pub run_in_terminal: bool,
}

/// Start the fake; returns a client connected to it and its command log.
pub fn start(p: Program, wake: Wake) -> (Client, Log) {
    let (from_client, to_fake) = std::io::pipe().expect("pipe");
    let (from_fake, to_client) = std::io::pipe().expect("pipe");
    let log: Log = Arc::default();
    let l = log.clone();
    std::thread::spawn(move || run(p, BufReader::new(from_client), to_client, l));
    let client = Client::from_io(Box::new(from_fake), Box::new(to_fake), wake).expect("client");
    (client, log)
}

struct Fake<W: Write> {
    out: W,
    seq: i64,
    p: Program,
    breaks: Vec<usize>,
    /// 1-based line it is stopped at.
    at: usize,
}

impl<W: Write> Fake<W> {
    fn send(&mut self, mut v: Value) {
        self.seq += 1;
        v["seq"] = json!(self.seq);
        let _ = self.out.write_all(&frame(v.to_string().as_bytes()));
        let _ = self.out.flush();
    }
    fn reply(&mut self, req: &Value, body: Value) {
        self.send(
            json!({"type": "response", "request_seq": req["seq"], "success": true,
            "command": req["command"], "body": body}),
        );
    }
    fn error(&mut self, req: &Value, msg: &str) {
        self.send(
            json!({"type": "response", "request_seq": req["seq"], "success": false,
            "command": req["command"], "message": msg}),
        );
    }
    fn event(&mut self, event: &str, body: Value) {
        self.send(json!({"type": "event", "event": event, "body": body}));
    }
    fn stop_at(&mut self, line: usize, reason: &str) {
        self.at = line;
        self.event(
            "stopped",
            json!({"reason": reason, "threadId": 1, "allThreadsStopped": true}),
        );
    }
    /// Run from after `from` to the next breakpoint, or to the end.
    fn run_on(&mut self, from: usize) {
        self.event("continued", json!({"threadId": 1}));
        match self.breaks.iter().copied().find(|&b| b > from) {
            Some(b) => self.stop_at(b, "breakpoint"),
            None => {
                self.event(
                    "output",
                    json!({"category": "stdout", "output": "hello from fake\n"}),
                );
                self.event("exited", json!({"exitCode": 0}));
                self.event("terminated", json!({}));
            }
        }
    }
}

fn run(p: Program, mut r: BufReader<std::io::PipeReader>, w: std::io::PipeWriter, log: Log) {
    let mut f = Fake {
        out: w,
        seq: 0,
        p,
        breaks: Vec::new(),
        at: 0,
    };
    while let Ok(body) = read_message(&mut r) {
        let Ok(req) = serde_json::from_slice::<Value>(&body) else {
            continue;
        };
        let cmd = req["command"].as_str().unwrap_or("").to_owned();
        if req["type"] == "response" {
            log.lock().unwrap().push(format!("response:{cmd}"));
            if cmd == "runInTerminal" {
                f.event(
                    "output",
                    json!({"category": "console", "output": "debuggee started\n"}),
                );
            }
            continue;
        }
        log.lock().unwrap().push(cmd.clone());
        match cmd.as_str() {
            "initialize" => f.reply(
                &req,
                json!({"supportsConfigurationDoneRequest": true,
                    "supportsConditionalBreakpoints": true, "supportsLogPoints": true,
                    "supportsEvaluateForHovers": true, "supportsTerminateRequest": true}),
            ),
            "launch" if f.p.fail_launch => f.error(&req, "program not found"),
            "launch" => {
                if f.p.run_in_terminal {
                    f.send(json!({"type": "request", "command": "runInTerminal",
                        "arguments": {"kind": "integrated", "title": "fake",
                            "cwd": "/", "args": ["sh", "-c", "true"], "env": {"A": "1"}}}));
                }
                f.event("initialized", json!({}));
                // Answered after configurationDone, like many adapters.
                f.reply(&req, json!({}));
            }
            "setBreakpoints" => {
                let lines: Vec<usize> = req["arguments"]["breakpoints"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|b| b["line"].as_u64().map(|l| l as usize))
                            .collect()
                    })
                    .unwrap_or_default();
                let lines_total = f.p.lines;
                let bps: Vec<Value> = lines
                    .iter()
                    .enumerate()
                    .map(|(i, &l)| {
                        if l <= lines_total {
                            json!({"id": i + 1, "verified": true, "line": l})
                        } else {
                            json!({"id": i + 1, "verified": false, "message": "no code here"})
                        }
                    })
                    .collect();
                f.breaks = lines.into_iter().filter(|&l| l <= lines_total).collect();
                f.breaks.sort_unstable();
                f.reply(&req, json!({ "breakpoints": bps }));
            }
            "configurationDone" => {
                f.reply(&req, json!({}));
                f.run_on(0);
            }
            "threads" => f.reply(&req, json!({"threads": [{"id": 1, "name": "main"}]})),
            "stackTrace" => {
                let path = f.p.path.to_string_lossy().into_owned();
                let at = f.at;
                f.reply(
                    &req,
                    json!({"stackFrames": [
                        {"id": 100, "name": "inner", "line": at, "column": 1,
                            "source": {"name": "main", "path": path}},
                        {"id": 101, "name": "main", "line": 1, "column": 1,
                            "source": {"name": "main", "path": path}}],
                        "totalFrames": 2}),
                );
            }
            "scopes" => f.reply(
                &req,
                json!({"scopes": [
                    {"name": "Locals", "variablesReference": 10, "expensive": false},
                    {"name": "Globals", "variablesReference": 20, "expensive": true}]}),
            ),
            "variables" => {
                let at = f.at;
                let vars = match req["arguments"]["variablesReference"].as_i64() {
                    Some(10) => json!([
                        {"name": "count", "value": at.to_string(), "type": "int", "variablesReference": 0},
                        {"name": "point", "value": "{x, y}", "variablesReference": 11}]),
                    Some(11) => json!([
                        {"name": "x", "value": "1", "variablesReference": 0},
                        {"name": "y", "value": "2", "variablesReference": 0}]),
                    _ => json!([{"name": "g", "value": "0", "variablesReference": 0}]),
                };
                f.reply(&req, json!({ "variables": vars }));
            }
            "evaluate" => {
                let e = req["arguments"]["expression"].as_str().unwrap_or("");
                if e == "bad" {
                    f.error(&req, "no symbol bad");
                } else {
                    let v = format!("{e} = {}", f.at);
                    f.reply(&req, json!({"result": v, "variablesReference": 0}));
                }
            }
            "continue" => {
                f.reply(&req, json!({"allThreadsContinued": true}));
                let at = f.at;
                f.run_on(at);
            }
            "next" | "stepIn" | "stepOut" => {
                f.reply(&req, json!({}));
                let next = f.at + 1;
                if next > f.p.lines {
                    f.run_on(f.p.lines);
                } else {
                    f.stop_at(next, "step");
                }
            }
            "pause" => {
                f.reply(&req, json!({}));
                let at = f.at.max(1);
                f.stop_at(at, "pause");
            }
            "terminate" => {
                f.reply(&req, json!({}));
                f.event("exited", json!({"exitCode": 3}));
                f.event("terminated", json!({}));
            }
            "disconnect" => {
                f.reply(&req, json!({}));
                return;
            }
            _ => f.error(&req, "unknown request"),
        }
    }
}
