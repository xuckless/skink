//! The "worker": tracks one long-running agent task (idea from
//! ThePrimeagen/99's work/search loop, re-implemented): a goal, what has been
//! found, what remains, and whether another round of prompting is worthwhile.
//! UI-free state machine: the host sends the prompts and feeds back replies.

use std::path::Path;

use super::context::{ContextConfig, ProjectFiles};
use super::search::{SearchHit, SearchRequest, parse_search_results, search_body, search_contract};
use super::{Prompt, RequestKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerState {
    /// Created, no prompt sent yet.
    Idle,
    /// A prompt is out; waiting for the reply.
    Waiting,
    /// Got a reply, more work remains: call [`Worker::continue_prompt`].
    NeedsContinue,
    /// The agent said it is done (or had nothing left).
    Done,
    /// No further progress, or the round limit was reached.
    Stalled,
    Cancelled,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyStatus {
    Done,
    Continue,
    /// The agent ignored the `STATUS:` footer.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    pub new_hits: usize,
    pub status: ReplyStatus,
    pub state: WorkerState,
}

#[derive(Debug, Clone)]
pub struct Worker {
    pub goal: String,
    pub state: WorkerState,
    pub rounds: u32,
    pub max_rounds: u32,
    pub hits: Vec<SearchHit>,
    pub remaining: Vec<String>,
    stalls: u32,
    max_stalls: u32,
    max_results: usize,
}

/// Parse the tracker footer: last `STATUS:` wins; `REMAINING:` bullets.
pub fn parse_tracker(reply: &str) -> (ReplyStatus, Vec<String>) {
    let mut status = ReplyStatus::Missing;
    let mut remaining = Vec::new();
    let mut in_remaining = false;
    for line in reply.lines() {
        let t0 = line.trim();
        let t = t0.trim_matches(['`', '*']).trim();
        let up = t.to_ascii_uppercase();
        if let Some(v) = up.strip_prefix("STATUS:") {
            let v = v.trim();
            status = if v.starts_with("DONE") {
                ReplyStatus::Done
            } else if v.starts_with("CONTINUE") {
                ReplyStatus::Continue
            } else {
                status
            };
            in_remaining = false;
        } else if up.starts_with("REMAINING:") {
            in_remaining = true;
            let inline = t["REMAINING:".len()..].trim();
            if !inline.is_empty() && !inline.eq_ignore_ascii_case("none") {
                remaining.push(inline.trim_start_matches(['-', '*', ' ']).to_owned());
            }
        } else if in_remaining {
            if let Some(item) = t0.strip_prefix("- ").or_else(|| t0.strip_prefix("* ")) {
                let item = item.trim();
                if !item.is_empty() && !item.eq_ignore_ascii_case("none") {
                    remaining.push(item.to_owned());
                }
            } else if !t.is_empty() {
                in_remaining = false;
            }
        }
    }
    (status, remaining)
}

impl Worker {
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            state: WorkerState::Idle,
            rounds: 0,
            max_rounds: 5,
            hits: Vec::new(),
            remaining: Vec::new(),
            stalls: 0,
            max_stalls: 2,
            max_results: 30,
        }
    }

    #[must_use]
    pub fn with_max_rounds(mut self, n: u32) -> Self {
        self.max_rounds = n;
        self
    }

    /// First prompt: a search request plus the tracker footer contract.
    pub fn start(
        &mut self,
        cfg: &ContextConfig,
        files: &dyn ProjectFiles,
        req: &SearchRequest,
    ) -> Prompt {
        self.goal.clone_from(&req.query);
        self.max_results = req.max_results;
        self.rounds = 1;
        self.state = WorkerState::Waiting;
        let mut t = search_contract(req.max_results, true);
        t.push('\n');
        t.push_str(&search_body(cfg, files, req));
        Prompt {
            kind: RequestKind::Worker,
            text: t,
        }
    }

    /// Feed the agent's reply. Merges new hits, updates the remaining list and
    /// decides the next state.
    pub fn on_reply(
        &mut self,
        reply: &str,
        root: &Path,
        files: Option<&dyn ProjectFiles>,
    ) -> StepReport {
        if self.state != WorkerState::Waiting {
            return StepReport {
                new_hits: 0,
                status: ReplyStatus::Missing,
                state: self.state.clone(),
            };
        }
        let parsed = parse_search_results(reply, root, files);
        let before = self.hits.len();
        for h in parsed.hits {
            if !self
                .hits
                .iter()
                .any(|x| x.path == h.path && x.line == h.line)
            {
                self.hits.push(h);
            }
        }
        let new_hits = self.hits.len() - before;
        let (status, remaining) = parse_tracker(reply);
        let changed = remaining != self.remaining;
        self.remaining = remaining;
        let status_eff = match status {
            ReplyStatus::Missing if self.remaining.is_empty() => ReplyStatus::Done,
            ReplyStatus::Missing => ReplyStatus::Continue,
            s => s,
        };
        self.state = if status_eff == ReplyStatus::Done {
            WorkerState::Done
        } else {
            if new_hits > 0 || changed {
                self.stalls = 0;
            } else {
                self.stalls += 1;
            }
            if self.stalls >= self.max_stalls || self.rounds >= self.max_rounds {
                WorkerState::Stalled
            } else {
                WorkerState::NeedsContinue
            }
        };
        StepReport {
            new_hits,
            status,
            state: self.state.clone(),
        }
    }

    /// Prompt asking the agent to keep going without repeating itself.
    /// `None` unless the state is `NeedsContinue`.
    pub fn continue_prompt(&mut self) -> Option<Prompt> {
        if self.state != WorkerState::NeedsContinue {
            return None;
        }
        self.rounds += 1;
        self.state = WorkerState::Waiting;
        let mut t = search_contract(self.max_results, true);
        t.push_str(&format!(
            "\n## QUERY (continued, round {}/{})\n{}\n\n",
            self.rounds,
            self.max_rounds,
            self.goal.trim()
        ));
        if !self.hits.is_empty() {
            t.push_str("## Already found (do not list these again)\n");
            for h in self.hits.iter().take(50) {
                t.push_str(&format!("- {}:{}:{}\n", h.path.display(), h.line, h.col));
            }
            t.push('\n');
        }
        if !self.remaining.is_empty() {
            t.push_str("## Still to search\n");
            for r in &self.remaining {
                t.push_str(&format!("- {r}\n"));
            }
            t.push('\n');
        }
        t.push_str("Continue the search. Report only NEW locations, then the STATUS footer.\n");
        Some(Prompt {
            kind: RequestKind::Worker,
            text: t,
        })
    }

    pub fn cancel(&mut self) {
        if !self.is_finished() {
            self.state = WorkerState::Cancelled;
        }
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        if !self.is_finished() {
            self.state = WorkerState::Failed(message.into());
        }
    }

    pub fn is_finished(&self) -> bool {
        matches!(
            self.state,
            WorkerState::Done
                | WorkerState::Stalled
                | WorkerState::Cancelled
                | WorkerState::Failed(_)
        )
    }

    /// One-line summary for a status bar.
    pub fn status_line(&self) -> String {
        let st = match &self.state {
            WorkerState::Idle => "idle".to_owned(),
            WorkerState::Waiting => {
                format!("searching (round {}/{})", self.rounds, self.max_rounds)
            }
            WorkerState::NeedsContinue => "paused".to_owned(),
            WorkerState::Done => "done".to_owned(),
            WorkerState::Stalled => "stopped (no progress)".to_owned(),
            WorkerState::Cancelled => "cancelled".to_owned(),
            WorkerState::Failed(m) => format!("failed: {m}"),
        };
        format!(
            "{st}: {} found, {} remaining",
            self.hits.len(),
            self.remaining.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::context::testfs::MemFiles;
    use super::*;

    fn fs() -> MemFiles {
        MemFiles::with(&[("/p/a.rs", ""), ("/p/b.rs", ""), ("/p/c.rs", "")])
    }

    fn started(max_rounds: u32) -> (Worker, ContextConfig, MemFiles) {
        let cfg = ContextConfig::new("/p");
        let files = fs();
        let mut w = Worker::new("x").with_max_rounds(max_rounds);
        let p = w.start(&cfg, &files, &SearchRequest::new("find all callers of foo"));
        assert_eq!(p.kind, RequestKind::Worker);
        assert!(p.text.contains("STATUS: DONE"));
        assert!(p.text.contains("find all callers of foo"));
        (w, cfg, files)
    }

    #[test]
    fn tracker_parsing() {
        let (s, r) =
            parse_tracker("a.rs:1:1: x\nSTATUS: CONTINUE\nREMAINING:\n- crate b\n* crate c\n\nbye");
        assert_eq!(
            (s, r),
            (
                ReplyStatus::Continue,
                vec!["crate b".to_string(), "crate c".to_string()]
            )
        );
        let (s, r) = parse_tracker("**STATUS: DONE**\nREMAINING: none");
        assert_eq!((s, r.len()), (ReplyStatus::Done, 0));
        let (s, r) = parse_tracker("REMAINING: tests dir\nstatus: continue");
        assert_eq!(
            (s, r),
            (ReplyStatus::Continue, vec!["tests dir".to_string()])
        );
        assert_eq!(parse_tracker("nothing").0, ReplyStatus::Missing);
    }

    #[test]
    fn runs_until_done() {
        let (mut w, _, files) = started(5);
        assert_eq!(w.state, WorkerState::Waiting);
        let rep = w.on_reply(
            "a.rs:1:1: one\nSTATUS: CONTINUE\nREMAINING:\n- dir b",
            Path::new("/p"),
            Some(&files),
        );
        assert_eq!(
            (rep.new_hits, rep.state.clone()),
            (1, WorkerState::NeedsContinue)
        );
        let p = w.continue_prompt().unwrap();
        assert!(
            p.text.contains("round 2/5")
                && p.text.contains("/p/a.rs:1:1")
                && p.text.contains("- dir b")
        );
        assert!(p.text.contains("Report only NEW"));
        assert_eq!(w.state, WorkerState::Waiting);
        assert!(
            w.continue_prompt().is_none(),
            "no double continue while waiting"
        );
        let rep = w.on_reply(
            "b.rs:2:2: two\na.rs:1:1: dup\nSTATUS: DONE",
            Path::new("/p"),
            Some(&files),
        );
        assert_eq!((rep.new_hits, rep.state), (1, WorkerState::Done));
        assert_eq!(w.hits.len(), 2);
        assert!(w.is_finished());
        assert!(w.status_line().starts_with("done: 2 found"));
    }

    #[test]
    fn stalls_without_progress() {
        let (mut w, _, files) = started(10);
        let root = Path::new("/p");
        let r1 = w.on_reply("STATUS: CONTINUE\nREMAINING:\n- x", root, Some(&files));
        assert_eq!(r1.state, WorkerState::NeedsContinue); // remaining list changed: progress
        w.continue_prompt().unwrap();
        let r2 = w.on_reply("STATUS: CONTINUE\nREMAINING:\n- x", root, Some(&files));
        assert_eq!(r2.state, WorkerState::NeedsContinue); // first stall
        w.continue_prompt().unwrap();
        let r3 = w.on_reply("STATUS: CONTINUE\nREMAINING:\n- x", root, Some(&files));
        assert_eq!(r3.state, WorkerState::Stalled);
    }

    #[test]
    fn round_limit() {
        let (mut w, _, files) = started(2);
        let root = Path::new("/p");
        w.on_reply(
            "a.rs:1:1: n\nSTATUS: CONTINUE\nREMAINING:\n- more",
            root,
            Some(&files),
        );
        w.continue_prompt().unwrap();
        let r = w.on_reply(
            "b.rs:1:1: n\nSTATUS: CONTINUE\nREMAINING:\n- more",
            root,
            Some(&files),
        );
        assert_eq!(r.state, WorkerState::Stalled);
    }

    #[test]
    fn missing_footer_is_treated_sensibly() {
        let (mut w, _, files) = started(5);
        let r = w.on_reply("a.rs:1:1: only line", Path::new("/p"), Some(&files));
        assert_eq!(
            (r.status, r.state),
            (ReplyStatus::Missing, WorkerState::Done)
        );
    }

    #[test]
    fn cancel_and_fail_are_sticky() {
        let (mut w, _, files) = started(5);
        w.cancel();
        assert_eq!(w.state, WorkerState::Cancelled);
        w.fail("late");
        assert_eq!(w.state, WorkerState::Cancelled);
        let r = w.on_reply("a.rs:1:1: x", Path::new("/p"), Some(&files));
        assert_eq!(r.new_hits, 0, "replies after cancel are ignored");
        assert!(w.hits.is_empty());
        let mut w2 = Worker::new("g");
        w2.fail("boom");
        assert_eq!(w2.state, WorkerState::Failed("boom".into()));
    }
}
