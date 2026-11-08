# Third-party notices

Skink is MIT OR Apache-2.0. Code adapted from other projects keeps its
upstream license and copyright; each adapted chunk carries a header comment
naming the source, commit and changes. Per-crate detail lives next to the code:

| Crate | Notices |
|---|---|
| skink-core | Undo-history idea from rhysd/tui-textarea (MIT), reimplemented over ropey; ropey (MIT) as the text store. See header of `src/history.rs`. |
| skink-config | Default key bindings adapted from zyedidia/micro `internal/action/defaults_other.go` @ c9ba51d (MIT). See `src/lib.rs`. |
| skink-host | [`crates/skink-host/NOTICES.md`](crates/skink-host/NOTICES.md): no copied code; EveryAI's host source was read for wire protocol only (FSL-1.1, not copied). |
| skink-lsp | [`crates/skink-lsp/NOTICES.md`](crates/skink-lsp/NOTICES.md): framing/lifecycle adapted from lapce/lapce @ f66ffaa (Apache-2.0). |
| skink-ai | [`crates/skink-ai/NOTICES.md`](crates/skink-ai/NOTICES.md): ACP field names from agent-client-protocol-schema (Apache-2.0); ThePrimeagen/99 has no license so only its ideas are used. |
| skink-term | [`crates/skink-term/NOTICES.md`](crates/skink-term/NOTICES.md): vendored `vt100` 0.16.2 (MIT, `src/vt/LICENSE-vt100`); portable-pty (MIT). |
| skink-syntax | [`crates/skink-syntax/NOTICES.md`](crates/skink-syntax/NOTICES.md): tree-sitter and MIT grammar crates; some bundled queries adjusted (Zig query descends from nvim-treesitter, Apache-2.0). |
| skink-ui | Terminal painting follows the approach of a-kenji/tui-term (MIT); written fresh. Colors are plain RGB values of the Tokyo Night palette (folke/tokyonight.nvim, MIT); Nerd Font glyph codepoints are public. Quick-open matching is written in-house. |

Not used as a source of code: Zed (GPL/AGPL), Helix and nucleo (MPL-2.0), EveryAI (FSL-1.1), ThePrimeagen/99 (unlicensed).
