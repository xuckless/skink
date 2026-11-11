//! The part of TOML that `skink-package.toml` uses, parsed into JSON values
//! (written here: a TOML crate costs more binary than this file). Tables,
//! arrays of tables, dotted keys, basic and literal strings, integers,
//! booleans, arrays and inline tables. Multi-line strings, floats and dates
//! are refused with the line number.

use serde_json::{Map, Value};

pub fn parse(text: &str) -> Result<Map<String, Value>, String> {
    let mut p = P {
        c: text.chars().collect(),
        i: 0,
    };
    let mut root = Map::new();
    let mut path: Vec<String> = Vec::new();
    loop {
        p.blank();
        match p.peek() {
            None => return Ok(root),
            Some('[') if p.at(1) == Some('[') => {
                p.i += 2;
                let keys = p.key()?;
                p.expect("]]")?;
                let (last, parent) = keys.split_last().ok_or_else(|| p.msg("empty table name"))?;
                let t = table_at(&mut root, parent).map_err(|e| p.msg(&e))?;
                match t
                    .entry(last.clone())
                    .or_insert_with(|| Value::Array(Vec::new()))
                {
                    Value::Array(a) => a.push(Value::Object(Map::new())),
                    _ => return p.err(&format!("{last} is not an array of tables")),
                }
                path = keys;
            }
            Some('[') => {
                p.i += 1;
                let keys = p.key()?;
                p.expect("]")?;
                table_at(&mut root, &keys).map_err(|e| p.msg(&e))?;
                path = keys;
            }
            Some(_) => {
                let keys = p.key()?;
                p.expect("=")?;
                p.ws();
                let v = p.value()?;
                let t = table_at(&mut root, &path).map_err(|e| p.msg(&e))?;
                insert(t, &keys, v).map_err(|e| p.msg(&e))?;
            }
        }
        p.end_of_line()?;
    }
}

/// The table at `path`; an array of tables stands for its last element.
fn table_at<'m>(
    mut cur: &'m mut Map<String, Value>,
    path: &[String],
) -> Result<&'m mut Map<String, Value>, String> {
    for k in path {
        let v = cur
            .entry(k.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        cur = match v {
            Value::Object(m) => m,
            Value::Array(a) => match a.last_mut() {
                Some(Value::Object(m)) => m,
                _ => return Err(format!("{k} is not a table")),
            },
            _ => return Err(format!("{k} is not a table")),
        };
    }
    Ok(cur)
}

fn insert(t: &mut Map<String, Value>, keys: &[String], v: Value) -> Result<(), String> {
    let (last, parent) = keys.split_last().ok_or("empty key")?;
    let t = table_at(t, parent)?;
    if t.contains_key(last) {
        return Err(format!("{last} is set twice"));
    }
    t.insert(last.clone(), v);
    Ok(())
}

struct P {
    c: Vec<char>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn at(&self, n: usize) -> Option<char> {
        self.c.get(self.i + n).copied()
    }

    fn msg(&self, m: &str) -> String {
        let line = self.c[..self.i.min(self.c.len())]
            .iter()
            .filter(|&&c| c == '\n')
            .count()
            + 1;
        format!("line {line}: {m}")
    }

    fn err<T>(&self, m: &str) -> Result<T, String> {
        Err(self.msg(m))
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.i += 1;
        }
    }

    fn comment(&mut self) {
        if self.peek() == Some('#') {
            while !matches!(self.peek(), None | Some('\n')) {
                self.i += 1;
            }
        }
    }

    /// Whitespace, newlines and comments.
    fn blank(&mut self) {
        loop {
            self.ws();
            self.comment();
            match self.peek() {
                Some('\n' | '\r') => self.i += 1,
                _ => return,
            }
        }
    }

    fn end_of_line(&mut self) -> Result<(), String> {
        self.ws();
        self.comment();
        match self.peek() {
            None | Some('\n' | '\r') => Ok(()),
            Some(c) => self.err(&format!("unexpected {c:?}")),
        }
    }

    fn expect(&mut self, s: &str) -> Result<(), String> {
        self.ws();
        for want in s.chars() {
            if self.peek() != Some(want) {
                return self.err(&format!("expected {s:?}"));
            }
            self.i += 1;
        }
        Ok(())
    }

    /// `a`, `a.b`, `"a b".c`.
    fn key(&mut self) -> Result<Vec<String>, String> {
        let mut parts = Vec::new();
        loop {
            self.ws();
            let part = match self.peek() {
                Some('"') => self.basic()?,
                Some('\'') => self.literal()?,
                _ => {
                    let start = self.i;
                    while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    {
                        self.i += 1;
                    }
                    if start == self.i {
                        return self.err("expected a key");
                    }
                    self.c[start..self.i].iter().collect()
                }
            };
            parts.push(part);
            self.ws();
            if self.peek() != Some('.') {
                return Ok(parts);
            }
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('"') if self.at(1) == Some('"') && self.at(2) == Some('"') => {
                self.err("multi-line strings are not supported")
            }
            Some('"') => self.basic().map(Value::String),
            Some('\'') => self.literal().map(Value::String),
            Some('[') => self.array(),
            Some('{') => self.inline(),
            Some('t' | 'f') => {
                let word: String = self.c[self.i..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .collect();
                let v = match word.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return self.err(&format!("unknown value {word}")),
                };
                self.i += word.len();
                Ok(Value::Bool(v))
            }
            Some(c) if c.is_ascii_digit() || c == '-' || c == '+' => {
                let start = self.i;
                self.i += 1;
                while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '_') {
                    self.i += 1;
                }
                let n: String = self.c[start..self.i]
                    .iter()
                    .filter(|&&c| c != '_')
                    .collect();
                match n.parse::<i64>() {
                    Ok(n) => Ok(Value::from(n)),
                    Err(_) => self.err("only whole numbers are supported"),
                }
            }
            _ => self.err("expected a value"),
        }
    }

    fn basic(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return self.err("unterminated string");
            };
            self.i += 1;
            match c {
                '"' => return Ok(out),
                '\n' => return self.err("unterminated string"),
                '\\' => {
                    let e = self.peek();
                    self.i += 1;
                    match e {
                        Some('n') => out.push('\n'),
                        Some('t') => out.push('\t'),
                        Some('r') => out.push('\r'),
                        Some('"') => out.push('"'),
                        Some('\\') => out.push('\\'),
                        Some('u') => {
                            let hex: String = self
                                .c
                                .get(self.i..self.i + 4)
                                .unwrap_or(&[])
                                .iter()
                                .collect();
                            let ch = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
                            match ch {
                                Some(ch) if hex.len() == 4 => out.push(ch),
                                _ => return self.err("bad \\u escape"),
                            }
                            self.i += 4;
                        }
                        _ => return self.err("unknown escape"),
                    }
                }
                c => out.push(c),
            }
        }
    }

    fn literal(&mut self) -> Result<String, String> {
        self.i += 1;
        let start = self.i;
        while !matches!(self.peek(), None | Some('\'' | '\n')) {
            self.i += 1;
        }
        if self.peek() != Some('\'') {
            return self.err("unterminated string");
        }
        let s = self.c[start..self.i].iter().collect();
        self.i += 1;
        Ok(s)
    }

    fn array(&mut self) -> Result<Value, String> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            self.blank();
            if self.peek() == Some(']') {
                self.i += 1;
                return Ok(Value::Array(out));
            }
            out.push(self.value()?);
            self.blank();
            match self.peek() {
                Some(',') => self.i += 1,
                Some(']') => {}
                _ => return self.err("expected , or ]"),
            }
        }
    }

    fn inline(&mut self) -> Result<Value, String> {
        self.i += 1;
        let mut t = Map::new();
        self.ws();
        if self.peek() == Some('}') {
            self.i += 1;
            return Ok(Value::Object(t));
        }
        loop {
            let keys = self.key()?;
            self.expect("=")?;
            self.ws();
            let v = self.value()?;
            insert(&mut t, &keys, v).map_err(|e| self.msg(&e))?;
            self.ws();
            match self.peek() {
                Some(',') => self.i += 1,
                Some('}') => {
                    self.i += 1;
                    return Ok(Value::Object(t));
                }
                _ => return self.err("expected , or }"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use serde_json::json;

    #[test]
    fn reads_the_package_manifest_subset() {
        let t = parse(
            r#"
# a package
[package]
name = "kotlin"   # trailing comment
version = '0.1.0'
n = 1_000

[[grammar]]
name = "kotlin"
extensions = [
  "kt",
  "kts", # comment
]

[[tool]]
name = "a"
[tool.targets]
"x86_64-linux" = { url = "http://h/a.tar.gz", sha256 = "ab" }

[[tool]]
name = "b"
dotted.key = true
"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::Value::Object(t),
            json!({
                "package": {"name": "kotlin", "version": "0.1.0", "n": 1000},
                "grammar": [{"name": "kotlin", "extensions": ["kt", "kts"]}],
                "tool": [
                    {"name": "a", "targets": {"x86_64-linux": {"url": "http://h/a.tar.gz", "sha256": "ab"}}},
                    {"name": "b", "dotted": {"key": true}},
                ],
            })
        );
    }

    #[test]
    fn errors_name_the_line() {
        assert_eq!(
            parse("a = 1\nb = \"x").unwrap_err(),
            "line 2: unterminated string"
        );
        assert_eq!(parse("a = 1\na = 2").unwrap_err(), "line 2: a is set twice");
        assert!(parse("a = 1.5").unwrap_err().starts_with("line 1"));
    }
}
