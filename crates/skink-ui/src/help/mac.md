# macOS: Option as Alt

Mac terminals type a character for Option+key (Option+Z gives "Ω"). Skink reads those characters back as the Alt chord when that chord does something, so Option+Z zooms but Ω still types where Option+Z means nothing. `"macoptionisalt"`: `"auto"` (the default), `"always"` or `"never"`.

Dead keys (Option+E, I, N, U, \`) cannot be read back. Set your terminal to send Option as Alt:

| Terminal | Setting |
|---|---|
| Ghostty | `macos-option-as-alt = left` |
| Kitty | `macos_option_as_alt left` |
| WezTerm | `send_composed_key_when_left_alt_is_pressed = false` |
| iTerm2 | Profiles › Keys › Left Option key: Esc+ |
| Terminal.app | Settings › Profiles › Keyboard › Use Option as Meta key |
| Alacritty | `option_as_alt = "OnlyLeft"` |

Skink also turns on the kitty keyboard protocol in Ghostty, Kitty and WezTerm.

Keys that never need Alt: F1 palette, `:recent`, `:zoom`, `:dotfiles`, F5 / F9 / F10 debugging; Ctrl-Z is always undo. And every key has a [typed command](typed).
