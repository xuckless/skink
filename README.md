<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/readme/logo-dark.svg"/>
  <img src="docs/readme/logo-light.svg" alt="Skink" width="260"/>
</picture>

**The code editor for your terminal, without modes**

<a href="https://github.com/xuckless/skink/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/xuckless/skink?style=flat-square&color=7aa2f7&label=release"/></a> <a href="https://github.com/xuckless/skink/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/xuckless/skink/ci.yml?branch=main&style=flat-square&label=CI"/></a> <a href="#license"><img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-7aa2f7?style=flat-square"/></a>

[Docs](https://skink.cc/docs/) · [Quick start](https://skink.cc/start/quick-start/) · [Compare](https://skink.cc/compare/) · [Releases](https://github.com/xuckless/skink/releases/latest)

</div>

## Install

```sh
curl -fsSL https://skink.cc/install.sh | sh      # Linux, macOS
```

```powershell
irm https://skink.cc/install.ps1 | iex           # Windows
```

No sudo. The installer checks the SHA-256 and puts one binary in your user folder. [Other ways to install](https://skink.cc/start/install/)

## Keys you already know

No insert mode, no motions to learn. Copy, paste, undo and save work the way they do everywhere else.

<img src="docs/readme/keys.svg" alt="Typing straight into a file, then Ctrl+S saves, Ctrl+C copies, Ctrl+Z undoes and Ctrl+Q quits" width="480"/>

## Everything is a tab

Files, shells, diffs, your AI agent and the help share one tab bar. <kbd>F12</kbd> flips between the file and the shell.

<img src="docs/readme/tabs.svg" alt="One tab bar holding a file, a shell, a diff, an AI agent and the help" width="480"/>

## The same editor over SSH

One binary, no GUI, no server component. What you copy on the server lands on your own clipboard.

<img src="docs/readme/ssh.svg" alt="Your laptop runs ssh to a server where Skink runs; text you copy there lands on your laptop's clipboard" width="480"/>

## Bring your own AI agent

Claude Code, Gemini CLI, Codex and others run in a tab or drive the editor directly. Their edits arrive as a diff you keep or drop.

<img src="docs/readme/agent.svg" alt="An AI agent's edit appears as an inline diff; pressing y keeps the change" width="480"/>

## First steps

```sh
skink .                  # open a folder
skink src/main.rs:42     # open a file at line 42
```

<kbd>Ctrl</kbd>+<kbd>S</kbd> save · <kbd>Ctrl</kbd>+<kbd>Q</kbd> quit · <kbd>Alt</kbd>+<kbd>P</kbd> open a file · <kbd>Ctrl</kbd>+<kbd>E</kbd> command line · <kbd>F10</kbd> menus · <kbd>F12</kbd> terminal

Also inside: a settings screen, language servers, git hunks, blame and code review, a debugger, multiple cursors, soft wrap, project search, packages and Lua scripts. [Read the docs](https://skink.cc/docs/)

## Develop

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

Code adapted from other projects says so in its header; the full list is in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Built on [Micro](https://micro-editor.github.io)'s keys, [ratatui](https://ratatui.rs), [ropey](https://github.com/cessen/ropey), [tree-sitter](https://tree-sitter.github.io), [Lapce](https://lapce.dev)'s LSP plumbing and the [Agent Client Protocol](https://agentclientprotocol.com).

## License

[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
