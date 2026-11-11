//! Files on disk: noticing external changes, crash backups of unsaved edits,
//! privileged (sudo) saves and large-file mode. Every per-document state
//! here is an explicit enum stored on [`Doc`]; the app only drives
//! transitions.

use crate::prompt::Choice;
use crate::{App, Overlay, Tab, doc::Doc};
use sha2::{Digest, Sha256};
use skink_core::DiskChange;
use skink_core::{Buffer, Editor};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// At or above this many chars a file opens without syntax colours or a
/// language server: a tree-sitter tree costs ~35 bytes per source byte.
pub const LARGE_CHARS: usize = 8 << 20;
/// Disk checks triggered by typing are at most this frequent.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// Unsaved edits reach the backup file after this pause.
pub const BACKUP_DELAY: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeClass {
    Normal,
    /// No syntax, no LSP (see [`LARGE_CHARS`]).
    Large,
    /// The large-file view: read-only, straight from disk, any size.
    Huge,
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

/// A file whose unsaved edits came back at start-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    pub file: PathBuf,
    /// The disk changed since the edits began (both kept, user chooses).
    pub conflict: bool,
}

/// When restored files are announced: start-up collects them into one
/// toast; after it, each file opened later says so at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestoreToast {
    #[default]
    Collect,
    Immediate,
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

/// A crash backup as stored: `skink-backup<TAB>path<TAB>pid<TAB>base` on the
/// first line, then the unsaved text. `base` hashes what the file held when
/// the edits started, so a restore can tell whether the disk moved on since.
/// Older backups carry only the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackupFile {
    pub file: PathBuf,
    /// The Skink process that wrote it (a live one still owns it).
    pub owner: Option<u32>,
    pub base: Option<String>,
    pub text: String,
}

fn text_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn write_backup(state_dir: &Path, file: &Path, text: &str, base: &str) -> io::Result<()> {
    let p = backup_path(state_dir, file);
    fs::create_dir_all(p.parent().expect("backup path has a parent"))?;
    fs::write(
        p,
        format!(
            "{BACKUP_HEADER}{}\t{}\t{}\n{text}",
            file.display(),
            std::process::id(),
            text_hash(base)
        ),
    )
}

fn parse_backup(s: &str) -> Option<BackupFile> {
    let (head, text) = s.split_once('\n')?;
    let head = head.strip_prefix(BACKUP_HEADER)?;
    let mut parts = head.rsplitn(3, '\t');
    let (base, pid, file) = (parts.next(), parts.next(), parts.next());
    Some(match (file, pid.and_then(|p| p.parse().ok()), base) {
        (Some(file), Some(pid), Some(base)) => BackupFile {
            file: PathBuf::from(file),
            owner: Some(pid),
            base: Some(base.to_owned()),
            text: text.to_owned(),
        },
        _ => BackupFile {
            file: PathBuf::from(head),
            owner: None,
            base: None,
            text: text.to_owned(),
        },
    })
}

/// The backup for `file`, if one exists.
fn read_backup_file(state_dir: &Path, file: &Path) -> Option<BackupFile> {
    parse_backup(&fs::read_to_string(backup_path(state_dir, file)).ok()?)
}

/// The backed-up text for `file`, if a backup exists.
fn read_backup(state_dir: &Path, file: &Path) -> Option<String> {
    read_backup_file(state_dir, file).map(|b| b.text)
}

/// Each running Skink holds a lock on `backup/owners/<pid>.lock`, so another
/// one can tell a crashed writer (lock free) from a live one (lock held).
#[derive(Debug, Default)]
pub enum BackupOwner {
    #[default]
    Unclaimed,
    Held {
        _lock: fs::File,
    },
    /// The lock could not be taken (read-only state folder).
    Unavailable,
}

fn owner_lock(state_dir: &Path, pid: u32) -> PathBuf {
    state_dir
        .join("backup")
        .join("owners")
        .join(format!("{pid}.lock"))
}

/// Is the process that wrote a backup still a running Skink?
fn owner_alive(state_dir: &Path, owner: Option<u32>) -> bool {
    let Some(pid) = owner else {
        return false;
    };
    let Ok(f) = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(owner_lock(state_dir, pid))
    else {
        return false;
    };
    matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock))
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
        self.check_env_files();
        let mut notes = Vec::new();
        for i in 0..self.tabs.len() {
            let Some(d) = self.doc_at_mut(i) else {
                continue;
            };
            if d.disk == DiskWatch::Conflict {
                continue;
            }
            if !matches!(d.load, crate::bigfile::Load::Ready) {
                continue;
            }
            match d.editor.buffer.disk_change() {
                None | Some(DiskChange::Unchanged) => {}
                // A log that grew: index it again, keeping the view.
                Some(DiskChange::Modified) if d.size == SizeClass::Huge => {
                    let path = d.editor.buffer.path().map(Path::to_path_buf);
                    if let Some(Ok(b)) = path.map(|p| skink_core::Buffer::open_disk(&p)) {
                        let view = d.editor.view();
                        let tab = d.editor.tab_width;
                        d.editor = skink_core::Editor::new(b);
                        d.editor.tab_width = tab;
                        d.editor.set_view(view);
                        notes.push(format!("Reloaded {} (changed on disk)", d.title()));
                    }
                }
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
        if self.backup_due() {
            self.claim_backups(&state);
        }
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
                let base = d.editor.buffer.saved_text();
                match write_backup(&state, &path, &d.editor.buffer.text(), &base) {
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

    /// Is a backup by `owner` still being written by a running Skink (this
    /// one included)?
    fn backup_is_live(&self, state: &Path, owner: Option<u32>) -> bool {
        let mine = owner == Some(std::process::id());
        (mine && matches!(self.backup_owner, BackupOwner::Held { .. })) || owner_alive(state, owner)
    }

    /// Take this process's owner lock (once), so other instances leave
    /// its backups alone while it runs.
    fn claim_backups(&mut self, state: &Path) {
        if !matches!(self.backup_owner, BackupOwner::Unclaimed) {
            return;
        }
        let lock = owner_lock(state, std::process::id());
        let held = lock
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| {
                fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(&lock)
            })
            .ok()
            .filter(|f| f.try_lock().is_ok());
        self.backup_owner = match held {
            Some(f) => BackupOwner::Held { _lock: f },
            None => BackupOwner::Unavailable,
        };
    }

    /// After opening a file: edits left in a backup by a crash come back
    /// by themselves (`restoreunsaved`), or are offered.
    pub(crate) fn offer_recovery(&mut self) {
        let (Some(state), Some(path)) = (self.state_dir.clone(), self.doc.canon.clone()) else {
            return;
        };
        if matches!(self.doc.backup, Backup::Written { .. }) {
            return; // already restored into this document
        }
        if self.doc.size == SizeClass::Huge {
            return; // read-only: nothing of its own to recover
        }
        let Some(b) = read_backup_file(&state, &path) else {
            return;
        };
        if self.backup_is_live(&state, b.owner) {
            return; // another Skink is editing it right now
        }
        if b.text == self.doc.editor.buffer.text() {
            remove_backup(&state, &path);
            return;
        }
        if self.settings.restore_unsaved && self.doc.role.lean().is_none() {
            let conflict = self.restore_into_live(&b);
            self.restored.push(Restored {
                file: path,
                conflict,
            });
            // Opened later (not at start-up): say so right away.
            if self.restore_toast == RestoreToast::Immediate {
                self.toast_restored();
            }
            return;
        }
        let backup = backup_path(&state, &path);
        self.open_choice(Choice::Recover { backup });
    }

    /// Put a backup's text into the live document as an undoable unsaved
    /// edit. True when the file changed on disk since the edits began.
    fn restore_into_live(&mut self, b: &BackupFile) -> bool {
        let disk_moved = match (&b.base, self.doc.editor.buffer.read_disk()) {
            (Some(base), Ok(now)) => text_hash(&now) != *base,
            // Deleted since, or an old backup with no base: nothing to
            // compare; the disk stays as it is and the edits unsaved.
            _ => false,
        };
        self.doc.editor.set_text(&b.text);
        self.doc.backup = Backup::Written {
            rev: self.doc.editor.buffer.revision(),
        };
        if disk_moved {
            // Keep both: the edits stay unsaved here, the disk is untouched,
            // and the usual keep mine / take disk / diff choice decides.
            self.doc.disk = DiskWatch::Conflict;
        }
        disk_moved
    }

    /// At start-up: bring back every unsaved file a crashed or killed Skink
    /// left behind, as unsaved tabs, and say so in one toast.
    pub fn restore_unsaved(&mut self) {
        self.restore_toast = RestoreToast::Immediate;
        let Some(state) = self.state_dir.clone() else {
            return;
        };
        if self.lean_process() || !self.settings.restore_unsaved {
            return self.toast_restored();
        }
        let mut found: Vec<BackupFile> = fs::read_dir(state.join("backup"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "txt"))
            .filter_map(|e| parse_backup(&fs::read_to_string(e.path()).ok()?))
            .filter(|b| !self.backup_is_live(&state, b.owner))
            .collect();
        found.sort_by(|a, b| a.file.cmp(&b.file));
        let back_to = self.active;
        for b in found {
            let canon = crate::doc::canonical(&b.file);
            if self.restored.iter().any(|r| r.file == canon) {
                continue;
            }
            let i = match self.find_doc(|d| d.canon.as_ref() == Some(&canon)) {
                Some(i) => i,
                None => {
                    let buf = match Buffer::open(&b.file) {
                        Ok(buf) => buf,
                        // Deleted since: the edits come back as a new file
                        // that the first save recreates.
                        Err(e) if e.kind() == io::ErrorKind::NotFound => Buffer::new_file(&b.file),
                        Err(_) => continue,
                    };
                    self.add_doc(Editor::new(buf), true)
                }
            };
            self.switch_to(i);
            if matches!(self.doc.backup, Backup::Written { .. }) {
                continue;
            }
            if b.text == self.doc.editor.buffer.text() {
                remove_backup(&state, &canon);
                continue;
            }
            let conflict = self.restore_into_live(&b);
            self.restored.push(Restored {
                file: canon,
                conflict,
            });
        }
        if back_to < self.tabs.len() {
            self.switch_to(back_to);
        }
        // From now on this process's backups are live (claimed after the
        // scan: a test, or a pid reused, must not hide a dead one's files).
        self.claim_backups(&state);
        // Owner locks of processes that are gone.
        for e in fs::read_dir(state.join("backup").join("owners"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let pid = e
                .path()
                .file_stem()
                .and_then(|s| s.to_str()?.parse::<u32>().ok());
            if pid.is_some() && !owner_alive(&state, pid) {
                let _ = fs::remove_file(e.path());
            }
        }
        self.toast_restored();
    }

    /// One toast for everything restored so far.
    fn toast_restored(&mut self) {
        let done = std::mem::take(&mut self.restored);
        if done.is_empty() {
            return;
        }
        let conflicts = done.iter().filter(|r| r.conflict).count();
        let names: Vec<String> = done
            .iter()
            .map(|r| {
                r.file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        let mut msg = format!(
            "Restored {} unsaved file{} after a crash ({}): Ctrl-Z shows what changed, :discard drops them",
            done.len(),
            if done.len() == 1 { "" } else { "s" },
            names.join(", ")
        );
        if conflicts > 0 {
            msg.push_str(&format!(
                ". {conflicts} changed on disk since: both kept, you choose when you open it"
            ));
        }
        self.toast(crate::nav::Level::Warn, msg);
    }

    /// `:discard`: drop this file's unsaved edits (back to the disk).
    pub(crate) fn discard_cmd(&mut self) {
        if !self.doc.editor.buffer.is_dirty() {
            self.editor_mut().message = Some("Nothing unsaved to discard".into());
            return;
        }
        match self.doc.editor.buffer.read_disk() {
            Ok(text) => {
                self.doc.editor.set_text(&text);
                self.doc.editor.buffer.mark_synced();
                self.doc.disk = DiskWatch::InSync;
                self.editor_mut().message =
                    Some("Unsaved changes discarded (Ctrl-Z brings them back)".into());
            }
            Err(e) => {
                self.editor_mut().message =
                    Some(format!("Cannot read the file to go back to: {e}"));
            }
        }
        self.write_backups();
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
            self.load_timeout(),
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
        self.poll_loads();
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
