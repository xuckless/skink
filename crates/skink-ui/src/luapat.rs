//! Lua patterns for scripts, with a time limit.
//!
//! Ported from Lua 5.4's `lstrlib.c` (MIT, Copyright (C) 1994-2023
//! Lua.org, PUC-Rio; see NOTICES.md). Lua's own matcher runs in C, where the
//! instruction hook that stops endless scripts never fires, so a pattern
//! like `('a*'):rep(60) .. 'b'` against 3000 `a`s froze the editor. This one
//! checks the script's deadline as it backtracks. `string.find`, `match`,
//! `gmatch` and `gsub` are replaced with it; behaviour is Lua's.

use std::time::Instant;

use mlua::{Function, Lua, MultiValue, Table, Value};

const ESC: u8 = b'%';
const SPECIALS: &[u8] = b"^$*+?.([%-";
const MAX_CALLS: usize = 200;
const MAX_CAPTURES: usize = 32;

#[derive(Clone, Copy)]
enum Cap {
    Len(usize),
    Unfinished,
    Position,
}

type R<T> = Result<T, String>;

struct Ms<'a> {
    src: &'a [u8],
    pat: &'a [u8],
    level: usize,
    cap: [(usize, Cap); MAX_CAPTURES],
    depth: usize,
    steps: u32,
    deadline: Instant,
}

fn class(c: u8, cl: u8) -> bool {
    let res = match cl.to_ascii_lowercase() {
        b'a' => c.is_ascii_alphabetic(),
        b'c' => c.is_ascii_control(),
        b'd' => c.is_ascii_digit(),
        b'g' => c.is_ascii_graphic(),
        b'l' => c.is_ascii_lowercase(),
        b'p' => c.is_ascii_punctuation(),
        b's' => matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'),
        b'u' => c.is_ascii_uppercase(),
        b'w' => c.is_ascii_alphanumeric(),
        b'x' => c.is_ascii_hexdigit(),
        _ => return cl == c,
    };
    if cl.is_ascii_uppercase() { !res } else { res }
}

impl<'a> Ms<'a> {
    fn new(src: &'a [u8], pat: &'a [u8], deadline: Instant) -> Self {
        Ms {
            src,
            pat,
            level: 0,
            cap: [(0, Cap::Unfinished); MAX_CAPTURES],
            depth: 0,
            steps: 0,
            deadline,
        }
    }

    fn reset(&mut self) {
        self.level = 0;
        self.depth = 0;
    }

    fn tick(&mut self) -> R<()> {
        self.steps = self.steps.wrapping_add(1);
        if self.steps & 0x3ff == 0 && Instant::now() > self.deadline {
            return Err(format!(
                "stopped after {} ms (pattern too slow?)",
                crate::script::TIME_LIMIT.as_millis()
            ));
        }
        Ok(())
    }

    fn class_end(&self, mut p: usize) -> R<usize> {
        let pat = self.pat;
        let c = pat[p];
        p += 1;
        if c == ESC {
            if p >= pat.len() {
                return Err("malformed pattern (ends with '%')".into());
            }
            return Ok(p + 1);
        }
        if c == b'[' {
            if pat.get(p) == Some(&b'^') {
                p += 1;
            }
            loop {
                if p >= pat.len() {
                    return Err("malformed pattern (missing ']')".into());
                }
                let cc = pat[p];
                p += 1;
                if cc == ESC && p < pat.len() {
                    p += 1;
                }
                if pat.get(p) == Some(&b']') {
                    break;
                }
            }
            return Ok(p + 1);
        }
        Ok(p)
    }

    /// `p` is at the `[`, `ec` at the closing `]`.
    fn bracket(&self, c: u8, mut p: usize, ec: usize) -> bool {
        let pat = self.pat;
        let mut sig = true;
        if pat[p + 1] == b'^' {
            sig = false;
            p += 1;
        }
        loop {
            p += 1;
            if p >= ec {
                break;
            }
            if pat[p] == ESC {
                p += 1;
                if class(c, pat[p]) {
                    return sig;
                }
            } else if pat[p + 1] == b'-' && p + 2 < ec {
                p += 2;
                if pat[p - 2] <= c && c <= pat[p] {
                    return sig;
                }
            } else if pat[p] == c {
                return sig;
            }
        }
        !sig
    }

    fn single(&self, s: usize, p: usize, ep: usize) -> bool {
        let Some(&c) = self.src.get(s) else {
            return false;
        };
        match self.pat[p] {
            b'.' => true,
            ESC => class(c, self.pat[p + 1]),
            b'[' => self.bracket(c, p, ep - 1),
            pc => pc == c,
        }
    }

    fn balance(&self, s: usize, p: usize) -> R<Option<usize>> {
        if p + 1 >= self.pat.len() {
            return Err("malformed pattern (missing arguments to '%b')".into());
        }
        if self.src.get(s) != Some(&self.pat[p]) {
            return Ok(None);
        }
        let (b, e) = (self.pat[p], self.pat[p + 1]);
        let mut open = 1;
        for (i, &c) in self.src.iter().enumerate().skip(s + 1) {
            if c == e {
                open -= 1;
                if open == 0 {
                    return Ok(Some(i + 1));
                }
            } else if c == b {
                open += 1;
            }
        }
        Ok(None)
    }

    fn max_expand(&mut self, s: usize, p: usize, ep: usize) -> R<Option<usize>> {
        let mut i = 0;
        while self.single(s + i, p, ep) {
            i += 1;
        }
        loop {
            if let Some(r) = self.do_match(s + i, ep + 1)? {
                return Ok(Some(r));
            }
            if i == 0 {
                return Ok(None);
            }
            i -= 1;
        }
    }

    fn min_expand(&mut self, mut s: usize, p: usize, ep: usize) -> R<Option<usize>> {
        loop {
            if let Some(r) = self.do_match(s, ep + 1)? {
                return Ok(Some(r));
            }
            if self.single(s, p, ep) {
                s += 1;
            } else {
                return Ok(None);
            }
        }
    }

    fn start_capture(&mut self, s: usize, p: usize, what: Cap) -> R<Option<usize>> {
        if self.level >= MAX_CAPTURES {
            return Err("too many captures".into());
        }
        self.cap[self.level] = (s, what);
        self.level += 1;
        let r = self.do_match(s, p)?;
        if r.is_none() {
            self.level -= 1;
        }
        Ok(r)
    }

    fn end_capture(&mut self, s: usize, p: usize) -> R<Option<usize>> {
        let l = (0..self.level)
            .rev()
            .find(|&l| matches!(self.cap[l].1, Cap::Unfinished))
            .ok_or("invalid pattern capture")?;
        self.cap[l].1 = Cap::Len(s - self.cap[l].0);
        let r = self.do_match(s, p)?;
        if r.is_none() {
            self.cap[l].1 = Cap::Unfinished;
        }
        Ok(r)
    }

    fn back_ref(&self, s: usize, d: u8) -> R<Option<usize>> {
        let l = (d - b'1') as usize;
        let bad = || format!("invalid capture index %{}", l + 1);
        if l >= self.level {
            return Err(bad());
        }
        let (init, len) = match self.cap[l] {
            (_, Cap::Unfinished) => return Err(bad()),
            (_, Cap::Position) => return Ok(None),
            (i, Cap::Len(n)) => (i, n),
        };
        Ok(
            (self.src.len() - s >= len && self.src[init..init + len] == self.src[s..s + len])
                .then_some(s + len),
        )
    }

    fn do_match(&mut self, s: usize, p: usize) -> R<Option<usize>> {
        self.depth += 1;
        if self.depth > MAX_CALLS {
            return Err("pattern too complex".into());
        }
        let r = self.match_inner(s, p);
        self.depth -= 1;
        r
    }

    fn match_inner(&mut self, mut s: usize, mut p: usize) -> R<Option<usize>> {
        let (plen, slen) = (self.pat.len(), self.src.len());
        loop {
            self.tick()?;
            if p == plen {
                return Ok(Some(s));
            }
            let next = self.pat.get(p + 1).copied();
            match self.pat[p] {
                b'(' => {
                    return if next == Some(b')') {
                        self.start_capture(s, p + 2, Cap::Position)
                    } else {
                        self.start_capture(s, p + 1, Cap::Unfinished)
                    };
                }
                b')' => return self.end_capture(s, p + 1),
                b'$' if p + 1 == plen => return Ok((s == slen).then_some(s)),
                ESC if next == Some(b'b') => match self.balance(s, p + 2)? {
                    Some(n) => {
                        s = n;
                        p += 4;
                    }
                    None => return Ok(None),
                },
                ESC if next == Some(b'f') => {
                    p += 2;
                    if self.pat.get(p) != Some(&b'[') {
                        return Err("missing '[' after '%f' in pattern".into());
                    }
                    let ep = self.class_end(p)?;
                    let prev = if s == 0 { 0 } else { self.src[s - 1] };
                    let cur = self.src.get(s).copied().unwrap_or(0);
                    if self.bracket(prev, p, ep - 1) || !self.bracket(cur, p, ep - 1) {
                        return Ok(None);
                    }
                    p = ep;
                }
                ESC if next.is_some_and(|d| d.is_ascii_digit()) => {
                    match self.back_ref(s, next.unwrap_or(b'0'))? {
                        Some(n) => {
                            s = n;
                            p += 2;
                        }
                        None => return Ok(None),
                    }
                }
                _ => {
                    let ep = self.class_end(p)?;
                    let q = self.pat.get(ep).copied();
                    if !self.single(s, p, ep) {
                        if matches!(q, Some(b'*' | b'?' | b'-')) {
                            p = ep + 1;
                            continue;
                        }
                        return Ok(None);
                    }
                    match q {
                        Some(b'?') => {
                            if let Some(r) = self.do_match(s + 1, ep + 1)? {
                                return Ok(Some(r));
                            }
                            p = ep + 1;
                        }
                        Some(b'+') => return self.max_expand(s + 1, p, ep),
                        Some(b'*') => return self.max_expand(s, p, ep),
                        Some(b'-') => return self.min_expand(s, p, ep),
                        _ => {
                            s += 1;
                            p = ep;
                        }
                    }
                }
            }
        }
    }

    /// Capture `i` of the match `s..e` (the whole match when there are none).
    fn capture(&self, lua: &Lua, i: usize, s: usize, e: usize) -> mlua::Result<Value> {
        if i >= self.level {
            if i == 0 {
                return Ok(Value::String(lua.create_string(&self.src[s..e])?));
            }
            return Err(err(format!("invalid capture index %{}", i + 1)));
        }
        match self.cap[i] {
            (_, Cap::Unfinished) => Err(err("unfinished capture")),
            (init, Cap::Position) => Ok(Value::Integer(init as i64 + 1)),
            (init, Cap::Len(n)) => Ok(Value::String(lua.create_string(&self.src[init..init + n])?)),
        }
    }

    fn captures(&self, lua: &Lua, s: usize, e: usize, whole: bool) -> mlua::Result<Vec<Value>> {
        let n = if self.level == 0 && whole {
            1
        } else {
            self.level
        };
        (0..n).map(|i| self.capture(lua, i, s, e)).collect()
    }
}

fn err(msg: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(msg)
}

fn deadline(lua: &Lua) -> Instant {
    lua.app_data_ref::<crate::script::Deadline>()
        .map_or_else(|| Instant::now() + crate::script::TIME_LIMIT, |d| d.0)
}

/// A string argument (numbers are turned into strings, as in Lua).
fn bytes(lua: &Lua, v: &Value, n: usize, f: &str) -> mlua::Result<Vec<u8>> {
    match v {
        Value::String(s) => Ok(s.as_bytes().to_vec()),
        Value::Integer(_) | Value::Number(_) => Ok(lua
            .coerce_string(v.clone())?
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default()),
        other => Err(err(format!(
            "bad argument #{n} to '{f}' (string expected, got {})",
            other.type_name()
        ))),
    }
}

fn opt_int(v: Option<&Value>, n: usize, f: &str, default: i64) -> mlua::Result<i64> {
    match v {
        None | Some(Value::Nil) => Ok(default),
        Some(Value::Integer(i)) => Ok(*i),
        Some(Value::Number(x)) if x.fract() == 0.0 => Ok(*x as i64),
        Some(other) => Err(err(format!(
            "bad argument #{n} to '{f}' (number expected, got {})",
            other.type_name()
        ))),
    }
}

/// Lua's `posrelatI`: a 1-based start position from a possibly negative one.
fn start_pos(pos: i64, len: usize) -> usize {
    let len = len as i64;
    (if pos > 0 {
        pos
    } else if pos == 0 || pos < -len {
        1
    } else {
        len + pos + 1
    }) as usize
}

fn find_aux(lua: &Lua, args: MultiValue, find: bool) -> mlua::Result<MultiValue> {
    let f = if find { "find" } else { "match" };
    let a: Vec<Value> = args.into_iter().collect();
    let nil = Value::Nil;
    let src = bytes(lua, a.first().unwrap_or(&nil), 1, f)?;
    let pat = bytes(lua, a.get(1).unwrap_or(&nil), 2, f)?;
    let init = start_pos(opt_int(a.get(2), 3, f, 1)?, src.len()) - 1;
    if init > src.len() {
        return Ok(MultiValue::from_iter([Value::Nil]));
    }
    let plain = a
        .get(3)
        .is_some_and(|v| !matches!(v, Value::Nil | Value::Boolean(false)));
    if find && (plain || !pat.iter().any(|c| SPECIALS.contains(c))) {
        let hit = if pat.is_empty() {
            Some(init)
        } else {
            src[init..]
                .windows(pat.len())
                .position(|w| w == &pat[..])
                .map(|i| i + init)
        };
        return Ok(match hit {
            Some(i) => MultiValue::from_iter([
                Value::Integer(i as i64 + 1),
                Value::Integer((i + pat.len()) as i64),
            ]),
            None => MultiValue::from_iter([Value::Nil]),
        });
    }
    let anchor = pat.first() == Some(&b'^');
    let mut ms = Ms::new(&src, &pat, deadline(lua));
    let mut s1 = init;
    loop {
        ms.reset();
        if let Some(e) = ms.do_match(s1, anchor as usize).map_err(err)? {
            return Ok(if find {
                let mut out = vec![Value::Integer(s1 as i64 + 1), Value::Integer(e as i64)];
                out.extend(ms.captures(lua, s1, e, false)?);
                MultiValue::from_iter(out)
            } else {
                MultiValue::from_iter(ms.captures(lua, s1, e, true)?)
            });
        }
        if s1 >= src.len() || anchor {
            break;
        }
        s1 += 1;
    }
    Ok(MultiValue::from_iter([Value::Nil]))
}

fn gmatch(lua: &Lua, args: MultiValue) -> mlua::Result<Function> {
    let a: Vec<Value> = args.into_iter().collect();
    let nil = Value::Nil;
    let src = bytes(lua, a.first().unwrap_or(&nil), 1, "gmatch")?;
    let pat = bytes(lua, a.get(1).unwrap_or(&nil), 2, "gmatch")?;
    let init = start_pos(opt_int(a.get(2), 3, "gmatch", 1)?, src.len()) - 1;
    // Where the next search starts, and where the last match ended (an empty
    // match there is skipped, as in Lua).
    let state = std::cell::Cell::new((init, usize::MAX));
    lua.create_function(move |lua, ()| {
        let (mut pos, last) = state.get();
        let mut ms = Ms::new(&src, &pat, deadline(lua));
        while pos <= src.len() {
            ms.reset();
            if let Some(e) = ms.do_match(pos, 0).map_err(err)?
                && e != last
            {
                state.set((e, e));
                return Ok(MultiValue::from_iter(ms.captures(lua, pos, e, true)?));
            }
            pos += 1;
        }
        state.set((pos, last));
        Ok(MultiValue::from_iter([Value::Nil]))
    })
}

fn gsub(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let a: Vec<Value> = args.into_iter().collect();
    let nil = Value::Nil;
    let src = bytes(lua, a.first().unwrap_or(&nil), 1, "gsub")?;
    let pat = bytes(lua, a.get(1).unwrap_or(&nil), 2, "gsub")?;
    let repl = a.get(2).cloned().unwrap_or(Value::Nil);
    if !matches!(
        repl,
        Value::String(_)
            | Value::Integer(_)
            | Value::Number(_)
            | Value::Table(_)
            | Value::Function(_)
    ) {
        return Err(err(format!(
            "bad argument #3 to 'gsub' (string/function/table expected, got {})",
            repl.type_name()
        )));
    }
    let max = opt_int(a.get(3), 4, "gsub", src.len() as i64 + 1)?;
    let anchor = pat.first() == Some(&b'^');
    let mut ms = Ms::new(&src, &pat, deadline(lua));
    let (mut at, mut last, mut n) = (0, usize::MAX, 0i64);
    let mut out = Vec::new();
    while n < max {
        ms.reset();
        match ms.do_match(at, anchor as usize).map_err(err)? {
            Some(e) if e != last => {
                n += 1;
                add_value(lua, &ms, at, e, &repl, &mut out)?;
                at = e;
                last = e;
            }
            _ if at < src.len() => {
                out.push(src[at]);
                at += 1;
            }
            _ => break,
        }
        if anchor {
            break;
        }
    }
    out.extend_from_slice(&src[at.min(src.len())..]);
    Ok(MultiValue::from_iter([
        Value::String(lua.create_string(&out)?),
        Value::Integer(n),
    ]))
}

fn add_value(
    lua: &Lua,
    ms: &Ms,
    s: usize,
    e: usize,
    repl: &Value,
    out: &mut Vec<u8>,
) -> mlua::Result<()> {
    let value = match repl {
        Value::Table(t) => {
            let k = ms.capture(lua, 0, s, e)?;
            Table::get::<Value>(t, k)?
        }
        Value::Function(f) => {
            let caps = ms.captures(lua, s, e, true)?;
            f.call::<MultiValue>(MultiValue::from_iter(caps))?
                .into_iter()
                .next()
                .unwrap_or(Value::Nil)
        }
        _ => {
            let r = bytes(lua, repl, 3, "gsub")?;
            let mut i = 0;
            while i < r.len() {
                let c = r[i];
                i += 1;
                if c != ESC {
                    out.push(c);
                    continue;
                }
                let Some(&d) = r.get(i) else {
                    return Err(err("invalid use of '%' in replacement string"));
                };
                i += 1;
                if d == ESC {
                    out.push(ESC);
                } else if d.is_ascii_digit() {
                    let v = if d == b'0' {
                        Value::String(lua.create_string(&ms.src[s..e])?)
                    } else {
                        ms.capture(lua, (d - b'1') as usize, s, e)?
                    };
                    let v = lua.coerce_string(v)?.ok_or_else(|| err("bad capture"))?;
                    out.extend_from_slice(&v.as_bytes());
                } else {
                    return Err(err("invalid use of '%' in replacement string"));
                }
            }
            return Ok(());
        }
    };
    match value {
        Value::Nil | Value::Boolean(false) => out.extend_from_slice(&ms.src[s..e]),
        Value::String(_) | Value::Integer(_) | Value::Number(_) => {
            let v = lua.coerce_string(value)?.ok_or_else(|| err("bad value"))?;
            out.extend_from_slice(&v.as_bytes());
        }
        other => {
            return Err(err(format!(
                "invalid replacement value (a {})",
                other.type_name()
            )));
        }
    }
    Ok(())
}

/// Replace `string.find`, `match`, `gmatch` and `gsub` with the ones here.
pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let string: Table = lua.globals().get("string")?;
    string.set(
        "find",
        lua.create_function(|lua, a| find_aux(lua, a, true))?,
    )?;
    string.set(
        "match",
        lua.create_function(|lua, a| find_aux(lua, a, false))?,
    )?;
    string.set("gmatch", lua.create_function(gmatch)?)?;
    string.set("gsub", lua.create_function(gsub)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(code: &str) -> String {
        let lua = Lua::new();
        install(&lua).unwrap();
        lua.load(code)
            .eval::<MultiValue>()
            .map(|v| {
                v.into_iter()
                    .map(|v| {
                        lua.coerce_string(v.clone())
                            .ok()
                            .flatten()
                            .map_or_else(|| format!("{v:?}"), |s| s.to_string_lossy())
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    #[test]
    fn behaves_like_lua() {
        // Expected values are what stock Lua 5.4 returns.
        let cases = [
            ("return ('hello world'):find('o w')", "5,7"),
            ("return ('hello world'):find('l+')", "3,4"),
            ("return ('hello'):find('l', 1, true)", "3,3"),
            ("return ('a.b'):find('.', 1, true)", "2,2"),
            (
                "return ('key = value'):match('(%w+)%s*=%s*(%w+)')",
                "key,value",
            ),
            ("return ('  trim  '):match('^%s*(.-)%s*$')", "trim"),
            ("return ('x(a(b)c)y'):match('%b()')", "(a(b)c)"),
            ("return ('THE (quick) fox'):find('%f[%a]%a+', 5)", "6,10"),
            ("return ('abcabc'):match('(a)(b)c%1%2')", "a,b"),
            ("return ('hello'):match('()ll()')", "3,5"),
            ("return ('hello world'):gsub('o', '0')", "hell0 w0rld,2"),
            ("return ('abc'):gsub('', '-')", "-a-b-c-,4"),
            (
                "return ('hello world'):gsub('(%w+)', '<%1>')",
                "<hello> <world>,2",
            ),
            (
                "return ('$name is $age'):gsub('%$(%w+)', {name='Ann', age=3})",
                "Ann is 3,2",
            ),
            (
                "return ('abc'):gsub('%w', function(c) return c:upper() .. '.' end)",
                "A.B.C.,3",
            ),
            ("return ('abc'):gsub('b', '%%')", "a%c,1"),
            ("return ('aaa'):gsub('a', 'b', 2)", "bba,2"),
            ("return ('^x'):gsub('^%^', '')", "x,1"),
            (
                "local t = {} for w in ('one two  three'):gmatch('%a+') do t[#t+1] = w end return table.concat(t, '|')",
                "one|two|three",
            ),
            (
                "local t = {} for k, v in ('a=1, b=2'):gmatch('(%w+)=(%w+)') do t[#t+1] = k .. v end return table.concat(t, '|')",
                "a1|b2",
            ),
            ("return ('[x]'):match('[%[]')", "["),
            ("return ('a-b'):match('[a-c]+')", "a"),
            ("return ('x]'):match('[]]')", "]"),
            ("return ('abc'):find('b', -2)", "2,2"),
            ("return ('abc'):find('x')", "nil"),
            ("return string.find(123, '2')", "2,2"),
        ];
        for (code, want) in cases {
            let got = eval(code).replace("Nil", "nil");
            assert_eq!(got, want, "{code}");
        }
        assert!(eval("return ('a'):find('%')").contains("malformed pattern"));
        assert!(eval("return ('a'):find('[a')").contains("missing ']'"));
        assert!(eval("return ('a'):gsub('a', '%2')").contains("invalid capture index"));
    }

    #[test]
    fn a_pathological_pattern_stops_at_the_deadline() {
        let lua = Lua::new();
        install(&lua).unwrap();
        lua.set_app_data(crate::script::Deadline(
            Instant::now() + std::time::Duration::from_millis(100),
        ));
        let t = Instant::now();
        let r = lua
            .load("local s = ('a'):rep(3000); return s:find(('a*'):rep(60) .. 'b')")
            .exec();
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        assert!(r.unwrap_err().to_string().contains("stopped after"));
    }
}
