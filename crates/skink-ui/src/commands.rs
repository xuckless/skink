//! Command-line mode (Ctrl-E) and app-level actions. Each command is one
//! table row holding its name, docs *and* handler, so `help`, completion and
//! dispatch are generated from the same data and cannot drift apart.

use crate::{App, prompt::PromptKind};
use skink_core::CommandRegistry;
use skink_term::CommandSpec;
use std::path::Path;

/// A `:` command.
pub struct Cmd {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: &'static str,
    pub about: &'static str,
    run: fn(&mut App, &[&str]),
}

impl Cmd {
    fn names(&self) -> impl Iterator<Item = &'static str> {
        std::iter::once(self.name).chain(self.aliases.iter().copied())
    }
}

/// Arguments joined back into one string (shell command lines).
fn rest(args: &[&str]) -> String {
    args.join(" ")
}

pub static COMMANDS: &[Cmd] = &[
    Cmd {
        name: "help",
        aliases: &["docs"],
        args: "[topic]",
        about: "open this documentation in a tab (topics: tabs, editing, conflicts, settings, ...)",
        run: |app, args| app.open_help(args.first().copied()),
    },
    Cmd {
        name: "open",
        aliases: &["e", "tabnew"],
        args: "[FILE[:LINE[:COL]]]",
        about: "open a file or folder in a tab (no argument: new empty tab)",
        run: |app, args| match args {
            [] => app.new_scratch_tab(),
            _ => app.open_location(&rest(args)),
        },
    },
    Cmd {
        name: "goto",
        aliases: &[],
        args: "LINE[:COL]",
        about: "jump to a line",
        run: |app, args| {
            let mut it = args.first().copied().unwrap_or("").split(':');
            let line = it.next().and_then(|s| s.parse().ok());
            let col = it.next().and_then(|s| s.parse().ok());
            let ed = app.editor_mut();
            match line {
                Some(_) => ed.goto(line, col),
                None => ed.message = Some("usage: goto LINE[:COL]".into()),
            }
        },
    },
    Cmd {
        name: "save",
        aliases: &["w"],
        args: "",
        about: "save the file",
        run: |app, _| app.run_chain("Save"),
    },
    Cmd {
        name: "quit",
        aliases: &["q"],
        args: "",
        about: "close the tab (quits on the last document tab)",
        run: |app, _| app.request_close(app.active),
    },
    Cmd {
        name: "qa",
        aliases: &["quitall"],
        args: "",
        about: "quit everything (refuses when tabs have unsaved changes)",
        run: |app, _| {
            if app.any_dirty() {
                app.editor_mut().message =
                    Some("Unsaved changes in some tabs (:q them one by one)".into());
            } else {
                app.quit = true;
            }
        },
    },
    Cmd {
        name: "tab",
        aliases: &[],
        args: "N",
        about: "select tab N",
        run: |app, args| {
            if let Some(n) = args.first().and_then(|s| s.parse::<usize>().ok()) {
                app.switch_to(n.saturating_sub(1));
            }
        },
    },
    Cmd {
        name: "term",
        aliases: &["terminal", "agent"],
        args: "[COMMAND...]",
        about: "new terminal tab running COMMAND (default: your shell), e.g. `term claude`",
        run: |app, args| {
            let spec = match args.split_first() {
                Some((prog, more)) => CommandSpec::new(*prog).args(more.iter().copied()),
                None => CommandSpec::default_shell(),
            };
            app.open_terminal_or_report(spec);
        },
    },
    Cmd {
        name: "tree",
        aliases: &[],
        args: "[collapse|expand|hide]",
        about: "show / focus the file tree (F8); collapse all, expand the selected folder, hide",
        run: |app, args| match args {
            ["collapse"] => app.tree_op(crate::tree::FileTree::collapse_all),
            ["expand"] => app.tree_op(crate::tree::FileTree::expand_recursive),
            ["hide"] => app.set_sidebar_view(crate::SidebarView::Hidden),
            _ => app.toggle_tree(),
        },
    },
    Cmd {
        name: "new",
        aliases: &["touch"],
        args: "[PATH]",
        about: "create a file (parents too) and open it; relative to the project root",
        run: |app, args| app.new_entry_command(args, crate::tree::NewEntry::File),
    },
    Cmd {
        name: "mkdir",
        aliases: &[],
        args: "[PATH]",
        about: "create a folder (parents too); relative to the project root",
        run: |app, args| app.new_entry_command(args, crate::tree::NewEntry::Folder),
    },
    Cmd {
        name: "recent",
        aliases: &["quickopen"],
        args: "",
        about: "quick open: tabs, recent files, project files (Alt-P)",
        run: |app, _| app.open_picker(),
    },
    Cmd {
        name: "upgrade",
        aliases: &["update"],
        args: "",
        about: "download and install the latest skink release (runs from the next start)",
        run: |app, _| app.begin_upgrade(),
    },
    Cmd {
        name: "run",
        aliases: &["!"],
        args: "COMMAND...",
        about: "run a shell command in the project root, output in a tab",
        run: |app, args| match args {
            [] => app.editor_mut().message = Some("usage: run COMMAND".into()),
            _ => {
                let c = rest(args);
                app.run_to_view(&c, &format!("$ {c}"));
            }
        },
    },
    Cmd {
        name: "diff",
        aliases: &[],
        args: "",
        about: "show `git diff` in a tab",
        run: |app, _| app.run_to_view("git --no-pager diff", "git diff"),
    },
    Cmd {
        name: "gitstatus",
        aliases: &["status"],
        args: "",
        about: "show `git status` in a tab",
        run: |app, _| app.run_to_view("git --no-pager status -sb", "git status"),
    },
    Cmd {
        name: "git",
        aliases: &[],
        args: "ARGS...",
        about: "run git, output in a tab",
        run: |app, args| {
            let c = format!("git --no-pager {}", rest(args));
            app.run_to_view(c.trim_end(), &format!("$ {}", c.trim_end()));
        },
    },
    Cmd {
        name: "conflicts",
        aliases: &[],
        args: "",
        about: "open all git-conflicted files at the first conflict",
        run: |app, _| app.open_conflicts(),
    },
    Cmd {
        name: "nextconflict",
        aliases: &[],
        args: "",
        about: "next merge conflict",
        run: |app, _| app.run_chain("ConflictNext"),
    },
    Cmd {
        name: "prevconflict",
        aliases: &[],
        args: "",
        about: "previous merge conflict",
        run: |app, _| app.run_chain("ConflictPrevious"),
    },
    Cmd {
        name: "ours",
        aliases: &[],
        args: "",
        about: "resolve the conflict under the cursor: keep ours",
        run: |app, _| app.run_chain("AcceptOurs"),
    },
    Cmd {
        name: "theirs",
        aliases: &[],
        args: "",
        about: "resolve the conflict under the cursor: keep theirs",
        run: |app, _| app.run_chain("AcceptTheirs"),
    },
    Cmd {
        name: "both",
        aliases: &[],
        args: "",
        about: "resolve the conflict under the cursor: keep both",
        run: |app, _| app.run_chain("AcceptBoth"),
    },
    Cmd {
        name: "settings",
        aliases: &[],
        args: "",
        about: "open settings.json (created with defaults if missing)",
        run: |app, _| app.open_config_file("settings.json", skink_config::DEFAULT_SETTINGS),
    },
    Cmd {
        name: "bindings",
        aliases: &["keys"],
        args: "",
        about: "open bindings.json (Micro format: key -> action)",
        run: |app, _| app.open_config_file("bindings.json", skink_config::DEFAULT_BINDINGS_FILE),
    },
    Cmd {
        name: "reload",
        aliases: &[],
        args: "",
        about: "reload settings.json and bindings.json",
        run: |app, _| {
            let errs = app.load_user_config();
            app.editor_mut().message = Some(if errs.is_empty() {
                "Config reloaded".into()
            } else {
                errs.join("; ")
            });
        },
    },
];

/// Actions that act on the app rather than the buffer. They can be bound
/// to keys and chained exactly like editor actions.
pub struct AppAction {
    pub name: &'static str,
    pub about: &'static str,
    run: fn(&mut App),
}

pub static APP_ACTIONS: &[AppAction] = &[
    AppAction {
        name: "PreviousTab",
        about: "Select the tab to the left",
        run: |app| app.next_tab(-1),
    },
    AppAction {
        name: "NextTab",
        about: "Select the tab to the right",
        run: |app| app.next_tab(1),
    },
    AppAction {
        name: "FirstTab",
        about: "Select the first tab",
        run: |app| app.switch_to(0),
    },
    AppAction {
        name: "LastTab",
        about: "Select the last tab",
        run: |app| app.switch_to(app.tabs.len() - 1),
    },
    AppAction {
        name: "AddTab",
        about: "New empty document tab",
        run: App::new_scratch_tab,
    },
    AppAction {
        name: "OpenFile",
        about: "Open a file or folder by path",
        run: |app| app.open_prompt(PromptKind::Open, "Open: ", ""),
    },
    AppAction {
        name: "QuickOpen",
        about: "Quick open: tabs, recent and project files",
        run: |app| app.open_picker(),
    },
    AppAction {
        name: "ToggleTree",
        about: "Show / focus the file tree",
        run: App::toggle_tree,
    },
    AppAction {
        name: "ToggleSidebar",
        about: "Hide / show the file tree sidebar",
        run: App::toggle_sidebar,
    },
    AppAction {
        name: "ToggleTerminal",
        about: "Jump between the document and a terminal tab",
        run: App::toggle_terminal,
    },
    AppAction {
        name: "NewTerminal",
        about: "Open another terminal tab",
        run: |app| app.open_terminal_or_report(CommandSpec::default_shell()),
    },
    AppAction {
        name: "CloseTerminal",
        about: "Close the active terminal tab",
        run: App::close_terminal,
    },
];

/// Run an app action by name; false if `name` is not one.
pub fn run_app_action(app: &mut App, name: &str) -> bool {
    match APP_ACTIONS.iter().find(|a| a.name == name) {
        Some(a) => {
            (a.run)(app);
            true
        }
        None => false,
    }
}

/// Commands whose argument is a path (for Tab completion).
pub fn takes_path(cmd: &str) -> bool {
    COMMANDS
        .iter()
        .any(|c| c.name == "open" && c.names().any(|n| n == cmd))
}

impl App {
    /// Run a command-line line: a `:` command, or any action by name.
    pub fn run_command(&mut self, line: &str) {
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { return };
        let args: Vec<&str> = parts.collect();
        if let Some(cmd) = COMMANDS.iter().find(|c| c.names().any(|n| n == name)) {
            return (cmd.run)(self, &args);
        }
        if self.registry.get(name).is_some() || APP_ACTIONS.iter().any(|a| a.name == name) {
            return self.run_chain(name);
        }
        self.doc.editor.message = Some(format!("Unknown command: {name} (try :help)"));
    }

    fn open_help(&mut self, topic: Option<&str>) {
        let text = help_text(topic, &self.effective_bindings(), &self.registry);
        let title = topic.map_or_else(|| "Help".to_owned(), |t| format!("Help: {t}"));
        self.open_view(&title, &text);
    }

    /// Default bindings with the user's `bindings.json` applied, for help.
    fn effective_bindings(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = skink_config::DEFAULT_BINDINGS
            .iter()
            .map(|(k, a)| ((*k).to_owned(), (*a).to_owned()))
            .collect();
        for (key, action) in &self.user_bindings {
            let spec = skink_config::parse_key(key);
            let marked = format!("{action}   (bindings.json)");
            match out
                .iter_mut()
                .find(|(k, _)| skink_config::parse_key(k) == spec)
            {
                Some(slot) => slot.1 = marked,
                None => out.push((key.clone(), marked)),
            }
        }
        out
    }

    /// Open a file from the config directory, creating it with defaults.
    fn open_config_file(&mut self, name: &str, default: &str) {
        let Some(dir) = self.config_dir.clone() else {
            self.doc.editor.message = Some("No config directory (set SKINK_CONFIG_DIR)".into());
            return;
        };
        let path = dir.join(name);
        if !path.exists()
            && let Err(e) =
                std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, default))
        {
            self.doc.editor.message = Some(format!("{}: {e}", path.display()));
            return;
        }
        self.open_path(&path);
    }

    /// Open every file git reports as conflicted, at the first marker.
    fn open_conflicts(&mut self) {
        use crate::run::run_capture;
        use std::time::Duration;
        let root = self.project_root();
        let (top, ok) = run_capture(
            "git rev-parse --show-toplevel",
            Some(&root),
            Duration::from_secs(5),
        );
        if !ok {
            self.doc.editor.message = Some("Not a git repository".into());
            return;
        }
        let top = Path::new(top.trim()).to_path_buf();
        let (list, _) = run_capture(
            "git diff --name-only --diff-filter=U",
            Some(&top),
            Duration::from_secs(10),
        );
        let files: Vec<_> = list
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| top.join(l))
            .collect();
        if files.is_empty() {
            self.doc.editor.message = Some("No merge conflicts".into());
            return;
        }
        for f in &files {
            self.open_path(f);
        }
        self.doc.editor.goto_conflict(true);
        self.doc.editor.message = Some(format!(
            "{} conflicted file(s) opened; :ours / :theirs / :both resolve the block",
            files.len()
        ));
    }
}

/// Command names and actions starting with `prefix`.
pub fn complete_name(prefix: &str, registry: &CommandRegistry) -> Vec<String> {
    let mut out: Vec<String> = COMMANDS
        .iter()
        .flat_map(Cmd::names)
        .filter(|n| n.chars().all(char::is_alphanumeric))
        .chain(registry.iter().map(|c| c.name))
        .chain(APP_ACTIONS.iter().map(|a| a.name))
        .filter(|n| n.starts_with(prefix))
        .map(str::to_owned)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Longest common prefix of `items` (on a char boundary).
pub fn common_prefix(items: &[String]) -> String {
    let Some(first) = items.first() else {
        return String::new();
    };
    let mut end = first.len();
    for s in &items[1..] {
        end = end.min(
            first
                .bytes()
                .zip(s.bytes())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    while !first.is_char_boundary(end) {
        end -= 1;
    }
    first[..end].to_owned()
}

/// Complete a file path argument: entries of the directory part of `partial`.
pub fn complete_path(partial: &str) -> Vec<String> {
    let (dir, name) = match partial.rfind(['/', '\\']) {
        Some(i) => (&partial[..=i], &partial[i + 1..]),
        None => ("", partial),
    };
    let read = if dir.is_empty() {
        Path::new(".")
    } else {
        Path::new(dir)
    };
    let Ok(rd) = std::fs::read_dir(read) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            // Hidden entries only when asked for.
            if !n.starts_with(name) || (n.starts_with('.') && !name.starts_with('.')) {
                return None;
            }
            let slash = if e.file_type().is_ok_and(|t| t.is_dir()) {
                "/"
            } else {
                ""
            };
            Some(format!("{dir}{n}{slash}"))
        })
        .collect();
    out.sort();
    out
}

const DOC: &str = include_str!("help.txt");

/// The help document, or just the sections whose heading matches `topic`.
pub fn help_text(
    topic: Option<&str>,
    bindings: &[(String, String)],
    registry: &CommandRegistry,
) -> String {
    let mut full = String::from(DOC);
    full.push_str("\n## Commands (command line, Ctrl-E)\n");
    for c in COMMANDS {
        let names = c.names().collect::<Vec<_>>().join(" | ");
        let head = if c.args.is_empty() {
            names
        } else {
            format!("{names} {}", c.args)
        };
        full.push_str(&format!("  {head}\n      {}\n", c.about));
    }
    full.push_str("\n## Key bindings\n");
    for (k, a) in bindings {
        full.push_str(&format!("  {k:<18} {a}\n"));
    }
    full.push_str("\n## Actions (bind them, or run them by name from the command line)\n");
    for cmd in registry.iter() {
        full.push_str(&format!("  {:<18} {}\n", cmd.name, cmd.description));
    }
    for a in APP_ACTIONS {
        full.push_str(&format!("  {:<18} {}\n", a.name, a.about));
    }
    let Some(t) = topic.map(str::to_lowercase).filter(|t| !t.is_empty()) else {
        return full;
    };
    let mut out = String::new();
    let mut keep = false;
    for line in full.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            keep = h.to_lowercase().contains(&t);
        }
        if keep {
            out.push_str(line);
            out.push('\n');
        }
    }
    if out.is_empty() {
        format!("No help section matches \"{t}\".\n\n{full}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Vec<(String, String)> {
        skink_config::DEFAULT_BINDINGS
            .iter()
            .map(|(k, a)| ((*k).to_owned(), (*a).to_owned()))
            .collect()
    }

    #[test]
    fn completion_and_prefix() {
        let r = CommandRegistry::builtin();
        let m = complete_name("conf", &r);
        assert!(m.contains(&"conflicts".to_owned()) && !m.contains(&"ConflictNext".to_owned()));
        let m = complete_name("Conf", &r);
        assert!(m.contains(&"ConflictNext".to_owned()));
        assert!(complete_name("Quick", &r).contains(&"QuickOpen".to_owned()));
        assert_eq!(common_prefix(&["theirs".into(), "term".into()]), "t");
        assert_eq!(common_prefix(&["é1".into(), "é2".into()]), "é");
        assert_eq!(common_prefix(&[]), "");
    }

    #[test]
    fn help_has_sections_and_topics() {
        let r = CommandRegistry::builtin();
        let all = help_text(None, &defaults(), &r);
        for needle in [
            "## Tabs",
            "## Merge conflicts",
            "conflicts",
            "Ctrl-q",
            "ConflictNext",
            "QuickOpen",
        ] {
            assert!(all.contains(needle), "{needle}");
        }
        let one = help_text(Some("conflict"), &defaults(), &r);
        assert!(one.starts_with("## Merge conflicts") && !one.contains("## Tabs"));
        assert!(help_text(Some("zzz"), &defaults(), &r).starts_with("No help section"));
    }

    #[test]
    fn names_are_unique() {
        let mut all: Vec<&str> = COMMANDS.iter().flat_map(Cmd::names).collect();
        all.extend(APP_ACTIONS.iter().map(|a| a.name));
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n, "duplicate command or action name");
        assert!(takes_path("e") && takes_path("open") && !takes_path("goto"));
    }
}
