//! The debug adapters Skink knows, found on `PATH` like language servers.
//! A launch configuration's `type` picks one; its other fields go to the
//! adapter as the `launch` / `attach` arguments (as in VS Code).

use crate::client::{AdapterProcess, Transport};
use std::net::TcpListener;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adapter {
    /// GDB's built-in adapter (`gdb -i dap`, GDB 14 and later): C, C++,
    /// Rust, Zig, Go (without goroutines).
    Gdb,
    /// LLVM's `lldb-dap` (formerly `lldb-vscode`).
    LldbDap,
    /// CodeLLDB (`codelldb --port N`), launch.json type `lldb`.
    CodeLldb,
    /// Python: `python -m debugpy.adapter`.
    Debugpy,
    /// Go: `dlv dap --listen 127.0.0.1:N`.
    Delve,
    /// Node / browsers: js-debug. Needs child sessions (`startDebugging`),
    /// which Skink does not run yet.
    JsDebug,
}

pub const ALL: [Adapter; 6] = [
    Adapter::Gdb,
    Adapter::LldbDap,
    Adapter::CodeLldb,
    Adapter::Debugpy,
    Adapter::Delve,
    Adapter::JsDebug,
];

impl Adapter {
    /// The adapter for a launch.json `type`.
    pub fn for_type(ty: &str) -> Option<Adapter> {
        Some(match ty {
            "gdb" | "cppdbg" => Adapter::Gdb,
            "lldb-dap" | "lldb-vscode" => Adapter::LldbDap,
            // CodeLLDB's type; plain lldb-dap serves it when CodeLLDB is absent.
            "lldb" | "codelldb" if skink_lsp::find_in_path("codelldb").is_some() => {
                Adapter::CodeLldb
            }
            "lldb" | "codelldb" => Adapter::LldbDap,
            "debugpy" | "python" => Adapter::Debugpy,
            "go" | "delve" | "dlv" => Adapter::Delve,
            "node" | "pwa-node" | "js" | "chrome" | "pwa-chrome" => Adapter::JsDebug,
            _ => return None,
        })
    }

    /// The `type` written in generated configurations.
    pub fn type_name(self) -> &'static str {
        match self {
            Adapter::Gdb => "gdb",
            Adapter::LldbDap => "lldb-dap",
            Adapter::CodeLldb => "lldb",
            Adapter::Debugpy => "debugpy",
            Adapter::Delve => "go",
            Adapter::JsDebug => "pwa-node",
        }
    }

    /// `initialize.adapterID`.
    pub fn id(self) -> &'static str {
        match self {
            Adapter::Gdb => "gdb",
            Adapter::LldbDap => "lldb-dap",
            Adapter::CodeLldb => "lldb",
            Adapter::Debugpy => "debugpy",
            Adapter::Delve => "go",
            Adapter::JsDebug => "pwa-node",
        }
    }

    /// The program looked up on `PATH`.
    pub fn program(self) -> &'static str {
        match self {
            Adapter::Gdb => "gdb",
            Adapter::LldbDap => "lldb-dap",
            Adapter::CodeLldb => "codelldb",
            Adapter::Debugpy => {
                if cfg!(windows) {
                    "python"
                } else {
                    "python3"
                }
            }
            Adapter::Delve => "dlv",
            Adapter::JsDebug => "js-debug-adapter",
        }
    }

    /// How to install it, for the "not found" message.
    pub fn install_hint(self) -> &'static str {
        match self {
            Adapter::Gdb => "install GDB 14 or later",
            Adapter::LldbDap => "install LLVM's lldb-dap",
            Adapter::CodeLldb => "install CodeLLDB (codelldb on PATH)",
            Adapter::Debugpy => "pip install debugpy",
            Adapter::Delve => "go install github.com/go-delve/delve/cmd/dlv@latest",
            Adapter::JsDebug => "not supported yet",
        }
    }

    /// Whether the adapter can be started here (checks `PATH` only).
    pub fn installed(self) -> bool {
        self.find().is_some()
    }

    fn find(self) -> Option<PathBuf> {
        match self {
            // lldb-dap was called lldb-vscode before LLVM 18.
            Adapter::LldbDap => skink_lsp::find_in_path("lldb-dap")
                .or_else(|| skink_lsp::find_in_path("lldb-vscode")),
            a => skink_lsp::find_in_path(a.program()),
        }
    }

    /// The process to start, or why it cannot be.
    pub fn process(self) -> Result<AdapterProcess, String> {
        if self == Adapter::JsDebug {
            return Err(
                "js-debug needs child debug sessions (startDebugging), which Skink does not run yet"
                    .into(),
            );
        }
        let Some(program) = self.find() else {
            return Err(format!(
                "{} not found on PATH ({})",
                self.program(),
                self.install_hint()
            ));
        };
        let port = || free_port().ok_or("no free local port for the adapter".to_string());
        let (args, transport) = match self {
            Adapter::Gdb => (vec!["-i".into(), "dap".into()], Transport::Stdio),
            Adapter::LldbDap => (Vec::new(), Transport::Stdio),
            Adapter::Debugpy => (
                vec!["-m".into(), "debugpy.adapter".into()],
                Transport::Stdio,
            ),
            Adapter::CodeLldb => {
                let p = port()?;
                (
                    vec!["--port".into(), p.to_string()],
                    Transport::Tcp { port: p },
                )
            }
            Adapter::Delve => {
                let p = port()?;
                (
                    vec!["dap".into(), "--listen".into(), format!("127.0.0.1:{p}")],
                    Transport::Tcp { port: p },
                )
            }
            Adapter::JsDebug => unreachable!("handled above"),
        };
        Ok(AdapterProcess {
            program,
            args,
            cwd: None,
            env: Vec::new(),
            transport,
        })
    }
}

/// A port nothing listens on right now (the adapter binds it next).
fn free_port() -> Option<u16> {
    let l = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    l.local_addr().ok().map(|a| a.port())
}
