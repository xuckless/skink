use crate::Editor;
use std::collections::HashMap;

/// A named editor action. Keymaps, the future palette and scripting all
/// resolve names through the same [`CommandRegistry`].
pub struct Command {
    /// Micro-compatible name, e.g. `CursorDown`.
    pub name: &'static str,
    pub description: &'static str,
    pub run: fn(&mut Editor),
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

    /// Run a command by name; returns false if it does not exist.
    pub fn run(&self, name: &str, editor: &mut Editor) -> bool {
        match self.get(name) {
            Some(c) => {
                (c.run)(editor);
                true
            }
            None => false,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.commands.iter()
    }

    /// The built-in command set.
    pub fn builtin() -> Self {
        let mut r = Self::new();
        let mut add = |name, description, run| {
            r.register(Command {
                name,
                description,
                run,
            })
        };
        add("Quit", "Quit the editor", |e| e.quit = true);
        add("CursorUp", "Move the cursor up", |e| e.move_vertical(-1));
        add("CursorDown", "Move the cursor down", |e| e.move_vertical(1));
        add("CursorLeft", "Move the cursor left", Editor::move_left);
        add("CursorRight", "Move the cursor right", Editor::move_right);
        add("CursorPageUp", "Move up one page", |e| {
            e.move_vertical(-(e.viewport_height as isize))
        });
        add("CursorPageDown", "Move down one page", |e| {
            e.move_vertical(e.viewport_height as isize)
        });
        add("StartOfLine", "Move to the start of the line", |e| {
            e.move_to_col(0)
        });
        add(
            "EndOfLine",
            "Move to the end of the line",
            Editor::move_line_end,
        );
        add("CursorStart", "Move to the start of the file", |e| {
            e.move_to_col(0);
            e.move_to_line(0);
        });
        add("CursorEnd", "Move to the end of the file", |e| {
            e.move_to_line(usize::MAX);
            e.move_line_end();
        });
        r
    }
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

    #[test]
    fn run_by_name() {
        let r = CommandRegistry::builtin();
        let mut e = Editor::new(Buffer::from_text("a\nb\nc"));
        assert!(r.run("CursorDown", &mut e));
        assert_eq!(e.cursor.line, 1);
        assert!(!r.run("Nope", &mut e));
        assert!(r.run("Quit", &mut e) && e.quit);
    }

    #[test]
    fn file_start_and_end() {
        let r = CommandRegistry::builtin();
        let mut e = Editor::new(Buffer::from_text("a\nbcd"));
        r.run("CursorEnd", &mut e);
        assert_eq!((e.cursor.line, e.cursor.col), (1, 3));
        r.run("CursorStart", &mut e);
        assert_eq!((e.cursor.line, e.cursor.col), (0, 0));
    }
}
