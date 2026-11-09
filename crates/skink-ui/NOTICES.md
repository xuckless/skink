# Third-party notices: skink-ui

No code is copied into this crate; these are linked dependencies added for
previews (licences checked by cargo-deny).

| Crate | Licence | Use |
|---|---|---|
| [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) 0.13 | MIT | Markdown preview (`:preview`, Alt-V); default build, ~0.4 MB |
| [resvg](https://github.com/linebender/resvg) 0.49 | Apache-2.0 OR MIT | SVG preview, half-block raster; optional `svg-preview` feature (~0.8 MB, off by default: the default build would pass 15 MB) |
| [grep-searcher](https://github.com/BurntSushi/ripgrep/tree/master/crates/searcher), [grep-regex](https://github.com/BurntSushi/ripgrep/tree/master/crates/regex), [grep-matcher](https://github.com/BurntSushi/ripgrep/tree/master/crates/matcher) 0.1 | Unlicense OR MIT | Project-wide search (`projsearch.rs`, Alt-s) |
| [serde](https://serde.rs) / serde_json 1 | MIT OR Apache-2.0 | Pane-tree shape for session restore |

Design references (no code copied): the pane tree in `panes.rs` follows the
binary split-tree layout of [zellij](https://github.com/zellij-org/zellij)
and [WezTerm](https://github.com/wez/wezterm) (both MIT).

## Themes

The built-in themes (`themes/*.json`) contain colour values only, no code or
files from these projects. Each file names its source.

| Theme | Palette source | Licence |
|---|---|---|
| tokyonight-night, -storm, -moon, -day | [folke/tokyonight.nvim](https://github.com/folke/tokyonight.nvim) (`extras/lua/tokyonight_*.lua` palettes) | Apache-2.0 |
| catppuccin-mocha, catppuccin-latte | [catppuccin/catppuccin](https://github.com/catppuccin/catppuccin) (style guide palette) | MIT |
| gruvbox-dark, gruvbox-light | [morhetz/gruvbox](https://github.com/morhetz/gruvbox) (palette in the README) | MIT/X11 |
| high-contrast | Skink's own | MIT OR Apache-2.0 |

Micro colorschemes (`.micro`) are read in Micro's own format
(`color-link GROUP "FG,BG"`), parsed from scratch. Micro's colorscheme files
themselves are not shipped.

| Crate | Licence | Use |
|---|---|---|
| [rustix](https://github.com/bytecodealliance/rustix) 1 (unix) | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | `poll` for the 250 ms terminal-background (OSC 11) query at start; already linked on Linux through arboard |
