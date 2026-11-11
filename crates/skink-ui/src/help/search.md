# Search the project

Alt-S (or `:grep TEXT`) searches every file in the project. Results stream into a tab grouped by file; Enter jumps to one.

> [!NOTE]
> Ctrl-Shift-F does the same only in terminals that tell it apart from Ctrl-F (kitty, Ghostty, WezTerm, foot). Alt-S works everywhere.

- Alt-R regex, Alt-C case, Alt-W whole word, as in the [find bar](find).
- .gitignore and .ignore are honoured; it stops at 5000 matches. `:stopsearch` stops it early.
- `:greprep WITH` replaces the last search's matches in every file, through the refactor preview (regex mode expands `$1`).

## Structural search

Search code by shape (ast-grep), in files of the current file's language. `$NAME` matches one node, `$$$NAME` any number; whitespace and comments don't matter.

```
:sgrep foo($A, $$$REST)
:srep foo($A, $B) => bar($B, $A)
```

`:srep` rewrites every match through the preview. After `:sgrep`, `:greprep WITH` rewrites the searched pattern.

See also: [Code intelligence](code)
