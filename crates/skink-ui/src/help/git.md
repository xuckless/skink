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
| `:diff` | this file side by side against the index; `:diff HEAD`, `:diff all`; `:diff OTHER` compares it with another file |
| `:blame` | the file's blame in a tab; Enter shows the commit |
| `:log [PATH]` | the commit list with the selected commit's diff beside it (Up/Down follow); Enter opens it, c reviews it |
| `:codereview` | review a diff with comments, see below |
| `:branches` `:branch NAME` `:switch NAME` | list (Enter switches, D deletes), create, switch |
| `:stash [MSG]` `:stashes` | stash; list (Enter apply, p pop, D drop) |
| `:push` `:pull` `:fetch [ARGS]` | in a terminal tab |

## Code review

`:codereview` reviews the uncommitted changes; `:codereview staged`, `:codereview REV` (one commit), `:codereview branch [BASE]` (this branch against main), `:codereview since REV` (everything changed since a commit: what an agent did).

On a diff line, c writes a comment; on a comment, e edits it (emptied: deleted), x resolves or reopens it, D deletes it. ] / [ go to the next / previous comment, Enter opens the file at that line, g refreshes. Comments follow their line when the code moves; those whose line is gone are listed at the end as outdated.

Comments are kept in `.git/skink-review/` (never in the work tree), per repository and per target, so they survive restarts. m writes the review as Markdown next to them and opens it; a sends the open comments to the agent panel to address.

## Comparing two files

`skink --diff A B` (or `:diff OTHER` from a file) shows two files side by side. n / p next / previous change, `>` copies the change under the cursor to the right file, `<` to the left one, Enter opens the right file at that line to edit (o the left one), s saves both, r re-reads them. Copies are unsaved edits in the file's tab (Ctrl-Z there undoes one); edits made in those tabs show when you come back.

As git's difftool:

```sh
git config --global diff.tool skink
git config --global difftool.skink.cmd 'skink --diff "$LOCAL" "$REMOTE"'
git difftool
```

Folders (`git difftool --dir-diff`) are not supported yet.

## Merge conflicts

`:conflicts` opens every conflicted file at its first marker. Ours is tinted green, theirs blue, and each block's first line says "Accept ours | theirs | both": click one, or use `:ours` `:theirs` `:both`. `:nextconflict` / `:prevconflict` move between blocks.

## In the file tree

M modified (yellow), A added (green), U untracked (teal), ! conflicted (red), ignored grey. Folders take their worst file's colour and show a dot. The status line shows [+] while a file has unsaved changes.

Settings: `"gitgutter": true`, `"inlineblame": true`.
