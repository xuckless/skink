# Files on disk

- **Saves are atomic**: a temp file and a rename, permissions and symlinks kept, so a crash or a full disk never leaves half a file.
- **Encodings** are kept as read: UTF-8, UTF-8 with BOM, UTF-16 LE/BE. Other bytes open read-only (RO) so a save cannot corrupt them.
- **Line endings** are kept; `:set eol=lf` or `:set eol=crlf` changes them at the next save.
- **Changed on disk**: clean files reload by themselves (Ctrl-Z undoes it). With unsaved edits Skink asks: (k)eep mine, (t)ake disk or (d)iff. Saving over a file changed since it was read asks first.
- **Backups**: unsaved edits are backed up after a 3 s pause. After a crash, opening the file offers (r)ecover or (d)iscard.
- **Permission denied** on save: Skink offers sudo and runs `sudo tee` in a terminal tab, where sudo can ask for your password.
- **Large files** (2 MB or more) open as LARGE: no syntax colours, no language server.

Skink checks for changes when the terminal regains focus, on tab switches and while you work (at most every 2 s, never when idle).
