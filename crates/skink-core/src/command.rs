use crate::{Editor, Order, Request, Resolution, Scope};
use std::collections::HashMap;

/// A named editor action. Keymaps, the future palette and scripting all
/// resolve names through the same [`CommandRegistry`]. Returns whether it
/// did something, so Micro-style `A|B|C` fallbacks can try the next one.
pub struct Command {
    /// Micro-compatible name, e.g. `CursorDown`.
    pub name: &'static str,
    pub description: &'static str,
    pub run: fn(&mut Editor) -> bool,
}

pub struct CommandRegistry {
    commands: Vec<Command>,
    index: HashMap<&'static str, usize>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            index: HashMap::new(),
        }
    }

    pub fn register(&mut self, cmd: Command) {
        self.index.insert(cmd.name, self.commands.len());
        self.commands.push(cmd);
    }

    pub fn get(&self, name: &str) -> Option<&Command> {
        self.index.get(name).map(|&i| &self.commands[i])
    }

    /// Run one command by name; false if unknown or it declined.
    pub fn run(&self, name: &str, editor: &mut Editor) -> bool {
        self.get(name).is_some_and(|c| (c.run)(editor))
    }

    /// Run a Micro action string: `,` separates commands that all run,
    /// `|` separates fallbacks where the first that succeeds wins.
    pub fn run_chain(&self, chain: &str, editor: &mut Editor) -> bool {
        let mut any = false;
        for part in chain.split(',') {
            any |= part.split('|').any(|n| self.run(n.trim(), editor));
        }
        any
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.commands.iter()
    }

    /// The built-in command set (names follow Micro's actions).
    pub fn builtin() -> Self {
        let mut r = Self::new();
        let mut add = |name, description, run| {
            r.register(Command {
                name,
                description,
                run,
            })
        };

        add("Quit", "Quit the editor", |e| {
            e.request = Some(Request::Quit);
            true
        });
        add("Save", "Save the file", |e| {
            e.save();
            true
        });
        add("Find", "Search in the file", |e| {
            e.request = Some(Request::Find);
            true
        });
        add("FindNext", "Next match", |e| {
            let q = e.last_search.clone();
            e.find(&q, true)
        });
        add("FindPrevious", "Previous match", |e| {
            let q = e.last_search.clone();
            e.find(&q, false)
        });
        add("Undo", "Undo", |e| {
            e.undo();
            true
        });
        add("Redo", "Redo", |e| {
            e.redo();
            true
        });

        // Cursor movement and selection.
        add("CursorUp", "Move up", |e| {
            mv(e, |e| e.move_vertical(-1, false))
        });
        add("CursorDown", "Move down", |e| {
            mv(e, |e| e.move_vertical(1, false))
        });
        add("CursorLeft", "Move left", |e| mv(e, |e| e.move_left(false)));
        add("CursorRight", "Move right", |e| {
            mv(e, |e| e.move_right(false))
        });
        add("SelectUp", "Select up", |e| {
            mv(e, |e| e.move_vertical(-1, true))
        });
        add("SelectDown", "Select down", |e| {
            mv(e, |e| e.move_vertical(1, true))
        });
        add("SelectLeft", "Select left", |e| {
            mv(e, |e| e.move_left(true))
        });
        add("SelectRight", "Select right", |e| {
            mv(e, |e| e.move_right(true))
        });
        add("WordLeft", "Move a word left", |e| {
            mv(e, |e| e.move_word_left(false))
        });
        add("WordRight", "Move a word right", |e| {
            mv(e, |e| e.move_word_right(false))
        });
        add("SelectWordLeft", "Select a word left", |e| {
            mv(e, |e| e.move_word_left(true))
        });
        add("SelectWordRight", "Select a word right", |e| {
            mv(e, |e| e.move_word_right(true))
        });
        add("StartOfLine", "Start of line", |e| {
            mv(e, |e| e.move_line_start(false))
        });
        add("StartOfText", "First non-blank", |e| {
            mv(e, |e| e.move_line_start_text(false))
        });
        add("StartOfTextToggle", "First non-blank / column 0", |e| {
            mv(e, |e| e.move_line_start_text(false))
        });
        add("EndOfLine", "End of line", |e| {
            mv(e, |e| e.move_line_end(false))
        });
        add("SelectToStartOfTextToggle", "Select to line start", |e| {
            mv(e, |e| e.move_line_start_text(true))
        });
        add("SelectToEndOfLine", "Select to line end", |e| {
            mv(e, |e| e.move_line_end(true))
        });
        add("CursorStart", "Start of file", |e| {
            mv(e, |e| e.move_file_start(false))
        });
        add("CursorEnd", "End of file", |e| {
            mv(e, |e| e.move_file_end(false))
        });
        add("SelectToStart", "Select to file start", |e| {
            mv(e, |e| e.move_file_start(true))
        });
        add("SelectToEnd", "Select to file end", |e| {
            mv(e, |e| e.move_file_end(true))
        });
        add("CursorPageUp", "Page up", |e| {
            mv(e, |e| e.move_vertical(-page(e), false))
        });
        add("CursorPageDown", "Page down", |e| {
            mv(e, |e| e.move_vertical(page(e), false))
        });
        add("SelectPageUp", "Select page up", |e| {
            mv(e, |e| e.move_vertical(-page(e), true))
        });
        add("SelectPageDown", "Select page down", |e| {
            mv(e, |e| e.move_vertical(page(e), true))
        });
        add("SelectAll", "Select all", |e| mv(e, Editor::select_all));
        add("Deselect", "Clear selection", |e| {
            let had = e.anchor.is_some() || e.secondary().iter().any(|s| s.anchor.is_some());
            e.each_cursor(Scope::Cursors, |e, _| e.anchor = None);
            had
        });
        add("Escape", "Cancel", |e| {
            e.message = None;
            true
        });
        add("ClearInfo", "Clear the status message", |e| {
            e.message = None;
            true
        });

        // Editing.
        add("InsertNewline", "Insert a newline", |e| {
            ed(e, Editor::insert_newline)
        });
        add("InsertTab", "Insert a tab", |e| ed(e, Editor::insert_tab));
        add("Backspace", "Delete backwards", |e| {
            ed(e, Editor::backspace)
        });
        add("Delete", "Delete forwards", |e| {
            ed(e, Editor::delete_forward)
        });
        add("DeleteWordLeft", "Delete word left", |e| {
            ed(e, Editor::delete_word_left)
        });
        add("DeleteWordRight", "Delete word right", |e| {
            ed(e, Editor::delete_word_right)
        });
        add("IndentSelection", "Indent selected lines", |e| {
            e.anchor.is_some() && lines(e, Order::BottomUp, |e| e.indent_lines(true))
        });
        add("OutdentSelection", "Outdent selected lines", |e| {
            e.anchor.is_some() && lines(e, Order::BottomUp, |e| e.indent_lines(false))
        });
        add("OutdentLine", "Outdent the line", |e| {
            lines(e, Order::BottomUp, |e| e.indent_lines(false))
        });
        add("MoveLinesUp", "Move lines up", |e| {
            lines(e, Order::TopDown, |e| e.move_lines(true))
        });
        add("MoveLinesDown", "Move lines down", |e| {
            lines(e, Order::BottomUp, |e| e.move_lines(false))
        });
        add("Duplicate", "Duplicate the selection", |e| {
            e.selection().is_some()
                && ed(e, |e| {
                    let Some((_, end)) = e.selection() else {
                        return;
                    };
                    let t = e.buffer.slice(e.selection().map_or(end, |s| s.0), end);
                    e.anchor = None;
                    e.cursor = e.cursor_at(end);
                    e.insert_text(&t);
                })
        });
        add("DuplicateLine", "Duplicate the line", |e| {
            lines(e, Order::BottomUp, Editor::duplicate_line)
        });
        add("Copy", "Copy the selection", |e| e.copy());
        add("CopyLine", "Copy the line", |e| {
            e.copy_line();
            true
        });
        add("Cut", "Cut the selection", |e| e.cut());
        add("CutLine", "Cut the line", |e| {
            lines(e, Order::BottomUp, Editor::cut_line)
        });
        add("Paste", "Paste", |e| once(e, Editor::paste));

        // Multiple cursors (Micro's names).
        add(
            "SpawnMultiCursor",
            "Select the word, then add a cursor at the next match",
            Editor::spawn_cursor_next_match,
        );
        add(
            "SpawnMultiCursorUp",
            "Add a cursor on the line above",
            |e| e.spawn_cursor_vertical(-1),
        );
        add(
            "SpawnMultiCursorDown",
            "Add a cursor on the line below",
            |e| e.spawn_cursor_vertical(1),
        );
        add(
            "SkipMultiCursor",
            "Skip this match and select the next",
            Editor::skip_cursor_match,
        );
        add(
            "RemoveMultiCursor",
            "Remove the newest cursor",
            Editor::remove_primary_cursor,
        );
        add(
            "RemoveAllMultiCursors",
            "Back to a single cursor",
            Editor::remove_all_cursors,
        );
        add("ConflictNext", "Next merge conflict", |e| {
            e.goto_conflict(true)
        });
        add("ConflictPrevious", "Previous merge conflict", |e| {
            e.goto_conflict(false)
        });
        add("AcceptOurs", "Resolve conflict: keep ours", |e| {
            e.resolve_conflict(Resolution::Ours)
        });
        add("AcceptTheirs", "Resolve conflict: keep theirs", |e| {
            e.resolve_conflict(Resolution::Theirs)
        });
        add("AcceptBoth", "Resolve conflict: keep both", |e| {
            e.resolve_conflict(Resolution::Both)
        });
        r
    }
}

fn page(e: &Editor) -> isize {
    e.viewport_height.saturating_sub(1).max(1) as isize
}

/// Movement helper: runs at every cursor; always "handled".
fn mv(e: &mut Editor, f: impl Fn(&mut Editor)) -> bool {
    e.each_cursor(Scope::Cursors, |e, _| f(e));
    true
}

/// Editing helper: runs at every cursor; handled unless read-only.
fn ed(e: &mut Editor, f: impl Fn(&mut Editor)) -> bool {
    writable(e) && {
        e.each_cursor(Scope::Cursors, |e, _| f(e));
        true
    }
}

/// Line-based editing helper: runs once per line span of the cursors.
fn lines(e: &mut Editor, order: Order, f: impl Fn(&mut Editor)) -> bool {
    writable(e) && {
        e.each_cursor(Scope::Lines(order), |e, _| f(e));
        true
    }
}

/// Editing helper for operations that handle all cursors themselves.
fn once(e: &mut Editor, f: impl FnOnce(&mut Editor)) -> bool {
    writable(e) && {
        f(e);
        true
    }
}

fn writable(e: &mut Editor) -> bool {
    if e.buffer.is_read_only() {
        e.message = Some("Buffer is read-only".into());
        return false;
    }
    true
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Buffer;

    fn ed(text: &str) -> Editor {
        Editor::new(Buffer::from_text(text))
    }

    #[test]
    fn run_by_name() {
        let r = CommandRegistry::builtin();
        let mut e = ed("a\nb\nc");
        assert!(r.run("CursorDown", &mut e));
        assert_eq!(e.cursor.line, 1);
        assert!(!r.run("Nope", &mut e));
        assert!(r.run("Quit", &mut e));
        assert_eq!(e.request, Some(Request::Quit));
    }

    #[test]
    fn fallback_chains() {
        let r = CommandRegistry::builtin();
        let mut e = ed("hello\nworld");
        // No selection: Copy declines, CopyLine runs.
        assert!(r.run_chain("Copy|CopyLine", &mut e));
        assert_eq!(e.clipboard, "hello\n");
        // Tab with a selection indents, without inserts a tab.
        assert!(r.run_chain("Autocomplete|IndentSelection|InsertTab", &mut e));
        assert_eq!(e.buffer.text(), "\thello\nworld");
        e.select_all();
        assert!(r.run_chain("Autocomplete|IndentSelection|InsertTab", &mut e));
        assert_eq!(e.buffer.text(), "\t\thello\n\tworld");
    }

    #[test]
    fn comma_runs_all() {
        let r = CommandRegistry::builtin();
        let mut e = ed("abc");
        e.select_all();
        assert!(r.run_chain("Escape,Deselect", &mut e));
        assert!(e.anchor.is_none());
    }

    #[test]
    fn file_start_and_end() {
        let r = CommandRegistry::builtin();
        let mut e = ed("a\nbcd");
        r.run("CursorEnd", &mut e);
        assert_eq!((e.cursor.line, e.cursor.col), (1, 3));
        r.run("CursorStart", &mut e);
        assert_eq!((e.cursor.line, e.cursor.col), (0, 0));
    }

    #[test]
    fn page_moves_by_viewport() {
        let r = CommandRegistry::builtin();
        let mut e = ed(&"x\n".repeat(100));
        e.set_viewport_height(10);
        r.run("CursorPageDown", &mut e);
        assert_eq!(e.cursor.line, 9);
    }
}
