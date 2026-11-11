# Scripts (Lua)

`~/.config/skink/init.lua` runs at start-up. A project's `.skink/init.lua` runs only after you trust it, asked again whenever it changes.

```lua
skink.add_command("reviewed", function()
  skink.insert("// reviewed\n")
end)

skink.on("save", function(path)
  skink.message("saved " .. path)
end)
```

Lua 5.4 with table, string, math and utf8 (no coroutines: the time limit cannot reach them); no io or os: files and processes go through editor commands. Each call may run 250 ms, then it is stopped; scripts share 32 MB. Errors show as a toast, and all edits of one call undo as one step.

| Call | Does |
|---|---|
| `skink.on(event, fn)` | `"open"`, `"save"`, `"change"` (after a typing pause) get the path; `"key"` gets an unbound key like `"Ctrl-Alt-J"`, return true to take it |
| `skink.add_command(name, fn)` | `:name ARGS` calls `fn(ARGS...)` |
| `skink.text()` `skink.path()` `skink.lang()` `skink.selection()` | read the file |
| `skink.cursor()` / `skink.set_cursor(line [, col])` | 1-based |
| `skink.insert(text)` / `skink.set_text(text)` | edit at the cursors / replace all |
| `skink.action("Save")` `skink.command("goto 10")` | run an action or a command |
| `skink.message(text)` | a message; `print()` shows a toast |
| `skink.set_status(name, text)` | an item on the right of the status line (nil removes it) |
| `skink.pick(title, items, fn)` | a list; `fn(label, index)` on Enter |

More examples are in `docs/scripts/` in the repository.
