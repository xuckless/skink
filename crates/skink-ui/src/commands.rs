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
        about: "zoom pane: only the text (also Alt-Z, F5)",
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
        args: "[MESSAGE | cancel | stop | send]",
        about: "agent panel: chat with the configured ACP agent; send = selection or terminal output",
        run: |app, args| match args.first().copied() {
            Some("cancel") if args.len() == 1 => app.cancel_panel(),
            Some("stop") if args.len() == 1 => app.stop_agent(),
            Some("send") if args.len() == 1 => app.send_context_to_agent(),
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
        about: "type and docs of the symbol at the cursor",
        run: App::hover,
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

/// Every app action, editor action and `:` command for the palette, with
/// its description and current key.
pub fn palette_items(app: &App) -> Vec<crate::picker::Item> {
    use crate::picker::{Item, Target};
    let key_for = |name: &str| {
        app.keymap
            .bindings()
            .into_iter()
            .find(|(_, chain)| chain.split(['|', ',']).any(|n| n.trim() == name))
            .map(|(k, _)| k)
            .unwrap_or_default()
    };
    let mut items: Vec<Item> = Vec::new();
    for a in APP_ACTIONS {
        items.push(Item {
            label: a.name.to_owned(),
            detail: a.about.to_owned(),
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
    for c in app.registry.iter() {
        if items.iter().any(|i| i.label == c.name) {
            continue;
        }
        items.push(Item {
            label: c.name.to_owned(),
            detail: c.description.to_owned(),
            target: Target::Command(c.name.to_owned()),
            tag: leak_key(key_for(c.name)),
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
