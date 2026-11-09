//! Colours. A theme is a set of named RGB values; the built-in ones ship as
//! theme files (themes/*.json, the format users write too) and Micro
//! colorschemes load as well. On terminals without truecolor the values are
//! mapped to the xterm-256 cube.
//!
//! Format: relaxed JSON (comments, trailing commas) like settings.json, so
//! there is one parser and no extra dependency:
//! `{"name", "appearance": "dark"|"light", "base": "<theme>",
//! "colors": {"bg": "#1a1b26", ...}}`. Only `bg` and `fg` are required:
//! missing accents come from the base (else Tokyo Night of the same
//! appearance) and the rest is mixed from what is given.
//!
//! Palettes: Tokyo Night (folke/tokyonight.nvim, Apache-2.0), Catppuccin
//! (catppuccin/catppuccin, MIT), Gruvbox (morhetz/gruvbox, MIT/X11); only
//! colour values are taken (see NOTICES.md).

use std::{collections::BTreeMap, path::PathBuf};

use ratatui::style::Color;

type Rgb = (u8, u8, u8);

macro_rules! theme_fields {
    ($($field:ident = $key:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct Theme {
            pub true_color: bool,
            pub dark: bool,
            $(pub $field: Rgb,)*
        }

        /// Every colour key a theme file can set.
        pub const KEYS: &[&str] = &[$($key),*];

        impl Theme {
            fn from_map(m: &BTreeMap<String, Rgb>, dark: bool, true_color: bool) -> Self {
                Self { true_color, dark, $($field: m[$key],)* }
            }
        }
    };
}

theme_fields! {
    bg = "bg", bg_dark = "bg_dark", bg_hl = "bg_hl", bg_sel = "bg_sel",
    bg_search = "bg_search", bg_popup = "bg_popup", fg = "fg", fg_dim = "fg_dim",
    comment = "comment", line_nr = "line_nr", guide = "guide", blue = "blue",
    cyan = "cyan", green = "green", orange = "orange", purple = "purple",
    red = "red", yellow = "yellow", teal = "teal", ty = "type",
    operator = "operator", punct = "punct", diff_ours = "diff_ours",
    diff_theirs = "diff_theirs", diff_add = "diff_add", diff_del = "diff_del",
    debug_line = "debug_line",
}

/// Built-in themes: (name, file).
pub const BUILT_IN: &[(&str, &str)] = &[
    (
        "tokyonight-night",
        include_str!("../themes/tokyonight-night.json"),
    ),
    (
        "tokyonight-storm",
        include_str!("../themes/tokyonight-storm.json"),
    ),
    (
        "tokyonight-moon",
        include_str!("../themes/tokyonight-moon.json"),
    ),
    (
        "tokyonight-day",
        include_str!("../themes/tokyonight-day.json"),
    ),
    (
        "catppuccin-mocha",
        include_str!("../themes/catppuccin-mocha.json"),
    ),
    (
        "catppuccin-latte",
        include_str!("../themes/catppuccin-latte.json"),
    ),
    ("gruvbox-dark", include_str!("../themes/gruvbox-dark.json")),
    (
        "gruvbox-light",
        include_str!("../themes/gruvbox-light.json"),
    ),
    (
        "high-contrast",
        include_str!("../themes/high-contrast.json"),
    ),
];

/// The themes `"colorscheme": "auto"` picks.
pub const DEFAULT_DARK: &str = "tokyonight-night";
pub const DEFAULT_LIGHT: &str = "tokyonight-day";

/// What the terminal's background looks like (for `"colorscheme": "auto"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    /// Not asked yet, no answer, or not a terminal known to answer: dark.
    Unknown,
    Dark,
    Light,
}

impl Appearance {
    /// From a background colour: light when its luminance is above half.
    pub fn of(bg: Rgb) -> Self {
        let l = 0.2126 * f64::from(bg.0) + 0.7152 * f64::from(bg.1) + 0.0722 * f64::from(bg.2);
        if l > 127.5 {
            Appearance::Light
        } else {
            Appearance::Dark
        }
    }

    /// The theme `"auto"` means.
    pub fn theme(self) -> &'static str {
        match self {
            Appearance::Light => DEFAULT_LIGHT,
            Appearance::Dark | Appearance::Unknown => DEFAULT_DARK,
        }
    }
}

/// Where a theme comes from (shown in the picker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    BuiltIn,
    /// `<config>/themes/NAME.json`.
    File(PathBuf),
    /// A Micro `.micro` colorscheme.
    Micro(PathBuf),
}

impl Origin {
    pub fn tag(&self) -> &'static str {
        match self {
            Origin::BuiltIn => "built-in",
            Origin::File(_) => "theme file",
            Origin::Micro(_) => "micro",
        }
    }
}

/// One theme file, parsed but not resolved.
#[derive(Debug, Clone, Default)]
struct Raw {
    /// `Some(true)`: dark.
    appearance: Option<bool>,
    base: Option<String>,
    colors: BTreeMap<String, Rgb>,
}

fn hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn parse_json(text: &str) -> Result<Raw, String> {
    use serde_json::Value;
    let v: Value = serde_json::from_str(&skink_config::relaxed_json(text))
        .map_err(|e| format!("not valid JSON: {e}"))?;
    let obj = v.as_object().ok_or("a theme must be an object: { ... }")?;
    let mut raw = Raw::default();
    for (k, v) in obj {
        match (k.as_str(), v) {
            ("name" | "$schema", _) => {}
            ("appearance", Value::String(a)) if a == "dark" || a == "light" => {
                raw.appearance = Some(a == "dark");
            }
            ("appearance", _) => return Err("appearance must be \"dark\" or \"light\"".into()),
            ("base", Value::String(b)) => raw.base = Some(b.clone()),
            ("colors", Value::Object(c)) => {
                for (ck, cv) in c {
                    if !KEYS.contains(&ck.as_str()) {
                        return Err(format!(
                            "unknown colour \"{ck}\" (known: {})",
                            KEYS.join(", ")
                        ));
                    }
                    let rgb = cv
                        .as_str()
                        .and_then(hex)
                        .ok_or_else(|| format!("{ck} must be \"#rrggbb\""))?;
                    raw.colors.insert(ck.clone(), rgb);
                }
            }
            (k, _) => {
                return Err(format!(
                    "unknown theme key \"{k}\" (name, appearance, base, colors)"
                ));
            }
        }
    }
    Ok(raw)
}

/// xterm's 256 colours as RGB.
fn xterm(i: u8) -> Rgb {
    const BASIC: [Rgb; 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match i {
        0..=15 => BASIC[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        _ => {
            let g = 8 + 10 * (i - 232);
            (g, g, g)
        }
    }
}

/// A Micro colour: `#rrggbb`, 0-255, a name, or `default` (None).
fn micro_color(s: &str) -> Option<Rgb> {
    const NAMES: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    let s = s.trim().to_ascii_lowercase();
    if let Some(rgb) = hex(&s) {
        return Some(rgb);
    }
    if let Ok(n) = s.parse::<u8>() {
        return Some(xterm(n));
    }
    let (bright, name) = s.strip_prefix("bright").map_or((0, s.as_str()), |n| (8, n));
    let name = if name == "grey" || name == "gray" {
        "white"
    } else {
        name
    };
    NAMES
        .iter()
        .position(|n| *n == name)
        .map(|i| xterm(i as u8 + bright))
}

/// A Micro colorscheme (`color-link GROUP "[attrs] FG,BG"`) as theme
/// colours. Micro paints cursor-line and indent-char with their
/// foreground, so those map from it.
fn parse_micro(text: &str) -> Result<Raw, String> {
    // (group, take the background?, theme key); later lines win.
    const MAP: &[(&str, bool, &str)] = &[
        ("default", false, "fg"),
        ("default", true, "bg"),
        ("comment", false, "comment"),
        ("identifier", false, "blue"),
        ("statement", false, "purple"),
        ("preproc", false, "cyan"),
        ("type", false, "type"),
        ("constant", false, "orange"),
        ("constant.number", false, "orange"),
        ("constant.string", false, "green"),
        ("special", false, "yellow"),
        ("symbol", false, "operator"),
        ("symbol.operator", false, "operator"),
        ("symbol.brackets", false, "punct"),
        ("error", false, "red"),
        ("line-number", false, "line_nr"),
        ("cursor-line", false, "bg_hl"),
        ("selection", true, "bg_sel"),
        ("statusline", true, "bg_dark"),
        ("indent-char", false, "guide"),
    ];
    let mut raw = Raw::default();
    let mut seen = false;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("color-link") else {
            continue;
        };
        seen = true;
        let mut it = rest.trim().splitn(2, char::is_whitespace);
        let group = it.next().unwrap_or("");
        let spec = it.next().unwrap_or("").trim().trim_matches('"');
        let mut parts = spec.splitn(2, ',');
        let pick = |p: Option<&str>| {
            p.and_then(|p| p.split_whitespace().last())
                .and_then(micro_color)
        };
        let fg = pick(parts.next());
        let bg = pick(parts.next());
        for (g, use_bg, key) in MAP {
            if *g == group
                && let Some(c) = if *use_bg { bg } else { fg }
            {
                raw.colors.insert((*key).to_owned(), c);
            }
        }
    }
    if !seen {
        return Err("no color-link lines: not a Micro colorscheme".into());
    }
    if let Some(bg) = raw.colors.get("bg") {
        raw.appearance = Some(Appearance::of(*bg) != Appearance::Light);
    }
    Ok(raw)
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let m = |x: u8, y: u8| (f64::from(x) * (1.0 - t) + f64::from(y) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// Where theme files are looked for.
pub struct ThemeDirs {
    /// `<config>/themes` (`.json`) and `<config>/colorschemes` (`.micro`).
    pub config: Option<PathBuf>,
    /// Micro's config folder, so existing colorschemes work as they are.
    pub micro: Option<PathBuf>,
}

impl ThemeDirs {
    pub fn new(config: Option<PathBuf>) -> Self {
        let micro = std::env::var_os("MICRO_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
                Some(PathBuf::from(home).join(".config").join("micro"))
            });
        Self { config, micro }
    }

    /// (folder, extension) pairs, most specific first.
    fn dirs(&self) -> Vec<(PathBuf, &'static str)> {
        let mut v = Vec::new();
        if let Some(c) = &self.config {
            v.push((c.join("themes"), "json"));
            v.push((c.join("colorschemes"), "micro"));
        }
        if let Some(m) = &self.micro {
            v.push((m.join("colorschemes"), "micro"));
        }
        v
    }

    /// Every theme: built-in ones, then files (a file shadows nothing in
    /// the list but wins when loading).
    pub fn list(&self) -> Vec<(String, Origin)> {
        let mut out: Vec<(String, Origin)> = BUILT_IN
            .iter()
            .map(|(n, _)| ((*n).to_owned(), Origin::BuiltIn))
            .collect();
        for (dir, ext) in self.dirs() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            files.sort();
            for p in files {
                if p.extension().and_then(|e| e.to_str()) != Some(ext) {
                    continue;
                }
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if out.iter().any(|(n, _)| n == stem) {
                    continue;
                }
                let origin = match ext {
                    "json" => Origin::File(p.clone()),
                    _ => Origin::Micro(p.clone()),
                };
                out.push((stem.to_owned(), origin));
            }
        }
        out
    }

    fn raw(&self, name: &str, depth: usize) -> Result<Raw, String> {
        if depth > 8 {
            return Err(format!("{name}: base themes loop"));
        }
        let name = canonical_name(name);
        // A user file of the same name wins over a built-in theme.
        for (dir, ext) in self.dirs() {
            let path = dir.join(format!("{name}.{ext}"));
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let parsed = match ext {
                "json" => parse_json(&text),
                _ => parse_micro(&text),
            };
            let mut raw = parsed.map_err(|e| format!("{}: {e}", path.display()))?;
            if let Some(b) = raw.base.take() {
                raw = layer(self.raw(&b, depth + 1)?, raw);
            }
            return Ok(raw);
        }
        let (_, text) = BUILT_IN
            .iter()
            .find(|(n, _)| *n == name)
            .ok_or_else(|| format!("no theme \"{name}\" (:theme lists them)"))?;
        parse_json(text).map_err(|e| format!("{name}: {e}"))
    }

    /// Load a theme by name.
    pub fn load(&self, name: &str, true_color: bool) -> Result<Theme, String> {
        resolve(self.raw(name, 0)?, true_color)
    }
}

/// `over` on top of `base`.
fn layer(base: Raw, over: Raw) -> Raw {
    let mut colors = base.colors;
    colors.extend(over.colors);
    Raw {
        appearance: over.appearance.or(base.appearance),
        base: None,
        colors,
    }
}

/// Short names: `tokyonight` is the night variant, and so on.
pub fn canonical_name(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    match n.as_str() {
        "tokyonight" | "tokyo-night" => DEFAULT_DARK.into(),
        "catppuccin" => "catppuccin-mocha".into(),
        "gruvbox" => "gruvbox-dark".into(),
        "highcontrast" | "hc" => "high-contrast".into(),
        _ => n,
    }
}

/// Fill what a theme left out: accents from Tokyo Night of the same
/// appearance, the rest mixed from bg, fg and the accents.
fn resolve(raw: Raw, true_color: bool) -> Result<Theme, String> {
    let mut m = raw.colors;
    let (Some(&bg), Some(&fg)) = (m.get("bg"), m.get("fg")) else {
        return Err("a theme needs at least \"bg\" and \"fg\"".into());
    };
    let dark = raw
        .appearance
        .unwrap_or(Appearance::of(bg) != Appearance::Light);
    let fallback = if dark { BUILT_IN[0].1 } else { BUILT_IN[3].1 };
    let accents = parse_json(fallback).map(|r| r.colors).unwrap_or_default();
    for k in ["blue", "cyan", "green", "orange", "purple", "red", "yellow"] {
        let c = accents.get(k).copied().unwrap_or(fg);
        m.entry(k.to_owned()).or_insert(c);
    }
    let shade = if dark { (0, 0, 0) } else { (0x80, 0x80, 0x80) };
    // In order: later entries may use earlier ones.
    type Derive = fn(&BTreeMap<String, Rgb>, Rgb, Rgb, Rgb) -> Rgb;
    let derived: [(&str, Derive); 18] = [
        ("bg_dark", |_, bg, _, shade| mix(bg, shade, 0.15)),
        ("bg_hl", |_, bg, fg, _| mix(bg, fg, 0.07)),
        ("bg_sel", |m, bg, _, _| mix(bg, m["blue"], 0.25)),
        ("bg_search", |m, bg, _, _| mix(bg, m["blue"], 0.45)),
        ("bg_popup", |m, _, _, _| m["bg_dark"]),
        ("fg_dim", |_, bg, fg, _| mix(fg, bg, 0.12)),
        ("comment", |_, bg, fg, _| mix(fg, bg, 0.45)),
        ("line_nr", |_, bg, fg, _| mix(fg, bg, 0.65)),
        ("guide", |_, bg, fg, _| mix(fg, bg, 0.85)),
        ("teal", |m, _, _, _| m["cyan"]),
        ("type", |m, _, _, _| m["cyan"]),
        ("operator", |m, _, _, _| m["cyan"]),
        ("punct", |m, _, _, _| m["fg_dim"]),
        ("diff_ours", |m, bg, _, _| mix(bg, m["cyan"], 0.14)),
        ("diff_theirs", |m, bg, _, _| mix(bg, m["purple"], 0.14)),
        ("diff_add", |m, bg, _, _| mix(bg, m["green"], 0.18)),
        ("diff_del", |m, bg, _, _| mix(bg, m["red"], 0.18)),
        ("debug_line", |m, bg, _, _| mix(bg, m["yellow"], 0.16)),
    ];
    for (k, f) in derived {
        if !m.contains_key(k) {
            let c = f(&m, bg, fg, shade);
            m.insert(k.to_owned(), c);
        }
    }
    Ok(Theme::from_map(&m, dark, true_color))
}

/// The terminal advertises truecolor (`COLORTERM`).
pub fn true_color_env() -> bool {
    std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"))
}

impl Theme {
    pub fn tokyo_night(true_color: bool) -> Self {
        Self::built_in(DEFAULT_DARK, true_color)
    }

    /// A built-in theme by its exact name (they are fixed, so this panics
    /// on an unknown one).
    pub fn built_in(name: &str, true_color: bool) -> Self {
        let (_, text) = BUILT_IN
            .iter()
            .find(|(n, _)| *n == name)
            .expect("built-in theme");
        let raw = parse_json(text).expect("built-in theme parses");
        resolve(raw, true_color).expect("built-in theme resolves")
    }

    /// The default theme, with truecolor detected from the environment.
    pub fn auto() -> Self {
        Self::tokyo_night(true_color_env())
    }

    pub fn c(&self, rgb: Rgb) -> Color {
        if self.true_color {
            Color::Rgb(rgb.0, rgb.1, rgb.2)
        } else {
            Color::Indexed(to_256(rgb))
        }
    }

    /// ANSI colours 0-15 for terminal tabs, from the theme (on a light theme
    /// "black" is the light end, as light terminal themes have it).
    pub fn ansi(&self, i: u8) -> Rgb {
        let black = mix(self.bg, (0, 0, 0), 0.2);
        let base = [
            black,
            self.red,
            self.green,
            self.yellow,
            self.blue,
            self.purple,
            self.cyan,
            self.fg_dim,
        ];
        match i {
            8 => mix(self.comment, self.line_nr, 0.5),
            15 => self.fg,
            _ => base[(i % 8) as usize],
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
            Hl::Type | Hl::Constructor => fg(self.ty),
            Hl::Constant | Hl::Number | Hl::Boolean => fg(self.orange),
            Hl::Parameter | Hl::Attribute => fg(self.yellow),
            Hl::Property => fg(self.teal),
            Hl::String => fg(self.green),
            Hl::Escape | Hl::Operator => fg(self.operator),
            Hl::Comment => fg(self.comment).add_modifier(Modifier::ITALIC),
            Hl::Punctuation => fg(self.punct),
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

    #[test]
    fn built_in_themes_load_and_user_files_layer_and_micro_maps() {
        let none = ThemeDirs {
            config: None,
            micro: None,
        };
        for (n, _) in BUILT_IN {
            let t = none.load(n, true).unwrap();
            assert_eq!(
                t.dark,
                Appearance::of(t.bg) != Appearance::Light,
                "{n} appearance"
            );
        }
        // The old hard-coded palette is the night theme, value for value.
        let t = Theme::tokyo_night(true);
        assert_eq!(
            (t.bg, t.ty, t.operator, t.punct),
            (
                (0x1a, 0x1b, 0x26),
                (0x2a, 0xc3, 0xde),
                (0x89, 0xdd, 0xff),
                (0x9a, 0xa5, 0xce)
            )
        );
        let d = std::env::temp_dir().join(format!("skink-themes-{}", std::process::id()));
        std::fs::create_dir_all(d.join("themes")).unwrap();
        std::fs::create_dir_all(d.join("micro/colorschemes")).unwrap();
        std::fs::write(
            d.join("themes/mine.json"),
            "// mine\n{\"base\": \"gruvbox\", \"colors\": {\"red\": \"#ff0000\",},}",
        )
        .unwrap();
        std::fs::write(
            d.join("themes/bad.json"),
            r##"{"colors": {"bgg": "#000000"}}"##,
        )
        .unwrap();
        std::fs::write(
            d.join("micro/colorschemes/old.micro"),
            "color-link default \"#F8F8F2,#272822\"\ncolor-link comment \"italic #75715E\"\ncolor-link statement \"bold 197\"\ncolor-link cursor-line \"#3E3D32\"\n",
        )
        .unwrap();
        let dirs = ThemeDirs {
            config: Some(d.clone()),
            micro: Some(d.join("micro")),
        };
        let mine = dirs.load("mine", true).unwrap();
        assert_eq!((mine.red, mine.bg), ((255, 0, 0), (0x28, 0x28, 0x28)));
        assert!(
            dirs.load("bad", true)
                .unwrap_err()
                .contains("unknown colour \"bgg\"")
        );
        let old = dirs.load("old", true).unwrap();
        assert_eq!(
            (old.bg, old.comment, old.purple, old.bg_hl),
            (
                (0x27, 0x28, 0x22),
                (0x75, 0x71, 0x5e),
                xterm(197),
                (0x3e, 0x3d, 0x32)
            )
        );
        assert!(old.dark);
        let names: Vec<String> = dirs.list().into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&"mine".to_owned()) && names.contains(&"old".to_owned()));
        assert!(dirs.load("nope", true).unwrap_err().contains("no theme"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
