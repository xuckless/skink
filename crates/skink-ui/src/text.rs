//! Display-width helpers and per-cell styling of one source line: tabs, wide
//! chars, horizontal clipping, syntax colors, selection and indent guides.

use ratatui::{style::Style, text::Span};
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

/// How to paint one line.
pub struct LineStyle<'a> {
    pub base: Style,
    /// Syntax spans in char columns (sorted, non-overlapping).
    pub hl: &'a [(usize, usize, Style)],
    /// Selected char-column range, end exclusive.
    pub sel: Option<(usize, usize)>,
    pub sel_style: Style,
    /// Show the line break as a selected cell when the selection continues.
    pub sel_eol: bool,
    /// Indent guide glyph and style; `None` disables guides.
    pub guide: Option<(&'a str, Style)>,
    pub tab: usize,
}

/// The part of `line` visible in `[hscroll, hscroll + width)` as styled
/// spans, padded with the base style to exactly `width` cells.
pub fn spans(line: &str, hscroll: usize, width: usize, ls: &LineStyle) -> Vec<Span<'static>> {
    let indent_end = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
    let indent_w = display_col(line, indent_end, ls.tab);
    let mut cells: Vec<(String, Style)> = Vec::with_capacity(width);
    let mut hl_i = 0;
    let mut x = 0;
    let mut n = 0;
    for (i, c) in line.chars().enumerate() {
        n = i + 1;
        let w = char_width(c, x, ls.tab);
        let (start, end) = (x, x + w);
        x = end;
        if end <= hscroll {
            continue;
        }
        if start >= hscroll + width {
            break;
        }
        while hl_i < ls.hl.len() && ls.hl[hl_i].1 <= i {
            hl_i += 1;
        }
        let mut st = ls.base;
        if let Some((s, e, hs)) = ls.hl.get(hl_i) {
            if *s <= i && i < *e {
                st = st.patch(*hs);
            }
        }
        if ls.sel.is_some_and(|(s, e)| i >= s && i < e) {
            st = st.patch(ls.sel_style);
        }
        if c == '\t' || start < hscroll || end > hscroll + width {
            for col in start.max(hscroll)..end.min(hscroll + width) {
                // Indent guide at tab stops inside the leading whitespace.
                let guide = ls
                    .guide
                    .filter(|_| i < indent_end && col < indent_w && col % ls.tab == 0);
                match guide {
                    Some((g, gs)) => cells.push((g.to_owned(), st.patch(gs))),
                    None => cells.push((" ".to_owned(), st)),
                }
            }
        } else if c == ' '
            && i < indent_end
            && start % ls.tab == 0
            && start < indent_w
            && ls.guide.is_some()
        {
            let (g, gs) = ls.guide.unwrap_or(("|", Style::new()));
            cells.push((g.to_owned(), st.patch(gs)));
        } else {
            cells.push((c.to_string(), st));
            // A wide char occupies its extra columns itself.
            for _ in 1..w {
                cells.push((String::new(), st));
            }
        }
    }
    // A selected line break shows as one highlighted cell.
    let len = line.chars().count();
    if ls.sel_eol && n == len && x >= hscroll && x < hscroll + width {
        cells.push((" ".to_owned(), ls.base.patch(ls.sel_style)));
    }
    // Pad to the full width so the line background reaches the edge.
    for _ in cells.len()..width {
        cells.push((" ".to_owned(), ls.base));
    }
    // Coalesce runs of equal style.
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_style = ls.base;
    for (s, st) in cells {
        if st != run_style && !run.is_empty() {
            out.push(Span::styled(std::mem::take(&mut run), run_style));
        }
        run_style = st;
        run.push_str(&s);
    }
    if !run.is_empty() {
        out.push(Span::styled(run, run_style));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    fn ls<'a>(sel: Option<(usize, usize)>, hl: &'a [(usize, usize, Style)]) -> LineStyle<'a> {
        LineStyle {
            base: Style::new(),
            hl,
            sel,
            sel_style: Style::new().add_modifier(Modifier::REVERSED),
            sel_eol: false,
            guide: None,
            tab: 4,
        }
    }

    fn plain(spans: Vec<Span<'static>>) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn tabs_expand_to_stops() {
        assert_eq!(display_col("\tx", 1, 4), 4);
        assert_eq!(display_col("a\tx", 2, 4), 4);
        assert_eq!(plain(spans("a\tb", 0, 5, &ls(None, &[]))), "a   b");
    }

    #[test]
    fn clicks_map_back_to_chars() {
        assert_eq!(char_col_at("a\tb", 0, 4), 0);
        assert_eq!(char_col_at("a\tb", 2, 4), 1);
        assert_eq!(char_col_at("a\tb", 4, 4), 2);
        assert_eq!(char_col_at("ab", 99, 4), 2);
    }

    #[test]
    fn horizontal_clip_and_padding() {
        assert_eq!(plain(spans("0123456789", 3, 4, &ls(None, &[]))), "3456");
        assert_eq!(plain(spans("ab", 0, 5, &ls(None, &[]))), "ab   ");
    }

    #[test]
    fn selection_and_syntax_styles() {
        let hl = [(0, 2, Style::new().fg(Color::Red))];
        let s = spans("abcd", 0, 4, &ls(Some((1, 3)), &hl));
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].content, "a");
        assert_eq!(s[0].style.fg, Some(Color::Red));
        assert_eq!(s[1].content, "b");
        assert!(s[1].style.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(s[1].style.fg, Some(Color::Red)); // b is both highlighted and selected
        assert_eq!(s[2].content, "c");
        assert_eq!(s[2].style.fg, None);
    }

    #[test]
    fn indent_guides_at_tab_stops() {
        let mut l = ls(None, &[]);
        l.guide = Some(("|", Style::new()));
        assert_eq!(plain(spans("        x", 0, 10, &l)), "|   |   x ");
        assert_eq!(plain(spans("\t\tx", 0, 10, &l)), "|   |   x ");
        assert_eq!(plain(spans("x", 0, 3, &l)), "x  ");
    }
}
