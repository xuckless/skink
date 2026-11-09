//! LSP snippet syntax (`$1`, `${2:name}`, `${3|a,b|}`, `$0`) to plain text
//! plus tabstop ranges. Variables (`$TM_FILENAME`, `${VAR:default}`) expand
//! to their default (or nothing): servers rarely send them.

/// Parsed snippet: the text to insert and its tabstops in visiting order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    /// Tabstops `1, 2, ...` then `0` (the final cursor). Each holds every
    /// occurrence as char ranges relative to the start of `text`; several
    /// occurrences of one number become several cursors.
    pub stops: Vec<Vec<(usize, usize)>>,
}

impl Snippet {
    pub fn parse(src: &str) -> Snippet {
        let mut p = P {
            s: src.chars().collect(),
            i: 0,
            out: String::new(),
            len: 0,
            found: Vec::new(),
        };
        p.body(None);
        let mut nums: Vec<u32> = p.found.iter().map(|(n, ..)| *n).collect();
        nums.sort_unstable_by_key(|&n| (n == 0, n));
        nums.dedup();
        let mut stops: Vec<Vec<(usize, usize)>> = nums
            .iter()
            .map(|&n| {
                p.found
                    .iter()
                    .filter(|(m, ..)| *m == n)
                    .map(|&(_, a, b)| (a, b))
                    .collect()
            })
            .collect();
        if !nums.contains(&0) {
            stops.push(vec![(p.len, p.len)]);
        }
        Snippet { text: p.out, stops }
    }
}

struct P {
    s: Vec<char>,
    i: usize,
    out: String,
    /// `out` length in chars.
    len: usize,
    /// (number, start, end) per tabstop occurrence.
    found: Vec<(u32, usize, usize)>,
}

impl P {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn push(&mut self, c: char) {
        self.out.push(c);
        self.len += 1;
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        (self.i > start).then(|| {
            self.s[start..self.i]
                .iter()
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
    }

    fn name(&mut self) -> bool {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.i += 1;
        }
        self.i > start
    }

    /// Text up to the closing `}` of an enclosing `${` (or the end).
    fn body(&mut self, close: Option<char>) {
        while let Some(c) = self.peek() {
            if Some(c) == close {
                return;
            }
            self.i += 1;
            match c {
                '\\' => {
                    if let Some(n) = self.peek() {
                        self.i += 1;
                        self.push(n);
                    }
                }
                '$' => self.dollar(),
                _ => self.push(c),
            }
        }
    }

    fn dollar(&mut self) {
        let at = self.len;
        if let Some(n) = self.number() {
            self.found.push((n, at, at));
            return;
        }
        if self.peek() == Some('{') {
            self.i += 1;
            if let Some(n) = self.number() {
                match self.peek() {
                    Some(':') => {
                        self.i += 1;
                        self.body(Some('}'));
                    }
                    Some('|') => {
                        self.i += 1;
                        let start = self.i;
                        while self.peek().is_some_and(|c| c != '|' && c != ',') {
                            self.i += 1;
                        }
                        let first: String = self.s[start..self.i].iter().collect();
                        first.chars().for_each(|c| self.push(c));
                        while self.peek().is_some_and(|c| c != '}') {
                            self.i += 1;
                        }
                    }
                    _ => {}
                }
                self.i += 1; // '}'
                self.found.push((n, at, self.len));
                return;
            }
            // ${VAR} or ${VAR:default}
            self.name();
            if self.peek() == Some(':') {
                self.i += 1;
                self.body(Some('}'));
            } else {
                while self.peek().is_some_and(|c| c != '}') {
                    self.i += 1;
                }
            }
            self.i += 1;
            return;
        }
        if !self.name() {
            self.push('$');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabstops_placeholders_choices_and_final() {
        let s = Snippet::parse("fn ${1:name}(${2:arg}: ${3|u8,u16|}) {\n\t$0\n}");
        assert_eq!(s.text, "fn name(arg: u8) {\n\t\n}");
        assert_eq!(s.stops[0], vec![(3, 7)]);
        assert_eq!(s.stops[1], vec![(8, 11)]);
        assert_eq!(s.stops[2], vec![(13, 15)]);
        assert_eq!(s.stops[3], vec![(20, 20)]); // $0 last
    }

    #[test]
    fn mirrored_stops_nested_and_escapes() {
        let s = Snippet::parse("${1:a} = $1; \\$x ${2:f(${3:y})}$TM_X");
        assert_eq!(s.text, "a = ; $x f(y)");
        assert_eq!(s.stops[0], vec![(0, 1), (4, 4)]);
        assert_eq!(s.stops[1], vec![(9, 13)]);
        assert_eq!(s.stops[2], vec![(11, 12)]);
        assert_eq!(s.stops.last().unwrap(), &vec![(13, 13)]); // implicit $0
        assert_eq!(Snippet::parse("push($1)").text, "push()");
    }
}
