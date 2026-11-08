//! Command-line mode (Ctrl-E): the command table, completion and the help
//! document. The same table feeds `help`, so the docs cannot drift from what
//! the command line accepts.

use skink_core::CommandRegistry;

pub struct CmdDoc {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: &'static str,
    pub about: &'static str,
}

const fn c(
    name: &'static str,
    aliases: &'static [&'static str],
    args: &'static str,
    about: &'static str,
) -> CmdDoc {
    CmdDoc {
        name,
        aliases,
        args,
        about,
    }
}

pub const COMMANDS: &[CmdDoc] = &[
    c(
        "help",
        &["docs"],
        "[topic]",
        "open this documentation in a tab (topics: tabs, editing, conflicts, settings, ...)",
    ),
    c(
        "open",
        &["e", "tabnew"],
        "[FILE[:LINE]]",
        "open a file or folder in a tab (no argument: new empty tab)",
    ),
    c("goto", &[], "LINE[:COL]", "jump to a line"),
    c("save", &["w"], "", "save the file"),
    c(
        "quit",
        &["q"],
        "",
        "close the tab (quits on the last file tab)",
    ),
    c(
        "qa",
        &["quitall"],
        "",
        "quit everything (refuses when tabs have unsaved changes)",
    ),
    c("tab", &[], "N", "select tab N"),
    c(
        "term",
        &["terminal", "agent", "tabterm"],
        "[COMMAND...]",
        "new terminal tab running COMMAND (default: your shell)",
    ),
    c("tree", &[], "", "toggle the file tree"),
    c(
        "recent",
        &["quickopen"],
        "",
        "quick open: tabs, recent files, project files",
    ),
    c(
        "run",
        &["!"],
        "COMMAND...",
        "run a shell command, show its output in a tab",
    ),
    c("diff", &[], "", "show `git diff` in a tab"),
    c("gitstatus", &["status"], "", "show `git status` in a tab"),
    c("git", &[], "ARGS...", "run git, show its output in a tab"),
    c(
        "conflicts",
        &[],
        "",
        "open all git-conflicted files at the first conflict",
    ),
    c("nextconflict", &[], "", "next merge conflict"),
    c("prevconflict", &[], "", "previous merge conflict"),
    c(
        "ours",
        &[],
        "",
        "resolve the conflict under the cursor: keep ours",
    ),
    c(
        "theirs",
        &[],
        "",
        "resolve the conflict under the cursor: keep theirs",
    ),
    c(
        "both",
        &[],
        "",
        "resolve the conflict under the cursor: keep both",
    ),
    c("settings", &[], "", "open settings.json"),
    c("bindings", &["keys"], "", "open bindings.json"),
    c("reload", &[], "", "reload settings.json and bindings.json"),
];

/// Names (and aliases) starting with `prefix`; registry actions included.
pub fn complete_name(prefix: &str, registry: &CommandRegistry) -> Vec<String> {
    let mut out: Vec<String> = COMMANDS
        .iter()
        .flat_map(|d| std::iter::once(d.name).chain(d.aliases.iter().copied()))
        .filter(|n| n.starts_with(prefix) && n.chars().all(|c| c.is_alphanumeric()))
        .map(str::to_owned)
        .collect();
    out.extend(
        registry
            .iter()
            .map(|c| c.name)
            .filter(|n| n.starts_with(prefix))
            .map(str::to_owned),
    );
    out.sort();
    out.dedup();
    out
}

/// Longest common prefix of `items`.
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

/// Complete a file path argument: matches in the directory part of `partial`.
pub fn complete_path(partial: &str) -> Vec<String> {
    let (dir, name) = match partial.rfind(['/', '\\']) {
        Some(i) => (&partial[..=i], &partial[i + 1..]),
        None => ("", partial),
    };
    let read = if dir.is_empty() {
        std::path::PathBuf::from(".")
    } else {
        std::path::PathBuf::from(dir)
    };
    let Ok(rd) = std::fs::read_dir(read) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            if !n.starts_with(name) || (n.starts_with('.') && !name.starts_with('.')) {
                return None;
            }
            let slash = e.file_type().is_ok_and(|t| t.is_dir());
            Some(format!("{dir}{n}{}", if slash { "/" } else { "" }))
        })
        .collect();
    out.sort();
    out
}

const DOC: &str = include_str!("help.txt");

/// The help document, or just the section matching `topic`.
pub fn help_text(
    topic: Option<&str>,
    bindings: &[(&str, &str)],
    registry: &CommandRegistry,
) -> String {
    let mut full = String::from(DOC);
    full.push_str("\n## Commands (command line, Ctrl-E)\n");
    for d in COMMANDS {
        let names = std::iter::once(d.name)
            .chain(d.aliases.iter().copied())
            .collect::<Vec<_>>()
            .join(" | ");
        let head = if d.args.is_empty() {
            names
        } else {
            format!("{names} {}", d.args)
        };
        full.push_str(&format!("  {head}\n      {}\n", d.about));
    }
    full.push_str("\n## Key bindings\n");
    for (k, a) in bindings {
        full.push_str(&format!("  {k:<18} {a}\n"));
    }
    full.push_str("\n## Actions (bind them, or run them by name from the command line)\n");
    for cmd in registry.iter() {
        full.push_str(&format!("  {:<18} {}\n", cmd.name, cmd.description));
    }
    full.push_str(
        "  Tab actions: PreviousTab NextTab FirstTab LastTab AddTab OpenFile QuickOpen\n",
    );
    let Some(t) = topic.map(str::to_lowercase).filter(|t| !t.is_empty()) else {
        return full;
    };
    // One section: a `## ` heading containing the topic.
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

    #[test]
    fn completion_and_prefix() {
        let r = CommandRegistry::builtin();
        let m = complete_name("conf", &r);
        assert!(m.contains(&"conflicts".to_owned()) && !m.contains(&"ConflictNext".to_owned()));
        let m = complete_name("Conf", &r);
        assert!(m.contains(&"ConflictNext".to_owned()));
        assert_eq!(common_prefix(&["theirs".into(), "term".into()]), "t");
        assert_eq!(common_prefix(&[]), "");
    }

    #[test]
    fn help_has_sections_and_topics() {
        let r = CommandRegistry::builtin();
        let all = help_text(None, skink_config::DEFAULT_BINDINGS, &r);
        for needle in [
            "## Tabs",
            "## Merge conflicts",
            "conflicts",
            "Ctrl-q",
            "ConflictNext",
        ] {
            assert!(all.contains(needle), "{needle}");
        }
        let one = help_text(Some("conflict"), skink_config::DEFAULT_BINDINGS, &r);
        assert!(one.starts_with("## Merge conflicts") && !one.contains("## Tabs"));
        assert!(
            help_text(Some("zzz"), skink_config::DEFAULT_BINDINGS, &r)
                .starts_with("No help section")
        );
    }

    #[test]
    fn every_documented_command_is_handled() {
        // The command bar's match arms must cover the table; checked by
        // running each name against an App in tests.rs (see command_table_is_wired).
        assert!(COMMANDS.iter().all(|d| !d.about.is_empty()));
    }
}
