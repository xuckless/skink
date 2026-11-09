//! Self-update: `skink --upgrade`, `:upgrade` and the background check.
//!
//! Downloads go through the tools the installers already rely on (`curl`,
//! then `wget`, then PowerShell on Windows), so the binary carries no HTTP or
//! TLS stack. The archive is checked against its `.sha256`, unpacked next to
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
    Checksum {
        want: String,
        got: String,
    },
    Unpack(String),
    BadBinary(String),
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
            Error::Checksum { want, got } => {
                write!(
                    f,
                    "checksum mismatch (expected {want}, got {got}); nothing changed"
                )
            }
            Error::Unpack(e) => write!(f, "could not unpack the release: {e}"),
            Error::BadBinary(e) => write!(f, "downloaded binary is not usable: {e}"),
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

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Download `url` to `dest`.
fn fetch(url: &str, dest: &Path) -> Result<(), Error> {
    let ok = if have("curl") {
        run_ok(
            Command::new("curl")
                .args(["-fsSL", "--retry", "2", "-o"])
                .arg(dest)
                .arg(url),
        )
    } else if have("wget") {
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
fn make_executable(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> std::io::Result<()> {
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

/// [`upgrade`] for the binary at `exe` (tests point it at a copy).
pub fn upgrade_at(exe: &Path, source: &Source, mode: Mode) -> Result<Outcome, Error> {
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
    let asset = asset_name(target);
    let base = source.base_url();
    let archive = scratch.0.join(&asset);
    let sums = scratch.0.join(format!("{asset}.sha256"));
    fetch(&format!("{base}/{asset}"), &archive)?;
    fetch(&format!("{base}/{asset}.sha256"), &sums)?;
    let sum_text = std::fs::read_to_string(&sums).map_err(Error::io("read the checksum"))?;
    verify(&archive, &sum_text)?;
    unpack(&archive, &scratch.0)?;
    let exe_name = if cfg!(windows) { "skink.exe" } else { "skink" };
    let unpacked = scratch.0.join(format!("skink-{target}")).join(exe_name);
    make_executable(&unpacked).map_err(Error::io("mark the new binary executable"))?;
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
