# Moving around

| Key | Does |
|---|---|
| Ctrl-L | go to line (`:40`, `:goto 40`) |
| Alt-U | back to where you were before a jump (`:back`) |
| Alt-Shift-U | forward again (`:forward`) |
| Alt-M | fold or unfold the block at the cursor (`:fold`) |
| Alt-Shift-M | open every fold; `:foldall` closes every top-level block |
| Ctrl-R | symbols in this file |

Jumps are definitions, results, quick open and `:goto`.

## Folding

Folds use the language server's ranges when it offers them (`#region`, imports, comment blocks) and indentation everywhere else. A closed fold shows a mark in the gutter (click it to open) and "N lines" after its first line. The cursor steps over it, and jumping into it opens it.

## Sticky scroll

The headers of the blocks you are scrolled into stay at the top; click one to go there. Off with `"stickyscroll": false`.

## Rulers and marks

- `"rulers": [80, 100]` in settings.json draws rulers (Micro's `"colorcolumn"` works too).
- The last column marks errors (red), warnings (yellow), find matches (orange) and the cursor (blue) across the whole file.
- A thin thumb on the bottom row appears when lines are wider than the view.
