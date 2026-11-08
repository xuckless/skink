//! A tiny side-channel scanner over the raw pty output stream.
//!
//! `vt100` is a pure screen model: it never answers terminal queries and
//! ignores a few modes hosts need. Shells (fish), vim and many TUIs send
//! `CSI 6 n` / `CSI c` and wait for a reply, so this scanner spots those, plus
//! cursor-style (DECSCUSR) and focus-reporting (DECSET 1004), without
//! disturbing the parser (the same bytes are still fed to `vt100`).
//! Idea from alacritty_terminal's event-listener design (`PtyWrite`), logic
//! written from the xterm `ctlseqs` spec.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    /// `CSI 5 n` -> `CSI 0 n`.
    DeviceStatus,
    /// `CSI 6 n` -> cursor position report.
    CursorPosition,
    /// `CSI c` / `CSI 0 c`.
    PrimaryDa,
    /// `CSI > c` / `CSI > 0 c`.
    SecondaryDa,
    /// `CSI 18 t` -> text area size in characters.
    WindowChars,
    /// DECSCUSR, `CSI Ps SP q`.
    CursorStyle(u8),
    /// DECSET/DECRST 1004.
    FocusReporting(bool),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    Ground,
    Esc,
    Csi,
    /// Inside OSC/DCS/APC/PM/SOS payload.
    Str,
    StrEsc,
}

const MAX: usize = 24;

pub(crate) struct Sniffer {
    st: St,
    buf: [u8; MAX],
    len: usize,
    overflow: bool,
}

impl Sniffer {
    pub fn new() -> Self {
        Sniffer {
            st: St::Ground,
            buf: [0; MAX],
            len: 0,
            overflow: false,
        }
    }

    /// Scan `bytes`; push `(end, event)` where `end` is the index just past
    /// the sequence's final byte. State carries across calls.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<(usize, Event)>) {
        for (i, &b) in bytes.iter().enumerate() {
            self.step(b, i + 1, out);
        }
    }

    fn step(&mut self, b: u8, end: usize, out: &mut Vec<(usize, Event)>) {
        match self.st {
            St::Ground => {
                if b == 0x1b {
                    self.st = St::Esc;
                }
            }
            St::Esc => match b {
                b'[' => {
                    self.st = St::Csi;
                    self.len = 0;
                    self.overflow = false;
                }
                b']' | b'P' | b'_' | b'^' | b'X' => self.st = St::Str,
                0x1b => {}
                _ => self.st = St::Ground,
            },
            St::Csi => match b {
                0x20..=0x3f => {
                    if self.len < MAX {
                        self.buf[self.len] = b;
                        self.len += 1;
                    } else {
                        self.overflow = true;
                    }
                }
                0x40..=0x7e => {
                    if !self.overflow
                        && let Some(ev) = dispatch(&self.buf[..self.len], b)
                    {
                        out.push((end, ev));
                    }
                    self.st = St::Ground;
                }
                0x1b => self.st = St::Esc,
                0x18 | 0x1a => self.st = St::Ground,
                _ => {} // other C0 controls execute in place
            },
            St::Str => match b {
                0x07 | 0x18 | 0x1a => self.st = St::Ground,
                0x1b => self.st = St::StrEsc,
                _ => {}
            },
            St::StrEsc => match b {
                b'\\' => self.st = St::Ground,
                // ESC inside a string aborts it and starts a new sequence.
                _ => {
                    self.st = St::Esc;
                    self.step(b, end, out);
                }
            },
        }
    }
}

fn dispatch(params: &[u8], fin: u8) -> Option<Event> {
    match (params, fin) {
        (b"5", b'n') => Some(Event::DeviceStatus),
        (b"6", b'n') => Some(Event::CursorPosition),
        (b"" | b"0", b'c') => Some(Event::PrimaryDa),
        (b">" | b">0", b'c') => Some(Event::SecondaryDa),
        (b"18", b't') => Some(Event::WindowChars),
        (p, b'q') if p.last() == Some(&b' ') => {
            let n = &p[..p.len() - 1];
            if n.iter().all(u8::is_ascii_digit) {
                let v = if n.is_empty() {
                    0
                } else {
                    std::str::from_utf8(n).ok()?.parse().ok()?
                };
                Some(Event::CursorStyle(v))
            } else {
                None
            }
        }
        (p, b'h' | b'l') if p.first() == Some(&b'?') => {
            let has = p[1..].split(|&c| c == b';').any(|m| m == b"1004");
            has.then_some(Event::FocusReporting(fin == b'h'))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<Event> {
        let mut s = Sniffer::new();
        let mut all = Vec::new();
        for c in chunks {
            let mut out = Vec::new();
            s.feed(c, &mut out);
            all.extend(out.into_iter().map(|(_, e)| e));
        }
        all
    }

    #[test]
    fn queries() {
        assert_eq!(scan(&[b"\x1b[5n"]), [Event::DeviceStatus]);
        assert_eq!(scan(&[b"hi\x1b[6n"]), [Event::CursorPosition]);
        assert_eq!(scan(&[b"\x1b[c"]), [Event::PrimaryDa]);
        assert_eq!(scan(&[b"\x1b[0c"]), [Event::PrimaryDa]);
        assert_eq!(scan(&[b"\x1b[>c"]), [Event::SecondaryDa]);
        assert_eq!(scan(&[b"\x1b[18t"]), [Event::WindowChars]);
    }

    #[test]
    fn split_across_chunks_and_offsets() {
        assert_eq!(scan(&[b"\x1b", b"[", b"6", b"n"]), [Event::CursorPosition]);
        let mut s = Sniffer::new();
        let mut out = Vec::new();
        s.feed(b"ab\x1b[6ncd", &mut out);
        assert_eq!(out, [(6, Event::CursorPosition)]);
    }

    #[test]
    fn modes() {
        assert_eq!(scan(&[b"\x1b[?1004h"]), [Event::FocusReporting(true)]);
        assert_eq!(scan(&[b"\x1b[?25;1004l"]), [Event::FocusReporting(false)]);
        assert_eq!(scan(&[b"\x1b[?25h"]), []);
        assert_eq!(scan(&[b"\x1b[5 q"]), [Event::CursorStyle(5)]);
        assert_eq!(scan(&[b"\x1b[ q"]), [Event::CursorStyle(0)]);
    }

    #[test]
    fn ignores_strings_and_overlong() {
        // Query-looking bytes inside an OSC payload are not queries.
        assert_eq!(scan(&[b"\x1b]0;a[6n\x07"]), []);
        assert_eq!(scan(&[b"\x1bP+q6e\x1b\\"]), []);
        assert_eq!(
            scan(&[b"\x1b]0;title\x1b\\\x1b[6n"]),
            [Event::CursorPosition]
        );
        let long = format!("\x1b[{}n", "1;".repeat(40));
        assert_eq!(scan(&[long.as_bytes()]), []);
    }
}
