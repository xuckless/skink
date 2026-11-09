//! "Is the agent listening?": a one-question health check run when the AI
//! settings change. It starts the agent, opens a session in a throwaway
//! directory, applies the model and effort, asks for a one-word reply and
//! then removes every trace of that chat:
//!
//! - the session on the agent side (`session/close` or `session/delete`,
//!   when the agent advertises one);
//! - Claude Code's transcript, which Zed's adapter (`claude-code-acp`
//!   0.16.x, Apache-2.0) stores at
//!   `$CLAUDE_CONFIG_DIR/projects/<cwd with every non-alphanumeric char
//!   replaced by '-'>/<sessionId>.jsonl` (`CLAUDE_CONFIG_DIR` defaults to
//!   `~/.claude`; see `sessionFilePath` / `encodeProjectPath` in the
//!   adapter). Only that one file is deleted, plus its folder when it is
//!   left empty;
//! - the throwaway directory itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use crate::acp::{AcpClient, AcpError, ClientOptions, NoHandler, Transport};
use crate::agent::{
    AgentEvent, EffortOutcome, EffortRequest, ModelInfo, PromptBlock, SessionId, SessionOptions,
    StopReason, pick_model,
};

/// The question asked. Short, so the check costs next to nothing.
pub const CHECK_PROMPT: &str = "Reply with exactly: OK";

/// What to check.
#[derive(Debug, Clone)]
pub struct CheckRequest {
    /// Model to select (`None`: the agent's default).
    pub model: Option<String>,
    pub effort: EffortRequest,
    /// Throwaway working directory (created by the caller so the agent
    /// process can start in it; removed by the check).
    pub workdir: PathBuf,
    /// Budget for the whole check (a first `npx` run downloads the adapter).
    pub timeout: Duration,
    /// Where Claude Code keeps its chats (`$CLAUDE_CONFIG_DIR` or `~/.claude`).
    pub claude_config_dir: Option<PathBuf>,
    /// Set to stop early (the settings changed again).
    pub cancel: Arc<AtomicBool>,
}

/// Why the check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckFailure {
    /// The handshake failed (not installed, crashed, wrong protocol).
    Start(String),
    /// The agent wants a login first.
    LoginNeeded(String),
    /// `ai.model` matched nothing, or several models.
    Model(String),
    /// No answer within the budget, at this step.
    Timeout(&'static str),
    /// The agent answered with an error.
    Agent(String),
    /// The turn ended without any text.
    NoReply,
    Cancelled,
}

impl std::fmt::Display for CheckFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Start(e) => write!(f, "could not start: {e}"),
            Self::LoginNeeded(e) => write!(
                f,
                "needs a login ({e}); run it once in a terminal tab to sign in"
            ),
            Self::Model(e) => f.write_str(e),
            Self::Timeout(step) => write!(f, "no answer in time ({step})"),
            Self::Agent(e) => f.write_str(e),
            Self::NoReply => f.write_str("answered with nothing (check its login and model)"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

/// The verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Passed {
        /// The model the session ended up with (when the agent reports one).
        model: Option<String>,
        effort: EffortOutcome,
        reply: String,
    },
    Failed(CheckFailure),
}

/// Verdict plus what was cleaned up.
#[derive(Debug, Clone)]
pub struct CheckReport {
    pub outcome: CheckOutcome,
    /// Session closed / deleted on the agent side.
    pub session_removed: bool,
    /// Files and folders deleted afterwards.
    pub removed: Vec<PathBuf>,
    /// Cleanup steps that failed (never fatal).
    pub cleanup_errors: Vec<String>,
}

/// Claude Code's folder name for a project directory (`encodeProjectPath`
/// in claude-code-acp: every char outside `[a-zA-Z0-9]` becomes `-`).
pub fn claude_project_dir_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".claude"))
        })
}

/// A fresh throwaway directory under `base` (`<base>/agent-check/<id>`).
pub fn make_workdir(base: &Path) -> std::io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = base
        .join("agent-check")
        .join(format!("{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Remove a directory from [`make_workdir`] when no check ran in it.
pub fn discard_workdir(dir: &Path) {
    let mut r = CheckReport {
        outcome: CheckOutcome::Failed(CheckFailure::Cancelled),
        session_removed: false,
        removed: Vec::new(),
        cleanup_errors: Vec::new(),
    };
    remove_workdir(dir, &mut r);
}

/// Run the check over `transport` (blocking; call it from a worker).
pub fn run_check(transport: Transport, req: &CheckRequest) -> CheckReport {
    let deadline = Instant::now() + req.timeout;
    let left = || deadline.saturating_duration_since(Instant::now());
    let opts = ClientOptions {
        request_timeout: req.timeout,
        init_timeout: req.timeout,
        ..ClientOptions::default()
    };
    let mut report = CheckReport {
        outcome: CheckOutcome::Failed(CheckFailure::Cancelled),
        session_removed: false,
        removed: Vec::new(),
        cleanup_errors: Vec::new(),
    };
    let started = AcpClient::start_with_options(transport, Box::new(NoHandler), opts);
    let (client, rx) = match started {
        Ok(c) => c,
        Err(e) => {
            report.outcome = CheckOutcome::Failed(CheckFailure::Start(e.to_string()));
            remove_workdir(&req.workdir, &mut report);
            return report;
        }
    };
    let claude = client.info().is_claude_code();
    let mut session: Option<SessionId> = None;
    report.outcome = (|| -> Result<CheckOutcome, CheckFailure> {
        let fail = |e: AcpError| {
            let info = e.info();
            match e {
                AcpError::Timeout => CheckFailure::Timeout("opening a session"),
                _ if info.auth_required => CheckFailure::LoginNeeded(info.message),
                _ => CheckFailure::Agent(match client.stderr_hint() {
                    Some(h) => format!("{} ({h})", info.message),
                    None => info.message,
                }),
            }
        };
        let s = client
            .new_session_opts(&req.workdir, &[], &SessionOptions { effort: req.effort })
            .map_err(fail)?;
        session = Some(s.id.clone());
        let model = choose_model(req.model.as_deref(), &s.models, s.current_model.clone())?;
        if let Some(m) = &model
            && s.current_model.as_deref() != Some(m.as_str())
        {
            client.set_model(&s.id, m).map_err(fail)?;
        }
        if req.cancel.load(Ordering::Relaxed) {
            return Err(CheckFailure::Cancelled);
        }
        let mut turn = client
            .prompt(&s.id, &[PromptBlock::text(CHECK_PROMPT)])
            .map_err(fail)?;
        let mut reply = String::new();
        loop {
            if req.cancel.load(Ordering::Relaxed) {
                let _ = client.cancel(&s.id);
                return Err(CheckFailure::Cancelled);
            }
            if left().is_zero() {
                let _ = client.cancel(&s.id);
                return Err(CheckFailure::Timeout("waiting for the reply"));
            }
            match rx.recv_timeout(left().min(Duration::from_millis(100))) {
                Ok(AgentEvent::MessageChunk { text, .. }) => reply.push_str(&text),
                // Nothing to approve in a health check.
                Ok(AgentEvent::Permission(p)) => {
                    p.reply.cancel();
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {}
            }
            if let Some(done) = turn.try_result() {
                // Late chunks are already queued: drain them.
                while let Ok(ev) = rx.try_recv() {
                    if let AgentEvent::MessageChunk { text, .. } = ev {
                        reply.push_str(&text);
                    }
                }
                match done {
                    Ok(StopReason::Cancelled) => return Err(CheckFailure::Cancelled),
                    Ok(_) if reply.trim().is_empty() => return Err(CheckFailure::NoReply),
                    Ok(_) => {
                        return Ok(CheckOutcome::Passed {
                            model: model.or(s.current_model),
                            effort: s.effort,
                            reply: reply.trim().to_owned(),
                        });
                    }
                    Err(e) => return Err(fail(e)),
                }
            }
        }
    })()
    .unwrap_or_else(CheckOutcome::Failed);
    if let Some(s) = &session {
        match client.close_session(s) {
            Ok(removed) => report.session_removed = removed,
            Err(e) => report
                .cleanup_errors
                .push(format!("closing the session: {e}")),
        }
    }
    client.shutdown();
    if let (true, Some(s), Some(dir)) = (claude, &session, &req.claude_config_dir) {
        remove_claude_transcript(dir, &req.workdir, s, &mut report);
    }
    remove_workdir(&req.workdir, &mut report);
    report
}

/// The model to select: `None` keeps the agent's default.
fn choose_model(
    want: Option<&str>,
    offered: &[ModelInfo],
    current: Option<String>,
) -> Result<Option<String>, CheckFailure> {
    let Some(want) = want else {
        return Ok(None);
    };
    if offered.is_empty() {
        // The agent does not let clients choose; nothing to verify.
        return Ok(current);
    }
    pick_model(want, offered)
        .map(Some)
        .map_err(|e| CheckFailure::Model(e.to_string()))
}

/// Delete exactly `<config>/projects/<encoded workdir>/<session>.jsonl`, and
/// that folder if nothing else is in it. Nothing else under the config dir is
/// touched.
fn remove_claude_transcript(
    config: &Path,
    workdir: &Path,
    session: &SessionId,
    report: &mut CheckReport,
) {
    // The adapter encodes the cwd string it was given (`abs(cwd)` on our side).
    let cwd = crate::acp::abs_path_string(workdir);
    let folder = config.join("projects").join(claude_project_dir_name(&cwd));
    let file = folder.join(format!("{}.jsonl", session.0));
    match std::fs::remove_file(&file) {
        Ok(()) => report.removed.push(file),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => report
            .cleanup_errors
            .push(format!("{}: {e}", file.display())),
    }
    if std::fs::remove_dir(&folder).is_ok() {
        report.removed.push(folder);
    }
}

fn remove_workdir(dir: &Path, report: &mut CheckReport) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => report.removed.push(dir.to_path_buf()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => report
            .cleanup_errors
            .push(format!("{}: {e}", dir.display())),
    }
    // `<state>/agent-check` itself, once empty (only that folder, by name).
    if let Some(parent) = dir.parent()
        && parent.file_name().is_some_and(|n| n == "agent-check")
    {
        let _ = std::fs::remove_dir(parent);
    }
}
