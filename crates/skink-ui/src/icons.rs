//! Nerd Font glyphs (https://www.nerdfonts.com, glyph codepoints are public)
//! with a plain-ASCII fallback. The font itself is the terminal's: a TUI cannot
//! ship fonts. Ghostty, Kitty and WezTerm render these without extra setup;
//! others need a Nerd Font (see `scripts/install-nerd-font.sh`).
//! Set `SKINK_ICONS=ascii` (or `nerd`) to override auto-detection.

use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconSet {
    Nerd,
    Ascii,
}

impl IconSet {
    pub fn auto() -> Self {
        match std::env::var("SKINK_ICONS").as_deref() {
            Ok("ascii" | "off" | "none") => IconSet::Ascii,
            Ok("nerd") => IconSet::Nerd,
            _ => {
                // Linux virtual consoles and dumb terminals have no glyphs.
                let term = std::env::var("TERM").unwrap_or_default();
                if term == "linux" || term == "dumb" {
                    IconSet::Ascii
                } else {
                    IconSet::Nerd
                }
            }
        }
    }

    fn pick(self, nerd: &'static str, ascii: &'static str) -> &'static str {
        if self == IconSet::Nerd { nerd } else { ascii }
    }

    pub fn folder(self, open: bool) -> &'static str {
        if open {
            self.pick("\u{f07c}", "v")
        } else {
            self.pick("\u{f07b}", ">")
        }
    }

    /// Powerline-style rounded pill ends and arrow separators.
    pub fn pill_left(self) -> &'static str {
        self.pick("\u{e0b6}", "")
    }
    pub fn pill_right(self) -> &'static str {
        self.pick("\u{e0b4}", "")
    }
    pub fn branch(self) -> &'static str {
        self.pick("\u{e0a0}", "git:")
    }
    pub fn dirty(self) -> &'static str {
        self.pick("\u{f111}", "*")
    }
    pub fn check(self) -> &'static str {
        self.pick("\u{f00c}", "ok")
    }
    pub fn error(self) -> &'static str {
        self.pick("\u{f057}", "E")
    }
    pub fn warning(self) -> &'static str {
        self.pick("\u{f071}", "W")
    }
    pub fn info(self) -> &'static str {
        self.pick("\u{f05a}", "i")
    }
    /// Code actions available on this line.
    /// Debugger: a breakpoint the adapter bound (or not yet asked about).
    pub fn breakpoint(self) -> &'static str {
        self.pick("\u{25cf}", "B")
    }
    /// A breakpoint with a condition or hit count.
    pub fn breakpoint_cond(self) -> &'static str {
        self.pick("\u{25c6}", "C")
    }
    /// A logpoint (logs instead of stopping).
    pub fn logpoint(self) -> &'static str {
        self.pick("\u{25c7}", "L")
    }
    /// A breakpoint the adapter could not bind.
    pub fn breakpoint_off(self) -> &'static str {
        self.pick("\u{25cb}", "b")
    }
    /// The line the debugger is stopped at.
    pub fn stopped(self) -> &'static str {
        self.pick("\u{25b6}", "@")
    }
    pub fn bulb(self) -> &'static str {
        self.pick("\u{f0eb}", "*")
    }
    pub fn terminal(self) -> &'static str {
        self.pick("\u{f120}", "$")
    }
    pub fn lock(self) -> &'static str {
        self.pick("\u{f023}", "ro")
    }
    /// Gutter mark of a closed fold.
    pub fn folded(self) -> &'static str {
        self.pick("\u{25b8}", ">")
    }
    /// Gutter mark of a line that starts a foldable block.
    pub fn foldable(self) -> &'static str {
        self.pick("\u{25be}", "v")
    }
    pub fn guide(self) -> &'static str {
        self.pick("\u{2502}", "|")
    }

    /// Icon for a file, by name then extension.
    pub fn file(self, path: &Path) -> &'static str {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let nerd = match (name, ext) {
            ("Cargo.toml" | "Cargo.lock", _) => "\u{e7a8}",
            ("README.md" | "README", _) => "\u{f48a}",
            ("LICENSE" | "LICENSE-MIT" | "LICENSE-APACHE", _) => "\u{f0219}",
            (".gitignore" | ".gitattributes", _) => "\u{e702}",
            ("Dockerfile", _) => "\u{f308}",
            (_, "rs") => "\u{e7a8}",
            (_, "py") => "\u{e73c}",
            (_, "js" | "mjs" | "cjs" | "jsx") => "\u{e74e}",
            (_, "ts" | "tsx") => "\u{e628}",
            (_, "go") => "\u{e627}",
            (_, "zig") => "\u{e6a9}",
            (_, "json") => "\u{e60b}",
            (_, "toml" | "yaml" | "yml" | "ini" | "conf") => "\u{e615}",
            (_, "md" | "markdown") => "\u{e609}",
            (_, "html" | "htm") => "\u{e736}",
            (_, "css" | "scss") => "\u{e749}",
            (_, "sh" | "bash" | "zsh") => "\u{f489}",
            (_, "lock") => "\u{f023}",
            (_, "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico") => "\u{f1c5}",
            (_, "c" | "h") => "\u{e61e}",
            (_, "cpp" | "cc" | "hpp") => "\u{e61d}",
            _ => "\u{f15b}",
        };
        self.pick(nerd, "-")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nerd_and_ascii() {
        let p = Path::new("src/main.rs");
        assert_eq!(IconSet::Nerd.file(p), "\u{e7a8}");
        assert_eq!(IconSet::Ascii.file(p), "-");
        assert_eq!(IconSet::Ascii.pill_left(), "");
        assert_eq!(IconSet::Nerd.file(Path::new("x.unknown")), "\u{f15b}");
    }
}
