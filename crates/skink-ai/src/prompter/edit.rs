//! Edit results the UI can show as an inline diff with per-block accept/reject.
//!
//! Line diffs come from the `similar` crate (Apache-2.0). The model here is a
//! list of [`Segment`]s: unchanged runs and *change blocks*; accepting a
//! subset of blocks reproduces exactly the text the user chose.

use std::path::PathBuf;

use similar::{DiffOp, TextDiff};

use super::coords::{CharRange, TextIndex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Lines (with their terminators) identical on both sides.
    Equal(Vec<String>),
    /// A block the user can accept (take `new`) or reject (keep `old`).
    Change { old: Vec<String>, new: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
    pub blocks: usize,
}

/// Line diff between an original region and its replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub segments: Vec<Segment>,
}

fn lines_of(s: &str) -> Vec<String> {
    s.split_inclusive('\n').map(str::to_owned).collect()
}

impl Diff {
    pub fn new(old: &str, new: &str) -> Self {
        let td = TextDiff::from_lines(old, new);
        let ol = lines_of(old);
        let nl = lines_of(new);
        let mut segments: Vec<Segment> = Vec::new();
        for op in td.ops() {
            match *op {
                DiffOp::Equal { old_index, len, .. } => {
                    segments.push(Segment::Equal(ol[old_index..old_index + len].to_vec()));
                }
                DiffOp::Delete {
                    old_index, old_len, ..
                } => push_change(
                    &mut segments,
                    ol[old_index..old_index + old_len].to_vec(),
                    Vec::new(),
                ),
                DiffOp::Insert {
                    new_index, new_len, ..
                } => push_change(
                    &mut segments,
                    Vec::new(),
                    nl[new_index..new_index + new_len].to_vec(),
                ),
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => push_change(
                    &mut segments,
                    ol[old_index..old_index + old_len].to_vec(),
                    nl[new_index..new_index + new_len].to_vec(),
                ),
            }
        }
        Self { segments }
    }

    pub fn is_empty(&self) -> bool {
        !self
            .segments
            .iter()
            .any(|s| matches!(s, Segment::Change { .. }))
    }

    /// Number of change blocks (indices passed to [`apply`](Self::apply)).
    pub fn block_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| matches!(s, Segment::Change { .. }))
            .count()
    }

    pub fn stats(&self) -> DiffStats {
        let mut st = DiffStats::default();
        for s in &self.segments {
            if let Segment::Change { old, new } = s {
                st.blocks += 1;
                st.removed += old.len();
                st.added += new.len();
            }
        }
        st
    }

    /// Build the text where change block `i` is taken from the new side iff
    /// `accept(i)` (blocks are numbered in order, from 0).
    pub fn apply(&self, accept: impl Fn(usize) -> bool) -> String {
        let mut out = String::new();
        let mut idx = 0;
        for s in &self.segments {
            match s {
                Segment::Equal(l) => l.iter().for_each(|x| out.push_str(x)),
                Segment::Change { old, new } => {
                    let side = if accept(idx) { new } else { old };
                    side.iter().for_each(|x| out.push_str(x));
                    idx += 1;
                }
            }
        }
        out
    }

    pub fn accept_all(&self) -> String {
        self.apply(|_| true)
    }

    pub fn reject_all(&self) -> String {
        self.apply(|_| false)
    }

    /// Unified-diff-like text with `context` unchanged lines around each block
    /// (for logs, tests, or a plain-text preview).
    pub fn unified(&self, context: usize) -> String {
        let mut out = String::new();
        let n = self.segments.len();
        for (i, s) in self.segments.iter().enumerate() {
            match s {
                Segment::Equal(l) => {
                    let ctx = |out: &mut String, ls: &[String]| {
                        ls.iter().for_each(|x| push_line(out, ' ', x))
                    };
                    if i == 0 && n > 1 {
                        ctx(&mut out, &l[l.len().saturating_sub(context)..]);
                    } else if i + 1 == n && n > 1 {
                        ctx(&mut out, &l[..context.min(l.len())]);
                    } else if l.len() > 2 * context && n > 1 {
                        ctx(&mut out, &l[..context]);
                        out.push_str("@@\n");
                        ctx(&mut out, &l[l.len() - context..]);
                    } else if n > 1 {
                        ctx(&mut out, l);
                    }
                }
                Segment::Change { old, new } => {
                    old.iter().for_each(|x| push_line(&mut out, '-', x));
                    new.iter().for_each(|x| push_line(&mut out, '+', x));
                }
            }
        }
        out
    }
}

fn push_line(out: &mut String, tag: char, line: &str) {
    out.push(tag);
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push_str("\n\\ No newline at end of text\n");
    }
}

fn push_change(segs: &mut Vec<Segment>, old: Vec<String>, new: Vec<String>) {
    if let Some(Segment::Change { old: o, new: n }) = segs.last_mut() {
        o.extend(old);
        n.extend(new);
    } else {
        segs.push(Segment::Change { old, new });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// The buffer no longer contains the original region (the user kept
    /// editing while the agent worked, and it cannot be relocated).
    Stale,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the text changed since the request was made")
    }
}

impl std::error::Error for ApplyError {}

/// A proposed replacement of `range` in `path`, ready for inline review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditProposal {
    pub path: PathBuf,
    /// Region of the buffer (char coordinates) that `original` came from.
    pub range: CharRange,
    pub original: String,
    pub replacement: String,
    pub diff: Diff,
}

impl EditProposal {
    pub fn new(path: PathBuf, range: CharRange, original: String, replacement: String) -> Self {
        let diff = Diff::new(&original, &replacement);
        Self {
            path,
            range,
            original,
            replacement,
            diff,
        }
    }

    /// No effective change?
    pub fn is_noop(&self) -> bool {
        self.diff.is_empty()
    }

    /// Where the original region currently sits in `buffer`: the recorded
    /// range if it still matches, else the unique occurrence of the original
    /// text, else `Stale`.
    pub fn locate(&self, buffer: &str) -> Result<CharRange, ApplyError> {
        let ix = TextIndex::new(buffer);
        if ix.slice(self.range) == self.original {
            return Ok(self.range.ordered());
        }
        if self.original.is_empty() {
            return Err(ApplyError::Stale);
        }
        let mut hits = buffer.match_indices(&self.original);
        match (hits.next(), hits.next()) {
            (Some((at, _)), None) => Ok(CharRange::new(
                ix.byte_to_pos(at),
                ix.byte_to_pos(at + self.original.len()),
            )),
            _ => Err(ApplyError::Stale),
        }
    }

    /// New buffer text with the accepted blocks applied (relocating if the
    /// buffer drifted).
    pub fn apply_to(
        &self,
        buffer: &str,
        accept: impl Fn(usize) -> bool,
    ) -> Result<String, ApplyError> {
        let r = self.locate(buffer)?;
        Ok(TextIndex::new(buffer).replace(r, &self.diff.apply(accept)))
    }

    pub fn accept_all_to(&self, buffer: &str) -> Result<String, ApplyError> {
        self.apply_to(buffer, |_| true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::coords::CharPos;
    use super::*;

    const OLD: &str = "a\nb\nc\nd\ne\n";
    const NEW: &str = "a\nB\nc\nd\nE\nf\n";

    #[test]
    fn diff_blocks_and_stats() {
        let d = Diff::new(OLD, NEW);
        assert_eq!(d.block_count(), 2);
        assert_eq!(
            d.stats(),
            DiffStats {
                added: 3,
                removed: 2,
                blocks: 2
            }
        );
        assert_eq!(d.accept_all(), NEW);
        assert_eq!(d.reject_all(), OLD);
        assert!(!d.is_empty());
    }

    #[test]
    fn per_block_accept() {
        let d = Diff::new(OLD, NEW);
        assert_eq!(d.apply(|i| i == 0), "a\nB\nc\nd\ne\n");
        assert_eq!(d.apply(|i| i == 1), "a\nb\nc\nd\nE\nf\n");
    }

    #[test]
    fn identical_is_empty() {
        let d = Diff::new(OLD, OLD);
        assert!(d.is_empty());
        assert_eq!(d.block_count(), 0);
        assert_eq!(d.accept_all(), OLD);
        let d = Diff::new("", "");
        assert!(d.is_empty());
    }

    #[test]
    fn pure_insert_delete_and_missing_trailing_newline() {
        let d = Diff::new("", "x\ny\n");
        assert_eq!(d.stats().added, 2);
        assert_eq!(d.accept_all(), "x\ny\n");
        let d = Diff::new("x\ny", "x\ny\nz");
        assert_eq!(d.accept_all(), "x\ny\nz");
        assert_eq!(d.reject_all(), "x\ny");
        let d = Diff::new("keep\ngone\n", "keep\n");
        assert_eq!(d.accept_all(), "keep\n");
        assert_eq!(d.reject_all(), "keep\ngone\n");
    }

    #[test]
    fn unified_text() {
        let d = Diff::new("1\n2\n3\n4\n5\n6\n7\n8\n", "1\n2\n3\n4\nFIVE\n6\n7\n8\n");
        assert_eq!(d.unified(1), " 4\n-5\n+FIVE\n 6\n");
        let d = Diff::new(
            "1\n2\n3\n4\n5\nx\n6\n7\n8\n9\n",
            "1\n2\n3\n4\n5\ny\n6\n7\n8\n9\n",
        );
        assert_eq!(d.unified(2), " 4\n 5\n-x\n+y\n 6\n 7\n");
    }

    #[test]
    fn crlf_roundtrips() {
        let old = "a\r\nb\r\n";
        let new = "a\r\nB\r\n";
        let d = Diff::new(old, new);
        assert_eq!(d.accept_all(), new);
        assert_eq!(d.reject_all(), old);
    }

    fn proposal() -> EditProposal {
        EditProposal::new(
            "f.rs".into(),
            CharRange::new(CharPos::new(1, 0), CharPos::new(3, 0)),
            "b\nc\n".into(),
            "B\nc\nC\n".into(),
        )
    }

    #[test]
    fn apply_in_place() {
        let p = proposal();
        assert_eq!(p.accept_all_to("a\nb\nc\nd\n").unwrap(), "a\nB\nc\nC\nd\n");
        assert_eq!(
            p.apply_to("a\nb\nc\nd\n", |_| false).unwrap(),
            "a\nb\nc\nd\n"
        );
    }

    #[test]
    fn apply_relocates_when_buffer_drifted() {
        let p = proposal();
        // two lines were inserted above while the agent worked
        assert_eq!(
            p.accept_all_to("new1\nnew2\na\nb\nc\nd\n").unwrap(),
            "new1\nnew2\na\nB\nc\nC\nd\n"
        );
    }

    #[test]
    fn apply_detects_stale_and_ambiguous() {
        let p = proposal();
        assert_eq!(p.accept_all_to("a\nx\ny\nd\n"), Err(ApplyError::Stale));
        // ambiguous: original text occurs twice and the recorded range no longer matches
        assert_eq!(
            p.accept_all_to("zz\nzz\nb\nc\nxx\nb\nc\n"),
            Err(ApplyError::Stale)
        );
    }

    #[test]
    fn noop_proposal() {
        let p = EditProposal::new("f".into(), CharRange::default(), "x\n".into(), "x\n".into());
        assert!(p.is_noop());
    }
}
