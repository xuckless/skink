use ropey::Rope;
use std::{
    fs, io,
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
#[derive(Debug)]
pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    line_ending: LineEnding,
    read_only: bool,
}

impl Buffer {
    pub fn empty() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            line_ending: LineEnding::Lf,
            read_only: false,
        }
    }

    pub fn from_text(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            path: None,
            line_ending: detect_line_ending(text),
            read_only: false,
        }
    }

    /// Open a file, or, for a directory, a read-only listing (placeholder
    /// until the file tree exists in Phase 1).
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

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Number of lines; a trailing newline yields a final empty line.
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
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
    }

    #[test]
    fn lf_by_default() {
        assert_eq!(Buffer::from_text("a\nb").line_ending(), LineEnding::Lf);
        assert_eq!(Buffer::empty().len_lines(), 1);
    }
}
