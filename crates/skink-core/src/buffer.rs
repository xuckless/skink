use ropey::Rope;
use std::{
    fs,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    /// Both CRLF and lone LF in one file: the text keeps every `\r` as read
    /// and a save writes the bytes back unchanged (v1.0 finding: a mixed
    /// file whose first line was CRLF had all its LF lines rewritten).
    /// `:set eol=lf|crlf` normalizes on the next save.
    Mixed,
}

impl LineEnding {
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
            LineEnding::Mixed => "MIXED",
        }
    }
}

/// How the file's bytes map to text. Kept so a save writes back what was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    /// UTF-8 with a byte-order mark (kept on save).
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

impl Encoding {
    /// Status-line label; plain UTF-8 needs none.
    pub fn label(self) -> Option<&'static str> {
        match self {
            Encoding::Utf8 => None,
            Encoding::Utf8Bom => Some("UTF-8 BOM"),
            Encoding::Utf16Le => Some("UTF-16LE"),
            Encoding::Utf16Be => Some("UTF-16BE"),
        }
    }

    fn bom(self) -> &'static [u8] {
        match self {
            Encoding::Utf8 => b"",
            Encoding::Utf8Bom => b"\xEF\xBB\xBF",
            Encoding::Utf16Le => b"\xFF\xFE",
            Encoding::Utf16Be => b"\xFE\xFF",
        }
    }

    fn write(self, w: &mut impl Write, s: &str) -> io::Result<()> {
        match self {
            Encoding::Utf8 | Encoding::Utf8Bom => w.write_all(s.as_bytes()),
            Encoding::Utf16Le => s
                .encode_utf16()
                .try_for_each(|u| w.write_all(&u.to_le_bytes())),
            Encoding::Utf16Be => s
                .encode_utf16()
                .try_for_each(|u| w.write_all(&u.to_be_bytes())),
        }
    }
}

/// Whether the buffer may be edited, and if not, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Writable,
    ReadOnly(ReadOnly),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnly {
    /// Help, command output, lists: not a file.
    View,
    /// A directory listing.
    Directory,
    /// The bytes are not valid UTF-8/UTF-16; saving would corrupt them.
    Undecodable,
    /// Too big to load: shown straight from disk (see [`Buffer::open_disk`]).
    Huge,
    /// Still being read (big files load in the background).
    Loading,
}

impl ReadOnly {
    pub fn reason(self) -> &'static str {
        match self {
            ReadOnly::View | ReadOnly::Directory => "Buffer is read-only",
            ReadOnly::Undecodable => "Read-only: the file is not valid UTF-8 or UTF-16",
            ReadOnly::Huge => "Read-only: a very large file, shown in the large-file view",
            ReadOnly::Loading => "Still loading the file",
        }
    }
}

/// What the file on disk looked like when it was last read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskStamp {
    pub modified: SystemTime,
    pub len: u64,
}

impl DiskStamp {
    fn of(path: &Path) -> Option<Self> {
        let m = fs::metadata(path).ok()?;
        Some(DiskStamp {
            modified: m.modified().ok()?,
            len: m.len(),
        })
    }
}

/// The file on disk compared with the last read/write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskChange {
    Unchanged,
    Modified,
    Deleted,
}

/// A (row, byte column) position: what incremental parsers want.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextPoint {
    pub row: usize,
    /// Bytes from the start of the line.
    pub col: usize,
}

/// One edit as byte offsets and points, against the text before (`old_*`)
/// and after (`new_*`) it. Consecutive edits chain: each is relative to the
/// text the previous one produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start: TextPoint,
    pub old_end: TextPoint,
    pub new_end: TextPoint,
}

/// The most recent edits, so consumers (the syntax tree) can catch up from
/// an older revision without re-reading everything. Invariant:
/// `base + edits.len() == revision`.
#[derive(Debug, Default)]
struct Journal {
    /// Revision before the oldest kept edit.
    base: u64,
    edits: std::collections::VecDeque<ByteEdit>,
}

/// Edits kept; a consumer further behind re-reads the whole text.
const JOURNAL_MAX: usize = 4096;

impl Journal {
    fn push(&mut self, e: ByteEdit) {
        if self.edits.len() == JOURNAL_MAX {
            self.edits.pop_front();
            self.base += 1;
        }
        self.edits.push_back(e);
    }
}

/// Where a buffer's text lives.
#[derive(Debug)]
enum Store {
    /// In memory, editable.
    Rope(Rope),
    /// A very large file read from disk on demand; read-only.
    Disk(std::sync::Arc<crate::disktext::DiskText>),
}

/// The most a slice of a disk-backed buffer reads (copying more is refused
/// by the editor, see [`Buffer::slice_limit`]).
pub const DISK_SLICE_CAP: usize = 64 << 20;

/// Text storage. A rope keeps edits and memory use sane for large files.
///
/// Text is held with `\n` line endings only; a CRLF file is detected on load
/// and converted back on save, so the rest of the editor never sees `\r`.
#[derive(Debug)]
pub struct Buffer {
    store: Store,
    path: Option<PathBuf>,
    line_ending: LineEnding,
    encoding: Encoding,
    access: Access,
    dirty: bool,
    /// The text as last saved or loaded (a cheap rope clone that shares
    /// unchanged chunks): undo back to it makes the buffer clean again.
    saved: (Rope, LineEnding),
    revision: u64,
    journal: Journal,
    /// Last known state of the file (None: no file, or not read yet).
    disk: Option<DiskStamp>,
    /// Display name for buffers that are not files (help, command output).
    title: Option<String>,
}

impl Buffer {
    pub fn empty() -> Self {
        Self::from_text("")
    }

    pub fn from_text(text: &str) -> Self {
        let line_ending = detect_line_ending(text);
        let rope = match line_ending {
            LineEnding::CrLf => Rope::from_str(&text.replace("\r\n", "\n")),
            LineEnding::Lf | LineEnding::Mixed => Rope::from_str(text),
        };
        Self {
            saved: (rope.clone(), line_ending),
            store: Store::Rope(rope),
            path: None,
            line_ending,
            encoding: Encoding::Utf8,
            access: Access::Writable,
            dirty: false,
            revision: 0,
            journal: Journal::default(),
            disk: None,
            title: None,
        }
    }

    /// Open a file, or, for a directory, a read-only listing.
    pub fn open(path: &Path) -> io::Result<Self> {
        if fs::metadata(path)?.is_dir() {
            let mut names: Vec<String> = fs::read_dir(path)?
                .filter_map(Result::ok)
                .map(|e| {
                    let mut n = e.file_name().to_string_lossy().into_owned();
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        n.push('/');
                    }
                    n
                })
                .collect();
            names.sort_by_key(|n| (!n.ends_with('/'), n.to_lowercase()));
            let mut b = Self::from_text(&names.join("\n"));
            b.path = Some(path.to_path_buf());
            b.access = Access::ReadOnly(ReadOnly::Directory);
            return Ok(b);
        }
        let stamp = DiskStamp::of(path);
        let (text, encoding, ok) = decode(&fs::read(path)?);
        let mut b = Self::from_text(&text);
        b.encoding = encoding;
        if !ok {
            b.access = Access::ReadOnly(ReadOnly::Undecodable);
        }
        b.path = Some(path.to_path_buf());
        b.disk = stamp;
        Ok(b)
    }

    /// Open a very large file read-only, straight from disk: its lines are
    /// indexed in the background and read on demand, so memory stays small
    /// whatever the size. Lines appear as the index grows.
    pub fn open_disk(path: &Path) -> io::Result<Self> {
        let disk = crate::disktext::DiskText::open(path)?;
        let mut b = Self::empty();
        b.store = Store::Disk(std::sync::Arc::new(disk));
        b.access = Access::ReadOnly(ReadOnly::Huge);
        b.path = Some(path.to_path_buf());
        b.disk = DiskStamp::of(path);
        Ok(b)
    }

    /// An empty read-only stand-in for `path` while it loads.
    pub fn loading(path: &Path) -> Self {
        let mut b = Self::empty();
        b.access = Access::ReadOnly(ReadOnly::Loading);
        b.path = Some(path.to_path_buf());
        b
    }

    /// The disk store of a buffer opened with [`open_disk`](Self::open_disk).
    pub fn disk_text(&self) -> Option<&crate::disktext::DiskText> {
        match &self.store {
            Store::Rope(_) => None,
            Store::Disk(d) => Some(d),
        }
    }

    /// Longest slice (bytes) this buffer hands out; `None`: no limit.
    pub fn slice_limit(&self) -> Option<usize> {
        match self.store {
            Store::Rope(_) => None,
            Store::Disk(_) => Some(DISK_SLICE_CAP),
        }
    }

    /// A buffer for a path that does not exist yet: empty, remembers the
    /// path so the first save creates the file.
    pub fn new_file(path: &Path) -> Self {
        let mut b = Self::empty();
        b.path = Some(path.to_path_buf());
        b
    }

    /// A read-only buffer that is not a file: help, command output, lists.
    /// Anything Skink shows opens as one of these, so it gets a tab, search,
    /// copy and cursor movement like any file.
    pub fn view(title: &str, text: &str) -> Self {
        let mut b = Self::from_text(text);
        b.title = Some(title.to_owned());
        b.access = Access::ReadOnly(ReadOnly::View);
        b
    }

    /// Name for tab bars: the title of a virtual buffer, else the file name.
    pub fn title(&self) -> Option<String> {
        self.title.clone().or_else(|| {
            self.path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn set_line_ending(&mut self, le: LineEnding) {
        if self.line_ending != le {
            self.line_ending = le;
            self.dirty = true;
        }
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub fn access(&self) -> Access {
        self.access
    }

    pub fn is_read_only(&self) -> bool {
        matches!(self.access, Access::ReadOnly(_))
    }

    /// How the file on disk differs from what this buffer last read or wrote.
    /// `None` for buffers that are not (yet) files.
    pub fn disk_change(&self) -> Option<DiskChange> {
        let (path, known) = (self.path.as_deref()?, self.disk?);
        Some(match DiskStamp::of(path) {
            None => DiskChange::Deleted,
            Some(now) if now == known => DiskChange::Unchanged,
            Some(_) => DiskChange::Modified,
        })
    }

    /// Accept the current disk state as known (after "keep mine").
    pub fn acknowledge_disk(&mut self) {
        self.disk = self.path.as_deref().and_then(DiskStamp::of);
    }

    /// Read the file again. Returns the decoded text; the caller applies it
    /// through the editor so the reload is one undoable edit.
    pub fn read_disk(&self) -> io::Result<String> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| io::Error::other("no file name"))?;
        let (text, _, _) = decode(&fs::read(path)?);
        Ok(if detect_line_ending(&text) == LineEnding::CrLf {
            text.replace("\r\n", "\n")
        } else {
            text
        })
    }

    /// The text now matches the file (reloaded, or written by someone else
    /// such as `sudo tee`): clean and in sync.
    pub fn mark_synced(&mut self) {
        self.dirty = false;
        if let Store::Rope(rope) = &self.store {
            self.saved = (rope.clone(), self.line_ending);
        }
        self.acknowledge_disk();
    }

    /// The text as last saved or loaded (what the file held then).
    pub fn saved_text(&self) -> String {
        self.saved.0.to_string()
    }

    /// After undo/redo: back at the saved text means not modified.
    pub fn recheck_clean(&mut self) {
        let Store::Rope(rope) = &self.store else {
            return;
        };
        if self.dirty
            && self.line_ending == self.saved.1
            && rope.len_bytes() == self.saved.0.len_bytes()
            && *rope == self.saved.0
        {
            self.dirty = false;
        }
    }

    /// The bytes a save would write (encoding, BOM and line endings applied).
    pub fn encoded(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len_bytes() + 4);
        let _ = self.write_to(&mut out);
        out
    }

    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        let rope = match &self.store {
            Store::Rope(r) => r,
            Store::Disk(d) => return w.write_all(d.read_all().as_bytes()),
        };
        w.write_all(self.encoding.bom())?;
        let newline = match self.line_ending {
            LineEnding::Mixed => {
                for chunk in rope.chunks() {
                    self.encoding.write(w, chunk)?;
                }
                return Ok(());
            }
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        };
        // `\r\n` in the text (a mixed file converted with `:set eol=...`)
        // becomes one newline; a `\r` ending a chunk waits for the next one.
        let mut carry = String::new();
        for chunk in rope.chunks() {
            carry.push_str(chunk);
            let keep = usize::from(carry.ends_with('\r'));
            let body = &carry[..carry.len() - keep];
            if body.contains('\n') {
                let out = body.replace("\r\n", "\n");
                if newline == "\n" {
                    self.encoding.write(w, &out)?;
                } else {
                    self.encoding.write(w, &out.replace('\n', newline))?;
                }
            } else {
                self.encoding.write(w, body)?;
            }
            carry.drain(..carry.len() - keep);
        }
        self.encoding.write(w, &carry)?;
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Bumped on every text change; lets observers (LSP sync) skip no-ops.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Write to the buffer's path, restoring CRLF, BOM and encoding.
    /// Atomic where possible: a temp file next to the target, then a rename,
    /// so a crash or full disk never leaves a half-written file.
    pub fn save(&mut self) -> io::Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| io::Error::other("no file name"))?;
        if let Access::ReadOnly(why) = self.access {
            return Err(io::Error::other(why.reason()));
        }
        // Its folder was deleted under it (or `skink new/dir/file`):
        // recreate it, as the "deleted on disk" note promises.
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
            && !dir.exists()
        {
            fs::create_dir_all(dir)?;
        }
        write_file(&path, |w| self.write_to(w))?;
        self.dirty = false;
        if let Store::Rope(rope) = &self.store {
            self.saved = (rope.clone(), self.line_ending);
        }
        self.acknowledge_disk();
        Ok(())
    }

    /// Number of lines; a trailing newline yields a final empty line.
    pub fn len_lines(&self) -> usize {
        match &self.store {
            Store::Rope(r) => r.len_lines(),
            Store::Disk(d) => d.len_lines(),
        }
    }

    pub fn len_bytes(&self) -> usize {
        match &self.store {
            Store::Rope(r) => r.len_bytes(),
            Store::Disk(d) => d.len_bytes(),
        }
    }

    pub fn len_chars(&self) -> usize {
        match &self.store {
            Store::Rope(r) => r.len_chars(),
            Store::Disk(d) => d.len_chars(),
        }
    }

    pub fn line_to_char(&self, line: usize) -> usize {
        let line = line.min(self.len_lines() - 1);
        match &self.store {
            Store::Rope(r) => r.line_to_char(line),
            Store::Disk(d) => d.line_to_char(line),
        }
    }

    pub fn char_to_line(&self, idx: usize) -> usize {
        let idx = idx.min(self.len_chars());
        match &self.store {
            Store::Rope(r) => r.char_to_line(idx),
            Store::Disk(d) => d.char_to_line(idx),
        }
    }

    pub fn char_to_byte(&self, idx: usize) -> usize {
        let idx = idx.min(self.len_chars());
        match &self.store {
            Store::Rope(r) => r.char_to_byte(idx),
            Store::Disk(d) => d.char_to_byte(idx),
        }
    }

    pub fn byte_to_char(&self, byte: usize) -> usize {
        match &self.store {
            Store::Rope(r) => r.byte_to_char(byte.min(r.len_bytes())),
            Store::Disk(d) => d.byte_to_char(byte),
        }
    }

    pub fn char_at(&self, idx: usize) -> Option<char> {
        match &self.store {
            Store::Rope(r) => (idx < r.len_chars()).then(|| r.char(idx)),
            Store::Disk(d) => d.char_at(idx),
        }
    }

    /// Text in `[start, end)` chars (a disk-backed buffer reads at most
    /// [`DISK_SLICE_CAP`] bytes).
    pub fn slice(&self, start: usize, end: usize) -> String {
        let end = end.min(self.len_chars());
        match &self.store {
            Store::Rope(r) => r.slice(start.min(end)..end).to_string(),
            Store::Disk(d) => d.slice(start, end, DISK_SLICE_CAP),
        }
    }

    /// Whole buffer text (for LSP full sync, agents). A disk-backed buffer
    /// reads the whole file: only for explicit user actions.
    pub fn text(&self) -> String {
        match &self.store {
            Store::Rope(r) => r.to_string(),
            Store::Disk(d) => d.read_all(),
        }
    }

    /// Insert at a char index. Low level: history is the editor's job.
    pub fn insert(&mut self, at: usize, text: &str) {
        if matches!(self.store, Store::Disk(_)) {
            return;
        }
        let at = at.min(self.len_chars());
        let (start_byte, start) = self.point_at(at);
        if let Store::Rope(rope) = &mut self.store {
            rope.insert(at, text);
        }
        let new_end = match text.rfind('\n') {
            Some(i) => TextPoint {
                row: start.row + text.matches('\n').count(),
                col: text.len() - i - 1,
            },
            None => TextPoint {
                row: start.row,
                col: start.col + text.len(),
            },
        };
        self.journal.push(ByteEdit {
            start_byte,
            old_end_byte: start_byte,
            new_end_byte: start_byte + text.len(),
            start,
            old_end: start,
            new_end,
        });
        self.dirty = true;
        self.revision += 1;
    }

    /// Remove `[start, end)` chars.
    pub fn remove(&mut self, start: usize, end: usize) {
        let end = end.min(self.len_chars());
        if start < end
            && let Store::Rope(_) = self.store
        {
            let (start_byte, sp) = self.point_at(start);
            let (old_end_byte, ep) = self.point_at(end);
            if let Store::Rope(rope) = &mut self.store {
                rope.remove(start..end);
            }
            self.journal.push(ByteEdit {
                start_byte,
                old_end_byte,
                new_end_byte: start_byte,
                start: sp,
                old_end: ep,
                new_end: sp,
            });
            self.dirty = true;
            self.revision += 1;
        }
    }

    fn point_at(&self, char_idx: usize) -> (usize, TextPoint) {
        let byte = self.char_to_byte(char_idx);
        let row = self.char_to_line(char_idx);
        let col = byte - self.char_to_byte(self.line_to_char(row));
        (byte, TextPoint { row, col })
    }

    /// The edits that turn revision `rev` into the current text, oldest
    /// first; `None` when `rev` is too old (or not from this buffer).
    pub fn edits_since(&self, rev: u64) -> Option<Vec<ByteEdit>> {
        if rev > self.revision || rev < self.journal.base {
            return None;
        }
        let skip = (rev - self.journal.base) as usize;
        Some(self.journal.edits.iter().skip(skip).copied().collect())
    }

    /// Line text without its line terminator.
    pub fn line_text(&self, idx: usize) -> String {
        if idx >= self.len_lines() {
            return String::new();
        }
        let rope = match &self.store {
            Store::Rope(r) => r,
            Store::Disk(d) => return d.line_text(idx),
        };
        let mut s = rope.line(idx).to_string();
        while s.ends_with(['\n', '\r']) {
            s.pop();
        }
        s
    }

    /// Whether line `idx` starts with `prefix`, without allocating the line.
    pub fn line_starts_with(&self, idx: usize, prefix: &str) -> bool {
        if idx >= self.len_lines() {
            return prefix.is_empty();
        }
        let rope = match &self.store {
            Store::Rope(r) => r,
            Store::Disk(d) => return d.line_text(idx).starts_with(prefix),
        };
        let mut line = rope.line(idx).chars();
        prefix.chars().all(|p| line.next() == Some(p))
    }

    /// Length of a line in chars, excluding its terminator.
    pub fn line_len(&self, idx: usize) -> usize {
        if idx >= self.len_lines() {
            return 0;
        }
        let rope = match &self.store {
            Store::Rope(r) => r,
            Store::Disk(d) => return d.line_len(idx),
        };
        let line = rope.line(idx);
        let mut n = line.len_chars();
        while n > 0 && matches!(line.char(n - 1), '\n' | '\r') {
            n -= 1;
        }
        n
    }
}

/// Decode file bytes: BOM-marked UTF-8/UTF-16, else UTF-8. The flag is false
/// when bytes were not valid (replaced with U+FFFD).
fn decode(bytes: &[u8]) -> (String, Encoding, bool) {
    fn utf16(b: &[u8], le: bool) -> (String, bool) {
        let units = b.chunks(2).map(|c| match (c, le) {
            ([a, b], true) => u16::from_le_bytes([*a, *b]),
            ([a, b], false) => u16::from_be_bytes([*a, *b]),
            _ => 0xFFFD,
        });
        let mut ok = b.len().is_multiple_of(2);
        let s = char::decode_utf16(units)
            .map(|r| {
                r.unwrap_or_else(|_| {
                    ok = false;
                    char::REPLACEMENT_CHARACTER
                })
            })
            .collect();
        (s, ok)
    }
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        let (s, ok) = match std::str::from_utf8(rest) {
            Ok(s) => (s.to_owned(), true),
            Err(_) => (String::from_utf8_lossy(rest).into_owned(), false),
        };
        return (s, Encoding::Utf8Bom, ok);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        let (s, ok) = utf16(rest, true);
        return (s, Encoding::Utf16Le, ok);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        let (s, ok) = utf16(rest, false);
        return (s, Encoding::Utf16Be, ok);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_owned(), Encoding::Utf8, true),
        Err(_) => (
            String::from_utf8_lossy(bytes).into_owned(),
            Encoding::Utf8,
            false,
        ),
    }
}

/// Write a file through `fill`. Prefers temp file + rename in the same
/// directory (keeping permissions, and owner on Unix, of the old file).
/// Writes in place when that is not possible or would break something:
/// no permission to create files in the directory, or a hard-linked file.
/// Symlinks are followed so the link stays a link.
pub fn write_file(
    path: &Path,
    fill: impl FnOnce(&mut BufWriter<fs::File>) -> io::Result<()>,
) -> io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let old = fs::metadata(&target).ok();
    #[cfg(unix)]
    let hard_linked = {
        use std::os::unix::fs::MetadataExt;
        old.as_ref().is_some_and(|m| m.nlink() > 1)
    };
    #[cfg(not(unix))]
    let hard_linked = false;
    if let Some(m) = &old
        && m.permissions().readonly()
    {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.skink-{}~", std::process::id()));
    let temp = match hard_linked {
        true => None,
        // `None` when the directory is not writable (or a stale temp exists).
        false => fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .ok(),
    };
    let Some(file) = temp else {
        // In place: keeps hard links, works in read-only directories.
        let mut w = BufWriter::new(fs::File::create(&target)?);
        fill(&mut w)?;
        return w.into_inner().map_err(|e| e.into_error())?.sync_all();
    };
    let result = (|| {
        let mut w = BufWriter::new(file);
        fill(&mut w)?;
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        if let Some(m) = &old {
            fs::set_permissions(&tmp, m.permissions())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                // Best effort: only root may give a file away.
                let _ = std::os::unix::fs::chown(&tmp, Some(m.uid()), Some(m.gid()));
            }
        }
        fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// CRLF only when every line break is CRLF; both kinds is `Mixed`.
fn detect_line_ending(text: &str) -> LineEnding {
    let b = text.as_bytes();
    let (mut crlf, mut lf) = (0usize, 0usize);
    for (i, _) in text.match_indices('\n') {
        if i > 0 && b[i - 1] == b'\r' {
            crlf += 1;
        } else {
            lf += 1;
        }
        if crlf > 0 && lf > 0 {
            return LineEnding::Mixed;
        }
    }
    if crlf > 0 {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_line_endings_save_byte_for_byte_and_convert_cleanly() {
        // v1.0 finding: CRLF first, then LF lines, came back all CRLF.
        let raw = "a\r\nb\nc\rd\r\n\n\r\re\n";
        let mut b = Buffer::from_text(raw);
        assert_eq!(b.line_ending(), LineEnding::Mixed);
        assert_eq!(b.encoded(), raw.as_bytes());
        // Long enough that `\r\n` pairs straddle rope chunks.
        let big: String = (0..20_000)
            .map(|i| {
                if i % 3 == 0 {
                    format!("l{i}\r\n")
                } else {
                    format!("l{i}\n")
                }
            })
            .collect();
        let mut m = Buffer::from_text(&big);
        assert_eq!(m.encoded(), big.as_bytes());
        m.set_line_ending(LineEnding::CrLf);
        let crlf = String::from_utf8(m.encoded()).unwrap();
        assert!(!crlf.contains("\r\r") && !crlf.replace("\r\n", "").contains('\n'));
        m.set_line_ending(LineEnding::Lf);
        assert!(!String::from_utf8(m.encoded()).unwrap().contains('\r'));
        b.set_line_ending(LineEnding::Lf);
        assert_eq!(b.encoded(), b"a\nb\nc\rd\n\n\r\re\n");
        // Pure files keep their kind.
        assert_eq!(
            Buffer::from_text("x\r\ny\r\n").line_ending(),
            LineEnding::CrLf
        );
        assert_eq!(Buffer::from_text("x\ny").line_ending(), LineEnding::Lf);
    }

    #[test]
    fn lines_without_terminators() {
        let b = Buffer::from_text("one\r\ntwo\r\n");
        assert_eq!(b.line_ending(), LineEnding::CrLf);
        assert_eq!(b.len_lines(), 3);
        assert_eq!(b.line_text(0), "one");
        assert_eq!(b.line_len(1), 3);
        assert_eq!(b.line_text(2), "");
        assert_eq!(b.line_text(99), "");
        assert_eq!(b.text(), "one\ntwo\n");
    }

    #[test]
    fn save_recreates_a_deleted_folder() {
        // v1.0 finding: "deleted on disk (save to recreate it)" for a file
        // whose folder was removed, and the save then failed.
        let dir = std::env::temp_dir().join(format!("skink-gone-{}", std::process::id()));
        let f = dir.join("sub/d.txt");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, "sub\n").unwrap();
        let mut b = Buffer::open(&f).unwrap();
        fs::remove_dir_all(dir.join("sub")).unwrap();
        b.save().unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "sub\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn encodings_round_trip_and_bad_bytes_are_read_only() {
        let dir = std::env::temp_dir().join(format!("skink-enc-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let cases: [(&str, Vec<u8>, Encoding); 3] = [
            (
                "bom.txt",
                b"\xEF\xBB\xBFh\xC3\xA9\r\n".to_vec(),
                Encoding::Utf8Bom,
            ),
            (
                "le.txt",
                vec![0xFF, 0xFE, b'h', 0, 0xE9, 0, b'\n', 0],
                Encoding::Utf16Le,
            ),
            (
                "be.txt",
                vec![0xFE, 0xFF, 0, b'h', 0, 0xE9, 0, b'\n'],
                Encoding::Utf16Be,
            ),
        ];
        for (name, bytes, enc) in cases {
            let p = dir.join(name);
            fs::write(&p, &bytes).unwrap();
            let mut b = Buffer::open(&p).unwrap();
            assert_eq!(b.encoding(), enc, "{name}");
            assert_eq!(b.line_text(0), "h\u{e9}");
            b.insert(0, "x");
            b.remove(0, 1);
            b.save().unwrap();
            assert_eq!(fs::read(&p).unwrap(), bytes, "{name} round-trips");
        }
        let bad = dir.join("latin1.txt");
        fs::write(&bad, b"caf\xE9").unwrap();
        let b = Buffer::open(&bad).unwrap();
        assert_eq!(b.access(), Access::ReadOnly(ReadOnly::Undecodable));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_keeps_mode_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("skink-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let real = dir.join("script.sh");
        fs::write(&real, "echo a\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.join("link.sh");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut b = Buffer::open(&link).unwrap();
        b.insert(0, "#!/bin/sh\n");
        b.save().unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "#!/bin/sh\necho a\n");
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2, "no temp file left");
        // External change is noticed.
        assert_eq!(b.disk_change(), Some(DiskChange::Unchanged));
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&real, "changed and longer\n").unwrap();
        assert_eq!(b.disk_change(), Some(DiskChange::Modified));
        fs::remove_file(&real).unwrap();
        assert_eq!(b.disk_change(), Some(DiskChange::Deleted));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn view_buffers_are_read_only_with_a_title() {
        let b = Buffer::view("Help", "a\nb");
        assert!(b.is_read_only());
        assert_eq!(b.title().as_deref(), Some("Help"));
        assert!(b.path().is_none());
    }

    #[test]
    fn lf_by_default() {
        assert_eq!(Buffer::from_text("a\nb").line_ending(), LineEnding::Lf);
        assert_eq!(Buffer::empty().len_lines(), 1);
    }

    #[test]
    fn journal_chains_edits_with_points() {
        let mut b = Buffer::from_text("ab\ncd");
        let r0 = b.revision();
        b.insert(4, "X\nYZ"); // after "c"
        b.remove(0, 1); // drop "a"
        let e = b.edits_since(r0).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].start_byte, e[0].new_end_byte), (4, 8));
        assert_eq!(e[0].start, TextPoint { row: 1, col: 1 });
        assert_eq!(e[0].new_end, TextPoint { row: 2, col: 2 });
        assert_eq!(
            (e[1].start_byte, e[1].old_end_byte, e[1].new_end_byte),
            (0, 1, 0)
        );
        assert_eq!(b.edits_since(b.revision()).unwrap().len(), 0);
        assert!(b.edits_since(b.revision() + 1).is_none());
    }

    #[test]
    fn insert_remove_and_dirty() {
        let mut b = Buffer::from_text("hello");
        assert!(!b.is_dirty());
        b.insert(5, " world");
        b.remove(0, 6);
        assert_eq!(b.text(), "world");
        assert!(b.is_dirty());
    }

    #[test]
    fn save_roundtrip_keeps_crlf() {
        let dir = std::env::temp_dir().join(format!("skink-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.txt");
        fs::write(&p, "a\r\nb\r\n").unwrap();
        let mut b = Buffer::open(&p).unwrap();
        b.insert(1, "X");
        b.save().unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"aX\r\nb\r\n");
        assert!(!b.is_dirty());
        let mut n = Buffer::new_file(&dir.join("new.txt"));
        n.insert(0, "hi\n");
        n.save().unwrap();
        assert_eq!(fs::read(dir.join("new.txt")).unwrap(), b"hi\n");
        fs::remove_dir_all(&dir).unwrap();
    }
}
