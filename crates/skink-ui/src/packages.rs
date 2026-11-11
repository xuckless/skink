//! Packages: grammars (tree-sitter WASM), language servers, the tools they
//! need, themes, fonts and Lua scripts, installed at run time.
//!
//! A package is a folder with a `skink-package.toml`. It comes from a
//! verified short name (`kotlin`, looked up in `registry/verified.json` and
//! the docs site's index.json), a GitHub release (`owner/repo[/package][@tag]`:
//! the asset `<package>.skink.tar.gz` and its `.sha256`, downloaded like
//! `skink --upgrade` with the direct link then the API), or a local folder.
//!
//! Everything lands under `<data>`: `packages/<name>` (the package as
//! shipped), `tools/<name>` (downloaded servers, on the PATH of language
//! servers and terminal tabs only) and `installed.json`. Fonts go to the
//! user's font folder and are listed in `installed.json`, so uninstalling
//! removes everything the package placed.

use crate::update::{self, Downloader};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// The docs site's package index (`SKINK_INDEX_URL` overrides it).
pub const INDEX_URL: &str = "https://xuckless.github.io/skink/packages/index.json";
pub const MANIFEST: &str = "skink-package.toml";
const DB: &str = "installed.json";
const VERIFIED: &str = include_str!("../../../registry/verified.json");
/// `--install .` looks at this many files at most.
const SCAN_CAP: usize = 20_000;

// ---- the manifest -----------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub license: String,
    pub grammars: Vec<GrammarDef>,
    pub lsp: Vec<LspDef>,
    pub tools: Vec<ToolDef>,
    /// Theme files (`.json`, `.micro`), relative to the package.
    pub themes: Vec<String>,
    pub fonts: Vec<FontDef>,
    /// Lua files run like `init.lua`, after the trust prompt.
    pub scripts: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GrammarDef {
    pub name: String,
    pub wasm: String,
    pub highlights: String,
    pub injections: Option<String>,
    pub extensions: Vec<String>,
    pub file_names: Vec<String>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LspDef {
    pub language: String,
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    pub root_markers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Download {
    pub url: String,
    pub sha256: String,
    /// A tool's executable on this target when it differs (`.bat` on Windows).
    pub bin: Option<String>,
}

/// A program downloaded per platform (`targets` keys: `x86_64-linux`,
/// `aarch64-macos`, `x86_64-windows`, ...).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    /// The executable inside the download (`bin/zls`); `.exe` is added on Windows.
    pub bin: String,
    pub targets: BTreeMap<String, Download>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FontDef {
    pub name: String,
    pub download: Download,
    /// File name patterns with one `*` (`*.ttf`).
    pub files: Vec<String>,
}

fn text(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
}

fn texts(v: &Value, k: &str) -> Vec<String> {
    v[k].as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn tables<'v>(v: &'v Value, k: &str) -> &'v [Value] {
    v[k].as_array().map_or(&[], Vec::as_slice)
}

fn download(v: &Value, what: &str) -> Result<Download, String> {
    let d = Download {
        url: text(v, "url"),
        sha256: text(v, "sha256").to_ascii_lowercase(),
        bin: v["bin"].as_str().map(str::to_owned),
    };
    if d.url.is_empty() || d.sha256.len() != 64 {
        return Err(format!("{what}: needs url and a sha256 (64 hex digits)"));
    }
    Ok(d)
}

/// A package-relative path that stays inside the package.
fn inside(p: &str) -> Result<PathBuf, String> {
    let path = Path::new(p);
    let ok = !p.is_empty()
        && !p.contains('\\')
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if ok {
        Ok(path.to_path_buf())
    } else {
        Err(format!("{p}: paths must stay inside the package"))
    }
}

/// Lower-case letters, digits and inner `-`, at most 64 long.
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && !n.starts_with('-')
        && !n.ends_with('-')
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Manifest {
    pub fn parse(src: &str) -> Result<Self, String> {
        let v = Value::Object(crate::minitoml::parse(src)?);
        let p = &v["package"];
        let m = Manifest {
            name: text(p, "name"),
            version: text(p, "version"),
            description: text(p, "description"),
            license: text(p, "license"),
            grammars: tables(&v, "grammar")
                .iter()
                .map(|g| GrammarDef {
                    name: text(g, "name"),
                    wasm: text(g, "wasm"),
                    highlights: text(g, "highlights"),
                    injections: g["injections"].as_str().map(str::to_owned),
                    extensions: texts(g, "extensions"),
                    file_names: texts(g, "file-names"),
                    aliases: texts(g, "aliases"),
                })
                .collect(),
            lsp: tables(&v, "lsp")
                .iter()
                .map(|l| LspDef {
                    language: text(l, "language"),
                    command: texts(l, "command"),
                    extensions: texts(l, "extensions"),
                    root_markers: texts(l, "root-markers"),
                })
                .collect(),
            tools: tables(&v, "tool")
                .iter()
                .map(|t| {
                    let name = text(t, "name");
                    let mut targets = BTreeMap::new();
                    for (k, d) in t["targets"].as_object().into_iter().flatten() {
                        targets.insert(k.clone(), download(d, &format!("tool {name} {k}"))?);
                    }
                    Ok(ToolDef {
                        bin: text(t, "bin"),
                        name,
                        targets,
                    })
                })
                .collect::<Result<_, String>>()?,
            themes: tables(&v, "theme")
                .iter()
                .map(|t| text(t, "path"))
                .collect(),
            fonts: tables(&v, "font")
                .iter()
                .map(|f| {
                    let name = text(f, "name");
                    Ok(FontDef {
                        download: download(f, &format!("font {name}"))?,
                        files: texts(f, "files"),
                        name,
                    })
                })
                .collect::<Result<_, String>>()?,
            scripts: tables(&v, "script")
                .iter()
                .map(|s| text(s, "path"))
                .collect(),
        };
        if !valid_name(&m.name) {
            return Err(format!(
                "package.name {:?}: lower-case letters, digits and -",
                m.name
            ));
        }
        if m.version.is_empty() {
            return Err("package.version is missing".into());
        }
        if m.version.len() > 32 || !m.version.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(format!(
                "package.version {:?}: at most 32 visible characters, like 1.2.0",
                m.version.chars().take(40).collect::<String>()
            ));
        }
        Ok(m)
    }

    /// Read and check the package in `dir`: every file it names is there.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let src = std::fs::read_to_string(dir.join(MANIFEST))
            .map_err(|e| format!("{}: {e}", dir.join(MANIFEST).display()))?;
        let m = Self::parse(&src).map_err(|e| format!("{MANIFEST}: {e}"))?;
        let file = |p: &str| -> Result<PathBuf, String> {
            let rel = inside(p)?;
            let f = dir.join(&rel);
            // A symlink anywhere on the way is refused: installs copy real
            // files only, so it would point outside or install broken.
            let mut at = dir.to_path_buf();
            for c in rel.components() {
                at.push(c);
                if at
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_symlink())
                {
                    return Err(format!("{p}: symlinks are not allowed in a package"));
                }
            }
            if f.is_file() {
                Ok(f)
            } else {
                Err(format!("{p} is missing from the package"))
            }
        };
        for g in &m.grammars {
            // The grammar's C name: tree-sitter loads `tree_sitter_<name>`.
            let c_name = !g.name.is_empty()
                && g.name.len() <= 64
                && g.name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !c_name {
                return Err(format!(
                    "grammar name {:?}: lower-case letters, digits and _ (its C name)",
                    g.name
                ));
            }
            // Only the magic number: this runs at start-up.
            let mut magic = [0u8; 4];
            let _ = std::fs::File::open(file(&g.wasm)?)
                .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic));
            if magic != *b"\0asm" {
                return Err(format!("{} is not a WebAssembly file", g.wasm));
            }
            file(&g.highlights)?;
            if let Some(i) = &g.injections {
                file(i)?;
            }
        }
        for l in &m.lsp {
            if l.language.is_empty() || l.command.is_empty() {
                return Err("[[lsp]] needs language and command".into());
            }
        }
        for t in &m.tools {
            if t.name.is_empty() {
                return Err("[[tool]] needs a name".into());
            }
            inside(&t.bin)?;
            for d in t.targets.values() {
                if let Some(b) = &d.bin {
                    inside(b)?;
                }
            }
        }
        for p in m.themes.iter().chain(&m.scripts) {
            file(p)?;
        }
        Ok(m)
    }
}

// ---- what is installed ------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PkgState {
    Installed,
    Disabled,
    /// Its files are missing or no longer check out.
    Broken(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub name: String,
    pub version: String,
    /// What to install it from again (`--update-packages`).
    pub source: String,
    pub state: PkgState,
    /// Files placed outside `<data>` (fonts).
    pub placed: Vec<PathBuf>,
    /// Folders with its tools, for PATH.
    pub tool_dirs: Vec<PathBuf>,
}

// ---- where a package comes from ---------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    /// A verified short name.
    Name(String),
    GitHub {
        owner: String,
        repo: String,
        /// The package inside a multi-package repo (default: the repo name).
        package: Option<String>,
        tag: Option<String>,
    },
    Dir(PathBuf),
}

fn gh_part(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

impl Spec {
    pub fn parse(s: &str) -> Result<Spec, String> {
        let s = s.trim();
        let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        if let Some(rest) = s.strip_prefix("~/")
            && let Some(h) = home()
        {
            return Ok(Spec::Dir(PathBuf::from(h).join(rest)));
        }
        let p = Path::new(s);
        if s.starts_with('.') || p.is_absolute() || s.contains('\\') || p.is_dir() {
            return Ok(Spec::Dir(p.to_path_buf()));
        }
        if s.contains('/') {
            let (base, tag) = match s.split_once('@') {
                Some((b, t)) if !t.is_empty() => (b, Some(t.to_owned())),
                Some(_) => return Err(format!("{s}: empty tag after @")),
                None => (s, None),
            };
            let parts: Vec<&str> = base.split('/').collect();
            return match parts[..] {
                [o, r] if gh_part(o) && gh_part(r) => Ok(Spec::GitHub {
                    owner: o.into(),
                    repo: r.into(),
                    package: None,
                    tag,
                }),
                [o, r, p] if gh_part(o) && gh_part(r) && valid_name(p) => Ok(Spec::GitHub {
                    owner: o.into(),
                    repo: r.into(),
                    package: Some(p.into()),
                    tag,
                }),
                _ => Err(format!(
                    "{s}: expected NAME, owner/repo[/package][@tag] or a folder"
                )),
            };
        }
        if valid_name(s) {
            return Ok(Spec::Name(s.to_owned()));
        }
        let near = s.to_ascii_lowercase().replace(['_', ' ', '.'], "-");
        if valid_name(&near) {
            return Err(format!(
                "{s}: package names are lower-case letters, digits and - (did you mean {near}?)"
            ));
        }
        Err(format!(
            "{s}: package names are lower-case letters, digits and -"
        ))
    }
}

impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Spec::Name(n) => f.write_str(n),
            Spec::GitHub {
                owner,
                repo,
                package,
                tag,
            } => {
                write!(f, "{owner}/{repo}")?;
                if let Some(p) = package {
                    write!(f, "/{p}")?;
                }
                if let Some(t) = tag {
                    write!(f, "@{t}")?;
                }
                Ok(())
            }
            Spec::Dir(d) => write!(f, "{}", d.display()),
        }
    }
}

// ---- the registry -----------------------------------------------------------

/// One package in `verified.json` or index.json.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// `owner/repo`.
    pub repo: String,
    pub package: Option<String>,
    pub description: String,
    pub version: String,
    pub verified: bool,
    pub extensions: Vec<String>,
    pub file_names: Vec<String>,
}

impl Entry {
    fn from_json(name: &str, v: &Value, verified: bool) -> Option<Self> {
        let repo = text(v, "repo");
        let (o, r) = repo.split_once('/')?;
        if !(valid_name(name) && gh_part(o) && gh_part(r)) {
            return None;
        }
        Some(Entry {
            name: name.to_owned(),
            package: v["package"].as_str().map(str::to_owned),
            description: text(v, "description"),
            version: text(v, "version"),
            verified: verified || v["verified"].as_bool() == Some(true),
            extensions: texts(v, "extensions"),
            file_names: texts(v, "file_names"),
            repo,
        })
    }

    pub fn spec(&self) -> Spec {
        let (o, r) = self.repo.split_once('/').unwrap_or_default();
        Spec::GitHub {
            owner: o.into(),
            repo: r.into(),
            package: self.package.clone(),
            tag: None,
        }
    }
}

/// `registry/verified.json`, built in.
pub fn verified() -> &'static [Entry] {
    static V: std::sync::OnceLock<Vec<Entry>> = std::sync::OnceLock::new();
    V.get_or_init(|| parse_verified(VERIFIED))
}

pub fn parse_verified(text: &str) -> Vec<Entry> {
    let v: Value = serde_json::from_str(text).unwrap_or_default();
    v["packages"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(n, e)| Entry::from_json(n, e, true))
        .collect()
}

/// index.json: `{"schema": 1, "packages": [{"name", "repo", ...}]}`.
pub fn parse_index(src: &str) -> Result<Vec<Entry>, String> {
    let v: Value = serde_json::from_str(src).map_err(|e| format!("index.json: {e}"))?;
    if v["schema"].as_u64() != Some(1) {
        return Err("index.json: unknown schema (a newer Skink reads it)".into());
    }
    Ok(tables(&v, "packages")
        .iter()
        .filter_map(|e| Entry::from_json(&text(e, "name"), e, false))
        .collect())
}

/// A name with the look-alike differences removed: case, `-` and `_`, and
/// digits that pass for letters.
pub fn fold(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '-' | '_' | '.' | ' '))
        .map(|c| match c.to_ascii_lowercase() {
            '0' => 'o',
            '1' | 'i' => 'l',
            '3' => 'e',
            '4' => 'a',
            '5' => 's',
            '7' => 't',
            '8' => 'b',
            '9' => 'g',
            c => c,
        })
        .collect::<String>()
        .replace("rn", "m")
        .replace("vv", "w")
}

/// The verified package `name` imitates, if it is not that package itself.
pub fn lookalike_of<'e>(name: &str, verified: &'e [Entry]) -> Option<&'e Entry> {
    let f = fold(name);
    verified
        .iter()
        .find(|e| e.name != name && fold(&e.name) == f)
}

// ---- an install -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Resolving,
    Downloading,
    Verifying,
    Installing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    New,
    Replaced {
        from: String,
    },
    /// `--update-packages`: the same version is installed.
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub name: String,
    pub version: String,
    pub change: Change,
    /// Things to tell the user (a tool with no download for this platform).
    pub notes: Vec<String>,
}

/// One install, step by step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Resolving,
    Downloading(String),
    Verifying,
    Installing,
    Done(Done),
    Failed { step: Step, reason: String },
}

impl fmt::Display for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Job::Resolving => f.write_str("resolving"),
            Job::Downloading(what) => write!(f, "downloading {what}"),
            Job::Verifying => f.write_str("verifying"),
            Job::Installing => f.write_str("installing"),
            Job::Done(d) => match &d.change {
                Change::New => write!(f, "installed {} {}", d.name, d.version),
                Change::Replaced { from } if *from == d.version => {
                    write!(f, "reinstalled {} {}", d.name, d.version)
                }
                Change::Replaced { from } => {
                    write!(f, "updated {} {from} -> {}", d.name, d.version)
                }
                Change::Unchanged => write!(f, "{} {} is up to date", d.name, d.version),
            },
            Job::Failed { step, reason } => {
                let at = match step {
                    Step::Resolving => "finding it",
                    Step::Downloading => "downloading",
                    Step::Verifying => "checking it",
                    Step::Installing => "installing",
                };
                write!(f, "failed while {at}: {reason}")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Always,
    /// Only when the source has another version (`--update-packages`).
    IfChanged,
}

/// What a resolved spec downloads.
enum Fetch {
    Dir(PathBuf),
    Release { dl: Downloader, asset: String },
}

/// A scratch folder under `<data>`, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(data: &Path) -> Result<Self, String> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let p = data.join(format!(".tmp-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        Ok(Scratch(p))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `x86_64-linux`, `aarch64-macos`, `x86_64-windows`.
pub fn host_target() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn exe(name: &str) -> String {
    if cfg!(windows) && Path::new(name).extension().is_none() {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let name = e.file_name();
        if name == ".git" {
            continue;
        }
        let t = e.file_type()?;
        if t.is_dir() {
            copy_dir(&e.path(), &to.join(&name))?;
        } else if t.is_file() {
            std::fs::copy(e.path(), to.join(&name))?;
        }
    }
    Ok(())
}

/// Unpack by extension: `.tar.gz`/`.tgz`, `.tar.xz`, `.zip`; anything else is
/// a single file (a bare executable) and is moved in as `raw_name`.
fn unpack(archive: &Path, into: &Path, raw_name: &str) -> Result<(), String> {
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
    let run = |cmd: &mut std::process::Command| {
        cmd.stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| e.to_string())
            .and_then(|o| {
                if o.status.success() {
                    Ok(())
                } else {
                    Err(String::from_utf8_lossy(&o.stderr).trim().to_owned())
                }
            })
    };
    let tar = |flag: &str| {
        run(std::process::Command::new("tar")
            .arg(flag)
            .arg(archive)
            .arg("-C")
            .arg(into))
    };
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        tar("-xzf")
    } else if name.ends_with(".tar.xz") {
        tar("-xJf")
    } else if name.ends_with(".zip") {
        // bsdtar (Windows 10+, macOS) reads zip; GNU tar does not.
        tar("-xf").or_else(|e| {
            if update::have("unzip") {
                run(std::process::Command::new("unzip")
                    .arg("-qo")
                    .arg(archive)
                    .arg("-d")
                    .arg(into))
            } else {
                Err(e)
            }
        })
    } else {
        let to = into.join(raw_name);
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        std::fs::copy(archive, &to)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// A file name against a pattern with at most one `*`.
fn glob(pat: &str, name: &str) -> bool {
    match pat.split_once('*') {
        Some((a, b)) => name.len() >= a.len() + b.len() && name.starts_with(a) && name.ends_with(b),
        None => pat == name,
    }
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => files_under(&p, out),
            Ok(t) if t.is_file() => out.push(p),
            _ => {}
        }
    }
}

/// Where packages live and come from.
pub struct Store {
    pub data: PathBuf,
    pub fonts: Option<PathBuf>,
    pub index_url: String,
    /// `https://github.com` (tests: a loopback server).
    pub github: String,
    /// `https://api.github.com`, the fallback when github.com fails.
    pub api: Option<String>,
    pub target: String,
}

impl Store {
    pub fn open() -> Option<Self> {
        Some(Self::at(skink_config::data_dir()?))
    }

    /// A store in `data` (the editor passes its own; tests a temp dir).
    pub fn at(data: PathBuf) -> Self {
        // Tests never reach the network.
        let index_url = match std::env::var("SKINK_INDEX_URL") {
            _ if cfg!(test) => String::new(),
            Ok(u) if !u.is_empty() => u,
            _ => INDEX_URL.to_owned(),
        };
        Store {
            data,
            // Tests never touch the real font folder.
            fonts: if cfg!(test) {
                None
            } else {
                skink_config::font_dir()
            },
            index_url,
            github: "https://github.com".into(),
            api: Some("https://api.github.com".into()),
            target: host_target(),
        }
    }

    pub fn pkg_dir(&self, name: &str) -> PathBuf {
        self.data.join("packages").join(name)
    }

    fn tool_dir(&self, name: &str) -> PathBuf {
        self.data.join("tools").join(name)
    }

    fn db(&self) -> Map<String, Value> {
        let read = std::fs::read_to_string(self.data.join(DB));
        let parsed = read
            .as_ref()
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|v| v["packages"].as_object().cloned());
        let mut m = match (read, parsed) {
            (_, Some(m)) => m,
            (Err(_), None) => Map::new(),
            // Damaged: rebuild from the package folders, so --list stays
            // truthful and the next save does not forget them.
            (Ok(_), None) => std::fs::read_dir(self.data.join("packages"))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let m = Manifest::load(&e.path()).ok()?;
                    Some((
                        m.name.clone(),
                        json!({"version": m.version, "source": m.name}),
                    ))
                })
                .collect(),
        };
        // Names become paths (uninstall removes them): nothing else is kept.
        m.retain(|k, v| valid_name(k) && v.is_object());
        m
    }

    fn save_db(&self, m: Map<String, Value>) -> Result<(), String> {
        let body = serde_json::to_string_pretty(&json!({ "packages": m })).unwrap_or_default();
        std::fs::create_dir_all(&self.data).map_err(|e| e.to_string())?;
        let tmp = self.data.join(format!("{DB}.tmp"));
        std::fs::write(&tmp, body)
            .and_then(|()| std::fs::rename(&tmp, self.data.join(DB)))
            .map_err(|e| format!("{DB}: {e}"))
    }

    /// Every installed package, checked against its files.
    pub fn installed(&self) -> Vec<Installed> {
        self.db()
            .iter()
            .map(|(name, v)| {
                let paths = |k| texts(v, k).into_iter().map(PathBuf::from).collect();
                let state = match Manifest::load(&self.pkg_dir(name)) {
                    Err(e) => PkgState::Broken(e),
                    Ok(_) if text(v, "state") == "disabled" => PkgState::Disabled,
                    Ok(_) => PkgState::Installed,
                };
                Installed {
                    name: name.clone(),
                    version: text(v, "version"),
                    source: text(v, "source"),
                    state,
                    placed: paths("placed"),
                    tool_dirs: paths("path"),
                }
            })
            .collect()
    }

    pub fn is_installed(&self, name: &str) -> bool {
        self.db().contains_key(name)
    }

    /// Enable or disable an installed package (it stays on disk).
    pub fn set_enabled(&self, name: &str, on: bool) -> Result<(), String> {
        let mut db = self.db();
        let Some(Value::Object(e)) = db.get_mut(name) else {
            return Err(format!("{name} is not installed"));
        };
        e.insert(
            "state".into(),
            (if on { "installed" } else { "disabled" }).into(),
        );
        self.save_db(db)
    }

    /// Remove the package and everything it placed.
    pub fn uninstall(&self, name: &str) -> Result<(), String> {
        let mut db = self.db();
        let entry = db.remove(name);
        let dir = self.pkg_dir(name);
        if !valid_name(name) || (entry.is_none() && !dir.exists()) {
            return Err(format!("{name} is not installed"));
        }
        // The record first: if it cannot be written, nothing is removed.
        self.save_db(db)?;
        for f in entry
            .as_ref()
            .map(|v| texts(v, "placed"))
            .unwrap_or_default()
        {
            let _ = std::fs::remove_file(f);
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(self.tool_dir(name));
        Ok(())
    }

    /// The index from the docs site.
    pub fn index(&self) -> Result<Vec<Entry>, String> {
        let scratch = Scratch::new(&self.data)?;
        let (base, file) = self
            .index_url
            .rsplit_once('/')
            .ok_or("bad SKINK_INDEX_URL")?;
        let dest = scratch.0.join("index.json");
        plain(base).get(file, &dest).map_err(|e| e.to_string())?;
        parse_index(&std::fs::read_to_string(&dest).map_err(|e| e.to_string())?)
    }

    /// Verified packages: the built-in list, then the index's newer ones.
    fn registry(&self, with_index: bool) -> Vec<Entry> {
        let mut all = verified().to_vec();
        if with_index {
            for e in self.index().unwrap_or_default() {
                if e.verified && !all.iter().any(|v| v.name == e.name) {
                    all.push(e);
                }
            }
        }
        all
    }

    fn resolve(&self, name: &str) -> Result<Entry, String> {
        let find = |reg: &[Entry]| reg.iter().find(|e| e.name == name).cloned();
        if let Some(e) = find(verified()) {
            return Ok(e);
        }
        let reg = self.registry(true);
        if let Some(e) = find(&reg) {
            return Ok(e);
        }
        if let Some(e) = lookalike_of(name, &reg) {
            return Err(format!(
                "{name} is not a verified package (did you mean {}?)",
                e.name
            ));
        }
        Err(format!(
            "no verified package named {name}; --search finds others, install them as owner/repo"
        ))
    }

    fn fetch_for(&self, spec: &Spec) -> Result<(Fetch, Option<String>), String> {
        match spec {
            Spec::Dir(d) => {
                if !d.join(MANIFEST).is_file() {
                    return Err(format!("{} has no {MANIFEST}", d.display()));
                }
                Ok((Fetch::Dir(d.clone()), None))
            }
            Spec::Name(n) => {
                let e = self.resolve(n)?;
                Ok((self.fetch_for(&e.spec())?.0, Some(n.clone())))
            }
            Spec::GitHub {
                owner,
                repo,
                package,
                tag,
            } => {
                let (direct, release) = match tag {
                    Some(t) => (
                        format!("{}/{owner}/{repo}/releases/download/{t}", self.github),
                        format!("releases/tags/{t}"),
                    ),
                    None => (
                        format!("{}/{owner}/{repo}/releases/latest/download", self.github),
                        "releases/latest".to_owned(),
                    ),
                };
                let name = package.clone().unwrap_or_else(|| repo.clone());
                let dl = Downloader {
                    direct,
                    direct_name: "github.com".into(),
                    api: self
                        .api
                        .as_ref()
                        .map(|a| format!("{a}/repos/{owner}/{repo}")),
                    release,
                    hint_release: tag.as_ref().map_or_else(String::new, |t| format!("{t} ")),
                    repo: format!("{owner}/{repo}"),
                    backoff: std::time::Duration::from_secs(2),
                };
                Ok((
                    Fetch::Release {
                        dl,
                        asset: format!("{name}.skink.tar.gz"),
                    },
                    package.clone(),
                ))
            }
        }
    }

    /// Install (or update) one package. `on` sees every step.
    pub fn install(&self, spec: &Spec, when: When, on: &mut dyn FnMut(&Job)) -> Job {
        let job = self.run(spec, when, on);
        on(&job);
        job
    }

    fn run(&self, spec: &Spec, when: When, on: &mut dyn FnMut(&Job)) -> Job {
        let fail = |step, reason: String| Job::Failed { step, reason };
        on(&Job::Resolving);
        let (fetch, want_name) = match self.fetch_for(spec) {
            Ok(f) => f,
            Err(e) => return fail(Step::Resolving, e),
        };
        let scratch = match Scratch::new(&self.data) {
            Ok(s) => s,
            Err(e) => return fail(Step::Downloading, e),
        };
        let dir = match fetch {
            Fetch::Dir(d) => {
                on(&Job::Verifying);
                d
            }
            Fetch::Release { dl, asset } => {
                on(&Job::Downloading(asset.clone()));
                let archive = scratch.0.join(&asset);
                let sum = scratch.0.join(format!("{asset}.sha256"));
                if let Err(e) = dl
                    .get(&asset, &archive)
                    .and_then(|()| dl.get(&format!("{asset}.sha256"), &sum))
                {
                    return fail(Step::Downloading, e.to_string());
                }
                on(&Job::Verifying);
                let sum_text = std::fs::read_to_string(&sum).unwrap_or_default();
                if let Err(e) = update::verify(&archive, &sum_text) {
                    return fail(Step::Verifying, e.to_string());
                }
                let out = scratch.0.join("pkg");
                if let Err(e) = unpack(&archive, &out, "") {
                    return fail(Step::Verifying, format!("could not unpack {asset}: {e}"));
                }
                // The manifest at the top, or in the one folder there.
                let mut dirs = std::fs::read_dir(&out).into_iter().flatten().flatten();
                match (out.join(MANIFEST).is_file(), dirs.next(), dirs.next()) {
                    (true, ..) => out,
                    (false, Some(only), None) if only.path().join(MANIFEST).is_file() => {
                        only.path()
                    }
                    _ => return fail(Step::Verifying, format!("{asset} has no {MANIFEST}")),
                }
            }
        };
        let m = match Manifest::load(&dir) {
            Ok(m) => m,
            Err(e) => return fail(Step::Verifying, e),
        };
        if let Some(n) = want_name.filter(|n| *n != m.name) {
            return fail(
                Step::Verifying,
                format!("asked for {n}, the download is {}", m.name),
            );
        }
        if let Err(e) = self.check_grammars(&dir, &m) {
            return fail(Step::Verifying, e);
        }
        let before = self.db().get(&m.name).map(|v| text(v, "version"));
        if when == When::IfChanged && before.as_deref() == Some(m.version.as_str()) {
            return Job::Done(Done {
                name: m.name,
                version: before.unwrap_or_default(),
                change: Change::Unchanged,
                notes: Vec::new(),
            });
        }
        on(&Job::Installing);
        let source = match spec {
            Spec::Dir(d) => crate::doc::canonical(d).display().to_string(),
            s => s.to_string(),
        };
        match self.place(&dir, &m, &source, &scratch.0) {
            Ok(notes) => Job::Done(Done {
                name: m.name,
                version: m.version,
                change: before.map_or(Change::New, |from| Change::Replaced { from }),
                notes,
            }),
            Err(e) => fail(Step::Installing, e),
        }
    }

    /// Every grammar loads and its queries compile (a broken one would only
    /// leave files uncoloured, with no word why), and no other installed
    /// package already provides a grammar of that name.
    fn check_grammars(&self, dir: &Path, m: &Manifest) -> Result<(), String> {
        for g in &m.grammars {
            for (other, _) in self.db().iter().filter(|(n, _)| **n != m.name) {
                let theirs = Manifest::load(&self.pkg_dir(other)).map(|o| o.grammars);
                if theirs.is_ok_and(|gs| gs.iter().any(|o| o.name == g.name)) {
                    return Err(format!(
                        "grammar {} already comes with package {other} (:uninstall {other} first)",
                        g.name
                    ));
                }
            }
            skink_syntax::check_grammar(&dyn_spec(dir, g.clone()))
                .map_err(|e| format!("grammar {}: {e}", g.name))?;
        }
        Ok(())
    }

    /// Copy the package in, download its tools and fonts, record it.
    fn place(
        &self,
        dir: &Path,
        m: &Manifest,
        source: &str,
        scratch: &Path,
    ) -> Result<Vec<String>, String> {
        let io = |what: &str| {
            let what = what.to_owned();
            move |e: std::io::Error| format!("{what}: {e}")
        };
        let mut notes = Vec::new();
        // Downloads first: a failure leaves the old version in place.
        let tools_new = self.data.join("tools").join(format!(".{}.new", m.name));
        let _ = std::fs::remove_dir_all(&tools_new);
        let mut tool_dirs = Vec::new();
        for t in &m.tools {
            let Some(d) = t.targets.get(&self.target) else {
                notes.push(format!(
                    "{} has no download for {}; install it yourself",
                    t.name, self.target
                ));
                continue;
            };
            let file = fetch_pinned(d, scratch)?;
            let into = tools_new.join(&t.name);
            let bin_rel = exe(d.bin.as_deref().unwrap_or(&t.bin));
            unpack(&file, &into, &bin_rel).map_err(|e| format!("{}: {e}", t.name))?;
            let bin = into.join(&bin_rel);
            if !bin.is_file() {
                return Err(format!("{}: {} is not in the download", t.name, t.bin));
            }
            update::make_executable(&bin).map_err(io(&t.name))?;
            let final_bin = self.tool_dir(&m.name).join(&t.name).join(&bin_rel);
            tool_dirs.push(
                final_bin
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default(),
            );
        }
        let mut fonts = Vec::new();
        for f in &m.fonts {
            let Some(font_dir) = &self.fonts else {
                notes.push(format!("{}: no user font folder on this system", f.name));
                continue;
            };
            let file = fetch_pinned(&f.download, scratch)?;
            let out = scratch.join(format!("font-{}", fonts.len()));
            unpack(&file, &out, "font").map_err(|e| format!("{}: {e}", f.name))?;
            let mut all = Vec::new();
            files_under(&out, &mut all);
            for p in all {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if f.files.iter().any(|pat| glob(pat, &name)) {
                    fonts.push((p, font_dir.join(&name)));
                }
            }
        }
        // Now replace: package, tools, fonts.
        let old = self.db().remove(&m.name);
        for f in old.as_ref().map(|v| texts(v, "placed")).unwrap_or_default() {
            let _ = std::fs::remove_file(f);
        }
        let dest = self.pkg_dir(&m.name);
        let staged = self.data.join("packages").join(format!(".{}.new", m.name));
        let _ = std::fs::remove_dir_all(&staged);
        copy_dir(dir, &staged).map_err(io("copy the package"))?;
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&staged, &dest).map_err(io("move the package in"))?;
        let _ = std::fs::remove_dir_all(self.tool_dir(&m.name));
        if tools_new.exists() {
            std::fs::rename(&tools_new, self.tool_dir(&m.name)).map_err(io("move the tools in"))?;
        }
        let mut placed = Vec::new();
        for (from, to) in fonts {
            if let Some(p) = to.parent() {
                std::fs::create_dir_all(p).map_err(io("create the font folder"))?;
            }
            std::fs::copy(&from, &to).map_err(io("copy a font"))?;
            placed.push(to.display().to_string());
        }
        if !placed.is_empty() && cfg!(windows) {
            // Per-user fonts also need a registry entry; not done yet.
            notes.push(
                "fonts copied to your font folder; open each one there and choose Install \
                 for terminals to see it"
                    .into(),
            );
        }
        if !placed.is_empty() && cfg!(target_os = "linux") && update::have("fc-cache") {
            let _ = std::process::Command::new("fc-cache")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        let mut db = self.db();
        db.insert(
            m.name.clone(),
            json!({
                "version": m.version,
                "source": source,
                "state": "installed",
                "placed": placed,
                "path": tool_dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>(),
            }),
        );
        self.save_db(db)?;
        Ok(notes)
    }

    /// `--install .`: what the project in `root` needs.
    pub fn project_plan(&self, root: &Path) -> Result<Vec<(Spec, String)>, String> {
        let listed = root.join(".skink").join("packages.json");
        if listed.is_file() {
            let text = std::fs::read_to_string(&listed).map_err(|e| e.to_string())?;
            let v: Value = serde_json::from_str(&skink_config::relaxed_json(&text))
                .map_err(|e| format!(".skink/packages.json: {e}"))?;
            let list = v
                .as_array()
                .cloned()
                .unwrap_or_else(|| tables(&v, "packages").to_vec());
            let mut out = Vec::new();
            for s in list.iter().filter_map(Value::as_str) {
                let spec = Spec::parse(s)?;
                let have = match &spec {
                    Spec::Name(n) => self.is_installed(n),
                    Spec::GitHub { package, repo, .. } => {
                        self.is_installed(package.as_deref().unwrap_or(repo))
                    }
                    Spec::Dir(_) => false,
                };
                if !have {
                    out.push((spec, "listed in .skink/packages.json".to_owned()));
                }
            }
            return Ok(out);
        }
        let mut by_ext: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
        let walk = ignore::WalkBuilder::new(root).max_depth(Some(16)).build();
        for e in walk.flatten().take(SCAN_CAP) {
            if !e.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let p = e.path();
            if let Some(n) = p.file_name() {
                *by_name.entry(n.to_string_lossy().into_owned()).or_default() += 1;
            }
            if let Some(x) = p.extension() {
                *by_ext
                    .entry(x.to_string_lossy().to_ascii_lowercase())
                    .or_default() += 1;
            }
        }
        let mut plan: Vec<(usize, Spec, String)> = Vec::new();
        for e in self.registry(true) {
            if self.is_installed(&e.name) {
                continue;
            }
            let mut n = 0;
            let mut seen = Vec::new();
            for x in &e.extensions {
                if let Some(c) = by_ext.get(&x.to_ascii_lowercase()) {
                    n += c;
                    seen.push(format!(".{x}"));
                }
            }
            for f in &e.file_names {
                if let Some(c) = by_name.get(f) {
                    n += c;
                    seen.push(f.clone());
                }
            }
            if n > 0 {
                let files = if n == 1 { "file" } else { "files" };
                plan.push((
                    n,
                    Spec::Name(e.name.clone()),
                    format!("{n} {files} ({})", seen.join(", ")),
                ));
            }
        }
        plan.sort_by_key(|a| std::cmp::Reverse(a.0));
        Ok(plan.into_iter().map(|(_, s, why)| (s, why)).collect())
    }
}

/// A downloader for one plain URL folder (no API fallback).
fn plain(base: &str) -> Downloader {
    Downloader {
        direct: base.to_owned(),
        direct_name: base.to_owned(),
        api: None,
        release: String::new(),
        hint_release: String::new(),
        repo: String::new(),
        backoff: std::time::Duration::from_secs(2),
    }
}

/// Download a file the manifest pins by SHA-256.
fn fetch_pinned(d: &Download, scratch: &Path) -> Result<PathBuf, String> {
    let (base, file) = d
        .url
        .rsplit_once('/')
        .ok_or_else(|| format!("bad url {}", d.url))?;
    let file = file.split(['?', '#']).next().unwrap_or(file);
    let dest = scratch
        .join(format!("dl-{}", d.sha256.get(..12).unwrap_or("x")))
        .join(file);
    std::fs::create_dir_all(dest.parent().unwrap_or(scratch)).map_err(|e| e.to_string())?;
    plain(base).get(file, &dest).map_err(|e| e.to_string())?;
    update::verify(&dest, &d.sha256).map_err(|e| format!("{file}: {e}"))?;
    Ok(dest)
}

/// What the installed, enabled packages add to the editor.
#[derive(Default)]
pub struct Active {
    pub grammars: Vec<skink_syntax::DynSpec>,
    pub lsp: Vec<(String, skink_lsp::Override)>,
    pub theme_dirs: Vec<PathBuf>,
    /// Tool folders, for PATH.
    pub path: Vec<PathBuf>,
    pub scripts: Vec<PathBuf>,
    /// Packages that did not load, with why.
    pub broken: Vec<String>,
}

impl Active {
    /// PATH with the tool folders first, for language servers and terminals.
    pub fn path_var(&self) -> Option<String> {
        if self.path.is_empty() {
            return None;
        }
        let old = std::env::var_os("PATH").unwrap_or_default();
        let all = self.path.iter().cloned().chain(std::env::split_paths(&old));
        std::env::join_paths(all)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }
}

fn dyn_spec(dir: &Path, g: GrammarDef) -> skink_syntax::DynSpec {
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
    skink_syntax::DynSpec {
        highlights: read(&g.highlights),
        injections: g.injections.as_deref().map(read),
        wasm: dir.join(&g.wasm),
        extensions: g
            .extensions
            .iter()
            .map(|e| e.to_ascii_lowercase())
            .collect(),
        file_names: g.file_names,
        aliases: g.aliases,
        name: g.name,
    }
}

impl Store {
    pub fn active(&self) -> Active {
        let mut a = Active::default();
        let installed = self.installed();
        for p in &installed {
            if let PkgState::Broken(why) = &p.state {
                a.broken.push(format!("{}: {why}", p.name));
            }
            if p.state == PkgState::Installed {
                a.path.extend(p.tool_dirs.iter().cloned());
            }
        }
        let path_var = a.path_var();
        for p in installed.iter().filter(|p| p.state == PkgState::Installed) {
            let dir = self.pkg_dir(&p.name);
            let Ok(m) = Manifest::load(&dir) else {
                continue;
            };
            for g in m.grammars {
                a.grammars.push(dyn_spec(&dir, g));
            }
            for l in m.lsp {
                let mut argv = l.command;
                // A command naming one of the package's tools runs that
                // tool's executable for this platform.
                if let Some(full) = m
                    .tools
                    .iter()
                    .filter(|t| t.name == argv[0])
                    .filter_map(|t| {
                        let bin = t
                            .targets
                            .get(&self.target)?
                            .bin
                            .as_deref()
                            .unwrap_or(&t.bin);
                        Some(self.tool_dir(&p.name).join(&t.name).join(exe(bin)))
                    })
                    .find(|f| f.is_file())
                {
                    argv[0] = full.display().to_string();
                }
                a.lsp.push((
                    l.language,
                    skink_lsp::Override::Server {
                        argv,
                        extensions: l.extensions,
                        root_markers: l.root_markers,
                        init_options: None,
                        env: path_var
                            .iter()
                            .map(|v| ("PATH".to_owned(), v.clone()))
                            .collect(),
                    },
                ));
            }
            for t in &m.themes {
                if let Some(d) = dir.join(t).parent()
                    && !a.theme_dirs.iter().any(|x| x == d)
                {
                    a.theme_dirs.push(d.to_path_buf());
                }
            }
            a.scripts.extend(m.scripts.iter().map(|s| dir.join(s)));
        }
        a
    }
}

/// The verified package for files like `path`, when it is not installed:
/// for the "no grammar for X" toast.
pub fn suggestion_for(path: &Path, installed: &[Installed]) -> Option<&'static str> {
    let name = path.file_name()?.to_string_lossy();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    verified()
        .iter()
        .find(|e| {
            e.file_names.iter().any(|f| *f == name)
                || ext
                    .as_ref()
                    .is_some_and(|x| e.extensions.iter().any(|y| y == x))
        })
        .filter(|e| !installed.iter().any(|i| i.name == e.name))
        .map(|e| e.name.as_str())
}

#[cfg(test)]
#[path = "packages_tests.rs"]
mod tests;
