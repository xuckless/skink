# Skink

Lightweight, modeless terminal editor in Rust. Early, but it edits, saves, shows LSP diagnostics and hosts a terminal pane for AI agents.

    skink {path}            # open a file or directory
    skink file.rs:42:7      # jump to line 42, column 7
    skink                   # empty scratch buffer

Everything opens as a tab: files, terminals, `:help`, `:diff`, `:conflicts`, command output (`:run CMD`),
your settings (`:settings`) and key bindings (`:bindings`). The tab bar is at the top; Alt-1..9 / Ctrl-PageUp/Down
switch, Alt-P is quick open (open tabs, recent files, project files), Ctrl-E is the command line (`help` documents
everything). Keys are Micro's defaults (Ctrl-S save, Ctrl-Q close tab, Ctrl-Z/Y undo/redo, Ctrl-F find, mouse works).
**F12** jumps between your file and a terminal tab (Shift-F12 opens another), F8 is the file tree.
Tokyo Night colors, tree-sitter highlighting (Rust, TS/JS, Python, Go, Zig, JSON, TOML, Bash), LSP diagnostics,
git conflict resolution (`:ours` / `:theirs` / `:both`). Icons are Nerd Font glyphs (`SKINK_ICONS=ascii` to disable).

## Layout

| Crate | Role |
|---|---|
| `skink-core` | rope buffer, selection, undo/redo, save (CRLF-safe), find, Micro-style command registry; no UI |
| `skink-config` | Micro key names and Micro's default bindings |
| `skink-syntax` | tree-sitter highlighting, lazy per-language |
| `skink-lsp` | thin LSP client (Lapce transport/lifecycle), lazy per-language servers |
| `skink-term` | terminal emulator pane: portable-pty + vendored vt100 |
| `skink-ai` | ACP client + 99-style prompter (rewrite/search/worker), `AgentController` seam |
| `skink-host` | EveryAI handover: MCP-over-HTTP client, `SKINK_HOST_URL` / `SKINK_HOST_TOKEN` (see `docs/HANDOVER.md`) |
| `skink-ui` | ratatui front end: tabs (files + terminals), tree, quick open, command line, themes; screen tests |
| `skink` | binary |

## Develop

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo deny check            # licenses: permissive only
    scripts/measure.py [path]   # cold start + peak RSS in a real pty (Linux/macOS)

License: MIT OR Apache-2.0.
