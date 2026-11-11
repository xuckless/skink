//! Language -> server mapping with lazy spawning.
//!
//! Idea (not code) from helix-lsp's registry: one server per
//! (language, project root), started on first use. Servers are only started
//! when their binary is on `PATH`; everything is shut down when the registry
//! is dropped.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc::Receiver},
};

use crate::{
    client::{EventSink, LspClient, ServerSpec},
    types::*,
    uri::path_to_uri,
};

/// A language (or family of languages sharing a server) and how to serve it.
#[derive(Debug, Clone)]
pub struct Language {
    /// Server-group id, e.g. `"typescript"` (also used as the registry key).
    pub id: String,
    /// `(extension without dot, LSP languageId)`; matched case-insensitively.
    pub extensions: Vec<(String, String)>,
    /// Files whose presence marks a project root (nearest ancestor wins).
    pub root_markers: Vec<String>,
    /// If set, an ancestor whose `file` contains `needle` (e.g. a Cargo
    /// `[workspace]`) wins over the nearest marker; the outermost match is used.
    pub workspace_marker: Option<(String, String)>,
    /// Candidate servers in preference order; the first installed one is used.
    /// This is the primary server: single-answer requests (hover, rename,
    /// format, ...) go to it first.
    pub servers: Vec<ServerSpec>,
    /// More servers for the same files (pyright + ruff, tsserver + eslint),
    /// each a candidate list like `servers`. They get the same documents;
    /// diagnostics, completions and code actions are merged from all, and
    /// single-answer requests go to the first server that supports them.
    pub also: Vec<Vec<ServerSpec>>,
    /// Where this entry came from (`:lsp` says why a server was picked).
    pub source: Source,
}

/// Origin of a [`Language`]'s server list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    /// Skink's built-in table.
    #[default]
    BuiltIn,
    /// `"lsp.<lang>"` in settings.json (replaced or added).
    Settings,
    /// `"lsp.<lang>": false`: no server for these files.
    Disabled,
}

/// A user's `"lsp.<lang>"` setting, ready to merge over the built-ins.
#[derive(Debug, Clone, PartialEq)]
pub enum Override {
    Off,
    /// The built-in (or package) servers, unchanged. Only meaningful inside
    /// [`Override::Many`] (`["default", "ruff server"]`).
    BuiltIn,
    /// Several servers for one language, primary first.
    Many(Vec<Override>),
    Server {
        /// Program and arguments.
        argv: Vec<String>,
        /// Extra extensions (required for a language Skink does not know;
        /// defaults to the language name).
        extensions: Vec<String>,
        /// Replaces the root markers when non-empty.
        root_markers: Vec<String>,
        init_options: Option<serde_json::Value>,
        env: Vec<(String, String)>,
    },
}

/// Settings names for the built-in groups (`"lsp.javascript"` is the
/// TypeScript server, which serves both).
fn canonical_id(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "ts" | "tsx" | "javascript" | "js" | "jsx" | "typescriptreact" | "javascriptreact" => {
            "typescript".into()
        }
        "rs" => "rust".into(),
        "py" => "python".into(),
        "golang" => "go".into(),
        other => other.into(),
    }
}

/// The built-in languages with the user's overrides applied, in order.
/// Unknown names add a language.
pub fn merge_overrides(
    mut langs: Vec<Language>,
    overrides: &[(String, Override)],
) -> Vec<Language> {
    for (name, ov) in overrides {
        let id = canonical_id(name);
        let at = langs.iter().position(|l| l.id == id);
        let entries: Vec<&Override> = match ov {
            Override::Off => {
                if let Some(i) = at {
                    langs[i].servers.clear();
                    langs[i].also.clear();
                    langs[i].source = Source::Disabled;
                }
                continue;
            }
            Override::BuiltIn => continue,
            Override::Many(list) => list.iter().collect(),
            server @ Override::Server { .. } => vec![server],
        };
        let i = at.unwrap_or_else(|| {
            langs.push(Language {
                id: id.clone(),
                extensions: Vec::new(),
                root_markers: Vec::new(),
                workspace_marker: None,
                servers: Vec::new(),
                also: Vec::new(),
                source: Source::Settings,
            });
            langs.len() - 1
        });
        let l = &mut langs[i];
        let builtin = std::mem::take(&mut l.servers);
        let mut slots: Vec<Vec<ServerSpec>> = Vec::new();
        for e in entries {
            match e {
                Override::BuiltIn if !builtin.is_empty() => slots.push(builtin.clone()),
                Override::Server {
                    argv,
                    extensions,
                    root_markers,
                    init_options,
                    env,
                } => {
                    let Some((program, args)) = argv.split_first() else {
                        continue;
                    };
                    slots.push(vec![ServerSpec {
                        program: program.clone(),
                        args: args.to_vec(),
                        env: env.clone(),
                        init_options: init_options.clone(),
                        ..ServerSpec::default()
                    }]);
                    let lang_id = l
                        .extensions
                        .first()
                        .map_or_else(|| id.clone(), |(_, lid)| lid.clone());
                    let mut exts = extensions.clone();
                    if exts.is_empty() && l.extensions.is_empty() {
                        exts.push(id.clone());
                    }
                    for e in exts {
                        let e = e.trim_start_matches('.').to_ascii_lowercase();
                        if !l.extensions.iter().any(|(x, _)| *x == e) {
                            l.extensions.push((e, lang_id.clone()));
                        }
                    }
                    if !root_markers.is_empty() {
                        l.root_markers = root_markers.clone();
                        l.workspace_marker = None;
                    }
                }
                Override::BuiltIn | Override::Off | Override::Many(_) => {}
            }
        }
        if slots.is_empty() {
            // Nothing usable in the setting: keep what was there.
            l.servers = builtin;
            continue;
        }
        l.servers = slots.remove(0);
        l.also = slots;
        l.source = Source::Settings;
    }
    langs
}

/// `(language, LSP languageId)` for a file in `langs`, by extension.
pub fn language_in<'a>(langs: &'a [Language], path: &Path) -> Option<(&'a Language, &'a str)> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    langs.iter().find_map(|l| {
        l.extensions
            .iter()
            .find(|(e, _)| *e == ext)
            .map(|(_, id)| (l, id.as_str()))
    })
}

fn lang(id: &str, exts: &[(&str, &str)], markers: &[&str], servers: Vec<ServerSpec>) -> Language {
    Language {
        id: id.into(),
        extensions: exts
            .iter()
            .map(|(e, l)| (e.to_string(), l.to_string()))
            .collect(),
        root_markers: markers.iter().map(|s| s.to_string()).collect(),
        workspace_marker: None,
        servers,
        also: Vec::new(),
        source: Source::BuiltIn,
    }
}

/// Skink's built-in language set: Rust, TS/JS, Python, Go, Dart, Zig.
pub fn default_languages() -> Vec<Language> {
    let mut rust = lang(
        "rust",
        &[("rs", "rust")],
        &["Cargo.toml", "rust-project.json"],
        vec![ServerSpec::new("rust-analyzer", &[])],
    );
    rust.workspace_marker = Some(("Cargo.toml".into(), "[workspace]".into()));
    vec![
        rust,
        lang(
            "typescript",
            &[
                ("ts", "typescript"),
                ("mts", "typescript"),
                ("cts", "typescript"),
                ("tsx", "typescriptreact"),
                ("js", "javascript"),
                ("mjs", "javascript"),
                ("cjs", "javascript"),
                ("jsx", "javascriptreact"),
            ],
            &["tsconfig.json", "jsconfig.json", "package.json"],
            vec![ServerSpec::new("typescript-language-server", &["--stdio"])],
        ),
        lang(
            "python",
            &[("py", "python"), ("pyi", "python")],
            &[
                "pyproject.toml",
                "pyrightconfig.json",
                "setup.py",
                "setup.cfg",
                "requirements.txt",
            ],
            vec![
                ServerSpec::new("basedpyright-langserver", &["--stdio"]),
                ServerSpec::new("pyright-langserver", &["--stdio"]),
            ],
        ),
        lang(
            "go",
            &[("go", "go")],
            &["go.work", "go.mod"],
            vec![ServerSpec::new("gopls", &[])],
        ),
        lang(
            "dart",
            &[("dart", "dart")],
            &["pubspec.yaml", "analysis_options.yaml"],
            // The Dart SDK's own server (also what Flutter uses).
            vec![ServerSpec::new(
                "dart",
                &["language-server", "--protocol=lsp"],
            )],
        ),
        lang(
            "json",
            &[("json", "json"), ("jsonc", "jsonc")],
            &[],
            vec![ServerSpec::new("vscode-json-language-server", &["--stdio"])],
        ),
        lang(
            "zig",
            &[("zig", "zig"), ("zon", "zig")],
            &["build.zig", "build.zig.zon"],
            vec![ServerSpec::new("zls", &[])],
        ),
    ]
}

/// Locate `program` on `PATH` (honouring `PATHEXT` on Windows). A program
/// containing a path separator is checked directly.
pub fn find_in_path(program: &str) -> Option<PathBuf> {
    find_in(
        program,
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("PATHEXT").as_deref(),
    )
}

fn find_in(
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
    pathext: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        let raw = pathext
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
        let mut v: Vec<String> = raw
            .split(';')
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        v.insert(0, String::new()); // as typed first
        v
    } else {
        vec![String::new()]
    };
    let check = |dir: &Path| -> Option<PathBuf> {
        exts.iter().find_map(|ext| {
            let cand = dir.join(format!("{program}{ext}"));
            is_executable(&cand).then_some(cand)
        })
    };
    if program.contains(['/', '\\']) {
        return check(Path::new(""));
    }
    std::env::split_paths(path_var?).find_map(|d| check(&d))
}

fn is_executable(p: &Path) -> bool {
    let Ok(md) = std::fs::metadata(p) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

/// What the registry knows about the server for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    /// No language configured for this file type.
    Unsupported,
    /// A language is configured but none of its servers is on `PATH`.
    NotInstalled(String),
    /// Turned off with `"lsp.<lang>": false`.
    Disabled,
    /// Installed, not started yet (starts on first use).
    NotStarted,
    /// Running (server name).
    Ready(String),
    /// Spawn / handshake failed, or the server exited. Use
    /// [`ServerRegistry::restart`] to try again.
    Failed(String),
}

#[derive(Default)]
enum Slot {
    #[default]
    Empty,
    /// A thread is running the handshake; others wait on the cell's condvar.
    Starting,
    Ready(Arc<LspClient>),
    Failed(LspError),
}

/// One (language, root) server. The mutex is only ever held briefly (never
/// across a spawn / handshake), so status and cache reads from the UI thread
/// cannot stall behind a slow server start.
#[derive(Default)]
struct Cell {
    slot: Mutex<Slot>,
    ready: Condvar,
    usage: Mutex<Usage>,
}

/// Whether a running server has documents open (see [`ServerRegistry::stop_idle`]).
#[derive(Debug, Default, Clone, Copy)]
enum Usage {
    #[default]
    InUse,
    /// No documents open since this moment.
    IdleSince(std::time::Instant),
}

/// The next usage state of a running server with `open` documents, or
/// `None` when it has been idle for `after` and should stop.
fn next_usage(
    usage: Usage,
    open: usize,
    now: std::time::Instant,
    after: std::time::Duration,
) -> Option<Usage> {
    match usage {
        _ if open > 0 => Some(Usage::InUse),
        Usage::InUse => Some(Usage::IdleSince(now)),
        Usage::IdleSince(t) if now.saturating_duration_since(t) >= after => None,
        idle => Some(idle),
    }
}

/// Puts a cell back to `Empty` (and wakes waiters) if the starting thread
/// unwinds before publishing a result.
struct StartGuard<'a>(&'a Cell);

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        let mut slot = lock(&self.0.slot);
        if matches!(*slot, Slot::Starting) {
            *slot = Slot::Empty;
        }
        self.0.ready.notify_all();
    }
}

type Key = (String, PathBuf);

pub struct ServerRegistry {
    languages: Vec<Language>,
    default_root: PathBuf,
    slots: Mutex<HashMap<Key, Arc<Cell>>>,
    /// `root_for` per (language, directory): root detection touches the file
    /// system and runs on every lookup (diagnostics reads included).
    roots: Mutex<HashMap<Key, PathBuf>>,
    sink: EventSink,
    events: Mutex<Receiver<LspEvent>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ServerRegistry {
    /// Registry with the built-in languages. `default_root` is used for files
    /// outside any recognisable project and bounds the root search.
    pub fn new(default_root: impl Into<PathBuf>) -> Self {
        Self::with_languages(default_root, default_languages())
    }

    pub fn with_languages(default_root: impl Into<PathBuf>, languages: Vec<Language>) -> Self {
        let (sink, rx) = EventSink::new();
        Self {
            languages,
            default_root: {
                let r = default_root.into();
                std::path::absolute(&r).unwrap_or(r)
            },
            slots: Mutex::new(HashMap::new()),
            roots: Mutex::new(HashMap::new()),
            sink,
            events: Mutex::new(rx),
        }
    }

    /// Call `waker` after every event so an event loop can wake up and
    /// [`drain_events`](Self::drain_events). Must be set before servers start.
    pub fn set_waker(&mut self, waker: impl Fn() + Send + Sync + 'static) {
        self.sink = self.sink.clone().with_waker(waker);
    }

    /// Add or replace (same `id`) a language.
    pub fn register(&mut self, language: Language) {
        self.languages.retain(|l| l.id != language.id);
        self.languages.push(language);
    }

    /// `(language, LSP languageId)` for a file, by extension.
    pub fn language_for(&self, path: &Path) -> Option<(&Language, &str)> {
        language_in(&self.languages, path)
    }

    /// The LSP `languageId` for a file ("rust", "typescriptreact", ...).
    pub fn language_id(&self, path: &Path) -> Option<&str> {
        self.language_for(path).map(|(_, id)| id)
    }

    /// Project root used for `path` under `lang` (cached per directory).
    pub fn root_for(&self, lang: &Language, path: &Path) -> PathBuf {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let dir = path.parent().unwrap_or(&path).to_path_buf();
        let key = (lang.id.clone(), dir);
        if let Some(root) = lock(&self.roots).get(&key) {
            return root.clone();
        }
        let root = self.detect_root(lang, &path);
        lock(&self.roots).insert(key, root.clone());
        root
    }

    fn detect_root(&self, lang: &Language, path: &Path) -> PathBuf {
        let start = path.parent().unwrap_or(path);
        let bounded = path.starts_with(&self.default_root);
        let mut nearest = None;
        let mut outermost_ws = None;
        for dir in start.ancestors() {
            if nearest.is_none() && lang.root_markers.iter().any(|m| dir.join(m).exists()) {
                nearest = Some(dir.to_path_buf());
            }
            if let Some((file, needle)) = &lang.workspace_marker
                && std::fs::read_to_string(dir.join(file))
                    .is_ok_and(|t| t.contains(needle.as_str()))
            {
                outermost_ws = Some(dir.to_path_buf());
            }
            if bounded && dir == self.default_root {
                break;
            }
        }
        outermost_ws
            .or(nearest)
            .unwrap_or_else(|| self.default_root.clone())
    }

    fn installed_server(candidates: &[ServerSpec]) -> Option<ServerSpec> {
        candidates.iter().find_map(|s| {
            find_in_path(&s.program).map(|full| ServerSpec {
                program: full.to_string_lossy().into_owned(),
                ..s.clone()
            })
        })
    }

    /// The servers configured for `lang`: the primary's candidates, then
    /// each additional server's.
    fn slots(lang: &Language) -> impl Iterator<Item = &[ServerSpec]> {
        std::iter::once(lang.servers.as_slice()).chain(lang.also.iter().map(Vec::as_slice))
    }

    /// How many servers serve `path` (0 when none is configured).
    pub fn server_count(&self, path: &Path) -> usize {
        self.language_for(path)
            .map_or(0, |(l, _)| Self::slots(l).filter(|s| !s.is_empty()).count())
    }

    /// Registry key and candidates of server `slot` for `path`. Slot 0 keeps
    /// the plain language id (its key is unchanged from one-server days).
    fn slot_key(&self, path: &Path, slot: usize) -> Option<(&Language, Key, &[ServerSpec])> {
        let (lang, _) = self.language_for(path)?;
        let candidates = Self::slots(lang).nth(slot)?;
        let id = match slot {
            0 => lang.id.clone(),
            n => format!("{}#{n}", lang.id),
        };
        Some((lang, (id, self.root_for(lang, path)), candidates))
    }

    fn cell(&self, key: &Key) -> Option<Arc<Cell>> {
        lock(&self.slots).get(key).cloned()
    }

    fn not_installed(candidates: &[ServerSpec]) -> String {
        candidates
            .first()
            .map_or_else(String::new, |s| s.program.clone())
    }

    /// Status without starting anything. With several servers: the best of
    /// them (any ready one makes the file served), ready names joined by +.
    pub fn status(&self, path: &Path) -> ServerStatus {
        let all = self.statuses(path);
        let rank = |s: &ServerStatus| match s {
            ServerStatus::Ready(_) => 0,
            ServerStatus::NotStarted => 1,
            ServerStatus::Failed(_) => 2,
            ServerStatus::NotInstalled(_) => 3,
            ServerStatus::Disabled => 4,
            ServerStatus::Unsupported => 5,
        };
        let ready: Vec<&str> = all
            .iter()
            .filter_map(|s| match s {
                ServerStatus::Ready(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        if !ready.is_empty() {
            return ServerStatus::Ready(ready.join(" + "));
        }
        all.into_iter()
            .min_by_key(rank)
            .unwrap_or(ServerStatus::Unsupported)
    }

    /// The status of each server for `path`, primary first.
    pub fn statuses(&self, path: &Path) -> Vec<ServerStatus> {
        let n = self
            .language_for(path)
            .map_or(1, |(l, _)| Self::slots(l).count());
        (0..n).map(|i| self.slot_status(path, i)).collect()
    }

    fn slot_status(&self, path: &Path, slot: usize) -> ServerStatus {
        let Some((lang, key, candidates)) = self.slot_key(path, slot) else {
            return ServerStatus::Unsupported;
        };
        if lang.source == Source::Disabled {
            return ServerStatus::Disabled;
        }
        if let Some(cell) = self.cell(&key) {
            match &*lock(&cell.slot) {
                Slot::Ready(c) if c.is_alive() => return ServerStatus::Ready(c.name().to_string()),
                Slot::Ready(_) => return ServerStatus::Failed("server exited".into()),
                Slot::Failed(e) => return ServerStatus::Failed(e.to_string()),
                Slot::Starting => return ServerStatus::NotStarted,
                Slot::Empty => {}
            }
        }
        match Self::installed_server(candidates) {
            Some(_) => ServerStatus::NotStarted,
            None => ServerStatus::NotInstalled(Self::not_installed(candidates)),
        }
    }

    /// The running client for `path`, starting it if needed. The first call
    /// per (language, root) blocks for the handshake; call it from a worker
    /// thread (or use [`ensure_started`](Self::ensure_started)).
    pub fn client_for(&self, path: &Path) -> Result<Arc<LspClient>> {
        self.slot_client(path, 0)
    }

    /// Every server for `path`, primary first, starting those not running
    /// (blocks for their handshakes). One result per configured server.
    pub fn clients_for(&self, path: &Path) -> Vec<Result<Arc<LspClient>>> {
        let n = self
            .language_for(path)
            .map_or(1, |(l, _)| Self::slots(l).count());
        (0..n).map(|i| self.slot_client(path, i)).collect()
    }

    /// Server `slot` for `path`, started if needed.
    pub fn slot_client(&self, path: &Path, slot: usize) -> Result<Arc<LspClient>> {
        let no_server = || {
            LspError::NoServer(path.extension().map_or_else(
                || path.display().to_string(),
                |e| format!(".{}", e.to_string_lossy()),
            ))
        };
        let Some((lang, key, candidates)) = self.slot_key(path, slot) else {
            return Err(no_server());
        };
        if lang.source == Source::Disabled || candidates.is_empty() {
            return Err(no_server());
        }
        let root = key.1.clone();
        let cell = Arc::clone(lock(&self.slots).entry(key).or_default());
        {
            // Concurrent first users wait for the one doing the handshake.
            let mut slot = lock(&cell.slot);
            loop {
                match &*slot {
                    Slot::Ready(c) if c.is_alive() => return Ok(c.clone()),
                    Slot::Ready(_) => return Err(LspError::Disconnected),
                    Slot::Failed(e) => return Err(e.clone()),
                    Slot::Starting => {
                        slot = cell
                            .ready
                            .wait(slot)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    Slot::Empty => break,
                }
            }
            *slot = Slot::Starting;
        }
        let _guard = StartGuard(&cell);
        let result = match Self::installed_server(candidates) {
            None => Err(LspError::NotInstalled(Self::not_installed(candidates))),
            Some(mut spec) => {
                spec.cwd.get_or_insert_with(|| root.clone());
                LspClient::spawn_with(spec, &path_to_uri(&root), self.sink.clone()).map(Arc::new)
            }
        };
        *lock(&cell.slot) = match &result {
            Ok(c) => Slot::Ready(c.clone()),
            Err(e) => Slot::Failed(e.clone()),
        };
        result
    }

    /// Start (if needed) the server responsible for `path`.
    pub fn ensure_started(&self, path: &Path) -> Result<()> {
        self.client_for(path).map(|_| ())
    }

    /// The client for `path` only if it is already running.
    pub fn running_client(&self, path: &Path) -> Option<Arc<LspClient>> {
        self.running_slot(path, 0)
    }

    fn running_slot(&self, path: &Path, slot: usize) -> Option<Arc<LspClient>> {
        let (_, key, _) = self.slot_key(path, slot)?;
        match &*lock(&self.cell(&key)?.slot) {
            Slot::Ready(c) if c.is_alive() => Some(c.clone()),
            _ => None,
        }
    }

    /// Every running server for `path`, primary first.
    pub fn running_clients_for(&self, path: &Path) -> Vec<Arc<LspClient>> {
        let n = self
            .language_for(path)
            .map_or(1, |(l, _)| Self::slots(l).count());
        (0..n).filter_map(|i| self.running_slot(path, i)).collect()
    }

    /// Forget (and shut down) the server for `path` so the next use starts a
    /// fresh one; documents must be re-opened by the caller.
    /// Root detection is redone too (markers may have appeared).
    pub fn restart(&self, path: &Path) {
        let keys: Vec<Key> = (0..self.statuses(path).len())
            .filter_map(|i| self.slot_key(path, i).map(|(_, k, _)| k))
            .collect();
        lock(&self.roots).clear();
        for key in keys {
            let cell = lock(&self.slots).remove(&key);
            let client = cell.and_then(|cell| match &*lock(&cell.slot) {
                Slot::Ready(c) => Some(c.clone()),
                _ => None,
            });
            if let Some(c) = client {
                c.shutdown(); // outside the cell lock: this takes up to 3 s
            }
        }
    }

    /// Shut down servers that have had no open documents for `after`
    /// (checked at `now`); they start again on next use. Returns the names of
    /// the servers stopped. Shutdowns run on their own threads.
    pub fn stop_idle(&self, now: std::time::Instant, after: std::time::Duration) -> Vec<String> {
        let mut stop = Vec::new();
        let cells: Vec<(Key, Arc<Cell>)> = lock(&self.slots)
            .iter()
            .map(|(k, c)| (k.clone(), c.clone()))
            .collect();
        for (key, cell) in cells {
            let Slot::Ready(client) = &*lock(&cell.slot) else {
                continue;
            };
            let mut usage = lock(&cell.usage);
            match next_usage(*usage, client.open_count(), now, after) {
                Some(u) => *usage = u,
                None => stop.push((key, client.clone())),
            }
        }
        let mut names = Vec::new();
        for (key, client) in stop {
            lock(&self.slots).remove(&key);
            names.push(client.name().to_string());
            std::thread::spawn(move || client.shutdown());
        }
        names
    }

    /// Every server that is up (project-wide requests such as file renames).
    pub fn running_clients(&self) -> Vec<Arc<LspClient>> {
        let cells: Vec<Arc<Cell>> = lock(&self.slots).values().cloned().collect();
        cells
            .iter()
            .filter_map(|c| match &*lock(&c.slot) {
                Slot::Ready(c) if c.is_alive() => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    /// Process ids of the running servers.
    pub fn server_pids(&self) -> Vec<u32> {
        let cells: Vec<Arc<Cell>> = lock(&self.slots).values().cloned().collect();
        cells
            .iter()
            .filter_map(|c| match &*lock(&c.slot) {
                Slot::Ready(c) if c.is_alive() => c.pid(),
                _ => None,
            })
            .collect()
    }

    /// Take everything queued from all servers (non-blocking).
    pub fn drain_events(&self) -> Vec<LspEvent> {
        lock(&self.events).try_iter().collect()
    }

    /// Shut down every server in parallel. Called on drop; idempotent.
    pub fn shutdown_all(&self) {
        let cells: Vec<Arc<Cell>> = lock(&self.slots).values().cloned().collect();
        let clients: Vec<Arc<LspClient>> = cells
            .iter()
            .filter_map(|c| match &*lock(&c.slot) {
                Slot::Ready(c) => Some(c.clone()),
                _ => None,
            })
            .collect();
        std::thread::scope(|s| {
            for c in &clients {
                s.spawn(|| c.shutdown());
            }
        });
    }
}

impl Drop for ServerRegistry {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> ServerRegistry {
        ServerRegistry::new("/nonexistent-skink-root")
    }

    #[test]
    fn language_by_extension() {
        let r = reg();
        let id = |p: &str| r.language_id(Path::new(p)).map(str::to_owned);
        assert_eq!(id("a/b.rs").as_deref(), Some("rust"));
        assert_eq!(id("x.TSX").as_deref(), Some("typescriptreact"));
        assert_eq!(id("x.mjs").as_deref(), Some("javascript"));
        assert_eq!(id("x.py").as_deref(), Some("python"));
        assert_eq!(id("x.go").as_deref(), Some("go"));
        assert_eq!(id("x.zig").as_deref(), Some("zig"));
        assert_eq!(id("x.txt"), None);
        assert_eq!(id("Makefile"), None);
        assert_eq!(
            r.language_for(Path::new("a.ts")).unwrap().0.id,
            "typescript"
        );
        assert_eq!(
            r.language_for(Path::new("a.jsx")).unwrap().0.id,
            "typescript"
        );
    }

    #[test]
    fn default_server_commands() {
        let langs = default_languages();
        let cmd = |id: &str| -> Vec<(String, Vec<String>)> {
            langs
                .iter()
                .find(|l| l.id == id)
                .unwrap()
                .servers
                .iter()
                .map(|s| (s.program.clone(), s.args.clone()))
                .collect()
        };
        assert_eq!(cmd("rust"), [("rust-analyzer".into(), vec![])]);
        assert_eq!(
            cmd("typescript"),
            [("typescript-language-server".into(), vec!["--stdio".into()])]
        );
        assert_eq!(
            cmd("python"),
            [
                ("basedpyright-langserver".into(), vec!["--stdio".into()]),
                ("pyright-langserver".into(), vec!["--stdio".into()])
            ]
        );
        assert_eq!(cmd("go"), [("gopls".into(), vec![])]);
        assert_eq!(cmd("zig"), [("zls".into(), vec![])]);
        assert_eq!(
            cmd("dart"),
            [(
                "dart".into(),
                vec!["language-server".into(), "--protocol=lsp".into()]
            )]
        );
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("skink-lsp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn root_detection_nearest_and_workspace() {
        let d = tmpdir("root");
        let member = d.join("crates/a/src");
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(d.join("Cargo.toml"), "[workspace]\nmembers=[\"crates/a\"]").unwrap();
        std::fs::write(d.join("crates/a/Cargo.toml"), "[package]\nname=\"a\"").unwrap();
        let r = ServerRegistry::new(&d);
        let rust = r.language_for(Path::new("x.rs")).unwrap().0.clone();
        assert_eq!(
            r.root_for(&rust, &member.join("lib.rs")),
            d,
            "workspace root wins"
        );
        // no workspace marker: nearest Cargo.toml. Roots are cached per
        // directory; `restart` forgets them.
        std::fs::write(d.join("Cargo.toml"), "[package]\nname=\"top\"").unwrap();
        assert_eq!(r.root_for(&rust, &member.join("lib.rs")), d, "cached");
        r.restart(&member.join("lib.rs"));
        assert_eq!(
            r.root_for(&rust, &member.join("lib.rs")),
            d.join("crates/a")
        );
        // python without markers falls back to default root
        let py = r.language_for(Path::new("x.py")).unwrap().0.clone();
        assert_eq!(r.root_for(&py, &d.join("s.py")), d);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn find_in_path_checks_dirs_and_exec_bit() {
        let d = tmpdir("which");
        let name = if cfg!(windows) {
            "fake-ls.exe"
        } else {
            "fake-ls"
        };
        let f = d.join(name);
        std::fs::write(&f, "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // not executable yet
            let mut p = std::fs::metadata(&f).unwrap().permissions();
            p.set_mode(0o644);
            std::fs::set_permissions(&f, p.clone()).unwrap();
            let var = std::env::join_paths([&d]).unwrap();
            assert_eq!(find_in("fake-ls", Some(&var), None), None);
            p.set_mode(0o755);
            std::fs::set_permissions(&f, p).unwrap();
        }
        let var = std::env::join_paths([Path::new("/nonexistent-dir"), &d]).unwrap();
        assert_eq!(find_in("fake-ls", Some(&var), None), Some(f.clone()));
        assert_eq!(find_in("nope-ls", Some(&var), None), None);
        assert_eq!(find_in("fake-ls", None, None), None);
        assert_eq!(find_in(f.to_str().unwrap(), None, None), Some(f));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn settings_override_add_and_disable_servers() {
        let ov = |argv: &[&str], exts: &[&str]| Override::Server {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            extensions: exts.iter().map(|s| s.to_string()).collect(),
            root_markers: vec![],
            init_options: None,
            env: vec![],
        };
        let langs = merge_overrides(
            default_languages(),
            &[
                ("dart".into(), ov(&["fvm", "dart", "language-server"], &[])),
                ("elixir".into(), ov(&["elixir-ls"], &["ex", ".exs"])),
                ("javascript".into(), Override::Off),
            ],
        );
        let (dart, id) = language_in(&langs, Path::new("a.dart")).unwrap();
        assert_eq!((dart.source, id), (Source::Settings, "dart"));
        assert_eq!(dart.servers[0].program, "fvm");
        assert_eq!(dart.servers[0].args, ["dart", "language-server"]);
        let (ex, id) = language_in(&langs, Path::new("m.exs")).unwrap();
        assert_eq!((ex.id.as_str(), id), ("elixir", "elixir"));
        let r = ServerRegistry::with_languages("/nonexistent", langs);
        assert_eq!(r.status(Path::new("a.ts")), ServerStatus::Disabled);
        assert!(r.client_for(Path::new("a.js")).is_err());
        // A new language with no extensions given serves files named after it.
        let lua = merge_overrides(vec![], &[("lua".into(), ov(&["lua-ls"], &[]))]);
        assert!(language_in(&lua, Path::new("x.lua")).is_some());
    }

    #[test]
    fn status_unsupported_and_not_installed() {
        let mut r = reg();
        assert_eq!(r.status(Path::new("a.txt")), ServerStatus::Unsupported);
        r.register(Language {
            id: "nolang".into(),
            extensions: vec![("nl".into(), "nolang".into())],
            root_markers: vec![],
            workspace_marker: None,
            servers: vec![ServerSpec::new("skink-definitely-not-installed-ls", &[])],
            also: Vec::new(),
            source: Source::BuiltIn,
        });
        assert_eq!(
            r.status(Path::new("a.nl")),
            ServerStatus::NotInstalled("skink-definitely-not-installed-ls".into())
        );
        assert!(matches!(
            r.client_for(Path::new("a.nl")),
            Err(LspError::NotInstalled(_))
        ));
        // cached failure
        assert!(matches!(
            r.status(Path::new("a.nl")),
            ServerStatus::Failed(_)
        ));
        assert!(matches!(
            r.client_for(Path::new("a.txt")),
            Err(LspError::NoServer(_))
        ));
        // restart clears the cached failure
        r.restart(Path::new("a.nl"));
        assert!(matches!(
            r.status(Path::new("a.nl")),
            ServerStatus::NotInstalled(_)
        ));
    }
}

#[cfg(test)]
mod idle_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn servers_stop_only_after_a_full_idle_period() {
        let t0 = Instant::now();
        let after = Duration::from_secs(600);
        // Documents open: stays in use.
        assert!(matches!(
            next_usage(Usage::InUse, 2, t0, after),
            Some(Usage::InUse)
        ));
        // Last document closed: the idle clock starts, nothing stops yet.
        let idle = next_usage(Usage::InUse, 0, t0, after).unwrap();
        assert!(matches!(idle, Usage::IdleSince(t) if t == t0));
        assert!(next_usage(idle, 0, t0 + Duration::from_secs(599), after).is_some());
        // A document reopened resets it.
        assert!(matches!(
            next_usage(idle, 1, t0 + after, after),
            Some(Usage::InUse)
        ));
        // Idle for the whole period: stop.
        assert!(next_usage(idle, 0, t0 + after, after).is_none());
    }
}
