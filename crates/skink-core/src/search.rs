//! Find: every query (literal or regex, any case mode, whole word) compiles
//! to one `regex::Regex` over the buffer text; positions come back as char
//! indices. The regex crate is already in the build through tree-sitter.

use crate::Editor;
use regex::{Regex, RegexBuilder};

/// How letter case is matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Case {
    /// Case-insensitive unless the query has an uppercase letter.
    #[default]
    Smart,
    Sensitive,
}

/// Find-bar toggles (Alt-R, Alt-C, Alt-W).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOptions {
    pub regex: bool,
    pub case: Case,
    pub whole_word: bool,
}

impl SearchOptions {
    /// Compile `query`; an invalid regex comes back as a short message.
    pub fn compile(&self, query: &str) -> Result<Regex, String> {
        let body = if self.regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };
        let body = if self.whole_word {
            format!(r"\b(?:{body})\b")
        } else {
            body
        };
        let insensitive = match self.case {
            Case::Smart => !query.chars().any(char::is_uppercase),
            Case::Sensitive => false,
        };
        RegexBuilder::new(&body)
            .case_insensitive(insensitive)
            .multi_line(true)
            .size_limit(1 << 20)
            .build()
            .map_err(|e| match e {
                regex::Error::Syntax(s) => {
                    s.lines().last().unwrap_or("bad regex").trim().to_owned()
                }
                other => other.to_string(),
            })
    }
}

/// Where the current match is among all matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchCount {
    /// 1-based index of the match at the primary selection; 0 if none is selected.
    pub current: usize,
    pub total: usize,
    /// Counting stopped at `MAX_COUNT`.
    pub capped: bool,
}

const MAX_COUNT: usize = 9_999;

impl Editor {
    /// Non-empty matches of `re` as char ranges, within chars `[lo, hi)`.
    pub fn matches_in(&self, re: &Regex, lo: usize, hi: usize) -> Vec<(usize, usize)> {
        let text = self.buffer.slice(lo, hi);
        let mut chars = lo;
        let mut byte = 0;
        let mut out = Vec::new();
        for m in re.find_iter(&text).filter(|m| !m.is_empty()) {
            chars += text[byte..m.start()].chars().count();
            let len = m.as_str().chars().count();
            out.push((chars, chars + len));
            chars += len;
            byte = m.end();
        }
        out
    }

    /// Select the first match of `re` at or after char `from` (wrapping).
    /// Live search uses this from where the find bar opened.
    pub fn select_match_from(&mut self, re: &Regex, from: usize) -> bool {
        let text = self.buffer.text();
        let from_b = self.buffer.char_to_byte(from.min(self.buffer.len_chars()));
        let hit = re
            .find_iter(&text[from_b..])
            .map(|m| (from_b + m.start(), from_b + m.end()))
            .chain(re.find_iter(&text).map(|m| (m.start(), m.end())))
            .find(|(a, b)| a < b);
        self.select_bytes(hit)
    }

    /// Select the next (or previous) match after (before) the primary
    /// selection, wrapping.
    pub fn select_next_match(&mut self, re: &Regex, forward: bool) -> bool {
        let (s, e) = self
            .selection()
            .unwrap_or((self.cursor_idx(), self.cursor_idx()));
        if forward {
            return self.select_match_from(re, e);
        }
        let text = self.buffer.text();
        let before = self.buffer.char_to_byte(s);
        let all: Vec<(usize, usize)> = re
            .find_iter(&text)
            .filter(|m| !m.is_empty())
            .map(|m| (m.start(), m.end()))
            .collect();
        let hit = all
            .iter()
            .rev()
            .find(|(a, _)| *a < before)
            .or(all.last())
            .copied();
        self.select_bytes(hit)
    }

    fn select_bytes(&mut self, hit: Option<(usize, usize)>) -> bool {
        let Some((a, b)) = hit else {
            return false;
        };
        let (a, b) = (self.buffer.byte_to_char(a), self.buffer.byte_to_char(b));
        self.secondary.clear();
        self.history.close();
        self.anchor = Some(self.cursor_at(a));
        self.set_cursor_idx(b);
        true
    }

    /// Number of matches and the index of the selected one.
    pub fn match_count(&self, re: &Regex) -> MatchCount {
        let text = self.buffer.text();
        let sel = self
            .selection()
            .map(|(s, e)| (self.buffer.char_to_byte(s), self.buffer.char_to_byte(e)));
        let mut c = MatchCount {
            current: 0,
            total: 0,
            capped: false,
        };
        for m in re.find_iter(&text).filter(|m| !m.is_empty()) {
            if c.total == MAX_COUNT {
                c.capped = true;
                break;
            }
            c.total += 1;
            if sel == Some((m.start(), m.end())) {
                c.current = c.total;
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Buffer;

    #[test]
    fn options_change_what_matches() {
        let e = Editor::new(Buffer::from_text("Foo foo food\nfo+o"));
        let count = |q: &str, o: SearchOptions| e.match_count(&o.compile(q).unwrap()).total;
        let d = SearchOptions::default();
        assert_eq!(count("foo", d), 3); // smart case: lowercase matches Foo
        assert_eq!(count("Foo", d), 1);
        assert_eq!(
            count(
                "foo",
                SearchOptions {
                    whole_word: true,
                    ..d
                }
            ),
            2
        );
        assert_eq!(count("fo+o", d), 1); // literal by default
        assert_eq!(count("fo+o", SearchOptions { regex: true, ..d }), 3);
        assert!(SearchOptions { regex: true, ..d }.compile("(").is_err());
    }

    #[test]
    fn next_previous_wrap_and_count_the_current_match() {
        let mut e = Editor::new(Buffer::from_text("a1 a2 a3"));
        let re = SearchOptions::default().compile("a").unwrap();
        assert!(e.select_match_from(&re, 4));
        assert_eq!(e.selection(), Some((6, 7)));
        assert_eq!(e.match_count(&re).current, 3);
        assert!(e.select_next_match(&re, true)); // wraps to the first
        assert_eq!(e.selection(), Some((0, 1)));
        assert!(e.select_next_match(&re, false)); // and back to the last
        assert_eq!(e.selection(), Some((6, 7)));
        assert_eq!(e.matches_in(&re, 2, 8), vec![(3, 4), (6, 7)]);
    }
}
