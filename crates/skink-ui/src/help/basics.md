# Opening, tabs and quitting

Everything opens as a tab: files, help, command output, git views, settings and terminals.

## Start Skink

| Command | Opens |
|---|---|
| `skink FILE[:LINE[:COL]]` | a file, at a position |
| `skink DIR` | a folder: file tree and an empty buffer |
| `skink` | inside a project: your last session; elsewhere a scratch buffer |

## Tabs

| Key | Does |
|---|---|
| Ctrl-O | open a file or folder by path |
| Alt-P | quick open (`:recent`) |
| Ctrl-T | new empty tab |
| Alt-1 … Alt-9 | tab 1 to 9 |
| Ctrl-PageUp / Ctrl-PageDown | previous / next tab (also Alt-, and Alt-.) |
| Ctrl-Q | close the tab; the last file tab quits |

Click a tab to select it, its × or a middle-click to close it.

## Sessions

`skink` with no arguments inside a project (a folder with .git, Cargo.toml, package.json, go.mod, pyproject.toml or build.zig, or below one) reopens the files, cursors, splits, file tree and shells it had when you quit there. Opening a file or folder starts fresh and leaves the saved session alone. Turn it off with `"restoresession": false`.

## Views

Help, command output and git views are read-only tabs; opening the same view again reuses its tab. `:run CMD` runs a command in the project root in the background and shows its output in a tab when it finishes.

Notes from background work (search done, session restored) show briefly in the top-right corner.

See also: [Terminals](terminal) · [File tree](tree) · [Files on disk](files)
