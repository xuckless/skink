//! Command-line mode (Ctrl-E) and app-level actions. Each command is one
//! table row holding its name, docs *and* handler, so `help`, completion and
//! dispatch are generated from the same data and cannot drift apart.

use crate::{App, prompt::PromptKind};
use skink_core::CommandRegistry;
use skink_lsp::GotoKind;
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

/// Two arguments for `replace`: `"a b" "c"` (quotes allow spaces, `\"`
/// escapes a quote) or `a c`. An empty replacement must be quoted: `x ""`.
pub fn two_args(line: &str) -> Option<(String, String)> {
    fn quoted(s: &str) -> Option<(String, &str)> {
        let body = s.strip_prefix('"')?;
        let mut out = String::new();
        let mut chars = body.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => out.extend(chars.next().map(|(_, c)| c)),
                '"' => return Some((out, &body[i + 1..])),
                c => out.push(c),
            }
        }
        None
    }
    fn word(s: &str) -> Option<(String, &str)> {
        let s = s.trim_start();
        if s.starts_with('"') {
            return quoted(s);
        }
        let end = s.find(char::is_whitespace).unwrap_or(s.len());
        (end > 0).then(|| (s[..end].to_owned(), &s[end..]))
    }
    let (a, rest) = word(line)?;
    let (b, rest) = word(rest)?;
    rest.trim().is_empty().then_some((a, b))
}

pub static COMMANDS: &[Cmd] = &[
    Cmd {
        name: "ports",
        aliases: &[],
        args: "[stop PORT | forward PORT[:LOCAL]]",
        about: "skink ssh: ports listening on this machine and their forwards; stop one (:port adds one)",
        run: |app, args| match args.first().copied() {
            Some("stop") => app.unforward_port(args.get(1).copied()),
            Some("forward") => app.forward_port(args.get(1).copied()),
            _ => app.show_ports(),
        },
    },
    Cmd {
        name: "port",
        aliases: &[],
        args: "PORT[:LOCAL]",
        about: "skink ssh: forward a port of this machine to yours (:port 3000, :port 3000:8080)",
        run: |app, args| app.forward_port(args.first().copied()),
    },
    Cmd {
        name: "install",
        aliases: &[],
        args: "NAME | owner/repo[/package][@tag] | DIR | .",
        about: "install a package (grammar, language server, theme, font); . = what this project needs",
        run: |app, args| app.install_cmd(&rest(args)),
    },
    Cmd {
        name: "uninstall",
        aliases: &[],
        args: "NAME",
        about: "remove a package and everything it placed",
        run: |app, args| app.uninstall_cmd(&rest(args)),
    },
    Cmd {
        name: "packages",
        aliases: &[],
        args: "[enable NAME | disable NAME]",
        about: "installed packages; turn one off or on without removing it",
        run: |app, args| app.packages_cmd(args),
    },
    Cmd {
        name: "remotes",
        aliases: &[],
        args: "",
        about: "saved skink ssh targets (\"remotes\" in settings.json)",
        run: |app, _| app.show_remotes(),
    },
    Cmd {
        name: "lsp",
        aliases: &[],
        args: "[restart|log]",
        about: "language server status (memory, progress, log); restart it; full log",
        run: |app, args| match args.first().copied() {
            Some("restart") => app.restart_lsp(),
            Some("log") => app.show_lsp_log(),
            _ => app.show_lsp_status(),
        },
    },
    Cmd {
        name: "format",
        aliases: &["fmt"],
        args: "",
        about: "format the selection or the file with the language server",
        run: |app, _| {
            app.format(crate::intel::AfterFormat::Nothing);
        },
    },
    Cmd {
        name: "problems",
        aliases: &["diagnostics"],
        args: "",
        about: "errors and warnings of the open files (Enter jumps)",
        run: |app, _| app.show_problems(),
    },
    Cmd {
        name: "references",
        aliases: &["refs"],
        args: "",
        about: "every use of the symbol at the cursor (Enter jumps)",
        run: |app, _| app.find_references(),
    },
    Cmd {
        name: "definition",
        aliases: &["def"],
        args: "",
        about: "go to the definition of the symbol at the cursor",
        run: |app, _| app.goto(GotoKind::Definition, false),
    },
    Cmd {
        name: "implementation",
        aliases: &["impl"],
        args: "",
        about: "go to the implementations of the symbol at the cursor",
        run: |app, _| app.goto(GotoKind::Implementation, false),
    },
    Cmd {
        name: "symbols",
        aliases: &["outline"],
        args: "",
        about: "symbols in this file",
        run: |app, _| app.open_outline(),
    },
    Cmd {
        name: "help",
        aliases: &["docs"],
        args: "[topic]",
        about: "open this documentation in a tab (topics: tabs, editing, conflicts, settings, ...)",
        run: |app, args| app.open_help(args.first().copied()),
    },
    Cmd {
        name: "termdock",
        aliases: &["panel"],
        args: "",
        about: "show or hide a shell along the bottom (Alt-T)",
        run: |app, _| app.toggle_terminal_dock(),
    },
    Cmd {
        name: "agentdock",
        aliases: &[],
        args: "",
        about: "show or hide the agent panel on the right (Alt-Shift-T)",
        run: |app, _| app.toggle_agent_dock(),
    },
    Cmd {
        name: "back",
        aliases: &[],
        args: "",
        about: "back to where you were before the last jump (Alt-U)",
        run: |app, _| app.jump_back(),
    },
    Cmd {
        name: "forward",
        aliases: &[],
        args: "",
        about: "forward again after :back (Alt-Shift-U)",
        run: |app, _| app.jump_forward(),
    },
    Cmd {
        name: "addroot",
        aliases: &["add-folder"],
        args: "DIR",
        about: "add another project root (tree, quick open, project search)",
        run: |app, args| app.add_root(&rest(args)),
    },
    Cmd {
        name: "removeroot",
        aliases: &[],
        args: "[DIR]",
        about: "remove an added root (no argument: the one selected in the tree)",
        run: |app, args| app.remove_root(&rest(args)),
    },
    Cmd {
        name: "fold",
        aliases: &[],
        args: "",
        about: "fold or unfold the block at the cursor (Alt-M)",
        run: |app, _| app.toggle_fold(),
    },
    Cmd {
        name: "foldall",
        aliases: &[],
        args: "",
        about: "fold every top-level block",
        run: |app, _| app.fold_all(),
    },
    Cmd {
        name: "unfoldall",
        aliases: &[],
        args: "",
        about: "open every fold (Alt-Shift-M)",
        run: |app, _| app.unfold_all(),
    },
    Cmd {
        name: "grep",
        aliases: &["search", "rg"],
        args: "[TEXT]",
        about: "search every file in the project (Alt-s); results stream into a tab, Enter jumps",
        run: |app, args| match args {
            [] => app.project_search_prompt(),
            _ => app.start_project_search(&rest(args)),
        },
    },
    Cmd {
        name: "greprep",
        aliases: &["replace-in-files"],
        args: "WITH",
        about: "replace the last project search's matches in every file, through the preview",
        run: |app, args| app.project_replace(&rest(args)),
    },
    Cmd {
        name: "scrollback",
        aliases: &["termhistory"],
        args: "[last]",
        about: "the terminal's history in a searchable tab (Shift-F11); `last` selects the last output (Shift-F4)",
        run: |app, args| match args {
            ["last"] => app.terminal_history(crate::termtools::Focus::LastOutput),
            _ => app.terminal_history(crate::termtools::Focus::End),
        },
    },
    Cmd {
        name: "debug",
        aliases: &["dbg"],
        args: "[NAME|stop|pause|continue|restart|panel|eval EXPR|watch EXPR]",
        about: "debug: pick a launch configuration (F5 reruns the last), control the session, evaluate",
        run: |app, args| app.debug_command(args),
    },
    Cmd {
        name: "breakpoint",
        aliases: &["bp", "break"],
        args: "[if COND|hits N|log MSG|clear|clearall|list]",
        about: "toggle a breakpoint on the cursor line (F9), or set a condition, hit count or logpoint",
        run: |app, args| app.breakpoint_command(args),
    },
    Cmd {
        name: "task",
        aliases: &["tasks"],
        args: "[LABEL|last]",
        about: "run a task in a terminal tab (Shift-F10 picks, Shift-F9 reruns); problems go to Shift-F8",
        run: |app, args| app.task_command(&rest(args)),
    },
    Cmd {
        name: "mv",
        aliases: &["move"],
        args: "[NEW_PATH]",
        about: "move / rename the current file (relative to the project root); servers update imports",
        run: |app, args| app.move_current_file(&rest(args)),
    },
    Cmd {
        name: "extract",
        aliases: &[],
        args: "var|fn [NAME]",
        about: "extract the selection into a variable or function (server action; tree-sitter fallback for var)",
        run: |app, args| app.extract(args),
    },
    Cmd {
        name: "safedelete",
        aliases: &[],
        args: "",
        about: "delete the declaration at the cursor if find-references shows no other use; preview first",
        run: |app, _| app.safe_delete(crate::refactors::Check::Refs),
    },
    Cmd {
        name: "safedelete!",
        aliases: &[],
        args: "",
        about: "preview deleting the declaration at the cursor without checking references",
        run: |app, _| app.safe_delete(crate::refactors::Check::Force),
    },
    Cmd {
        name: "sgrep",
        aliases: &["structural"],
        args: "PATTERN",
        about: "structural search (ast-grep) in files of the current language: `foo($A, $$$REST)`",
        run: |app, args| app.start_structural_search(&rest(args)),
    },
    Cmd {
        name: "srep",
        aliases: &[],
        args: "PATTERN => WITH",
        about: "structural replace through the preview: `srep foo($A, $B) => bar($B, $A)`",
        run: |app, args| app.structural_replace(&rest(args)),
    },
    Cmd {
        name: "stopsearch",
        aliases: &[],
        args: "",
        about: "stop a running project search",
        run: |app, _| app.stop_project_search(),
    },
    Cmd {
        name: "vsplit",
        aliases: &["vs"],
        args: "[FILE]",
        about: "split the pane side by side, optionally opening FILE in the new pane (Ctrl-\\)",
        run: |app, args| {
            app.split(crate::panes::Axis::Columns);
            if !args.is_empty() {
                app.open_location(&rest(args));
            }
        },
    },
    Cmd {
        name: "hsplit",
        aliases: &["split", "sp"],
        args: "[FILE]",
        about: "split the pane top and bottom, optionally opening FILE in the new pane (Alt-\\)",
        run: |app, args| {
            app.split(crate::panes::Axis::Rows);
            if !args.is_empty() {
                app.open_location(&rest(args));
            }
        },
    },
    Cmd {
        name: "unsplit",
        aliases: &["close-pane"],
        args: "",
        about: "close the focused pane; its tab stays open (Alt-Q)",
        run: |app, _| app.unsplit(),
    },
    Cmd {
        name: "only",
        aliases: &[],
        args: "",
        about: "close every pane but the focused one",
        run: |app, _| app.only_pane(),
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
            app.record_jump();
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
        name: "set",
        aliases: &[],
        args: "eol=lf|crlf",
        about: "set a file option (line endings are written on the next save)",
        run: |app, args| {
            let le = match args.first().copied() {
                Some("eol=lf") => skink_core::LineEnding::Lf,
                Some("eol=crlf") => skink_core::LineEnding::CrLf,
                _ => {
                    app.editor_mut().message = Some("usage: set eol=lf|crlf".into());
                    return;
                }
            };
            app.editor_mut().buffer.set_line_ending(le);
        },
    },
    Cmd {
        name: "fontsize",
        aliases: &["font"],
        args: "+1|-1|0",
        about: "change the terminal font size (EveryAI, Kitty; else use the terminal's keys)",
        run: |app, args| {
            use skink_host::tools::FontStep;
            let step = match args.first().copied() {
                Some("+1" | "+" | "bigger") => FontStep::Bigger,
                Some("-1" | "-" | "smaller") => FontStep::Smaller,
                Some("0" | "reset") => FontStep::Reset,
                _ => {
                    app.editor_mut().message = Some("usage: fontsize +1|-1|0".into());
                    return;
                }
            };
            app.change_font(step);
        },
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
        name: "cq",
        aliases: &["cancel"],
        args: "",
        about: "leave without saving and exit 1 (git aborts the commit or rebase)",
        run: |app, _| app.cancel_quit(),
    },
    Cmd {
        name: "mode",
        aliases: &[],
        args: "",
        about: "full editor or a lean file for another tool, and why",
        run: |app, _| app.describe_mode(),
    },
    Cmd {
        name: "rebase",
        aliases: &[],
        args: "pick|reword|edit|squash|fixup|drop",
        about: "set the verb of the cursor line in git-rebase-todo (Alt-Shift-K cycles)",
        run: |app, args| app.rebase_command(args),
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
        aliases: &["terminal"],
        args: "[COMMAND...]",
        about: "new terminal tab running COMMAND (default: your shell), e.g. `term claude`",
        run: |app, args| {
            // `:term sh -c 'exit 7'`: quotes group the program's arguments.
            let words = split_words(&rest(args));
            let spec = match words.split_first() {
                Some((prog, more)) => CommandSpec::new(prog.as_str()).args(more.iter().cloned()),
                None => CommandSpec::default_shell(),
            };
            app.open_terminal_or_report(spec);
        },
    },
    Cmd {
        name: "tree",
        aliases: &[],
        args: "[collapse|expand|hide|hidden]",
        about: "show / focus the file tree (F8); collapse all, expand the selected folder, hide it, show/hide dotfiles",
        run: |app, args| match args {
            ["collapse"] => app.tree_op(crate::tree::FileTree::collapse_all),
            ["expand"] => app.tree_op(crate::tree::FileTree::expand_recursive),
            ["hide"] => app.set_sidebar_view(crate::SidebarView::Hidden),
            ["hidden"] => app.toggle_hidden_files(),
            _ => app.toggle_tree(),
        },
    },
    Cmd {
        name: "preview",
        aliases: &[],
        args: "",
        about: "live preview of this Markdown or SVG file in a tab (Alt-V); again to go back",
        run: |app, _| app.toggle_preview(),
    },
    Cmd {
        name: "zoom",
        aliases: &[],
        args: "",
        about: "zoom pane: only the text (also Alt-Z)",
        run: |app, _| app.toggle_zoom(),
    },
    Cmd {
        name: "hidden",
        aliases: &[],
        args: "",
        about: "show / hide dotfiles in the file tree (Alt-H)",
        run: |app, _| app.toggle_hidden_files(),
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
        name: "ai",
        aliases: &[],
        args: "[TEXT]",
        about: "AI prompt box for the selection (Ctrl-G); /ask /search /implement /model inside",
        run: |app, args| app.open_ai_prompt_with(&rest(args)),
    },
    Cmd {
        name: "agent",
        aliases: &[],
        args: "[MESSAGE | cancel | stop | send | errors]",
        about: "agent panel: chat with the configured ACP agent; send = selection or terminal output; errors = diagnostics or the last terminal command",
        run: |app, args| match args.first().copied() {
            Some("cancel") if args.len() == 1 => app.cancel_panel(),
            Some("stop") if args.len() == 1 => app.stop_agent(),
            Some("send") if args.len() == 1 => app.send_context_to_agent(),
            Some("errors") if args.len() == 1 => app.send_errors_to_agent(),
            _ => app.open_agent_panel(&rest(args)),
        },
    },
    Cmd {
        name: "review",
        aliases: &[],
        args: "",
        about: "review the edits the agent staged (refactor preview)",
        run: |app, _| {
            app.review_staged();
            if !app.refactor_open() {
                app.editor_mut().message = Some("The agent has no staged edits".into());
            }
        },
    },
    Cmd {
        name: "agents",
        aliases: &[],
        args: "[check]",
        about: "supported agents, which are installed, the active one; check = test it now",
        run: |app, args| match args.first().copied() {
            Some("check") => app.start_agent_check(),
            _ => app.open_agents_view(),
        },
    },
    Cmd {
        name: "model",
        aliases: &[],
        args: "[NAME | default]",
        about: "show or set the model for AI requests (until restart; settings: ai.model)",
        run: |app, args| app.set_model_command(&rest(args)),
    },
    Cmd {
        name: "renamesymbol",
        aliases: &["rn"],
        args: "NEW_NAME",
        about: "rename the symbol at the cursor everywhere (language server; shown in the preview)",
        run: |app, args| app.rename_symbol(&rest(args)),
    },
    Cmd {
        name: "replace",
        aliases: &["replaceall"],
        args: "FIND WITH",
        about: "replace every FIND with WITH (selection, else whole file; literal; quote to use spaces)",
        run: |app, args| match two_args(&rest(args)) {
            Some((find, with)) => {
                let n = app.editor_mut().replace_all(&find, &with);
                if n > 0 {
                    app.editor_mut().message = Some(format!("replaced {n}"));
                }
            }
            None => {
                app.editor_mut().message = Some(
                    "usage: replace FIND WITH   (quote text with spaces: \"a b\" \"c\")".into(),
                )
            }
        },
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
        args: "[HEAD|all]",
        about: "this file side by side against the index (or HEAD); `all`: `git diff`",
        run: |app, args| app.diff_cmd(args),
    },
    Cmd {
        name: "gitstatus",
        aliases: &["status", "gitpanel"],
        args: "",
        about: "the git panel: status, stage, commit, branches, stash, push",
        run: |app, _| app.git_panel(),
    },
    Cmd {
        name: "stage",
        aliases: &[],
        args: "[PATH]",
        about: "git add this file (or PATH)",
        run: |app, args| app.stage_file(args, true),
    },
    Cmd {
        name: "unstage",
        aliases: &[],
        args: "[PATH]",
        about: "take this file (or PATH) out of the index",
        run: |app, args| app.stage_file(args, false),
    },
    Cmd {
        name: "stagehunk",
        aliases: &[],
        args: "",
        about: "stage the change at the cursor",
        run: |app, _| app.stage_hunk(),
    },
    Cmd {
        name: "unstagehunk",
        aliases: &[],
        args: "",
        about: "unstage the staged change at the cursor",
        run: |app, _| app.unstage_hunk(),
    },
    Cmd {
        name: "reverthunk",
        aliases: &[],
        args: "",
        about: "put the index version of the change at the cursor back (undoable)",
        run: |app, _| app.revert_hunk(),
    },
    Cmd {
        name: "hunk",
        aliases: &["peekhunk"],
        args: "",
        about: "show the change at the cursor inline (s stage, u unstage, r revert)",
        run: |app, _| app.peek_hunk(),
    },
    Cmd {
        name: "nexthunk",
        aliases: &[],
        args: "",
        about: "go to the next git change",
        run: |app, _| app.next_hunk(),
    },
    Cmd {
        name: "prevhunk",
        aliases: &[],
        args: "",
        about: "go to the previous git change",
        run: |app, _| app.previous_hunk(),
    },
    Cmd {
        name: "blame",
        aliases: &[],
        args: "",
        about: "git blame of this file in a tab (Enter shows the commit)",
        run: |app, _| app.blame_view(),
    },
    Cmd {
        name: "inlineblame",
        aliases: &[],
        args: "",
        about: "toggle the cursor line's blame",
        run: |app, _| app.toggle_inline_blame(),
    },
    Cmd {
        name: "commit",
        aliases: &[],
        args: "[MESSAGE...]",
        about: "commit with MESSAGE, or open a message tab (saving it commits)",
        run: |app, args| app.commit_cmd(args),
    },
    Cmd {
        name: "amend",
        aliases: &[],
        args: "",
        about: "toggle amending the last commit in the message tab",
        run: |app, _| app.toggle_draft(true),
    },
    Cmd {
        name: "signoff",
        aliases: &[],
        args: "",
        about: "toggle a Signed-off-by line in the message tab",
        run: |app, _| app.toggle_draft(false),
    },
    Cmd {
        name: "branches",
        aliases: &[],
        args: "",
        about: "local branches; Enter switches",
        run: |app, _| app.branches_cmd(),
    },
    Cmd {
        name: "branch",
        aliases: &[],
        args: "NAME",
        about: "create branch NAME and switch to it",
        run: |app, args| match args.first() {
            Some(n) => app.git_simple(&format!("new branch {n}"), &["switch", "-c", n]),
            None => app.branches_cmd(),
        },
    },
    Cmd {
        name: "switch",
        aliases: &["checkout"],
        args: "BRANCH",
        about: "switch to BRANCH",
        run: |app, args| match args.first() {
            Some(n) => app.git_simple(&format!("switch to {n}"), &["switch", n]),
            None => app.branches_cmd(),
        },
    },
    Cmd {
        name: "stash",
        aliases: &[],
        args: "[MESSAGE...]",
        about: "stash the current changes",
        run: |app, args| {
            let m = rest(args);
            if m.is_empty() {
                app.git_simple("stash", &["stash", "push"]);
            } else {
                app.git_simple("stash", &["stash", "push", "-m", &m]);
            }
        },
    },
    Cmd {
        name: "stashes",
        aliases: &[],
        args: "",
        about: "list stashes; Enter applies, p pops, D drops",
        run: |app, _| app.stashes_cmd(),
    },
    Cmd {
        name: "push",
        aliases: &[],
        args: "[ARGS...]",
        about: "git push in a terminal tab (credentials stay with git)",
        run: |app, args| app.git_command_terminal(&[&["push"], args].concat()),
    },
    Cmd {
        name: "pull",
        aliases: &[],
        args: "[ARGS...]",
        about: "git pull in a terminal tab",
        run: |app, args| app.git_command_terminal(&[&["pull"], args].concat()),
    },
    Cmd {
        name: "fetch",
        aliases: &[],
        args: "[ARGS...]",
        about: "git fetch in a terminal tab",
        run: |app, args| app.git_command_terminal(&[&["fetch"], args].concat()),
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
        name: "theme",
        aliases: &["colorscheme"],
        args: "[NAME]",
        about: "pick a theme with a live preview (Esc keeps the old one), or switch to NAME",
        run: |app, args| app.theme_command(args.first().copied()),
    },
    Cmd {
        name: "settings",
        aliases: &[],
        args: "[project]",
        about: "open settings.json (created with defaults if missing); project: this project's .skink/settings.json",
        run: |app, args| match args.first().copied() {
            Some("project") => app.open_project_settings(),
            _ => app.open_settings(),
        },
    },
    Cmd {
        name: "bindings",
        aliases: &["keys"],
        args: "",
        about: "open bindings.json (Micro format: key -> action); keys that replace a default or repeat show as problems",
        run: |app, _| {
            app.open_config_file("bindings.json", skink_config::DEFAULT_BINDINGS_FILE);
            app.say_binding_conflicts();
        },
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
        name: "DebugContinue",
        about: "Debug: start (the last configuration, or pick one) or continue",
        run: App::debug_continue,
    },
    AppAction {
        name: "DebugStop",
        about: "Debug: stop the program (again: kill the adapter)",
        run: App::debug_stop,
    },
    AppAction {
        name: "DebugStepOver",
        about: "Debug: step over (falls through to the next action when not debugging)",
        run: |a| {
            a.debug_step(crate::debug::Step::Over);
        },
    },
    AppAction {
        name: "DebugStepInto",
        about: "Debug: step into (falls through when not debugging)",
        run: |a| {
            a.debug_step(crate::debug::Step::Into);
        },
    },
    AppAction {
        name: "DebugStepOut",
        about: "Debug: step out (falls through when not debugging)",
        run: |a| {
            a.debug_step(crate::debug::Step::Out);
        },
    },
    AppAction {
        name: "DebugPause",
        about: "Debug: pause the running program (falls through when nothing runs)",
        run: |a| {
            a.debug_pause();
        },
    },
    AppAction {
        name: "ToggleBreakpoint",
        about: "Debug: breakpoint on the cursor line, on or off",
        run: App::toggle_breakpoint,
    },
    AppAction {
        name: "EditBreakpoint",
        about: "Debug: condition, hit count or log message of the cursor line's breakpoint",
        run: App::edit_breakpoint,
    },
    AppAction {
        name: "DebugPanel",
        about: "Debug: the panel (stack, variables, watch, breakpoints, console) on or off",
        run: App::toggle_debug_panel,
    },
    AppAction {
        name: "CycleRebaseVerb",
        about: "Rebase list: pick → reword → edit → squash → fixup → drop on the cursor line",
        run: App::cycle_rebase_verb,
    },
    AppAction {
        name: "ToggleMacro",
        about: "Start / stop recording a keyboard macro",
        run: App::toggle_macro,
    },
    AppAction {
        name: "PlayMacro",
        about: "Replay the last keyboard macro",
        run: App::play_macro,
    },
    AppAction {
        name: "TerminalHistory",
        about: "The terminal's history in a tab: search, select and copy with editor keys; Enter opens file:line",
        run: |a| a.terminal_history(crate::termtools::Focus::End),
    },
    AppAction {
        name: "SelectLastOutput",
        about: "Select the last command's output in the terminal history (needs OSC 133)",
        run: |a| a.terminal_history(crate::termtools::Focus::LastOutput),
    },
    AppAction {
        name: "PreviousPrompt",
        about: "Scroll the terminal to the previous shell prompt (needs OSC 133)",
        run: |a| a.jump_prompt(false),
    },
    AppAction {
        name: "NextPrompt",
        about: "Scroll the terminal to the next shell prompt (needs OSC 133)",
        run: |a| a.jump_prompt(true),
    },
    AppAction {
        name: "RunTask",
        about: "Pick a task (tasks.json or detected cargo / npm / go / zig / pytest / make) and run it",
        run: |a| a.task_command(""),
    },
    AppAction {
        name: "RerunTask",
        about: "Run the last task again",
        run: App::rerun_task,
    },
    AppAction {
        name: "NextHunk",
        about: "Go to the next git change",
        run: App::next_hunk,
    },
    AppAction {
        name: "PreviousHunk",
        about: "Go to the previous git change",
        run: App::previous_hunk,
    },
    AppAction {
        name: "PeekHunk",
        about: "Show the git change at the cursor (s stage, u unstage, r revert)",
        run: App::peek_hunk,
    },
    AppAction {
        name: "StageHunk",
        about: "Stage the git change at the cursor",
        run: App::stage_hunk,
    },
    AppAction {
        name: "UnstageHunk",
        about: "Unstage the staged change at the cursor",
        run: App::unstage_hunk,
    },
    AppAction {
        name: "RevertHunk",
        about: "Put the index version of the change at the cursor back",
        run: App::revert_hunk,
    },
    AppAction {
        name: "ToggleInlineBlame",
        about: "Show or hide the cursor line's git blame",
        run: App::toggle_inline_blame,
    },
    AppAction {
        name: "GitPanel",
        about: "Git status, staging, commit, branches, stash, push / pull",
        run: App::git_panel,
    },
    AppAction {
        name: "JumpBack",
        about: "Back to where you were before the last jump",
        run: App::jump_back,
    },
    AppAction {
        name: "JumpForward",
        about: "Forward again after JumpBack",
        run: App::jump_forward,
    },
    AppAction {
        name: "ToggleTerminalDock",
        about: "Show or hide a shell along the bottom",
        run: App::toggle_terminal_dock,
    },
    AppAction {
        name: "ToggleAgentDock",
        about: "Show or hide the agent panel on the right",
        run: App::toggle_agent_dock,
    },
    AppAction {
        name: "ToggleFold",
        about: "Fold or unfold the block at the cursor (by indentation)",
        run: App::toggle_fold,
    },
    AppAction {
        name: "FoldAll",
        about: "Fold every top-level block",
        run: App::fold_all,
    },
    AppAction {
        name: "UnfoldAll",
        about: "Open every fold",
        run: App::unfold_all,
    },
    AppAction {
        name: "FindInFiles",
        about: "Search every file in the project",
        run: App::project_search_prompt,
    },
    AppAction {
        name: "VSplit",
        about: "Split the pane: a new one to the right (same file, own cursor)",
        run: |app| app.split(crate::panes::Axis::Columns),
    },
    AppAction {
        name: "HSplit",
        about: "Split the pane: a new one below (same file, own cursor)",
        run: |app| app.split(crate::panes::Axis::Rows),
    },
    AppAction {
        name: "NextSplit",
        about: "Focus the next pane",
        run: |app| app.next_split(1),
    },
    AppAction {
        name: "PreviousSplit",
        about: "Focus the previous pane",
        run: |app| app.next_split(-1),
    },
    AppAction {
        name: "Unsplit",
        about: "Close the focused pane (its tab stays open)",
        run: App::unsplit,
    },
    AppAction {
        name: "GrowSplit",
        about: "Make the focused pane bigger",
        run: |app| app.resize_split(true),
    },
    AppAction {
        name: "ShrinkSplit",
        about: "Make the focused pane smaller",
        run: |app| app.resize_split(false),
    },
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
        name: "JumpToMatchingBrace",
        about: "Move to the bracket matching the one at the cursor",
        run: |app| app.doc.jump_to_matching_bracket(),
    },
    AppAction {
        name: "ScrollLeft",
        about: "Scroll the view left without moving the cursor",
        run: |app| {
            app.doc
                .scroll_horizontally(-(crate::doc::HWHEEL_COLS as isize))
        },
    },
    AppAction {
        name: "ScrollRight",
        about: "Scroll the view right without moving the cursor",
        run: |app| {
            app.doc
                .scroll_horizontally(crate::doc::HWHEEL_COLS as isize)
        },
    },
    AppAction {
        name: "TogglePreview",
        about: "Preview this Markdown or SVG file in a tab, or go back to its source",
        run: |app| app.toggle_preview(),
    },
    AppAction {
        name: "ToggleZoom",
        about: "zoom pane: hide the tab bar, file tree and status line",
        run: App::toggle_zoom,
    },
    AppAction {
        name: "FontBigger",
        about: "bigger terminal font (EveryAI or Kitty)",
        run: |app| app.change_font(skink_host::tools::FontStep::Bigger),
    },
    AppAction {
        name: "FontSmaller",
        about: "smaller terminal font (EveryAI or Kitty)",
        run: |app| app.change_font(skink_host::tools::FontStep::Smaller),
    },
    AppAction {
        name: "FontReset",
        about: "default terminal font size (EveryAI or Kitty)",
        run: |app| app.change_font(skink_host::tools::FontStep::Reset),
    },
    AppAction {
        name: "ExpandSelection",
        about: "grow the selection to the enclosing syntax node",
        run: |app| {
            app.doc.expand_selection();
        },
    },
    AppAction {
        name: "ShrinkSelection",
        about: "undo the last ExpandSelection",
        run: |app| {
            app.doc.shrink_selection();
        },
    },
    AppAction {
        name: "TreeWider",
        about: "make the file tree wider",
        run: |app| app.resize_sidebar(2),
    },
    AppAction {
        name: "TreeNarrower",
        about: "make the file tree narrower",
        run: |app| app.resize_sidebar(-2),
    },
    AppAction {
        name: "ToggleHiddenFiles",
        about: "show / hide dotfiles in the file tree (works from the editor too)",
        run: App::toggle_hidden_files,
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
    // ---- code intelligence ----
    AppAction {
        name: "Complete",
        about: "completion list at the cursor (language server)",
        run: |app| app.request_completion(crate::intel::Trigger::Manual),
    },
    AppAction {
        name: "Hover",
        about: "type and docs of the symbol at the cursor (its value while debugging)",
        run: |a| {
            if !a.debug_hover() {
                a.hover();
            }
        },
    },
    AppAction {
        name: "GotoDefinition",
        about: "go to the definition of the symbol at the cursor",
        run: |app| app.goto(GotoKind::Definition, false),
    },
    AppAction {
        name: "GotoDeclaration",
        about: "go to the declaration of the symbol at the cursor",
        run: |app| app.goto(GotoKind::Declaration, false),
    },
    AppAction {
        name: "GotoTypeDefinition",
        about: "go to the definition of the type at the cursor",
        run: |app| app.goto(GotoKind::TypeDefinition, false),
    },
    AppAction {
        name: "GotoImplementation",
        about: "go to the implementations of the symbol at the cursor",
        run: |app| app.goto(GotoKind::Implementation, false),
    },
    AppAction {
        name: "PeekDefinition",
        about: "show the definition in a popup without leaving",
        run: |app| app.goto(GotoKind::Definition, true),
    },
    AppAction {
        name: "FindReferences",
        about: "every use of the symbol at the cursor, in a results tab",
        run: App::find_references,
    },
    AppAction {
        name: "CodeActions",
        about: "quick fixes and refactorings at the cursor",
        run: App::code_actions,
    },
    AppAction {
        name: "Format",
        about: "format the selection or the file (language server)",
        run: |app| {
            app.format(crate::intel::AfterFormat::Nothing);
        },
    },
    AppAction {
        name: "Outline",
        about: "symbols in this file (quick open with @)",
        run: App::open_outline,
    },
    AppAction {
        name: "WorkspaceSymbols",
        about: "symbols in the project (quick open with #)",
        run: App::open_workspace_symbols,
    },
    AppAction {
        name: "CommandPalette",
        about: "every action and command (quick open with >)",
        run: App::open_palette,
    },
    AppAction {
        name: "AiPrompt",
        about: "AI prompt box for the selection or the code at the cursor",
        run: App::open_ai_prompt,
    },
    AppAction {
        name: "AgentPanel",
        about: "show the agent panel (chat with the ACP agent)",
        run: |app| app.open_agent_panel(""),
    },
    AppAction {
        name: "AgentSendErrors",
        about: "send the errors to the agent: diagnostics, or the last command in a terminal",
        run: App::send_errors_to_agent,
    },
    AppAction {
        name: "RenameSymbol",
        about: "rename the symbol at the cursor (asks for the new name)",
        run: App::prompt_rename_symbol,
    },
    AppAction {
        name: "ReviewAgentEdits",
        about: "review the edits the agent staged",
        run: App::review_staged,
    },
    AppAction {
        name: "Problems",
        about: "errors and warnings of the open files, in a results tab",
        run: App::show_problems,
    },
    AppAction {
        name: "NextDiagnostic",
        about: "next error or warning in this file",
        run: |app| app.goto_diagnostic(true),
    },
    AppAction {
        name: "PreviousDiagnostic",
        about: "previous error or warning in this file",
        run: |app| app.goto_diagnostic(false),
    },
];

/// Short command-line names for actions, so every key has a typed fallback
/// (`Ctrl-E` then the name) when a terminal, OS or keyboard layout eats the
/// chord. Any action also runs by its own name in any spelling:
/// `:togglecomment`, `:toggle-comment`, `:ToggleComment`.
pub static SHORT_NAMES: &[(&str, &str)] = &[
    // editing
    ("undo", "Undo"),
    ("redo", "Redo"),
    ("copy", "Copy|CopyLine"),
    ("cut", "Cut|CutLine"),
    ("paste", "Paste"),
    ("duplicate", "Duplicate|DuplicateLine"),
    ("moveup", "MoveLinesUp"),
    ("movedown", "MoveLinesDown"),
    ("comment", "ToggleComment"),
    ("selectall", "SelectAll"),
    ("expand", "ExpandSelection"),
    ("contract", "ShrinkSelection"),
    ("bracket", "JumpToMatchingBrace"),
    ("complete", "Complete"),
    ("indent", "IndentSelection"),
    ("outdent", "OutdentSelection|OutdentLine"),
    // multiple cursors
    ("addcursor", "SpawnMultiCursor"),
    ("cursorabove", "SpawnMultiCursorUp"),
    ("cursorbelow", "SpawnMultiCursorDown"),
    ("skipcursor", "SkipMultiCursor"),
    ("onecursor", "RemoveAllMultiCursors"),
    // find and navigate
    ("find", "Find"),
    ("findnext", "FindNext"),
    ("findprev", "FindPrevious"),
    ("palette", "CommandPalette"),
    ("hover", "Hover"),
    ("typedef", "GotoTypeDefinition"),
    ("peek", "PeekDefinition"),
    ("fix", "CodeActions"),
    ("actions", "CodeActions"),
    ("allsymbols", "WorkspaceSymbols"),
    ("nextproblem", "NextDiagnostic"),
    ("prevproblem", "PreviousDiagnostic"),
    // tabs, splits, panels
    ("nexttab", "NextTab"),
    ("prevtab", "PreviousTab"),
    ("newtab", "AddTab"),
    ("nextsplit", "NextSplit"),
    ("prevsplit", "PreviousSplit"),
    ("files", "ToggleTree"),
    ("sidebar", "ToggleSidebar"),
    ("dotfiles", "ToggleHiddenFiles"),
    ("newterm", "NewTerminal"),
    ("toggleterm", "ToggleTerminal"),
    // AI
    ("ask", "AiPrompt"),
    ("senderrors", "AgentSendErrors"),
    // macros and tasks
    ("record", "ToggleMacro"),
    ("replay", "PlayMacro"),
    ("rerun", "RerunTask"),
    // terminal
    ("nextprompt", "NextPrompt"),
    ("prevprompt", "PreviousPrompt"),
    ("lastoutput", "SelectLastOutput"),
    // debugger
    ("continue", "DebugContinue"),
    ("step", "DebugStepOver"),
    ("stepin", "DebugStepInto"),
    ("stepout", "DebugStepOut"),
    ("pause", "DebugPause"),
    ("condition", "EditBreakpoint"),
];

/// `ToggleComment`, `toggle-comment` and `togglecomment` compare equal.
fn loose(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// The action (or `A|B` chain) a typed command name stands for: a short
/// name, or an action's own name in any spelling.
pub fn action_for(name: &str, registry: &CommandRegistry) -> Option<String> {
    if let Some((_, a)) = SHORT_NAMES.iter().find(|(n, _)| *n == name) {
        return Some((*a).to_owned());
    }
    let want = loose(name);
    registry
        .iter()
        .map(|c| c.name)
        .chain(APP_ACTIONS.iter().map(|a| a.name))
        .find(|n| loose(n) == want)
        .map(str::to_owned)
}

/// The command to type for a key's action chain: its short name if it has
/// one, else the first action's name in lower case.
pub fn command_for(chain: &str) -> Option<String> {
    if chain.starts_with("command-edit:") {
        return None;
    }
    if let Some((n, _)) = SHORT_NAMES.iter().find(|(_, a)| *a == chain) {
        return Some((*n).to_owned());
    }
    // Completion-popup steps only mean something on the key itself.
    let first = chain
        .split(['|', ','])
        .map(str::trim)
        .find(|n| !matches!(*n, "Autocomplete" | "CycleAutocompleteBack"))?;
    if let Some(c) = COMMANDS.iter().find(|c| c.name == first.to_lowercase()) {
        return Some(c.name.to_owned());
    }
    match SHORT_NAMES
        .iter()
        .find(|(_, a)| a.split('|').next() == Some(first))
    {
        Some((n, _)) => Some((*n).to_owned()),
        None => Some(first.to_lowercase()),
    }
}

/// A key as people read it: `Alt-N` (Micro's capital = Shift) becomes
/// `Alt-Shift-N`, `Ctrl-p` becomes `Ctrl-P`, `CtrlPageUp` `Ctrl-PageUp`.
pub fn pretty_key(k: &str) -> String {
    use skink_config::Key;
    let Some(spec) = skink_config::parse_key(k) else {
        return k.to_owned();
    };
    let mut out = String::new();
    if spec.ctrl {
        out.push_str("Ctrl-");
    }
    if spec.alt {
        out.push_str("Alt-");
    }
    if spec.shift {
        out.push_str("Shift-");
    }
    match spec.key {
        Key::Char(' ') => out.push_str("Space"),
        Key::Char(c) => out.extend(c.to_uppercase()),
        Key::F(n) => out.push_str(&format!("F{n}")),
        Key::BackTab => out.push_str("Shift-Tab"),
        other => out.push_str(&format!("{other:?}")),
    }
    out
}

/// Every app action, editor action and `:` command for the palette, with
/// its description and current key.
pub fn palette_items(app: &App) -> Vec<crate::picker::Item> {
    use crate::picker::{Item, Target};
    let key_for = |name: &str| {
        app.keymap
            .bindings()
            .into_iter()
            .filter(|(_, chain)| chain.split(['|', ',']).any(|n| n.trim() == name))
            // Already readable (`Alt-P`); pretty_key would read the capital
            // as Shift. The simplest key when several do it.
            .map(|(k, _)| k)
            .min_by_key(String::len)
            .unwrap_or_default()
    };
    let typed = |name: &str, about: &str| match command_for(name) {
        Some(c) => format!("{about}  (:{c})"),
        None => about.to_owned(),
    };
    // Everyday editing actions first (an empty filter lists them), then the
    // app's (tree, git, debugger...), then the typed commands.
    let mut items: Vec<Item> = Vec::new();
    for c in app.registry.iter() {
        if APP_ACTIONS.iter().any(|a| a.name == c.name) {
            continue; // the app's version wins
        }
        items.push(Item {
            label: c.name.to_owned(),
            detail: typed(c.name, c.description),
            target: Target::Command(c.name.to_owned()),
            tag: leak_key(key_for(c.name)),
        });
    }
    for a in APP_ACTIONS {
        items.push(Item {
            label: a.name.to_owned(),
            detail: typed(a.name, a.about),
            target: Target::Command(a.name.to_owned()),
            tag: leak_key(key_for(a.name)),
        });
    }
    for c in COMMANDS {
        items.push(Item {
            label: format!(":{} {}", c.name, c.args).trim_end().to_owned(),
            detail: c.about.to_owned(),
            target: Target::Command(c.name.to_owned()),
            tag: "",
        });
    }
    items
}

/// Key names are few and fixed per session; the picker wants `&'static`.
fn leak_key(k: String) -> &'static str {
    use std::{collections::HashMap, sync::Mutex};
    static KEYS: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);
    let mut g = KEYS.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(v) = map.get(&k) {
        return v;
    }
    let v: &'static str = Box::leak(k.clone().into_boxed_str());
    map.insert(k, v);
    v
}

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

/// Split a program's command line into words; '…' and "…" group a word
/// with spaces (`:term sh -c 'exit 7'`). Backslashes stay literal
/// so regex arguments are untouched; with an unbalanced quote the line is
/// split on whitespace as typed.
pub fn split_words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            None => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return line.split_whitespace().map(str::to_owned).collect();
    }
    if in_word {
        out.push(cur);
    }
    out
}

impl App {
    /// Run a command-line line: a `:` command, or any action by name.
    pub fn run_command(&mut self, line: &str) {
        // `::help` (the ":" typed out of habit) is `help`.
        let line = line.trim_start().trim_start_matches(':');
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { return };
        let args: Vec<&str> = parts.collect();
        // `:40` / `:40:5` go to that line, as in Vim and Micro.
        if name.starts_with(|c: char| c.is_ascii_digit())
            && name.split(':').all(|n| n.parse::<usize>().is_ok())
        {
            return self.run_command(&format!("goto {name}"));
        }
        let cmd = COMMANDS
            .iter()
            .find(|c| c.names().any(|n| n == name))
            .or_else(|| {
                COMMANDS
                    .iter()
                    .find(|c| c.names().any(|n| n.eq_ignore_ascii_case(name)))
            });
        if let Some(cmd) = cmd {
            return (cmd.run)(self, &args);
        }
        if self.registry.get(name).is_some() || APP_ACTIONS.iter().any(|a| a.name == name) {
            return self.run_chain(name);
        }
        if let Some(chain) = action_for(name, &self.registry) {
            return self.run_chain(&chain);
        }
        if self.run_script_command(name, &args) {
            return;
        }
        let near = similar_names(name, &self.registry);
        self.doc.editor.message = Some(match near.as_slice() {
            [] => format!("Unknown command: {name} (F1 lists every command)"),
            some => format!(
                "Unknown command: {name}. Did you mean {}?",
                some.iter()
                    .map(|n| format!(":{n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
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
        self.check_config_doc();
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
        let (list, ok) = run_capture(
            "git diff --name-only --diff-filter=U",
            Some(&top),
            Duration::from_secs(10),
        );
        // A failing git (corrupt index) prints its error, not file names.
        if !ok {
            let why = list
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("failed");
            self.doc.editor.message = Some(format!("git: {}", why.trim()));
            return;
        }
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
        // Every file's cursor on its first conflict, so :ours / :theirs work
        // in each tab without hunting for the block; the first file shows.
        for f in &files {
            self.open_path(f);
            if let Some(c) = self.doc.editor.conflicts().first() {
                let line = c.start + 1;
                self.doc.editor.goto(Some(line), Some(1));
            }
        }
        self.open_path(&files[0]); // already open: switches to it
        self.doc.editor.message = Some(format!(
            "{} conflicted file(s) opened; :ours / :theirs / :both resolve the block",
            files.len()
        ));
    }
}

/// Command names and actions starting with `prefix`.
pub fn complete_name(prefix: &str, registry: &CommandRegistry) -> Vec<String> {
    let all = || {
        COMMANDS
            .iter()
            .flat_map(Cmd::names)
            .filter(|n| n.chars().all(char::is_alphanumeric))
            .chain(registry.iter().map(|c| c.name))
            .chain(APP_ACTIONS.iter().map(|a| a.name))
            .chain(SHORT_NAMES.iter().map(|(n, _)| *n))
    };
    let mut out: Vec<String> = all()
        .filter(|n| n.starts_with(prefix))
        .map(str::to_owned)
        .collect();
    // Any case and spelling (`upper` -> `UpperCase`) when nothing matches
    // exactly.
    if out.is_empty() {
        let want = loose(prefix);
        out = all()
            .filter(|n| loose(n).starts_with(&want))
            .map(str::to_owned)
            .collect();
    }
    out.sort();
    out.dedup();
    out
}

/// Names close to an unknown `name`: ones it starts or is contained in,
/// in any case (at most four).
fn similar_names(name: &str, registry: &CommandRegistry) -> Vec<String> {
    let want = loose(name);
    if want.len() < 2 {
        return Vec::new();
    }
    let mut out = complete_name(name, registry);
    if out.is_empty() {
        out = COMMANDS
            .iter()
            .flat_map(Cmd::names)
            .filter(|n| n.chars().all(char::is_alphanumeric))
            .chain(registry.iter().map(|c| c.name))
            .chain(APP_ACTIONS.iter().map(|a| a.name))
            .chain(SHORT_NAMES.iter().map(|(n, _)| *n))
            .filter(|n| loose(n).contains(&want))
            .map(str::to_owned)
            .collect();
        out.sort();
        out.dedup();
    }
    out.truncate(4);
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
    full.push_str(
        "\n## Key bindings (each also runs from the command line: Ctrl-E, then the :name)\n",
    );
    for (k, a) in bindings {
        let typed = command_for(a).map(|c| format!(":{c}")).unwrap_or_default();
        full.push_str(&format!("  {:<20} {typed:<16} {a}\n", pretty_key(k)));
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

    #[test]
    fn replace_arguments() {
        let t = two_args;
        assert_eq!(t("foo bar"), Some(("foo".into(), "bar".into())));
        assert_eq!(
            t(r#""a b" "c \"d\"""#),
            Some(("a b".into(), "c \"d\"".into()))
        );
        assert_eq!(t(r#"x """#), Some(("x".into(), String::new())));
        assert_eq!(t("only"), None);
        assert_eq!(t("a b c"), None);
    }

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
    fn command_words_honour_quotes() {
        // v1.0 B: `:term sh -c 'exit 7'` ran sh with "'exit" and "7'".
        assert_eq!(
            split_words("term sh -c 'exit 7'"),
            ["term", "sh", "-c", "exit 7"]
        );
        assert_eq!(split_words(r#"cat "my file.txt""#), ["cat", "my file.txt"]);
        assert_eq!(split_words(r"psearch a\d+  b"), ["psearch", r"a\d+", "b"]);
        assert_eq!(split_words("psearch don't"), ["psearch", "don't"]);
        assert_eq!(split_words("x ''"), ["x", ""]);
    }

    #[test]
    fn help_has_sections_and_topics() {
        let r = CommandRegistry::builtin();
        let all = help_text(None, &defaults(), &r);
        for needle in [
            "## Tabs",
            "## Merge conflicts",
            "conflicts",
            "Ctrl-Q",
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
    fn every_default_key_has_a_typed_command() {
        // Owner request: a key a terminal or OS swallows must still be
        // reachable from Ctrl-E.
        let r = CommandRegistry::builtin();
        let presets = skink_config::VSCODE_BINDINGS
            .iter()
            .chain(skink_config::SUBLIME_BINDINGS);
        for (key, chain) in skink_config::DEFAULT_BINDINGS.iter().chain(presets) {
            let Some(cmd) = command_for(chain) else {
                continue; // command-edit: opens the command line itself
            };
            let runs = COMMANDS.iter().any(|c| c.names().any(|n| n == cmd))
                || action_for(&cmd, &r).is_some();
            assert!(runs, "{key} -> {chain}: :{cmd} does not run");
        }
        for (n, chain) in SHORT_NAMES {
            for a in chain.split('|') {
                assert!(
                    r.get(a).is_some() || APP_ACTIONS.iter().any(|x| x.name == a),
                    ":{n} names unknown action {a}"
                );
            }
            assert!(
                !COMMANDS.iter().any(|c| c.names().any(|m| m == *n)),
                ":{n} shadows a command"
            );
        }
        assert_eq!(
            action_for("toggle-comment", &r).as_deref(),
            Some("ToggleComment")
        );
        assert_eq!(pretty_key("Alt-N"), "Alt-Shift-N");
        assert_eq!(pretty_key("Ctrl-p"), "Ctrl-P");
        assert_eq!(pretty_key("CtrlPageUp"), "Ctrl-PageUp");
        assert_eq!(pretty_key("Backtab"), "Shift-Tab");
    }

    #[test]
    fn names_are_unique() {
        let mut all: Vec<&str> = COMMANDS.iter().flat_map(Cmd::names).collect();
        all.extend(APP_ACTIONS.iter().map(|a| a.name));
        all.extend(SHORT_NAMES.iter().map(|(n, _)| *n));
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n, "duplicate command or action name");
        assert!(takes_path("e") && takes_path("open") && !takes_path("goto"));
    }
}
