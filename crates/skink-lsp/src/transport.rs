//! LSP base-protocol framing (`Content-Length: N\r\n\r\n<json>`).

#[cfg(test)]
use std::io::Write;
use std::io::{self, BufRead, Read};

/// Refuse absurd frames instead of allocating them.
const MAX_MESSAGE: usize = 256 * 1024 * 1024;
/// Longest header line accepted (a server printing junk without newlines to
/// stdout must not grow a buffer forever).
const MAX_HEADER_LINE: u64 = 8 * 1024;

// Adapted from lapce/lapce-proxy/src/plugin/lsp.rs (parse_header, read_message)
// @ f66ffaa95387794205c35864517de70958b9ea8d, Apache-2.0, Copyright (c) Lapce Authors.
// Changes: byte based headers and body (no UTF-8 requirement at the framing layer),
// explicit EOF error (upstream spins on EOF), tolerates unknown headers and bare `\n`,
// header line and frame size caps, no anyhow / tracing, returns io::Error.
/// Read one framed message body. `UnexpectedEof` means the peer closed the pipe.
pub fn read_message<R: BufRead + ?Sized>(reader: &mut R) -> io::Result<Vec<u8>> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    let mut line = Vec::new();
    let mut content_length: Option<usize> = None;
    loop {
        line.clear();
        if (&mut *reader)
            .take(MAX_HEADER_LINE)
            .read_until(b'\n', &mut line)?
            == 0
        {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if line.last() != Some(&b'\n') && line.len() as u64 >= MAX_HEADER_LINE {
            return Err(bad("header line too long"));
        }
        let l = String::from_utf8_lossy(&line);
        let l = l.trim_end_matches(['\r', '\n']);
        if l.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue; // stray blank line between messages
        }
        if let Some((name, value)) = l.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = Some(
                value
                    .trim()
                    .parse()
                    .map_err(|_| bad("bad Content-Length"))?,
            );
        }
        // Content-Type and unknown headers are ignored.
    }
    let len = content_length.unwrap_or(0);
    if len > MAX_MESSAGE {
        return Err(bad("message too large"));
    }
    // Grow with the bytes that actually arrive: a lying header must not make
    // us allocate the whole claimed size up front.
    let mut body = Vec::with_capacity(len.min(64 * 1024));
    (&mut *reader).take(len as u64).read_to_end(&mut body)?;
    if body.len() < len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(body)
}

// Adapted from lapce/lapce-proxy/src/plugin/lsp.rs (writer thread, `format!("Content-Length...")`)
// @ f66ffaa95387794205c35864517de70958b9ea8d, Apache-2.0, Copyright (c) Lapce Authors.
// Changes: single write of header+body into a byte buffer; flushes; io::Result.
/// Frame a serialized JSON body.
pub fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// Write one framed message and flush.
#[cfg(test)]
pub fn write_message<W: Write + ?Sized>(w: &mut W, body: &[u8]) -> io::Result<()> {
    w.write_all(&frame(body))?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        write_message(&mut buf, b"{\"a\":1}").unwrap();
        write_message(&mut buf, b"{\"b\":2}").unwrap();
        let mut r = Cursor::new(buf);
        assert_eq!(read_message(&mut r).unwrap(), b"{\"a\":1}");
        assert_eq!(read_message(&mut r).unwrap(), b"{\"b\":2}");
        assert_eq!(
            read_message(&mut r).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn headers_case_insensitive_with_content_type() {
        let raw = b"content-type: application/vscode-jsonrpc; charset=utf-8\r\nCONTENT-LENGTH:  2 \r\n\r\n{}";
        assert_eq!(read_message(&mut Cursor::new(&raw[..])).unwrap(), b"{}");
    }

    #[test]
    fn multibyte_body_counts_bytes() {
        let body = "{\"s\":\"é你\"}".as_bytes();
        let mut buf = Vec::new();
        write_message(&mut buf, body).unwrap();
        assert!(
            String::from_utf8_lossy(&buf).starts_with(&format!("Content-Length: {}", body.len()))
        );
        assert_eq!(read_message(&mut Cursor::new(buf)).unwrap(), body);
    }

    #[test]
    fn truncated_body_is_eof() {
        let e = read_message(&mut Cursor::new(&b"Content-Length: 10\r\n\r\n{}"[..])).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn bad_length_is_invalid_data() {
        let e = read_message(&mut Cursor::new(&b"Content-Length: x\r\n\r\n"[..])).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn oversized_rejected() {
        let raw = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE + 1);
        let e = read_message(&mut Cursor::new(raw.into_bytes())).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn endless_header_line_is_rejected_not_buffered() {
        // A server spewing bytes without a newline on stdout.
        let mut junk = std::io::repeat(b'x').take(10 * 1024 * 1024);
        let mut r = io::BufReader::new(&mut junk);
        let e = read_message(&mut r).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn non_utf8_junk_header_is_ignored() {
        let raw = b"X-Junk: \xff\xfe\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(read_message(&mut Cursor::new(&raw[..])).unwrap(), b"{}");
    }

    #[test]
    fn lying_length_is_eof_without_huge_allocation() {
        let raw = format!("Content-Length: {}\r\n\r\n{{}}", MAX_MESSAGE);
        let e = read_message(&mut Cursor::new(raw.into_bytes())).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn lone_lf_line_endings() {
        assert_eq!(
            read_message(&mut Cursor::new(&b"Content-Length: 2\n\n{}"[..])).unwrap(),
            b"{}"
        );
    }
}
