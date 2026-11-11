# Themes, icons and fonts

`:theme` (or the colorscheme row of `:settings`) lists every theme and previews the one you move to; Enter picks it, Esc puts the old one back. `:theme NAME` switches straight away. Either way the choice is saved to settings.json (`"colorscheme": "NAME"`).

Built in: tokyonight-night, tokyonight-storm, tokyonight-moon, tokyonight-day, catppuccin-mocha, catppuccin-latte, gruvbox-dark, gruvbox-light, one-dark, one-light, high-contrast. More come as [packages](packages).

`"colorscheme": "auto"` (the default) is Tokyo Night, day or night after the terminal's background: COLORFGBG when set, else Skink asks terminals known to answer (Ghostty, kitty, WezTerm, iTerm2, Terminal.app, Alacritty, foot, VS Code, GNOME/VTE, Konsole, tmux) for at most 250 ms. Others get the dark one.

## Your own theme

`<config>/themes/NAME.json`:

```json
{"name": "Mine", "base": "gruvbox-dark", "colors": {"bg": "#1d2021"}}
```

Without a base only bg and fg are required; the rest is mixed from them. Colours: bg bg_dark bg_hl bg_sel bg_search bg_popup fg fg_dim comment line_nr guide blue cyan green orange purple red yellow teal type operator punct diff_ours diff_theirs diff_add diff_del debug_line.

Micro colorschemes load as they are, from `<config>/colorschemes/NAME.micro` or Micro's `~/.config/micro/colorschemes`. Terminal tabs take their 16 colours from the theme.

## Icons and fonts

Icons are Nerd Font glyphs; the font belongs to your terminal (Ghostty, Kitty and WezTerm include Nerd Font symbols). `"icons": "ascii"` or `SKINK_ICONS=ascii` switches to plain text.
