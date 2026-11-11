//! Files on disk: noticing external changes, crash backups of unsaved edits,
//! privileged (sudo) saves and large-file mode. Every per-document state
//! here is an explicit enum stored on [`Doc`]; the app only drives
//! transitions.

use crate::prompt::Choice;
use crate::{App, Overlay, Tab, doc::Doc};
use sha2::{Digest, Sha256};
use skink_core::DiskChange;
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// At or above this many chars a file opens without syntax colours or a
/// language server: a tree-sitter tree costs ~35 bytes per source byte.
pub const LARGE_CHARS: usize = 2 << 20;
/// Disk checks triggered by typing are at most this frequent.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// Unsaved edits reach the backup file after this pause.
pub const BACKUP_DELAY: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeClass {
    Normal,
    /// No syntax, no LSP (see [`LARGE_CHARS`]).
    Large,
}

impl SizeClass {
    pub fn of(chars: usize) -> Self {
        if chars >= LARGE_CHARS {
            SizeClass::Large
        } else {
            SizeClass::Normal
        }
    }
}

/// The file on disk versus this document's unsaved edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskWatch {
    InSync,
    /// Changed on disk while the buffer has unsaved edits; the user picks
    /// keep mine / take disk / diff when the document is on screen.
    Conflict,
}

/// Crash backup of a document's unsaved edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backup {
    /// No backup file (the buffer is clean, or nothing was written yet).
    None,
    /// The backup file holds the text at buffer revision `rev`.
    Written { rev: u64 },
    /// Writing failed at `rev`; retried after the next edit.
    Failed { rev: u64 },
}

/// A document saved through `sudo tee` in a terminal tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SudoSave {
    pub doc: crate::doc::DocId,
    /// Temp file holding the bytes being written (removed afterwards).
    pub tmp: PathBuf,
}

/// Where the crash backup of `file` lives.
fn backup_path(state_dir: &Path, file: &Path) -> PathBuf {
    let hash = Sha256::digest(file.to_string_lossy().as_bytes());
    let name: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    state_dir.join("backup").join(format!("{name}.txt"))
}

const BACKUP_HEADER: &str = "skink-backup\t";

fn write_backup(state_dir: &Path, file: &Path, text: &str) -> io::Result<()> {
    let p = backup_path(state_dir, file);
    fs::create_dir_all(p.parent().expect("backup path has a parent"))?;
    fs::write(p, format!("{BACKUP_HEADER}{}\n{text}", file.display()))
}

/// The backed-up text for `file`, if a backup exists.
fn read_backup(state_dir: &Path, file: &Path) -> Option<String> {
    let s = fs::read_to_string(backup_path(state_dir, file)).ok()?;
    let (head, text) = s.split_once('\n')?;
    head.starts_with(BACKUP_HEADER).then(|| text.to_owned())
}

fn remove_backup(state_dir: &Path, file: &Path) {
    let _ = fs::remove_file(backup_path(state_dir, file));
}

/// Quote one argument for the job shell (`sh -c` / `cmd /C`).
fn shell_quote(s: &str) -> String {
    if cfg!(windows) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

impl App {
    // ---- external changes ----------------------------------------------

    /// Compare every open file with the disk. `force` skips the throttle
    /// (focus regained, tab switch); typing checks at most every 2 s.
    pub(crate) fn check_disk(&mut self, force: bool) {
        if !force && self.disk_clock.elapsed() < CHECK_EVERY {
            return;
        }
        self.disk_clock = Instant::now();
        let mut notes = Vec::new();
        for i in 0..self.tabs.len() {
            let Some(d) = self.doc_at_mut(i) else {
                continue;
            };
            if d.disk == DiskWatch::Conflict {
                continue;
            }
            match d.editor.buffer.disk_change() {
                None | Some(DiskChange::Unchanged) => {}
                Some(DiskChange::Modified) if !d.editor.buffer.is_dirty() => {
                    match d.editor.buffer.read_disk() {
                        Ok(text) => {
                            d.editor.reload(&text);
                            notes.push(format!("Reloaded {} (changed on disk)", d.title()));
                        }
                        Err(e) => notes.push(format!("{}: {e}", d.title())),
                    }
                }
                Some(DiskChange::Modified) => d.disk = DiskWatch::Conflict,
                Some(DiskChange::Deleted) => {
                    d.editor.buffer.acknowledge_disk();
                    notes.push(format!(
                        "{} was deleted on disk (save to recreate it)",
                        d.title()
                    ));
                }
            }
        }
        if let Some(n) = notes.pop() {
            self.doc.editor.message = Some(n);
        }
        self.prompt_disk_conflict();
    }

    /// Ask about the live document's disk conflict when nothing else is open.
    pub(crate) fn prompt_disk_conflict(&mut self) {
        if self.doc.disk == DiskWatch::Conflict && matches!(self.overlay, Overlay::None) {
            self.open_choice(Choice::DiskChanged);
        }
    }

    /// The user's answer to a disk conflict on the live document.
    pub(crate) fn resolve_disk_conflict(&mut self, take_disk: bool) {
        self.doc.disk = DiskWatch::InSync;
        if take_disk {
            match self.doc.editor.buffer.read_disk() {
                Ok(text) => {
                    self.doc.editor.reload(&text);
                    self.doc.editor.message = Some("Took the disk version (Ctrl-Z undoes)".into());
                }
                Err(e) => self.doc.editor.message = Some(format!("reload: {e}")),
            }
        } else {
            // Keep mine: the next save overwrites without asking again.
            self.doc.editor.buffer.acknowledge_disk();
        }
    }

    /// Show `git diff` between the disk file and the unsaved buffer.
    pub(crate) fn diff_with_disk(&mut self) {
        let (Some(path), Some(state)) = (self.doc.canon.clone(), self.state_dir.clone()) else {
            return;
        };
        let mine = state.join("diff-unsaved.txt");
        if let Err(e) = fs::create_dir_all(&state)
            .and_then(|_| fs::write(&mine, self.doc.editor.buffer.encoded()))
        {
            self.doc.editor.message = Some(format!("diff: {e}"));
            return;
        }
        let cmd = format!(
            "git diff --no-index --no-color -- {} {}",
            shell_quote(&path.to_string_lossy()),
            shell_quote(&mine.to_string_lossy())
        );
        let title = format!("disk vs unsaved: {}", self.doc.title());
        // The conflict stays open: answer it after reading the diff.
        self.doc.disk = DiskWatch::Conflict;
        self.run_to_view(&cmd, &title);
    }

    // ---- crash backups -------------------------------------------------

    fn backup_due_for(d: &Doc) -> bool {
        // A file another tool waits on is not the user's work to recover
        // later (and lean mode leaves the state folder alone).
        if d.role.lean().is_some() {
            return false;
        }
        let rev = d.editor.buffer.revision();
        match (d.editor.buffer.is_dirty(), d.backup) {
            (true, Backup::Written { rev: r } | Backup::Failed { rev: r }) => r != rev,
            (true, Backup::None) => true,
            (false, Backup::Written { .. }) => true, // stale: remove it
            (false, Backup::None | Backup::Failed { .. }) => false,
        }
    }

    pub(crate) fn backup_due(&self) -> bool {
        self.state_dir.is_some()
            && (0..self.tabs.len())
                .filter_map(|i| self.doc_at(i))
                .any(|d| d.canon.is_some() && Self::backup_due_for(d))
    }

    /// Write backups of unsaved files; remove those of files now saved.
    pub(crate) fn write_backups(&mut self) {
        let Some(state) = self.state_dir.clone() else {
            return;
        };
        for i in 0..self.tabs.len() {
            let Some(d) = self.doc_at_mut(i) else {
                continue;
            };
            let Some(path) = d.canon.clone() else {
                continue;
            };
            if !Self::backup_due_for(d) {
                continue;
            }
            let rev = d.editor.buffer.revision();
            d.backup = if d.editor.buffer.is_dirty() {
                match write_backup(&state, &path, &d.editor.buffer.text()) {
                    Ok(()) => Backup::Written { rev },
                    Err(_) => Backup::Failed { rev },
                }
            } else {
                remove_backup(&state, &path);
                Backup::None
            };
        }
    }

    /// Forget the backup of a document being closed (its edits were saved
    /// or deliberately discarded).
    pub(crate) fn drop_backup(&self, d: &Doc) {
        if let (Some(state), Some(path), Backup::Written { .. }) =
            (&self.state_dir, &d.canon, d.backup)
        {
            remove_backup(state, path);
        }
    }

    /// Normal exit: every open document was saved or discarded on purpose.
    pub fn discard_backups(&mut self) {
        for i in 0..self.tabs.len() {
            if let Some(d) = self.doc_at(i) {
                self.drop_backup(d);
            }
        }
    }

    /// After opening a file: offer edits left in a backup by a crash.
    pub(crate) fn offer_recovery(&mut self) {
        let (Some(state), Some(path)) = (&self.state_dir, &self.doc.canon) else {
            return;
        };
        let Some(text) = read_backup(state, path) else {
            return;
        };
        if text == self.doc.editor.buffer.text() {
            remove_backup(state, path);
            return;
        }
        let backup = backup_path(state, path);
        self.open_choice(Choice::Recover { backup });
    }

    pub(crate) fn recover(&mut self, backup: &Path, take: bool) {
        if take
            && let Some(state) = self.state_dir.clone()
            && let Some(path) = self.doc.canon.clone()
            && let Some(text) = read_backup(&state, &path)
        {
            self.doc.editor.set_text(&text);
            // The backup file now belongs to this document's unsaved edits.
            self.doc.backup = Backup::Written {
                rev: self.doc.editor.buffer.revision(),
            };
            self.doc.editor.message = Some("Recovered unsaved changes (save to keep them)".into());
            return;
        }
        let _ = fs::remove_file(backup);
    }

    /// When the loop should wake without input: the typing pause before
    /// LSP sync, or a pending backup. `None` = sleep until something happens.
    pub fn next_timeout(&self) -> Option<Duration> {
        [
            self.next_job_timeout(),
            self.toast_timeout(),
            self.script_timeout(),
            self.remote_timeout(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn next_job_timeout(&self) -> Option<Duration> {
        if self.lsp_pending() {
            Some(crate::LSP_DEBOUNCE)
        } else if self.backup_due() {
            Some(BACKUP_DELAY)
        } else if self.ai.busy() {
            // The AI box shows elapsed seconds.
            Some(Duration::from_secs(1))
        } else {
            None
        }
    }

    pub fn on_timeout(&mut self) {
        self.expire_toasts();
        self.script_tick();
        self.remote_tick();
        if self.lsp_pending() {
            self.sync_lsp();
        }
        self.write_backups();
    }

    // ---- privileged save -------------------------------------------------

    /// Save the live document with `sudo tee` in a terminal tab, where sudo
    /// can ask for a password. Finished in [`App::finish_sudo_saves`].
    pub(crate) fn sudo_save(&mut self) {
        if cfg!(windows) {
            self.doc.editor.message =
                Some("Permission denied: run skink as administrator to save this file".into());
            return;
        }
        let (Some(path), Some(state)) = (self.doc.canon.clone(), self.state_dir.clone()) else {
            return;
        };
        let tmp = state.join(format!("sudo-save-{}.tmp", self.doc.id.0));
        if let Err(e) = fs::create_dir_all(&state)
            .and_then(|_| fs::write(&tmp, self.doc.editor.buffer.encoded()))
        {
            self.doc.editor.message = Some(format!("sudo save: {e}"));
            return;
        }
        let spec = skink_term::CommandSpec::new("sh").args([
            "-c".to_owned(),
            r#"echo "skink: saving $1 with sudo"; sudo tee -- "$1" < "$2" > /dev/null"#.to_owned(),
            "sh".to_owned(),
            path.to_string_lossy().into_owned(),
            tmp.to_string_lossy().into_owned(),
        ]);
        let job = SudoSave {
            doc: self.doc.id,
            tmp,
        };
        match self.open_terminal(spec) {
            Ok(()) => {
                if let Some(Tab::Term(p)) = self.tabs.last_mut() {
                    p.purpose = crate::Purpose::SudoSave(job);
                }
            }
            Err(e) => {
                let _ = fs::remove_file(&job.tmp);
                self.doc.editor.message = Some(format!("sudo save: {e}"));
            }
        }
    }

    /// Check finished `sudo tee` tabs: the file on disk must now hold
    /// exactly the buffer's bytes. Success closes the tab.
    pub(crate) fn finish_sudo_saves(&mut self) {
        let done: Vec<(usize, SudoSave)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| match t {
                Tab::Term(p) if p.exited() => match &p.purpose {
                    crate::Purpose::SudoSave(job) => Some((i, job.clone())),
                    crate::Purpose::Shell | crate::Purpose::Task(_) => None,
                },
                _ => None,
            })
            .collect();
        for (tab, job) in done.into_iter().rev() {
            let _ = fs::remove_file(&job.tmp);
            if let Some(Tab::Term(p)) = self.tabs.get_mut(tab) {
                p.purpose = crate::Purpose::Shell;
            }
            let Some(i) = self.find_doc(|d| d.id == job.doc) else {
                continue;
            };
            let d = self.doc_at_mut(i).expect("found above");
            let ok = d
                .editor
                .buffer
                .path()
                .and_then(|p| fs::read(p).ok())
                .is_some_and(|disk| disk == d.editor.buffer.encoded());
            if ok {
                d.editor.buffer.mark_synced();
                let name = d.title();
                self.close_tab(tab);
                self.doc.editor.message = Some(format!("Saved {name} with sudo"));
            } else {
                self.doc.editor.message = Some("sudo save failed (see the terminal tab)".into());
            }
        }
    }
}
