# Files on disk

- **Saves are atomic**: a temp file and a rename, permissions and symlinks kept, so a crash or a full disk never leaves half a file.
- **Encodings** are kept as read: UTF-8, UTF-8 with BOM, UTF-16 LE/BE. Other bytes open read-only (RO) so a save cannot corrupt them.
- **Line endings** are kept; `:set eol=lf` or `:set eol=crlf` changes them at the next save.
- **Changed on disk**: clean files reload by themselves (Ctrl-Z undoes it). With unsaved edits Skink asks: (k)eep mine, (t)ake disk or (d)iff. Saving over a file changed since it was read asks first.
- **Backups**: unsaved edits are backed up after a 3 s pause. After a crash or a kill, the next Skink brings them back by itself as unsaved tabs and says so in a toast; nothing on disk is overwritten. `Ctrl-Z` shows what changed, `:discard` drops a file's unsaved changes. If the file changed on disk since, both are kept and you choose (keep mine, take disk, diff). A Skink still running keeps its own backups. `"restoreunsaved": false` asks per file instead ((r)ecover or (d)iscard).
- **Permission denied** on save: Skink offers sudo and runs `sudo tee` in a terminal tab, where sudo can ask for your password.
- **Large files** (8 MB or more) open as LARGE: no syntax colours, no language server (colours parse only the part on screen from 256 KB up, so 8 MB of code stays around 60 MB of memory). From 16 MB they load in the background: the tab says "Loading 120 MB..." and you can keep working in other tabs.
- **Very large files** (256 MB or more, logs and dumps of any size) open in the **large-file view**: read-only, straight from disk, so a 2 GB log takes a few MB of memory. Lines appear while the file is indexed (the status line shows how far). Scrolling, Go to line, Find and copying (up to 64 MB) work; Find looks at one line at a time and stops after a moment on a long search: search again to go on. A log that grows is indexed again. `SKINK_BG_LOAD_BYTES` and `SKINK_HUGE_BYTES` change the two limits.

Skink checks for changes when the terminal regains focus, on tab switches and while you work (at most every 2 s, never when idle).
