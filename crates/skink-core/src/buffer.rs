use ropey::Rope;
use std::{
    fs,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
        }
    }
}

/// Text storage. A rope keeps edits and memory use sane for large files.
///
/// Text is held with `\n` line endings only; a CRLF file is detected on load
/// and converted back on save, so the rest of the editor never sees `\r`.
#[derive(Debug)]
pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    line_ending: LineEnding,
    read_only: bool,
    dirty: bool,
    revision: u64,
}

impl Buffer {
    pub fn empty() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            line_ending: LineEnding::Lf,
            read_only: false,
            dirty: false,
            revision: 0,
        }
    }

    pub fn from_text(text: &str) -> Self {
        let line_ending = detect_line_ending(text);
        let rope = match line_ending {
            LineEnding::CrLf => Rope::from_str(&text.replace("\r\n", "\n")),
            LineEnding::Lf => Rope::from_str(text),
        };
        Self {
            rope,
            path: None,
            line_ending,
            read_only: false,
            dirty: false,
            revision: 0,
        }
    }

    /// Open a file, or, for a directory, a read-only listing (placeholder
    /// until the file tree exists).
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
            b.read_only = true;
            return Ok(b);
        }
        let bytes = fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        let mut b = Self::from_text(&text);
        b.path = Some(path.to_path_buf());
        Ok(b)
    }

    /// A buffer for a path that does not exist yet: empty, remembers the
    /// path so the first save creates the file.
    pub fn new_file(path: &Path) -> Self {
        let mut b = Self::empty();
        b.path = Some(path.to_path_buf());
        b
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

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Bumped on every text change; lets observers (LSP sync) skip no-ops.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Write to the buffer's path, restoring CRLF if the file had it.
    pub fn save(&mut self) -> io::Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| io::Error::other("no file name"))?;
        if self.read_only {
            return Err(io::Error::other("buffer is read-only"));
        }
        let mut w = BufWriter::new(fs::File::create(path)?);
        for chunk in self.rope.chunks() {
            match self.line_ending {
                LineEnding::Lf => w.write_all(chunk.as_bytes())?,
                LineEnding::CrLf => {
                    let mut parts = chunk.split('\n');
                    if let Some(first) = parts.next() {
                        w.write_all(first.as_bytes())?;
                    }
                    for p in parts {
                        w.write_all(b"\r\n")?;
                        w.write_all(p.as_bytes())?;
                    }
                }
            }
        }
        w.flush()?;
        self.dirty = false;
        Ok(())
    }

    /// Number of lines; a trailing newline yields a final empty line.
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn line_to_char(&self, line: usize) -> usize {
        self.rope.line_to_char(line.min(self.len_lines() - 1))
    }

    pub fn char_to_line(&self, idx: usize) -> usize {
        self.rope.char_to_line(idx.min(self.len_chars()))
    }

    pub fn char_at(&self, idx: usize) -> Option<char> {
        (idx < self.len_chars()).then(|| self.rope.char(idx))
    }

    /// Text in `[start, end)` chars.
    pub fn slice(&self, start: usize, end: usize) -> String {
        let end = end.min(self.len_chars());
        self.rope.slice(start.min(end)..end).to_string()
    }

    /// Whole buffer text (for LSP full sync, agents).
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Insert at a char index. Low level: history is the editor's job.
    pub fn insert(&mut self, at: usize, text: &str) {
        self.rope.insert(at.min(self.len_chars()), text);
        self.dirty = true;
        self.revision += 1;
    }

    /// Remove `[start, end)` chars.
    pub fn remove(&mut self, start: usize, end: usize) {
        let end = end.min(self.len_chars());
        if start < end {
            self.rope.remove(start..end);
            self.dirty = true;
            self.revision += 1;
        }
    }

    /// Line text without its line terminator.
    pub fn line_text(&self, idx: usize) -> String {
        if idx >= self.len_lines() {
            return String::new();
        }
        let mut s = self.rope.line(idx).to_string();
        while s.ends_with(['\n', '\r']) {
            s.pop();
        }
        s
    }

    /// Length of a line in chars, excluding its terminator.
    pub fn line_len(&self, idx: usize) -> usize {
        if idx >= self.len_lines() {
            return 0;
        }
        let line = self.rope.line(idx);
        let mut n = line.len_chars();
        while n > 0 && matches!(line.char(n - 1), '\n' | '\r') {
            n -= 1;
        }
        n
    }
}

fn detect_line_ending(text: &str) -> LineEnding {
    match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => LineEnding::CrLf,
        _ => LineEnding::Lf,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn lf_by_default() {
        assert_eq!(Buffer::from_text("a\nb").line_ending(), LineEnding::Lf);
        assert_eq!(Buffer::empty().len_lines(), 1);
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
