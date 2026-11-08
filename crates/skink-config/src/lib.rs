//! Micro-compatible config: key names, default keybindings (settings and
//! colorschemes follow in Phase 1). No terminal-library dependency.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

/// Parse a Micro key name such as `Ctrl-q`, `CtrlQ`, `Alt-x`, `Shift-Up`,
/// `CtrlHome` or `PageDown`.
pub fn parse_key(name: &str) -> Option<KeySpec> {
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    let mut rest = name;
    loop {
        if let Some(r) = rest.strip_prefix("Ctrl") {
            ctrl = true;
            rest = r.strip_prefix('-').unwrap_or(r);
        } else if let Some(r) = rest.strip_prefix("Alt") {
            alt = true;
            rest = r.strip_prefix('-').unwrap_or(r);
        } else if let Some(r) = rest.strip_prefix("Shift") {
            shift = true;
            rest = r.strip_prefix('-').unwrap_or(r);
        } else {
            break;
        }
    }
    let key = match rest {
        "Up" => Key::Up,
        "Down" => Key::Down,
        "Left" => Key::Left,
        "Right" => Key::Right,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "Enter" => Key::Enter,
        "Tab" => Key::Tab,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "Escape" => Key::Escape,
        s => {
            let mut it = s.chars();
            let c = it.next()?;
            if it.next().is_some() {
                return None;
            }
            Key::Char(if ctrl || alt {
                c.to_ascii_lowercase()
            } else {
                c
            })
        }
    };
    Some(KeySpec {
        ctrl,
        alt,
        shift,
        key,
    })
}

/// Default bindings, a subset of Micro's `bindings.json` (MIT).
pub const DEFAULT_BINDINGS: &[(&str, &str)] = &[
    ("Up", "CursorUp"),
    ("Down", "CursorDown"),
    ("Left", "CursorLeft"),
    ("Right", "CursorRight"),
    ("PageUp", "CursorPageUp"),
    ("PageDown", "CursorPageDown"),
    ("Home", "StartOfLine"),
    ("End", "EndOfLine"),
    ("CtrlHome", "CursorStart"),
    ("CtrlEnd", "CursorEnd"),
    ("Ctrl-q", "Quit"),
];

#[derive(Debug, Clone, Default)]
pub struct Keymap {
    map: HashMap<KeySpec, String>,
}

impl Keymap {
    pub fn bind(&mut self, key: &str, command: &str) -> bool {
        match parse_key(key) {
            Some(k) => {
                self.map.insert(k, command.to_owned());
                true
            }
            None => false,
        }
    }

    pub fn command(&self, key: &KeySpec) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    pub fn defaults() -> Self {
        let mut m = Self::default();
        for (k, c) in DEFAULT_BINDINGS {
            assert!(m.bind(k, c), "bad default binding {k}");
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_micro_names() {
        let q = KeySpec {
            ctrl: true,
            alt: false,
            shift: false,
            key: Key::Char('q'),
        };
        assert_eq!(parse_key("Ctrl-q"), Some(q));
        assert_eq!(parse_key("CtrlQ"), Some(q));
        assert_eq!(
            parse_key("CtrlHome").map(|k| (k.ctrl, k.key)),
            Some((true, Key::Home))
        );
        assert_eq!(
            parse_key("Shift-Up").map(|k| (k.shift, k.key)),
            Some((true, Key::Up))
        );
        assert_eq!(
            parse_key("Alt-/").map(|k| (k.alt, k.key)),
            Some((true, Key::Char('/')))
        );
        assert_eq!(parse_key("Bogus"), None);
    }

    #[test]
    fn defaults_load() {
        let m = Keymap::defaults();
        let q = parse_key("Ctrl-q").unwrap();
        assert_eq!(m.command(&q), Some("Quit"));
    }
}
