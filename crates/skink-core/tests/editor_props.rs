//! Property tests: random editing sessions keep the editor's invariants.
//! `cargo test -p skink-core --test editor_props -- --ignored` runs a long
//! version (thousands of sessions).

use proptest::prelude::*;
use skink_core::{Buffer, Editor};

/// Pieces of text that trip editors: combining marks, ZWJ emoji, wide CJK,
/// CRLF, tabs, brackets and quotes (auto-pairs), lone newlines.
const PIECES: &[&str] = &[
    "a",
    "b",
    " ",
    "\n",
    "\t",
    "é",
    "e\u{301}",
    "👩\u{200d}💻",
    "漢字",
    "\r\n",
    "(",
    ")",
    "{",
    "}",
    "\"",
    "'",
    "x\ny",
    "",
];

#[derive(Debug, Clone)]
enum Op {
    Type(usize),
    Paste(usize),
    Backspace,
    Delete,
    DeleteWordLeft,
    Left(bool),
    Right(bool),
    Vertical(i8, bool),
    WordLeft(bool),
    WordRight(bool),
    LineStart,
    LineEnd,
    FileStart,
    FileEnd,
    SelectAll,
    /// Anywhere, including far past the end.
    Goto(usize, usize),
    Click(usize, usize),
    CursorBelow,
    CursorNextMatch,
    OneCursor,
    Indent(bool),
    MoveLines(bool),
    Duplicate,
    Cut,
    CopyPaste,
    Undo,
    Redo,
}

fn op() -> impl Strategy<Value = Op> {
    let n = PIECES.len();
    prop_oneof![
        4 => (0..n).prop_map(Op::Type),
        1 => (0..n).prop_map(Op::Paste),
        2 => Just(Op::Backspace),
        1 => Just(Op::Delete),
        1 => Just(Op::DeleteWordLeft),
        1 => any::<bool>().prop_map(Op::Left),
        1 => any::<bool>().prop_map(Op::Right),
        1 => (-3i8..=3, any::<bool>()).prop_map(|(d, s)| Op::Vertical(d, s)),
        1 => any::<bool>().prop_map(Op::WordLeft),
        1 => any::<bool>().prop_map(Op::WordRight),
        1 => Just(Op::LineStart),
        1 => Just(Op::LineEnd),
        1 => Just(Op::FileStart),
        1 => Just(Op::FileEnd),
        1 => Just(Op::SelectAll),
        1 => (0usize..40, 0usize..40).prop_map(|(l, c)| Op::Goto(l, c)),
        1 => (0usize..40, 0usize..40).prop_map(|(l, c)| Op::Click(l, c)),
        1 => Just(Op::CursorBelow),
        1 => Just(Op::CursorNextMatch),
        1 => Just(Op::OneCursor),
        1 => any::<bool>().prop_map(Op::Indent),
        1 => any::<bool>().prop_map(Op::MoveLines),
        1 => Just(Op::Duplicate),
        1 => Just(Op::Cut),
        1 => Just(Op::CopyPaste),
        2 => Just(Op::Undo),
        1 => Just(Op::Redo),
    ]
}

fn apply(e: &mut Editor, op: &Op) {
    match *op {
        Op::Type(i) => e.insert_text(PIECES[i]),
        Op::Paste(i) => {
            e.clipboard = PIECES[i].to_owned();
            e.paste();
        }
        Op::Backspace => e.backspace(),
        Op::Delete => e.delete_forward(),
        Op::DeleteWordLeft => e.delete_word_left(),
        Op::Left(s) => e.move_left(s),
        Op::Right(s) => e.move_right(s),
        Op::Vertical(d, s) => e.move_vertical(d as isize, s),
        Op::WordLeft(s) => e.move_word_left(s),
        Op::WordRight(s) => e.move_word_right(s),
        Op::LineStart => e.move_line_start(false),
        Op::LineEnd => e.move_line_end(false),
        Op::FileStart => e.move_file_start(false),
        Op::FileEnd => e.move_file_end(false),
        Op::SelectAll => e.select_all(),
        Op::Goto(l, c) => e.goto(Some(l), Some(c)),
        Op::Click(l, c) => e.click(l, c, false),
        Op::CursorBelow => {
            e.spawn_cursor_vertical(1);
        }
        Op::CursorNextMatch => {
            e.spawn_cursor_next_match();
        }
        Op::OneCursor => {
            e.remove_all_cursors();
        }
        Op::Indent(i) => e.indent_lines(i),
        Op::MoveLines(up) => e.move_lines(up),
        Op::Duplicate => e.duplicate_line(),
        Op::Cut => {
            e.cut();
        }
        Op::CopyPaste => {
            e.copy();
            e.paste();
        }
        Op::Undo => e.undo(),
        Op::Redo => e.redo(),
    }
}

fn check_cursor(e: &Editor, after: &Op) {
    let b = &e.buffer;
    let c = e.cursor;
    assert!(
        c.line < b.len_lines(),
        "line {} of {} after {after:?}",
        c.line,
        b.len_lines()
    );
    assert!(
        c.col <= b.line_len(c.line),
        "col {} past line {} ({} chars) after {after:?}",
        c.col,
        c.line,
        b.line_len(c.line)
    );
}

/// Undo until nothing is left; the count is bounded so a loop can't hang.
fn undo_all(e: &mut Editor) {
    for _ in 0..10_000 {
        e.message = None;
        e.undo();
        if e.message.as_deref() == Some("Nothing to undo") {
            return;
        }
    }
    panic!("undo never ran out");
}

fn redo_all(e: &mut Editor) {
    for _ in 0..10_000 {
        e.message = None;
        e.redo();
        if e.message.as_deref() == Some("Nothing to redo") {
            return;
        }
    }
    panic!("redo never ran out");
}

fn session(start: &str, ops: &[Op]) {
    let mut e = Editor::new(Buffer::from_text(start));
    let original = e.buffer.text();
    for op in ops {
        apply(&mut e, op);
        check_cursor(&e, op);
    }
    // Undo must walk back to the exact starting text, redo to the end. Only
    // valid without a trailing undo/redo mix confusing "the end": take the
    // text after a final redo_all as the end state.
    redo_all(&mut e);
    let end = e.buffer.text();
    undo_all(&mut e);
    assert_eq!(e.buffer.text(), original, "undo all != original");
    redo_all(&mut e);
    assert_eq!(e.buffer.text(), end, "redo all != end");
    check_cursor(&e, &Op::Redo);
}

const STARTS: &[&str] = &[
    "",
    "fn main() {\n    let x = 1;\n}\n",
    "a\r\nb\r\n\r\nc",
    "👩\u{200d}💻 e\u{301} 漢字\n\ttab\n",
    "no newline at end",
];

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn random_sessions_keep_invariants(start in 0..STARTS.len(), ops in prop::collection::vec(op(), 1..60)) {
        session(STARTS[start], &ops);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// A CRLF file stays CRLF on disk whatever is typed (including "\r\n"
    /// and lone "\n" pieces).
    #[test]
    fn crlf_files_save_as_crlf(ops in prop::collection::vec(op(), 1..40)) {
        let dir = std::env::temp_dir().join(format!("skink-props-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("crlf-{}.txt", ops.len()));
        std::fs::write(&path, "one\r\ntwo\r\n").unwrap();
        let mut e = Editor::new(Buffer::open(&path).unwrap());
        for op in &ops {
            apply(&mut e, op);
        }
        e.buffer.save().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                prop_assert!(i > 0 && bytes[i - 1] == b'\r', "lone LF at {i}: {:?}", String::from_utf8_lossy(&bytes));
            }
        }
    }
}

#[test]
#[ignore = "long run: cargo test -p skink-core --test editor_props -- --ignored"]
fn long_random_sessions() {
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig {
        cases: 5000,
        ..ProptestConfig::default()
    });
    let strat = (0..STARTS.len(), prop::collection::vec(op(), 1..200));
    runner
        .run(&strat, |(start, ops)| {
            session(STARTS[start], &ops);
            Ok(())
        })
        .unwrap();
}

#[test]
fn moving_lines_down_with_a_selection_ending_at_column_0_keeps_the_cursor_in_the_text() {
    // Found by the long run: the cursor went one line past the end.
    use Op::*;
    session(
        "no newline at end",
        &[
            Type(0),
            FileEnd,
            Duplicate,
            Type(4),
            MoveLines(true),
            Right(true),
            MoveLines(false),
        ],
    );
}
