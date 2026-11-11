# Git

Marks next to the line numbers compare the file with the index: a thin green bar added, a thick blue bar changed, a red mark deleted. Dotted bars are already staged. The scrollbar shows them too.

| Key | Does |
|---|---|
| Alt-Shift-N / Alt-Shift-P | next / previous change (`:nexthunk`, `:prevhunk`) |
| Alt-Shift-D | peek at the change: s stage, u unstage, r revert (or click the mark) |
| Alt-Shift-S | stage the change (`:stagehunk`, `:unstagehunk`, `:reverthunk`) |
| Alt-Shift-B | the faded blame on the cursor line (`:inlineblame`) |
| Alt-Shift-C | the git panel (`:gitstatus`) |

Reverting a change is a normal edit: Ctrl-Z undoes it.

## The git panel

Enter open · s / u stage / unstage · S / U all · d discard (asks; untracked files go to the trash) · c commit · C amend · b branches · z stash · Z stashes · g refresh · P push · p pull · f fetch.

Push, pull and fetch run in a terminal tab, so git asks for passwords and keys itself.

## Commands

| Command | Does |
|---|---|
| `:commit [MSG]` | commit, or open a message tab: saving it commits (`:amend`, `:signoff` toggle those) |
| `:stage` / `:unstage [PATH]` | this file or PATH |
| `:diff` | this file side by side against the index; `:diff HEAD`, `:diff all` |
| `:blame` | the file's blame in a tab; Enter shows the commit |
| `:branches` `:branch NAME` `:switch NAME` | list (Enter switches, D deletes), create, switch |
| `:stash [MSG]` `:stashes` | stash; list (Enter apply, p pop, D drop) |
| `:push` `:pull` `:fetch [ARGS]` | in a terminal tab |

## Merge conflicts

`:conflicts` opens every conflicted file at its first marker. Ours is tinted green, theirs blue, and each block's first line says "Accept ours | theirs | both": click one, or use `:ours` `:theirs` `:both`. `:nextconflict` / `:prevconflict` move between blocks.

## In the file tree

M modified (yellow), A added (green), U untracked (teal), ! conflicted (red), ignored grey. Folders take their worst file's colour and show a dot. The status line shows [+] while a file has unsaved changes.

Settings: `"gitgutter": true`, `"inlineblame": true`.
