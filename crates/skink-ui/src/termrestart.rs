//! Restarting a finished terminal tab, and resuming an AI CLI's last
//! conversation instead of starting a new one.

use crate::{App, Life, Purpose, Tab};
use skink_term::CommandSpec;
use std::path::Path;

/// How a finished terminal starts again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    /// The same command (a known AI CLI resumes its last conversation).
    Again,
    /// The same command, always from scratch.
    Fresh,
}

/// The command that continues the last conversation of a known AI CLI in
/// the same folder, or `None` when the program has no such flag (or the
/// command already resumes).
pub fn resume_spec(spec: &CommandSpec) -> Option<CommandSpec> {
    let name = Path::new(&spec.program)
        .file_stem()
        .map(|n| n.to_string_lossy().to_lowercase())?;
    let has = |a: &str| spec.args.iter().any(|x| x == a);
    let extra: &[&str] = match name.as_str() {
        "claude" if !has("--continue") && !has("-c") && !has("--resume") && !has("-r") => {
            &["--continue"]
        }
        "codex" if spec.args.first().map(String::as_str) != Some("resume") => &["resume", "--last"],
        "opencode" if !has("--continue") && !has("-c") => &["--continue"],
        "cursor-agent" if spec.args.first().map(String::as_str) != Some("resume") => &["resume"],
        "aider" if !has("--restore-chat-history") => &["--restore-chat-history"],
        _ => return None,
    };
    let mut out = spec.clone();
    // Subcommands (`codex resume`) go first, flags after the given ones.
    if extra[0].starts_with('-') {
        out.args.extend(extra.iter().map(|s| (*s).to_owned()));
    } else {
        let mut args: Vec<String> = extra.iter().map(|s| (*s).to_owned()).collect();
        args.append(&mut out.args);
        out.args = args;
    }
    Some(out)
}

impl App {
    /// What the status line offers on a finished terminal tab.
    pub(crate) fn restart_hint(&self) -> Option<&'static str> {
        let Some(Tab::Term(p)) = self.tabs.get(self.active) else {
            return None;
        };
        match (&p.life, &p.purpose) {
            (Life::Running, _) | (_, Purpose::SudoSave(_)) => None,
            _ if resume_spec(&p.spec).is_some() => {
                Some("Enter resumes the conversation, Shift-R starts fresh, other keys close")
            }
            _ => Some("Enter or r restarts, other keys close"),
        }
    }

    /// Start the finished terminal in tab `idx` again, in the same tab.
    pub(crate) fn restart_terminal(&mut self, idx: usize, how: Restart) {
        let Some(Tab::Term(p)) = self.tabs.get(idx) else {
            self.editor_mut().message = Some("Not a terminal tab".into());
            return;
        };
        if matches!(p.life, Life::Running) {
            self.editor_mut().message =
                Some("It is still running (exit it first, or open a new terminal)".into());
            return;
        }
        if matches!(p.purpose, Purpose::SudoSave(_)) {
            self.editor_mut().message = Some("A sudo save is not restarted: save again".into());
            return;
        }
        let spec = match how {
            Restart::Again => resume_spec(&p.spec).unwrap_or_else(|| p.spec.clone()),
            Restart::Fresh => p.spec.clone(),
        };
        let purpose = match &p.purpose {
            Purpose::Task(run) => Purpose::Task(run.clone()),
            _ => Purpose::Shell,
        };
        let shown = format!("{} {}", spec.program, spec.args.join(" "));
        let new = match self.spawn_terminal_tab(spec) {
            Ok(i) => i,
            Err(e) => {
                self.editor_mut().message = Some(format!("Could not restart {shown}: {e}"));
                return;
            }
        };
        let Tab::Term(mut pane) = self.tabs.remove(new) else {
            unreachable!("spawn_terminal_tab adds a terminal tab");
        };
        pane.purpose = purpose;
        self.tabs[idx] = Tab::Term(pane);
        self.switch_to(idx);
        self.toast(
            crate::nav::Level::Info,
            format!("Restarted: {}", shown.trim()),
        );
    }

    /// `:restart-term [fresh]`: the active terminal tab, or the most recent
    /// finished one.
    pub(crate) fn restart_term_cmd(&mut self, args: &[&str]) {
        let how = match args.first() {
            Some(a) if a.eq_ignore_ascii_case("fresh") => Restart::Fresh,
            _ => Restart::Again,
        };
        let finished = |t: &Tab| matches!(t, Tab::Term(p) if !matches!(p.life, Life::Running));
        let idx = if self.tabs.get(self.active).is_some_and(finished) {
            Some(self.active)
        } else {
            (0..self.tabs.len())
                .rev()
                .find(|&i| finished(&self.tabs[i]))
        };
        match idx {
            Some(i) => self.restart_terminal(i, how),
            None => self.editor_mut().message = Some("No finished terminal to restart".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(cmd: &str) -> CommandSpec {
        let mut w = cmd.split_whitespace();
        CommandSpec::new(w.next().unwrap()).args(w)
    }

    #[test]
    fn known_ai_clis_resume_their_last_conversation() {
        let r =
            |c: &str| resume_spec(&spec(c)).map(|s| format!("{} {}", s.program, s.args.join(" ")));
        assert_eq!(r("claude").as_deref(), Some("claude --continue"));
        assert_eq!(
            r("claude --model opus").as_deref(),
            Some("claude --model opus --continue")
        );
        assert_eq!(r("claude --continue"), None, "already resuming");
        assert_eq!(
            r("/usr/bin/codex").as_deref(),
            Some("/usr/bin/codex resume --last")
        );
        assert_eq!(r("opencode").as_deref(), Some("opencode --continue"));
        assert_eq!(r("bash"), None);
    }
}
