//! Display-width helpers: tabs, wide chars, clipping to a viewport.

use ratatui::{
    style::{Modifier, Style},
    text::Span,
};
use unicode_width::UnicodeWidthChar;

pub fn char_width(c: char, x: usize, tab: usize) -> usize {
    if c == '\t' {
        tab - x % tab
    } else {
        c.width().unwrap_or(0)
    }
}

/// Display column of char index `col`, expanding tabs to tab stops.
pub fn display_col(line: &str, col: usize, tab: usize) -> usize {
    let mut x = 0;
    for c in line.chars().take(col) {
        x += char_width(c, x, tab);
    }
    x
}

/// Char column whose cell contains display column `x` (for mouse clicks).
pub fn char_col_at(line: &str, x: usize, tab: usize) -> usize {
    let mut pos = 0;
    for (i, c) in line.chars().enumerate() {
        let w = char_width(c, pos, tab);
        if x < pos + w.max(1) {
            // Click on the right half of a wide cell goes after it.
            return if w > 1 && c != '\t' && x >= pos + w / 2 {
                i + 1
            } else {
                i
            };
        }
        pos += w;
    }
    line.chars().count()
}

/// The part of `line` visible in `[hscroll, hscroll + width)` as styled
/// spans; `sel` is a selected char-column range (end exclusive).
pub fn spans(
    line: &str,
    hscroll: usize,
    width: usize,
    tab: usize,
    sel: Option<(usize, usize)>,
) -> Vec<Span<'static>> {
    let sel_style = Style::new().add_modifier(Modifier::REVERSED);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_sel = false;
    let flush = |run: &mut String, selected: bool, out: &mut Vec<Span<'static>>| {
        if !run.is_empty() {
            let style = if selected { sel_style } else { Style::new() };
            out.push(Span::styled(std::mem::take(run), style));
        }
    };
    let mut x = 0;
    let mut count = 0;
    for (i, c) in line.chars().enumerate() {
        count = i + 1;
        let w = char_width(c, x, tab);
        let (start, end) = (x, x + w);
        x = end;
        if end <= hscroll {
            continue;
        }
        if start >= hscroll + width {
            break;
        }
        let selected = sel.is_some_and(|(s, e)| i >= s && i < e);
        if selected != run_sel {
            flush(&mut run, run_sel, &mut out);
            run_sel = selected;
        }
        if start < hscroll || end > hscroll + width || c == '\t' {
            let from = start.max(hscroll);
            let to = end.min(hscroll + width);
            run.extend(std::iter::repeat_n(' ', to - from));
        } else {
            run.push(c);
        }
    }
    flush(&mut run, run_sel, &mut out);
    // A selected line break shows as one highlighted cell.
    if let Some((_, e)) = sel {
        let len = line.chars().count();
        if e > len && count == len && x >= hscroll && x < hscroll + width {
            out.push(Span::styled(" ", sel_style));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(spans: Vec<Span<'static>>) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn tabs_expand_to_stops() {
        assert_eq!(display_col("\tx", 1, 4), 4);
        assert_eq!(display_col("a\tx", 2, 4), 4);
        assert_eq!(plain(spans("a\tb", 0, 10, 4, None)), "a   b");
    }

    #[test]
    fn clicks_map_back_to_chars() {
        assert_eq!(char_col_at("a\tb", 0, 4), 0);
        assert_eq!(char_col_at("a\tb", 2, 4), 1);
        assert_eq!(char_col_at("a\tb", 4, 4), 2);
        assert_eq!(char_col_at("ab", 99, 4), 2);
    }

    #[test]
    fn horizontal_clip() {
        assert_eq!(plain(spans("0123456789", 3, 4, 4, None)), "3456");
    }

    #[test]
    fn selection_is_reversed() {
        let s = spans("abcd", 0, 10, 4, Some((1, 3)));
        assert_eq!(s.len(), 3);
        assert_eq!(s[1].content, "bc");
        assert!(s[1].style.add_modifier.contains(Modifier::REVERSED));
    }
}
