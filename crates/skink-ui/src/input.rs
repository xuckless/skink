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
