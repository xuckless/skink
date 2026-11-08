//! Paint a `skink_term::Screen` into a ratatui buffer.
//!
//! Approach follows tui-term (MIT, https://github.com/a-kenji/tui-term):
//! map each emulator cell to a ratatui cell. Written fresh against skink-term's types.

use crate::theme::Theme;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use skink_term::{Cell, Color as TColor, Screen};

/// Tokyo Night ANSI palette (indices 0-15).
const ANSI: [(u8, u8, u8); 16] = [
    (0x15, 0x16, 0x1e),
    (0xf7, 0x76, 0x8e),
    (0x9e, 0xce, 0x6a),
    (0xe0, 0xaf, 0x68),
    (0x7a, 0xa2, 0xf7),
    (0xbb, 0x9a, 0xf7),
    (0x7d, 0xcf, 0xff),
    (0xa9, 0xb1, 0xd6),
    (0x41, 0x48, 0x68),
    (0xf7, 0x76, 0x8e),
    (0x9e, 0xce, 0x6a),
    (0xe0, 0xaf, 0x68),
    (0x7a, 0xa2, 0xf7),
    (0xbb, 0x9a, 0xf7),
    (0x7d, 0xcf, 0xff),
    (0xc0, 0xca, 0xf5),
];

fn color(c: TColor, default: (u8, u8, u8), t: &Theme) -> Color {
    match c {
        TColor::Default => t.c(default),
        TColor::Indexed(i) if i < 16 => t.c(ANSI[i as usize]),
        TColor::Indexed(i) => Color::Indexed(i),
        TColor::Rgb(r, g, b) => t.c((r, g, b)),
    }
}

fn style(cell: &Cell, t: &Theme) -> Style {
    let (mut fg, mut bg) = (color(cell.fg, t.fg, t), color(cell.bg, t.bg, t));
    if cell.attrs.reverse() {
        std::mem::swap(&mut fg, &mut bg);
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

pub fn render(buf: &mut Buffer, area: Rect, screen: &Screen, t: &Theme) {
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
                target.set_style(style(cell, t));
                continue;
            }
            text.clear();
            cell.push_text(&mut text);
            target.set_symbol(if text.is_empty() { " " } else { &text });
            target.set_style(style(cell, t));
        }
    }
}
