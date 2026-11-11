//! Self-update: `skink --upgrade`, `:upgrade` and the background check.
//!
//! Downloads go through the tools the installers already rely on (`curl`,
//! then `wget`, then PowerShell on Windows), so the binary carries no HTTP or
//! TLS stack. The archive is checked against its `.sha256` (or, in builds with a
//! minisign key, against the signed `SHA256SUMS`, see docs/RELEASING.md), unpacked next to
//! the running binary and swapped in with a rename:
//! - Unix: rename over the running file (the old inode lives on until exit);
//! - Windows: a running `.exe` cannot be overwritten but can be renamed, so it
//!   moves to `skink.old.exe` (removed on the next start) and the new one
//!   takes its name.

use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    fmt,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    time::{Duration, SystemTime},
};

pub const REPO: &str = "https://github.com/xuckless/skink";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// One background check per day at most.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const STAMP: &str = "update-check";

// ---- versions ---------------------------------------------------------------

/// `major.minor.patch[-pre]`, with an optional leading `v`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    nums: [u64; 3],
    pre: Option<String>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('v');
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p.to_owned())),
            None => (s, None),
        };
        let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
        let nums = [it.next()??, it.next()??, it.next()??];
        it.next().is_none().then_some(Version { nums, pre })
    }

    pub fn current() -> Self {
        Self::parse(CURRENT).expect("crate version is semver")
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        self.nums
            .cmp(&o.nums)
            .then_with(|| match (&self.pre, &o.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater, // 1.0.0 > 1.0.0-rc1
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c] = self.nums;
        write!(f, "{a}.{b}.{c}")?;
        match &self.pre {
            Some(p) => write!(f, "-{p}"),
            None => Ok(()),
        }
    }
}

// ---- release assets ---------------------------------------------------------

/// The release target this binary was built for (the asset naming of
/// `.github/workflows/release.yml`).
pub fn target() -> Option<&'static str> {
    let t = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    };
    Some(t)
}

pub fn asset_name(target: &str) -> String {
    let ext = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("skink-{target}.{ext}")
}

/// Where releases come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The newest GitHub release.
    Latest,
    /// One GitHub release by tag (`--version`, `SKINK_VERSION`): may downgrade.
    Pinned(String),
    /// A directory of release assets (`SKINK_BASE_URL`: mirrors, tests). The
    /// version is only known once the binary is downloaded.
    Mirror(String),
}

impl Source {
    /// `--version X` beats `SKINK_BASE_URL` beats `SKINK_VERSION`, like the
    /// installers.
    pub fn from_env(pin: Option<&str>) -> Self {
        let env = |k| std::env::var(k).ok().filter(|v: &String| !v.is_empty());
        if let Some(v) = pin {
            return Source::Pinned(tag(v));
        }
        if let Some(base) = env("SKINK_BASE_URL") {
            return Source::Mirror(base.trim_end_matches('/').to_owned());
        }
        match env("SKINK_VERSION") {
            Some(v) => Source::Pinned(tag(&v)),
            None => Source::Latest,
        }
    }

    fn base_url(&self) -> String {
        match self {
            Source::Latest => format!("{REPO}/releases/latest/download"),
            Source::Pinned(t) => format!("{REPO}/releases/download/{t}"),
            Source::Mirror(b) => b.clone(),
        }
    }
}

fn tag(v: &str) -> String {
    if v.starts_with('v') {
        v.to_owned()
    } else {
        format!("v{v}")
    }
}

// ---- how skink was installed ------------------------------------------------

/// Who owns the running binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    /// Installed by our installers (or copied by hand): we may replace it.
    SelfManaged(PathBuf),
    /// A package manager owns it: tell the user its upgrade command.
    Managed {
        manager: &'static str,
        command: &'static str,
    },
    /// A cargo build directory: rebuilding is the upgrade.
    BuildDir,
}

impl Install {
    pub fn classify(exe: &Path) -> Self {
        let p = exe.to_string_lossy().replace('\\', "/").to_lowercase();
        let has = |s: &str| p.contains(s);
        let managed = |manager, command| Install::Managed { manager, command };
        if has("/target/debug/") || has("/target/release/") || has("/target/x86_64-") {
            Install::BuildDir
        } else if has("/.cargo/bin/") {
            managed(
                "cargo",
                "cargo install --git https://github.com/xuckless/skink skink",
            )
        } else if has("/cellar/") || has("/homebrew/") || has("/linuxbrew/") {
            managed("Homebrew", "brew upgrade skink")
        } else if has("/scoop/") {
            managed("scoop", "scoop update skink")
        } else if has("/winget/") {
            managed("winget", "winget upgrade skink")
        } else if p.starts_with("/nix/store/") {
            managed("Nix", "update skink through your Nix configuration")
        } else if p.starts_with("/usr/bin/") || p.starts_with("/bin/") {
            managed(
                "your system package manager",
                "upgrade skink with your system package manager",
            )
        } else {
            Install::SelfManaged(exe.to_path_buf())
        }
    }

    pub fn current() -> Result<Self, Error> {
        let exe = std::env::current_exe().map_err(Error::io("locate the running binary"))?;
        Ok(Self::classify(&crate::doc::canonical(&exe)))
    }
}

// ---- trust ------------------------------------------------------------------

/// How a download is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// The per-asset SHA-256 from the same release: catches corruption, not
    /// a compromised release.
    Checksum,
    /// `SHA256SUMS` must carry a valid minisign signature by this public key
    /// (base64), and the archive must match its line there.
    Signed(&'static str),
}

impl Trust {
    /// The key compiled into this binary (release builds set
    /// `SKINK_MINISIGN_PUBKEY`; local builds have none).
    pub fn built_in() -> Self {
        match option_env!("SKINK_MINISIGN_PUBKEY") {
            Some(k) if !k.trim().is_empty() => Trust::Signed(k.trim()),
            _ => Trust::Checksum,
        }
    }
}

/// Check a minisign signature (`.minisig` file text) over `data`.
pub fn verify_signature(pubkey_b64: &str, data: &[u8], sig_text: &str) -> Result<(), Error> {
    let key = minisign_verify::PublicKey::from_base64(pubkey_b64)
        .map_err(|e| Error::Signature(format!("bad built-in key: {e}")))?;
    let sig = minisign_verify::Signature::decode(sig_text)
        .map_err(|e| Error::Signature(format!("unreadable signature: {e}")))?;
    key.verify(data, &sig, true)
        .map_err(|e| Error::Signature(e.to_string()))
}

/// The line for `asset` in a `SHA256SUMS` file (`<hash>  <name>`).
fn sums_line<'a>(sums: &'a str, asset: &str) -> Option<&'a str> {
    sums.lines().find(|l| {
        l.split_whitespace()
            .nth(1)
            .map(|n| n.trim_start_matches('*'))
            == Some(asset)
    })
}

// ---- errors and outcomes ----------------------------------------------------

#[derive(Debug)]
pub enum Error {
    UnsupportedTarget,
    Managed {
        manager: &'static str,
        command: &'static str,
    },
    BuildDir,
    NotWritable(PathBuf),
    NoDownloader,
    Download(String),
    /// Both the direct link and the API fallback failed.
    DownloadFailed(Box<BothFailed>),
    Checksum {
        want: String,
        got: String,
    },
    Unpack(String),
    BadBinary(String),
    /// The release is not signed but this build requires signatures.
    Unsigned,
    /// The signature or the signed checksum list does not check out.
    Signature(String),
    Io(&'static str, std::io::Error),
}

impl Error {
    fn io(what: &'static str) -> impl FnOnce(std::io::Error) -> Error {
        move |e| Error::Io(what, e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnsupportedTarget => write!(f, "no release is built for this platform"),
            Error::Managed { manager, command } => {
                write!(f, "skink is managed by {manager}; run: {command}")
            }
            Error::BuildDir => write!(f, "this is a cargo build; rebuild it instead"),
            Error::NotWritable(d) => write!(
                f,
                "{} is not writable; re-run with the permissions that installed skink",
                d.display()
            ),
            Error::NoDownloader => write!(f, "need curl or wget (or PowerShell on Windows)"),
            Error::Download(u) => write!(f, "download failed: {u}"),
            Error::DownloadFailed(b) => {
                let BothFailed {
                    asset,
                    direct,
                    api,
                    hint,
                } = &**b;
                write!(
                    f,
                    "download of {asset} failed: {direct}, API fallback {api}; try again later or run: {hint}"
                )
            }
            Error::Checksum { want, got } => {
                write!(
                    f,
                    "checksum mismatch (expected {want}, got {got}); nothing changed"
                )
            }
            Error::Unpack(e) => write!(f, "could not unpack the release: {e}"),
            Error::BadBinary(e) => write!(f, "downloaded binary is not usable: {e}"),
            Error::Unsigned => write!(
                f,
                "the release has no signature (SHA256SUMS.minisig); nothing changed \
                 (SKINK_ALLOW_UNSIGNED=1 accepts checksum-only releases)"
            ),
            Error::Signature(e) => write!(f, "signature check failed: {e}; nothing changed"),
            Error::Io(what, e) => write!(f, "could not {what}: {e}"),
        }
    }
}

/// What an upgrade (or check) found or did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate(Version),
    /// `--check`: a different version is available, nothing was changed.
    Available(Version),
    /// The binary at `path` is now `to`; it runs from the next start.
    Installed {
        from: Version,
        to: Version,
        path: PathBuf,
    },
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::UpToDate(v) => write!(f, "skink {v} is up to date"),
            Outcome::Available(v) => write!(f, "skink {v} is available (you have {CURRENT})"),
            Outcome::Installed { from, to, path } => {
                write!(f, "skink {from} -> {to} installed at {}", path.display())
            }
        }
    }
}

/// `--check` reports; `Install` replaces the binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Check,
    Install,
}

// ---- download helpers -------------------------------------------------------

fn run_ok(cmd: &mut Command) -> bool {
    cmd.stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub(crate) fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Download `url` to `dest` with wget or PowerShell (no status codes; the
/// curl path below reports them).
fn fetch_plain(url: &str, dest: &Path) -> Result<(), Error> {
    let ok = if have("wget") {
        run_ok(Command::new("wget").args(["-q", "-O"]).arg(dest).arg(url))
    } else if cfg!(windows) {
        run_ok(Command::new("powershell").args([
            "-NoProfile",
            "-Command",
            &format!(
                "$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -UseBasicParsing -Uri '{url}' -OutFile '{}'",
                dest.display()
            ),
        ]))
    } else {
        return Err(Error::NoDownloader);
    };
    if ok {
        Ok(())
    } else {
        Err(Error::Download(url.to_owned()))
    }
}

/// [`Error::DownloadFailed`]'s details.
#[derive(Debug)]
pub struct BothFailed {
    pub asset: String,
    pub direct: Failure,
    pub api: Failure,
    /// A command that fetches the same asset another way.
    pub hint: String,
}

/// Why one source did not deliver, for the error message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// "github.com", "api.github.com", a mirror's host.
    pub source: String,
    pub what: Problem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// An HTTP error status (and "rate limited" when GitHub says so).
    Status { code: u16, rate_limited: bool },
    /// No HTTP answer at all (DNS, refused, reset, TLS, timeout).
    Network(String),
    /// The API answered, but the release has no such asset.
    NoAsset,
    /// The API's release JSON could not be read.
    BadReply,
}

impl Problem {
    /// Worth trying the other source (and a retry): server trouble or no
    /// answer, not "this does not exist" or "you are not allowed".
    fn transient(&self) -> bool {
        match self {
            Problem::Status { code, rate_limited } => *code >= 500 || *code == 429 || *rate_limited,
            Problem::Network(_) => true,
            Problem::NoAsset | Problem::BadReply => false,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let src = &self.source;
        match &self.what {
            Problem::Status {
                code,
                rate_limited: true,
            } => {
                write!(f, "{src} answered {code} rate limited")
            }
            Problem::Status { code, .. } => write!(f, "{src} answered {code} {}", reason(*code)),
            Problem::Network(e) => write!(f, "{src}: {e}"),
            Problem::NoAsset => write!(f, "{src}: the release has no such file"),
            Problem::BadReply => write!(f, "{src}: unreadable release list"),
        }
    }
}

fn reason(code: u16) -> &'static str {
    match code {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// curl's exit codes for "no HTTP answer", in words.
fn curl_error(code: i32) -> String {
    match code {
        5 | 6 => "could not resolve the host".into(),
        7 => "could not connect".into(),
        28 => "timed out".into(),
        35 | 60 => "TLS handshake failed".into(),
        52 => "empty reply".into(),
        55 | 56 => "connection reset".into(),
        n => format!("curl error {n}"),
    }
}

/// One GET with curl: the body lands in `dest`; Ok on 2xx.
fn curl_get(url: &str, headers: &[String], dest: &Path, source: &str) -> Result<(), Failure> {
    let fail = |what| Failure {
        source: source.to_owned(),
        what,
    };
    let mut cmd = Command::new("curl");
    cmd.args(["-sSL", "--connect-timeout", "15", "--max-time", "600"]);
    for h in headers {
        cmd.arg("-H").arg(h);
    }
    let out = cmd
        .arg("-o")
        .arg(dest)
        .args(["-w", "%{http_code}"])
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| fail(Problem::Network(e.to_string())))?;
    let code: u16 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    if !out.status.success() || code == 0 {
        return Err(fail(Problem::Network(curl_error(
            out.status.code().unwrap_or(-1),
        ))));
    }
    if (200..300).contains(&code) {
        return Ok(());
    }
    let body = std::fs::read_to_string(dest).unwrap_or_default();
    let _ = std::fs::remove_file(dest);
    Err(fail(Problem::Status {
        code,
        rate_limited: code == 403 && body.to_ascii_lowercase().contains("rate limit"),
    }))
}

/// Where release assets come from: the direct link, then the API.
pub struct Downloader {
    /// `.../releases/download/<tag>` or a mirror directory.
    pub direct: String,
    /// The direct host, for messages.
    pub direct_name: String,
    /// `https://api.github.com/repos/<owner>/<repo>` (none for mirrors).
    pub api: Option<String>,
    /// `releases/latest` or `releases/tags/<tag>`, under `api`.
    pub release: String,
    /// `gh release download ...` for the error message.
    pub hint_release: String,
    /// `owner/repo`, for that message.
    pub repo: String,
    /// Pause before the one retry of a source.
    pub backoff: std::time::Duration,
}

/// How far a download got. One method moves it on.
enum Attempt {
    Direct,
    Api { direct: Failure },
    Failed { direct: Failure, api: Failure },
}

impl Downloader {
    pub fn for_source(source: &Source) -> Self {
        let repo = REPO.trim_start_matches("https://github.com/");
        let api = format!("https://api.github.com/repos/{repo}");
        let (release, hint_release) = match source {
            Source::Latest => ("releases/latest".to_owned(), String::new()),
            Source::Pinned(t) => (format!("releases/tags/{t}"), format!("{t} ")),
            Source::Mirror(_) => (String::new(), String::new()),
        };
        Self {
            direct: source.base_url(),
            direct_name: match source {
                Source::Mirror(b) => b.clone(),
                _ => "github.com".into(),
            },
            api: (!matches!(source, Source::Mirror(_))).then_some(api),
            release,
            hint_release,
            repo: repo.to_owned(),
            backoff: std::time::Duration::from_secs(2),
        }
    }

    /// A source, tried twice when the first try is transient.
    fn twice(&self, mut once: impl FnMut() -> Result<(), Failure>) -> Result<(), Failure> {
        match once() {
            Err(f) if f.what.transient() => {
                std::thread::sleep(self.backoff);
                once()
            }
            r => r,
        }
    }

    fn token_header() -> Option<String> {
        ["GH_TOKEN", "GITHUB_TOKEN"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|t| !t.is_empty()))
            .map(|t| format!("Authorization: Bearer {t}"))
    }

    fn via_api(&self, api: &str, asset: &str, dest: &Path) -> Result<(), Failure> {
        let fail = |what| Failure {
            source: "api.github.com".into(),
            what,
        };
        let auth: Vec<String> = Self::token_header().into_iter().collect();
        let mut h = vec!["Accept: application/vnd.github+json".to_owned()];
        h.extend(auth.iter().cloned());
        let list = dest.with_extension("release.json");
        self.twice(|| {
            curl_get(
                &format!("{api}/{}", self.release),
                &h,
                &list,
                "api.github.com",
            )
        })?;
        let text = std::fs::read_to_string(&list).unwrap_or_default();
        let _ = std::fs::remove_file(&list);
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| fail(Problem::BadReply))?;
        let id = json["assets"]
            .as_array()
            .ok_or_else(|| fail(Problem::BadReply))?
            .iter()
            .find(|a| a["name"].as_str() == Some(asset))
            .and_then(|a| a["id"].as_u64())
            .ok_or_else(|| fail(Problem::NoAsset))?;
        let mut h = vec!["Accept: application/octet-stream".to_owned()];
        h.extend(auth);
        self.twice(|| {
            curl_get(
                &format!("{api}/releases/assets/{id}"),
                &h,
                dest,
                "api.github.com",
            )
        })
    }

    /// Download release file `asset` to `dest`.
    pub fn get(&self, asset: &str, dest: &Path) -> Result<(), Error> {
        let direct_url = format!("{}/{asset}", self.direct);
        if !have("curl") {
            return fetch_plain(&direct_url, dest);
        }
        let mut at = Attempt::Direct;
        loop {
            at = match at {
                Attempt::Direct => {
                    match self.twice(|| curl_get(&direct_url, &[], dest, &self.direct_name)) {
                        Ok(()) => return Ok(()),
                        Err(f) if f.what.transient() && self.api.is_some() => {
                            Attempt::Api { direct: f }
                        }
                        Err(f) => return Err(Error::Download(format!("{direct_url}: {f}"))),
                    }
                }
                Attempt::Api { direct } => {
                    let api = self.api.as_deref().unwrap_or_default();
                    match self.via_api(api, asset, dest) {
                        Ok(()) => return Ok(()),
                        Err(api) => Attempt::Failed { direct, api },
                    }
                }
                Attempt::Failed { direct, api } => {
                    let repo = &self.repo;
                    return Err(Error::DownloadFailed(Box::new(BothFailed {
                        asset: asset.to_owned(),
                        direct,
                        api,
                        hint: format!(
                            "gh release download {}-R {repo} -p {asset}",
                            self.hint_release
                        ),
                    })));
                }
            }
        }
    }
}

/// The newest release's version, from where `releases/latest` redirects to
/// (no API call, so no rate limit). `None` when curl is missing or offline.
pub fn latest_version() -> Option<Version> {
    let out = Command::new("curl")
        .args(["-fsSIL", "--max-time", "15", "-o"])
        .arg(if cfg!(windows) { "NUL" } else { "/dev/null" })
        .args(["-w", "%{url_effective}"])
        .arg(format!("{REPO}/releases/latest"))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let url = String::from_utf8(out.stdout).ok()?;
    Version::parse(url.trim().rsplit('/').next()?)
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut h = Sha256::new();
    let mut f = std::fs::File::open(path)?;
    std::io::copy(&mut f, &mut h)?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Compare a downloaded file against a `.sha256` file's first field.
pub fn verify(file: &Path, sum_file_text: &str) -> Result<(), Error> {
    let want = sum_file_text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let got = sha256_file(file).map_err(Error::io("hash the download"))?;
    if want.len() == 64 && want == got {
        Ok(())
    } else {
        Err(Error::Checksum { want, got })
    }
}

fn unpack(archive: &Path, into: &Path) -> Result<(), Error> {
    // bsdtar ships with Windows 10+ and reads zip as well.
    let tar = Command::new("tar")
        .arg(if archive.extension().is_some_and(|e| e == "zip") {
            "-xf"
        } else {
            "-xzf"
        })
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(std::process::Stdio::null())
        .output();
    if tar.as_ref().is_ok_and(|o| o.status.success()) {
        return Ok(());
    }
    if cfg!(windows)
        && run_ok(Command::new("powershell").args([
            "-NoProfile",
            "-Command",
            &format!(
                "Expand-Archive -Force -Path '{}' -DestinationPath '{}'",
                archive.display(),
                into.display()
            ),
        ]))
    {
        return Ok(());
    }
    Err(Error::Unpack(match tar {
        Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_owned(),
        Err(e) => format!("tar: {e}"),
    }))
}

/// Ask a binary for its version (`skink X.Y.Z`).
fn binary_version(bin: &Path) -> Result<Version, Error> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| Error::BadBinary(e.to_string()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.strip_prefix("skink ")
        .and_then(Version::parse)
        .ok_or_else(|| Error::BadBinary(format!("unexpected --version output {text:?}")))
}

/// Remove what a previous Windows upgrade left behind (call at startup).
pub fn cleanup_previous() {
    if !cfg!(windows) {
        return;
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let _ = std::fs::remove_file(dir.join("skink.old.exe"));
    }
}

/// Swap `new` in for `exe`. Both are in the same directory.
fn replace(exe: &Path, new: &Path) -> Result<(), Error> {
    if cfg!(windows) {
        let old = exe.with_file_name("skink.old.exe");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old).map_err(Error::io("move the running binary aside"))?;
        if let Err(e) = std::fs::rename(new, exe) {
            let _ = std::fs::rename(&old, exe); // put it back
            return Err(Error::Io("move the new binary into place", e));
        }
        Ok(())
    } else {
        std::fs::rename(new, exe).map_err(Error::io("move the new binary into place"))
    }
}

#[cfg(unix)]
pub(crate) fn make_executable(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
pub(crate) fn make_executable(_: &Path) -> std::io::Result<()> {
    Ok(())
}

/// A scratch directory next to the binary (same filesystem, so the final
/// rename is atomic), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(dir: &Path) -> Result<Self, Error> {
        let p = dir.join(format!(".skink-upgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        match std::fs::create_dir_all(&p) {
            Ok(()) => Ok(Scratch(p)),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                Err(Error::NotWritable(dir.to_path_buf()))
            }
            Err(e) => Err(Error::Io("create a scratch directory", e)),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---- the upgrade ------------------------------------------------------------

/// Check for, or install, the release `source` points at.
pub fn upgrade(source: &Source, mode: Mode) -> Result<Outcome, Error> {
    let exe = match Install::current()? {
        Install::SelfManaged(exe) => exe,
        Install::Managed { manager, command } => return Err(Error::Managed { manager, command }),
        Install::BuildDir => return Err(Error::BuildDir),
    };
    upgrade_at(&exe, source, mode)
}

/// Download the release archive for `target` from `source` into `dir`,
/// check it (signature or checksum, as `trust` says), unpack it and return
/// the binary inside.
fn fetch_verified(
    source: &Source,
    target: &str,
    dir: &Path,
    trust: Trust,
) -> Result<PathBuf, Error> {
    let asset = asset_name(target);
    let dl = Downloader::for_source(source);
    let archive = dir.join(&asset);
    dl.get(&asset, &archive)?;
    let read = |p: &Path| std::fs::read_to_string(p).map_err(Error::io("read the checksum"));
    let allow_unsigned = std::env::var_os("SKINK_ALLOW_UNSIGNED").is_some_and(|v| v == "1");
    match trust {
        Trust::Signed(key) => {
            let sums = dir.join("SHA256SUMS");
            let sig = dir.join("SHA256SUMS.minisig");
            dl.get("SHA256SUMS", &sums)?;
            match dl.get("SHA256SUMS.minisig", &sig) {
                Ok(()) => {
                    let text = read(&sums)?;
                    verify_signature(key, text.as_bytes(), &read(&sig)?)?;
                    let line = sums_line(&text, &asset)
                        .ok_or_else(|| Error::Signature(format!("{asset} is not in SHA256SUMS")))?;
                    verify(&archive, line)?;
                }
                Err(_) if allow_unsigned => {
                    verify(&archive, sums_line(&read(&sums)?, &asset).unwrap_or(""))?
                }
                Err(_) => return Err(Error::Unsigned),
            }
        }
        Trust::Checksum => {
            let sums = dir.join(format!("{asset}.sha256"));
            dl.get(&format!("{asset}.sha256"), &sums)?;
            verify(&archive, &read(&sums)?)?;
        }
    }
    unpack(&archive, dir)?;
    let exe_name = if target.contains("windows") {
        "skink.exe"
    } else {
        "skink"
    };
    let unpacked = dir.join(format!("skink-{target}")).join(exe_name);
    make_executable(&unpacked).map_err(Error::io("mark the new binary executable"))?;
    Ok(unpacked)
}

/// This version's release binary for another machine (`skink ssh`): kept
/// in `cache` (`<cache>/<version>/<target>/skink`), downloaded and checked
/// once. `SKINK_BASE_URL` points it at a mirror, like `--upgrade`.
pub fn binary_for(target: &str, cache: &Path) -> Result<PathBuf, Error> {
    let version = Version::current();
    let dir = cache.join(version.to_string()).join(target);
    let bin = dir.join("skink");
    if bin.is_file() {
        return Ok(bin);
    }
    let work = dir.join(format!(".fetch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(Error::io("create the download folder"))?;
    let source = match Source::from_env(None) {
        Source::Mirror(m) => Source::Mirror(m),
        _ => Source::Pinned(tag(&version.to_string())),
    };
    let got = fetch_verified(&source, target, &work, Trust::built_in());
    let result = got.and_then(|b| {
        std::fs::rename(&b, &bin).map_err(Error::io("keep the downloaded binary"))?;
        Ok(bin.clone())
    });
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// [`upgrade`] for the binary at `exe` (tests point it at a copy).
pub fn upgrade_at(exe: &Path, source: &Source, mode: Mode) -> Result<Outcome, Error> {
    upgrade_with(exe, source, mode, Trust::built_in())
}

/// [`upgrade_at`] with an explicit trust policy.
pub fn upgrade_with(
    exe: &Path,
    source: &Source,
    mode: Mode,
    trust: Trust,
) -> Result<Outcome, Error> {
    let current = Version::current();
    let target = target().ok_or(Error::UnsupportedTarget)?;
    // Know the version up front when we can, so "up to date" costs no download.
    let known = match source {
        Source::Latest => latest_version(),
        Source::Pinned(t) => Version::parse(t),
        Source::Mirror(_) => None,
    };
    let pinned = matches!(source, Source::Pinned(_));
    let wanted = |v: &Version| if pinned { *v != current } else { *v > current };
    if let Some(v) = &known {
        if !wanted(v) {
            return Ok(Outcome::UpToDate(current));
        }
        if mode == Mode::Check {
            return Ok(Outcome::Available(v.clone()));
        }
    }

    let dir = exe.parent().unwrap_or(Path::new("."));
    let scratch = Scratch::new(dir)?;
    let unpacked = fetch_verified(source, target, &scratch.0, trust)?;
    let new_version = binary_version(&unpacked)?;
    if !wanted(&new_version) {
        return Ok(Outcome::UpToDate(current));
    }
    if mode == Mode::Check {
        return Ok(Outcome::Available(new_version));
    }
    // Stage beside the binary, then one rename.
    let staged = dir.join(if cfg!(windows) {
        ".skink.new.exe"
    } else {
        ".skink.new"
    });
    std::fs::rename(&unpacked, &staged).map_err(Error::io("stage the new binary"))?;
    if let Err(e) = replace(exe, &staged) {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }
    Ok(Outcome::Installed {
        from: current,
        to: new_version,
        path: exe.to_path_buf(),
    })
}

// ---- background check (the editor) ------------------------------------------

/// Lifecycle of the in-editor update check. Each variant owns what it needs;
/// transitions happen in [`App`](crate::App) methods only.
pub enum UpdateState {
    /// No check this session (policy off, CI, checked recently, not started).
    Idle,
    /// A worker is asking GitHub for the latest version.
    Checking(mpsc::Receiver<Option<Version>>),
    /// A newer release exists; `:upgrade` installs it.
    Available(Version),
    /// A worker is downloading and installing.
    Installing(mpsc::Receiver<Result<Outcome, String>>),
    /// The new binary is in place and runs from the next start.
    Installed(Version),
    Failed(String),
}

/// Whether a background check is due: policy on, not CI, not opted out, and
/// the last check older than a day.
pub fn check_due(state_dir: Option<&Path>, now: SystemTime) -> bool {
    let env_set = |k| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if env_set("CI") || env_set("SKINK_NO_UPDATE_CHECK") || env_set("SKINK_BASE_URL") {
        return false;
    }
    let Some(dir) = state_dir else {
        return false;
    };
    let last = std::fs::metadata(dir.join(STAMP)).and_then(|m| m.modified());
    match last {
        Ok(t) => now.duration_since(t).is_ok_and(|age| age >= CHECK_EVERY),
        Err(_) => true,
    }
}

/// Record that a check started (the stamp's mtime is the time).
pub fn stamp(state_dir: &Path) {
    if std::fs::create_dir_all(state_dir).is_ok() {
        let _ = std::fs::write(state_dir.join(STAMP), CURRENT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A loopback HTTP server: `routes` maps a path to (status, body).
    fn fake_http(routes: Vec<(&'static str, u16, Vec<u8>)>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in l.incoming().flatten() {
                let mut r = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).map_or(true, |n| n == 0) || h == "\r\n" {
                        break;
                    }
                }
                let (code, body) = routes
                    .iter()
                    .find(|(p, ..)| *p == path)
                    .map_or((404, b"nope".to_vec()), |(_, c, b)| (*c, b.clone()));
                let mut s = stream;
                let _ = write!(
                    s,
                    "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(&body);
            }
        });
        format!("http://{addr}")
    }

    fn downloader(base: &str) -> Downloader {
        Downloader {
            direct: format!("{base}/direct"),
            direct_name: "github.com".into(),
            api: Some(format!("{base}/api")),
            release: "releases/tags/v9.9.9".into(),
            hint_release: "v9.9.9 ".into(),
            repo: "xuckless/skink".into(),
            backoff: std::time::Duration::ZERO,
        }
    }

    #[test]
    fn a_504_on_the_direct_link_falls_back_to_the_api() {
        if !have("curl") {
            return;
        }
        let list = br#"{"assets": [{"name": "skink-x.tar.gz", "id": 7}]}"#.to_vec();
        let base = fake_http(vec![
            ("/direct/skink-x.tar.gz", 504, b"gateway".to_vec()),
            ("/api/releases/tags/v9.9.9", 200, list),
            ("/api/releases/assets/7", 200, b"ARCHIVE".to_vec()),
        ]);
        let dir = std::env::temp_dir().join(format!("skink-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("skink-x.tar.gz");
        downloader(&base).get("skink-x.tar.gz", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"ARCHIVE");

        // Both down: the message names both answers and another way in.
        let base = fake_http(vec![
            ("/direct/skink-x.tar.gz", 504, b"gateway".to_vec()),
            (
                "/api/releases/tags/v9.9.9",
                403,
                b"API rate limit exceeded".to_vec(),
            ),
        ]);
        let e = downloader(&base)
            .get("skink-x.tar.gz", &dest)
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "download of skink-x.tar.gz failed: github.com answered 504 Gateway Timeout, API fallback \
             api.github.com answered 403 rate limited; try again later or run: gh release download \
             v9.9.9 -R xuckless/skink -p skink-x.tar.gz"
        );
        // A 404 is not a hiccup: no fallback, the URL is named.
        let base = fake_http(vec![]);
        let e = downloader(&base)
            .get("skink-x.tar.gz", &dest)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("/direct/skink-x.tar.gz: github.com answered 404 Not Found"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_order_like_semver() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("v0.2.0") > v("0.1.9"));
        assert!(v("0.10.0") > v("0.9.0"));
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        assert!(Version::parse("1.2").is_none() && Version::parse("1.2.3.4").is_none());
    }

    #[test]
    fn asset_names_match_the_release_workflow() {
        assert_eq!(
            asset_name("x86_64-unknown-linux-musl"),
            "skink-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            asset_name("x86_64-pc-windows-msvc"),
            "skink-x86_64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn package_managers_are_not_overwritten() {
        let c = |p: &str| Install::classify(Path::new(p));
        assert!(matches!(
            c("/home/u/.cargo/bin/skink"),
            Install::Managed { .. }
        ));
        assert!(matches!(
            c("/opt/homebrew/Cellar/skink/0.1.0/bin/skink"),
            Install::Managed { .. }
        ));
        assert!(matches!(
            c(r"C:\Users\u\scoop\apps\skink\current\skink.exe"),
            Install::Managed { .. }
        ));
        assert_eq!(
            c("/home/u/dev/skink/target/release/skink"),
            Install::BuildDir
        );
        assert!(matches!(
            c("/home/u/.local/bin/skink"),
            Install::SelfManaged(_)
        ));
    }

    #[test]
    fn signatures_are_checked_and_sums_lines_found() {
        // Test vector from minisign-verify (MIT): a signature over b"test".
        let key = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let sig = "untrusted comment: signature from minisign secret key
RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=
trusted comment: timestamp:1555779966\tfile:test
QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==";
        assert!(verify_signature(key, b"test", sig).is_ok());
        assert!(matches!(
            verify_signature(key, b"tampered", sig),
            Err(Error::Signature(_))
        ));
        let sums =
            "aa  skink-x86_64-unknown-linux-musl.tar.gz\nbb *skink-x86_64-pc-windows-msvc.zip\n";
        assert_eq!(
            sums_line(sums, "skink-x86_64-pc-windows-msvc.zip"),
            Some("bb *skink-x86_64-pc-windows-msvc.zip")
        );
        assert_eq!(sums_line(sums, "skink-aarch64-apple-darwin.tar.gz"), None);
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let d = std::env::temp_dir().join(format!("skink-sum-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a");
        std::fs::write(&f, "hello").unwrap();
        let good = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  a\n";
        assert!(verify(&f, good).is_ok());
        assert!(matches!(
            verify(&f, &good.replace('2', "3")),
            Err(Error::Checksum { .. })
        ));
        assert!(matches!(verify(&f, ""), Err(Error::Checksum { .. })));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
