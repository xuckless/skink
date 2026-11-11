# Find and replace

Ctrl-F opens the find bar. It searches as you type, marks every match and shows "N of M".

| Key | Does |
|---|---|
| Enter | keep the match |
| Esc | back to where you were |
| Ctrl-N / Ctrl-P | next / previous match |
| Alt-R | regex `.*` |
| Alt-C | case-sensitive `Aa` (default: lowercase matches any case) |
| Alt-W | whole word `\b` |
| Alt-S | only inside the selection `[sel]` |
| Tab | to "Replace with"; Enter there replaces every match |
| Ctrl-L | go to line |

Replacing is one undo step. In regex mode `$1` and `${name}` insert capture groups. A selection over several lines starts the bar in `[sel]` mode.

From the command line:

```
:replace FIND WITH
:replace "two words" "other words"
```

Search across files is on [Search the project](search).
