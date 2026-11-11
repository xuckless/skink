//! Grammars installed at run time from packages: tree-sitter WASM modules,
//! compiled once per process by wasmtime on first use of the language.
//!
//! A package registers a [`DynSpec`] at start-up (cheap: nothing is read or
//! compiled). The first parse of a file in that language loads the module on
//! whatever thread asks, which for live documents is the parse worker, so the
//! first frame never waits. Every parser then gets its own `WasmStore` (about
//! 10 ms to instantiate); the compiled module is shared through the engine.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock};
use tree_sitter::wasmtime::Engine;
use tree_sitter::{Language, Parser, WasmStore};

/// A run-time language, an index into the registry. `Copy`, like [`Lang`].
///
/// [`Lang`]: crate::Lang
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DynLang(u16);

/// What a package says about one grammar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DynSpec {
    /// Lower-case language name (`"kotlin"`): code fences and injections use it.
    pub name: String,
    /// Other names a code fence may use (`"kt"`).
    pub aliases: Vec<String>,
    /// The `.wasm` grammar file.
    pub wasm: PathBuf,
    /// File extensions without the dot, lower case (`"kt"`, `"kts"`).
    pub extensions: Vec<String>,
    /// Exact file names (`"build.gradle.kts"` is matched by extension; this is
    /// for names like `"Podfile"`).
    pub file_names: Vec<String>,
    /// `highlights.scm` source.
    pub highlights: String,
    /// `injections.scm` source, if the package ships one.
    pub injections: Option<String>,
}

/// Where a run-time grammar is, as `:packages` and the status line show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadState {
    /// Registered; nothing read yet.
    Unloaded,
    /// A thread is compiling the module right now.
    Loading,
    Ready,
    /// The file is missing, not WASM, or built for another tree-sitter ABI.
    Failed(String),
}

enum Slot {
    Unloaded,
    Loading,
    Ready(Language),
    Failed(String),
}

struct Entry {
    name: &'static str,
    /// Read on every file-type lookup; written on (re)install.
    spec: RwLock<DynSpec>,
    /// Read by lookups and `state`; written briefly around a compile.
    slot: RwLock<Slot>,
    /// Taken only by loaders, so a second caller waits for the first compile
    /// instead of starting another, and readers never wait on a compile.
    loading: Mutex<()>,
}

static REGISTRY: RwLock<Vec<&'static Entry>> = RwLock::new(Vec::new());
static ENGINE: OnceLock<Engine> = OnceLock::new();

fn engine() -> &'static Engine {
    ENGINE.get_or_init(Engine::default)
}

fn entry(d: DynLang) -> Option<&'static Entry> {
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(d.0 as usize)
        .copied()
}

fn read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|e| e.into_inner())
}

/// Register (or replace, on reinstall) a package grammar. Reads nothing.
pub fn register(spec: DynSpec) -> DynLang {
    let mut reg = write(&REGISTRY);
    if let Some(i) = reg.iter().position(|e| e.name == spec.name) {
        let e = reg[i];
        let _load = e.loading.lock().unwrap_or_else(|e| e.into_inner());
        *write(&e.spec) = spec;
        *write(&e.slot) = Slot::Unloaded;
        return DynLang(i as u16);
    }
    // Entries live for the process: a handful per installed package.
    let name: &'static str = Box::leak(spec.name.clone().into_boxed_str());
    let e: &'static Entry = Box::leak(Box::new(Entry {
        name,
        spec: RwLock::new(spec),
        slot: RwLock::new(Slot::Unloaded),
        loading: Mutex::new(()),
    }));
    reg.push(e);
    DynLang((reg.len() - 1) as u16)
}

/// Uninstall: the grammar stops claiming files; its id stays valid.
pub fn unregister(name: &str) {
    if let Some(e) = read(&REGISTRY).iter().find(|e| e.name == name) {
        *write(&e.slot) = Slot::Failed("uninstalled".into());
    }
}

fn find(pred: impl Fn(&DynSpec) -> bool) -> Option<DynLang> {
    read(&REGISTRY).iter().enumerate().find_map(|(i, e)| {
        let usable = !matches!(*read(&e.slot), Slot::Failed(_));
        (usable && pred(&read(&e.spec))).then_some(DynLang(i as u16))
    })
}

pub(crate) fn by_extension(ext: &str) -> Option<DynLang> {
    find(|s| s.extensions.iter().any(|e| e == ext))
}

pub(crate) fn by_file_name(file: &str) -> Option<DynLang> {
    find(|s| s.file_names.iter().any(|f| f == file))
}

pub(crate) fn by_name(name: &str) -> Option<DynLang> {
    find(|s| s.name == name || s.aliases.iter().any(|a| a == name))
}

impl DynLang {
    pub fn name(self) -> &'static str {
        entry(self).map_or("unknown", |e| e.name)
    }

    pub fn state(self) -> LoadState {
        let Some(e) = entry(self) else {
            return LoadState::Failed("not registered".into());
        };
        match &*read(&e.slot) {
            Slot::Unloaded => LoadState::Unloaded,
            Slot::Loading => LoadState::Loading,
            Slot::Ready(_) => LoadState::Ready,
            Slot::Failed(why) => LoadState::Failed(why.clone()),
        }
    }

    /// The grammar, compiling the module on the first call (blocks this
    /// thread for ~0.1-0.2 s per language; live documents call it from the
    /// parse worker). `None` once it has failed.
    pub(crate) fn language(self) -> Option<Language> {
        let e = entry(self)?;
        if let Some(l) = ready(e) {
            return l;
        }
        let _load = e.loading.lock().unwrap_or_else(|e| e.into_inner());
        // Another caller may have finished while this one waited.
        if let Some(l) = ready(e) {
            return l;
        }
        *write(&e.slot) = Slot::Loading;
        let spec = read(&e.spec).clone();
        let (slot, out) = match load(&spec) {
            Ok(l) => (Slot::Ready(l.clone()), Some(l)),
            Err(why) => (Slot::Failed(why), None),
        };
        *write(&e.slot) = slot;
        out
    }

    pub(crate) fn highlights(self) -> String {
        entry(self).map_or_else(String::new, |e| read(&e.spec).highlights.clone())
    }

    pub(crate) fn injections(self) -> Option<String> {
        entry(self).and_then(|e| read(&e.spec).injections.clone())
    }
}

/// `Some(answer)` once the slot is decided, `None` while it still needs a
/// load.
fn ready(e: &Entry) -> Option<Option<Language>> {
    match &*read(&e.slot) {
        Slot::Ready(l) => Some(Some(l.clone())),
        Slot::Failed(_) => Some(None),
        Slot::Unloaded | Slot::Loading => None,
    }
}

fn load(spec: &DynSpec) -> Result<Language, String> {
    let bytes = std::fs::read(&spec.wasm).map_err(|e| format!("{}: {e}", spec.wasm.display()))?;
    let mut store = WasmStore::new(engine()).map_err(|e| e.to_string())?;
    store
        .load_language(&spec.name, &bytes)
        .map_err(|e| format!("{}: {e}", spec.wasm.display()))
}

/// Load `spec`'s module and compile its queries without registering it, so
/// an install can refuse a grammar that would only fail silently later.
pub fn check(spec: &DynSpec) -> Result<(), String> {
    let l = load(spec)?;
    tree_sitter::Query::new(&l, &spec.highlights).map_err(|e| format!("highlights: {e}"))?;
    if let Some(src) = &spec.injections {
        tree_sitter::Query::new(&l, src).map_err(|e| format!("injections: {e}"))?;
    }
    Ok(())
}

/// A parser for a WASM language: its own store from the shared engine.
pub(crate) fn attach_store(p: &mut Parser) -> Option<()> {
    let store = WasmStore::new(engine()).ok()?;
    p.set_wasm_store(store).ok()
}
