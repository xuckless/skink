//! Paint a `skink_term::Screen` into a ratatui buffer.
//!
//! Approach follows tui-term (MIT, https://github.com/a-kenji/tui-term):
//! map each emulator cell to a ratatui cell. Written fresh against skink-term's types.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use skink_term::{Cell, Color as TColor, Screen};

fn color(c: TColor) -> Color {
    match c {
        TColor::Default => Color::Reset,
        TColor::Indexed(i) => Color::Indexed(i),
        TColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn style(cell: &Cell) -> Style {
    let (mut fg, mut bg) = (color(cell.fg), color(cell.bg));
    if cell.attrs.reverse() {
        std::mem::swap(&mut fg, &mut bg);
        // Reversed default colors would both be Reset: force a visible pair.
        if fg == Color::Reset && bg == Color::Reset {
            return Style::new().add_modifier(Modifier::REVERSED);
        }
    }
    let mut s = Style::new().fg(fg).bg(bg);
    if cell.attrs.bold() {
        s = s.add_modifier(Modifier::BOLD);
    }
    if cell.attrs.italic() {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if cell.attrs.underline() {
        s = s.add_modifier(Modifier::UNDERLINED);
    }
    if cell.attrs.dim() {
        s = s.add_modifier(Modifier::DIM);
    }
    s
}

pub fn render(buf: &mut Buffer, area: Rect, screen: &Screen) {
    let rows = area.height.min(screen.rows);
    let cols = area.width.min(screen.cols);
    let mut text = String::new();
    for r in 0..rows {
        for c in 0..cols {
            let Some(cell) = screen.cell(r, c) else {
                continue;
            };
            let target = &mut buf[(area.x + c, area.y + r)];
            if cell.is_wide_continuation() {
                target.set_symbol("");
                target.set_style(style(cell));
                continue;
            }
            text.clear();
            cell.push_text(&mut text);
            target.set_symbol(if text.is_empty() { " " } else { &text });
            target.set_style(style(cell));
        }
    }
}
