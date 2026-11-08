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

#[derive(Debug, Default)]
pub struct History {
    undo: VecDeque<Group>,
    redo: Vec<Group>,
    open: bool,
}

impl History {
    /// Stop coalescing: the next edit starts a new undo step.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Record an applied edit. `typing` marks a single-char insertion that
    /// may extend the previous typing run.
    pub fn record(&mut self, edit: Edit, cursor_before: usize, cursor_after: usize, typing: bool) {
        self.redo.clear();
        if self.open && typing {
            if let Some(g) = self.undo.back_mut() {
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
        self.open = typing;
    }

    /// Add an edit to the current (just recorded) group instead of starting
    /// a new one; for multi-edit operations like indenting a selection.
    pub fn record_into_open(&mut self, edit: Edit, cursor_after: usize) {
        if let Some(g) = self.undo.back_mut() {
            g.edits.push(edit);
            g.after = cursor_after;
        }
    }

    pub fn pop_undo(&mut self) -> Option<Group> {
        self.open = false;
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
        self.open = false;
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
    fn close_breaks_runs() {
        let mut h = History::default();
        h.record(ins(0, "a"), 0, 1, true);
        h.close();
        h.record(ins(1, "b"), 1, 2, true);
        assert_eq!(h.pop_undo().unwrap().edits.len(), 1);
        assert_eq!(h.pop_undo().unwrap().edits.len(), 1);
    }
}
