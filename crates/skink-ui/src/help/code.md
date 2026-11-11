# Code intelligence

Language servers start on first use, when installed: rust-analyzer, typescript-language-server, basedpyright or pyright, gopls, `dart language-server` and zls. Inside EveryAI they come from EveryAI.

| Key | Does |
|---|---|
| Ctrl-Space | completion (also opens after two letters or `.` `::` `->`) |
| Alt-K | type and docs of the symbol |
| Alt-G / Alt-Shift-G | go to definition / type definition |
| Alt-J | peek the definition in a popup |
| Alt-R | references, in a results tab |
| Alt-Enter | code actions: quick fixes, refactorings |
| Alt-Shift-R | rename the symbol everywhere (`:rn NEW`) |
| Alt-Shift-F | format the selection or file |
| Ctrl-R / Alt-W | symbols in this file / in the project |
| Shift-F8 | problems in the open files |
| F6 / Shift-F6 | next / previous problem |
| F1 | command palette |

In the completion list Up/Down choose, Enter or Tab insert, Esc closes; docs show beside it. Snippets select their first placeholder, Tab moves to the next. `"autocomplete": false` stops it opening by itself.

Signature help opens after `(` and `,`. Breadcrumbs show in the status line. `"inlayhints": true` shows type hints, `"formatonsave": true` formats on save. A lightbulb in the sign column means code actions are available. `:impl` lists implementations.

## Quick open

Alt-P (or `:recent`) lists tabs, recent and project files. A first character switches the list: `>` commands, `@` symbols in the file, `#` symbols in the project. A `/` matches path segments in order: `s/c/grade` finds `src/color/grade.rs`.

## Refactoring

Edits to several files open the refactor preview: files and hunks with checkboxes and a side-by-side diff. Up/Down move, Space toggles, `a` all, `n` none, Enter applies (one undo step per file), Esc cancels.

| Command | Does |
|---|---|
| `:safedelete` | delete the function or type at the cursor once nothing outside uses it (`:safedelete!` previews it unchecked) |
| `:mv PATH` | move or rename the file; servers update imports |
| `:extract var\|fn [NAME]` | extract the selection (var also works without a server for Rust, TS/JS and Python) |

## Your own servers

In settings.json:

```json
"lsp.dart": "fvm dart language-server --protocol=lsp",
"lsp.elixir": {"command": "elixir-ls", "extensions": ["ex", "exs"],
               "rootmarkers": ["mix.exs"], "initoptions": {}, "env": {}},
"lsp.python": false
```

Names: rust, typescript (also javascript, ts, js), python, go, dart, zig, or a new one. Changes apply at the next start.

`:lsp` shows the server a file gets, where it came from, its memory and progress; `:lsp restart`, `:lsp log`. A crashed server restarts once by itself; one with no open files for 10 minutes stops and starts again when needed.
