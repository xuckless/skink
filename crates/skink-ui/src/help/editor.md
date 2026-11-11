# Skink as your $EDITOR

```sh
git config --global core.editor skink
export EDITOR=skink VISUAL=skink
```

That covers commit messages and rebase lists, Ctrl-G in Claude Code and Codex, `crontab -e`, shell edit-command-line, `kubectl edit` and `gh`.

Files a tool hands its editor open **lean**: one buffer, no file tree, project scan, language server, git gutter, update check, session or recents. Skink picks it the way other editors do: git's own files by name (COMMIT_EDITMSG, MERGE_MSG, TAG_EDITMSG, SQUASH_MSG, git-rebase-todo, addp-hunk-edit.diff), and any file that belongs to no project. `:mode` says which and why.

| Key | Does |
|---|---|
| Ctrl-S | save |
| Ctrl-Q | done (asks if unsaved) |
| `:cq` | leave without saving and exit 1, so git aborts |
| Alt-Shift-K | in a rebase list: cycle pick, reword, edit, squash, fixup, drop (`:rebase VERB`) |
| Alt-Up / Alt-Down | reorder commits |

Commit messages get rulers at 50 and 72, dimmed `#` lines and a note when the subject passes 50 characters. Temp notes and prompts colour as Markdown and start with the cursor at the end.

`skink --scratch FILE` forces lean mode, `skink --project FILE` the full editor.

## From a Skink terminal

`skink FILE` in a Skink terminal tab opens a tab in that Skink instead of a second editor. Files a tool waits for (git's, an AI tool's) return when the tab closes: Ctrl-Q finishes (exit 0), `:cq` cancels (exit 1). Other files return at once. The shell gets SKINK_IPC and SKINK_IPC_TOKEN; `env -u SKINK_IPC skink FILE` edits inside the terminal instead.
