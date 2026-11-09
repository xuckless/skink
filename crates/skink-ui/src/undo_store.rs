//! Persistent undo: a file's undo and redo steps survive closing it and
//! restarting Skink. Stored per file in `<state>/undo/`, keyed by the
//! canonical path, and only trusted while the file on disk is byte for byte
//! the one the steps were saved against (FNV-1a hash + length). Saved when a
//! clean document closes or Skink exits; never in lean mode (`$EDITOR`).

use serde::{Deserialize, Serialize};
use skink_core::Editor;
use skink_core::history::{Edit, Group};
use std::path::{Path, PathBuf};

/// Largest saved history per file; the oldest steps go first.
const MAX_FILE_BYTES: usize = 2 << 20;
/// All saved histories together; the least recently written go first.
const MAX_TOTAL_BYTES: u64 = 32 << 20;
/// Files bigger than this keep their history in memory only.
const MAX_SOURCE_BYTES: u64 = 32 << 20;

/// What opening a file found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoLoad {
    /// Nothing saved for this file.
    Fresh,
    /// Steps brought back.
    Restored { steps: usize },
    /// Saved for an older version of the file: deleted.
    Stale,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    path: PathBuf,
    len: u64,
    hash: u64,
    undo: Vec<SavedGroup>,
    redo: Vec<SavedGroup>,
}

#[derive(Serialize, Deserialize)]
struct SavedGroup {
    before: usize,
    after: usize,
    edits: Vec<SavedEdit>,
}

#[derive(Serialize, Deserialize)]
struct SavedEdit {
    at: usize,
    removed: String,
    inserted: String,
}

const VERSION: u32 = 1;

/// FNV-1a: stable across Rust versions and platforms (std's hasher is not).
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn dir(state: &Path) -> PathBuf {
    state.join("undo")
}

fn entry(state: &Path, canon: &Path) -> PathBuf {
    let key = fnv(canon.to_string_lossy().as_bytes());
    dir(state).join(format!("{key:016x}.json"))
}

/// The file's current bytes, hashed, unless it is too big to bother.
fn fingerprint(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some((bytes.len() as u64, fnv(&bytes)))
}

fn to_saved(g: Group) -> SavedGroup {
    SavedGroup {
        before: g.before,
        after: g.after,
        edits: g
            .edits
            .into_iter()
            .map(|e| SavedEdit {
                at: e.at,
                removed: e.removed,
                inserted: e.inserted,
            })
            .collect(),
    }
}

fn from_saved(g: SavedGroup) -> Group {
    let edits = g
        .edits
        .into_iter()
        .map(|e| Edit {
            at: e.at,
            removed: e.removed,
            inserted: e.inserted,
        })
        .collect();
    Group::new(edits, g.before, g.after)
}

/// Keep `editor`'s history for `canon`. Only for a clean buffer: its text
/// is then the file on disk, which is what the steps are checked against.
pub fn save(state: &Path, canon: &Path, editor: &Editor) {
    if editor.buffer.is_dirty() {
        return;
    }
    let (undo, redo) = editor.history_steps();
    let path = entry(state, canon);
    if undo.is_empty() && redo.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let Some((len, hash)) = fingerprint(canon) else {
        return;
    };
    let mut saved = Saved {
        version: VERSION,
        path: canon.to_path_buf(),
        len,
        hash,
        undo: undo.into_iter().map(to_saved).collect(),
        redo: redo.into_iter().map(to_saved).collect(),
    };
    // Bounded per file: redo goes first, then the oldest undo steps.
    let mut json = serde_json::to_vec(&saved).unwrap_or_default();
    while json.len() > MAX_FILE_BYTES && !(saved.undo.is_empty() && saved.redo.is_empty()) {
        if !saved.redo.is_empty() {
            saved.redo.clear();
        } else {
            let drop = (saved.undo.len() / 4).max(1);
            saved.undo.drain(..drop);
        }
        json = serde_json::to_vec(&saved).unwrap_or_default();
    }
    if saved.undo.is_empty() && saved.redo.is_empty() {
        return;
    }
    if std::fs::create_dir_all(dir(state)).is_err() {
        return;
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &json).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
        evict(state);
    }
}

/// Bring back `canon`'s saved history into `editor` when the file on disk
/// is unchanged since it was saved.
pub fn load(state: &Path, canon: &Path, editor: &mut Editor) -> UndoLoad {
    let path = entry(state, canon);
    let Ok(bytes) = std::fs::read(&path) else {
        return UndoLoad::Fresh;
    };
    let stale = || {
        let _ = std::fs::remove_file(&path);
        UndoLoad::Stale
    };
    let Ok(saved) = serde_json::from_slice::<Saved>(&bytes) else {
        return stale();
    };
    if saved.version != VERSION || saved.path != canon || editor.buffer.is_dirty() {
        return stale();
    }
    if fingerprint(canon) != Some((saved.len, saved.hash)) {
        return stale();
    }
    let steps = saved.undo.len();
    editor.restore_history(
        saved.undo.into_iter().map(from_saved).collect(),
        saved.redo.into_iter().map(from_saved).collect(),
    );
    UndoLoad::Restored { steps }
}

/// Keep the folder under its size limit: oldest written go first.
fn evict(state: &Path) {
    let Ok(rd) = std::fs::read_dir(dir(state)) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, size, p) in files {
        if total <= MAX_TOTAL_BYTES {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skink_core::Buffer;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("skink-undo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn open(path: &Path) -> Editor {
        Editor::new(Buffer::open(path).unwrap())
    }

    #[test]
    fn history_survives_closing_and_is_dropped_when_the_file_changes() {
        let d = tmp("survive");
        let state = d.join("state");
        let file = crate::doc::canonical(&d.join("a.txt"));
        std::fs::write(&file, "one\n").unwrap();
        let mut e = open(&file);
        e.insert_text("zero ");
        e.save();
        save(&state, &file, &e);

        let mut back = open(&file);
        assert_eq!(
            load(&state, &file, &mut back),
            UndoLoad::Restored { steps: 1 }
        );
        back.undo();
        assert_eq!(back.buffer.text(), "one\n");
        back.redo();
        assert_eq!(back.buffer.text(), "zero one\n");

        // Changed on disk in between: the steps would not fit, so they go.
        std::fs::write(&file, "other\n").unwrap();
        let mut changed = open(&file);
        assert_eq!(load(&state, &file, &mut changed), UndoLoad::Stale);
        assert_eq!(load(&state, &file, &mut changed), UndoLoad::Fresh);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn undo_works_after_quitting_and_reopening_the_app() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let d = tmp("app");
        let file = d.join("notes.txt");
        std::fs::write(&file, "keep\n").unwrap();
        let state = d.join("state");
        let new_app = || {
            let mut a = crate::App::new(
                Editor::new(Buffer::empty()),
                crate::Keymap::defaults(),
                skink_core::Backend::Local,
            );
            a.state_dir = Some(state.clone());
            a.open_path(&file);
            a
        };
        let mut a = new_app();
        for c in "new ".chars() {
            a.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        a.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        a.shutdown();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new keep\n");

        let mut b = new_app();
        assert!(
            b.doc
                .editor
                .message
                .as_deref()
                .is_some_and(|m| m.contains("restored")),
            "{:?}",
            b.doc.editor.message
        );
        b.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(b.doc.editor.buffer.text(), "keep\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_dirty_buffer_saves_nothing_and_size_is_bounded() {
        let d = tmp("bounds");
        let state = d.join("state");
        let file = crate::doc::canonical(&d.join("b.txt"));
        std::fs::write(&file, "").unwrap();
        let mut e = open(&file);
        e.insert_text("x");
        save(&state, &file, &e); // dirty: not saved
        assert_eq!(load(&state, &file, &mut open(&file)), UndoLoad::Fresh);

        // ~3 MB of history in 1000 steps: trimmed to the per-file limit.
        let chunk = "y".repeat(3000);
        for _ in 0..1000 {
            e.insert_text(&chunk);
        }
        e.save();
        save(&state, &file, &e);
        let size = std::fs::metadata(entry(&state, &file)).unwrap().len();
        assert!(size <= MAX_FILE_BYTES as u64, "{size}");
        let mut back = open(&file);
        let UndoLoad::Restored { steps } = load(&state, &file, &mut back) else {
            panic!("restored");
        };
        assert!(steps > 0 && steps < 1000, "{steps}");
        back.undo();
        assert_eq!(back.buffer.len_chars(), e.buffer.len_chars() - 3000);
        let _ = std::fs::remove_dir_all(&d);
    }
}
