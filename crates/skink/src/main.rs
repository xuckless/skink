use skink_config::Keymap;
use skink_core::{Buffer, Editor, parse_location};
use skink_host::HostEndpoint;
use skink_ui::{App, run, update};
use std::{path::Path, process::ExitCode};

const USAGE: &str = "usage: skink [--version] [path[:line[:col]]]
       skink --upgrade [--check] [--version X]   (also: skink upgrade ...)
  path may be a file or a directory; no path inside a project restores
  its last session";

/// `skink --upgrade [--check] [--version X]`.
fn upgrade(args: &[String]) -> ExitCode {
    let mut mode = update::Mode::Install;
    let mut pin = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--upgrade" | "upgrade" => {}
            "--check" => mode = update::Mode::Check,
            "--version" => match it.next() {
                Some(v) => pin = Some(v.clone()),
                None => {
                    eprintln!("skink: --version needs a version, e.g. --version 0.1.0");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("skink: unknown upgrade option {other}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let source = update::Source::from_env(pin.as_deref());
    match update::upgrade(&source, mode) {
        Ok(o) => {
            println!("{o}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("skink: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    update::cleanup_previous();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "upgrade") || args.iter().any(|a| a == "--upgrade") {
        return upgrade(&args);
    }
    let mut target = None;
    let mut exit_after_first_frame = false;
    for arg in args {
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

    let no_args = target.is_none();
    let mut tree_root = None;
    let (mut editor, goto, is_file) = match target {
        None => (Editor::new(Buffer::empty()), None, false),
        Some(arg) => {
            let loc = parse_location(&arg, Path::exists);
            if loc.path.is_dir() {
                // A directory opens the file tree beside an empty buffer.
                tree_root = Some(loc.path.clone());
                (Editor::new(Buffer::empty()), None, false)
            } else {
                match Buffer::open(&loc.path) {
                    Ok(b) => {
                        let file = !b.is_read_only();
                        (Editor::new(b), Some((loc.line, loc.col)), file)
                    }
                    // A path that does not exist yet opens as an empty buffer
                    // that remembers the name, so the first save creates it.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        (Editor::new(Buffer::new_file(&loc.path)), None, true)
                    }
                    Err(e) => {
                        eprintln!("skink: {}: {e}", loc.path.display());
                        return ExitCode::FAILURE;
                    }
                }
            }
        }
    };
    if let Some((line, col)) = goto {
        editor.goto(line, col);
    }

    let host = HostEndpoint::from_env();
    let mut app = App::new(
        editor,
        Keymap::defaults(),
        skink_host::backend(host.as_ref()),
    );
    // Language servers run locally (when installed); inside EveryAI the
    // delegated provider takes over once it is wired (see docs/HANDOVER.md).
    app.startup();
    if let Some(root) = tree_root {
        app.set_tree_root(root, true);
    }
    if no_args && !exit_after_first_frame {
        app.start_session_here();
    }
    if is_file && !exit_after_first_frame {
        app.request_lsp();
    }
    match run(app, exit_after_first_frame) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("skink: {e}");
            ExitCode::FAILURE
        }
    }
}
