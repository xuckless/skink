//! User scripts: Lua 5.4 through mlua (MIT; Lua itself MIT, built from the
//! vendored C sources). `init.lua` in the config folder always runs; a
//! project's `.skink/init.lua` runs only once trusted (asked once per
//! content hash, remembered in the state folder).
//!
//! The Lua state exists only when a script was found, so editors without
//! scripts pay nothing. Each entry into Lua (loading, an event, a script
//! command, a picker choice) runs under a time limit and a memory cap, and
//! its edits form one undo step. Errors become toasts.
//!
//! The `skink` API functions borrow the [`App`] for the duration of one
//! entry ([`mlua::Lua::scope`]), so scripts always see the live editor.

use crate::App;
use crate::nav::Level;
use mlua::{Function, HookTriggers, Lua, LuaOptions, StdLib, Table, Value, Variadic, VmState};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// One entry into Lua may run this long before it is stopped.
const TIME_LIMIT: Duration = Duration::from_millis(250);
/// Heap cap for all scripts together.
const MEMORY_LIMIT: usize = 32 << 20;
/// `change` fires once typing pauses this long.
pub(crate) const CHANGE_DEBOUNCE: Duration = Duration::from_millis(300);
const EVENTS: [&str; 4] = ["open", "save", "change", "key"];
const TRUST_FILE: &str = "trusted-scripts.json";

pub(crate) enum Scripting {
    /// No script found: no Lua state at all.
    Off,
    Ready(Box<Lua>),
    /// Taken out while Lua runs; events raised meanwhile are dropped.
    Running,
}

/// The debounced `change` event.
pub(crate) enum ChangeWatch {
    /// Nothing pending; `seen` is the revision last reported.
    Quiet {
        seen: u64,
    },
    Due {
        seen: u64,
        at: Instant,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    Open,
    Save,
    Change,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Event::Open => "open",
            Event::Save => "save",
            Event::Change => "change",
        }
    }
}

/// Whether a project script may run.
enum Trust {
    Trusted(String),
    /// Not trusted, or changed since it was.
    Ask,
}

fn runtime(msg: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(msg)
}

fn new_engine() -> mlua::Result<Lua> {
    // No io / os / package / debug: scripts reach files and processes only
    // through editor commands.
    let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE;
    let lua = Lua::new_with(libs, LuaOptions::default())?;
    lua.set_memory_limit(MEMORY_LIMIT)?;
    let g = lua.globals();
    for f in ["dofile", "loadfile"] {
        g.set(f, Value::Nil)?;
    }
    let hooks = lua.create_table()?;
    for e in EVENTS {
        hooks.set(e, lua.create_table()?)?;
    }
    lua.set_named_registry_value("skink_hooks", hooks)?;
    lua.set_named_registry_value("skink_commands", lua.create_table()?)?;
    let api = lua.create_table()?;
    api.set(
        "on",
        lua.create_function(|lua, (ev, f): (String, Function)| {
            let hooks: Table = lua.named_registry_value("skink_hooks")?;
            match hooks.get::<Option<Table>>(ev.as_str())? {
                Some(list) => list.raw_push(f),
                None => Err(runtime(format!(
                    "unknown event \"{ev}\" (open, save, change, key)"
                ))),
            }
        })?,
    )?;
    api.set(
        "add_command",
        lua.create_function(|lua, (name, f): (String, Function)| {
            if name.is_empty() || name.contains(char::is_whitespace) {
                return Err(runtime(format!("bad command name \"{name}\"")));
            }
            let cmds: Table = lua.named_registry_value("skink_commands")?;
            cmds.set(name, f)
        })?,
    )?;
    g.set("skink", api)?;
    Ok(lua)
}

/// The first useful line of a Lua error.
fn error_line(e: &mlua::Error) -> String {
    let root = match e {
        mlua::Error::CallbackError { cause, .. } => cause.as_ref(),
        e => e,
    };
    let text = match root {
        mlua::Error::RuntimeError(m) | mlua::Error::SyntaxError { message: m, .. } => m.clone(),
        e => e.to_string(),
    };
    text.lines().next().unwrap_or("").to_owned()
}

fn hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl App {
    /// Run the user's `init.lua` from `config` and the project's (trusted)
    /// `.skink/init.lua`, then report the current file as opened.
    pub(crate) fn load_scripts(&mut self, config: Option<PathBuf>) {
        if self.lean_process() {
            return;
        }
        if let Some(user) = config.map(|d| d.join("init.lua"))
            && let Ok(text) = std::fs::read_to_string(&user)
        {
            self.run_script("init.lua", &text);
        }
        let project = self.project_root().join(".skink").join("init.lua");
        if project.is_file() {
            match self.trust_of(&project) {
                Trust::Trusted(text) => self.run_script(".skink/init.lua", &text),
                Trust::Ask => {
                    self.open_choice(crate::prompt::Choice::TrustScript { path: project })
                }
            }
        }
        if self.doc.editor.buffer.path().is_some() {
            self.script_event(Event::Open);
        }
    }

    fn trust_file(&self) -> Option<PathBuf> {
        self.state_dir.as_ref().map(|d| d.join(TRUST_FILE))
    }

    fn trusted(&self) -> serde_json::Map<String, serde_json::Value> {
        self.trust_file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn trust_of(&self, path: &Path) -> Trust {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Trust::Ask;
        };
        let key = crate::doc::canonical(path).display().to_string();
        match self.trusted().get(&key).and_then(|v| v.as_str()) {
            Some(h) if h == hash(&text) => Trust::Trusted(text),
            _ => Trust::Ask,
        }
    }

    /// The answer to the trust question for a project script.
    pub(crate) fn answer_trust(&mut self, path: &Path, trust: bool) {
        if !trust {
            self.toast(Level::Info, "Project script not run");
            return;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        if let Some(f) = self.trust_file() {
            let mut all = self.trusted();
            let key = crate::doc::canonical(path).display().to_string();
            all.insert(key, hash(&text).into());
            let _ = std::fs::create_dir_all(f.parent().unwrap_or(Path::new(".")));
            let body = serde_json::to_string_pretty(&all).unwrap_or_default();
            let _ = std::fs::write(f, body);
        }
        self.run_script(".skink/init.lua", &text);
    }

    /// Run a chunk, starting Lua on first use.
    pub(crate) fn run_script(&mut self, name: &str, text: &str) {
        if let Scripting::Off = self.script {
            match new_engine() {
                Ok(lua) => self.script = Scripting::Ready(Box::new(lua)),
                Err(e) => return self.toast(Level::Error, format!("Scripts: {e}")),
            }
        }
        let name = name.to_owned();
        self.enter(move |lua| lua.load(text).set_name(name).exec());
    }

    pub(crate) fn scripts_loaded(&self) -> bool {
        matches!(self.script, Scripting::Ready(_))
    }

    /// Call one entry into Lua with the API bound to this app.
    fn enter<R>(&mut self, call: impl FnOnce(&Lua) -> mlua::Result<R>) -> Option<R> {
        let lua = match std::mem::replace(&mut self.script, Scripting::Running) {
            Scripting::Ready(lua) => lua,
            other => {
                self.script = other;
                return None;
            }
        };
        let (doc, mark) = (self.doc.id, self.doc.editor.undo_mark());
        let deadline = Instant::now() + TIME_LIMIT;
        lua.set_hook(
            HookTriggers::new().every_nth_instruction(4096),
            move |_, _| match Instant::now() > deadline {
                true => Err(runtime(format!(
                    "stopped after {} ms (endless loop?)",
                    TIME_LIMIT.as_millis()
                ))),
                false => Ok(VmState::Continue),
            },
        );
        let res = {
            let app = RefCell::new(&mut *self);
            lua.scope(|s| {
                bind_api(&lua, s, &app)?;
                call(&lua)
            })
        };
        lua.remove_hook();
        // The scoped API functions are dead now; drop them from the table.
        if let Ok(api) = lua.globals().get::<Table>("skink") {
            for f in SCOPED {
                let _ = api.set(f, Value::Nil);
            }
        }
        let _ = lua.globals().set("print", Value::Nil);
        self.script = Scripting::Ready(lua);
        if self.doc.id == doc {
            self.doc.editor.group_since(mark);
        }
        match res {
            Ok(r) => Some(r),
            Err(e) => {
                self.toast(Level::Error, format!("Script: {}", error_line(&e)));
                None
            }
        }
    }

    /// Run the handlers for `ev` with the current file's path.
    pub(crate) fn script_event(&mut self, ev: Event) {
        if !self.scripts_loaded() {
            return;
        }
        let path = self
            .doc
            .editor
            .buffer
            .path()
            .map(|p| p.display().to_string());
        self.enter(|lua| {
            let hooks: Table = lua.named_registry_value("skink_hooks")?;
            let list: Table = hooks.get(ev.name())?;
            for f in list.sequence_values::<Function>() {
                f?.call::<()>(path.clone())?;
            }
            Ok(())
        });
    }

    /// An unbound key: true when a `key` handler returned true.
    pub(crate) fn script_key(&mut self, key: &str) -> bool {
        if !self.scripts_loaded() {
            return false;
        }
        self.enter(|lua| {
            let hooks: Table = lua.named_registry_value("skink_hooks")?;
            let list: Table = hooks.get("key")?;
            for f in list.sequence_values::<Function>() {
                if f?.call::<Option<bool>>(key)? == Some(true) {
                    return Ok(true);
                }
            }
            Ok(false)
        })
        .unwrap_or(false)
    }

    /// Commands added with `skink.add_command`, sorted.
    pub(crate) fn script_commands(&self) -> Vec<String> {
        let Scripting::Ready(lua) = &self.script else {
            return Vec::new();
        };
        let mut out: Vec<String> = lua
            .named_registry_value::<Table>("skink_commands")
            .map(|t| {
                t.pairs::<String, Function>()
                    .filter_map(Result::ok)
                    .map(|(k, _)| k)
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    /// Run a script command; false when there is none called `name`.
    pub(crate) fn run_script_command(&mut self, name: &str, args: &[&str]) -> bool {
        if !self.script_commands().iter().any(|c| c == name) {
            return false;
        }
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        self.enter(|lua| {
            let cmds: Table = lua.named_registry_value("skink_commands")?;
            cmds.get::<Function>(name)?
                .call::<()>(Variadic::from_iter(args))
        });
        true
    }

    /// The user chose item `index` (0-based) of a `skink.pick` list.
    pub(crate) fn script_picked(&mut self, index: usize, label: String) {
        self.enter(|lua| {
            let f: Option<Function> = lua.named_registry_value("skink_pick")?;
            lua.set_named_registry_value("skink_pick", Value::Nil)?;
            match f {
                Some(f) => f.call::<()>((label, index + 1)),
                None => Ok(()),
            }
        });
    }

    /// Call after an edit: arms the debounced `change` event.
    pub(crate) fn script_edited(&mut self) {
        if !self.scripts_loaded() {
            return;
        }
        let seen = match self.script_change {
            ChangeWatch::Quiet { seen } | ChangeWatch::Due { seen, .. } => seen,
        };
        if self.doc.editor.buffer.revision() != seen {
            self.script_change = ChangeWatch::Due {
                seen,
                at: Instant::now() + CHANGE_DEBOUNCE,
            };
        }
    }

    pub(crate) fn script_timeout(&self) -> Option<Duration> {
        match self.script_change {
            ChangeWatch::Due { at, .. } => Some(at.saturating_duration_since(Instant::now())),
            ChangeWatch::Quiet { .. } => None,
        }
    }

    /// Fire `change` once typing has paused.
    pub(crate) fn script_tick(&mut self) {
        if let ChangeWatch::Due { at, .. } = self.script_change
            && Instant::now() >= at
        {
            self.script_change = ChangeWatch::Quiet {
                seen: self.doc.editor.buffer.revision(),
            };
            self.script_event(Event::Change);
        }
    }
}

/// API functions bound per entry (see [`App::enter`]).
const SCOPED: [&str; 13] = [
    "text",
    "path",
    "selection",
    "cursor",
    "set_cursor",
    "insert",
    "set_text",
    "action",
    "command",
    "message",
    "set_status",
    "pick",
    "lang",
];

fn bind_api<'s>(
    lua: &Lua,
    s: &'s mlua::Scope<'s, '_>,
    app: &'s RefCell<&mut App>,
) -> mlua::Result<()> {
    let api: Table = lua.globals().get("skink")?;
    api.set(
        "text",
        s.create_function(|_, ()| Ok(app.borrow().doc.editor.buffer.text()))?,
    )?;
    api.set(
        "path",
        s.create_function(|_, ()| {
            Ok(app
                .borrow()
                .doc
                .editor
                .buffer
                .path()
                .map(|p| p.display().to_string()))
        })?,
    )?;
    api.set(
        "lang",
        s.create_function(|_, ()| {
            Ok(app
                .borrow()
                .doc
                .syntax
                .as_ref()
                .map(|s| s.hl.lang().name().to_owned()))
        })?,
    )?;
    api.set(
        "selection",
        s.create_function(|_, ()| Ok(app.borrow().doc.editor.selected_text()))?,
    )?;
    api.set(
        "cursor",
        s.create_function(|_, ()| {
            let c = app.borrow().doc.editor.cursor;
            Ok((c.line + 1, c.col + 1))
        })?,
    )?;
    api.set(
        "set_cursor",
        s.create_function(|_, (line, col): (usize, Option<usize>)| {
            let mut a = app.borrow_mut();
            a.doc.editor.anchor = None;
            a.doc.editor.goto(Some(line), col);
            Ok(())
        })?,
    )?;
    api.set(
        "insert",
        s.create_function(|_, text: String| {
            app.borrow_mut().doc.editor.insert_text(&text);
            Ok(())
        })?,
    )?;
    api.set(
        "set_text",
        s.create_function(|_, text: String| {
            app.borrow_mut().doc.editor.set_text(&text);
            Ok(())
        })?,
    )?;
    api.set(
        "action",
        s.create_function(|_, chain: String| {
            app.borrow_mut().run_chain(&chain);
            Ok(())
        })?,
    )?;
    api.set(
        "command",
        s.create_function(|_, line: String| {
            app.borrow_mut().run_command(line.trim_start_matches(':'));
            Ok(())
        })?,
    )?;
    api.set(
        "message",
        s.create_function(|_, text: String| {
            app.borrow_mut().toast(Level::Info, text);
            Ok(())
        })?,
    )?;
    // `print` would write over the screen: it becomes a toast too.
    lua.globals().set(
        "print",
        s.create_function(|_, vals: Variadic<Value>| {
            let parts: Vec<String> = vals
                .iter()
                .map(|v| v.to_string().unwrap_or_else(|_| "?".into()))
                .collect();
            app.borrow_mut().toast(Level::Info, parts.join("\t"));
            Ok(())
        })?,
    )?;
    api.set(
        "set_status",
        s.create_function(|_, (name, text): (String, Option<String>)| {
            let mut a = app.borrow_mut();
            let items = &mut a.script_status;
            match (items.iter().position(|(n, _)| *n == name), text) {
                (Some(i), Some(t)) => items[i].1 = t,
                (Some(i), None) => {
                    items.remove(i);
                }
                (None, Some(t)) => items.push((name, t)),
                (None, None) => {}
            }
            Ok(())
        })?,
    )?;
    api.set(
        "pick",
        s.create_function(|lua, (title, items, f): (String, Vec<String>, Function)| {
            lua.set_named_registry_value("skink_pick", f)?;
            let items = items
                .into_iter()
                .enumerate()
                .map(|(index, label)| crate::picker::Item {
                    target: crate::picker::Target::Script {
                        index,
                        label: label.clone(),
                    },
                    label,
                    detail: String::new(),
                    tag: "",
                })
                .collect();
            app.borrow_mut().open_choices(&title, items);
            Ok(())
        })?,
    )?;
    Ok(())
}
