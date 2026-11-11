# Settings and key bindings

| Command | Opens |
|---|---|
| `:settings` | the settings screen: every setting, grouped and searchable |
| `:settings KEY` | the screen at that setting (`:settings tabsize`) |
| `:keys [ACTION]` | the key-binding editor (`:keys Save`) |
| `:settings json` | settings.json as text (made with defaults when missing) |
| `:settings project` | this project's `.skink/settings.json`: its keys override yours here |
| `:bindings` | bindings.json as text |
| `:reload` | apply the files again, and reload installed packages |

## The settings screen

Type to search (names, descriptions, values). Each setting shows its value and where it comes from: default, settings.json or project.

| Key | Does |
|---|---|
| Enter | flip a toggle, pick from a list, or type a value |
| Left / Right | a number one down or up, the previous or next choice |
| Delete | remove it from the file: the default comes back |
| Tab | save to your settings.json or to this project's file |
| Ctrl-O | open that file at the setting |
| Esc | clear the search, then close |

The theme list previews each theme as you move; Esc puts the old one back. Changes apply at once and keep the file's comments. A file with unsaved edits open, or with errors, is left alone (the status line says why). `lsp.<language>` servers and `[language]` sections are added in the file itself.

## The key-binding editor

Every action with its keys and its [typed command](typed); search by action, command or key (`ctrl-s`). An orange dot marks keys you changed.

| Key | Does |
|---|---|
| Enter | press the new key: it replaces the action's keys |
| Ctrl-N | press a key to add |
| Delete | unbind the action |
| Ctrl-R | back to the default keys |
| Ctrl-O | open bindings.json |

A key that already does something asks first and says what it replaces. So does a key terminals may not send: Ctrl-Shift-letter needs the kitty keyboard protocol (kitty, Ghostty, WezTerm, foot), Ctrl-I is Tab, Ctrl-M is Enter, Ctrl-H is often Backspace.

## The menu bar

F10 opens the menus (File, Edit, Selection, View, Go, Code, Git, Terminal, Help); arrows move, Enter runs, a letter jumps, Esc closes. Every entry shows its key. `"menubar": true` keeps the bar above the tabs, where a click opens a menu. In a terminal tab F10 stays the program's (htop and mc quit with it).

## The files

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

## bindings.json

Micro's format, key name to action:

```json
{"Ctrl-j": "Save", "Alt-g": "command-edit:goto ", "Ctrl-5": "JumpToMatchingBrace", "F4": "None"}
```

- Key names: `Ctrl-x`, `Alt-x`, `Shift-Up`, `CtrlShiftRight`, `F5`, `PageDown`, `Backtab`.
- `"Copy|CopyLine"` runs the first that works; `"Escape,Deselect"` runs both; `"None"` unbinds the key.
- `"command-edit:TEXT"` opens the command line with TEXT.
- `:bindings` marks each key that replaces a default, and keys written twice (`Ctrl-q` and `CtrlQ` are one key).

`"keymap": "vscode"` or `"sublime"` puts those editors' keys over Micro's. VS Code: Ctrl-P quick open, Ctrl-Shift-P palette, Ctrl-G go to line, F12 definition, Shift-F12 references, F2 rename, F8 next problem, Ctrl-. code actions, Ctrl-` terminal, Ctrl-T symbols, Ctrl-W close, Ctrl-N new tab, Ctrl-Shift-I AI prompt. Sublime: the same palette and go-to keys plus Ctrl-B build task and Ctrl-Shift-Up/Down move lines. bindings.json still goes on top.

Every key, the [actions](actions) you can bind and the [commands](commands) have their own pages.
