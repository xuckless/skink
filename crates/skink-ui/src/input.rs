use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use skink_config::{Key, KeySpec};

/// Translate a crossterm key event into Skink's terminal-library-free form.
pub fn key_spec(ev: KeyEvent) -> Option<KeySpec> {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);
    let mut shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    let key = match ev.code {
        KeyCode::Char(c) => {
            if (ctrl || alt) && c.is_ascii_uppercase() && !ctrl {
                shift = true;
            }
            Key::Char(if ctrl || alt {
                c.to_ascii_lowercase()
            } else {
                c
            })
        }
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => {
            shift = false;
            Key::BackTab
        }
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Esc => Key::Escape,
        KeyCode::F(n) if (1..=20).contains(&n) => Key::F(n),
        _ => return None,
    };
    // Shift is already folded into the char for printable keys.
    if matches!(key, Key::Char(_)) && !alt {
        shift = false;
    }
    Some(KeySpec {
        ctrl,
        alt,
        shift,
        key,
    })
}

/// What US-layout macOS Option(+Shift) produces per key, as the plain
/// character the terminal delivers: `(composed, key, shift)`. Dead keys
/// (Option+E, I, N, U, `) compose with the next key and cannot be mapped.
/// Source: Apple's US keyboard layout.
const MAC_OPTION_US: &[(char, char, bool)] = &[
    ('å', 'a', false),
    ('∫', 'b', false),
    ('ç', 'c', false),
    ('∂', 'd', false),
    ('ƒ', 'f', false),
    ('©', 'g', false),
    ('˙', 'h', false),
    ('∆', 'j', false),
    ('˚', 'k', false),
    ('¬', 'l', false),
    ('µ', 'm', false),
    ('ø', 'o', false),
    ('π', 'p', false),
    ('œ', 'q', false),
    ('®', 'r', false),
    ('ß', 's', false),
    ('†', 't', false),
    ('√', 'v', false),
    ('∑', 'w', false),
    ('≈', 'x', false),
    ('¥', 'y', false),
    ('Ω', 'z', false),
    ('≥', '.', false),
    ('≤', ',', false),
    ('÷', '/', false),
    ('“', '[', false),
    ('‘', ']', false),
    ('«', '\\', false),
    ('≠', '=', false),
    ('–', '-', false),
    ('º', '0', false),
    ('¡', '1', false),
    ('™', '2', false),
    ('£', '3', false),
    ('¢', '4', false),
    ('∞', '5', false),
    ('§', '6', false),
    ('¶', '7', false),
    ('•', '8', false),
    ('ª', '9', false),
    // Shift+Option
    ('Å', 'a', true),
    ('ı', 'b', true),
    ('Ç', 'c', true),
    ('Î', 'd', true),
    ('Ï', 'f', true),
    ('˝', 'g', true),
    ('Ó', 'h', true),
    ('Ô', 'j', true),
    ('\u{F8FF}', 'k', true), // Apple logo
    ('Ò', 'l', true),
    ('Â', 'm', true),
    ('Ø', 'o', true),
    ('∏', 'p', true),
    ('Œ', 'q', true),
    ('‰', 'r', true),
    ('Í', 's', true),
    ('ˇ', 't', true),
    ('◊', 'v', true),
    ('„', 'w', true),
    ('˛', 'x', true),
    ('Á', 'y', true),
    ('¸', 'z', true),
    ('˘', '.', true),
    ('¯', ',', true),
    // Option+E/I/N/U are dead keys on the US layout, but with Shift they type
    // these spacing accents directly (Alt-Shift-N next change, Alt-Shift-E
    // send errors, Alt-Shift-U jump forward).
    ('\u{B4}', 'e', true),  // ´
    ('\u{2C6}', 'i', true), // ˆ
    ('\u{2DC}', 'n', true), // ˜
    ('\u{A8}', 'u', true),  // ¨
];

/// The Alt chord a macOS Option character stands for, if any.
pub fn mac_option_chord(c: char) -> Option<KeySpec> {
    MAC_OPTION_US
        .iter()
        .find(|(composed, ..)| *composed == c)
        .map(|&(_, key, shift)| KeySpec {
            ctrl: false,
            alt: true,
            shift,
            key: Key::Char(key),
        })
}

/// Turn a typed Option character back into its Alt chord (macOS terminals
/// send Option+Z as "Ω"). `Auto` translates only chords that are bound, so
/// people who type Ω or å on purpose still can; `Always` translates every
/// mapped character; `Never` keeps the character. Off macOS only `Always`
/// applies. Pure: the caller passes the platform and the binding lookup.
pub fn translate_option(
    spec: KeySpec,
    mode: skink_config::MacOption,
    on_mac: bool,
    is_bound: impl Fn(&KeySpec) -> bool,
) -> KeySpec {
    use skink_config::MacOption;
    let Key::Char(c) = spec.key else {
        return spec;
    };
    if spec.ctrl || spec.alt {
        return spec;
    }
    let Some(chord) = mac_option_chord(c) else {
        return spec;
    };
    match (mode, on_mac) {
        (MacOption::Always, _) => chord,
        (MacOption::Auto, true) if is_bound(&chord) => chord,
        _ => spec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skink_config::MacOption;

    #[test]
    fn shift_option_dead_key_letters_reach_the_git_chords() {
        // Stock macOS Ghostty: Shift+Option+N types "˜", not Alt-Shift-N.
        for (c, key) in [
            ('\u{2DC}', "Alt-N"),
            ('\u{B4}', "Alt-E"),
            ('\u{A8}', "Alt-U"),
        ] {
            assert_eq!(
                translate_option(ch(c), MacOption::Auto, true, |_| true),
                skink_config::parse_key(key).unwrap(),
                "{c}"
            );
        }
    }

    fn ch(c: char) -> KeySpec {
        KeySpec {
            ctrl: false,
            alt: false,
            shift: false,
            key: Key::Char(c),
        }
    }

    #[test]
    fn option_characters_become_bound_alt_chords_on_macos_only() {
        let keymap = skink_config::Keymap::defaults();
        let bound = |k: &KeySpec| keymap.command(k).is_some();
        let alt_z = skink_config::parse_key("Alt-z").unwrap();
        // Owner report: Option+Z typed "Ω" instead of zooming.
        assert_eq!(
            translate_option(ch('Ω'), MacOption::Auto, true, bound),
            alt_z
        );
        assert_eq!(
            translate_option(ch('≥'), MacOption::Auto, true, bound),
            skink_config::parse_key("Alt-.").unwrap()
        );
        // Unbound chord (Alt-Y): the character is typed as is.
        assert_eq!(
            translate_option(ch('¥'), MacOption::Auto, true, bound),
            ch('¥')
        );
        // Never, and other platforms in Auto: untouched.
        assert_eq!(
            translate_option(ch('Ω'), MacOption::Never, true, bound),
            ch('Ω')
        );
        assert_eq!(
            translate_option(ch('Ω'), MacOption::Auto, false, bound),
            ch('Ω')
        );
        assert!(translate_option(ch('¥'), MacOption::Always, false, bound).alt);
        // Shift+Option+G is Alt-Shift-G (type definition).
        assert_eq!(
            translate_option(ch('˝'), MacOption::Auto, true, bound),
            skink_config::parse_key("Alt-G").unwrap()
        );
        // Ordinary letters never change.
        assert_eq!(
            translate_option(ch('z'), MacOption::Always, true, bound),
            ch('z')
        );
    }
}

/// The rest of an escape sequence that arrived split from its ESC.
///
/// On terminals without the kitty keyboard protocol, an F-key is `ESC [ 1 9 ~`.
/// When the ESC is read on its own (a slow link, or Escape pressed just before
/// the F-key), the terminal library reports Escape and then the tail as typed
/// text, and `[19~` landed in the file. A `[` or `O` right after Escape starts
/// such a tail; it is dropped up to its final byte. Nobody types `[` within
/// a few milliseconds of Escape by hand.
#[derive(Debug, Clone, Copy, Default)]
pub enum EscTail {
    #[default]
    Idle,
    /// Escape was just pressed.
    AfterEsc(std::time::Instant),
    /// Inside a CSI tail (`[`, parameters, then a final byte).
    Csi(std::time::Instant),
    /// After SS3 (`O`): one final byte.
    Ss3(std::time::Instant),
}

/// How soon after Escape a `[` counts as the tail of a sequence.
const TAIL_WINDOW: std::time::Duration = std::time::Duration::from_millis(30);

impl EscTail {
    /// Whether to drop `ev` (part of a split escape sequence).
    pub fn drop_key(&mut self, ev: &KeyEvent, now: std::time::Instant) -> bool {
        let plain = |ev: &KeyEvent| (ev.modifiers - KeyModifiers::SHIFT).is_empty();
        let next = match (*self, ev.code) {
            (_, KeyCode::Esc) if plain(ev) => (EscTail::AfterEsc(now), false),
            (EscTail::AfterEsc(t), KeyCode::Char(c @ ('[' | 'O')))
                if plain(ev) && now.duration_since(t) <= TAIL_WINDOW =>
            {
                let tail = match c {
                    '[' => EscTail::Csi(now),
                    _ => EscTail::Ss3(now),
                };
                (tail, true)
            }
            (EscTail::Csi(since) | EscTail::Ss3(since), KeyCode::Char(c))
                if plain(ev) && now.duration_since(since) <= TAIL_WINDOW =>
            {
                let csi = matches!(*self, EscTail::Csi(_));
                match c as u32 {
                    // Parameters and intermediates: more to come (CSI only).
                    0x20..=0x3f if csi => (EscTail::Csi(now), true),
                    // The final byte ends it.
                    0x40..=0x7e => (EscTail::Idle, true),
                    _ => (EscTail::Idle, false),
                }
            }
            _ => (EscTail::Idle, false),
        };
        *self = next.0;
        next.1
    }
}
