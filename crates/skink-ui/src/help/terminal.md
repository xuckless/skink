# Terminals

Terminal tabs are real terminal emulators (colours, mouse, full-screen programs), so any CLI runs unchanged, AI ones included: `:term claude`.

| Key | Does |
|---|---|
| F12 | switch between the file and your terminal tab |
| Shift-F12 | another terminal tab |
| F11 | close the terminal tab |
| Alt-T | a shell docked along the bottom |
| Shift-F11 | the terminal's history in a tab (`:scrollback`) |
| Shift-F2 / Shift-F3 | previous / next shell prompt |
| Shift-F4 | select the last command's output (`:scrollback last`) |
| Ctrl-click | open a `file:line[:col]` |

In a terminal tab every key goes to the program, except Alt-1…9 and the keys bound to tab actions (ToggleTerminal, NewTerminal, CloseTerminal, ToggleTree, ToggleSidebar, PreviousTab, NextTab), which bindings.json can change. A finished terminal closes on any key. Tabs show the title the program sets.

In the history tab Ctrl-F searches, Shift+arrows select, Ctrl-C copies and Enter on a `file:line` opens it.

## Shell integration

Skink adds prompt marks (OSC 133) to the bash, zsh, fish and PowerShell it starts. Your own rc files (oh-my-zsh, starship, …) load first and unchanged, and are never edited. The marks power prompt jumps, output selection and exact output for Alt-Shift-E. Off with `"terminal.shellintegration": false`; without marks Alt-Shift-E guesses the last command from the screen.

## Inside EveryAI

Skink finds the host through SKINK_HOST_URL and SKINK_HOST_TOKEN and hands language servers and agents to it; the status line says "EveryAI". The prompt box and agent panel then use EveryAI's models.

See also: [Tasks](tasks) · [Skink as your $EDITOR](editor)
