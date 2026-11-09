//! Themes in the app: `"colorscheme"` (a name or "auto"), the `:theme`
//! picker with live preview (Esc puts the old theme back), and the
//! terminal-background answer that "auto" follows.

use crate::picker::{self, Item, Target};
use crate::theme::{self, Appearance, Theme, ThemeDirs};
use crate::{App, Overlay};

/// The `:theme` picker's lifecycle.
#[derive(Debug, Clone, Copy, Default)]
pub enum ThemePick {
    #[default]
    Closed,
    /// Open; `before` is put back on Esc.
    Open { before: Theme },
}

pub const PICKER_TITLE: &str = "Theme";

impl App {
    fn theme_dirs(&self) -> ThemeDirs {
        ThemeDirs::new(self.config_dir.clone())
    }

    /// The theme name in effect: the setting, with "auto" resolved.
    pub(crate) fn theme_name(&self) -> String {
        match self.settings.colorscheme.as_str() {
            "auto" => self.appearance.theme().to_owned(),
            n => theme::canonical_name(n),
        }
    }

    /// Load the configured theme (after settings change or the terminal
    /// said what its background is). Errors keep the current theme.
    pub(crate) fn apply_colorscheme(&mut self) -> Option<String> {
        let name = self.theme_name();
        match self.theme_dirs().load(&name, self.theme.true_color) {
            Ok(t) => {
                self.theme = t;
                None
            }
            Err(e) => Some(format!("colorscheme: {e}")),
        }
    }

    /// The terminal answered the background query.
    pub(crate) fn set_appearance(&mut self, a: Appearance) {
        self.appearance = a;
        if self.settings.colorscheme == "auto" && matches!(self.theme_pick, ThemePick::Closed) {
            let _ = self.apply_colorscheme();
        }
    }

    /// Whether to ask the terminal for its background at start.
    pub(crate) fn wants_appearance(&self) -> bool {
        self.settings.colorscheme == "auto"
    }

    /// `:theme NAME` applies a theme for this session; `:theme` lists them
    /// with a live preview.
    pub(crate) fn theme_command(&mut self, arg: Option<&str>) {
        let Some(name) = arg else {
            return self.open_theme_picker();
        };
        let name = theme::canonical_name(name);
        self.theme_pick = ThemePick::Closed;
        match self.theme_dirs().load(&name, self.theme.true_color) {
            Ok(t) => {
                self.theme = t;
                self.doc.editor.message = Some(format!(
                    "Theme {name} (to keep it: \"colorscheme\": \"{name}\" in :settings)"
                ));
            }
            Err(e) => self.doc.editor.message = Some(e),
        }
    }

    fn open_theme_picker(&mut self) {
        let current = self.theme_name();
        let mut items: Vec<Item> = self
            .theme_dirs()
            .list()
            .into_iter()
            .map(|(name, origin)| Item {
                detail: if name == current {
                    "current".into()
                } else {
                    String::new()
                },
                target: Target::Command(format!("theme {name}")),
                label: name,
                tag: origin.tag(),
            })
            .collect();
        // The current theme first, so the preview starts on it.
        if let Some(i) = items.iter().position(|it| it.label == current) {
            let it = items.remove(i);
            items.insert(0, it);
        }
        self.theme_pick = ThemePick::Open { before: self.theme };
        self.set_overlay(Overlay::Picker(Box::new(picker::Picker::choices(
            PICKER_TITLE,
            items,
        ))));
    }

    /// After a key in the picker: preview the selection, or on Esc put the
    /// old theme back.
    pub(crate) fn theme_picker_step(&mut self, escaped: bool) {
        let ThemePick::Open { before } = self.theme_pick else {
            return;
        };
        let target = match &self.overlay {
            Overlay::Picker(p) if p.mode == picker::Mode::Choices(PICKER_TITLE.into()) => {
                p.current()
            }
            _ => {
                // Closed: Enter ran `:theme NAME` (which closed the pick).
                self.theme_pick = ThemePick::Closed;
                if escaped {
                    self.theme = before;
                }
                return;
            }
        };
        if let Some(Target::Command(c)) = target
            && let Some(name) = c.strip_prefix("theme ")
            && let Ok(t) = self.theme_dirs().load(name, self.theme.true_color)
        {
            self.theme = t;
        }
    }
}

/// Terminals known to answer an OSC 11 background query (others may echo
/// it or say nothing, so they are not asked).
pub fn answers_background_query() -> bool {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let term = var("TERM");
    if term == "linux" || term == "dumb" || term.is_empty() {
        return false;
    }
    let program = var("TERM_PROGRAM");
    matches!(
        program.as_str(),
        "ghostty"
            | "iTerm.app"
            | "Apple_Terminal"
            | "WezTerm"
            | "vscode"
            | "kitty"
            | "tmux"
            | "rio"
    ) || [
        "kitty",
        "ghostty",
        "alacritty",
        "foot",
        "wezterm",
        "contour",
    ]
    .iter()
    .any(|t| term.contains(t))
        || std::env::var_os("VTE_VERSION").is_some()
        || std::env::var_os("KONSOLE_VERSION").is_some()
}

/// `COLORFGBG` ("15;0": light text on black), set by some terminals: free
/// to read, so it is used before asking.
pub fn appearance_from_env() -> Option<Appearance> {
    let v = std::env::var("COLORFGBG").ok()?;
    let bg: u8 = v.rsplit(';').next()?.trim().parse().ok()?;
    Some(match bg {
        7 | 9..=15 => Appearance::Light,
        _ => Appearance::Dark,
    })
}

/// Parse an OSC 11 answer (`ESC ] 11 ; rgb:RRRR/GGGG/BBBB` + BEL or ST)
/// somewhere in `buf`.
pub fn parse_background_reply(buf: &[u8]) -> Option<Appearance> {
    let s = String::from_utf8_lossy(buf);
    let at = s.find("]11;rgb:")? + "]11;rgb:".len();
    let rest = &s[at..];
    let end = rest.find(['\x07', '\x1b']).unwrap_or(rest.len());
    let mut parts = rest[..end].split('/').map(|h| {
        // 1-4 hex digits; scale to 8 bits.
        let n = u32::from_str_radix(h, 16).ok()?;
        let max = (1u32 << (4 * h.len().clamp(1, 4))) - 1;
        Some((n * 255 / max) as u8)
    });
    let (r, g, b) = (parts.next()??, parts.next()??, parts.next()??);
    Some(Appearance::of((r, g, b)))
}

/// Ask the terminal for its background and wait briefly for the answer.
/// Runs on the input thread before it starts reading keys, so the first
/// frame never waits for it. A DA1 query rides along: every terminal
/// answers that, so the wait ends as soon as the terminal has replied.
#[cfg(unix)]
pub fn query_background(timeout: std::time::Duration) -> Option<Appearance> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use std::io::{Read, Write};
    use std::time::Instant;
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    let mut out = std::io::stdout();
    out.write_all(b"\x1b]11;?\x1b\\\x1b[c").ok()?;
    out.flush().ok()?;
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let wait = Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: left.subsec_nanos() as _,
        };
        let ready = poll(&mut [PollFd::new(&tty, PollFlags::IN)], Some(&wait));
        if !matches!(ready, Ok(n) if n > 0) {
            break;
        }
        let mut chunk = [0u8; 256];
        let got = match tty.read(&mut chunk) {
            Ok(n) if n > 0 => n,
            _ => break,
        };
        buf.extend_from_slice(&chunk[..got]);
        // The DA1 answer (ESC [ ? ... c) comes after the OSC 11 one.
        let s = String::from_utf8_lossy(&buf);
        if let Some(i) = s.find("\x1b[?")
            && s[i..].contains('c')
        {
            break;
        }
    }
    parse_background_reply(&buf)
}

#[cfg(not(unix))]
pub fn query_background(_timeout: std::time::Duration) -> Option<Appearance> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_replies_parse_in_any_digit_width() {
        let dark = b"\x1b]11;rgb:1a1a/1b1b/2626\x1b\\\x1b[?62;22c";
        assert_eq!(parse_background_reply(dark), Some(Appearance::Dark));
        assert_eq!(
            parse_background_reply(b"\x1b]11;rgb:ff/fa/f0\x07"),
            Some(Appearance::Light)
        );
        assert_eq!(parse_background_reply(b"\x1b[?62c"), None);
    }
}
