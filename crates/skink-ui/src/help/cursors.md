# Multiple cursors

| Key | Does |
|---|---|
| Ctrl-D (or Alt-N) | select the word; again: add a cursor at the next match |
| Alt-X | skip this match |
| Alt-Shift-Up / Down (or Ctrl-Alt-Up / Down) | add a cursor above / below |
| Alt-click | add or remove a cursor |
| Esc (or Alt-C) | back to one cursor |

Typing, deleting, moving and indenting happen at every cursor, and one undo undoes them all. The status line shows ×N.

Copying with N cursors puts one line per cursor on the clipboard; pasting N lines into N cursors gives each its own line.

Actions, with Micro's names: SpawnMultiCursor, SpawnMultiCursorUp, SpawnMultiCursorDown, SkipMultiCursor, RemoveMultiCursor, RemoveAllMultiCursors.

See also: [Editing](editing)
