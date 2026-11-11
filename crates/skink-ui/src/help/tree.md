# File tree

F8 shows the tree, moves the keys into it, and back. Ctrl-B hides or shows it.

When the tree has the keys its title says "(focused)", its border and selection use the accent colour and the status line says TREE. Click the editor or a tab to hand the keys back.

| Key | Does |
|---|---|
| Up / Down, PageUp / PageDown, Home / End | move |
| Enter / Right | open or expand |
| Left | collapse, or go to the parent |
| a | new file in the selected folder (`src/x/y.rs` makes the folders) |
| A | new folder |
| r / F2 | rename or move (open tabs follow) |
| d / Delete | move to the trash (asks first) |
| y / Y | copy the absolute / relative path |
| / | filter files as you type; Esc clears |
| . or Alt-H | show or hide dotfiles (Alt-H works anywhere) |
| - / * | collapse all / expand recursively |
| < / > | narrower / wider (or drag the border) |
| q / Esc, h | back to the editor, hide |

Commands: `:new PATH` `:mkdir PATH` `:tree collapse|expand|hide|hidden` `:hidden`.

Renaming asks running language servers first, and the imports they would change open in the refactor preview.

Git-ignored entries are dimmed and never auto-expanded. The tree follows the active file, remembers open folders per project and hides itself in windows narrower than 50 columns. Names are coloured by [git](git) status.

## More folders

`:addroot DIR` adds another project folder below the main one; quick open and project search cover it, and the session remembers it. `:removeroot [DIR]` takes it away.
