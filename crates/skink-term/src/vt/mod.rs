// Vendored from doy/vt100-rust (crates.io vt100 0.16.2, git eb66ffaf7d771c13303ef73b29f6f2a56fdacecf), MIT, Copyright (c) 2016 Jesse Luehrs.
// Changes: lib.rs turned into this module file; lints relaxed (see NOTICES.md).
//! Terminal screen model (vendored `vt100`).
#![allow(
    dead_code,
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::nursery
)]

mod attrs;
mod callbacks;
mod cell;
mod grid;
mod parser;
mod perform;
mod row;
mod screen;
mod term;

pub use attrs::Color;
pub use callbacks::Callbacks;
pub use cell::Cell;
pub use parser::Parser;
pub use screen::{MouseProtocolEncoding, MouseProtocolMode, Screen};
