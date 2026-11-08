//! Client-side callbacks: what Skink does when the *agent* asks for something.
//!
//! `session/request_permission` is not here: it surfaces as an
//! [`AgentEvent::Permission`](crate::agent::AgentEvent) carrying a reply handle,
//! so the UI answers whenever the user decides.
//!
//! Handler methods are called on short-lived helper threads (never on the
//! reader thread), so they may block briefly (e.g. wait for the UI thread to
//! hand back an unsaved buffer).

use std::fs;
use std::io::{self, BufRead, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Why a client-side request failed. Mapped to JSON-RPC error codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerError {
    /// Method or capability not provided (JSON-RPC -32601).
    Unsupported,
    /// File does not exist (ACP -32002).
    NotFound(String),
    /// Path escapes the workspace root or is not absolute-resolvable (-32602).
    OutsideWorkspace(String),
    /// Anything else (-32603).
    Failed(String),
}

impl std::fmt::Display for HandlerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("not supported"),
            Self::NotFound(m) => write!(f, "not found: {m}"),
            Self::OutsideWorkspace(m) => write!(f, "outside workspace: {m}"),
            Self::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for HandlerError {}

impl From<io::Error> for HandlerError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::NotFound {
            Self::NotFound(e.to_string())
        } else {
            Self::Failed(e.to_string())
        }
    }
}

/// Terminal creation request (`terminal/create`), wire-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSpec {
    pub session: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    pub output_byte_limit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalExit {
    pub exit_code: Option<u32>,
    pub signal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalOutput {
    pub output: String,
    pub truncated: bool,
    /// `Some` once the command has exited.
    pub exit: Option<TerminalExit>,
}

/// Hook for the `terminal/*` client methods. Every method defaults to
/// "unsupported", so a host implements only what it can back with a real
/// terminal pane (`skink-term`) and opts in via [`AcpHandler::terminal`].
pub trait TerminalHost: Send + Sync {
    /// Start `spec`; returns the terminal id.
    fn create(&self, _spec: &TerminalSpec) -> Result<String, HandlerError> {
        Err(HandlerError::Unsupported)
    }
    fn output(&self, _session: &str, _terminal: &str) -> Result<TerminalOutput, HandlerError> {
        Err(HandlerError::Unsupported)
    }
    /// Block until the command exits.
    fn wait_for_exit(&self, _session: &str, _terminal: &str) -> Result<TerminalExit, HandlerError> {
        Err(HandlerError::Unsupported)
    }
    fn kill(&self, _session: &str, _terminal: &str) -> Result<(), HandlerError> {
        Err(HandlerError::Unsupported)
    }
    fn release(&self, _session: &str, _terminal: &str) -> Result<(), HandlerError> {
        Err(HandlerError::Unsupported)
    }
}

/// What the client answers to `fs/*` and `terminal/*` requests.
pub trait AcpHandler: Send + Sync + 'static {
    /// `fs/read_text_file`. `line` is 1-based, `limit` a line count.
    fn read_text_file(
        &self,
        session: &str,
        path: &Path,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> Result<String, HandlerError>;

    /// `fs/write_text_file`.
    fn write_text_file(
        &self,
        session: &str,
        path: &Path,
        content: &str,
    ) -> Result<(), HandlerError>;

    /// Advertised as `clientCapabilities.fs.{readTextFile,writeTextFile}`.
    fn fs_capabilities(&self) -> (bool, bool) {
        (true, true)
    }

    /// Terminal support; `Some` advertises `clientCapabilities.terminal`.
    fn terminal(&self) -> Option<&dyn TerminalHost> {
        None
    }
}

/// Handler for hosts that want agents to use their own file tools: no fs, no
/// terminal advertised.
pub struct NoHandler;

impl AcpHandler for NoHandler {
    fn read_text_file(
        &self,
        _: &str,
        _: &Path,
        _: Option<u32>,
        _: Option<u32>,
    ) -> Result<String, HandlerError> {
        Err(HandlerError::Unsupported)
    }
    fn write_text_file(&self, _: &str, _: &Path, _: &str) -> Result<(), HandlerError> {
        Err(HandlerError::Unsupported)
    }
    fn fs_capabilities(&self) -> (bool, bool) {
        (false, false)
    }
}

/// Access to editor buffers so agents see *unsaved* text and writes can be
/// applied as editor edits instead of silently changing disk under the user.
pub trait BufferSource: Send + Sync {
    /// Current text of the open buffer for `path`, or `None` if not open.
    /// `path` is the resolved (symlink-free where possible) absolute path.
    fn read(&self, path: &Path) -> Option<String>;
    /// Apply a full-content write to an open buffer. `None`: not an open
    /// buffer, so the caller writes to disk. `Some(Err)`: refused.
    fn write(&self, _path: &Path, _content: &str) -> Option<Result<(), String>> {
        None
    }
}

/// No open buffers: always read from and write to disk.
pub struct NoBuffers;

impl BufferSource for NoBuffers {
    fn read(&self, _: &Path) -> Option<String> {
        None
    }
}

/// Default handler: serves `fs/*` inside a workspace root, preferring editor
/// buffers (via [`BufferSource`]) over disk. Paths outside the root (after
/// resolving `..` and symlinks) are refused.
pub struct WorkspaceFs {
    root: PathBuf,
    buffers: Arc<dyn BufferSource>,
    terminal: Option<Arc<dyn TerminalHost>>,
}

impl WorkspaceFs {
    pub fn new(root: impl AsRef<Path>, buffers: Arc<dyn BufferSource>) -> Self {
        let root = root.as_ref();
        let root = canonicalize(root)
            .unwrap_or_else(|_| normalize(root).unwrap_or_else(|| root.to_path_buf()));
        Self {
            root,
            buffers,
            terminal: None,
        }
    }
    /// Attach a terminal implementation (advertises the terminal capability).
    #[must_use]
    pub fn with_terminal(mut self, t: Arc<dyn TerminalHost>) -> Self {
        self.terminal = Some(t);
        self
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve an agent-supplied path to an absolute path inside the root.
    pub fn resolve(&self, path: &Path) -> Result<PathBuf, HandlerError> {
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let norm = normalize(&joined)
            .ok_or_else(|| HandlerError::OutsideWorkspace(path.display().to_string()))?;
        // Canonicalise the longest existing ancestor to defeat symlink escapes,
        // then re-append the not-yet-existing tail.
        let mut existing = norm.as_path();
        let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
        let canon = loop {
            match canonicalize(existing) {
                Ok(c) => break c,
                Err(_) => match (existing.parent(), existing.file_name()) {
                    (Some(p), Some(n)) => {
                        tail.push(n);
                        existing = p;
                    }
                    _ => break existing.to_path_buf(),
                },
            }
        };
        let mut full = canon;
        for n in tail.iter().rev() {
            full.push(n);
        }
        if full.starts_with(&self.root) {
            Ok(full)
        } else {
            Err(HandlerError::OutsideWorkspace(path.display().to_string()))
        }
    }
}

/// `fs::canonicalize`, but in the ordinary (non-`\\?\`) form on Windows, so
/// resolved paths match the editor's buffer paths and the root prefix check.
fn canonicalize(p: &Path) -> io::Result<PathBuf> {
    let c = fs::canonicalize(p)?;
    if cfg!(windows)
        && let Some(s) = c.to_str().and_then(crate::agent::strip_verbatim)
    {
        return Ok(PathBuf::from(s));
    }
    Ok(c)
}

/// Largest file `fs/read_text_file` returns in one response (the agent's
/// reader typically has a similar line limit; ours is 32 MiB).
pub const MAX_READ_BYTES: usize = 16 << 20;

/// Read a disk file's `line`/`limit` window without loading more than the
/// window (plus one line) into memory; errors past [`MAX_READ_BYTES`].
fn read_disk_window(
    p: &Path,
    line: Option<u32>,
    limit: Option<u32>,
) -> Result<String, HandlerError> {
    let too_big = || {
        HandlerError::Failed(format!(
            "{} is larger than {} MiB; read it with line/limit",
            p.display(),
            MAX_READ_BYTES >> 20
        ))
    };
    let mut r = io::BufReader::new(fs::File::open(p)?);
    for _ in 1..line.unwrap_or(1) {
        if r.skip_until(b'\n')? == 0 {
            break;
        }
    }
    let mut out = Vec::new();
    let mut lines = 0u64;
    while limit.is_none_or(|n| lines < u64::from(n)) {
        let mut chunk = (&mut r).take((MAX_READ_BYTES + 1 - out.len()) as u64);
        if chunk.read_until(b'\n', &mut out)? == 0 {
            break;
        }
        if out.len() > MAX_READ_BYTES {
            return Err(too_big());
        }
        lines += 1;
    }
    String::from_utf8(out)
        .map_err(|_| HandlerError::Failed(format!("{} is not valid UTF-8", p.display())))
}

/// Lexically remove `.` and `..`; `None` if `..` climbs above the root of the path.
fn normalize(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

/// Apply ACP's optional `line` (1-based) / `limit` (line count) window.
pub fn slice_lines(content: &str, line: Option<u32>, limit: Option<u32>) -> String {
    if line.is_none() && limit.is_none() {
        return content.to_owned();
    }
    let start = line.map_or(0, |l| l.saturating_sub(1) as usize);
    let it = content.split_inclusive('\n').skip(start);
    match limit {
        Some(n) => it.take(n as usize).collect(),
        None => it.collect(),
    }
}

impl AcpHandler for WorkspaceFs {
    fn read_text_file(
        &self,
        _session: &str,
        path: &Path,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> Result<String, HandlerError> {
        let p = self.resolve(path)?;
        match self.buffers.read(&p) {
            Some(text) => Ok(slice_lines(&text, line, limit)),
            None => read_disk_window(&p, line, limit),
        }
    }

    fn write_text_file(
        &self,
        _session: &str,
        path: &Path,
        content: &str,
    ) -> Result<(), HandlerError> {
        let p = self.resolve(path)?;
        match self.buffers.write(&p, content) {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => Err(HandlerError::Failed(e)),
            None => {
                if let Some(parent) = p.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&p, content)?;
                Ok(())
            }
        }
    }

    fn terminal(&self) -> Option<&dyn TerminalHost> {
        self.terminal.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct Bufs(Mutex<HashMap<PathBuf, String>>);
    impl BufferSource for Bufs {
        fn read(&self, p: &Path) -> Option<String> {
            self.0.lock().unwrap().get(p).cloned()
        }
        fn write(&self, p: &Path, c: &str) -> Option<Result<(), String>> {
            let mut g = self.0.lock().unwrap();
            g.get_mut(p).map(|b| {
                *b = c.to_owned();
                Ok(())
            })
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("skink-ai-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::canonicalize(d).unwrap()
    }

    #[test]
    fn reads_disk_and_prefers_buffers() {
        let root = tmp("rd");
        fs::write(root.join("a.txt"), "disk\n").unwrap();
        let bufs = Arc::new(Bufs(Mutex::new(HashMap::new())));
        bufs.0
            .lock()
            .unwrap()
            .insert(root.join("a.txt"), "unsaved\n".into());
        let h = WorkspaceFs::new(&root, bufs.clone());
        assert_eq!(
            h.read_text_file("s", &root.join("a.txt"), None, None)
                .unwrap(),
            "unsaved\n"
        );
        bufs.0.lock().unwrap().clear();
        assert_eq!(
            h.read_text_file("s", &root.join("a.txt"), None, None)
                .unwrap(),
            "disk\n"
        );
        // relative paths resolve against the root
        assert_eq!(
            h.read_text_file("s", Path::new("a.txt"), None, None)
                .unwrap(),
            "disk\n"
        );
        assert!(matches!(
            h.read_text_file("s", &root.join("nope"), None, None),
            Err(HandlerError::NotFound(_))
        ));
    }

    #[test]
    fn rejects_escapes() {
        let root = tmp("esc");
        let h = WorkspaceFs::new(&root, Arc::new(NoBuffers));
        for p in [
            root.join("../outside.txt"),
            PathBuf::from("../../etc/passwd"),
            root.parent().unwrap().join("other"),
        ] {
            assert!(
                matches!(
                    h.read_text_file("s", &p, None, None),
                    Err(HandlerError::OutsideWorkspace(_))
                ),
                "{p:?}"
            );
            assert!(matches!(
                h.write_text_file("s", &p, "x"),
                Err(HandlerError::OutsideWorkspace(_))
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let root = tmp("sym");
        let outside = tmp("sym-out");
        fs::write(outside.join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let h = WorkspaceFs::new(&root, Arc::new(NoBuffers));
        assert!(matches!(
            h.read_text_file("s", &root.join("link/secret"), None, None),
            Err(HandlerError::OutsideWorkspace(_))
        ));
        assert!(matches!(
            h.write_text_file("s", &root.join("link/new"), "x"),
            Err(HandlerError::OutsideWorkspace(_))
        ));
    }

    #[test]
    fn writes_go_to_buffer_or_disk() {
        let root = tmp("wr");
        let bufs = Arc::new(Bufs(Mutex::new(HashMap::new())));
        bufs.0
            .lock()
            .unwrap()
            .insert(root.join("open.rs"), "old".into());
        let h = WorkspaceFs::new(&root, bufs.clone());
        h.write_text_file("s", &root.join("open.rs"), "new")
            .unwrap();
        assert_eq!(bufs.0.lock().unwrap()[&root.join("open.rs")], "new");
        assert!(
            !root.join("open.rs").exists(),
            "open buffer must not hit disk"
        );
        h.write_text_file("s", &root.join("sub/dir/new.rs"), "fresh")
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("sub/dir/new.rs")).unwrap(),
            "fresh"
        );
    }

    #[test]
    fn line_window() {
        let t = "a\nb\nc\nd\n";
        assert_eq!(slice_lines(t, Some(2), Some(2)), "b\nc\n");
        assert_eq!(slice_lines(t, Some(3), None), "c\nd\n");
        assert_eq!(slice_lines(t, None, Some(1)), "a\n");
        assert_eq!(slice_lines(t, Some(99), None), "");
        assert_eq!(slice_lines(t, None, None), t);
    }

    #[test]
    fn disk_reads_are_windowed_and_bounded() {
        let root = tmp("big");
        let h = WorkspaceFs::new(&root, Arc::new(NoBuffers));
        fs::write(root.join("t"), "a\nb\nc\nd").unwrap();
        let read = |l, n| h.read_text_file("s", &root.join("t"), l, n).unwrap();
        assert_eq!(read(Some(2), Some(2)), "b\nc\n");
        assert_eq!(read(Some(4), None), "d");
        assert_eq!(read(Some(0), Some(1)), "a\n");
        assert_eq!(read(Some(9), None), "");
        assert_eq!(read(None, None), "a\nb\nc\nd");
        // Regression: the whole file was loaded whatever its size.
        let big = vec![b'x'; MAX_READ_BYTES + 10];
        fs::write(root.join("big"), &big).unwrap();
        assert!(matches!(
            h.read_text_file("s", &root.join("big"), None, None),
            Err(HandlerError::Failed(_))
        ));
        let mut lines = b"first\n".to_vec();
        lines.extend_from_slice(&big);
        fs::write(root.join("big"), &lines).unwrap();
        assert_eq!(
            h.read_text_file("s", &root.join("big"), Some(1), Some(1))
                .unwrap(),
            "first\n"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn non_utf8_is_an_error_not_a_panic() {
        let root = tmp("bin");
        fs::write(root.join("b"), [0xff, 0xfe, 0x00]).unwrap();
        let h = WorkspaceFs::new(&root, Arc::new(NoBuffers));
        assert!(matches!(
            h.read_text_file("s", &root.join("b"), None, None),
            Err(HandlerError::Failed(_))
        ));
    }
}
