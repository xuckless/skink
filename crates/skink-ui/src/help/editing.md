# Editing

Keys follow Micro; [every key](keys) has a list.

| Key | Does |
|---|---|
| Shift + arrows, mouse drag | select |
| Ctrl-A | select all |
| Ctrl-C / Ctrl-X / Ctrl-V | copy / cut / paste (no selection: the whole line) |
| Ctrl-Z / Ctrl-Y | undo / redo |
| Alt-Up / Alt-Down | move lines |
| Ctrl-Shift-D | duplicate the line |
| Tab / Shift-Tab | indent / outdent |
| Alt-/ or Ctrl-/ | comment or uncomment lines |
| Ctrl-] | jump to the matching bracket |
| Alt-O / Alt-I | grow / shrink the selection by syntax |
| Ctrl-U | start / stop recording a macro (● REC in the status line) |
| Ctrl-J | replay the macro |

## Typing helps

- Brackets and quotes close themselves; typing the closer steps over it, and Backspace between an empty pair deletes both. With a selection they wrap it. Quotes don't pair inside words. Off with `"autoclose": false`.
- Enter after `(` `[` `{` indents and puts the closer on its own line.
- Tab inserts the file's own indentation (tabs or spaces, detected). `"tabstospaces": true` picks spaces for new files.
- The cursor moves over whole characters as you see them: emoji, accented letters, flags.

## Lines and case

`:JoinLines` `:SortLines` `:UpperCase` `:LowerCase` `:TitleCase` `:TrimTrailingWhitespace` work on the selection (case: the word at the cursor), at every cursor, as one undo step.

## Undo history

Undo survives closing a saved file and quitting, as long as the file didn't change on disk in between (2 MB per file, 32 MB in all, in the state folder).

## Scrolling

The wheel scrolls lines; Shift+wheel or a sideways swipe scrolls long lines until the cursor moves.

## Soft wrap

`:wrap` (or the ToggleSoftWrap action) wraps long lines at the window edge in this tab; `:set softwrap` / `:set nosoftwrap` say which. Continued rows show `↪` instead of a line number. Up, Down, Home and End move by screen rows (Home and End go to the row's start or end first, then the line's), and the wheel scrolls rows.

For every file: `"softwrap": true` in settings.json; for one language: `"[markdown]": {"softwrap": true}`. Lines break at spaces; `"wordwrap": false` breaks anywhere.

> [!NOTE]
> Without the kitty keyboard protocol some terminals send Ctrl-] as Ctrl-5 and Ctrl-Shift-D as Ctrl-D. Bind another key in bindings.json, for example `{"Ctrl-5": "JumpToMatchingBrace"}`, or use the [typed command](typed).

See also: [Multiple cursors](cursors) · [Find and replace](find)
