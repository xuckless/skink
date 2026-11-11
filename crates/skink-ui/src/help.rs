//! `:help`: short Markdown pages, one per topic, shown in a single help tab
//! with the Markdown renderer. Enter follows a link to another topic,
//! Backspace / Alt-Left go back, Esc closes. `:help WORDS` opens the topic
//! those words name, or a list of the pages that mention them.

use crate::App;
use crate::doc::DocKind;
use crate::preview::{Preview, PreviewKind, RenderState, Rendered, Source};

/// One help page.
pub struct Topic {
    pub id: &'static str,
    pub title: &'static str,
    /// Other words that open it (`:help tree` and `:help sidebar`).
    pub aliases: &'static [&'static str],
    pub text: &'static str,
}

macro_rules! topic {
    ($id:literal, $title:literal, [$($alias:literal),*]) => {
        Topic {
            id: $id,
            title: $title,
            aliases: &[$($alias),*],
            text: include_str!(concat!("help/", $id, ".md")),
        }
    };
}

/// Every page, in the order the landing page lists them. `keys`,
/// `commands` and `actions` end with tables generated from the keymap and
/// the command table.
pub static TOPICS: &[Topic] = &[
    topic!("index", "Help", ["start here", "contents"]),
    topic!(
        "basics",
        "Opening, tabs and quitting",
        [
            "start",
            "getting started",
            "open",
            "tabs",
            "tab",
            "quit",
            "sessions",
            "session",
            "views",
            "cli"
        ]
    ),
    topic!(
        "editing",
        "Editing",
        [
            "edit",
            "undo",
            "clipboard",
            "copy",
            "paste",
            "selection",
            "macros",
            "macro",
            "case",
            "comment",
            "brackets",
            "typing"
        ]
    ),
    topic!(
        "cursors",
        "Multiple cursors",
        ["multiple cursors", "multicursor", "multi-cursor", "cursor"]
    ),
    topic!(
        "find",
        "Find and replace",
        ["replace", "find and replace", "search in file"]
    ),
    topic!(
        "search",
        "Search the project",
        [
            "grep",
            "project search",
            "search in files",
            "structural search",
            "sgrep",
            "srep"
        ]
    ),
    topic!(
        "tree",
        "File tree",
        ["sidebar", "explorer", "roots", "file tree"]
    ),
    topic!(
        "layout",
        "Splits, docks and zoom",
        ["splits", "split", "panes", "pane", "docks", "dock", "zoom"]
    ),
    topic!(
        "navigate",
        "Moving around",
        [
            "jumps",
            "jump",
            "folding",
            "fold",
            "sticky scroll",
            "rulers",
            "marks",
            "goto",
            "go to line"
        ]
    ),
    topic!(
        "code",
        "Code intelligence",
        [
            "lsp",
            "language servers",
            "language server",
            "completion",
            "definition",
            "references",
            "rename",
            "refactor",
            "refactoring",
            "format",
            "symbols",
            "quick open"
        ]
    ),
    topic!(
        "git",
        "Git",
        [
            "merge conflicts",
            "conflicts",
            "conflict",
            "blame",
            "diff",
            "commit",
            "hunks",
            "stage",
            "branches",
            "stash"
        ]
    ),
    topic!(
        "terminal",
        "Terminals",
        [
            "terminals",
            "shell",
            "term",
            "shell integration",
            "scrollback",
            "everyai"
        ]
    ),
    topic!("tasks", "Tasks", ["task", "build", "problem matchers"]),
    topic!(
        "debug",
        "Debugging",
        ["debugger", "dap", "breakpoints", "breakpoint", "gdb"]
    ),
    topic!(
        "ai",
        "AI",
        ["agent", "agents", "claude", "prompt", "rewrite"]
    ),
    topic!(
        "packages",
        "Packages and languages",
        [
            "package",
            "install",
            "languages",
            "language",
            "grammars",
            "grammar",
            "syntax"
        ]
    ),
    topic!(
        "themes",
        "Themes, icons and fonts",
        [
            "theme",
            "colors",
            "colours",
            "colorscheme",
            "icons",
            "fonts",
            "font"
        ]
    ),
    topic!(
        "settings",
        "Settings and key bindings",
        [
            "config",
            "configuration",
            "bindings",
            "keymap",
            "settings.json",
            "bindings.json"
        ]
    ),
    topic!(
        "files",
        "Files on disk",
        [
            "saving",
            "save",
            "encoding",
            "line endings",
            "backup",
            "recover",
            "sudo",
            "large files"
        ]
    ),
    topic!("preview", "Previews", ["previews", "markdown", "svg"]),
    topic!(
        "lua",
        "Scripts (Lua)",
        ["scripts", "script", "scripting", "init.lua", "plugins"]
    ),
    topic!(
        "editor",
        "Skink as your $EDITOR",
        ["$editor", "git editor", "lean", "scratch", "core.editor"]
    ),
    topic!(
        "remote",
        "Remote development",
        ["ssh", "ports", "port", "forwarding", "remotes"]
    ),
    topic!("updates", "Updates", ["upgrade", "update"]),
    topic!("mac", "macOS: Option as Alt", ["macos", "option", "alt"]),
    topic!(
        "typed",
        "Typed commands for every key",
        ["typed commands", "short names"]
    ),
    topic!(
        "keys",
        "Key bindings",
        ["key bindings", "keybindings", "shortcuts", "keyboard"]
    ),
    topic!("commands", "Commands", ["command line", "command"]),
    topic!("actions", "Actions", ["action"]),
];

/// What the help tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpPage {
    Topic(&'static str),
    /// Several pages mention the words: links to each.
    Results {
        query: String,
        hits: Vec<Hit>,
    },
}

/// A page that matched a search, with the line that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub id: &'static str,
    pub line: String,
}

/// Where the help tab is, and the pages Back returns to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpNav {
    pub page: HelpPage,
    pub back: Vec<HelpPage>,
}

/// What `:help WORDS` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Found {
    Page(&'static str),
    Many(Vec<Hit>),
    Nothing,
}

pub fn topic(id: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.id == id)
}

impl HelpPage {
    pub fn title(&self) -> String {
        match self {
            HelpPage::Topic("index") => "Help".into(),
            HelpPage::Topic(id) => format!("Help: {}", topic(id).map_or(*id, |t| t.title)),
            HelpPage::Results { query, .. } => format!("Help: \"{query}\""),
        }
    }
}

fn norm(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Text of a line without Markdown punctuation, for a search hit.
fn plain(line: &str) -> String {
    // Links keep their text: `[every key](keys)` reads "every key".
    let mut line = line.to_owned();
    while let Some(i) = line.find("](") {
        let Some(j) = line[i..].find(')') else { break };
        line.replace_range(i..i + j + 1, "");
    }
    // Table rows read as "cell · cell"; escaped pipes stay pipes.
    let line = line.replace("\\|", "\u{0}");
    let line = line.trim().trim_start_matches('>').trim();
    let line = match line.strip_prefix('|') {
        Some(row) => row.trim_end_matches('|').replace(" | ", " · "),
        None => line.to_owned(),
    };
    let s: String = line
        .chars()
        .filter(|c| !matches!(c, '`' | '*' | '\\'))
        .map(|c| if c == '\u{0}' { '|' } else { c })
        .collect();
    let s = s.trim().trim_start_matches("- ").trim();
    let mut out: String = s.chars().take(70).collect();
    if s.chars().count() > 70 {
        out.push('…');
    }
    out
}

/// Find the page for `query`: a topic's id, title or alias first (exactly,
/// then as a prefix), else every page whose text has all the words.
/// `full(id)` is a page's whole text (generated pages included).
pub fn find(query: &str, full: &dyn Fn(&Topic) -> String) -> Found {
    let q = norm(query.trim_start_matches(':'));
    if q.is_empty() {
        return Found::Page("index");
    }
    let names = |t: &Topic| {
        std::iter::once(t.id.to_owned())
            .chain(std::iter::once(norm(t.title)))
            .chain(t.aliases.iter().map(|a| norm(a)))
            .collect::<Vec<_>>()
    };
    if let Some(t) = TOPICS.iter().find(|t| names(t).contains(&q)) {
        return Found::Page(t.id);
    }
    let prefixed: Vec<&Topic> = TOPICS
        .iter()
        .filter(|t| t.id != "index" && names(t).iter().any(|n| n.starts_with(&q)))
        .collect();
    if let [one] = prefixed[..] {
        return Found::Page(one.id);
    }
    // Pages with the words together beat pages with them apart; written
    // pages beat the generated reference tables.
    let words: Vec<&str> = q.split(' ').collect();
    let texts: Vec<(&Topic, String)> = TOPICS
        .iter()
        .filter(|t| t.id != "index")
        .map(|t| (t, full(t)))
        .collect();
    let phrase = texts.iter().any(|(_, x)| x.to_lowercase().contains(&q));
    let mut hits: Vec<(usize, Hit)> = texts
        .iter()
        .filter_map(|(t, text)| {
            let low = text.to_lowercase();
            let found = match phrase {
                true => low.contains(&q),
                false => words.iter().all(|w| low.contains(w)),
            };
            if !found {
                return None;
            }
            let generated = matches!(t.id, "keys" | "commands" | "actions");
            let count = match phrase {
                true => low.matches(&q).count() * 4,
                false => words.iter().map(|w| low.matches(w).count()).sum::<usize>(),
            } / if generated { 4 } else { 1 }
                + 100 * usize::from(prefixed.iter().any(|p| p.id == t.id));
            let line = text
                .lines()
                .filter(|l| !l.starts_with('#') && !l.starts_with("|---"))
                .find(|l| {
                    let l = l.to_lowercase();
                    match phrase {
                        true => l.contains(&q),
                        false => words.iter().any(|w| l.contains(w)),
                    }
                })
                .map(plain)
                .unwrap_or_default();
            Some((count, Hit { id: t.id, line }))
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    match hits.len() {
        0 => Found::Nothing,
        1 => Found::Page(hits[0].1.id),
        _ => Found::Many(hits.into_iter().take(12).map(|(_, h)| h).collect()),
    }
}

/// The Markdown of a results page.
fn results_page(query: &str, hits: &[Hit]) -> String {
    let mut s = format!(
        "# Help for \"{query}\"\n\n{} pages mention it. Enter opens one, Backspace comes back.\n\n",
        hits.len()
    );
    for h in hits {
        let title = topic(h.id).map_or(h.id, |t| t.title);
        s.push_str(&format!("- [{title}]({}) — {}\n", h.id, md_escape(&h.line)));
    }
    s
}

/// Escape Markdown punctuation in generated text (keys like `Ctrl-\`,
/// chains like `Copy|CopyLine`).
pub fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '|' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '#'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl App {
    /// A page's whole Markdown: the file, plus the generated tables.
    pub(crate) fn help_text(&self, t: &Topic) -> String {
        let mut s = t.text.to_owned();
        match t.id {
            "keys" => s.push_str(&crate::commands::keys_table(&self.effective_bindings())),
            "commands" => s.push_str(&crate::commands::commands_list()),
            "actions" => s.push_str(&crate::commands::actions_table(&self.registry)),
            _ => {}
        }
        s
    }

    fn help_markdown(&self, page: &HelpPage) -> String {
        match page {
            HelpPage::Topic(id) => topic(id).map_or_else(String::new, |t| self.help_text(t)),
            HelpPage::Results { query, hits } => results_page(query, hits),
        }
    }

    /// `:help [WORDS]`: open (or reuse) the help tab at that page.
    pub(crate) fn open_help(&mut self, query: Option<&str>) {
        let q = query.map(str::trim).unwrap_or_default();
        let page = match find(q, &|t| self.help_text(t)) {
            Found::Page(id) => HelpPage::Topic(id),
            Found::Many(hits) => HelpPage::Results {
                query: q.to_owned(),
                hits,
            },
            Found::Nothing => {
                self.doc.editor.message = Some(format!("No help for \"{q}\"; try :help"));
                return;
            }
        };
        let existing = self.find_doc(
            |d| matches!(&d.kind, DocKind::Preview(p) if matches!(p.source, Source::Help(_))),
        );
        match existing {
            Some(i) => {
                self.switch_to(i);
                self.help_go(page);
            }
            None => {
                let title = page.title();
                let idx = self.add_doc(
                    skink_core::Editor::new(skink_core::Buffer::view(&title, "")),
                    false,
                );
                if let Some(d) = self.doc_at_mut(idx) {
                    d.kind = DocKind::Preview(Box::new(Preview {
                        source: Source::Help(HelpNav {
                            page,
                            back: Vec::new(),
                        }),
                        kind: PreviewKind::Markdown(Rendered::default()),
                        state: RenderState::Stale,
                    }));
                    d.syntax = None;
                }
                self.preview_sync = crate::preview::PreviewSync::ToSourceLine(0);
            }
        }
    }

    /// Show `page` in the active help tab; the current page goes on the
    /// Back list.
    pub(crate) fn help_go(&mut self, page: HelpPage) {
        let DocKind::Preview(p) = &mut self.doc.kind else {
            return;
        };
        let Source::Help(nav) = &mut p.source else {
            return;
        };
        if nav.page != page {
            let old = std::mem::replace(&mut nav.page, page);
            nav.back.push(old);
        }
        self.help_show();
    }

    /// Back to the previous help page; false when there is none.
    pub(crate) fn help_back(&mut self) -> bool {
        let DocKind::Preview(p) = &mut self.doc.kind else {
            return false;
        };
        let Source::Help(nav) = &mut p.source else {
            return false;
        };
        let Some(prev) = nav.back.pop() else {
            return false;
        };
        nav.page = prev;
        self.help_show();
        true
    }

    /// Re-render the help tab at its current page, from the top.
    fn help_show(&mut self) {
        let DocKind::Preview(p) = &mut self.doc.kind else {
            return;
        };
        let Source::Help(nav) = &p.source else {
            return;
        };
        let title = nav.page.title();
        p.state = RenderState::Stale;
        self.doc.replace_view(&title, "");
        self.preview_sync = crate::preview::PreviewSync::ToSourceLine(0);
    }

    /// The Markdown the help tab shows now.
    pub(crate) fn help_source(&self, nav: &HelpNav) -> String {
        self.help_markdown(&nav.page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(q: &str) -> Found {
        find(q, &|t| t.text.to_owned())
    }

    #[test]
    fn topics_resolve_by_name_alias_prefix_and_text() {
        assert_eq!(found(""), Found::Page("index"));
        assert_eq!(found("git"), Found::Page("git"));
        assert_eq!(found("Merge  Conflicts"), Found::Page("git"));
        assert_eq!(found("sidebar"), Found::Page("tree"));
        assert_eq!(found("debu"), Found::Page("debug"));
        assert_eq!(found("zzzqqq"), Found::Nothing);
        // Words that only appear in the text list every page with them.
        match found("ControlMaster") {
            Found::Page("remote") => {}
            other => panic!("{other:?}"),
        }
        let Found::Many(hits) = found("terminal tab") else {
            panic!("several pages talk about terminal tabs");
        };
        assert!(hits.len() > 1 && hits.iter().all(|h| !h.line.is_empty()));
    }

    #[test]
    fn every_page_is_short_markdown_with_a_title_and_live_links() {
        for t in TOPICS {
            assert!(
                t.text.starts_with(&format!("# {}\n", t.title)),
                "{} starts with its title",
                t.id
            );
            let r = crate::preview::render_markdown(t.text, 90);
            for l in &r.links {
                let ok = l.url.contains("://") || topic(&l.url).is_some();
                assert!(ok, "{}: link to {:?}", t.id, l.url);
            }
        }
        // The landing page fits a screen and links every other page.
        let index = topic("index").unwrap();
        let r = crate::preview::render_markdown(index.text, 100);
        assert!(r.text.lines().count() <= 34, "{}", r.text.lines().count());
        for t in TOPICS.iter().filter(|t| t.id != "index") {
            assert!(
                r.links.iter().any(|l| l.url == t.id),
                "index links {}",
                t.id
            );
        }
    }

    #[test]
    fn names_do_not_collide() {
        let mut all: Vec<String> = TOPICS
            .iter()
            .flat_map(|t| {
                let mut names: Vec<String> = std::iter::once(t.id.to_owned())
                    .chain(std::iter::once(norm(t.title)))
                    .chain(t.aliases.iter().map(|a| norm(a)))
                    .collect();
                names.sort();
                names.dedup();
                names
            })
            .collect();
        let n = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), n, "a help name opens two topics");
    }
}
