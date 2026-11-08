//! System clipboard: arboard when it works, OSC 52 so it also works over SSH
//! and in terminals without a clipboard daemon.

use std::io::Write;

pub struct Clipboard {
    sys: Option<arboard::Clipboard>,
    tried: bool,
}

impl Clipboard {
    pub fn new() -> Self {
        // arboard is created lazily: connecting to X11 costs a few ms.
        Self {
            sys: None,
            tried: false,
        }
    }

    fn sys(&mut self) -> Option<&mut arboard::Clipboard> {
        if !self.tried {
            self.tried = true;
            self.sys = arboard::Clipboard::new().ok();
        }
        self.sys.as_mut()
    }

    /// Put text on the system clipboard (best effort) and emit OSC 52.
    pub fn set(&mut self, text: &str, osc52: bool) {
        if let Some(c) = self.sys() {
            let _ = c.set_text(text.to_owned());
        }
        if osc52 {
            write_osc52(&base64(text.as_bytes()));
        }
    }

    pub fn get(&mut self) -> Option<String> {
        self.sys()?.get_text().ok().filter(|t| !t.is_empty())
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        Self::new()
    }
}

/// Emit an OSC 52 "set clipboard" with already base64-encoded data.
pub fn write_osc52(b64: &str) {
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\x1b]52;c;{b64}\x07");
    let _ = out.flush();
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
