use std::path::{Path, PathBuf};

/// A path argument with an optional 1-based `:line[:col]` suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: Option<usize>,
    pub col: Option<usize>,
}

/// Parse `path`, `path:line` or `path:line:col`.
///
/// A path that exists verbatim always wins, so files literally named `a:1`
/// still open, and Windows drive prefixes (`C:\x`) are never split because
/// only all-digit trailing segments are treated as positions.
pub fn parse_location(arg: &str, exists: impl Fn(&Path) -> bool) -> Location {
    let whole = Location {
        path: PathBuf::from(arg),
        line: None,
        col: None,
    };
    if exists(&whole.path) {
        return whole;
    }
    let num = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    if let Some((rest, last)) = arg.rsplit_once(':').filter(|(r, _)| !r.is_empty())
        && let Some(n) = num(last)
    {
        if let Some((rest2, mid)) = rest.rsplit_once(':').filter(|(r, _)| !r.is_empty())
            && let Some(l) = num(mid)
        {
            return Location {
                path: rest2.into(),
                line: Some(l),
                col: Some(n),
            };
        }
        return Location {
            path: rest.into(),
            line: Some(n),
            col: None,
        };
    }
    whole
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &Path) -> bool {
        false
    }

    #[test]
    fn plain_path() {
        let l = parse_location("src/main.rs", none);
        assert_eq!(
            (l.path.as_path(), l.line, l.col),
            (Path::new("src/main.rs"), None, None)
        );
    }

    #[test]
    fn line_and_col() {
        let l = parse_location("a.rs:12:3", none);
        assert_eq!(
            (l.path.as_path(), l.line, l.col),
            (Path::new("a.rs"), Some(12), Some(3))
        );
        let l = parse_location("a.rs:12", none);
        assert_eq!(
            (l.path.as_path(), l.line, l.col),
            (Path::new("a.rs"), Some(12), None)
        );
    }

    #[test]
    fn existing_path_wins() {
        let l = parse_location("a.rs:12", |p| p == Path::new("a.rs:12"));
        assert_eq!((l.path.as_path(), l.line), (Path::new("a.rs:12"), None));
    }

    #[test]
    fn windows_drive_is_not_split() {
        let l = parse_location(r"C:\x\a.rs", none);
        assert_eq!((l.path.as_path(), l.line), (Path::new(r"C:\x\a.rs"), None));
        let l = parse_location(r"C:\x\a.rs:7", none);
        assert_eq!(
            (l.path.as_path(), l.line),
            (Path::new(r"C:\x\a.rs"), Some(7))
        );
        let l = parse_location("C:5", none); // drive-relative path, not a position
        assert_eq!(l.line, Some(5));
    }
}
