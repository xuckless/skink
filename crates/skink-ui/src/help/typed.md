# Typed commands for every key

A terminal, the OS or a keyboard layout can swallow any key (macOS Option, the Windows console, tmux). So every key also has a command: press Ctrl-E and type it. Tab completes, Up/Down recall history.

| Area | Commands |
|---|---|
| Editing | `:comment` `:duplicate` `:moveup` `:movedown` `:indent` `:outdent` `:bracket` `:expand` `:contract` |
| Cursors | `:addcursor` `:cursorabove` `:cursorbelow` `:onecursor` |
| Find | `:find` `:findnext` `:findprev` |
| Code | `:palette` `:hover` `:peek` `:def` `:refs` `:typedef` `:fix` `:allsymbols` `:nextproblem` `:prevproblem` |
| Tabs and panes | `:nexttab` `:prevtab` `:newtab` `:nextsplit` `:prevsplit` `:files` `:sidebar` `:dotfiles` `:zoom` `:fold` |
| Terminals | `:newterm` `:toggleterm` `:rerun` `:nextprompt` `:prevprompt` `:lastoutput` |
| AI | `:ask` `:senderrors` |
| Macros | `:record` `:replay` |
| Git | `:nexthunk` `:blame` `:gitstatus` |
| Debugging | `:continue` `:step` `:stepin` `:stepout` `:pause` `:condition` |

Any action also runs by its own name, in any spelling: `:ToggleComment`, `:toggle-comment` and `:togglecomment` are the same. F1 shows each action's key and command, and [every key](keys) lists the command beside it.
