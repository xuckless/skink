//! Colors. The default palette is Tokyo Night (folke/tokyonight.nvim, MIT);
//! colors are plain RGB values, nothing else is taken from that project.
//! On terminals without truecolor they are mapped to the xterm-256 cube.

use ratatui::style::Color;

#[derive(Clone, Copy)]
pub struct Theme {
    pub true_color: bool,
    pub bg: (u8, u8, u8),
    pub bg_dark: (u8, u8, u8),
    pub bg_hl: (u8, u8, u8),
    pub bg_sel: (u8, u8, u8),
    /// Find matches other than the selected one.
    pub bg_search: (u8, u8, u8),
    pub bg_popup: (u8, u8, u8),
    pub fg: (u8, u8, u8),
    pub fg_dim: (u8, u8, u8),
    pub comment: (u8, u8, u8),
    pub line_nr: (u8, u8, u8),
    pub guide: (u8, u8, u8),
    pub blue: (u8, u8, u8),
    pub cyan: (u8, u8, u8),
    pub green: (u8, u8, u8),
    pub orange: (u8, u8, u8),
    pub purple: (u8, u8, u8),
    pub red: (u8, u8, u8),
    pub yellow: (u8, u8, u8),
    pub teal: (u8, u8, u8),
    pub diff_ours: (u8, u8, u8),
    pub diff_theirs: (u8, u8, u8),
}

impl Theme {
    pub fn tokyo_night(true_color: bool) -> Self {
        Self {
            true_color,
            bg: (0x1a, 0x1b, 0x26),
            bg_dark: (0x16, 0x16, 0x1e),
            bg_hl: (0x29, 0x2e, 0x42),
            bg_sel: (0x28, 0x3a, 0x5e),
            bg_search: (0x3d, 0x59, 0xa1),
            bg_popup: (0x1f, 0x23, 0x35),
            fg: (0xc0, 0xca, 0xf5),
            fg_dim: (0xa9, 0xb1, 0xd6),
            comment: (0x56, 0x5f, 0x89),
            line_nr: (0x3b, 0x42, 0x61),
            guide: (0x2a, 0x2f, 0x45),
            blue: (0x7a, 0xa2, 0xf7),
            cyan: (0x7d, 0xcf, 0xff),
            green: (0x9e, 0xce, 0x6a),
            orange: (0xff, 0x9e, 0x64),
            purple: (0xbb, 0x9a, 0xf7),
            red: (0xf7, 0x76, 0x8e),
            yellow: (0xe0, 0xaf, 0x68),
            teal: (0x73, 0xda, 0xca),
            diff_ours: (0x20, 0x30, 0x3b),
            diff_theirs: (0x2b, 0x2b, 0x46),
        }
    }

    /// Detect truecolor support from the environment (`COLORTERM`).
    pub fn auto() -> Self {
        let tc = std::env::var("COLORTERM")
            .is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"));
        Self::tokyo_night(tc)
    }

    pub fn c(&self, rgb: (u8, u8, u8)) -> Color {
        if self.true_color {
            Color::Rgb(rgb.0, rgb.1, rgb.2)
        } else {
            Color::Indexed(to_256(rgb))
        }
    }
}

impl Theme {
    /// Style for a syntax class (None = default foreground).
    pub fn hl_style(&self, h: skink_syntax::Hl) -> ratatui::style::Style {
        use ratatui::style::{Modifier, Style};
        use skink_syntax::Hl;
        let fg = |c| Style::new().fg(self.c(c));
        match h {
            Hl::Keyword => fg(self.purple),
            Hl::Function | Hl::Method => fg(self.blue),
            Hl::Macro | Hl::Builtin | Hl::Namespace => fg(self.cyan),
            Hl::Type | Hl::Constructor => fg((0x2a, 0xc3, 0xde)),
            Hl::Constant | Hl::Number | Hl::Boolean => fg(self.orange),
            Hl::Parameter | Hl::Attribute => fg(self.yellow),
            Hl::Property => fg(self.teal),
            Hl::String => fg(self.green),
            Hl::Escape | Hl::Operator => fg((0x89, 0xdd, 0xff)),
            Hl::Comment => fg(self.comment).add_modifier(Modifier::ITALIC),
            Hl::Punctuation => fg((0x9a, 0xa5, 0xce)),
            Hl::Tag | Hl::Error => fg(self.red),
            Hl::Label => fg(self.blue),
            Hl::Heading => fg(self.blue).add_modifier(Modifier::BOLD),
            Hl::Emphasis => Style::new().add_modifier(Modifier::ITALIC),
            Hl::Strong => Style::new().add_modifier(Modifier::BOLD),
            Hl::Link => fg(self.cyan).add_modifier(Modifier::UNDERLINED),
            Hl::Literal => fg(self.green),
            _ => Style::new(),
        }
    }
}

/// Nearest xterm-256 color (6x6x6 cube or the grey ramp).
pub fn to_256((r, g, b): (u8, u8, u8)) -> u8 {
    let cube = |v: u8| -> u8 {
        match v {
            0..=47 => 0,
            48..=114 => 1,
            _ => ((v as u16 - 35) / 40) as u8,
        }
    };
    let (ri, gi, bi) = (cube(r), cube(g), cube(b));
    let level = |i: u8| -> i32 { if i == 0 { 0 } else { 55 + 40 * i as i32 } };
    let cube_idx = 16 + 36 * ri + 6 * gi + bi;
    let cube_d = (level(ri) - r as i32).pow(2)
        + (level(gi) - g as i32).pow(2)
        + (level(bi) - b as i32).pow(2);
    let avg = (r as i32 + g as i32 + b as i32) / 3;
    let gi2 = ((avg - 8).max(0) / 10).min(23) as u8;
    let gv = 8 + 10 * gi2 as i32;
    let grey_d = (gv - r as i32).pow(2) + (gv - g as i32).pow(2) + (gv - b as i32).pow(2);
    if grey_d < cube_d { 232 + gi2 } else { cube_idx }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_to_256() {
        assert_eq!(to_256((0, 0, 0)), 16);
        assert_eq!(to_256((255, 255, 255)), 231);
        assert!(to_256((0x1a, 0x1b, 0x26)) >= 16);
        // A mid grey prefers the grey ramp.
        assert!(to_256((128, 128, 128)) >= 232);
        assert!(Theme::tokyo_night(false).c((255, 0, 0)) == Color::Indexed(196));
    }
}
