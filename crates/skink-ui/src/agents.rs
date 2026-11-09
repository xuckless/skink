//! Which agents Skink knows, how a name in `"ai.agent"` maps to a command,
//! the `:agents` view, the reference block at the top of settings.json and
//! the health check run when the AI settings are saved.

use crate::App;
use skink_ai::check::{self, CheckFailure, CheckOutcome, CheckReport, CheckRequest};
use skink_ai::{AgentSpec, Effort, EffortOutcome, EffortRequest, normalize};
use skink_config::{AgentChoice, EffortChoice, ModelChoice};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// A coding agent Skink can start.
pub struct KnownAgent {
    /// Canonical `"ai.agent"` value.
    pub id: &'static str,
    pub label: &'static str,
    /// Other accepted names (compared after [`normalize`]: case, spaces,
    /// `-`, `_` and `.` do not matter).
    pub aliases: &'static [&'static str],
    /// The ACP command when its program is on PATH.
    pub argv: &'static [&'static str],
    /// npm package to run with `npx -y` when `argv[0]` is missing.
    pub npx: Option<&'static str>,
    /// What to install, for "not found" errors.
    pub needs: &'static str,
}

pub const KNOWN: &[KnownAgent] = &[
    KnownAgent {
        id: "claude",
        label: "Claude Code",
        aliases: &["claude-code", "claude-code-acp", "anthropic"],
        argv: &["claude-code-acp"],
        npx: Some("@zed-industries/claude-code-acp"),
        needs: "Node.js (npx) or `npm i -g @zed-industries/claude-code-acp`; log in once with `claude`",
    },
    KnownAgent {
        id: "gemini",
        label: "Gemini CLI",
        aliases: &["gemini-cli", "google-gemini"],
        argv: &["gemini", "--experimental-acp"],
        npx: None,
        needs: "`npm i -g @google/gemini-cli`",
    },
    KnownAgent {
        id: "opencode",
        label: "opencode",
        aliases: &["open-code"],
        argv: &["opencode", "acp"],
        npx: None,
        needs: "opencode on PATH (opencode.ai)",
    },
    KnownAgent {
        id: "goose",
        label: "Goose",
        aliases: &["goose-cli"],
        argv: &["goose", "acp"],
        npx: None,
        needs: "goose on PATH (block.github.io/goose)",
    },
    KnownAgent {
        id: "antigravity",
        label: "Antigravity",
        aliases: &["agy"],
        argv: &["agy", "acp"],
        npx: None,
        needs: "Antigravity's `agy` on PATH",
    },
    KnownAgent {
        id: "codex",
        label: "Codex",
        aliases: &["codex-acp", "openai-codex", "codex-cli"],
        argv: &["codex-acp"],
        npx: Some("@zed-industries/codex-acp"),
        needs: "Node.js (npx) or `npm i -g @zed-industries/codex-acp`",
    },
];

/// The known agent a settings name refers to.
pub fn find_known(name: &str) -> Option<&'static KnownAgent> {
    let n = normalize(name);
    KNOWN.iter().find(|k| {
        normalize(k.id) == n
            || normalize(k.label) == n
            || k.aliases.iter().any(|a| normalize(a) == n)
    })
}

/// How a known agent would start here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// Its program is on PATH.
    OnPath,
    /// Through `npx -y <package>`.
    Npx(&'static str),
    NotFound,
}

fn on_path(p: &str) -> bool {
    skink_lsp::find_in_path(p).is_some()
}

pub fn install_of(k: &KnownAgent) -> Install {
    if on_path(k.argv[0]) {
        Install::OnPath
    } else if let Some(pkg) = k.npx
        && on_path("npx")
    {
        Install::Npx(pkg)
    } else {
        Install::NotFound
    }
}

fn spec(argv: &[&str]) -> AgentSpec {
    AgentSpec::new(argv[0]).args(argv[1..].iter().copied())
}

fn spec_for(k: &KnownAgent, how: &Install) -> Option<AgentSpec> {
    match how {
        Install::OnPath => Some(spec(k.argv)),
        Install::Npx(pkg) => Some(spec(&["npx", "-y", pkg])),
        Install::NotFound => None,
    }
}

/// The agent command for the settings: (label, spec) or why there is none.
pub fn resolve_agent(choice: &AgentChoice) -> Result<(String, AgentSpec), String> {
    match choice {
        AgentChoice::Command(argv) => Ok((
            argv[0].clone(),
            AgentSpec::new(argv[0].clone()).args(argv[1..].iter().cloned()),
        )),
        AgentChoice::Named(n) => {
            let Some(k) = find_known(n) else {
                let names: Vec<&str> = KNOWN.iter().map(|k| k.id).collect();
                return Err(format!(
                    "Unknown agent \"{n}\". Known: {}, or a command list (see :agents)",
                    names.join(", ")
                ));
            };
            spec_for(k, &install_of(k))
                .map(|s| (k.label.to_owned(), s))
                .ok_or_else(|| format!("{} needs {}", k.label, k.needs))
        }
        AgentChoice::Auto => {
            // Claude Code first: its adapter, or (when the `claude` CLI is
            // installed, so there is a login to reuse) the adapter via npx.
            let claude = &KNOWN[0];
            let claude_npx = on_path("claude") && on_path("npx");
            let first = match install_of(claude) {
                Install::OnPath => Some(Install::OnPath),
                Install::Npx(p) if claude_npx => Some(Install::Npx(p)),
                _ => None,
            };
            if let Some(how) = first {
                return Ok((claude.label.to_owned(), spec_for(claude, &how).unwrap()));
            }
            KNOWN[1..]
                .iter()
                .find(|k| on_path(k.argv[0]))
                .map(|k| (k.label.to_owned(), spec(k.argv)))
                .ok_or_else(|| {
                    "No ACP agent found on PATH. Install Claude Code, gemini, opencode or goose, \
                     or set \"ai.agent\" in :settings (:agents lists them)"
                        .to_owned()
                })
        }
    }
}

fn effort_request(e: EffortChoice) -> EffortRequest {
    match e {
        EffortChoice::AgentDefault => EffortRequest::AgentDefault,
        EffortChoice::Low => EffortRequest::Level(Effort::Low),
        EffortChoice::Medium => EffortRequest::Level(Effort::Medium),
        EffortChoice::High => EffortRequest::Level(Effort::High),
        EffortChoice::Max => EffortRequest::Level(Effort::Max),
    }
}

pub fn session_options(e: EffortChoice) -> skink_ai::SessionOptions {
    skink_ai::SessionOptions {
        effort: effort_request(e),
    }
}

// ---- the health check ----------------------------------------------------------------

/// The agent health check run when the AI settings are saved.
pub enum AgentCheck {
    Idle,
    Running {
        started: Instant,
        agent: String,
        cancel: Arc<AtomicBool>,
        result: Receiver<CheckReport>,
    },
    Passed {
        /// "Claude Code ✓ (model …, effort …)".
        summary: String,
    },
    Failed {
        agent: String,
        reason: String,
    },
}

/// Budget for one check (a first `npx` run downloads the adapter).
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// One line for the status bar.
pub fn summary(label: &str, model: &ModelChoice, effort: EffortChoice, r: &CheckReport) -> String {
    match &r.outcome {
        CheckOutcome::Passed {
            model: got,
            effort: eff,
            ..
        } => {
            let model = got.clone().unwrap_or_else(|| match model {
                ModelChoice::AgentDefault => "default".into(),
                ModelChoice::Named(m) => m.clone(),
            });
            let effort = match eff {
                EffortOutcome::NotRequested => "effort default".to_owned(),
                EffortOutcome::ConfigOption { value, .. } => format!("effort {value}"),
                EffortOutcome::ClaudeThinkingTokens(n) => {
                    format!("effort {}: thinking budget {n} tokens", effort.name())
                }
                EffortOutcome::Unsupported(why) => {
                    format!("{label} doesn't support ai.effort over ACP ({why})")
                }
            };
            format!("{label} ✓ (model {model}, {effort})")
        }
        CheckOutcome::Failed(f) => format!("{label} ✗ {f}"),
    }
}

impl App {
    /// Start (or restart) the check for the current settings.
    pub(crate) fn start_agent_check(&mut self) {
        self.cancel_agent_check();
        if self.backend == skink_core::Backend::Delegated {
            self.doc.editor.message = Some("EveryAI manages the agents here".into());
            return;
        }
        let ai = self.settings.ai.clone();
        let (label, spec) = match resolve_agent(&ai.agent) {
            Ok(x) => x,
            Err(e) => {
                let agent = match &ai.agent {
                    AgentChoice::Auto => "auto".into(),
                    AgentChoice::Named(n) => n.clone(),
                    AgentChoice::Command(a) => a.join(" "),
                };
                self.doc.editor.message = Some(format!("Agent check: {e}"));
                self.ai.check = AgentCheck::Failed { agent, reason: e };
                return;
            }
        };
        let Some(state) = self.state_dir.clone() else {
            self.doc.editor.message = Some("Agent check: no state directory".into());
            return;
        };
        let workdir = match check::make_workdir(&state) {
            Ok(d) => d,
            Err(e) => {
                self.doc.editor.message = Some(format!("Agent check: {e}"));
                return;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let req = CheckRequest {
            model: match &ai.model {
                ModelChoice::AgentDefault => None,
                ModelChoice::Named(m) => Some(m.clone()),
            },
            effort: effort_request(ai.effort),
            workdir: workdir.clone(),
            timeout: CHECK_TIMEOUT,
            claude_config_dir: check::claude_config_dir(),
            cancel: cancel.clone(),
        };
        let (tx, rx) = mpsc::channel();
        let wake = self.wake.clone();
        let spec = spec.cwd(workdir.clone());
        let spawned = std::thread::Builder::new()
            .name("skink-agent-check".into())
            .spawn(move || {
                let report = match skink_ai::acp::spawn(&spec) {
                    Ok(t) => check::run_check(t, &req),
                    Err(e) => {
                        check::discard_workdir(&req.workdir);
                        CheckReport {
                            outcome: CheckOutcome::Failed(CheckFailure::Start(format!(
                                "{}: {e}",
                                spec.program
                            ))),
                            session_removed: false,
                            removed: Vec::new(),
                            cleanup_errors: Vec::new(),
                        }
                    }
                };
                let _ = tx.send(report);
                if let Some(w) = wake {
                    w();
                }
            });
        if let Err(e) = spawned {
            check::discard_workdir(&workdir);
            self.doc.editor.message = Some(format!("Agent check: {e}"));
            return;
        }
        self.doc.editor.message = Some(format!("Checking {label}…"));
        self.ai.check = AgentCheck::Running {
            started: Instant::now(),
            agent: label,
            cancel,
            result: rx,
        };
    }

    /// Stop a running check (its worker cleans up and exits).
    pub(crate) fn cancel_agent_check(&mut self) {
        if let AgentCheck::Running { cancel, .. } = &self.ai.check {
            cancel.store(true, Ordering::Relaxed);
            self.ai.check = AgentCheck::Idle;
        }
    }

    /// Pick up a finished check (called from the event loop).
    pub(crate) fn poll_agent_check(&mut self) {
        let AgentCheck::Running { agent, result, .. } = &self.ai.check else {
            return;
        };
        let report = match result.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                let agent = agent.clone();
                self.ai.check = AgentCheck::Failed {
                    agent,
                    reason: "the check stopped unexpectedly".into(),
                };
                return;
            }
        };
        let agent = agent.clone();
        let ai = &self.settings.ai;
        let line = summary(&agent, &ai.model, ai.effort, &report);
        self.ai.check = match report.outcome {
            CheckOutcome::Passed { .. } => AgentCheck::Passed {
                summary: line.clone(),
            },
            CheckOutcome::Failed(f) => AgentCheck::Failed {
                agent,
                reason: f.to_string(),
            },
        };
        self.doc.editor.message = Some(line);
    }

    /// Called after input: when settings.json (the active document) was
    /// saved, reload the config; changed AI settings start a check.
    pub(crate) fn watch_settings_save(&mut self) {
        let Some(path) = self.settings_path() else {
            return;
        };
        if self.doc.canon.as_deref() != Some(path.as_path()) || self.doc.editor.buffer.is_dirty() {
            return;
        }
        let stamp = file_stamp(&path);
        let seen = std::mem::replace(&mut self.ai.settings_seen, stamp);
        if seen.is_none() || seen == stamp {
            return;
        }
        let before = self.settings.ai.clone();
        let errs = self.load_user_config();
        if !errs.is_empty() {
            self.doc.editor.message = Some(errs.join("; "));
        }
        if self.settings.ai != before {
            if self.settings.ai.agent != before.agent && !self.ai.busy() {
                // The next request starts the newly chosen agent.
                self.stop_agent_quietly();
            }
            if errs.is_empty() {
                self.start_agent_check();
            }
        }
    }

    /// settings.json, canonical (as open documents are keyed).
    fn settings_path(&self) -> Option<PathBuf> {
        let p = self.config_dir.as_ref()?.join("settings.json");
        Some(crate::doc::canonical(&p))
    }

    /// `:settings`: refresh the reference block, then open the file.
    pub(crate) fn open_settings(&mut self) {
        let Some(dir) = self.config_dir.clone() else {
            self.doc.editor.message = Some("No config directory (set SKINK_CONFIG_DIR)".into());
            return;
        };
        let path = dir.join("settings.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| skink_config::DEFAULT_SETTINGS.to_owned());
        let open_dirty = (0..self.tabs.len()).any(|i| {
            self.doc_at(i).is_some_and(|d| {
                d.editor.buffer.is_dirty()
                    && d.canon.as_deref() == Some(crate::doc::canonical(&path).as_path())
            })
        });
        let fresh = with_reference(&text, &reference_block(&self.settings.ai));
        if !open_dirty
            && fresh != text
            && let Err(e) =
                std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &fresh))
        {
            self.doc.editor.message = Some(format!("{}: {e}", path.display()));
            return;
        }
        self.open_path(&path);
        // Writing the block is not a settings save.
        self.ai.settings_seen = file_stamp(&path);
    }

    /// `:agents`: what is supported and installed, and the last check.
    pub(crate) fn open_agents_view(&mut self) {
        let text = agents_text(&self.settings.ai, &self.agent_label(), &self.ai.check);
        self.open_view("Agents", &text);
    }
}

/// Modification time and length (two saves can share a coarse mtime).
pub type FileStamp = (std::time::SystemTime, u64);

fn file_stamp(p: &std::path::Path) -> Option<FileStamp> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

pub const REF_START: &str = "// ---- Skink AI reference";
pub const REF_END: &str = "// ---- end of Skink AI reference ----";

/// `text` with its reference block replaced by `block` (or `block` put first).
pub fn with_reference(text: &str, block: &str) -> String {
    let body = match (text.find(REF_START), text.find(REF_END)) {
        (Some(a), Some(b)) if a < b => {
            let end = b + REF_END.len();
            let end = end + text[end..].find('\n').map_or(text.len() - end, |i| i + 1);
            format!("{}{}", &text[..a], &text[end..])
        }
        _ => text.to_owned(),
    };
    format!("{block}{body}")
}

fn status_word(k: &KnownAgent) -> String {
    match install_of(k) {
        Install::OnPath => format!("installed ({})", k.argv[0]),
        Install::Npx(pkg) => format!("via npx ({pkg})"),
        Install::NotFound => format!("not found: needs {}", k.needs),
    }
}

/// The comment block at the top of settings.json.
pub fn reference_block(_ai: &skink_config::AiSettings) -> String {
    let mut s = String::new();
    s.push_str(REF_START);
    s.push_str(" (rewritten when :settings opens; your values are below) ----\n");
    s.push_str("// \"ai.agent\": which coding agent the AI box (Ctrl-G) and :agent talk to.\n");
    s.push_str("//   \"auto\"  the first available, in this order:\n");
    for k in KNOWN {
        let names: Vec<String> = std::iter::once(k.id)
            .chain(k.aliases.iter().copied())
            .map(|n| format!("\"{n}\""))
            .collect();
        s.push_str(&format!(
            "//   {:<12} {} — {}\n//   {:<12} also {}\n",
            format!("\"{}\"", k.id),
            k.label,
            status_word(k),
            "",
            names[1..].join(", ")
        ));
    }
    s.push_str("//   [\"my-agent\", \"--acp\"]  any command that speaks ACP on stdio\n");
    s.push_str("// \"ai.model\": \"default\" or a model the agent offers; loose names work\n");
    s.push_str(
        "//   (\"opus 5.5\", \"opus5.5\", \"claude-opus-5-5\"). :model lists the offered ones.\n",
    );
    s.push_str("// \"ai.effort\": \"default\", \"low\", \"medium\", \"high\" or \"max\".\n");
    s.push_str("//   Claude Code: thinking budget of 4k / 10k / 20k / 32k tokens. Other agents:\n");
    s.push_str("//   their reasoning-level option, when they offer one over ACP.\n");
    s.push_str("// Per mode: \"ai.model.rewrite\", \"ai.effort.ask\", ... (rewrite, ask, search, implement)\n");
    s.push_str("// Saving a change to these runs a one-line test chat (in a throwaway folder,\n");
    s.push_str("// deleted afterwards). :agents shows the result; :agents check reruns it.\n");
    s.push_str(REF_END);
    s.push('\n');
    s
}

fn agents_text(ai: &skink_config::AiSettings, active: &str, check: &AgentCheck) -> String {
    let mut s = String::from("Supported agents (\"ai.agent\" in :settings)\n\n");
    for k in KNOWN {
        let mark = if k.label == active { "▶" } else { " " };
        s.push_str(&format!(
            "{mark} {:<12} {:<12} {}\n",
            k.id,
            k.label,
            status_word(k)
        ));
        let mut names = vec![k.label];
        names.extend(k.aliases.iter().copied());
        s.push_str(&format!("  {:<12} names: {}\n", "", names.join(", ")));
        if install_of(k) == Install::NotFound {
            s.push_str(&format!("  {:<12} install: {}\n", "", k.needs));
        }
    }
    s.push_str("\nAny other ACP agent: \"ai.agent\": [\"program\", \"args\"...]\n\n");
    let model = match &ai.model {
        ModelChoice::AgentDefault => "default".to_owned(),
        ModelChoice::Named(m) => m.clone(),
    };
    s.push_str(&format!(
        "Active: {active}   model: {model}   effort: {}\n",
        ai.effort.name()
    ));
    for (m, c) in &ai.mode_models {
        if let ModelChoice::Named(n) = c {
            s.push_str(&format!("  ai.model.{} = {n}\n", m.name()));
        }
    }
    for (m, e) in &ai.mode_efforts {
        s.push_str(&format!("  ai.effort.{} = {}\n", m.name(), e.name()));
    }
    s.push_str("\nLast check: ");
    s.push_str(&match check {
        AgentCheck::Idle => "none yet (:agents check runs one)".to_owned(),
        AgentCheck::Running { agent, started, .. } => {
            format!("{agent}: running ({}s)", started.elapsed().as_secs())
        }
        AgentCheck::Passed { summary, .. } => summary.clone(),
        AgentCheck::Failed { agent, reason } => format!("{agent} ✗ {reason}"),
    });
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use skink_config::{Keymap, Settings};
    use skink_core::{Backend, Buffer, Editor};

    #[test]
    fn agent_names_are_matched_loosely() {
        for (name, id) in [
            ("claude-code-acp", "claude"),
            ("claude code", "claude"),
            ("Claude_Code", "claude"),
            ("claudecode", "claude"),
            ("gemini-cli", "gemini"),
            ("Gemini CLI", "gemini"),
            ("agy", "antigravity"),
            ("codex-acp", "codex"),
        ] {
            assert_eq!(find_known(name).map(|k| k.id), Some(id), "{name}");
        }
        assert!(find_known("vim").is_none());
        let e = resolve_agent(&AgentChoice::Named("vim".into())).unwrap_err();
        assert!(e.contains("Unknown agent") && e.contains("claude"), "{e}");
    }

    #[test]
    fn reference_block_is_replaced_and_keeps_values_parseable() {
        let user = "{\n  // mine\n  \"ai.agent\": \"gemini\",\n  \"tabsize\": 2\n}\n";
        let block = reference_block(&skink_config::AiSettings::default());
        let once = with_reference(user, &block);
        assert!(
            once.starts_with(REF_START) && once.ends_with(user),
            "{once}"
        );
        // Opening again replaces the block instead of stacking it.
        assert_eq!(with_reference(&once, &block), once);
        let (s, errs) = Settings::parse(&once);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(s.tabsize, 2);
        assert_eq!(s.ai.agent, AgentChoice::Named("gemini".into()));
    }

    fn save(a: &mut App, text: &str) {
        a.doc.editor.set_text(text);
        a.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        )));
    }

    #[test]
    fn saving_changed_ai_settings_runs_the_check_and_cleans_up() {
        let root = std::env::temp_dir().join(format!("skink-ui-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (cfg, state) = (root.join("cfg"), root.join("state"));
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(cfg.join("settings.json"), "{\"tabsize\": 4}\n").unwrap();
        let mut a = App::new(
            Editor::new(Buffer::from_text("")),
            Keymap::defaults(),
            Backend::Local,
        );
        a.config_dir = Some(cfg.clone());
        a.state_dir = Some(state.clone());
        a.load_user_config();
        a.open_settings();
        assert!(a.doc.editor.buffer.text().starts_with(REF_START));
        // Not an AI change: no check.
        save(&mut a, "{\"tabsize\": 2}\n");
        assert_eq!(a.doc.editor.tab_width, 2, "saved settings apply");
        assert!(matches!(a.ai.check, AgentCheck::Idle));
        // A new agent: checked (this one cannot start).
        save(&mut a, "{\"ai.agent\": [\"skink-no-such-agent\"]}\n");
        assert!(matches!(a.ai.check, AgentCheck::Running { .. }));
        let t = Instant::now();
        while matches!(a.ai.check, AgentCheck::Running { .. }) && t.elapsed().as_secs() < 10 {
            std::thread::sleep(Duration::from_millis(10));
            a.poll_agent_check();
        }
        let AgentCheck::Failed { reason, .. } = &a.ai.check else {
            panic!("check did not fail")
        };
        assert!(reason.contains("could not start"), "{reason}");
        assert!(!state.join("agent-check").exists(), "throwaway dir removed");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
