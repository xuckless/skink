//! Debug launch configurations: `.skink/launch.json` and
//! `.vscode/launch.json` (a VS Code subset: name, type, request,
//! preLaunchTask, per-OS overrides; every other field goes to the adapter
//! as is, with `${...}` variables expanded), plus cheap defaults for the
//! project and the file at hand (cargo binary, Python file or test at the
//! cursor, Go package or test at the cursor, a native program next to the
//! file).

use crate::App;
use serde_json::{Map, Value, json};
use skink_dap::{Adapter, RequestKind};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub name: String,
    pub adapter: Adapter,
    pub request: RequestKind,
    /// The adapter's `launch` / `attach` arguments, variables expanded.
    pub arguments: Value,
    /// A task label to run first (it must succeed).
    pub pre_launch_task: Option<String>,
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    File,
    Default,
}

/// Keys that are VS Code's, not the adapter's.
const CLIENT_KEYS: &[&str] = &[
    "name",
    "type",
    "request",
    "preLaunchTask",
    "postDebugTask",
    "presentation",
    "internalConsoleOptions",
    "windows",
    "osx",
    "linux",
];

/// Configurations from a launch.json text; also the ones skipped and why.
pub fn parse_launch_json(
    text: &str,
    root: &Path,
    file: Option<&Path>,
) -> Result<(Vec<Launch>, Vec<String>), String> {
    let v: Value = serde_json::from_str(&skink_config::relaxed_json(text))
        .map_err(|e| format!("launch.json: {e}"))?;
    let os_key = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    };
    let (mut out, mut skipped) = (Vec::new(), Vec::new());
    for c in v["configurations"].as_array().into_iter().flatten() {
        let Some(obj) = c.as_object() else { continue };
        let mut obj = obj.clone();
        if let Some(o) = obj.get(os_key).and_then(Value::as_object).cloned() {
            obj.extend(o);
        }
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed)")
            .to_owned();
        let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");
        let Some(adapter) = Adapter::for_type(ty) else {
            skipped.push(format!("{name}: unknown type {ty:?}"));
            continue;
        };
        let request = match obj.get("request").and_then(Value::as_str) {
            Some("attach") => RequestKind::Attach,
            _ => RequestKind::Launch,
        };
        let pre_launch_task = obj
            .get("preLaunchTask")
            .and_then(Value::as_str)
            .map(|t| expand_str(t, root, file));
        let args: Map<String, Value> = obj
            .into_iter()
            .filter(|(k, _)| !CLIENT_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k, expand_value(v, root, file)))
            .collect();
        out.push(Launch {
            name,
            adapter,
            request,
            arguments: Value::Object(args),
            pre_launch_task,
            origin: Origin::File,
        });
    }
    Ok((out, skipped))
}

fn expand_str(s: &str, root: &Path, file: Option<&Path>) -> String {
    crate::tasks::expand(s, root, file)
}

fn expand_value(v: Value, root: &Path, file: Option<&Path>) -> Value {
    match v {
        Value::String(s) => Value::String(expand_str(&s, root, file)),
        Value::Array(a) => {
            Value::Array(a.into_iter().map(|x| expand_value(x, root, file)).collect())
        }
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, x)| (k, expand_value(x, root, file)))
                .collect(),
        ),
        v => v,
    }
}

/// The native debugger to use: CodeLLDB, lldb-dap, else GDB.
fn native() -> Adapter {
    [Adapter::CodeLldb, Adapter::LldbDap, Adapter::Gdb]
        .into_iter()
        .find(|a| a.installed())
        .unwrap_or(Adapter::Gdb)
}

fn exe(p: PathBuf) -> PathBuf {
    if cfg!(windows) {
        p.with_extension("exe")
    } else {
        p
    }
}

/// `name = "..."` of `[package]` and each `[[bin]]` in a Cargo.toml.
fn cargo_bins(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut section = String::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.to_owned();
            continue;
        }
        if (section == "[package]" || section == "[[bin]]")
            && let Some(rest) = line.strip_prefix("name")
            && let Some(v) = rest.trim_start().strip_prefix('=')
        {
            let name = v.trim().trim_matches('"').to_owned();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    // A package whose binaries are listed explicitly runs those.
    if out.len() > 1 && text.contains("[[bin]]") {
        out.remove(0);
    }
    out
}

/// The `def test_...` (Python) or `func TestXxx(` (Go) the cursor is in.
fn test_at(text: &str, line: usize, py: bool) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    for l in lines
        .get(..=line.min(lines.len().saturating_sub(1)))?
        .iter()
        .rev()
    {
        let t = l.trim_start();
        if py {
            if let Some(rest) = t
                .strip_prefix("def ")
                .or_else(|| t.strip_prefix("async def "))
            {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                return name.starts_with("test").then_some(name);
            }
        } else if let Some(rest) = t.strip_prefix("func ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            return name.starts_with("Test").then_some(name);
        }
    }
    None
}

/// Configurations Skink offers without a launch.json.
pub fn defaults(root: &Path, file: Option<&Path>, text: &str, line: usize) -> Vec<Launch> {
    let mut out = Vec::new();
    let launch = |name: String, adapter: Adapter, args: Value, task: Option<&str>| Launch {
        name,
        adapter,
        request: RequestKind::Launch,
        arguments: args,
        pre_launch_task: task.map(str::to_owned),
        origin: Origin::Default,
    };
    if let Ok(cargo) = std::fs::read_to_string(root.join("Cargo.toml")) {
        for bin in cargo_bins(&cargo) {
            let program = exe(root.join("target").join("debug").join(&bin));
            out.push(launch(
                format!("cargo: {bin}"),
                native(),
                json!({"program": program, "args": [], "cwd": root}),
                Some("cargo build"),
            ));
        }
    }
    let ext = file.and_then(|f| f.extension()).and_then(|e| e.to_str());
    if let Some(f) = file {
        match ext {
            Some("py") => {
                if let Some(t) = test_at(text, line, true) {
                    let target = format!("{}::{t}", f.to_string_lossy());
                    out.push(launch(
                        format!("pytest: {t}"),
                        Adapter::Debugpy,
                        json!({"module": "pytest", "args": [target], "cwd": root,
                            "console": "integratedTerminal", "justMyCode": true}),
                        None,
                    ));
                }
                out.push(launch(
                    format!("python: {}", file_name(f)),
                    Adapter::Debugpy,
                    json!({"program": f, "cwd": root, "console": "integratedTerminal",
                        "justMyCode": true}),
                    None,
                ));
            }
            Some("go") => {
                let dir = f.parent().unwrap_or(root);
                if let Some(t) = test_at(text, line, false) {
                    out.push(launch(
                        format!("go test: {t}"),
                        Adapter::Delve,
                        json!({"mode": "test", "program": dir,
                            "args": ["-test.run", format!("^{t}$")]}),
                        None,
                    ));
                }
                out.push(launch(
                    format!("go: {}", file_name(dir)),
                    Adapter::Delve,
                    json!({"mode": "debug", "program": dir, "cwd": root}),
                    None,
                ));
            }
            Some("c" | "cc" | "cpp" | "cxx" | "zig" | "rs") => {
                // A program built next to the file (cc -g x.c -o x).
                let stem = f.with_extension("");
                let program = exe(stem);
                if program.is_file() {
                    out.push(launch(
                        format!("run {}", file_name(&program)),
                        native(),
                        json!({"program": program, "args": [], "cwd": f.parent().unwrap_or(root)}),
                        None,
                    ));
                }
            }
            _ => {}
        }
    }
    out
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy())
        .into_owned()
}

impl App {
    /// Every configuration for the current place: launch.json files first,
    /// then the defaults.
    pub(crate) fn launch_configs(&mut self) -> Vec<Launch> {
        let root = self.project_root();
        let file = self.doc.canon.clone();
        let mut out = Vec::new();
        for rel in [".skink/launch.json", ".vscode/launch.json"] {
            let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
                continue;
            };
            match parse_launch_json(&text, &root, file.as_deref()) {
                Ok((ls, skipped)) => {
                    out.extend(ls);
                    for s in skipped {
                        self.toast(crate::nav::Level::Warn, format!("{rel}: {s}"));
                    }
                }
                Err(e) => self.toast(crate::nav::Level::Error, format!("{rel}: {e}")),
            }
        }
        let text = self.doc.editor.buffer.text();
        let line = self.doc.editor.cursor.line;
        for d in defaults(&root, file.as_deref(), &text, line) {
            if !out.iter().any(|o| o.name == d.name) {
                out.push(d);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_json_subset_with_variables_and_os_overrides() {
        let text = r#"{
            // comments and trailing commas are fine
            "version": "0.2.0",
            "configurations": [
                {"name": "app", "type": "lldb", "request": "launch",
                 "program": "${workspaceFolder}/target/debug/app",
                 "args": ["--file", "${fileBasenameNoExtension}"],
                 "env": {"K": "${env:SKINK_LAUNCH_TEST}"},
                 "preLaunchTask": "cargo build",
                 "linux": {"args": ["--linux"]}, "osx": {"args": ["--mac"]},
                 "windows": {"args": ["--win"]}},
                {"name": "attach", "type": "debugpy", "request": "attach", "connect": {"port": 5678}},
                {"name": "x", "type": "mystery"},
            ],
        }"#;
        let root = Path::new("/proj");
        let (ls, skipped) =
            parse_launch_json(text, root, Some(Path::new("/proj/src/main.rs"))).unwrap();
        assert_eq!(ls.len(), 2);
        assert_eq!(skipped, ["x: unknown type \"mystery\""]);
        let a = &ls[0];
        assert_eq!(a.pre_launch_task.as_deref(), Some("cargo build"));
        assert_eq!(a.arguments["program"], "/proj/target/debug/app");
        assert!(a.arguments.get("preLaunchTask").is_none() && a.arguments.get("type").is_none());
        let os_arg = if cfg!(windows) {
            "--win"
        } else if cfg!(target_os = "macos") {
            "--mac"
        } else {
            "--linux"
        };
        assert_eq!(a.arguments["args"], json!([os_arg]));
        assert_eq!(ls[1].request, RequestKind::Attach);
        assert_eq!(ls[1].adapter, Adapter::Debugpy);
    }

    #[test]
    fn defaults_find_the_test_at_the_cursor_and_cargo_bins() {
        let py = "import x\n\ndef test_one():\n    assert 1\n\ndef helper():\n    pass\n";
        let ds = defaults(
            Path::new("/nowhere"),
            Some(Path::new("/p/t_test.py")),
            py,
            3,
        );
        assert_eq!(ds[0].name, "pytest: test_one");
        assert_eq!(ds[0].arguments["args"], json!(["/p/t_test.py::test_one"]));
        // Inside a non-test function: only the whole file.
        let ds = defaults(Path::new("/nowhere"), Some(Path::new("/p/t.py")), py, 6);
        assert_eq!(ds.len(), 1);
        let go = "package x\n\nfunc TestAdd(t *testing.T) {\n\tt.Fail()\n}\n";
        let ds = defaults(
            Path::new("/nowhere"),
            Some(Path::new("/p/x_test.go")),
            go,
            3,
        );
        assert_eq!(ds[0].arguments["args"], json!(["-test.run", "^TestAdd$"]));
        assert_eq!(
            cargo_bins(
                "[package]\nname = \"skink\"\n[[bin]]\nname = \"a\"\n[[bin]]\nname = \"b\"\n"
            ),
            ["a", "b"]
        );
        assert_eq!(
            cargo_bins("[package]\nname = \"one\"\nversion = \"1\"\n"),
            ["one"]
        );
    }
}
