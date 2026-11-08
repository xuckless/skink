//! A tiny, std-only HTTP/1.1 client: exactly what the EveryAI loopback host
//! needs and nothing more. One request per connection (`Connection: close`),
//! plain `http://`, loopback only. Handles `Content-Length`, `chunked` and
//! read-until-close bodies, and a Server-Sent-Events reader on top of them.
//!
//! Written from scratch against RFC 9112 / the WHATWG SSE spec: the usual
//! permissive crates (`httparse`, `ureq`, `minreq`) are general purpose and
//! pull in far more than the ~300 lines this needs (the chunked decoder and
//! head reader follow the same structure as `ureq`'s `chunked_transfer`, MIT OR
//! Apache-2.0, idea only; no code copied).

use crate::error::{Error, Result};
use std::io::{self, BufRead, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Longest header / chunk-size line we accept.
const MAX_LINE: usize = 16 * 1024;
const MAX_HEADERS: usize = 100;

// ---------------------------------------------------------------- URL

/// A validated loopback `http://` URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Url {
    /// Lowercase host: `127.0.0.1`, `localhost` or `::1`.
    pub host: String,
    pub port: u16,
    /// Path plus optional query, always starting with `/`.
    pub path: String,
}

impl Url {
    pub(crate) fn parse(s: &str) -> Result<Url> {
        let s = s.trim();
        let rest = match (s.get(..7), s.get(..8)) {
            (Some(p), _) if p.eq_ignore_ascii_case("http://") => &s[7..],
            (_, Some(p)) if p.eq_ignore_ascii_case("https://") => {
                return Err(Error::InvalidUrl(
                    "only plain http:// loopback urls are supported".into(),
                ));
            }
            _ => return Err(Error::InvalidUrl("expected an http:// url".into())),
        };
        let rest = rest.split('#').next().unwrap_or("");
        let (authority, tail) = match rest.find(['/', '?']) {
            Some(i) => rest.split_at(i),
            None => (rest, ""),
        };
        if tail.bytes().any(|b| b <= b' ' || b == 0x7f) {
            return Err(Error::InvalidUrl("illegal character in path".into()));
        }
        let path = if tail.is_empty() {
            "/".to_string()
        } else if tail.starts_with('?') {
            format!("/{tail}")
        } else {
            tail.to_string()
        };
        if authority.is_empty() || authority.contains('@') {
            return Err(Error::InvalidUrl(
                "bad authority (userinfo not allowed)".into(),
            ));
        }
        let (host, port_str) = if let Some(r) = authority.strip_prefix('[') {
            let end = r
                .find(']')
                .ok_or_else(|| Error::InvalidUrl("unterminated [ipv6]".into()))?;
            let after = &r[end + 1..];
            let port = match after {
                "" => None,
                a => Some(
                    a.strip_prefix(':')
                        .ok_or_else(|| Error::InvalidUrl("junk after [ipv6]".into()))?,
                ),
            };
            (r[..end].to_ascii_lowercase(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) if !h.contains(':') => (h.to_ascii_lowercase(), Some(p)),
                Some(_) => return Err(Error::InvalidUrl("unbracketed ipv6 host".into())),
                None => (authority.to_ascii_lowercase(), None),
            }
        };
        let port = match port_str {
            None | Some("") => 80,
            Some(p) => p
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| Error::InvalidUrl("bad port".into()))?,
        };
        if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1") {
            return Err(Error::NotLoopback(format!("{host}:{port}")));
        }
        Ok(Url { host, port, path })
    }

    /// The exact `Host` header value. EveryAI's DNS-rebinding guard accepts
    /// only `127.0.0.1:port`, `localhost:port` or `[::1]:port`.
    pub(crate) fn host_header(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Connect with a timeout, only ever to loopback addresses.
    pub(crate) fn connect(&self, timeout: Duration) -> Result<TcpStream> {
        let addrs: Vec<SocketAddr> = match self.host.as_str() {
            "127.0.0.1" => vec![(Ipv4Addr::LOCALHOST, self.port).into()],
            "::1" => vec![(Ipv6Addr::LOCALHOST, self.port).into()],
            h => (h, self.port)
                .to_socket_addrs()?
                .filter(|a| a.ip().is_loopback())
                .collect(),
        };
        if addrs.is_empty() {
            return Err(Error::NotLoopback(self.host_header()));
        }
        let timeout = timeout.max(Duration::from_millis(1));
        let mut last = None;
        for a in addrs {
            match TcpStream::connect_timeout(&a, timeout) {
                Ok(s) => {
                    let _ = s.set_nodelay(true);
                    return Ok(s);
                }
                Err(e) => last = Some(e),
            }
        }
        // Any failure to connect (refused, or "timed out" as Windows reports a
        // refused loopback port) means no host is listening: an I/O error, not
        // a slow reply. Timeouts on an established connection stay `Timeout`.
        Err(last
            .map(Error::Io)
            .unwrap_or_else(|| Error::Protocol("no address".into())))
    }
}

/// Header values must be visible ASCII (plus space/tab): no CR/LF injection.
pub(crate) fn valid_header_value(v: &str) -> bool {
    v.bytes().all(|b| b == b'\t' || (b' '..0x7f).contains(&b))
}

// ------------------------------------------------------------- streams

/// A `TcpStream` whose every read is bounded by an idle timeout and an
/// optional absolute deadline.
pub(crate) struct TimedStream {
    stream: TcpStream,
    idle: Duration,
    deadline: Option<Instant>,
}

impl TimedStream {
    pub(crate) fn new(stream: TcpStream, idle: Duration, total: Option<Duration>) -> Self {
        let _ = stream.set_write_timeout(Some(idle.max(Duration::from_millis(1))));
        TimedStream {
            stream,
            idle: idle.max(Duration::from_millis(1)),
            deadline: total.map(|t| Instant::now() + t),
        }
    }
}

impl Read for TimedStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let t = match self.deadline {
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                self.idle.min(d - now)
            }
            None => self.idle,
        };
        self.stream.set_read_timeout(Some(t))?;
        self.stream.read(buf)
    }
}

impl Write for TimedStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

// ------------------------------------------------------------- request

/// Serialise and send one `POST`.
pub(crate) fn write_post(
    w: &mut impl Write,
    url: &Url,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<()> {
    let mut out = Vec::with_capacity(256 + body.len());
    out.extend_from_slice(
        format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
            url.path,
            url.host_header(),
            body.len()
        )
        .as_bytes(),
    );
    for (k, v) in headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    w.write_all(&out)?;
    w.flush()
}

// ------------------------------------------------------------ response

/// Status line and headers (names lowercased).
#[derive(Debug)]
pub(crate) struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

impl Head {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Read one line (terminator included) of at most `max` bytes into `buf`.
/// Returns the number of bytes read; 0 means EOF.
pub(crate) fn read_line_limited<R: BufRead + ?Sized>(
    r: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> io::Result<usize> {
    let start = buf.len();
    loop {
        let (done, used) = {
            let avail = match r.fill_buf() {
                Ok(a) => a,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if avail.is_empty() {
                return Ok(buf.len() - start);
            }
            match avail.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    buf.extend_from_slice(&avail[..=i]);
                    (true, i + 1)
                }
                None => {
                    buf.extend_from_slice(avail);
                    (false, avail.len())
                }
            }
        };
        r.consume(used);
        if buf.len() - start > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        if done {
            return Ok(buf.len() - start);
        }
    }
}

fn trim_eol(line: &[u8]) -> &[u8] {
    let l = line.strip_suffix(b"\n").unwrap_or(line);
    l.strip_suffix(b"\r").unwrap_or(l)
}

/// Read a response head, skipping any `1xx` interim responses.
pub(crate) fn read_head<R: BufRead>(r: &mut R) -> io::Result<Head> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    loop {
        let mut line = Vec::new();
        if read_line_limited(r, &mut line, MAX_LINE)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let line = String::from_utf8_lossy(trim_eol(&line)).into_owned();
        let mut parts = line.splitn(3, ' ');
        let version = parts.next().unwrap_or("");
        if !version.starts_with("HTTP/1.") {
            return Err(bad("not an HTTP/1.x response"));
        }
        let status: u16 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .filter(|s| (100..=599).contains(s))
            .ok_or_else(|| bad("bad status line"))?;
        let mut headers = Vec::new();
        loop {
            let mut h = Vec::new();
            if read_line_limited(r, &mut h, MAX_LINE)? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let h = trim_eol(&h);
            if h.is_empty() {
                break;
            }
            if headers.len() >= MAX_HEADERS {
                return Err(bad("too many headers"));
            }
            let h = String::from_utf8_lossy(h);
            let (k, v) = h.split_once(':').ok_or_else(|| bad("bad header line"))?;
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
        if (100..200).contains(&status) && status != 101 {
            continue;
        }
        return Ok(Head { status, headers });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Fixed number of bytes left.
    Length(u64),
    /// Inside a chunked body: bytes left in the current chunk (0 = need a size line).
    Chunk(u64),
    /// Read until the peer closes.
    Close,
    Done,
}

/// Decodes a response body according to its framing.
pub(crate) struct BodyReader<R> {
    r: R,
    st: State,
}

impl<R: BufRead> BodyReader<R> {
    pub(crate) fn new(r: R, head: &Head) -> Result<Self> {
        let no_body = head.status == 204 || head.status == 304 || (100..200).contains(&head.status);
        let st = if no_body {
            State::Done
        } else if head
            .header("transfer-encoding")
            .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
        {
            State::Chunk(0)
        } else if let Some(cl) = head.header("content-length") {
            let n = cl
                .trim()
                .parse::<u64>()
                .map_err(|_| Error::Protocol("bad content-length".into()))?;
            State::Length(n)
        } else {
            State::Close
        };
        Ok(BodyReader { r, st })
    }

    fn line(&mut self) -> io::Result<Vec<u8>> {
        let mut l = Vec::new();
        if read_line_limited(&mut self.r, &mut l, MAX_LINE)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(l)
    }

    fn chunk_size(&mut self) -> io::Result<u64> {
        let l = self.line()?;
        let l = String::from_utf8_lossy(trim_eol(&l)).into_owned();
        let hex = l.split(';').next().unwrap_or("").trim();
        u64::from_str_radix(hex, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))
    }
}

impl<R: BufRead> Read for BodyReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let truncated = || io::Error::new(io::ErrorKind::UnexpectedEof, "body truncated");
        match self.st {
            State::Done => Ok(0),
            State::Close => self.r.read(out),
            State::Length(0) => {
                self.st = State::Done;
                Ok(0)
            }
            State::Length(n) => {
                let want = out.len().min(usize::try_from(n).unwrap_or(usize::MAX));
                let got = self.r.read(&mut out[..want])?;
                if got == 0 {
                    return Err(truncated());
                }
                self.st = State::Length(n - got as u64);
                Ok(got)
            }
            State::Chunk(mut left) => {
                if left == 0 {
                    left = self.chunk_size()?;
                    if left == 0 {
                        // Trailers, up to the blank line.
                        while !trim_eol(&self.line()?).is_empty() {}
                        self.st = State::Done;
                        return Ok(0);
                    }
                }
                let want = out.len().min(usize::try_from(left).unwrap_or(usize::MAX));
                let got = self.r.read(&mut out[..want])?;
                if got == 0 {
                    return Err(truncated());
                }
                left -= got as u64;
                if left == 0 && !trim_eol(&self.line()?).is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "missing CRLF after chunk",
                    ));
                }
                self.st = State::Chunk(left);
                Ok(got)
            }
        }
    }
}

/// Read the whole body, failing if it exceeds `max` bytes.
pub(crate) fn read_all_limited(r: &mut impl Read, max: usize) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    r.take(max as u64 + 1).read_to_end(&mut v)?;
    if v.len() > max {
        return Err(Error::Protocol("response body too large".into()));
    }
    Ok(v)
}

// ----------------------------------------------------------------- SSE

/// One Server-Sent Event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental SSE parser. Comment lines (`:` keep-alives) are skipped.
pub(crate) struct SseReader<R> {
    r: R,
    max_line: usize,
}

impl<R: BufRead> SseReader<R> {
    pub(crate) fn new(r: R, max_line: usize) -> Self {
        SseReader { r, max_line }
    }

    /// The next event with data, or `None` at end of stream.
    pub(crate) fn next_event(&mut self) -> io::Result<Option<SseEvent>> {
        let mut event: Option<String> = None;
        let mut data: Option<String> = None;
        loop {
            let mut raw = Vec::new();
            let n = read_line_limited(&mut self.r, &mut raw, self.max_line)?;
            if n == 0 {
                // Stream ended: flush a pending event leniently.
                return Ok(data.map(|data| SseEvent { event, data }));
            }
            let line = String::from_utf8_lossy(trim_eol(&raw)).into_owned();
            if line.is_empty() {
                if let Some(data) = data.take() {
                    return Ok(Some(SseEvent { event, data }));
                }
                event = None;
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line.as_str(), ""),
            };
            match field {
                "event" => event = Some(value.to_string()),
                "data" => match data.as_mut() {
                    Some(d) => {
                        d.push('\n');
                        d.push_str(value);
                    }
                    None => data = Some(value.to_string()),
                },
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn head_of(raw: &str) -> (Head, Cursor<Vec<u8>>) {
        let mut c = Cursor::new(raw.as_bytes().to_vec());
        let h = read_head(&mut c).unwrap();
        (h, c)
    }

    fn body(raw: &str) -> io::Result<String> {
        let (h, c) = head_of(raw);
        let mut b = BodyReader::new(c, &h).unwrap();
        let mut s = String::new();
        b.read_to_string(&mut s)?;
        Ok(s)
    }

    #[test]
    fn url_ok() {
        let u = Url::parse("http://127.0.0.1:41234/mcp").unwrap();
        assert_eq!(u.host_header(), "127.0.0.1:41234");
        assert_eq!(u.path, "/mcp");
        let u = Url::parse("HTTP://LocalHost:9").unwrap();
        assert_eq!(u.host_header(), "localhost:9");
        assert_eq!(u.path, "/");
        let u = Url::parse("http://[::1]:7/x?y=1#frag").unwrap();
        assert_eq!(u.host_header(), "[::1]:7");
        assert_eq!(u.path, "/x?y=1");
        assert_eq!(Url::parse("http://127.0.0.1").unwrap().port, 80);
    }

    #[test]
    fn url_rejects() {
        for bad in [
            "http://example.com/mcp",
            "http://10.0.0.1:80/mcp",
            "http://0.0.0.0:80/",
            "http://127.0.0.1.evil.com/mcp",
            "http://127.0.0.1@evil.com:1/mcp",
            "http://evil.com@127.0.0.1:1/mcp",
            "http://[fe80::1]:1/",
            "https://127.0.0.1:1/mcp",
            "ftp://127.0.0.1/",
            "127.0.0.1:1/mcp",
            "http://",
            "http://127.0.0.1:0/",
            "http://127.0.0.1:99999/",
            "http://127.0.0.1:1/a b",
            "http://127.0.0.1:1/a\r\nX: y",
            "http://::1/",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad} must be rejected");
        }
        assert!(matches!(
            Url::parse("http://example.com/").unwrap_err(),
            Error::NotLoopback(_)
        ));
    }

    #[test]
    fn content_length_body() {
        let s = body("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhelloEXTRA").unwrap();
        assert_eq!(s, "hello");
        assert!(body("HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nhi").is_err());
    }

    #[test]
    fn chunked_body() {
        let raw = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                   5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nX-Trailer: y\r\n\r\n";
        assert_eq!(body(raw).unwrap(), "hello world");
        let trunc = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel";
        assert!(body(trunc).is_err());
        let bad = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n";
        assert!(body(bad).is_err());
    }

    #[test]
    fn until_close_and_no_body() {
        assert_eq!(body("HTTP/1.1 200 OK\r\n\r\nabc").unwrap(), "abc");
        assert_eq!(
            body("HTTP/1.1 204 No Content\r\nContent-Length: 4\r\n\r\nabcd").unwrap(),
            ""
        );
    }

    #[test]
    fn head_skips_interim_and_lowercases() {
        let (h, _) =
            head_of("HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 202 Accepted\r\nX-Foo: Bar\r\n\r\n");
        assert_eq!(h.status, 202);
        assert_eq!(h.header("x-foo"), Some("Bar"));
        let mut c = Cursor::new(b"garbage\r\n\r\n".to_vec());
        assert!(read_head(&mut c).is_err());
        let mut c = Cursor::new(b"HTTP/1.1 200 OK\r\nbroken\r\n\r\n".to_vec());
        assert!(read_head(&mut c).is_err());
    }

    #[test]
    fn long_line_rejected() {
        let mut raw = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
        raw.extend(std::iter::repeat_n(b'a', MAX_LINE + 10));
        raw.extend_from_slice(b"\r\n\r\n");
        assert!(read_head(&mut Cursor::new(raw)).is_err());
    }

    #[test]
    fn sse_parses_comments_multiline_and_crlf() {
        let raw = ":\n\nevent: message\ndata: {\"a\":1}\n\n: keep\r\ndata: x\r\ndata: y\r\n\r\nevent: tail\ndata: z";
        let mut r = SseReader::new(Cursor::new(raw.as_bytes()), 1 << 20);
        let e = r.next_event().unwrap().unwrap();
        assert_eq!(e.event.as_deref(), Some("message"));
        assert_eq!(e.data, "{\"a\":1}");
        let e = r.next_event().unwrap().unwrap();
        assert_eq!(e.event, None);
        assert_eq!(e.data, "x\ny");
        let e = r.next_event().unwrap().unwrap();
        assert_eq!(e.event.as_deref(), Some("tail"));
        assert_eq!(e.data, "z");
        assert_eq!(r.next_event().unwrap(), None);
    }

    #[test]
    fn sse_over_chunked() {
        let raw = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                   8\r\nevent: m\r\n6\r\nessage\r\n10\r\n\ndata: ok\n\n\r\n0\r\n\r\n";
        let (h, c) = head_of(raw);
        let b = BodyReader::new(c, &h).unwrap();
        let mut r = SseReader::new(io::BufReader::new(b), 1 << 20);
        let e = r.next_event().unwrap().unwrap();
        assert_eq!(e.data, "ok");
    }
}
