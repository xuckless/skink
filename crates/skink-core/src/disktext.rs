//! Very large files read straight from disk: a read-only text store that
//! never holds the whole file. A background thread scans the file once and
//! keeps a sparse line index (a checkpoint every [`CP_LINES`] lines or
//! [`CP_BYTES`] bytes); lines are read on demand in blocks between two
//! checkpoints, and the last few blocks are cached. A 2 GB log costs a few
//! hundred KB of index plus the cached blocks.
//!
//! Characters are counted per byte that is not a UTF-8 continuation byte:
//! a valid sequence is one char, any other lead byte is shown as U+FFFD,
//! and stray continuation bytes are dropped. Counting and decoding use the
//! same rule, so char positions always agree with the text shown. Like the
//! rope's lines, a `\r` before `\n` is part of the line's chars but not of
//! its text.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

/// A checkpoint at least every this many lines...
pub const CP_LINES: u64 = 1024;
/// ... or at the first line start after this many bytes.
pub const CP_BYTES: u64 = 1 << 20;
/// At most this much of one block is read (only a block's last line can be
/// longer: it is cut there).
const BLOCK_CAP: u64 = 8 << 20;
/// Blocks kept in memory.
const CACHED: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Checkpoint {
    line: u64,
    byte: u64,
    char: u64,
}

/// How far the background scan got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scan {
    /// Still reading: `bytes` of `total` indexed so far.
    Running {
        bytes: u64,
        total: u64,
    },
    Done,
    Failed(String),
}

#[derive(Debug)]
struct Index {
    cps: Vec<Checkpoint>,
    /// Newlines, chars and bytes seen so far.
    lines: u64,
    chars: u64,
    bytes: u64,
    scan: Scan,
}

#[derive(Debug)]
struct Block {
    first_line: u64,
    byte0: u64,
    char0: u64,
    /// Bytes read (from `byte0`): whole lines, the last maybe cut.
    bytes: Vec<u8>,
    /// Offsets of each line start in `bytes`, then `bytes.len()`.
    starts: Vec<usize>,
    /// Chars before each line start (relative to `char0`).
    chars: Vec<u64>,
    /// The indexed end of the block when it was read (a growing last block
    /// is read again once the scan has moved on).
    end: u64,
}

/// A file on disk, indexed in the background; read-only.
#[derive(Debug)]
pub struct DiskText {
    file: File,
    total: u64,
    index: Arc<RwLock<Index>>,
    cache: Mutex<Vec<Arc<Block>>>,
}

fn is_cont(b: u8) -> bool {
    b & 0xC0 == 0x80
}

/// Chars in `bytes` (see the module docs).
pub fn count_chars(bytes: &[u8]) -> u64 {
    bytes.iter().filter(|&&b| !is_cont(b)).count() as u64
}

/// Decode `bytes` with the same one-char-per-lead-byte rule.
pub fn decode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            out.push(b as char);
            i += 1;
            continue;
        }
        if is_cont(b) {
            i += 1;
            continue;
        }
        let n = match b {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => 1,
        };
        match bytes
            .get(i..i + n)
            .and_then(|s| std::str::from_utf8(s).ok())
        {
            Some(s) if n > 1 => {
                out.push_str(s);
                i += n;
            }
            _ => {
                out.push(char::REPLACEMENT_CHARACTER);
                i += 1;
            }
        }
    }
    out
}

fn read_at(file: &File, buf: &mut [u8], at: u64) -> io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        let mut got = 0;
        while got < buf.len() {
            match file.read_at(&mut buf[got..], at + got as u64)? {
                0 => break,
                n => got += n,
            }
        }
        Ok(got)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut got = 0;
        while got < buf.len() {
            match file.seek_read(&mut buf[got..], at + got as u64)? {
                0 => break,
                n => got += n,
            }
        }
        Ok(got)
    }
}

impl DiskText {
    /// Open `path` and start indexing it in the background.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let total = file.metadata()?.len();
        let index = Arc::new(RwLock::new(Index {
            cps: vec![Checkpoint {
                line: 0,
                byte: 0,
                char: 0,
            }],
            lines: 0,
            chars: 0,
            bytes: 0,
            scan: Scan::Running { bytes: 0, total },
        }));
        let scan_file = file.try_clone()?;
        let shared = Arc::clone(&index);
        std::thread::Builder::new()
            .name("skink-index".into())
            .spawn(move || {
                if let Err(e) = scan(scan_file, total, &shared)
                    && let Ok(mut ix) = shared.write()
                {
                    ix.scan = Scan::Failed(e.to_string());
                }
            })?;
        Ok(Self {
            file,
            total,
            index,
            cache: Mutex::new(Vec::new()),
        })
    }

    /// Size of the file when it was opened.
    pub fn total_bytes(&self) -> u64 {
        self.total
    }

    pub fn scan(&self) -> Scan {
        self.index.read().map_or(Scan::Done, |ix| ix.scan.clone())
    }

    /// Wait for the scan to finish (tests, measurements).
    pub fn wait(&self, timeout: std::time::Duration) -> bool {
        let until = std::time::Instant::now() + timeout;
        while matches!(self.scan(), Scan::Running { .. }) {
            if std::time::Instant::now() > until {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        true
    }

    fn counts(&self) -> (u64, u64, u64) {
        self.index
            .read()
            .map_or((0, 0, 0), |ix| (ix.lines, ix.chars, ix.bytes))
    }

    /// Lines indexed so far (a trailing newline makes a last empty line).
    pub fn len_lines(&self) -> usize {
        self.counts().0 as usize + 1
    }

    pub fn len_chars(&self) -> usize {
        self.counts().1 as usize
    }

    pub fn len_bytes(&self) -> usize {
        self.counts().2 as usize
    }

    /// Checkpoint index `b` holding `key` (by line, char or byte).
    fn block_of(&self, key: impl Fn(&Checkpoint) -> u64, v: u64) -> usize {
        let ix = self.index.read().expect("index lock");
        ix.cps.partition_point(|c| key(c) <= v).saturating_sub(1)
    }

    fn block(&self, b: usize) -> Arc<Block> {
        let (cp, end) = {
            let ix = self.index.read().expect("index lock");
            let cp = ix.cps[b.min(ix.cps.len() - 1)];
            let end = ix.cps.get(b + 1).map_or(ix.bytes, |n| n.byte);
            (cp, end)
        };
        if let Ok(cache) = self.cache.lock()
            && let Some(hit) = cache.iter().find(|c| c.byte0 == cp.byte && c.end == end)
        {
            return Arc::clone(hit);
        }
        let want = (end - cp.byte).min(BLOCK_CAP) as usize;
        let mut bytes = vec![0; want];
        let got = read_at(&self.file, &mut bytes, cp.byte).unwrap_or(0);
        bytes.truncate(got);
        let mut starts = vec![0];
        let mut chars = vec![0];
        let mut n = 0u64;
        for (i, &c) in bytes.iter().enumerate() {
            if !is_cont(c) {
                n += 1;
            }
            if c == b'\n' {
                starts.push(i + 1);
                chars.push(n);
            }
        }
        // Where the last line ends.
        starts.push(bytes.len());
        chars.push(n);
        let block = Arc::new(Block {
            first_line: cp.line,
            byte0: cp.byte,
            char0: cp.char,
            bytes,
            starts,
            chars,
            end,
        });
        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() == CACHED {
                cache.remove(0);
            }
            cache.push(Arc::clone(&block));
        }
        block
    }

    /// The block holding `line` and the line's number inside it.
    fn line_block(&self, line: usize) -> (Arc<Block>, usize) {
        let line = line.min(self.len_lines() - 1) as u64;
        let b = self.block(self.block_of(|c| c.line, line));
        let k = ((line - b.first_line) as usize).min(b.starts.len().saturating_sub(2));
        (b, k)
    }

    /// Raw bytes of a line without its `\n` (and a `\r` before it).
    fn line_bytes(b: &Block, k: usize) -> &[u8] {
        let (s, e) = (
            b.starts[k],
            b.starts.get(k + 1).copied().unwrap_or(b.starts[k]),
        );
        let mut line = &b.bytes[s..e.max(s)];
        if let [rest @ .., b'\n'] = line {
            line = rest;
        }
        if let [rest @ .., b'\r'] = line {
            line = rest;
        }
        line
    }

    pub fn line_text(&self, line: usize) -> String {
        let (b, k) = self.line_block(line);
        decode(Self::line_bytes(&b, k))
    }

    pub fn line_len(&self, line: usize) -> usize {
        let (b, k) = self.line_block(line);
        count_chars(Self::line_bytes(&b, k)) as usize
    }

    pub fn line_to_char(&self, line: usize) -> usize {
        let (b, k) = self.line_block(line);
        (b.char0 + b.chars[k]) as usize
    }

    pub fn char_to_line(&self, c: usize) -> usize {
        let c = c.min(self.len_chars()) as u64;
        let b = self.block(self.block_of(|cp| cp.char, c));
        let k = b
            .chars
            .partition_point(|&n| b.char0 + n <= c)
            .saturating_sub(1)
            .min(b.starts.len().saturating_sub(2));
        ((b.first_line + k as u64) as usize).min(self.len_lines() - 1)
    }

    /// File byte of char `c`.
    pub fn char_to_byte(&self, c: usize) -> usize {
        let line = self.char_to_line(c);
        let (b, k) = self.line_block(line);
        let mut left = c as u64 - (b.char0 + b.chars[k]);
        let start = b.starts[k];
        for (i, &x) in b.bytes[start..].iter().enumerate() {
            if !is_cont(x) {
                if left == 0 {
                    return (b.byte0 + (start + i) as u64) as usize;
                }
                left -= 1;
            }
        }
        (b.byte0 as usize) + b.bytes.len()
    }

    /// Char of file byte `byte`.
    pub fn byte_to_char(&self, byte: usize) -> usize {
        let byte = (byte as u64).min(self.counts().2);
        let b = self.block(self.block_of(|cp| cp.byte, byte));
        let rel = ((byte - b.byte0) as usize).min(b.bytes.len());
        (b.char0 + count_chars(&b.bytes[..rel])) as usize
    }

    pub fn char_at(&self, c: usize) -> Option<char> {
        if c >= self.len_chars() {
            return None;
        }
        let line = self.char_to_line(c);
        let col = c - self.line_to_char(line);
        Some(self.line_text(line).chars().nth(col).unwrap_or('\n'))
    }

    /// Text of chars `[start, end)`, read from disk (at most `cap` bytes).
    pub fn slice(&self, start: usize, end: usize, cap: usize) -> String {
        let end = end.min(self.len_chars());
        let start = start.min(end);
        let (a, z) = (self.char_to_byte(start), self.char_to_byte(end));
        let mut buf = vec![0; (z - a).min(cap)];
        let got = read_at(&self.file, &mut buf, a as u64).unwrap_or(0);
        buf.truncate(got);
        decode(&buf)
    }

    /// Read the whole file (only for explicit user actions on it).
    pub fn read_all(&self) -> String {
        let mut buf = Vec::new();
        let mut chunk = vec![0; 1 << 20];
        let mut at = 0;
        while let Ok(n) = read_at(&self.file, &mut chunk, at) {
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            at += n as u64;
        }
        decode(&buf)
    }
}

/// The background index scan.
fn scan(mut file: File, total: u64, index: &RwLock<Index>) -> io::Result<()> {
    let mut buf = vec![0u8; 1 << 20];
    let (mut lines, mut chars, mut bytes) = (0u64, 0u64, 0u64);
    let mut since_cp = 0u64;
    let mut last_cp_line = 0u64;
    let mut pending: Vec<Checkpoint> = Vec::new();
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for &c in &buf[..n] {
            bytes += 1;
            since_cp += 1;
            if !is_cont(c) {
                chars += 1;
            }
            if c == b'\n' {
                lines += 1;
                if lines - last_cp_line >= CP_LINES || since_cp >= CP_BYTES {
                    pending.push(Checkpoint {
                        line: lines,
                        byte: bytes,
                        char: chars,
                    });
                    last_cp_line = lines;
                    since_cp = 0;
                }
            }
        }
        let mut ix = index.write().map_err(|_| io::Error::other("index lock"))?;
        ix.cps.append(&mut pending);
        (ix.lines, ix.chars, ix.bytes) = (lines, chars, bytes);
        ix.scan = Scan::Running {
            bytes,
            total: total.max(bytes),
        };
    }
    let mut ix = index.write().map_err(|_| io::Error::other("index lock"))?;
    (ix.lines, ix.chars, ix.bytes) = (lines, chars, bytes);
    ix.scan = Scan::Done;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("skink-disk-{}-{name}", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// The disk store answers exactly like a rope over the same text.
    #[test]
    fn positions_agree_with_a_rope() {
        let mut text = String::new();
        for i in 0..5000 {
            text.push_str(&format!("line {i} é日\u{1F600}{}\n", "x".repeat(i % 37)));
        }
        text.push_str("last");
        let p = file("agree", text.as_bytes());
        let d = DiskText::open(&p).unwrap();
        assert!(d.wait(Duration::from_secs(10)));
        let rope = ropey::Rope::from_str(&text);
        assert_eq!(d.len_lines(), rope.len_lines());
        assert_eq!(d.len_chars(), rope.len_chars());
        for l in [0, 1, 1023, 1024, 1025, 2047, 2048, 4999, 5000] {
            let mut want = rope.line(l).to_string();
            while want.ends_with('\n') {
                want.pop();
            }
            assert_eq!(d.line_text(l), want, "line {l}");
            assert_eq!(d.line_to_char(l), rope.line_to_char(l), "line {l}");
            assert_eq!(d.line_len(l), want.chars().count());
        }
        for c in [
            0,
            7,
            50_000,
            123_456,
            rope.len_chars() - 1,
            rope.len_chars(),
        ] {
            assert_eq!(d.char_to_line(c), rope.char_to_line(c), "char {c}");
            assert_eq!(d.char_to_byte(c), rope.char_to_byte(c), "char {c}");
            assert_eq!(d.byte_to_char(rope.char_to_byte(c)), c);
        }
        assert_eq!(
            d.slice(100, 400, usize::MAX),
            rope.slice(100..400).to_string()
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn crlf_and_bad_bytes_keep_char_positions_consistent() {
        let p = file("crlf", b"ab\r\nc\xffd\xe9\r\nlast");
        let d = DiskText::open(&p).unwrap();
        assert!(d.wait(Duration::from_secs(5)));
        assert_eq!(d.len_lines(), 3);
        assert_eq!(d.line_text(0), "ab");
        assert_eq!(d.line_text(1), "c\u{FFFD}d\u{FFFD}");
        assert_eq!(d.line_len(1), 4);
        // \r and \n are chars of line 0, like a rope with mixed endings.
        assert_eq!(d.line_to_char(1), 4);
        assert_eq!(d.line_to_char(2), 10);
        assert_eq!(d.line_text(2), "last");
        assert_eq!(d.char_at(5), Some('\u{FFFD}'));
        let _ = std::fs::remove_file(&p);
    }
}
