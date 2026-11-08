# Skink

Lightweight, modeless terminal editor in Rust. Work in progress (Phase 0).

    skink {path}            # open a file or directory
    skink file.rs:42:7      # jump to line 42, column 7
    skink                   # empty scratch buffer

Keys today: arrows, PageUp/PageDown, Home/End, Ctrl-Home/End, Ctrl-Q to quit
(Micro-compatible names; editing arrives in Phase 1).

## Layout

| Crate | Role |
|---|---|
| `skink-core` | buffer (ropey), cursor, command registry, `path:line:col` parsing; no UI |
| `skink-config` | Micro-format key names and default bindings |
| `skink-syntax` | tree-sitter highlighting (stub) |
| `skink-lsp` | thin LSP client seam (stub) |
| `skink-term` | embedded terminal pane (stub) |
| `skink-ai` | ACP/agent seam (stub) |
| `skink-host` | EveryAI handover via `SKINK_HOST_SOCKET` / `SKINK_HOST_TOKEN` |
| `skink-ui` | ratatui front end + screen-snapshot tests |
| `skink` | binary |

## Develop

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo deny check            # licenses: permissive only
    scripts/measure.py [path]   # cold start + peak RSS in a real pty (Linux/macOS)

License: MIT OR Apache-2.0.
