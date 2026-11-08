//! Headless demo: run a command on a pty, wait for it to exit, print what a
//! terminal would show.
//!
//! `cargo run -p skink-term --example skinkterm -- seq 1 5`
//! (no args: runs `echo hello from skink-term`)

use std::time::Duration;

use skink_term::{CommandSpec, Term};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let spec = match args.next() {
        Some(prog) => CommandSpec::new(prog).args(args),
        None => {
            CommandSpec::new(if cfg!(windows) { "cmd" } else { "echo" }).args(if cfg!(windows) {
                vec!["/C", "echo hello from skink-term"]
            } else {
                vec!["hello from skink-term"]
            })
        }
    };
    let term = Term::spawn(spec, 24, 80, 1000)?;
    let status = term.wait_exit(Duration::from_secs(30));
    // Give the reader a moment to drain the final output, then print.
    term.wait_for(Duration::from_millis(200), |_| false);
    let screen = term.screen();
    println!("{}", screen.text());
    println!(
        "-- exit: {:?}, cursor: ({}, {}), alt_screen: {}, scrollback: {}",
        status.map(|s| s.code),
        screen.cursor.row,
        screen.cursor.col,
        screen.alt_screen,
        screen.scrollback_len
    );
    Ok(())
}
