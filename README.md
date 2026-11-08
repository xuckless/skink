# Skink

Lightweight, modeless terminal editor in Rust. Early, but it edits, saves, shows LSP diagnostics and hosts a terminal pane for AI agents.

    skink {path}            # open a file or directory
    skink file.rs:42:7      # jump to line 42, column 7
    skink                   # empty scratch buffer

Keys are Micro's defaults (Ctrl-S save, Ctrl-Q quit, Ctrl-Z/Y undo/redo, Ctrl-C/X/V clipboard, Ctrl-F find,
Ctrl-L goto, Ctrl-E command bar, Shift+arrows select, Alt-Up/Down move lines, Tab/Shift-Tab indent, mouse click/drag/wheel).
**F12** opens a terminal pane (a shell, or `Ctrl-E` then `term claude` to run an AI agent CLI) and switches focus
between it and the editor; **F11** closes it.

## Layout

| Crate | Role |
|---|---|
| `skink-core` | rope buffer, selection, undo/redo, save (CRLF-safe), find, Micro-style command registry; no UI |
| `skink-config` | Micro key names and Micro's default bindings |
| `skink-syntax` | tree-sitter highlighting (stub) |
| `skink-lsp` | thin LSP client (Lapce transport/lifecycle), lazy per-language servers |
| `skink-term` | terminal emulator pane: portable-pty + vendored vt100 |
| `skink-ai` | ACP client + 99-style prompter (rewrite/search/worker), `AgentController` seam |
| `skink-host` | EveryAI handover: MCP-over-HTTP client, `SKINK_HOST_URL` / `SKINK_HOST_TOKEN` (see `docs/HANDOVER.md`) |
| `skink-ui` | ratatui front end + screen-snapshot tests |
| `skink` | binary |

## Develop

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo deny check            # licenses: permissive only
    scripts/measure.py [path]   # cold start + peak RSS in a real pty (Linux/macOS)

License: MIT OR Apache-2.0.
