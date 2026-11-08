//! Mouse report encoding (X10 / UTF-8 / SGR) per xterm `ctlseqs`
//! ("Mouse Tracking"). Written from the spec.

use crate::vt::{MouseProtocolEncoding as Enc, MouseProtocolMode as Mode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down(MouseButton),
    Up(MouseButton),
    /// Motion with a button held.
    Drag(MouseButton),
    /// Motion with no button held (only reported in any-motion mode).
    Move,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

/// A mouse event in pane-local cell coordinates (0-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl MouseEvent {
    pub fn new(kind: MouseKind, col: u16, row: u16) -> Self {
        Self {
            kind,
            col,
            row,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }
}

fn button_code(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// Encode `ev` for the application's requested `mode`/`enc`. Returns `None`
/// when the app has not asked for this class of event (or it cannot be
/// represented), in which case the host UI should handle it itself.
pub(crate) fn encode_mouse(ev: MouseEvent, mode: Mode, enc: Enc) -> Option<Vec<u8>> {
    if mode == Mode::None {
        return None;
    }
    // Which event classes each mode reports (X10 mode: presses only).
    let wanted = match ev.kind {
        MouseKind::Down(_) => true,
        MouseKind::Up(_)
        | MouseKind::ScrollUp
        | MouseKind::ScrollDown
        | MouseKind::ScrollLeft
        | MouseKind::ScrollRight => mode != Mode::Press,
        MouseKind::Drag(_) => matches!(mode, Mode::ButtonMotion | Mode::AnyMotion),
        MouseKind::Move => mode == Mode::AnyMotion,
    };
    if !wanted {
        return None;
    }
    let (mut cb, release) = match ev.kind {
        MouseKind::Down(b) => (button_code(b), false),
        MouseKind::Up(b) => (button_code(b), true),
        MouseKind::Drag(b) => (button_code(b) + 32, false),
        MouseKind::Move => (3 + 32, false),
        MouseKind::ScrollUp => (64, false),
        MouseKind::ScrollDown => (65, false),
        MouseKind::ScrollLeft => (66, false),
        MouseKind::ScrollRight => (67, false),
    };
    // X10 mode (9) reports no modifiers.
    if mode != Mode::Press {
        cb += 4 * u32::from(ev.shift) + 8 * u32::from(ev.alt) + 16 * u32::from(ev.ctrl);
    }
    let x = u32::from(ev.col) + 1;
    let y = u32::from(ev.row) + 1;
    match enc {
        Enc::Sgr => {
            let fin = if release { 'm' } else { 'M' };
            Some(format!("\x1b[<{cb};{x};{y}{fin}").into_bytes())
        }
        Enc::Default | Enc::Utf8 => {
            // Legacy encoding has no per-button release: button 3 = release.
            if release {
                cb = (cb & !3) | 3;
            }
            let mut out = vec![0x1b, b'[', b'M'];
            for v in [cb + 32, x + 32, y + 32] {
                if enc == Enc::Utf8 {
                    let c = char::from_u32(v)?;
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                } else if v <= 255 {
                    out.push(v as u8);
                } else {
                    return None; // coordinate not representable
                }
            }
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MouseButton::*;
    use MouseKind::*;

    fn ev(kind: MouseKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent::new(kind, col, row)
    }
    fn sgr(e: MouseEvent, m: Mode) -> Option<String> {
        encode_mouse(e, m, Enc::Sgr).map(|v| String::from_utf8(v).unwrap())
    }

    #[test]
    fn disabled_when_app_did_not_ask() {
        assert_eq!(
            encode_mouse(ev(Down(Left), 0, 0), Mode::None, Enc::Sgr),
            None
        );
    }

    #[test]
    fn sgr_click_and_release() {
        let m = Mode::PressRelease;
        assert_eq!(sgr(ev(Down(Left), 0, 0), m).unwrap(), "\x1b[<0;1;1M");
        assert_eq!(sgr(ev(Up(Left), 9, 4), m).unwrap(), "\x1b[<0;10;5m");
        assert_eq!(sgr(ev(Down(Right), 2, 3), m).unwrap(), "\x1b[<2;3;4M");
        assert_eq!(sgr(ev(Down(Middle), 2, 3), m).unwrap(), "\x1b[<1;3;4M");
    }

    #[test]
    fn sgr_modifiers_and_wheel() {
        let m = Mode::PressRelease;
        let mut e = ev(ScrollUp, 5, 5);
        assert_eq!(sgr(e, m).unwrap(), "\x1b[<64;6;6M");
        e.kind = ScrollDown;
        assert_eq!(sgr(e, m).unwrap(), "\x1b[<65;6;6M");
        e.ctrl = true;
        e.shift = true;
        assert_eq!(sgr(e, m).unwrap(), "\x1b[<85;6;6M");
    }

    #[test]
    fn motion_filtering() {
        let drag = ev(Drag(Left), 1, 1);
        assert_eq!(sgr(drag, Mode::PressRelease), None);
        assert_eq!(sgr(drag, Mode::ButtonMotion).unwrap(), "\x1b[<32;2;2M");
        assert_eq!(sgr(ev(Move, 1, 1), Mode::ButtonMotion), None);
        assert_eq!(
            sgr(ev(Move, 1, 1), Mode::AnyMotion).unwrap(),
            "\x1b[<35;2;2M"
        );
        // X10 mode: presses only.
        assert!(sgr(ev(Down(Left), 0, 0), Mode::Press).is_some());
        assert_eq!(sgr(ev(Up(Left), 0, 0), Mode::Press), None);
        assert_eq!(sgr(ev(ScrollUp, 0, 0), Mode::Press), None);
    }

    #[test]
    fn legacy_encoding() {
        let m = Mode::PressRelease;
        assert_eq!(
            encode_mouse(ev(Down(Left), 0, 0), m, Enc::Default).unwrap(),
            b"\x1b[M !!"
        );
        assert_eq!(
            encode_mouse(ev(Up(Right), 0, 0), m, Enc::Default).unwrap(),
            b"\x1b[M#!!"
        );
        // Column 300 does not fit the legacy single-byte encoding.
        assert_eq!(encode_mouse(ev(Down(Left), 300, 0), m, Enc::Default), None);
        // ...but does in UTF-8 encoding (301 + 32 = 333 -> 2 bytes).
        let v = encode_mouse(ev(Down(Left), 300, 0), m, Enc::Utf8).unwrap();
        assert_eq!(&v[..3], b"\x1b[M");
        assert_eq!(std::str::from_utf8(&v[4..6]).unwrap(), "\u{14d}");
    }
}
