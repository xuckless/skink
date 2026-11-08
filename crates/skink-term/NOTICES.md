# skink-term: third-party code, decisions, notes

## Copied / adapted code

| Project | URL | Commit / version | License | What | Changes |
|---|---|---|---|---|---|
| vt100 (Jesse Luehrs) | https://github.com/doy/vt100-rust | crates.io `vt100 0.16.2`, git `eb66ffaf7d771c13303ef73b29f6f2a56fdacecf` | MIT, Copyright (c) 2016 Jesse Luehrs (`src/vt/LICENSE-vt100`) | `src/vt/*.rs` (attrs, callbacks, cell, grid, parser, perform, row, screen, term; `mod.rs` is its old `lib.rs`) | `crate::` -> `crate::vt::`; lints relaxed on the module; `rustfmt` applied; `Row::trim_blank_tail` + a call in `Grid::scroll_up` so scrollback rows drop trailing blank cells (see memory below). Robustness fixes found by fuzzing (each marked `// skink:`): `Grid::col_wrap` underflow on a 1-row screen, wide chars dropped on a 1-column screen (`Screen::text`), `Row::resize` clears a wide char cut in half by a column shrink, and `insert_cells` / `insert_lines` / `scroll_down` counts clamped to the screen (`CSI 65535 @` was O(n^2), ~1 s). Depends on `vte 0.15`, `unicode-width` (requirement loosened to `0.2.0`), `itoa` instead of the crate. |
| portable-pty (wezterm) | https://github.com/wezterm/wezterm | crates.io `portable-pty 0.9.0` | MIT | used as a dependency (pty on Unix, ConPTY on Windows) | none; it has no feature flags to trim (`serde_support` is already off by default) |
| vte (Alacritty/Joe Wilm) | https://github.com/alacritty/vte | crates.io `vte 0.15.0` | Apache-2.0 OR MIT | VT state machine, dependency of the vendored vt100 | none |

Ideas only (no code copied):
- `tui-term` (MIT, https://github.com/a-kenji/tui-term @ f0ffe97): shared parser fed by a reader thread (`Arc<RwLock<Parser>>`). Its examples' key->bytes handling is mostly `todo!()`, so key and mouse encoding in `keys.rs` / `mouse.rs` were written from the xterm `ctlseqs` spec.
- `alacritty_terminal` (Apache-2.0): replying to terminal queries through an event listener; here done by `sniff.rs`.

## Why vt100 and not alacritty_terminal

Both permissive, both handle truecolor, alt screen, scrollback, mouse modes (1000/1002/1003, SGR/UTF-8), bracketed paste, wide chars/combining marks and OSC title. Measured on Linux x86_64, release/fat-LTO, feeding the same 20.9 MB of colored text to a 40x120 emulator with 10k scrollback (parse-only harness, `cargo tree` counts exclude skink crates):

| | vt100 0.15.2 | vt100 0.16.2 (chosen, vendored) | alacritty_terminal 0.26 (no default features) |
|---|---|---|---|
| LoC | 4.0k | 4.0k | 10.7k |
| extra crates beyond portable-pty | vte, utf8parse, arrayvec, unicode-width 0.1, itoa, log (6) | vte, arrayvec, memchr, unicode-width, itoa (5) | ~25 (rustix, rustix-openpty, polling, signal-hook, regex-automata, parking_lot, base64, home, cursor-icon, bitflags, vte+ansi, ...) |
| harness binary | 349 KB | n/a | 415 KB (+66 KB) |
| parse time (20.9 MB) | 148 ms | ~390 ms end to end through a real pty (`cat` of the same file, incl. kernel tty + reader thread; ~54 MB/s) | 114 ms |
| RSS, 10k scrollback full, 120 cols | 43 MB (36 B/cell) | 32 B/cell, trimmed rows (see below) | 28.5 MB (24 B/cell) |
| cursor shape, dim, strikethrough, underline styles, kitty keyboard flags, DA replies | no | dim yes; rest no | yes |

Verdict: vt100 is lighter in code, dependencies and compile time, and has a simple, snapshot-friendly API. alacritty_terminal is more faithful and 1.5x denser per cell, but also needs its own pty glue we would not use (we need portable-pty for ConPTY) and pulls ~4x the crates. The gaps are covered cheaply: `sniff.rs` answers DA/DSR/CPR/XTWINOPS queries and tracks cursor style (DECSCUSR) and focus reporting (1004) on the side, and trailing-blank trimming closes most of the memory gap.

Why vendored 0.16.2 instead of the crate: vt100 0.15.2 (the newest that resolves against `skink-ui`'s `unicode-width =0.2.0` pin from ratatui 0.29) panics with "attempt to subtract with overflow" when the scrollback offset exceeds the screen height (fixed upstream in 0.16); 0.16+/0.18 need `unicode-width >=0.2.1` (0.2.2 for 0.18), which cannot coexist with ratatui 0.29's pin. Vendoring 0.16.2 with a relaxed `unicode-width = "0.2.0"` gives the fix, dim, OSC 52 and callbacks today. **When ratatui is bumped to a release that drops the pin (0.30), delete `src/vt/` and depend on `vt100 0.18` directly** (re-apply `trim_blank_tail` upstream or accept the memory cost). `Emu` is the only user of the vt API, so the swap is local.

## Measured cost (Linux x86_64, release, fat LTO, strip, panic=abort)

- Binary: a trivial program that spawns a `Term` and takes a screen snapshot is 587 KB vs 291 KB for hello-world: **+296 KB**.
- Idle RSS of a `Term` (pty + 2 threads + emulator, 40x80, scrollback cap 10 000, nothing buffered yet): **+0.7 MB**. Scrollback is allocated lazily as lines arrive.
- RSS with the 10 000-line scrollback completely full of ~98-character lines: **18 MB at 80 columns, 34 MB at 200 columns** (cell = 32 B; rows are trimmed of trailing blanks when they enter scrollback, and re-allocated at exact size because an in-place `shrink_to_fit` left unusable holes and doubled RSS). Before trimming (vt100 0.15.2) the same runs cost 26 MB / 64 MB. Recommendation: default to 2 000-3 000 scrollback lines per pane (about 4-10 MB when full).
- Threads: 3 per `Term` (reader, writer, waiter), 256 / 64 / 128 KiB stacks. The writer thread exists so input (keys, pastes, query replies) never blocks a caller on a child that stops reading. The threads hold only a `Weak` to the emulator, so dropping a `Term` frees its scrollback even if a grandchild keeps the pty open (that reader thread then stays blocked until the grandchild exits). No timers, no polling: idle CPU is zero (blocking `read` and `wait`).

## Not implemented / known gaps

- Cursor keys in keypad-application mode (DECKPAM) and `modifyOtherKeys` / kitty keyboard protocol: legacy xterm key encoding only. Shift-Enter and Ctrl-Enter encode as plain Enter (apps that need to tell them apart, e.g. Claude Code's newline binding, fall back to Alt-Enter = `ESC CR`, which works).
- Not tracked by the screen model: strikethrough, blink, underline color/styles, hyperlinks (OSC 8), sixel/images, synchronized-output (DECSET 2026 is ignored; harmless, output is just not batched), reflow on resize (like most emulators other than the big GUI ones, a shell redraws after SIGWINCH).
- Not answered: OSC 10/11 color queries, DECRQM, XTVERSION, kitty keyboard `CSI ? u`. Apps time out and fall back (well-behaved TUIs do).
- `skink_config::Key` has no F-keys or Insert: `TermKey` is a local superset (`From<Key>`); propose adding `Insert` and `F(u8)` to `skink_config::Key` and then `TermKey` can be removed.
- Windows ConPTY code paths compile and clippy-clean (checked with `--target x86_64-pc-windows-gnu`) but were not run here. Windows exit-by-signal and ConPTY drop semantics are untested; `Drop` kills the child, closes the master and detaches the reader/writer threads (they exit at EOF / queue close), so a stuck thread never hangs the UI.
