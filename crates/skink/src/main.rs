use skink_config::Keymap;
use skink_core::{Buffer, Editor, parse_location};
use skink_host::HostEndpoint;
use skink_ui::{App, run};
use std::{path::Path, process::ExitCode};

const USAGE: &str =
    "usage: skink [--version] [path[:line[:col]]]\n  path may be a file or a directory";

fn main() -> ExitCode {
    let mut target = None;
    let mut exit_after_first_frame = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("skink {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            // Hidden: draw one frame and exit (startup/RSS measurement).
            "--exit-after-first-frame" => exit_after_first_frame = true,
            a if a.starts_with('-') && a != "-" => {
                eprintln!("skink: unknown option {a}\n{USAGE}");
                return ExitCode::from(2);
            }
            _ if target.is_some() => {
                eprintln!("skink: only one path is supported yet\n{USAGE}");
                return ExitCode::from(2);
            }
            _ => target = Some(arg),
        }
    }

    let (mut editor, goto) = match target {
        None => (Editor::new(Buffer::empty()), None),
        Some(arg) => {
            let loc = parse_location(&arg, Path::exists);
            match Buffer::open(&loc.path) {
                Ok(b) => (Editor::new(b), Some((loc.line, loc.col))),
                // A path that does not exist yet opens as an empty buffer
                // (saving arrives in Phase 1).
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    (Editor::new(Buffer::empty()), None)
                }
                Err(e) => {
                    eprintln!("skink: {}: {e}", loc.path.display());
                    return ExitCode::FAILURE;
                }
            }
        }
    };
    if let Some((line, col)) = goto {
        editor.goto(line, col);
    }

    let host = HostEndpoint::from_env();
    let app = App::new(
        editor,
        Keymap::defaults(),
        skink_host::backend(host.as_ref()),
    );
    match run(app, exit_after_first_frame) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("skink: {e}");
            ExitCode::FAILURE
        }
    }
}
