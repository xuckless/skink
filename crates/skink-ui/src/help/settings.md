# Settings and key bindings

| Command | Opens |
|---|---|
| `:settings` | settings.json (made with defaults when missing) |
| `:settings project` | this project's `.skink/settings.json`: its keys override yours here |
| `:bindings` | bindings.json |
| `:reload` | apply the files again |

Saving settings.json applies it. Config lives in `~/.config/skink` (or `$SKINK_CONFIG_DIR`); both files accept `//` comments and trailing commas.

```json
{
  "tabsize": 4,
  "icons": "auto",
  "indentguides": true,
  "updates": "notify",
  "[python]": {"tabsize": 4, "rulers": [88]},
  "[rust]": {"formatonsave": true, "lsp": "rust-analyzer"}
}
```

A `[section]` is named by language (rust, typescript, tsx, javascript, python, go, dart, …) or by extension (`"[md]"`).

Opening or saving a settings file checks it: bad JSON, unknown keys (with "did you mean"), wrong types and out-of-range values show in the gutter and in Shift-F8, at the key. Inside a key's quotes, completion offers every setting with its description. JSON schemas for both files are written to the state folder and handed to vscode-json-language-server when installed.

## Key bindings

bindings.json uses Micro's format, key name to action:

```json
{"Ctrl-j": "Save", "Alt-g": "command-edit:goto ", "Ctrl-5": "JumpToMatchingBrace"}
```

- Key names: `Ctrl-x`, `Alt-x`, `Shift-Up`, `CtrlShiftRight`, `F5`, `PageDown`, `Backtab`.
- `"Copy|CopyLine"` runs the first that works; `"Escape,Deselect"` runs both.
- `"command-edit:TEXT"` opens the command line with TEXT.
- `:bindings` marks each key that replaces a default, and keys written twice (`Ctrl-q` and `CtrlQ` are one key).

`"keymap": "vscode"` or `"sublime"` puts those editors' keys over Micro's. VS Code: Ctrl-P quick open, Ctrl-Shift-P palette, Ctrl-G go to line, F12 definition, Shift-F12 references, F2 rename, F8 next problem, Ctrl-. code actions, Ctrl-` terminal, Ctrl-T symbols, Ctrl-W close, Ctrl-N new tab, Ctrl-Shift-I AI prompt. Sublime: the same palette and go-to keys plus Ctrl-B build task and Ctrl-Shift-Up/Down move lines. bindings.json still goes on top.

Every key, the [actions](actions) you can bind and the [commands](commands) have their own pages.
