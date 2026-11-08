//! `file://` URI <-> filesystem path conversion.
//!
//! LSP identifies documents by URI and servers disagree about spelling
//! (`file:///c%3A/x`, `file:///C:/x`, `file:///c:/x`). Skink always *emits*
//! `file:///C:/x` (upper-case drive, literal colon) and uses [`uri_key`] to
//! compare URIs from any source.
//!
//! The pure string functions take an explicit `windows` flag so the Windows
//! rules are unit tested on every platform; [`path_to_uri`] and
//! [`uri_to_path`] pick the host style.

use std::path::{Path, PathBuf};

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Bytes kept verbatim in a URI path: RFC 3986 unreserved plus the `/`
/// separator and `:` (Windows drive letters and legal `pchar`).
fn is_path_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/' | b':')
}

fn percent_encode_into(out: &mut String, bytes: &[u8]) {
    for &b in bytes {
        if is_path_safe(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 15) as usize] as char);
        }
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode `%XX` escapes. Malformed escapes are kept literally.
pub fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// `X:` at the start of `s`, followed by end or a separator.
fn drive_prefix(s: &str) -> Option<char> {
    let b = s.as_bytes();
    (b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'/' || b[2] == b'\\'))
        .then(|| b[0].to_ascii_uppercase() as char)
}

/// Encode a path string as a `file://` URI. With `windows` set, `\` is a
/// separator, drive letters are upper-cased and UNC / `\\?\` paths are
/// understood.
pub fn path_str_to_uri(path: &str, windows: bool) -> String {
    let mut out = String::with_capacity(path.len() + 12);
    out.push_str("file://");
    if !windows {
        percent_encode_into(&mut out, path.as_bytes());
        return out;
    }
    let mut p = path.replace('\\', "/");
    // `\\?\C:\x` and `\\?\UNC\srv\share` (verbatim prefixes).
    if let Some(rest) = p.strip_prefix("//?/") {
        p = match rest.strip_prefix("UNC/") {
            Some(unc) => format!("//{unc}"),
            None => rest.to_string(),
        };
    }
    if let Some(unc) = p.strip_prefix("//") {
        // //server/share/x -> file://server/share/x
        percent_encode_into(&mut out, unc.as_bytes());
        return out;
    }
    out.push('/');
    let rest = match drive_prefix(&p) {
        Some(d) => {
            out.push(d);
            &p[1..]
        }
        None => p.strip_prefix('/').unwrap_or(&p),
    };
    percent_encode_into(&mut out, rest.as_bytes());
    out
}

/// Split `file:` URI into (authority, raw path). `None` for other schemes.
fn split_file_uri(uri: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = uri.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("file") {
        return None;
    }
    let rest = rest.strip_prefix("//")?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    match rest.find('/') {
        Some(i) => Some((&rest[..i], &rest[i..])),
        None => Some((rest, "")),
    }
}

/// Decode a `file://` URI into path bytes (UTF-8 unless the OS allows more),
/// using Windows rules when `windows` is set.
fn uri_to_path_bytes(uri: &str, windows: bool) -> Option<Vec<u8>> {
    let (authority, raw) = split_file_uri(uri)?;
    let local = authority.is_empty() || authority.eq_ignore_ascii_case("localhost");
    let decoded = percent_decode(raw);
    if !windows {
        return local.then_some(decoded);
    }
    let path = String::from_utf8(decoded).ok()?;
    if !local {
        // UNC: file://server/share/x -> \\server\share\x
        return Some(format!("\\\\{authority}{}", path.replace('/', "\\")).into_bytes());
    }
    let trimmed = path.strip_prefix('/').unwrap_or(&path);
    Some(
        match drive_prefix(trimmed) {
            Some(d) => format!("{d}:{}", trimmed[2..].replace('/', "\\")),
            None => path.replace('/', "\\"),
        }
        .into_bytes(),
    )
}

/// Windows path string for a `file://` URI (`None` if not a file URI).
pub fn uri_to_windows_path(uri: &str) -> Option<String> {
    String::from_utf8(uri_to_path_bytes(uri, true)?).ok()
}

/// Unix path bytes for a `file://` URI.
pub fn uri_to_unix_path(uri: &str) -> Option<Vec<u8>> {
    uri_to_path_bytes(uri, false)
}

/// Absolute path -> `file://` URI in the host's style. Relative paths are
/// resolved against the current directory.
pub fn path_to_uri(path: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    path_str_to_uri(&abs.to_string_lossy(), cfg!(windows))
}

/// `file://` URI -> path in the host's style. `None` for non-file URIs
/// (or remote hosts on Unix).
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(
            uri_to_path_bytes(uri, false)?,
        )))
    }
    #[cfg(not(unix))]
    {
        Some(PathBuf::from(
            String::from_utf8(uri_to_path_bytes(uri, true)?).ok()?,
        ))
    }
}

/// Canonical spelling of a URI for use as a map key: `file:` URIs are
/// decoded and re-encoded in Skink's style (drive letter upper-cased, one
/// escaping), anything else is returned unchanged. Style (Windows or Unix) is
/// inferred from the URI itself so the result is platform independent.
pub fn uri_key(uri: &str) -> String {
    let Some((authority, raw)) = split_file_uri(uri) else {
        return uri.to_string();
    };
    let local = authority.is_empty() || authority.eq_ignore_ascii_case("localhost");
    let decoded = percent_decode(raw);
    let windows = !local
        || String::from_utf8_lossy(&decoded)
            .strip_prefix('/')
            .is_some_and(|p| drive_prefix(p).is_some());
    if !windows {
        let mut out = String::from("file://");
        percent_encode_into(&mut out, &decoded);
        return out;
    }
    match uri_to_windows_path(uri) {
        Some(p) => path_str_to_uri(&p, true),
        None => uri.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_roundtrip_plain() {
        let u = path_str_to_uri("/home/me/src/main.rs", false);
        assert_eq!(u, "file:///home/me/src/main.rs");
        assert_eq!(uri_to_unix_path(&u).unwrap(), b"/home/me/src/main.rs");
    }

    #[test]
    fn unix_percent_encoding() {
        let u = path_str_to_uri("/tmp/a b/é#?%.rs", false);
        assert_eq!(u, "file:///tmp/a%20b/%C3%A9%23%3F%25.rs");
        assert_eq!(uri_to_unix_path(&u).unwrap(), "/tmp/a b/é#?%.rs".as_bytes());
    }

    #[test]
    fn unix_localhost_and_remote() {
        assert_eq!(uri_to_unix_path("file://localhost/x/y").unwrap(), b"/x/y");
        assert_eq!(uri_to_unix_path("file:///x").unwrap(), b"/x");
        assert!(uri_to_unix_path("file://otherhost/x").is_none());
        assert!(uri_to_unix_path("https://example.com/x").is_none());
        assert!(uri_to_unix_path("untitled:Untitled-1").is_none());
    }

    #[test]
    fn scheme_is_case_insensitive_and_fragment_ignored() {
        assert_eq!(uri_to_unix_path("FILE:///a/b#L10").unwrap(), b"/a/b");
    }

    #[test]
    fn bad_escapes_kept_literally() {
        assert_eq!(percent_decode("a%zzb%4"), b"a%zzb%4");
        assert_eq!(percent_decode("%41%62"), b"Ab");
        assert_eq!(percent_decode("%e4%BD%a0"), "你".as_bytes());
    }

    #[test]
    fn windows_drive_to_uri() {
        assert_eq!(
            path_str_to_uri(r"C:\Users\me\a b.rs", true),
            "file:///C:/Users/me/a%20b.rs"
        );
        assert_eq!(path_str_to_uri(r"c:\x", true), "file:///C:/x");
        assert_eq!(path_str_to_uri("c:/x/y", true), "file:///C:/x/y");
        assert_eq!(path_str_to_uri("D:", true), "file:///D:");
    }

    #[test]
    fn windows_verbatim_and_unc() {
        assert_eq!(path_str_to_uri(r"\\?\C:\x\y", true), "file:///C:/x/y");
        assert_eq!(
            path_str_to_uri(r"\\srv\share\f.rs", true),
            "file://srv/share/f.rs"
        );
        assert_eq!(
            path_str_to_uri(r"\\?\UNC\srv\share\f.rs", true),
            "file://srv/share/f.rs"
        );
    }

    #[test]
    fn windows_uri_to_path_variants() {
        for u in [
            "file:///C:/Users/me/a%20b.rs",
            "file:///c:/Users/me/a%20b.rs",
            "file:///c%3A/Users/me/a%20b.rs",
            "file:///C%3a/Users/me/a%20b.rs",
        ] {
            assert_eq!(
                uri_to_windows_path(u).unwrap(),
                r"C:\Users\me\a b.rs",
                "{u}"
            );
        }
        assert_eq!(uri_to_windows_path("file:///D:").unwrap(), "D:");
    }

    #[test]
    fn windows_unc_uri_to_path() {
        assert_eq!(
            uri_to_windows_path("file://Srv/share/f.rs").unwrap(),
            r"\\Srv\share\f.rs"
        );
    }

    #[test]
    fn windows_roundtrip() {
        for p in [r"C:\a\b c\d.rs", r"Z:\é\ü.txt", r"\\srv\share\x y"] {
            let u = path_str_to_uri(p, true);
            assert_eq!(uri_to_windows_path(&u).unwrap(), p);
        }
    }

    #[test]
    fn key_unifies_drive_letter_spellings() {
        let want = "file:///C:/Users/me/a.rs";
        for u in [
            "file:///c%3A/Users/me/a.rs",
            "file:///c:/Users/me/a.rs",
            "file:///C:/Users/me/a.rs",
            "file:///C%3A/Users/me/a.rs",
            "file://localhost/c:/Users/me/a.rs",
        ] {
            assert_eq!(uri_key(u), want, "{u}");
        }
    }

    #[test]
    fn key_unifies_escaping_on_unix() {
        assert_eq!(uri_key("file:///tmp/a%20b"), uri_key("file:///tmp/a%20b"));
        assert_eq!(uri_key("file:///tmp/%41"), "file:///tmp/A");
        assert_eq!(uri_key("file:///tmp/a b"), "file:///tmp/a%20b");
    }

    #[test]
    fn key_leaves_other_schemes() {
        assert_eq!(uri_key("untitled:Untitled-1"), "untitled:Untitled-1");
        assert_eq!(
            uri_key("jar:file:///x.jar!/A.class"),
            "jar:file:///x.jar!/A.class"
        );
    }

    #[cfg(unix)]
    #[test]
    fn host_roundtrip() {
        let p = Path::new("/tmp/skink test/ü.rs");
        let u = path_to_uri(p);
        assert_eq!(u, "file:///tmp/skink%20test/%C3%BC.rs");
        assert_eq!(uri_to_path(&u).unwrap(), p);
    }

    #[cfg(unix)]
    #[test]
    fn host_relative_becomes_absolute() {
        let u = path_to_uri(Path::new("rel/x.rs"));
        assert!(u.starts_with("file:///"));
        assert!(u.ends_with("/rel/x.rs"));
    }
}
