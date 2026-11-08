//! Key -> xterm byte sequence encoding.
//!
//! `skink_config::Key` has no F-keys or Insert, so this module defines a small
//! superset ([`TermKey`]) with `From<skink_config::Key>`. Encoding follows the
//! xterm "PC-style" function-key tables (`ctlseqs`, "PC-Style Function Keys"
//! and "Alt and Meta Keys"); it was written from that spec, not copied.
//! Not implemented: the kitty keyboard protocol and `modifyOtherKeys`, so
//! chords like Ctrl-Enter / Shift-Enter / Ctrl-1 are indistinguishable from
//! their unmodified forms (same as a legacy xterm).

use skink_config::{Key, KeySpec};

/// A key the terminal can encode. Superset of [`skink_config::Key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TermKey {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Enter,
    Tab,
    Backspace,
    Escape,
    /// Function key, `F(1)`..=`F(20)`.
    F(u8),
}

impl From<Key> for TermKey {
    fn from(k: Key) -> Self {
        match k {
            Key::Char(c) => TermKey::Char(c),
            Key::Up => TermKey::Up,
            Key::Down => TermKey::Down,
            Key::Left => TermKey::Left,
            Key::Right => TermKey::Right,
            Key::Home => TermKey::Home,
            Key::End => TermKey::End,
            Key::PageUp => TermKey::PageUp,
            Key::PageDown => TermKey::PageDown,
            Key::Enter => TermKey::Enter,
            Key::Tab => TermKey::Tab,
            Key::Backspace => TermKey::Backspace,
            Key::Delete => TermKey::Delete,
            Key::Escape => TermKey::Escape,
            Key::Insert => TermKey::Insert,
            Key::F(n) => TermKey::F(n),
            Key::BackTab => TermKey::Tab,
        }
    }
}

/// A key plus modifiers; what [`crate::Term::send_key`] accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyInput {
    pub key: TermKey,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyInput {
    pub fn new(key: TermKey) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }
    pub fn ctrl(mut self) -> Self {
        self.ctrl = true;
        self
    }
    pub fn alt(mut self) -> Self {
        self.alt = true;
        self
    }
    pub fn shift(mut self) -> Self {
        self.shift = true;
        self
    }
}

impl From<TermKey> for KeyInput {
    fn from(key: TermKey) -> Self {
        KeyInput::new(key)
    }
}

impl From<KeySpec> for KeyInput {
    fn from(s: KeySpec) -> Self {
        KeyInput {
            key: s.key.into(),
            ctrl: s.ctrl,
            alt: s.alt,
            // BackTab is Shift-Tab.
            shift: s.shift || s.key == Key::BackTab,
        }
    }
}

/// Encode `k` for a terminal whose application-cursor-keys mode (DECCKM) is
/// `app_cursor`. Returns the bytes to write to the pty (empty if the key has
/// no encoding, e.g. `F(0)`).
pub fn encode_key(k: KeyInput, app_cursor: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    // xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
    let m = 1 + u8::from(k.shift) + 2 * u8::from(k.alt) + 4 * u8::from(k.ctrl);
    let alt_prefix = |out: &mut Vec<u8>| {
        if k.alt {
            out.push(0x1b);
        }
    };
    match k.key {
        TermKey::Char(c) => {
            alt_prefix(&mut out);
            match ctrl_char(c).filter(|_| k.ctrl) {
                Some(b) => out.push(b),
                None => {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        TermKey::Enter => {
            alt_prefix(&mut out);
            out.push(b'\r');
        }
        TermKey::Tab => {
            if k.shift {
                out.extend_from_slice(b"\x1b[Z");
            } else {
                alt_prefix(&mut out);
                out.push(b'\t');
            }
        }
        TermKey::Backspace => {
            alt_prefix(&mut out);
            out.push(if k.ctrl { 0x08 } else { 0x7f });
        }
        TermKey::Escape => {
            alt_prefix(&mut out);
            out.push(0x1b);
        }
        TermKey::Up => cursor_key(&mut out, b'A', m, app_cursor),
        TermKey::Down => cursor_key(&mut out, b'B', m, app_cursor),
        TermKey::Right => cursor_key(&mut out, b'C', m, app_cursor),
        TermKey::Left => cursor_key(&mut out, b'D', m, app_cursor),
        TermKey::Home => cursor_key(&mut out, b'H', m, app_cursor),
        TermKey::End => cursor_key(&mut out, b'F', m, app_cursor),
        TermKey::Insert => tilde_key(&mut out, 2, m),
        TermKey::Delete => tilde_key(&mut out, 3, m),
        TermKey::PageUp => tilde_key(&mut out, 5, m),
        TermKey::PageDown => tilde_key(&mut out, 6, m),
        TermKey::F(n) => match n {
            1..=4 => {
                let fin = b'P' + (n - 1);
                if m == 1 {
                    out.extend_from_slice(&[0x1b, b'O', fin]);
                } else {
                    out.extend_from_slice(format!("\x1b[1;{m}{}", fin as char).as_bytes());
                }
            }
            5..=20 => {
                const CODES: [u8; 16] = [
                    15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34,
                ];
                tilde_key(&mut out, CODES[usize::from(n - 5)], m);
            }
            _ => {}
        },
    }
    out
}

fn cursor_key(out: &mut Vec<u8>, fin: u8, m: u8, app: bool) {
    if m != 1 {
        out.extend_from_slice(format!("\x1b[1;{m}{}", fin as char).as_bytes());
    } else if app {
        out.extend_from_slice(&[0x1b, b'O', fin]);
    } else {
        out.extend_from_slice(&[0x1b, b'[', fin]);
    }
}

fn tilde_key(out: &mut Vec<u8>, code: u8, m: u8) {
    if m == 1 {
        out.extend_from_slice(format!("\x1b[{code}~").as_bytes());
    } else {
        out.extend_from_slice(format!("\x1b[{code};{m}~").as_bytes());
    }
}

/// C0 control byte produced by Ctrl+`c`, per the classic terminal mapping.
fn ctrl_char(c: char) -> Option<u8> {
    match c {
        '@' | ' ' | '2' => Some(0x00),
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TermKey::*;

    fn enc(k: KeyInput) -> String {
        String::from_utf8(encode_key(k, false)).unwrap()
    }
    fn k(key: TermKey) -> KeyInput {
        KeyInput::new(key)
    }

    #[test]
    fn plain_and_ctrl_chars() {
        assert_eq!(enc(k(Char('a'))), "a");
        assert_eq!(enc(k(Char('é'))), "é");
        assert_eq!(enc(k(Char('a')).ctrl()), "\x01");
        assert_eq!(enc(k(Char('C')).ctrl()), "\x03");
        assert_eq!(enc(k(Char('z')).ctrl()), "\x1a");
        assert_eq!(enc(k(Char(' ')).ctrl()), "\0");
        assert_eq!(enc(k(Char('[')).ctrl()), "\x1b");
        assert_eq!(enc(k(Char('?')).ctrl()), "\x7f");
        // No legacy encoding: falls back to the plain character.
        assert_eq!(enc(k(Char('1')).ctrl()), "1");
    }

    #[test]
    fn alt_is_esc_prefix() {
        assert_eq!(enc(k(Char('x')).alt()), "\x1bx");
        assert_eq!(enc(k(Char('b')).alt().ctrl()), "\x1b\x02");
        assert_eq!(enc(k(Enter).alt()), "\x1b\r");
        assert_eq!(enc(k(Backspace).alt()), "\x1b\x7f");
        assert_eq!(enc(k(Escape).alt()), "\x1b\x1b");
    }

    #[test]
    fn simple_keys() {
        assert_eq!(enc(k(Enter)), "\r");
        assert_eq!(enc(k(Tab)), "\t");
        assert_eq!(enc(k(Tab).shift()), "\x1b[Z");
        assert_eq!(enc(k(Backspace)), "\x7f");
        assert_eq!(enc(k(Backspace).ctrl()), "\x08");
        assert_eq!(enc(k(Escape)), "\x1b");
    }

    #[test]
    fn cursor_keys_normal_and_application() {
        assert_eq!(enc(k(Up)), "\x1b[A");
        assert_eq!(enc(k(Down)), "\x1b[B");
        assert_eq!(enc(k(Right)), "\x1b[C");
        assert_eq!(enc(k(Left)), "\x1b[D");
        assert_eq!(enc(k(Home)), "\x1b[H");
        assert_eq!(enc(k(End)), "\x1b[F");
        assert_eq!(encode_key(k(Up), true), b"\x1bOA");
        assert_eq!(encode_key(k(Home), true), b"\x1bOH");
        assert_eq!(encode_key(k(End), true), b"\x1bOF");
    }

    #[test]
    fn modified_cursor_keys_ignore_application_mode() {
        assert_eq!(enc(k(Up).shift()), "\x1b[1;2A");
        assert_eq!(enc(k(Up).alt()), "\x1b[1;3A");
        assert_eq!(enc(k(Right).ctrl()), "\x1b[1;5C");
        assert_eq!(enc(k(Left).ctrl().shift()), "\x1b[1;6D");
        assert_eq!(enc(k(Home).ctrl().alt().shift()), "\x1b[1;8H");
        assert_eq!(encode_key(k(Right).ctrl(), true), b"\x1b[1;5C");
    }

    #[test]
    fn tilde_keys() {
        assert_eq!(enc(k(Insert)), "\x1b[2~");
        assert_eq!(enc(k(Delete)), "\x1b[3~");
        assert_eq!(enc(k(PageUp)), "\x1b[5~");
        assert_eq!(enc(k(PageDown)), "\x1b[6~");
        assert_eq!(enc(k(Delete).ctrl()), "\x1b[3;5~");
        assert_eq!(enc(k(PageDown).shift()), "\x1b[6;2~");
    }

    #[test]
    fn function_keys() {
        assert_eq!(enc(k(F(1))), "\x1bOP");
        assert_eq!(enc(k(F(4))), "\x1bOS");
        assert_eq!(enc(k(F(5))), "\x1b[15~");
        assert_eq!(enc(k(F(10))), "\x1b[21~");
        assert_eq!(enc(k(F(12))), "\x1b[24~");
        assert_eq!(enc(k(F(20))), "\x1b[34~");
        assert_eq!(enc(k(F(1)).ctrl()), "\x1b[1;5P");
        assert_eq!(enc(k(F(5)).shift()), "\x1b[15;2~");
        assert_eq!(enc(k(F(0))), "");
        assert_eq!(enc(k(F(21))), "");
    }

    #[test]
    fn from_keyspec() {
        let spec = skink_config::parse_key("Ctrl-Right").unwrap();
        assert_eq!(enc(spec.into()), "\x1b[1;5C");
        let spec = skink_config::parse_key("Alt-x").unwrap();
        assert_eq!(enc(spec.into()), "\x1bx");
    }
}
