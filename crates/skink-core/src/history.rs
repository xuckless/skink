//! Undo/redo. Edits are stored as replacements (`removed` -> `inserted` at a
//! char index), so any change is trivially invertible on a rope. Consecutive
//! single-char typing coalesces into one undo step.
//!
//! Idea from rhysd/tui-textarea (MIT) `history.rs`: bounded history of
//! invertible edits. Reimplemented over ropey indices rather than `Vec<String>`
//! rows, with typing-run coalescing added.

use std::collections::VecDeque;

const MAX_GROUPS: usize = 1000;

#[derive(Debug, Clone)]
pub struct Edit {
    pub at: usize,
    pub removed: String,
    pub inserted: String,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub edits: Vec<Edit>,
    /// Cursor (char index) before the first edit / after the last.
    pub before: usize,
    pub after: usize,
    typing: bool,
}

impl Group {
    /// A finished undo step (restored from disk: never extended by typing).
    pub fn new(edits: Vec<Edit>, before: usize, after: usize) -> Group {
        Group {
            edits,
            before,
            after,
            typing: false,
        }
    }
}

/// Whether the next edit may join the newest undo step.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Run {
    /// The next edit starts a new step.
    #[default]
    Closed,
    /// A typing run is open: the next adjacent keystroke extends it.
    Typing,
}

#[derive(Debug, Default)]
pub struct History {
    undo: VecDeque<Group>,
    redo: Vec<Group>,
    run: Run,
}

impl History {
    /// Stop coalescing: the next edit starts a new undo step.
    pub fn close(&mut self) {
        self.run = Run::Closed;
    }

    /// Undo steps oldest first, and redo steps (next redo last), for saving.
    pub fn steps(&self) -> (Vec<Group>, Vec<Group>) {
        (self.undo.iter().cloned().collect(), self.redo.clone())
    }

    /// Replace the history with saved steps (as returned by [`steps`]).
    ///
    /// [`steps`]: History::steps
    pub fn restore(&mut self, undo: Vec<Group>, redo: Vec<Group>) {
        let skip = undo.len().saturating_sub(MAX_GROUPS);
        self.undo = undo.into_iter().skip(skip).collect();
        self.redo = redo;
        self.run = Run::Closed;
    }

    /// Record an applied edit. `typing` marks a single-char insertion that
    /// may extend the previous typing run.
    pub fn record(&mut self, edit: Edit, cursor_before: usize, cursor_after: usize, typing: bool) {
        self.redo.clear();
        if self.run == Run::Typing
            && typing
            && let Some(g) = self.undo.back_mut()
        {
            let continues = g.typing
                && g.edits.last().is_some_and(|l| {
                    l.removed.is_empty() && l.at + l.inserted.chars().count() == edit.at
                });
            if continues {
                g.edits.push(edit);
                g.after = cursor_after;
                return;
            }
        }
        self.undo.push_back(Group {
            edits: vec![edit],
            before: cursor_before,
            after: cursor_after,
            typing,
        });
        if self.undo.len() > MAX_GROUPS {
            self.undo.pop_front();
        }
        self.run = if typing { Run::Typing } else { Run::Closed };
    }

    /// Add an edit to the current (just recorded) group instead of starting
    /// a new one; for multi-edit operations like indenting a selection.
    pub fn record_into_open(&mut self, edit: Edit, cursor_after: usize) {
        if let Some(g) = self.undo.back_mut() {
            g.edits.push(edit);
            g.after = cursor_after;
        }
    }

    /// Set the cursor positions restored by undo/redo of the newest group
    /// (a multi-cursor edit records them for the primary cursor).
    pub fn retarget_last(&mut self, before: usize, after: usize) {
        if let Some(g) = self.undo.back_mut() {
            g.before = before;
            g.after = after;
        }
    }

    /// Number of undo steps.
    pub fn len(&self) -> usize {
        self.undo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.undo.is_empty()
    }

    /// Fold the steps recorded after the first `from` into one (a script's
    /// edits undo together). A no-op when fewer than two were added.
    pub fn merge_from(&mut self, from: usize) {
        if self.undo.len() < from + 2 {
            return;
        }
        let tail: Vec<Group> = self.undo.drain(from..).collect();
        let (before, after) = (tail[0].before, tail[tail.len() - 1].after);
        self.undo.push_back(Group::new(
            tail.into_iter().flat_map(|g| g.edits).collect(),
            before,
            after,
        ));
        self.run = Run::Closed;
    }

    pub fn pop_undo(&mut self) -> Option<Group> {
        self.run = Run::Closed;
        self.undo.pop_back()
    }

    pub fn push_redo(&mut self, g: Group) {
        self.redo.push(g);
    }

    pub fn pop_redo(&mut self) -> Option<Group> {
        self.redo.pop()
    }

    pub fn push_undo_keep_redo(&mut self, g: Group) {
        self.undo.push_back(g);
        self.run = Run::Closed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(at: usize, s: &str) -> Edit {
        Edit {
            at,
            removed: String::new(),
            inserted: s.into(),
        }
    }

    #[test]
    fn typing_runs_coalesce() {
        let mut h = History::default();
        h.record(ins(0, "a"), 0, 1, true);
        h.record(ins(1, "b"), 1, 2, true);
        h.record(ins(5, "c"), 5, 6, true); // not adjacent: new group
        assert_eq!(h.pop_undo().unwrap().edits.len(), 1);
        let g = h.pop_undo().unwrap();
        assert_eq!((g.edits.len(), g.before, g.after), (2, 0, 2));
    }

    #[test]
    fn merged_steps_undo_together() {
        let mut h = History::default();
        h.record(ins(0, "a"), 0, 1, false);
        h.record(ins(1, "b"), 1, 2, false);
        h.record(ins(2, "c"), 2, 3, false);
        h.merge_from(1);
        let g = h.pop_undo().unwrap();
        assert_eq!((g.edits.len(), g.before, g.after), (2, 1, 3));
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn close_breaks_runs() {
        let mut h = History::default();
        h.record(ins(0, "a"), 0, 1, true);
        h.close();
        h.record(ins(1, "b"), 1, 2, true);
        assert_eq!(h.pop_undo().unwrap().edits.len(), 1);
        assert_eq!(h.pop_undo().unwrap().edits.len(), 1);
    }
}
