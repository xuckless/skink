//! Comparing two files side by side (`skink --diff A B`, `:diff OTHER`,
//! `git difftool`): next / previous change, copy a change to the other side,
//! open either side to edit, save both.
//!
//! The view reads each side from its open tab when there is one (unsaved
//! edits included), else from the disk. Copying a change edits the target
//! file's tab (opened if needed), so unsaved work has the usual protection:
//! quit asks, crash backups, one undo step per copy.

use crate::{
    App,
    doc::{DocKind, canonical},
    git::{self, Hunk},
    gitui::{Row, ViewWhat},
};
use skink_core::{Buffer, Editor};
use std::path::{Path, PathBuf};

/// Two files compared in a view tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compare {
    pub left: PathBuf,
    pub right: PathBuf,
    pub hunks: Vec<Hunk>,
}

/// Which side a change is copied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toward {
    Left,
    Right,
}

/// `old` lines `range_old` replaced by... : the target text with lines
/// `at` replaced by `with`, keeping the target's final newline.
fn splice(target: &str, at: std::ops::Range<usize>, with: &[&str]) -> String {
    let lines = git::lines(target);
    let mut out: Vec<&str> = Vec::with_capacity(lines.len() + with.len());
    out.extend_from_slice(&lines[..at.start.min(lines.len())]);
    out.extend_from_slice(with);
    out.extend_from_slice(&lines[at.end.min(lines.len())..]);
    let mut s = out.join("\n");
    if !out.is_empty() && (target.ends_with('\n') || target.is_empty()) {
        s.push('\n');
    }
    s
}

/// The one char-range edit that turns `old` into `new` (common prefix and
/// suffix kept), so the copy is a single small undo step.
fn minimal_edit(old: &str, new: &str) -> Option<(usize, usize, String)> {
    if old == new {
        return None;
    }
    let (o, n): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let pre = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let max_suf = o.len().min(n.len()) - pre;
    let suf = o
        .iter()
        .rev()
        .zip(n.iter().rev())
        .take(max_suf)
        .take_while(|(a, b)| a == b)
        .count();
    Some((pre, o.len() - suf, n[pre..n.len() - suf].iter().collect()))
}

impl App {
    /// The text of `path` as Skink sees it: its tab's buffer, else the disk.
    fn side_text(&self, path: &Path) -> Result<String, String> {
        let canon = canonical(path);
        if let Some(i) = self.find_doc(|d| d.canon.as_ref() == Some(&canon))
            && let Some(d) = self.doc_at(i)
        {
            return Ok(d.editor.buffer.text());
        }
        Buffer::open(path)
            .map(|b| b.text())
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Open (or refresh) the comparison of two files.
    pub fn open_compare(&mut self, left: &Path, right: &Path) -> Result<(), String> {
        for p in [left, right] {
            if p.is_dir() {
                return Err(format!(
                    "{} is a folder: compare two files (git difftool --dir-diff is not supported yet)",
                    p.display()
                ));
            }
        }
        let (lt, rt) = (self.side_text(left)?, self.side_text(right)?);
        let (l, r) = (canonical(left), canonical(right));
        self.show_compare(
            Compare {
                left: l,
                right: r,
                hunks: Vec::new(),
            },
            &lt,
            &rt,
        );
        Ok(())
    }

    /// `skink --diff A B`: the comparison is the only tab.
    pub fn open_compare_at_start(&mut self, left: &Path, right: &Path) -> Result<(), String> {
        self.open_compare(left, right)?;
        let here = self.active;
        let scratch = (0..self.tabs.len()).find(|&i| {
            i != here
                && self.doc_at(i).is_some_and(|d| {
                    let b = &d.editor.buffer;
                    b.path().is_none() && b.title().is_none() && !b.is_dirty() && b.len_chars() == 0
                })
        });
        if let Some(i) = scratch {
            self.close_tab(i);
        }
        Ok(())
    }

    fn compare_title(c: &Compare) -> String {
        let name = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        format!("compare {} \u{2194} {}", name(&c.left), name(&c.right))
    }

    fn show_compare(&mut self, mut c: Compare, lt: &str, rt: &str) {
        let (old, new) = (git::lines(lt), git::lines(rt));
        c.hunks = git::diff(&old, &new);
        let width = (self.areas.main.width as usize).saturating_sub(8).max(40);
        let (text, view) = crate::gitui::compare_view(&old, &new, &c, width);
        let title = Self::compare_title(&c);
        let count = c.hunks.len();
        let same = |d: &crate::doc::Doc| {
            matches!(&d.kind, DocKind::Git(v) if matches!(&v.what, ViewWhat::Compare(o)
                if o.left == c.left && o.right == c.right))
        };
        match self.find_doc(same) {
            Some(i) => self.replace_git_view_at(i, &title, &text, view),
            None => self.open_git_view(&title, &text, view),
        }
        self.editor_mut().message = Some(if count == 0 {
            "The files are the same".into()
        } else {
            format!(
                "{count} change{}: n/p next/previous, < > copy to the left/right, Enter edits, s saves",
                if count == 1 { "" } else { "s" }
            )
        });
    }

    /// The comparison in the live tab, if it is one.
    fn live_compare(&self) -> Option<Compare> {
        match &self.doc.kind {
            DocKind::Git(v) => match &v.what {
                ViewWhat::Compare(c) => Some((**c).clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Re-read both sides (after an edit in their tabs, a copy or a save).
    pub(crate) fn refresh_compare(&mut self) {
        let Some(c) = self.live_compare() else {
            return;
        };
        match (self.side_text(&c.left), self.side_text(&c.right)) {
            (Ok(lt), Ok(rt)) => {
                let line = self.doc.editor.cursor.line;
                self.show_compare(c, &lt, &rt);
                self.doc.editor.goto(Some(line + 1), None);
            }
            (Err(e), _) | (_, Err(e)) => self.editor_mut().message = Some(e),
        }
    }

    /// Plain keys in a comparison tab. True when consumed.
    pub(crate) fn compare_key(&mut self, c: char, row: &Row) -> bool {
        let Some(cmp) = self.live_compare() else {
            return false;
        };
        match (c, row) {
            ('n' | ']', _) => self.compare_step(true),
            ('p' | '[', _) => self.compare_step(false),
            ('>', Row::Pair { hunk: Some(h), .. }) => self.copy_change(&cmp, *h, Toward::Right),
            ('<', Row::Pair { hunk: Some(h), .. }) => self.copy_change(&cmp, *h, Toward::Left),
            ('<' | '>', _) => {
                self.editor_mut().message = Some("Put the cursor on a change first (n / p)".into());
            }
            ('\n', Row::Pair { left, right, .. }) => match (right, left) {
                (Some(r), _) => self.jump(&cmp.right, *r, 0),
                (None, Some(l)) => self.jump(&cmp.left, *l, 0),
                (None, None) => {}
            },
            ('o', Row::Pair { left: Some(l), .. }) => self.jump(&cmp.left, *l, 0),
            ('s', _) => self.save_compare(&cmp),
            ('r', _) => self.refresh_compare(),
            _ => return false,
        }
        true
    }

    /// Move the cursor to the next / previous change.
    fn compare_step(&mut self, forward: bool) {
        let rows = match &self.doc.kind {
            DocKind::Git(v) => v.rows.clone(),
            _ => return,
        };
        let here = self.doc.editor.cursor.line;
        let hunk_at = |i: usize| match rows.get(i) {
            Some(Row::Pair { hunk, .. }) => *hunk,
            _ => None,
        };
        let cur = hunk_at(here);
        // The first row of a change other than the one under the cursor.
        let starts = |i: usize| {
            hunk_at(i).is_some() && hunk_at(i) != cur && (i == 0 || hunk_at(i - 1) != hunk_at(i))
        };
        let found = if forward {
            (here + 1..rows.len()).find(|&i| starts(i))
        } else {
            (0..here).rev().find(|&i| starts(i)).map(|mut i| {
                while i > 0 && hunk_at(i - 1) == hunk_at(i) {
                    i -= 1;
                }
                i
            })
        };
        match found {
            Some(i) => self.doc.editor.goto(Some(i + 1), Some(1)),
            None => {
                self.editor_mut().message = Some(if forward {
                    "No more changes below".into()
                } else {
                    "No more changes above".into()
                });
            }
        }
    }

    /// Copy change `h` to one side, through that file's tab.
    fn copy_change(&mut self, cmp: &Compare, h: usize, toward: Toward) {
        let Some(hunk) = cmp.hunks.get(h).cloned() else {
            return;
        };
        let (src, dst, src_range, dst_range) = match toward {
            Toward::Right => (&cmp.left, &cmp.right, hunk.old.clone(), hunk.new.clone()),
            Toward::Left => (&cmp.right, &cmp.left, hunk.new.clone(), hunk.old.clone()),
        };
        let (src_text, dst_text) = match (self.side_text(src), self.side_text(dst)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                self.editor_mut().message = Some(e);
                return;
            }
        };
        let src_lines = git::lines(&src_text);
        let with =
            &src_lines[src_range.start.min(src_lines.len())..src_range.end.min(src_lines.len())];
        let new_text = splice(&dst_text, dst_range, with);
        let Some(edit) = minimal_edit(&dst_text, &new_text) else {
            return;
        };
        let here = self.active;
        let i = match self.find_doc(|d| d.canon.as_deref() == Some(dst.as_path())) {
            Some(i) => i,
            None => match Buffer::open(dst) {
                Ok(b) => self.add_doc(Editor::new(b), false),
                Err(e) => {
                    self.editor_mut().message = Some(format!("{}: {e}", dst.display()));
                    return;
                }
            },
        };
        let Some(d) = self.doc_at_mut(i) else {
            return;
        };
        if d.editor.buffer.is_read_only() {
            let name = d.title();
            self.switch_to(here);
            self.editor_mut().message = Some(format!("{name} is read-only: nothing copied"));
            return;
        }
        d.editor.apply_edits(vec![edit]);
        let name = d.title();
        self.switch_to(here);
        self.refresh_compare();
        self.editor_mut().message = Some(format!(
            "Copied to {name} (unsaved: s saves, Ctrl-Z in its tab undoes)"
        ));
    }

    /// `s`: save the sides that have unsaved changes.
    fn save_compare(&mut self, cmp: &Compare) {
        let here = self.active;
        let mut saved = Vec::new();
        for p in [&cmp.left, &cmp.right] {
            let Some(i) = self.find_doc(|d| d.canon.as_deref() == Some(p.as_path())) else {
                continue;
            };
            if !self.doc_at(i).is_some_and(|d| d.editor.buffer.is_dirty()) {
                continue;
            }
            // Saving goes through the normal path (disk-change checks,
            // format on save, sudo offer) on the live document.
            self.switch_to(i);
            self.save_live(false);
            if self.doc.editor.buffer.is_dirty() {
                // Not saved (a question is open, or an error): stay there.
                return;
            }
            saved.push(self.doc.title());
        }
        self.switch_to(here);
        self.refresh_compare();
        let same = self.live_compare().is_some_and(|c| c.hunks.is_empty());
        self.editor_mut().message = Some(match (saved.is_empty(), same) {
            (true, _) => "Nothing unsaved on either side".into(),
            (false, true) => format!("Saved {}: the files are now the same", saved.join(" and ")),
            (false, false) => format!("Saved {}", saved.join(" and ")),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_and_minimal_edit_keep_the_rest_of_the_file() {
        assert_eq!(splice("a\nb\nc\n", 1..2, &["B", "B2"]), "a\nB\nB2\nc\n");
        assert_eq!(splice("a\nb", 1..2, &[]), "a");
        assert_eq!(splice("", 0..0, &["x"]), "x\n");
        assert_eq!(
            minimal_edit("a\nb\nc\n", "a\nX\nc\n"),
            Some((2, 3, "X".into()))
        );
        assert_eq!(minimal_edit("same", "same"), None);
    }
}
