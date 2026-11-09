//! `skink-term`: embedded terminal emulator pane backend.
//!
//! A child process runs on a pty ([`portable_pty`], ConPTY on Windows); a
//! reader thread feeds its output to a (vendored) `vt100` screen model; the UI takes
//! cheap [`Screen`] snapshots and renders them itself. This crate is UI-toolkit
//! free and can also ship standalone.
//!
//! ```no_run
//! use std::time::Duration;
//! use skink_term::{CommandSpec, Term};
//! let t = Term::spawn(CommandSpec::new("echo").args(["hi"]), 24, 80, 1000)?;
//! t.wait_for(Duration::from_secs(2), |s| s.text().contains("hi"));
//! println!("{}", t.screen().text());
//! # Ok::<(), std::io::Error>(())
//! ```
#![forbid(unsafe_code)]

mod emu;
mod keys;
mod mouse;
mod screen;
mod sniff;
mod term;
mod vt;

pub use emu::{ClipboardRequest, History, Mark, MarkKind};
pub use keys::{KeyInput, TermKey, encode_key};
pub use mouse::{MouseButton, MouseEvent, MouseKind};
pub use screen::{Attrs, Cell, Color, Cursor, CursorShape, MouseMode, Screen};
pub use term::{CommandSpec, ExitStatus, Term, TermEvent, WakeFn};
