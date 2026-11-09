//! Move files to the OS trash instead of deleting them.
//!
//! - Linux/BSD: the freedesktop.org Trash spec (home trash, or
//!   `$topdir/.Trash-$uid` for other filesystems), so file managers can
//!   restore the entry.
//! - macOS: `~/.Trash`.
//! - Windows: the Recycle Bin through PowerShell (no extra dependency).
//!
//! Written from the spec rather than taking the `trash` crate, which pulls
//! in platform frameworks for what is a rename plus an info file here.

use std::{
    io,
    path::{Path, PathBuf},
};

/// Move `path` (file or folder) to the trash.
pub fn trash(path: &Path) -> io::Result<()> {
    imp::trash(path)
}

/// `name`, or `name 2`, `name 3`, ... until `free(candidate)` accepts it.
#[cfg(unix)]
fn unique_name(name: &str, mut free: impl FnMut(&str) -> bool) -> String {
    if free(name) {
        return name.to_owned();
    }
    (2..)
        .map(|i| format!("{name} {i}"))
        .find(|n| free(n))
        .expect("unbounded")
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::*;
    use std::{
        fs,
        io::Write,
        os::unix::fs::MetadataExt,
        time::{SystemTime, UNIX_EPOCH},
    };

    pub fn trash(path: &Path) -> io::Result<()> {
        let abs = std::path::absolute(path)?;
        let home = home_trash()?;
        match move_into(&home, &abs) {
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                let top = mount_top(&abs)?;
                let uid = fs::metadata(std::env::var_os("HOME").unwrap_or_default())?.uid();
                move_into(&top.join(format!(".Trash-{uid}")), &abs)
            }
            r => r,
        }
    }

    fn home_trash() -> io::Result<PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .ok_or_else(|| io::Error::other("no HOME"))?;
        Ok(data.join("Trash"))
    }

    /// The highest ancestor of `p` on the same filesystem.
    fn mount_top(p: &Path) -> io::Result<PathBuf> {
        let dev = fs::symlink_metadata(p)?.dev();
        let mut top = p.to_path_buf();
        for a in p.ancestors().skip(1) {
            match fs::metadata(a) {
                Ok(m) if m.dev() == dev => top = a.to_path_buf(),
                _ => break,
            }
        }
        Ok(top)
    }

    fn move_into(trash: &Path, abs: &Path) -> io::Result<()> {
        let (files, info) = (trash.join("files"), trash.join("info"));
        fs::create_dir_all(&files)?;
        fs::create_dir_all(&info)?;
        let name = abs
            .file_name()
            .ok_or_else(|| io::Error::other("nothing to trash"))?
            .to_string_lossy()
            .into_owned();
        // Reserve the name by creating the info file first (as the spec says).
        let mut info_file = None;
        let name = unique_name(&name, |n| {
            if files.join(n).exists() {
                return false;
            }
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(info.join(format!("{n}.trashinfo")))
            {
                Ok(f) => {
                    info_file = Some(f);
                    true
                }
                Err(_) => false,
            }
        });
        let info_path = info.join(format!("{name}.trashinfo"));
        let mut f = info_file.expect("reserved above");
        let written = writeln!(
            f,
            "[Trash Info]\nPath={}\nDeletionDate={}",
            percent_encode(abs),
            utc_now()
        );
        let moved = written.and_then(|_| fs::rename(abs, files.join(&name)));
        if moved.is_err() {
            let _ = fs::remove_file(info_path);
        }
        moved
    }

    fn percent_encode(p: &Path) -> String {
        use std::os::unix::ffi::OsStrExt;
        p.as_os_str()
            .as_bytes()
            .iter()
            .map(|&b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    }

    /// `YYYY-MM-DDThh:mm:ss` (UTC; the spec asks for local time, which std
    /// cannot tell. File managers only display it).
    fn utc_now() -> String {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let (days, rem) = (secs / 86_400, secs % 86_400);
        // Civil date from days since 1970 (Howard Hinnant's algorithm).
        let z = days as i64 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn trashes_with_an_info_file_and_unique_names() {
            let d = std::env::temp_dir().join(format!("skink-trash-{}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            let can = d.join("trash");
            for _ in 0..2 {
                let f = d.join("a b.txt");
                fs::write(&f, "x").unwrap();
                move_into(&can, &f).unwrap();
                assert!(!f.exists());
            }
            assert!(can.join("files/a b.txt").exists() && can.join("files/a b.txt 2").exists());
            let info = fs::read_to_string(can.join("info/a b.txt.trashinfo")).unwrap();
            assert!(info.starts_with("[Trash Info]\nPath=/") && info.contains("a%20b.txt"));
            assert!(info.contains("DeletionDate=20"));
            fs::remove_dir_all(&d).unwrap();
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    pub fn trash(path: &Path) -> io::Result<()> {
        let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("no HOME"))?;
        let can = PathBuf::from(home).join(".Trash");
        std::fs::create_dir_all(&can)?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("nothing to trash"))?
            .to_string_lossy()
            .into_owned();
        let name = unique_name(&name, |n| !can.join(n).exists());
        std::fs::rename(path, can.join(name))
    }
}

#[cfg(windows)]
mod imp {
    use super::*;

    pub fn trash(path: &Path) -> io::Result<()> {
        let abs = std::path::absolute(path)?;
        let p = abs.to_string_lossy().replace('\'', "''");
        let call = if abs.is_dir() {
            "DeleteDirectory"
        } else {
            "DeleteFile"
        };
        let script = format!(
            "Add-Type -AssemblyName Microsoft.VisualBasic; \
             [Microsoft.VisualBasic.FileIO.FileSystem]::{call}('{p}', 'OnlyErrorDialogs', 'SendToRecycleBin')"
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()?;
        if out.status.success() && !abs.exists() {
            Ok(())
        } else {
            Err(io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            ))
        }
    }
}
